//! The search window's option chips (Aa, Path, .*) in the results
//! controller and the sources: every search the controller makes carries
//! them, wrappers forward them, and an invalid regular expression replaces
//! the rows with an error until the next page.

// Row ranges are lists; many lists here hold one range.
#![allow(clippy::single_range_in_vec_init)]

use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};

use fx_engine::{Generation, QueryOptions, SearchError};
use fx_query::Cancelled;
use fx_sort::{Direction, SortKey};
use fx_ui_macos::controller::ResultsController;
use fx_ui_macos::source::{
    ResultPage, ResultsSource, Row, RowPositions, SearchQuery, SourceStatus,
};
use fx_ui_macos::switchable::SwitchableSource;

/// Files named `f0` .. `f9`; a query starting with `(` under the regex
/// option is an invalid regular expression. Records the options of every
/// search and lookup.
#[derive(Default)]
struct Recorder {
    searches: Mutex<Vec<(String, QueryOptions)>>,
    lookups: Mutex<Vec<QueryOptions>>,
}

impl Recorder {
    fn searches(&self) -> Vec<(String, QueryOptions)> {
        self.searches.lock().unwrap().clone()
    }

    fn lookups(&self) -> Vec<QueryOptions> {
        self.lookups.lock().unwrap().clone()
    }

    fn page(query: &str, offset: usize, limit: usize) -> ResultPage {
        let ids: Vec<u32> = (0..10u32)
            .filter(|i| format!("f{i}").contains(query))
            .collect();
        ResultPage {
            generation: Generation(1),
            total: ids.len(),
            ids: ids.into_iter().skip(offset).take(limit).collect(),
        }
    }
}

impl ResultsSource for Recorder {
    fn search(
        &self,
        query: &str,
        _sort: SortKey,
        _dir: Direction,
        offset: usize,
        limit: usize,
    ) -> Result<ResultPage, Cancelled> {
        self.searches
            .lock()
            .unwrap()
            .push((query.to_owned(), QueryOptions::default()));
        Ok(Recorder::page(query, offset, limit))
    }

    fn search_query(
        &self,
        query: &SearchQuery<'_>,
        offset: usize,
        limit: usize,
    ) -> Result<ResultPage, SearchError> {
        self.searches
            .lock()
            .unwrap()
            .push((query.text.to_owned(), query.options));
        if query.options.regex && query.text.starts_with('(') {
            return Err(SearchError::InvalidRegex {
                pattern: query.text.to_owned(),
                message: "unclosed group".to_owned(),
            });
        }
        Ok(Recorder::page(query.text, offset, limit))
    }

    fn cancel(&self) {}

    fn row(&self, _generation: Generation, id: u32) -> Option<Row> {
        (id < 10).then(|| Row {
            name: format!("f{id}"),
            full_path: format!("/f{id}"),
            ..Row::default()
        })
    }

    fn positions(
        &self,
        _query: &str,
        _sort: SortKey,
        _dir: Direction,
        paths: &[String],
    ) -> Result<RowPositions, Cancelled> {
        Ok(RowPositions {
            generation: Generation(1),
            rows: vec![None; paths.len()],
        })
    }

    fn positions_query(
        &self,
        query: &SearchQuery<'_>,
        paths: &[String],
    ) -> Result<RowPositions, Cancelled> {
        self.lookups.lock().unwrap().push(query.options);
        let rows = paths
            .iter()
            .map(|p| p.strip_prefix("/f").and_then(|n| n.parse().ok()))
            .collect();
        Ok(RowPositions {
            generation: Generation(2),
            rows,
        })
    }

    fn generation(&self) -> Generation {
        Generation(1)
    }

    fn status(&self) -> SourceStatus {
        SourceStatus {
            indexing: false,
            entries: 10,
        }
    }

    fn subscribe(&self) -> Receiver<Generation> {
        mpsc::channel().1
    }
}

const CASE: QueryOptions = QueryOptions {
    match_case: true,
    match_path: false,
    regex: false,
};

const REGEX: QueryOptions = QueryOptions {
    match_case: false,
    match_path: false,
    regex: true,
};

#[test]
fn the_controller_starts_from_the_saved_chips() {
    let c = ResultsController::new(Recorder::default()).with_options(CASE);
    assert_eq!(c.options(), CASE);
    let plain = ResultsController::new(Recorder::default());
    assert_eq!(plain.options(), QueryOptions::default());
}

#[test]
fn a_chip_click_runs_the_search_again_with_its_option() {
    let mut c = ResultsController::new(Recorder::default());
    let first = c.set_query("f1");
    let req = c.search_request(first);
    assert_eq!(req.options, QueryOptions::default());
    assert!(
        c.set_options(QueryOptions::default()).is_none(),
        "unchanged"
    );
    let next = c.set_options(CASE).expect("a new search");
    assert!(next > first);
    assert_eq!(c.options(), CASE);
    let req = c.search_request(next);
    assert_eq!((req.query.as_str(), req.options), ("f1", CASE));
    let page = req.execute(c.source()).expect("runs");
    assert!(c.apply(next, page));
    assert_eq!(c.row_count(), 1);
    // The superseded request is not run.
    assert!(c.run(first).is_err());
    assert_eq!(c.source().searches(), [("f1".to_owned(), CASE)]);
}

/// Row windows, copies and the kept selection's lookup all ask with the
/// same options as the page shown.
#[test]
fn every_search_of_the_controller_carries_the_chips() {
    let mut c = ResultsController::new(Recorder::default())
        .with_options(CASE)
        .with_row_window(2);
    let req = c.set_query("f");
    let page = c.run(req).expect("runs");
    assert!(c.apply(req, page));
    assert_eq!(c.row_count(), 10);
    // Row 6 is past the first window: one more search.
    assert_eq!(c.row(6).map(|r| r.name.clone()), Some("f6".to_owned()));
    let copied = c.paths_for_ranges(&[3..9]);
    assert_eq!(copied.paths.len(), 6);
    assert!(c.source().searches().iter().all(|(_, o)| *o == CASE));
    assert!(c.source().searches().len() >= 3);
    // Keep f3 selected across a refresh of the same question.
    c.remember_selection(&[3..4]);
    let rerun = c.on_generation(Generation(2), std::time::Instant::now());
    let rerun = rerun.expect("a re-run");
    let page = c.run(rerun).expect("runs");
    assert!(c.apply(
        rerun,
        ResultPage {
            generation: Generation(2),
            ..page
        }
    ));
    let again = c.reselect(false).expect("a reselection");
    assert_eq!(again.rows, [3]);
    assert_eq!(c.source().lookups(), [CASE]);
}

#[test]
fn an_invalid_regex_replaces_the_rows_until_the_next_page() {
    let mut c = ResultsController::new(Recorder::default()).with_options(REGEX);
    let req = c.set_query("f");
    let page = c.run(req).expect("runs");
    assert!(c.apply(req, page));
    assert_eq!(c.row_count(), 10);
    assert!(!c.invalid_regex());

    let bad = c.set_query("(f");
    let err = c.run(bad).expect_err("does not compile");
    assert!(matches!(err, SearchError::InvalidRegex { .. }));
    assert!(c.apply_error(bad, &err));
    assert!(c.invalid_regex());
    assert_eq!(c.row_count(), 0);
    assert!(c.row(0).is_none());

    // Turning .* off searches for the text as typed.
    let fixed = c
        .set_options(QueryOptions::default())
        .expect("a new search");
    let page = c.run(fixed).expect("runs");
    assert!(c.apply(fixed, page));
    assert!(!c.invalid_regex());
}

/// An error answering an older question is dropped like its page would
/// be; a cancelled search changes nothing.
#[test]
fn stale_and_cancelled_errors_change_nothing() {
    let mut c = ResultsController::new(Recorder::default()).with_options(REGEX);
    let bad = c.set_query("(f");
    let err = c.run(bad).expect_err("does not compile");
    let good = c.set_query("f");
    assert!(!c.apply_error(bad, &err), "superseded");
    assert!(!c.invalid_regex());
    assert!(!c.apply_error(good, &SearchError::Cancelled));
    assert!(!c.invalid_regex());
    let page = c.run(good).expect("runs");
    assert!(c.apply(good, page));
    assert_eq!(c.row_count(), 10);
}

/// `Arc` and the switchable source pass the options through; a source
/// without options of its own answers the plain search.
#[test]
fn wrappers_forward_the_options() {
    let recorder = Arc::new(Recorder::default());
    let q = SearchQuery {
        text: "f2",
        options: CASE,
        sort: SortKey::Name,
        direction: Direction::Ascending,
        your_files_first: false,
    };
    let shared: Arc<Recorder> = Arc::clone(&recorder);
    shared.search_query(&q, 0, 10).expect("runs");
    let switchable = SwitchableSource::new(Arc::clone(&recorder) as Arc<dyn ResultsSource>);
    let page = switchable.search_query(&q, 0, 10).expect("runs");
    assert_eq!(page.total, 1);
    switchable
        .positions_query(&q, &["/f2".to_owned()])
        .expect("runs");
    let bad = SearchQuery {
        text: "(",
        options: REGEX,
        ..q
    };
    assert!(matches!(
        switchable.search_query(&bad, 0, 10),
        Err(SearchError::InvalidRegex { .. })
    ));
    assert_eq!(
        recorder.searches(),
        [
            ("f2".to_owned(), CASE),
            ("f2".to_owned(), CASE),
            ("(".to_owned(), REGEX),
        ]
    );
    assert_eq!(recorder.lookups(), [CASE]);

    #[cfg(feature = "measure")]
    {
        let synthetic = fx_ui_macos::source::SyntheticSource::new(100, 1);
        let page = synthetic.search_query(&q, 0, 10).expect("runs");
        let plain = synthetic
            .search_grouped("f2", SortKey::Name, Direction::Ascending, false, 0, 10)
            .expect("runs");
        assert_eq!(page, plain);
    }
}
