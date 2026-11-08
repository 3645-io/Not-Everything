//! Shared table machinery for the query-syntax conformance suites.
//!
//! Each suite (`conformance_part1.rs` for the syntax, `conformance_part2.rs`
//! for functions and macros) builds a `Vec<Case>` and hands it to
//! [`run_cases`], which checks every case and reports all failures at once.
//!
//! A [`Case`] is a query plus the global [`SearchOptions`] it runs under,
//! and any of:
//!
//! - candidates that must match ([`Case::yes`]) and must not match
//!   ([`Case::no`]), checked through the reference evaluator;
//! - the expected tree shape ([`Case::shape`]), in the compact notation of
//!   [`render`];
//! - whether the parser must recover from a problem ([`Case::lenient`]) or
//!   parse cleanly (the default), or either ([`Case::any_diag`]).
//!
//! Candidates are written as paths. A trailing `/` makes the entry a folder.
//! A spec without any `/` other than a trailing one is a bare name and lives
//! in [`DEFAULT_PARENT`]:
//!
//! - `"notes.txt"`: file `notes.txt` in `/Users/me`;
//! - `"Projects/"`: folder `Projects` in `/Users/me`;
//! - `"/Users/me/src/main.rs"`: file `main.rs` in `/Users/me/src`;
//! - `"/Volumes/"`: folder `Volumes` in `/`.
//!
//! A spec can end with metadata words, each a space and then `key=value`
//! (see [`parse_spec`]): `"a.txt size=1500 dm=2026-11-03T10:00 attrib=H"`.
//! Everything before the first trailing metadata word is the path spec,
//! spaces included. Date functions run against the clock set with
//! [`Case::now`], by default [`EvalContext::default`] (2026-11-03T10:00Z,
//! UTC).

#![allow(dead_code)]

use fx_query::eval::{Candidate, EvalContext, matches_in};
use fx_query::{Node, Pattern, SearchOptions, parse};

/// Parent folder of candidates written as bare names.
pub const DEFAULT_PARENT: &str = "/Users/me";

/// No global toggles.
pub const DEFAULT: SearchOptions = SearchOptions {
    match_case: false,
    whole_word: false,
    match_path: false,
    match_diacritics: false,
    regex: false,
};
/// Global "Match Case".
pub const CASE: SearchOptions = SearchOptions {
    match_case: true,
    ..DEFAULT
};
/// Global "Match Whole Word".
pub const WW: SearchOptions = SearchOptions {
    whole_word: true,
    ..DEFAULT
};
/// Global "Match Path".
pub const PATH: SearchOptions = SearchOptions {
    match_path: true,
    ..DEFAULT
};
/// Global "Match Diacritics".
pub const DIACRITICS: SearchOptions = SearchOptions {
    match_diacritics: true,
    ..DEFAULT
};
/// Global "Enable Regex".
pub const REGEX: SearchOptions = SearchOptions {
    regex: true,
    ..DEFAULT
};

/// What the case expects of the parser's diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Diag {
    /// No diagnostics.
    Clean,
    /// At least one diagnostic (the parser recovered from partial input).
    Some,
    /// Not checked.
    Any,
}

/// One conformance case.
#[derive(Debug, Clone)]
pub struct Case {
    /// Group label, used in failure messages.
    pub group: &'static str,
    /// The query as typed.
    pub query: &'static str,
    /// Global toggles.
    pub opts: SearchOptions,
    /// Candidate specs that must match.
    pub yes: Vec<&'static str>,
    /// Candidate specs that must not match.
    pub no: Vec<&'static str>,
    /// Expected tree, in [`render`] notation.
    pub shape: Option<&'static str>,
    /// Expected diagnostics.
    pub diag: Diag,
    /// Clock and time zone for date functions.
    pub ctx: EvalContext,
}

/// Starts a case for `query` in `group`, with default options, no
/// expectations and a clean parse required.
pub fn case(group: &'static str, query: &'static str) -> Case {
    Case {
        group,
        query,
        opts: DEFAULT,
        yes: Vec::new(),
        no: Vec::new(),
        shape: None,
        diag: Diag::Clean,
        ctx: EvalContext::default(),
    }
}

impl Case {
    /// Runs under these global toggles.
    pub fn opts(mut self, opts: SearchOptions) -> Self {
        self.opts = opts;
        self
    }
    /// These candidates must match.
    pub fn yes(mut self, specs: &[&'static str]) -> Self {
        self.yes.extend_from_slice(specs);
        self
    }
    /// These candidates must not match.
    pub fn no(mut self, specs: &[&'static str]) -> Self {
        self.no.extend_from_slice(specs);
        self
    }
    /// The tree must render as `shape`.
    pub fn shape(mut self, shape: &'static str) -> Self {
        self.shape = Some(shape);
        self
    }
    /// The parser must report at least one diagnostic.
    pub fn lenient(mut self) -> Self {
        self.diag = Diag::Some;
        self
    }
    /// Diagnostics are not checked.
    pub fn any_diag(mut self) -> Self {
        self.diag = Diag::Any;
        self
    }
    /// Fixes the clock to `when`, a spec date (`YYYY-MM-DD` or
    /// `YYYY-MM-DDTHH:MM`, at UTC). Panics on a malformed date.
    pub fn now(mut self, when: &str) -> Self {
        self.ctx.now =
            parse_spec_date(when).unwrap_or_else(|| panic!("bad date {when:?} in Case::now"));
        self
    }
    /// Fixes the clock to `secs` Unix seconds.
    pub fn now_secs(mut self, secs: i64) -> Self {
        self.ctx.now = secs;
        self
    }
    /// Sets the local time zone, in seconds east of UTC.
    pub fn utc_offset(mut self, secs: i32) -> Self {
        self.ctx.utc_offset_secs = secs;
        self
    }
}

/// Days from 1970-01-01 to a proleptic Gregorian date.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let (y, m) = if month <= 2 {
        (year - 1, month + 9)
    } else {
        (year, month - 3)
    };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * m + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Parses a spec date at UTC: `YYYY-MM-DD` (midnight) or
/// `YYYY-MM-DDTHH:MM`. `None` if malformed.
pub fn parse_spec_date(text: &str) -> Option<i64> {
    let (date, time) = match text.split_once('T') {
        Some((d, t)) => (d, Some(t)),
        None => (text, None),
    };
    let num = |s: &str, len: usize| -> Option<i64> {
        (s.len() == len && s.bytes().all(|b| b.is_ascii_digit()))
            .then(|| s.parse().ok())
            .flatten()
    };
    let mut parts = date.split('-');
    let year = num(parts.next()?, 4)?;
    let month = num(parts.next()?, 2)?;
    let day = num(parts.next()?, 2)?;
    if parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let mut secs = days_from_civil(year, month, day) * 86_400;
    if let Some(time) = time {
        let (h, m) = time.split_once(':')?;
        let (h, m) = (num(h, 2)?, num(m, 2)?);
        if h > 23 || m > 59 {
            return None;
        }
        secs += h * 3_600 + m * 60;
    }
    Some(secs)
}

/// Splits a trailing ` key=value` metadata word off `spec`, if its key is
/// one of `size`, `dm`, `dc`, `da` or `attrib`.
fn split_meta(spec: &str) -> Option<(&str, &str, &str)> {
    let (rest, word) = spec.rsplit_once(' ')?;
    let (key, value) = word.split_once('=')?;
    matches!(key, "size" | "dm" | "dc" | "da" | "attrib").then_some((rest, key, value))
}

/// Turns a candidate spec, with any trailing metadata words, into a
/// [`Candidate`]. Metadata keys:
///
/// - `size=N`: size in bytes;
/// - `dm=`, `dc=`, `da=`: modified, created, accessed time as a spec date
///   (`YYYY-MM-DD` or `YYYY-MM-DDTHH:MM`, at UTC);
/// - `attrib=LETTERS`: `H` hidden, `L` symlink, `R` read-only.
///
/// Panics on a malformed metadata value, so a typo in a table fails loudly.
pub fn parse_spec(spec: &str) -> Candidate<'_> {
    let mut path = spec;
    let mut meta = Vec::new();
    while let Some((rest, key, value)) = split_meta(path) {
        meta.push((key, value));
        path = rest;
    }
    let (name, parent_path, is_dir) = split_spec(path);
    let mut cand = Candidate {
        name,
        parent_path,
        is_dir,
        ..Default::default()
    };
    let date = |value: &str| {
        parse_spec_date(value).unwrap_or_else(|| panic!("bad date {value:?} in spec {spec:?}"))
    };
    for (key, value) in meta {
        match key {
            "size" => {
                cand.size = Some(
                    value
                        .parse()
                        .unwrap_or_else(|_| panic!("bad size {value:?} in spec {spec:?}")),
                )
            }
            "dm" => cand.modified = Some(date(value)),
            "dc" => cand.created = Some(date(value)),
            "da" => cand.accessed = Some(date(value)),
            "attrib" => {
                for c in value.chars() {
                    match c.to_ascii_uppercase() {
                        'H' => cand.hidden = true,
                        'L' => cand.symlink = true,
                        'R' => cand.read_only = true,
                        _ => panic!("bad attribute {c:?} in spec {spec:?}"),
                    }
                }
            }
            _ => unreachable!("split_meta only returns known keys"),
        }
    }
    cand
}

/// Turns a candidate path spec (no metadata) into its parts: (name,
/// parent, is_dir).
pub fn split_spec(spec: &str) -> (&str, &str, bool) {
    let (body, is_dir) = match spec.strip_suffix('/') {
        Some(b) if !b.is_empty() => (b, true),
        _ => (spec, false),
    };
    match body.rfind('/') {
        None => (body, DEFAULT_PARENT, is_dir),
        Some(0) => (&body[1..], "/", is_dir),
        Some(i) => (&body[i + 1..], &body[..i], is_dir),
    }
}

fn render_text(text: &str) -> String {
    let plain = !text.is_empty()
        && !text
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, ',' | '(' | ')' | '"'));
    if plain {
        text.to_owned()
    } else {
        format!("{text:?}")
    }
}

/// Compact tree notation: `and(a, b)`, `or(a, b)`, `not(a)`, `ALL` for the
/// match-everything node. A literal or wildcard term is its text (quoted
/// with `"` when it is empty or holds spaces, commas, parentheses or
/// quotes); a regex term is `re(text)`. Term options are not shown; cases
/// check them through `yes`/`no` candidates. A search function is
/// `fn(name,args)` with the filter's canonical name and argument text
/// (`Filter::name`, `Filter::args`), such as `fn(size,>1024)` or
/// `fn(empty,)`.
pub fn render(node: &Node) -> String {
    let list = |children: &[Node]| children.iter().map(render).collect::<Vec<_>>().join(", ");
    match node {
        Node::And(children) => format!("and({})", list(children)),
        Node::Or(children) => format!("or({})", list(children)),
        Node::Not(inner) => format!("not({})", render(inner)),
        Node::MatchAll => "ALL".to_owned(),
        Node::Term(term) => match &term.pattern {
            Pattern::Literal(text) => render_text(text),
            Pattern::Wildcard(_) => render_text(&term.text),
            Pattern::Regex(text) => format!("re({})", render_text(text)),
        },
        Node::Function(filter) => format!("fn({},{})", filter.name(), filter.args()),
    }
}

/// Checks one case; returns a description of every way it fails.
pub fn check(case: &Case) -> Vec<String> {
    let mut problems = Vec::new();
    let label = format!("[{}] {:?} with {:?}", case.group, case.query, case.opts);
    let parsed = parse(case.query, &case.opts);

    match case.diag {
        Diag::Clean if !parsed.diagnostics.is_empty() => problems.push(format!(
            "{label}: expected a clean parse, got {:?}",
            parsed.diagnostics
        )),
        Diag::Some if parsed.diagnostics.is_empty() => {
            problems.push(format!("{label}: expected a recovery diagnostic, got none"))
        }
        _ => {}
    }

    if let Some(expected) = case.shape {
        let got = render(&parsed.query);
        if got != expected {
            problems.push(format!("{label}: shape {got} != expected {expected}"));
        }
    }

    for (specs, want) in [(&case.yes, true), (&case.no, false)] {
        for spec in specs.iter() {
            let cand = parse_spec(spec);
            match matches_in(&parsed.query, &cand, &case.ctx) {
                Ok(got) if got == want => {}
                Ok(got) => problems.push(format!(
                    "{label}: {spec:?} matched={got}, expected {want} (tree {})",
                    render(&parsed.query)
                )),
                Err(e) => problems.push(format!("{label}: {spec:?}: {e}")),
            }
        }
    }
    problems
}

/// Checks every case and panics with the full list of failures. Returns
/// the number of cases.
#[track_caller]
pub fn run_cases(suite: &str, cases: &[Case]) -> usize {
    let failures: Vec<String> = cases.iter().flat_map(check).collect();
    assert!(
        failures.is_empty(),
        "{suite}: {} problem(s) in {} cases:\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
    cases.len()
}
