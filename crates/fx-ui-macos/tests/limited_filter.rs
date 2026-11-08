//! Limited (non-FDA) mode change filtering: `limited::ExcludingChangeSource`.
//!
//! In limited mode, change events at or below a consent-prompt folder
//! (`fx_fs_macos::fda::consent_prompt_paths`) must never reach the engine,
//! because the engine's writer stats every `Change::CheckPath` and a stat
//! inside a protected folder can raise a TCC consent prompt. That includes
//! FSEvents history replayed from a saved cursor at launch.
//!
//! The wrapper is platform-agnostic: it takes any `ChangeSource` and a list
//! of excluded folders, delegates `start`/`cursor`, and drops path-bearing
//! changes for excluded paths, matching whole path components (so
//! `~/Documents2` is not excluded). A `RescanSubtree` for an ancestor of an
//! excluded folder passes through unchanged, as the scanner's own
//! exclusions already skip the protected folders.

use std::collections::VecDeque;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fx_fs::{ChangeSource, PathStat};
use fx_model::{Change, Entry, ResumeCursor, StartOutcome};
use fx_ui_macos::limited::ExcludingChangeSource;

const HOME: &str = "/Users/tester";

fn home() -> PathBuf {
    PathBuf::from(HOME)
}

/// A representative subset of the consent-prompt folders, as absolute paths.
fn excluded() -> Vec<PathBuf> {
    vec![
        home().join("Desktop"),
        home().join("Documents"),
        home().join("Downloads"),
        home().join("Library/Mobile Documents"),
        PathBuf::from("/Volumes"),
    ]
}

fn cursor_at(event_id: u64) -> ResumeCursor {
    ResumeCursor::FsEvents {
        event_id,
        db_uuid: [7; 16],
    }
}

/// What the fake source observed, shared with the test after the source is
/// moved into the wrapper.
#[derive(Default)]
struct Log {
    starts: Vec<Option<ResumeCursor>>,
    batches_served: u64,
}

/// A scripted change source: serves queued batches in order, records the
/// cursor `start` was called with, and advances its cursor by one per batch
/// served (like FSEvents advancing its event ID).
struct FakeSource {
    batches: VecDeque<io::Result<Vec<Change>>>,
    outcome: StartOutcome,
    fail_start: bool,
    log: Arc<Mutex<Log>>,
}

impl FakeSource {
    fn new(batches: Vec<Vec<Change>>) -> (Self, Arc<Mutex<Log>>) {
        let log = Arc::new(Mutex::new(Log::default()));
        let source = Self {
            batches: batches.into_iter().map(Ok).collect(),
            outcome: StartOutcome::Resumed,
            fail_start: false,
            log: Arc::clone(&log),
        };
        (source, log)
    }
}

impl ChangeSource for FakeSource {
    fn start(&mut self, from: Option<&ResumeCursor>) -> fx_fs::Result<StartOutcome> {
        self.log.lock().unwrap().starts.push(from.copied());
        if self.fail_start {
            return Err(io::Error::other("fake start failure"));
        }
        Ok(self.outcome)
    }

    fn next_batch(&mut self, _deadline: Instant) -> fx_fs::Result<Vec<Change>> {
        match self.batches.pop_front() {
            Some(batch) => {
                self.log.lock().unwrap().batches_served += 1;
                batch
            }
            None => Ok(Vec::new()),
        }
    }

    fn cursor(&self) -> ResumeCursor {
        cursor_at(1000 + self.log.lock().unwrap().batches_served)
    }
}

/// A `PathStat` that counts every path it is asked about and reports that
/// nothing exists. Stands in for the engine writer's stat of `CheckPath`s.
#[derive(Default)]
struct CountingStat {
    seen: Mutex<Vec<PathBuf>>,
}

impl PathStat for CountingStat {
    fn stat(&self, path: &Path) -> fx_fs::Result<Option<Entry>> {
        self.seen.lock().unwrap().push(path.to_path_buf());
        Ok(None)
    }
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_millis(10)
}

fn check(path: impl Into<PathBuf>) -> Change {
    Change::CheckPath(path.into())
}

fn rescan(path: impl Into<PathBuf>) -> Change {
    Change::RescanSubtree(path.into())
}

/// Runs one batch through a wrapper over `excluded()`.
fn filter_one(batch: Vec<Change>) -> Vec<Change> {
    let (inner, _log) = FakeSource::new(vec![batch]);
    let mut source = ExcludingChangeSource::new(inner, excluded());
    source.start(None).expect("start");
    source.next_batch(deadline()).expect("next_batch")
}

#[test]
fn check_paths_under_excluded_folders_are_dropped() {
    let out = filter_one(vec![
        check(home().join("Documents/report.pdf")),
        check(home().join("Desktop/a/b/c.txt")),
        check(home().join("Downloads/setup.dmg")),
        check(home().join("Library/Mobile Documents/com~apple~CloudDocs/x.txt")),
        check("/Volumes/USB/photo.jpg"),
    ]);
    assert!(
        out.is_empty(),
        "nothing under an excluded folder may pass: {out:?}"
    );
}

#[test]
fn the_excluded_folder_itself_is_dropped() {
    let out = filter_one(vec![
        check(home().join("Documents")),
        check(home().join("Desktop/")),
        check("/Volumes"),
        rescan(home().join("Downloads")),
    ]);
    assert!(
        out.is_empty(),
        "the excluded folders themselves are excluded: {out:?}"
    );
}

#[test]
fn rescan_subtree_inside_an_excluded_folder_is_dropped() {
    let out = filter_one(vec![
        rescan(home().join("Documents/Projects")),
        rescan("/Volumes/Backup"),
    ]);
    assert!(out.is_empty(), "rescans inside excluded folders: {out:?}");
}

#[test]
fn siblings_and_look_alike_prefixes_pass() {
    let batch = vec![
        check(home().join("Documents2/notes.txt")),
        check(home().join("Documents2")),
        check(home().join("Documents.bak/a")),
        check(home().join("DesktopPictures/x.png")),
        check(home().join("Music/song.mp3")),
        check(home().join("Library/Mobile Documents Old/y")),
        check("/VolumesExtra/z"),
        check(home().join("Projects/Documents/inner.txt")),
        rescan(home().join("Code")),
    ];
    let out = filter_one(batch.clone());
    assert_eq!(
        out, batch,
        "siblings and look-alike prefixes must pass unchanged and in order"
    );
}

#[test]
fn rescan_of_an_ancestor_of_an_excluded_folder_passes() {
    let batch = vec![
        rescan(home()),
        rescan(home().join("Library")),
        rescan("/"),
        Change::RescanVolume,
    ];
    let out = filter_one(batch.clone());
    assert_eq!(
        out, batch,
        "ancestor rescans and volume rescans pass through"
    );
}

#[test]
fn mixed_batch_keeps_only_allowed_changes_in_order() {
    let out = filter_one(vec![
        check(home().join("a.txt")),
        check(home().join("Documents/secret.txt")),
        check(home().join("Documents2/b.txt")),
        rescan("/Volumes/Disk"),
        check(home().join("Pictures/c.jpg")),
        check(home().join("Downloads/d.zip")),
    ]);
    assert_eq!(
        out,
        vec![
            check(home().join("a.txt")),
            check(home().join("Documents2/b.txt")),
            check(home().join("Pictures/c.jpg")),
        ]
    );
}

#[test]
fn start_forwards_the_saved_cursor_and_outcome() {
    let (mut inner, log) = FakeSource::new(Vec::new());
    inner.outcome = StartOutcome::GapDetected;
    let mut source = ExcludingChangeSource::new(inner, excluded());

    let saved = cursor_at(42);
    let outcome = source.start(Some(&saved)).expect("start");

    assert_eq!(outcome, StartOutcome::GapDetected);
    assert_eq!(log.lock().unwrap().starts, vec![Some(saved)]);
}

#[test]
fn start_without_cursor_is_forwarded_as_none() {
    let (inner, log) = FakeSource::new(Vec::new());
    let mut source = ExcludingChangeSource::new(inner, excluded());

    assert_eq!(source.start(None).expect("start"), StartOutcome::Resumed);
    assert_eq!(log.lock().unwrap().starts, vec![None]);
}

#[test]
fn start_errors_are_propagated() {
    let (mut inner, _log) = FakeSource::new(Vec::new());
    inner.fail_start = true;
    let mut source = ExcludingChangeSource::new(inner, excluded());
    assert!(source.start(None).is_err());
}

#[test]
fn next_batch_errors_are_propagated() {
    let (mut inner, _log) = FakeSource::new(Vec::new());
    inner
        .batches
        .push_back(Err(io::Error::other("fake stream failure")));
    let mut source = ExcludingChangeSource::new(inner, excluded());
    source.start(None).expect("start");
    assert!(source.next_batch(deadline()).is_err());
}

#[test]
fn cursor_advances_even_when_a_whole_batch_is_dropped() {
    // Replayed history that lies entirely inside excluded folders: the
    // events are dropped but the cursor must still move past them, so the
    // next launch does not replay them again.
    let (inner, _log) = FakeSource::new(vec![
        vec![check(home().join("Documents/a"))],
        vec![check(home().join("Desktop/b")), rescan("/Volumes/X")],
    ]);
    let mut source = ExcludingChangeSource::new(inner, excluded());
    source.start(Some(&cursor_at(1000))).expect("start");
    assert_eq!(source.cursor(), cursor_at(1000));

    assert!(source.next_batch(deadline()).expect("batch 1").is_empty());
    assert_eq!(source.cursor(), cursor_at(1001));

    assert!(source.next_batch(deadline()).expect("batch 2").is_empty());
    assert_eq!(source.cursor(), cursor_at(1002));
}

#[test]
fn cursor_matches_the_inner_source_after_passing_batches() {
    let (inner, _log) = FakeSource::new(vec![vec![check(home().join("Music/x"))]]);
    let mut source = ExcludingChangeSource::new(inner, excluded());
    source.start(None).expect("start");
    assert_eq!(
        source.next_batch(deadline()).expect("batch"),
        vec![check(home().join("Music/x"))]
    );
    assert_eq!(source.cursor(), cursor_at(1001));
}

#[test]
fn empty_exclusion_list_passes_everything() {
    let batch = vec![
        check(home().join("Documents/a")),
        rescan("/Volumes/X"),
        Change::RescanVolume,
    ];
    let (inner, _log) = FakeSource::new(vec![batch.clone()]);
    let mut source = ExcludingChangeSource::new(inner, Vec::new());
    source.start(None).expect("start");
    assert_eq!(source.next_batch(deadline()).expect("batch"), batch);
}

#[test]
fn replayed_history_never_stats_an_excluded_path() {
    // Drain the wrapper the way the engine writer does: stat every
    // CheckPath. No stat may land at or below an excluded folder.
    let (inner, _log) = FakeSource::new(vec![
        vec![
            check(home().join("Documents/old.txt")),
            check(home().join("notes.txt")),
        ],
        vec![
            check("/Volumes/Stick/file"),
            check(home().join("Downloads2/keep.txt")),
            check(home().join("Desktop")),
        ],
        vec![check(home().join("Library/Mobile Documents/x"))],
    ]);
    let mut source = ExcludingChangeSource::new(inner, excluded());
    source.start(Some(&cursor_at(1000))).expect("start");

    let stat = CountingStat::default();
    for _ in 0..4 {
        for change in source.next_batch(deadline()).expect("batch") {
            if let Change::CheckPath(path) = change {
                stat.stat(&path).expect("stat");
            }
        }
    }

    let seen = stat.seen.lock().unwrap().clone();
    for path in &seen {
        assert!(
            !excluded().iter().any(|ex| path.starts_with(ex)),
            "stat reached an excluded path: {}",
            path.display()
        );
    }
    assert_eq!(
        seen,
        vec![home().join("notes.txt"), home().join("Downloads2/keep.txt")]
    );
}

#[test]
fn wrapper_is_a_sendable_change_source() {
    // The engine moves its change source to the writer thread.
    fn assert_source<T: ChangeSource + Send + 'static>(_: &T) {}
    let (inner, _log) = FakeSource::new(Vec::new());
    let source = ExcludingChangeSource::new(inner, excluded());
    assert_source(&source);
    let _boxed: Box<dyn ChangeSource + Send> = Box::new(source);
}

/// With the real consent-prompt list from fx-fs-macos (a macOS-only
/// dependency of this crate).
#[cfg(target_os = "macos")]
#[test]
fn real_consent_prompt_paths_are_filtered() {
    use fx_fs_macos::fda::consent_prompt_paths;

    let home = home();
    let paths = consent_prompt_paths(&home);
    let (inner, _log) = FakeSource::new(vec![vec![
        check(home.join("Documents/a.txt")),
        check(home.join("Desktop/b.txt")),
        check(home.join("Downloads/c.txt")),
        check(home.join("Library/Mobile Documents/d.txt")),
        check("/Volumes/Ext/e.txt"),
        check(home.join("Documents2/keep.txt")),
        check(home.join("Library/Preferences/keep.plist")),
    ]]);
    let mut source = ExcludingChangeSource::new(inner, paths);
    source.start(None).expect("start");
    assert_eq!(
        source.next_batch(deadline()).expect("batch"),
        vec![
            check(home.join("Documents2/keep.txt")),
            check(home.join("Library/Preferences/keep.plist")),
        ]
    );
}
