//! End-to-end tests of the `not-everything` binary, built from the fx-cli
//! crate.
//!
//! Each test writes a small checkpoint store to a scratch folder with
//! `fx_store::write_checkpoint` (an `fx_index::Index` built from a fixed
//! fixture whose root entry is named `/`), runs the real binary with
//! `-store <file>` (or the default path under a scratch `$HOME`) and checks
//! the parsed output and the exit code.
//!
//! Contract these tests rely on:
//!
//! - txt (the default) prints one full path per line, `\n`-terminated; csv
//!   has a header row of column keys (`full_path`, `name`, `size`, ...) and
//!   RFC 4180 quoting; json is an array of objects keyed by column keys,
//!   with `size` as a number.
//! - The default sort is by name, ascending. `-s` sorts by path, `-sort
//!   size|date-modified|...` picks the field, `-sort-descending` reverses.
//! - `-n` limits and `-o` skips results after sorting.
//! - `-get-result-count` prints only the total match count.
//! - `count:N` in the query caps the total result count: paging (`-o`,
//!   `-n`) works inside the first N results, and `-get-result-count`
//!   prints `min(total, N)`.
//! - Exit codes: 0 success, 4 bad/missing option argument, 5 export file
//!   can't be created, 6 unknown option, 7 store rejected or query failed
//!   (an invalid `-r` pattern or `regex:` term; message on stderr, nothing
//!   on stdout), 8 no store (message on stderr), 9 no results with
//!   `-no-result-error`.
//! - With `-r` the whole search text is one regular expression, passed to
//!   the regex engine unchanged: spaces, `"`, `\"`, runs of backslashes,
//!   `!`, `<`, `>` and `|` are regex characters, not query operators.
//! - The store is never written: its bytes and its folder are unchanged
//!   after any run, and a read-only store in a read-only folder works.
//! - Without `-store`, the store is `default_store_path($HOME, root)`, where
//!   `root` is `-root` canonicalized (trailing slashes, `..`, relative
//!   paths and symlinks resolved; used as given if it can't be resolved).
//!   The canonical root's parent is also the prefix for folder-root paths.
//!   If the full-scope store is missing, the limited-scope store
//!   (`stores-limited/`, built without Full Disk Access) is read instead,
//!   with a note on stderr that mentions "limited". A Preview build reads
//!   its own `Not-Everything Preview` folder instead of `Not-Everything`.
//! - txt and csv output to a pipe or a file is exact, control characters
//!   included; on a terminal each control character in a value is shown as
//!   `?`. json escapes every control character (U+0000 to U+001F and
//!   U+007F to U+009F) wherever it is written. `-0` ends each txt result
//!   with NUL instead of a newline; with csv or json output it exits 4.
//! - `-h` prints the usage, which names the `not-everything` command, and
//!   `-version` prints `not-everything <version>`; a Preview build names
//!   `not-everything-preview` and prints its label, such as `0.2.0-RC-1`.
//! - fx-query recovers from malformed query syntax, so such a query exits
//!   0; an unclosed quote prints a warning on stderr. A regular expression
//!   that does not compile (the `-r` pattern or any `regex:` term) is an
//!   error: it exits 7 with the reason on stderr.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use fx_channel::{CURRENT_NUMBER, Channel, version_label};
use fx_cli::store_path::{StoreScope, default_store_path, store_path_for};
use fx_index::Index;
use fx_model::{Entry, EntryFlags, EntryKind, FileKey, VolumeId};
use fx_store::{SaveInput, write_checkpoint};

// ------------------------------------------------------------ fixture

/// (key, parent key, name, is folder, size, modified, created).
const FIXTURE: &[(u64, u64, &str, bool, u64, i64, i64)] = &[
    (1, 0, "/", true, 0, 1_600_000_000, 1_500_000_000),
    (2, 1, "docs", true, 0, 1_600_000_010, 1_500_000_010),
    (3, 2, "data.txt", false, 300, 1_600_000_300, 1_500_000_300),
    (
        4,
        2,
        "metadata.txt",
        false,
        100,
        1_600_000_100,
        1_500_000_100,
    ),
    (5, 2, "Readme.md", false, 50, 1_600_000_500, 1_500_000_500),
    (6, 1, "src", true, 0, 1_600_000_020, 1_500_000_020),
    (7, 6, "readme.txt", false, 700, 1_600_000_700, 1_500_000_700),
    (8, 6, "main.rs", false, 2_048, 1_600_000_200, 1_500_000_200),
    (
        9,
        6,
        "old data, v2.csv",
        false,
        10,
        1_600_000_050,
        1_500_000_050,
    ),
];

const DATA: &str = "/docs/data.txt";
const META: &str = "/docs/metadata.txt";
const OLD: &str = "/src/old data, v2.csv";

fn fixture_index() -> Index {
    fixture_index_rooted("/")
}

/// The fixture with its root entry named `root_name` (`/` for a volume
/// root, the folder's own name for a folder root).
fn fixture_index_rooted(root_name: &str) -> Index {
    let mut index = Index::new();
    for &(k, parent, name, folder, size, modified, created) in FIXTURE {
        let name = if parent == 0 { root_name } else { name };
        index
            .upsert(Entry {
                key: FileKey::new(VolumeId(1), k),
                parent: (parent != 0).then(|| FileKey::new(VolumeId(1), parent)),
                name: name.to_string(),
                kind: if folder {
                    EntryKind::Dir
                } else {
                    EntryKind::File
                },
                flags: EntryFlags::empty(),
                size,
                modified,
                created,
            })
            .expect("upsert into a fresh index");
    }
    index
}

fn write_store(path: &Path) {
    write_store_with(path, fixture_index());
}

fn write_store_with(path: &Path, index: Index) {
    fs::create_dir_all(path.parent().expect("store has a parent")).expect("store dir");
    let parts = index.snapshot().to_parts();
    write_checkpoint(
        path,
        &SaveInput {
            parts: &parts,
            volumes: &[],
            orders: &[],
            clean_shutdown: true,
        },
    )
    .expect("checkpoint written");
    assert!(path.is_file());
}

/// A unique scratch folder with a store and a `$HOME`, removed on drop.
struct Scratch {
    dir: PathBuf,
}

impl Scratch {
    fn new(name: &str) -> Scratch {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "fx-cli-e2e-{name}-{}-{nanos}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(dir.join("home")).expect("scratch home");
        let s = Scratch { dir };
        write_store(&s.store());
        s
    }

    fn store(&self) -> PathBuf {
        self.dir.join("store").join("index.fxstore")
    }

    fn home(&self) -> PathBuf {
        self.dir.join("home")
    }

    /// Runs the binary on this scratch store with `args`.
    fn run(&self, args: &[&str]) -> Output {
        let mut all: Vec<String> = vec!["-store".into(), self.store().display().to_string()];
        all.extend(args.iter().map(|a| a.to_string()));
        self.run_raw(&all)
    }

    /// Runs the binary with exactly `args` and `$HOME` set to the scratch
    /// home.
    fn run_raw<S: AsRef<std::ffi::OsStr>>(&self, args: &[S]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_not-everything"))
            .args(args)
            .env("HOME", self.home())
            .output()
            .expect("fx-cli runs")
    }

    /// Like [`run_raw`](Self::run_raw), from the working folder `cwd`.
    fn run_in<S: AsRef<std::ffi::OsStr>>(&self, cwd: &Path, args: &[S]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_not-everything"))
            .args(args)
            .current_dir(cwd)
            .env("HOME", self.home())
            .output()
            .expect("fx-cli runs")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(self.dir.join("store"), fs::Permissions::from_mode(0o755));
            let _ = fs::set_permissions(self.store(), fs::Permissions::from_mode(0o644));
        }
        let _ = fs::remove_dir_all(&self.dir);
    }
}

// ------------------------------------------------------------ helpers

fn code(out: &Output) -> i32 {
    out.status
        .code()
        .unwrap_or_else(|| panic!("fx-cli was killed by a signal: {out:?}"))
}

fn stdout(out: &Output) -> String {
    String::from_utf8(out.stdout.clone()).expect("stdout is UTF-8")
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn ok(out: &Output) -> String {
    assert_eq!(code(out), 0, "exit 0 expected; stderr: {}", stderr(out));
    stdout(out)
}

/// The txt lines of a successful run.
fn lines(out: &Output) -> Vec<String> {
    let text = ok(out);
    if !text.is_empty() {
        assert!(text.ends_with('\n'), "txt ends with a newline: {text:?}");
    }
    text.lines().map(str::to_string).collect()
}

fn paths(s: &Scratch, args: &[&str]) -> Vec<String> {
    lines(&s.run(args))
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

// ------------------------------------------------------------ txt

#[test]
fn txt_is_the_default_one_full_path_per_line_sorted_by_name() {
    let s = Scratch::new("txt");
    assert_eq!(paths(&s, &["data"]), strs(&[DATA, META, OLD]));
    assert_eq!(paths(&s, &["-txt", "data"]), strs(&[DATA, META, OLD]));
}

#[test]
fn search_words_are_joined_into_one_query() {
    let s = Scratch::new("words");
    assert_eq!(paths(&s, &["data", "txt"]), strs(&[DATA, META]));
    assert_eq!(paths(&s, &["main.rs|Readme.md"]).len(), 2);
}

#[test]
fn no_matches_prints_nothing_and_succeeds() {
    let s = Scratch::new("none");
    let out = s.run(&["zzzz-no-such-name"]);
    assert_eq!(ok(&out), "");
}

// ------------------------------------------------------------ -0 and control characters

/// Names with control characters (a C1 CSI that some terminals read as the
/// start of an escape sequence, an escape sequence that would turn a
/// terminal red, a newline, a tab), in the order a name sort gives.
const CONTROL_FIXTURE: [&str; 5] = [
    "c1\u{9b}2J.txt",
    "esc\u{1b}[31mred.txt",
    "line\nbreak.txt",
    "plain.txt",
    "tab\there.txt",
];

/// How a terminal shows [`CONTROL_FIXTURE`]: each control character as `?`.
const CONTROL_SHOWN: [&str; 5] = [
    "c1?2J.txt",
    "esc?[31mred.txt",
    "line?break.txt",
    "plain.txt",
    "tab?here.txt",
];

/// A scratch store whose root `/` holds the folder `c` with the
/// [`CONTROL_FIXTURE`] names.
fn control_scratch() -> Scratch {
    let s = Scratch::new("controls");
    let mut index = Index::new();
    let entry = |k: u64, parent: Option<u64>, name: &str, kind: EntryKind| Entry {
        key: FileKey::new(VolumeId(1), k),
        parent: parent.map(|p| FileKey::new(VolumeId(1), p)),
        name: name.to_string(),
        kind,
        flags: EntryFlags::empty(),
        size: 0,
        modified: 1_600_000_000,
        created: 1_500_000_000,
    };
    index
        .upsert(entry(1, None, "/", EntryKind::Dir))
        .expect("root");
    index
        .upsert(entry(2, Some(1), "c", EntryKind::Dir))
        .expect("folder c");
    for (i, name) in CONTROL_FIXTURE.iter().enumerate() {
        index
            .upsert(entry(10 + i as u64, Some(2), name, EntryKind::File))
            .expect("file");
    }
    write_store_with(&s.store(), index);
    s
}

/// `names` as full paths under `/c`, each followed by `end`.
fn ended(names: &[&str], end: &str) -> Vec<u8> {
    names
        .iter()
        .flat_map(|n| format!("/c/{n}{end}").into_bytes())
        .collect()
}

/// The parsed csv of `names` as full paths under `/c`: the header, then
/// one record per name.
fn csv_records(names: &[&str]) -> Vec<Vec<String>> {
    std::iter::once(strs(&["full_path"]))
        .chain(names.iter().map(|n| vec![format!("/c/{n}")]))
        .collect()
}

/// The `full_path` values of a json result array.
fn json_full_paths(text: &str) -> Vec<Json> {
    objects(&parse_json(text))
        .into_iter()
        .map(|o| o["full_path"].clone())
        .collect()
}

/// [`json_full_paths`] for `names` under `/c`.
fn json_paths_of(names: &[&str]) -> Vec<Json> {
    names.iter().map(|n| Json::Str(format!("/c/{n}"))).collect()
}

#[test]
fn nul_mode_ends_each_result_with_nul() {
    let s = Scratch::new("nul");
    let out = s.run(&["-0", "data"]);
    assert_eq!(code(&out), 0, "stderr {}", stderr(&out));
    assert_eq!(out.stdout, format!("{DATA}\0{META}\0{OLD}\0").into_bytes());

    // Several columns stay tab-separated.
    let out = s.run(&["--0", "-name", "-size", "-sort", "size", "data"]);
    assert_eq!(
        out.stdout,
        b"old data, v2.csv\t10\0metadata.txt\t100\0data.txt\t300\0".to_vec()
    );

    // No matches print nothing.
    let out = s.run(&["-0", "zzzz-nothing"]);
    assert_eq!(code(&out), 0);
    assert!(out.stdout.is_empty());

    // The count is not a result: it still ends with a newline.
    assert_eq!(ok(&s.run(&["-0", "-get-result-count", "data"])), "3\n");

    // An export file gets the same NUL ends.
    let file = s.dir.join("nul.txt");
    let out = s.run(&["-0", "-export-txt", file.to_str().unwrap(), "data"]);
    assert_eq!(ok(&out), "");
    assert_eq!(
        fs::read(&file).unwrap(),
        format!("{DATA}\0{META}\0{OLD}\0").into_bytes()
    );
}

#[test]
fn nul_mode_with_csv_or_json_exits_4() {
    let s = Scratch::new("nul4");
    let file = s.dir.join("out.csv");
    for args in [
        &["-0", "-csv", "data"][..],
        &["-json", "-0", "data"],
        &["-0", "-export-csv", file.to_str().unwrap(), "data"],
    ] {
        let out = s.run(args);
        assert_eq!(code(&out), 4, "{args:?}: stderr {}", stderr(&out));
        assert!(stderr(&out).contains("-0"), "{args:?}: {}", stderr(&out));
        assert!(out.stdout.is_empty(), "{args:?}");
    }
    assert!(!file.exists(), "no export file is created");
}

#[test]
fn control_characters_pass_through_a_pipe_and_a_file_exactly() {
    let s = control_scratch();
    let out = s.run(&["txt"]);
    assert_eq!(code(&out), 0, "stderr {}", stderr(&out));
    assert_eq!(out.stdout, ended(&CONTROL_FIXTURE, "\n"));

    // With -0 every name, newline included, can be split off exactly.
    let out = s.run(&["-0", "txt"]);
    let names: Vec<&[u8]> = out.stdout.split(|&b| b == 0).collect();
    assert_eq!(names.len(), CONTROL_FIXTURE.len() + 1, "{:?}", out.stdout);
    for (got, name) in names.iter().zip(CONTROL_FIXTURE) {
        assert_eq!(*got, format!("/c/{name}").as_bytes());
    }

    let file = s.dir.join("controls.txt");
    ok(&s.run(&["-export-txt", file.to_str().unwrap(), "txt"]));
    assert_eq!(fs::read(&file).unwrap(), ended(&CONTROL_FIXTURE, "\n"));

    // csv keeps the exact names too, quoted where RFC 4180 says.
    let out = ok(&s.run(&["-csv", "txt"]));
    assert!(out.contains("\"/c/line\nbreak.txt\""), "{out:?}");
    assert_eq!(parse_csv(&out), csv_records(&CONTROL_FIXTURE));
    let file = s.dir.join("controls.csv");
    ok(&s.run(&["-export-csv", file.to_str().unwrap(), "txt"]));
    let exported = fs::read_to_string(&file).unwrap();
    assert_eq!(exported, out);

    // json escapes every control character, and parses back to the names.
    let out = ok(&s.run(&["-json", "txt"]));
    assert!(out.contains("/c/c1\\u009b2J.txt"), "{out:?}");
    assert_eq!(json_full_paths(&out), json_paths_of(&CONTROL_FIXTURE));
}

/// Runs the binary on `s`'s store with `args` inside a pseudo-terminal
/// (macOS `script`), so its standard output is a terminal. `None` when
/// `script` is missing.
#[cfg(target_os = "macos")]
fn run_on_a_terminal(s: &Scratch, args: &[&str]) -> Option<Output> {
    let script = Path::new("/usr/bin/script");
    if !script.is_file() {
        return None;
    }
    let out = Command::new(script)
        .arg("-q")
        .arg("/dev/null")
        .arg(env!("CARGO_BIN_EXE_not-everything"))
        .arg("-store")
        .arg(s.store())
        .args(args)
        .env("HOME", s.home())
        .output()
        .expect("script runs");
    Some(out)
}

#[cfg(target_os = "macos")]
#[test]
fn control_characters_show_as_question_marks_on_a_terminal() {
    let s = control_scratch();
    let Some(out) = run_on_a_terminal(&s, &["txt"]) else {
        eprintln!("skipped: /usr/bin/script is missing");
        return;
    };
    assert_eq!(code(&out), 0, "{out:?}");
    // The terminal turns each `\n` into `\r\n`; `script` may also echo the
    // end of its empty input first.
    let text = String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n");
    let shown = String::from_utf8(ended(&CONTROL_SHOWN, "\n")).unwrap();
    assert!(text.ends_with(&shown), "{text:?}");
    assert!(
        !text.contains('\u{1b}'),
        "no escape reaches the terminal: {text:?}"
    );
    assert!(!text.contains("line\nbreak"), "{text:?}");

    // `-0` keeps the NUL ends and still shows the names safely.
    let out = run_on_a_terminal(&s, &["-0", "txt"]).expect("script ran before");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let shown = String::from_utf8(ended(&CONTROL_SHOWN, "\0")).unwrap();
    assert!(text.ends_with(&shown), "{text:?}");

    // An export file is exact even when standard output is a terminal.
    let file = s.dir.join("from-a-terminal.txt");
    let out = run_on_a_terminal(&s, &["-export-txt", file.to_str().unwrap(), "txt"])
        .expect("script ran before");
    assert_eq!(code(&out), 0, "{out:?}");
    assert_eq!(fs::read(&file).unwrap(), ended(&CONTROL_FIXTURE, "\n"));

    // csv shows them as `?` too. Each record ends in CRLF, which the
    // terminal writes as CR CR LF, so the CRs are dropped to compare.
    let out = run_on_a_terminal(&s, &["-csv", "txt"]).expect("script ran before");
    assert_eq!(code(&out), 0, "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout).replace('\r', "");
    let shown = String::from_utf8(ended(&CONTROL_SHOWN, "\n")).unwrap();
    assert!(text.ends_with(&format!("full_path\n{shown}")), "{text:?}");
    assert!(
        !text.contains(['\u{1b}', '\u{9b}', '\t', '"']),
        "no control character reaches the terminal, and none is quoted: {text:?}"
    );

    // A csv export file is exact even when standard output is a terminal.
    let file = s.dir.join("from-a-terminal.csv");
    let out = run_on_a_terminal(&s, &["-export-csv", file.to_str().unwrap(), "txt"])
        .expect("script ran before");
    assert_eq!(code(&out), 0, "{out:?}");
    let exported = fs::read_to_string(&file).unwrap();
    assert_eq!(parse_csv(&exported), csv_records(&CONTROL_FIXTURE));

    // json escapes them on a terminal as everywhere else, so only the
    // newlines between objects (and the CRs the terminal adds) are raw.
    let out = run_on_a_terminal(&s, &["-json", "txt"]).expect("script ran before");
    assert_eq!(code(&out), 0, "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let json = &text[text.find('[').expect("a json array")..];
    assert!(
        !json
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\r' | '\n')),
        "{json:?}"
    );
    assert_eq!(
        json_full_paths(&json.replace('\r', "")),
        json_paths_of(&CONTROL_FIXTURE)
    );
}

// ------------------------------------------------------------ csv

#[test]
fn csv_has_a_header_and_one_record_per_result() {
    let s = Scratch::new("csv");
    let rows = parse_csv(&ok(&s.run(&["-csv", "data"])));
    assert_eq!(rows[0], strs(&["full_path"]));
    assert_eq!(
        rows[1..].to_vec(),
        vec![strs(&[DATA]), strs(&[META]), strs(&[OLD])]
    );
}

#[test]
fn csv_columns_follow_the_options_and_quote_commas() {
    let s = Scratch::new("csvcols");
    let rows = parse_csv(&ok(
        &s.run(&["-csv", "-name", "-size", "-sort", "size", "data"])
    ));
    assert_eq!(rows[0], strs(&["name", "size"]));
    assert_eq!(
        rows[1..].to_vec(),
        vec![
            strs(&["old data, v2.csv", "10"]),
            strs(&["metadata.txt", "100"]),
            strs(&["data.txt", "300"]),
        ]
    );
}

// ------------------------------------------------------------ json

#[test]
fn json_is_an_array_of_objects_with_the_chosen_columns() {
    let s = Scratch::new("json");
    let out = ok(&s.run(&[
        "-json",
        "-full-path-and-name",
        "-name",
        "-size",
        "-sort",
        "size",
        "-sort-descending",
        "data",
    ]));
    let objs = objects(&parse_json(&out));
    assert_eq!(objs.len(), 3);
    let want = [
        (DATA, "data.txt", 300.0),
        (META, "metadata.txt", 100.0),
        (OLD, "old data, v2.csv", 10.0),
    ];
    for (obj, (full, name, size)) in objs.iter().zip(want) {
        assert_eq!(obj.len(), 3, "only the chosen columns: {obj:?}");
        assert_eq!(obj["full_path"], Json::Str(full.into()));
        assert_eq!(obj["name"], Json::Str(name.into()));
        assert_eq!(obj["size"], Json::Num(size));
    }
}

#[test]
fn json_default_column_is_the_full_path() {
    let s = Scratch::new("jsondef");
    let objs = objects(&parse_json(&ok(&s.run(&["-json", "readme"]))));
    assert!(objs.iter().all(|o| o.len() == 1), "one column: {objs:?}");
    let got: Vec<String> = objs
        .iter()
        .map(|o| match &o["full_path"] {
            Json::Str(p) => p.clone(),
            other => panic!("full_path is a string: {other:?}"),
        })
        .collect();
    assert_eq!(sorted(got), strs(&["/docs/Readme.md", "/src/readme.txt"]));
}

#[test]
fn json_with_no_results_is_an_empty_array() {
    let s = Scratch::new("jsonnone");
    assert_eq!(
        parse_json(&ok(&s.run(&["-json", "zzzz-nothing"]))),
        Json::Arr(vec![])
    );
}

// ------------------------------------------------------------ sorting and paging

#[test]
fn sort_by_size_date_and_path_in_both_directions() {
    let s = Scratch::new("sort");
    assert_eq!(
        paths(&s, &["-sort", "size", "data"]),
        strs(&[OLD, META, DATA])
    );
    assert_eq!(
        paths(&s, &["-sort", "size", "-sort-descending", "data"]),
        strs(&[DATA, META, OLD])
    );
    assert_eq!(
        paths(&s, &["-sort", "date-modified", "data"]),
        strs(&[OLD, META, DATA])
    );
    assert_eq!(
        paths(&s, &["-sort", "date-created", "-sort-descending", "data"]),
        strs(&[DATA, META, OLD])
    );
    assert_eq!(
        paths(&s, &["-s", "readme"]),
        strs(&["/docs/Readme.md", "/src/readme.txt"])
    );
    assert_eq!(
        paths(&s, &["-s", "-sort-descending", "readme"]),
        strs(&["/src/readme.txt", "/docs/Readme.md"])
    );
    assert_eq!(
        paths(&s, &["-sort", "name", "-sort-descending", "data"]),
        strs(&[OLD, META, DATA])
    );
}

#[test]
fn max_results_and_offset_page_the_sorted_results() {
    let s = Scratch::new("page");
    assert_eq!(
        paths(&s, &["-sort", "size", "-n", "2", "data"]),
        strs(&[OLD, META])
    );
    assert_eq!(
        paths(&s, &["-sort", "size", "-o", "1", "data"]),
        strs(&[META, DATA])
    );
    assert_eq!(
        paths(&s, &["-sort", "size", "-o", "1", "-n", "1", "data"]),
        strs(&[META])
    );
    assert_eq!(paths(&s, &["-o", "10", "data"]), Vec::<String>::new());
    assert_eq!(paths(&s, &["-n", "0", "data"]), Vec::<String>::new());
    let rows = parse_csv(&ok(&s.run(&["-csv", "-sort", "size", "-n", "1", "data"])));
    assert_eq!(rows, vec![strs(&["full_path"]), strs(&[OLD])]);
}

// ------------------------------------------------------------ toggles

#[test]
fn match_case_toggle() {
    let s = Scratch::new("case");
    assert_eq!(
        sorted(paths(&s, &["readme"])),
        strs(&["/docs/Readme.md", "/src/readme.txt"])
    );
    assert_eq!(paths(&s, &["-i", "Readme"]), strs(&["/docs/Readme.md"]));
    assert_eq!(paths(&s, &["-case", "readme"]), strs(&["/src/readme.txt"]));
}

#[test]
fn whole_word_toggle() {
    let s = Scratch::new("ww");
    assert_eq!(paths(&s, &["-w", "data"]), strs(&[DATA, OLD]));
    assert_eq!(paths(&s, &["-whole-word", "data"]), strs(&[DATA, OLD]));
}

#[test]
fn match_path_toggle() {
    let s = Scratch::new("path");
    assert_eq!(paths(&s, &["docs", "data"]), Vec::<String>::new());
    assert_eq!(
        sorted(paths(&s, &["-p", "docs", "data"])),
        strs(&[DATA, META])
    );
    assert_eq!(
        sorted(paths(&s, &["-match-path", "src", "txt"])),
        strs(&["/src/readme.txt"])
    );
}

#[test]
fn regex_toggle() {
    let s = Scratch::new("regex");
    assert_eq!(paths(&s, &["-r", "^data"]), strs(&[DATA]));
    assert_eq!(
        sorted(paths(&s, &["-regex", r"\.(md|rs)$"])),
        strs(&["/docs/Readme.md", "/src/main.rs"])
    );
}

/// The txt paths of a run that must succeed, sorted.
fn found(s: &Scratch, args: &[&str]) -> Vec<String> {
    sorted(paths(s, args))
}

/// Names chosen so each `-r` quoting case matches exactly one of them.
const QUOTING_FIXTURE: &[&str] = &[
    "wow b.txt",
    "wow!<b>.txt",
    "say \"hi\".txt",
    r#"back\"slash.txt"#,
    r"back\slash.txt",
    "plain.txt",
];

/// A scratch store whose root `/` holds the folder `q` with the
/// [`QUOTING_FIXTURE`] names.
fn quoting_scratch() -> Scratch {
    let s = Scratch::new("rquote");
    let mut index = Index::new();
    let dir = |k: u64, parent: Option<u64>, name: &str| Entry {
        key: FileKey::new(VolumeId(1), k),
        parent: parent.map(|p| FileKey::new(VolumeId(1), p)),
        name: name.to_string(),
        kind: EntryKind::Dir,
        flags: EntryFlags::empty(),
        size: 0,
        modified: 1_600_000_000,
        created: 1_500_000_000,
    };
    index.upsert(dir(1, None, "/")).expect("root");
    index.upsert(dir(2, Some(1), "q")).expect("folder q");
    for (i, name) in QUOTING_FIXTURE.iter().enumerate() {
        let k = 10 + i as u64;
        index
            .upsert(Entry {
                kind: EntryKind::File,
                size: 1 + k,
                ..dir(k, Some(2), name)
            })
            .expect("file");
    }
    write_store_with(&s.store(), index);
    s
}

#[test]
fn regex_with_a_space_is_one_pattern() {
    let s = quoting_scratch();
    // As a plain query `wow b` would be `wow AND b` and also find `wow!<b>`.
    assert_eq!(found(&s, &["-r", "wow b"]), strs(&["/q/wow b.txt"]));
    assert_eq!(found(&s, &["-r", "^wow b\\.txt$"]), strs(&["/q/wow b.txt"]));
}

#[test]
fn regex_with_a_literal_double_quote() {
    let s = quoting_scratch();
    assert_eq!(found(&s, &["-r", "\"hi\""]), strs(&["/q/say \"hi\".txt"]));
    // A quote after a space.
    assert_eq!(found(&s, &["-r", "y \"h"]), strs(&["/q/say \"hi\".txt"]));
    // Anchored, with a quote just before an escaped dot.
    assert_eq!(
        found(&s, &["-r", "^say \"hi\"\\.txt$"]),
        strs(&["/q/say \"hi\".txt"])
    );
}

#[test]
fn regex_with_an_escaped_double_quote() {
    let s = quoting_scratch();
    // `\"` is an escaped quote in the regex: a literal `"`.
    assert_eq!(
        found(&s, &["-r", r#"\"hi\""#]),
        strs(&["/q/say \"hi\".txt"])
    );
    assert_eq!(
        found(&s, &["-r", r#"^say \"hi\"\.txt$"#]),
        strs(&["/q/say \"hi\".txt"])
    );
}

#[test]
fn regex_with_backslash_runs_before_a_quote() {
    let s = quoting_scratch();
    // Backslashes with no quote pass through: `\\` is one literal `\`.
    assert_eq!(found(&s, &["-r", r"k\\s"]), strs(&[r"/q/back\slash.txt"]));
    // An even run (`\\`, a literal backslash) and then an unescaped `"`.
    assert_eq!(
        found(&s, &["-r", r#"k\\"s"#]),
        strs(&[r#"/q/back\"slash.txt"#])
    );
    assert_eq!(
        found(&s, &["-r", r#"^back\\"slash\.txt$"#]),
        strs(&[r#"/q/back\"slash.txt"#])
    );
    // An odd run (`\\` then `\"`): a literal backslash and a literal quote.
    assert_eq!(
        found(&s, &["-r", r#"k\\\"s"#]),
        strs(&[r#"/q/back\"slash.txt"#])
    );
    // The quote must follow the backslash: the quote-free name is not hit.
    assert_eq!(
        found(&s, &["-r", r#"\\""#]),
        strs(&[r#"/q/back\"slash.txt"#])
    );
}

#[test]
fn regex_with_bang_and_angle_brackets() {
    let s = quoting_scratch();
    assert_eq!(found(&s, &["-r", "!<b"]), strs(&["/q/wow!<b>.txt"]));
    assert_eq!(found(&s, &["-r", "<b>"]), strs(&["/q/wow!<b>.txt"]));
    assert_eq!(found(&s, &["-r", "^wow!"]), strs(&["/q/wow!<b>.txt"]));
    // `!` is not NOT: as a regex `!plain` matches the text `!plain` only.
    assert_eq!(found(&s, &["-r", "!plain"]), Vec::<String>::new());
    // `<` and `|` are not grouping or OR in the query syntax: `|` is the
    // regex alternation.
    assert_eq!(
        found(&s, &["-r", "^plain|<b>"]),
        strs(&["/q/plain.txt", "/q/wow!<b>.txt"])
    );
}

// ------------------------------------------------------------ apostrophes

/// A book title with the curly apostrophe macOS types.
const PLUMB: &str = "Plumb’s Veterinary Drug Handbook 10th Edition.pdf";
const PLUMB_DOWNLOADS: &str = "/Downloads/Plumb’s Veterinary Drug Handbook 10th Edition.pdf";
const PLUMB_WORK: &str = "/Desktop/Work/Plumb’s Veterinary Drug Handbook 10th Edition.pdf";

/// A scratch store with the handbook in `/Downloads` and a copy in
/// `/Desktop/Work`, next to `/docs/plumb.txt` and `/docs/notes.txt`.
fn apostrophe_scratch() -> Scratch {
    let s = Scratch::new("apostrophe");
    let mut index = Index::new();
    let entry = |k: u64, parent: Option<u64>, name: &str, kind: EntryKind| Entry {
        key: FileKey::new(VolumeId(1), k),
        parent: parent.map(|p| FileKey::new(VolumeId(1), p)),
        name: name.to_string(),
        kind,
        flags: EntryFlags::empty(),
        size: 10 + k,
        modified: 1_600_000_000,
        created: 1_500_000_000,
    };
    let (dir, file) = (EntryKind::Dir, EntryKind::File);
    for e in [
        entry(1, None, "/", dir),
        entry(2, Some(1), "Downloads", dir),
        entry(3, Some(2), PLUMB, file),
        entry(4, Some(1), "Desktop", dir),
        entry(5, Some(4), "Work", dir),
        entry(6, Some(5), PLUMB, file),
        entry(7, Some(1), "docs", dir),
        entry(8, Some(7), "plumb.txt", file),
        entry(9, Some(7), "notes.txt", file),
    ] {
        index.upsert(e).expect("entry");
    }
    write_store_with(&s.store(), index);
    s
}

#[test]
fn plumbs_finds_the_handbook_whatever_the_apostrophe() {
    let s = apostrophe_scratch();
    let both = strs(&[PLUMB_WORK, PLUMB_DOWNLOADS]);
    for query in ["plumbs", "plumb's", "plumb’s", "PLUMB‘S", "plumbs handbook"] {
        assert_eq!(found(&s, &[query]), both, "{query:?}");
    }
    assert_eq!(ok(&s.run(&["-get-result-count", "plumbs"])).trim(), "2");
    // A query of only an apostrophe finds the names that contain one.
    assert_eq!(found(&s, &["'"]), both);
    assert_eq!(found(&s, &["’"]), both);
    // Whole words come from the folded name, `plumbs`.
    assert_eq!(found(&s, &["ww:plumb"]), strs(&["/docs/plumb.txt"]));
    assert_eq!(found(&s, &["-w", "plumbs"]), both);
    assert_eq!(found(&s, &["-w", "plumb's"]), both);
}

#[test]
fn diacritics_and_regex_keep_the_apostrophe() {
    let s = apostrophe_scratch();
    let both = strs(&[PLUMB_WORK, PLUMB_DOWNLOADS]);
    let none = Vec::<String>::new();
    for flag in ["-a", "-diacritics"] {
        assert_eq!(found(&s, &[flag, "plumbs"]), none, "{flag}");
        assert_eq!(found(&s, &[flag, "plumb's"]), none, "{flag}");
        assert_eq!(found(&s, &[flag, "plumb’s"]), both, "{flag}");
        assert_eq!(found(&s, &[flag, "'"]), none, "{flag}");
    }
    assert_eq!(found(&s, &["diacritics:plumbs"]), none);
    assert_eq!(found(&s, &["regex:plumb.?s"]), both);
    assert_eq!(found(&s, &["-r", "plumb.?s"]), both);
    assert_eq!(found(&s, &["regex:plumbs"]), none);
    assert_eq!(found(&s, &["-r", "plumb's"]), none);
}

/// The apostrophe rule: `/docs` holds "plumbs.pdf", "Plumb's.pdf"
/// (straight apostrophe) and "Plumb’s.pdf" (curly, as macOS types it).
fn apostrophe_rule_scratch() -> Scratch {
    let s = Scratch::new("apostrophe-rule");
    let mut index = Index::new();
    let entry = |k: u64, parent: Option<u64>, name: &str, kind: EntryKind| Entry {
        key: FileKey::new(VolumeId(1), k),
        parent: parent.map(|p| FileKey::new(VolumeId(1), p)),
        name: name.to_string(),
        kind,
        flags: EntryFlags::empty(),
        size: 10 + k,
        modified: 1_600_000_000,
        created: 1_500_000_000,
    };
    let (dir, file) = (EntryKind::Dir, EntryKind::File);
    for e in [
        entry(1, None, "/", dir),
        entry(2, Some(1), "docs", dir),
        entry(3, Some(2), "plumbs.pdf", file),
        entry(4, Some(2), "Plumb's.pdf", file),
        entry(5, Some(2), "Plumb’s.pdf", file),
    ] {
        index.upsert(e).expect("entry");
    }
    write_store_with(&s.store(), index);
    s
}

#[test]
fn a_term_with_an_apostrophe_needs_one_in_the_name() {
    let s = apostrophe_rule_scratch();
    let all = sorted(strs(&[
        "/docs/plumbs.pdf",
        "/docs/Plumb's.pdf",
        "/docs/Plumb’s.pdf",
    ]));
    let with = sorted(strs(&["/docs/Plumb's.pdf", "/docs/Plumb’s.pdf"]));
    assert_eq!(sorted(found(&s, &["plumbs"])), all);
    for query in ["plumb's", "plumb’s", "PLUMB'S", "plumbʼs", "plumb‘s"] {
        assert_eq!(sorted(found(&s, &[query])), with, "{query:?}");
    }
    assert_eq!(ok(&s.run(&["-get-result-count", "plumb's"])).trim(), "2");
    // Whole words, wildcards and paths follow the same rule.
    assert_eq!(sorted(found(&s, &["-w", "plumbs"])), all);
    assert_eq!(sorted(found(&s, &["-w", "plumb's"])), with);
    assert_eq!(found(&s, &["ww:plumb"]), Vec::<String>::new());
    assert_eq!(sorted(found(&s, &["plumb's*"])), with);
    assert_eq!(sorted(found(&s, &["plumbs*"])), all);
    assert_eq!(sorted(found(&s, &["-p", "docs/plumb's"])), with);
    // Only apostrophes: any name with one.
    assert_eq!(sorted(found(&s, &["'"])), with);
    // `-a` keeps the exact text, so the two kinds differ there.
    assert_eq!(found(&s, &["-a", "plumbs"]), strs(&["/docs/plumbs.pdf"]));
    assert_eq!(found(&s, &["-a", "plumb's"]), strs(&["/docs/Plumb's.pdf"]));
    assert_eq!(found(&s, &["-a", "plumb’s"]), strs(&["/docs/Plumb’s.pdf"]));
    // A regular expression matches the names as written.
    assert_eq!(found(&s, &["-r", "plumb's"]), strs(&["/docs/Plumb's.pdf"]));
    assert_eq!(sorted(found(&s, &["-r", "plumb.?s"])), all);
}

/// Checks that `args` fail with exit 7: a reason on stderr and nothing on
/// stdout.
fn assert_query_failed(s: &Scratch, args: &[&str]) {
    let out = s.run(args);
    assert_eq!(
        code(&out),
        7,
        "{args:?}: exit 7 expected; stdout {:?}, stderr {}",
        stdout(&out),
        stderr(&out)
    );
    assert!(
        stdout(&out).is_empty(),
        "{args:?} prints no results: {:?}",
        stdout(&out)
    );
    assert!(
        !stderr(&out).trim().is_empty(),
        "{args:?} says on stderr why the query failed"
    );
}

#[test]
fn exit_7_for_a_bad_query() {
    let s = Scratch::new("badquery");
    // The `-r` pattern does not compile.
    assert_query_failed(&s, &["-r", "("]);
    assert_query_failed(&s, &["-r", "[unclosed("]);
    // In every output mode, and for the count alone.
    assert_query_failed(&s, &["-csv", "-r", "("]);
    assert_query_failed(&s, &["-json", "-r", "("]);
    assert_query_failed(&s, &["-get-result-count", "-r", "("]);
    assert_query_failed(&s, &["-no-result-error", "-r", "("]);
    // A `regex:` term that does not compile, without `-r`, alone or next to
    // valid terms.
    assert_query_failed(&s, &["regex:[bad"]);
    assert_query_failed(&s, &["data", "regex:[bad"]);
    assert_query_failed(&s, &["data | regex:[bad"]);

    // A valid `regex:` term searches normally.
    assert_eq!(paths(&s, &["regex:^data"]), strs(&[DATA]));
}

#[test]
fn malformed_queries_do_not_crash() {
    let s = Scratch::new("malformed");
    for args in [&["\"unclosed"][..], &["<a|"], &["size:abc"]] {
        let out = s.run(args);
        assert_eq!(
            code(&out),
            0,
            "{args:?}: exit 0 expected, stderr {}",
            stderr(&out)
        );
    }

    // An invalid regex is an error (exit 7), never a crash.
    for pattern in ["(", "[unclosed("] {
        let out = s.run(&["-r", pattern]);
        assert_eq!(code(&out), 7, "-r {pattern:?}: stderr {}", stderr(&out));
        assert!(
            stdout(&out).is_empty(),
            "-r {pattern:?} prints no results: {:?}",
            stdout(&out)
        );
        assert!(
            !stderr(&out).trim().is_empty(),
            "-r {pattern:?} says on stderr that the pattern is invalid"
        );
    }

    // An unclosed quote still searches, and warns on stderr.
    let out = s.run(&["\"unclosed"]);
    assert_eq!(code(&out), 0, "stderr {}", stderr(&out));
    assert!(
        !stderr(&out).trim().is_empty(),
        "an unclosed quote warns on stderr"
    );

    // A valid regex gives no warning.
    let out = s.run(&["-r", "^data"]);
    assert_eq!(stdout(&out).lines().count(), 1);
    assert!(
        stderr(&out).trim().is_empty(),
        "a valid -r pattern warns about nothing: {}",
        stderr(&out)
    );
}

// ------------------------------------------------------------ count: cap

#[test]
fn count_caps_the_total_before_paging() {
    let s = Scratch::new("countcap");
    // By name: DATA, META, OLD. By size: OLD (10), META (100), DATA (300).
    assert_eq!(paths(&s, &["count:2", "data"]), strs(&[DATA, META]));
    assert_eq!(paths(&s, &["-n", "1", "count:2", "data"]), strs(&[DATA]));
    // -o skips within the first N results; it does not slide the cap.
    assert_eq!(paths(&s, &["-o", "1", "count:2", "data"]), strs(&[META]));
    assert_eq!(
        paths(&s, &["-sort", "size", "-o", "1", "count:2", "data"]),
        strs(&[META])
    );
    assert_eq!(
        paths(&s, &["-o", "2", "count:2", "data"]),
        Vec::<String>::new()
    );
    assert_eq!(
        paths(&s, &["-o", "5", "count:2", "data"]),
        Vec::<String>::new()
    );
    assert_eq!(
        paths(&s, &["-o", "1", "-n", "5", "count:3", "data"]),
        strs(&[META, OLD])
    );
    assert_eq!(
        paths(&s, &["-o", "1", "-n", "5", "count:10", "data"]),
        strs(&[META, OLD])
    );
    // The same cap in csv.
    let rows = parse_csv(&ok(&s.run(&["-csv", "-o", "1", "count:2", "data"])));
    assert_eq!(rows, vec![strs(&["full_path"]), strs(&[META])]);
}

#[test]
fn count_caps_the_result_count() {
    let s = Scratch::new("countnum");
    let count = |args: &[&str]| ok(&s.run(args)).trim().to_string();
    assert_eq!(count(&["-get-result-count", "count:2", "data"]), "2");
    assert_eq!(count(&["-get-result-count", "count:3", "data"]), "3");
    assert_eq!(count(&["-get-result-count", "count:10", "data"]), "3");
    assert_eq!(count(&["-get-result-count", "data"]), "3");
    // Paging does not change the count.
    assert_eq!(
        count(&["-get-result-count", "-o", "1", "count:2", "data"]),
        "2"
    );
    assert_eq!(
        count(&["-get-result-count", "-n", "1", "count:2", "data"]),
        "2"
    );
}

// ------------------------------------------------------------ count

#[test]
fn get_result_count_prints_only_the_total() {
    let s = Scratch::new("count");
    assert_eq!(ok(&s.run(&["-get-result-count", "data"])).trim(), "3");
    assert_eq!(ok(&s.run(&["-get-result-count", "readme"])).trim(), "2");
    assert_eq!(
        ok(&s.run(&["-get-result-count", "zzzz-nothing"])).trim(),
        "0"
    );
}

// ------------------------------------------------------------ exit codes

#[test]
fn exit_0_for_help() {
    let s = Scratch::new("help");
    let out = s.run_raw(&["-h"]);
    assert!(ok(&out).to_lowercase().contains("usage"));
}

#[test]
fn help_and_version_name_the_command() {
    let s = Scratch::new("names");
    let command = Channel::CURRENT.command_name();
    if !Channel::CURRENT.is_preview() {
        assert_eq!(command, "not-everything");
    }
    let help = ok(&s.run_raw(&["-h"]));
    assert!(
        help.starts_with(&format!("usage: {command} [options] ")),
        "help: {help:?}"
    );
    assert!(!help.contains("fx-cli"), "help: {help:?}");
    // `-licenses` prints the third-party notices the command ships with,
    // exactly as the repository holds them.
    let licenses = ok(&s.run_raw(&["-licenses"]));
    let notices = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../THIRD-PARTY-NOTICES.md"),
    )
    .expect("THIRD-PARTY-NOTICES.md exists");
    assert_eq!(licenses, notices);
    assert!(help.contains("-licenses"), "help: {help:?}");
    let version = ok(&s.run_raw(&["-version"]));
    let label = version_label(env!("CARGO_PKG_VERSION"), Channel::CURRENT, CURRENT_NUMBER);
    assert_eq!(version, format!("{command} {label}\n"));
    let bad = s.run_raw(&["-bogus"]);
    assert!(
        stderr(&bad).starts_with(&format!("{command}: ")),
        "stderr: {:?}",
        stderr(&bad)
    );
    assert!(
        stderr(&bad).contains(&format!("run {command} -h for help")),
        "stderr: {:?}",
        stderr(&bad)
    );
}

#[test]
fn exit_4_for_a_missing_or_invalid_option_argument() {
    let s = Scratch::new("exit4");
    for args in [
        &["data", "-n"][..],
        &["-n", "abc", "data"],
        &["-o", "-3", "data"],
        &["-sort", "colour", "data"],
        &["data", "-export-csv"],
    ] {
        let out = s.run(args);
        assert_eq!(code(&out), 4, "{args:?}: stderr {}", stderr(&out));
        assert!(!stderr(&out).trim().is_empty(), "{args:?}: message");
        assert!(stdout(&out).is_empty(), "{args:?}: no results printed");
    }
}

#[test]
fn exit_5_when_the_export_file_cannot_be_created() {
    let s = Scratch::new("exit5");
    let missing = s.dir.join("no-such-dir").join("out.csv");
    let out = s.run(&["-export-csv", missing.to_str().unwrap(), "data"]);
    assert_eq!(code(&out), 5, "stderr {}", stderr(&out));
    assert!(!missing.exists());
    let a_dir = s.home();
    let out = s.run(&["-export-txt", a_dir.to_str().unwrap(), "data"]);
    assert_eq!(code(&out), 5, "stderr {}", stderr(&out));
}

#[test]
fn export_writes_the_file_instead_of_stdout() {
    let s = Scratch::new("export");
    let txt = s.dir.join("out.txt");
    let out = s.run(&["-export-txt", txt.to_str().unwrap(), "data"]);
    assert_eq!(ok(&out), "");
    let text = fs::read_to_string(&txt).expect("export written");
    assert_eq!(
        text.lines().map(str::to_string).collect::<Vec<_>>(),
        strs(&[DATA, META, OLD])
    );

    let csv = s.dir.join("out.csv");
    ok(&s.run(&[
        "-export-csv",
        csv.to_str().unwrap(),
        "-sort",
        "size",
        "data",
    ]));
    let rows = parse_csv(&fs::read_to_string(&csv).expect("csv written"));
    assert_eq!(
        rows,
        vec![
            strs(&["full_path"]),
            strs(&[OLD]),
            strs(&[META]),
            strs(&[DATA])
        ]
    );

    let json = s.dir.join("out.json");
    ok(&s.run(&["-export-json", json.to_str().unwrap(), "-name", "readme"]));
    let objs = objects(&parse_json(
        &fs::read_to_string(&json).expect("json written"),
    ));
    assert_eq!(objs.len(), 2);
    assert!(objs.iter().all(|o| o.contains_key("name")));
}

#[test]
fn exit_6_for_an_unknown_option() {
    let s = Scratch::new("exit6");
    let out = s.run(&["-frobnicate", "data"]);
    assert_eq!(code(&out), 6, "stderr {}", stderr(&out));
    assert!(!stderr(&out).trim().is_empty());
    assert!(stdout(&out).is_empty());
}

#[test]
fn exit_7_for_a_rejected_store() {
    let s = Scratch::new("exit7");
    let bad = s.dir.join("garbage.fxstore");
    fs::write(&bad, b"this is not an fx store, just some text bytes\n").unwrap();
    let out = s.run_raw(&[OsArg::from("-store"), bad.as_os_str().into(), "data".into()]);
    assert_eq!(code(&out), 7, "stderr {}", stderr(&out));
    assert!(!stderr(&out).trim().is_empty(), "a message on stderr");
    assert!(stdout(&out).is_empty());

    let good = fs::read(s.store()).unwrap();
    let truncated = s.dir.join("truncated.fxstore");
    fs::write(&truncated, &good[..good.len() / 2]).unwrap();
    let out = s.run_raw(&[
        OsArg::from("-store"),
        truncated.as_os_str().into(),
        "data".into(),
    ]);
    assert_eq!(code(&out), 7, "stderr {}", stderr(&out));
    assert!(!stderr(&out).trim().is_empty());
}

#[test]
fn exit_8_when_there_is_no_store() {
    let s = Scratch::new("exit8");
    let missing = s.dir.join("nowhere").join("index.fxstore");
    let out = s.run_raw(&[
        OsArg::from("-store"),
        missing.as_os_str().into(),
        "data".into(),
    ]);
    assert_eq!(code(&out), 8, "stderr {}", stderr(&out));
    assert!(!stderr(&out).trim().is_empty(), "a message on stderr");
    assert!(stdout(&out).is_empty());
    assert!(!missing.exists(), "nothing is created");

    // No -store and no app store under $HOME.
    let out = s.run_raw(&["data"]);
    assert_eq!(code(&out), 8, "stderr {}", stderr(&out));
}

#[test]
fn exit_9_for_no_results_with_no_result_error() {
    let s = Scratch::new("exit9");
    let out = s.run(&["-no-result-error", "zzzz-nothing"]);
    assert_eq!(code(&out), 9, "stderr {}", stderr(&out));
    assert!(stdout(&out).is_empty());
    assert_eq!(
        paths(&s, &["-no-result-error", "data"]),
        strs(&[DATA, META, OLD])
    );
}

// ------------------------------------------------------------ default store path

#[test]
fn default_store_is_found_under_home_for_the_root() {
    let s = Scratch::new("default");
    let bytes = fs::read(s.store()).unwrap();

    let for_slash = default_store_path(&s.home(), Path::new("/"));
    fs::create_dir_all(for_slash.parent().unwrap()).unwrap();
    fs::write(&for_slash, &bytes).unwrap();
    assert_eq!(lines(&s.run_raw(&["data"])), strs(&[DATA, META, OLD]));

    let root = "/Users/someone/proj";
    let out = s.run_raw(&["-root", root, "data"]);
    assert_eq!(code(&out), 8, "no store for {root} yet");
    let for_root = default_store_path(&s.home(), Path::new(root));
    fs::write(&for_root, &bytes).unwrap();
    assert_eq!(
        lines(&s.run_raw(&["-root", root, "data"])),
        strs(&[DATA, META, OLD])
    );
}

// ------------------------------------------------------------ canonical root

/// A real folder `<scratch>/real/proj` and its canonical path. (On macOS
/// the temp folder is itself behind the `/var` -> `/private/var` symlink,
/// so the two differ even before any test-made symlink.)
fn folder_root(s: &Scratch) -> (PathBuf, PathBuf) {
    let root = s.dir.join("real").join("proj");
    fs::create_dir_all(&root).unwrap();
    let canonical = fs::canonicalize(&root).unwrap();
    (root, canonical)
}

/// Writes the app's `scope` store for the canonical folder root, holding the
/// fixture with its root entry named `proj`.
fn write_folder_store(s: &Scratch, canonical: &Path, scope: StoreScope) -> PathBuf {
    let path = store_path_for(&s.home(), canonical, scope);
    write_store_with(&path, fixture_index_rooted("proj"));
    path
}

/// The `data` results (sorted by name) for the folder root, prefixed with
/// the canonical root's parent.
fn data_under(canonical: &Path) -> Vec<String> {
    ["docs/data.txt", "docs/metadata.txt", "src/old data, v2.csv"]
        .iter()
        .map(|p| canonical.join(p).to_string_lossy().into_owned())
        .collect()
}

#[test]
fn a_root_with_a_trailing_slash_finds_the_canonical_store() {
    let s = Scratch::new("rootslash");
    let (root, canonical) = folder_root(&s);
    write_folder_store(&s, &canonical, StoreScope::Full);
    let expected = data_under(&canonical);
    for given in [
        format!("{}/", root.display()),
        format!("{}/", canonical.display()),
        format!("{}//", canonical.display()),
        format!("{}/../proj/", canonical.display()),
        canonical.display().to_string(),
        root.display().to_string(),
    ] {
        let out = s.run_raw(&["-root", given.as_str(), "data"]);
        assert_eq!(code(&out), 0, "-root {given}: stderr {}", stderr(&out));
        assert_eq!(lines(&out), expected, "-root {given}");
    }
}

#[test]
fn a_relative_root_finds_the_canonical_store() {
    let s = Scratch::new("rootrel");
    let (root, canonical) = folder_root(&s);
    write_folder_store(&s, &canonical, StoreScope::Full);
    let expected = data_under(&canonical);
    for (cwd, given) in [
        (s.dir.clone(), "real/proj"),
        (s.dir.clone(), "real/proj/"),
        (s.dir.clone(), "./real/../real/proj"),
        (s.dir.join("real"), "proj"),
        (root.clone(), "."),
    ] {
        let out = s.run_in(&cwd, &["-root", given, "data"]);
        assert_eq!(
            code(&out),
            0,
            "-root {given} in {cwd:?}: stderr {}",
            stderr(&out)
        );
        assert_eq!(lines(&out), expected, "-root {given} in {cwd:?}");
    }
}

#[cfg(unix)]
#[test]
fn a_symlinked_root_finds_the_canonical_store() {
    let s = Scratch::new("rootlink");
    let (root, canonical) = folder_root(&s);
    write_folder_store(&s, &canonical, StoreScope::Full);
    let link = s.dir.join("link");
    std::os::unix::fs::symlink(&root, &link).unwrap();
    let expected = data_under(&canonical);
    let out = s.run_raw(&[OsArg::from("-root"), link.as_os_str().into(), "data".into()]);
    assert_eq!(lines(&out), expected, "-root {link:?}");
    let with_slash = format!("{}/", link.display());
    let out = s.run_raw(&["-root", with_slash.as_str(), "data"]);
    assert_eq!(lines(&out), expected, "-root {with_slash}");
    let out = s.run_in(&s.dir, &["-root", "link", "data"]);
    assert_eq!(lines(&out), expected, "-root link (relative)");
}

#[test]
fn a_missing_root_is_used_as_given_and_still_exits_8() {
    let s = Scratch::new("rootmissing");
    let missing = s.dir.join("gone").join("proj");
    let given = missing.display().to_string();
    let out = s.run_raw(&["-root", given.as_str(), "data"]);
    assert_eq!(code(&out), 8, "stderr {}", stderr(&out));
    assert!(!stderr(&out).trim().is_empty(), "a message on stderr");
    assert!(stdout(&out).is_empty());
    let with_slash = format!("{given}/");
    let out = s.run_raw(&["-root", with_slash.as_str(), "data"]);
    assert_eq!(code(&out), 8, "stderr {}", stderr(&out));
    assert!(!missing.exists(), "nothing is created");

    // A store saved for the path as given is still found.
    write_store(&default_store_path(&s.home(), &missing));
    let out = s.run_raw(&["-root", given.as_str(), "-get-result-count", "data"]);
    assert_eq!(ok(&out).trim(), "3");
}

// ------------------------------------------------------------ limited store

/// Bytes the store reader rejects (exit 7), as in `exit_7_for_a_rejected_store`.
const GARBAGE: &[u8] = b"this is not an fx store, just some text bytes\n";

#[test]
fn the_limited_store_is_read_when_there_is_no_full_store() {
    let s = Scratch::new("limited");
    let (root, canonical) = folder_root(&s);
    let limited = write_folder_store(&s, &canonical, StoreScope::Limited);
    let before = fs::read(&limited).unwrap();
    let full = store_path_for(&s.home(), &canonical, StoreScope::Full);
    assert_ne!(limited, full);

    let given = format!("{}/", root.display());
    let out = s.run_raw(&["-root", given.as_str(), "data"]);
    assert_eq!(code(&out), 0, "stderr {}", stderr(&out));
    assert_eq!(lines(&out), data_under(&canonical));
    assert!(
        stderr(&out).to_lowercase().contains("limited"),
        "a note on stderr says the limited store is used: {:?}",
        stderr(&out)
    );

    let out = s.run_raw(&["-root", given.as_str(), "-get-result-count", "data"]);
    assert_eq!(stdout(&out).trim(), "3", "stderr {}", stderr(&out));

    // Read-only: the limited store is unchanged and no full store appears.
    assert_eq!(fs::read(&limited).unwrap(), before);
    assert!(!full.exists(), "the CLI does not write a full store");
}

#[test]
fn the_limited_store_for_the_volume_root_is_found() {
    let s = Scratch::new("limitedslash");
    write_store(&store_path_for(
        &s.home(),
        Path::new("/"),
        StoreScope::Limited,
    ));
    let out = s.run_raw(&["data"]);
    assert_eq!(lines(&out), strs(&[DATA, META, OLD]));
    assert!(stderr(&out).to_lowercase().contains("limited"));
}

#[test]
fn the_full_store_is_preferred_over_the_limited_store() {
    let s = Scratch::new("fullfirst");
    let (_root, canonical) = folder_root(&s);
    write_folder_store(&s, &canonical, StoreScope::Full);
    // A limited store that would be rejected: reading it would exit 7.
    let limited = store_path_for(&s.home(), &canonical, StoreScope::Limited);
    fs::create_dir_all(limited.parent().unwrap()).unwrap();
    fs::write(&limited, GARBAGE).unwrap();

    let given = canonical.display().to_string();
    let out = s.run_raw(&["-root", given.as_str(), "data"]);
    assert_eq!(code(&out), 0, "stderr {}", stderr(&out));
    assert_eq!(lines(&out), data_under(&canonical));
    assert!(
        !stderr(&out).to_lowercase().contains("limited"),
        "no limited-store note when the full store is read: {:?}",
        stderr(&out)
    );
}

#[test]
fn a_rejected_full_store_does_not_fall_back_to_the_limited_store() {
    let s = Scratch::new("fullbad");
    let (_root, canonical) = folder_root(&s);
    write_folder_store(&s, &canonical, StoreScope::Limited);
    let full = store_path_for(&s.home(), &canonical, StoreScope::Full);
    fs::create_dir_all(full.parent().unwrap()).unwrap();
    fs::write(&full, GARBAGE).unwrap();

    let given = canonical.display().to_string();
    let out = s.run_raw(&["-root", given.as_str(), "data"]);
    assert_eq!(code(&out), 7, "stderr {}", stderr(&out));
    assert!(stdout(&out).is_empty());
}

#[test]
fn an_explicit_store_never_falls_back_to_the_limited_store() {
    let s = Scratch::new("explicit");
    let (_root, canonical) = folder_root(&s);
    write_folder_store(&s, &canonical, StoreScope::Limited);
    let missing = s.dir.join("nowhere").join("index.fxstore");
    let out = s.run_raw(&[
        OsArg::from("-store"),
        missing.as_os_str().into(),
        "-root".into(),
        canonical.as_os_str().into(),
        "data".into(),
    ]);
    assert_eq!(code(&out), 8, "stderr {}", stderr(&out));
    assert!(stdout(&out).is_empty());
}

// ------------------------------------------------------------ read-only

fn listing(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn the_store_is_byte_identical_after_runs() {
    let s = Scratch::new("readonly");
    let before = fs::read(s.store()).unwrap();
    let dir_before = listing(&s.dir.join("store"));
    let export = s.dir.join("x.json");
    for args in [
        &["data"][..],
        &["-csv", "-sort", "size", "-n", "1", "data"],
        &["-json", "-name", "-size", "-dm", "-dc", "readme"],
        &["-get-result-count", "data"],
        &["-no-result-error", "zzzz"],
        &["-export-json", export.to_str().unwrap(), "data"],
        &["-frobnicate"],
    ] {
        let _ = s.run(args);
        assert_eq!(
            fs::read(s.store()).unwrap(),
            before,
            "{args:?} kept the store"
        );
        assert_eq!(listing(&s.dir.join("store")), dir_before, "{args:?}");
    }
}

/// Whether the tests run as root (`id -u` prints 0). Root ignores file
/// modes, so a test that relies on them proves nothing then.
#[cfg(unix)]
fn running_as_root() -> bool {
    Command::new("id")
        .arg("-u")
        .output()
        .is_ok_and(|out| out.stdout.trim_ascii() == b"0")
}

#[cfg(unix)]
#[test]
fn a_read_only_store_in_a_read_only_folder_works() {
    use std::os::unix::fs::PermissionsExt;
    if running_as_root() {
        // Root could write the store and its folder despite the modes, so a
        // write by the command would go unnoticed.
        eprintln!("skipped: running as root, which ignores file modes");
        return;
    }
    let s = Scratch::new("ro");
    fs::set_permissions(s.store(), fs::Permissions::from_mode(0o444)).unwrap();
    fs::set_permissions(s.dir.join("store"), fs::Permissions::from_mode(0o555)).unwrap();
    assert_eq!(paths(&s, &["data"]), strs(&[DATA, META, OLD]));
    assert_eq!(ok(&s.run(&["-get-result-count", "data"])).trim(), "3");
}

type OsArg = std::ffi::OsString;

// ------------------------------------------------------------ csv parser

/// Parses RFC 4180 csv (CRLF or LF record ends) into records.
fn parse_csv(text: &str) -> Vec<Vec<String>> {
    let mut records = Vec::new();
    let mut record = Vec::new();
    let mut field = String::new();
    let mut chars = text.chars().peekable();
    let mut quoted = false;
    let mut any = false;
    while let Some(c) = chars.next() {
        any = true;
        if quoted {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    quoted = false;
                }
            } else {
                field.push(c);
            }
            continue;
        }
        match c {
            '"' => quoted = true,
            ',' => record.push(std::mem::take(&mut field)),
            '\r' => {}
            '\n' => {
                record.push(std::mem::take(&mut field));
                records.push(std::mem::take(&mut record));
                any = false;
            }
            _ => field.push(c),
        }
    }
    assert!(!quoted, "csv has an unclosed quote: {text:?}");
    if any {
        record.push(field);
        records.push(record);
    }
    records
}

// ------------------------------------------------------------ json parser

#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)]
enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(BTreeMap<String, Json>),
}

fn objects(json: &Json) -> Vec<BTreeMap<String, Json>> {
    match json {
        Json::Arr(items) => items
            .iter()
            .map(|i| match i {
                Json::Obj(m) => m.clone(),
                other => panic!("array item is not an object: {other:?}"),
            })
            .collect(),
        other => panic!("not an array: {other:?}"),
    }
}

fn parse_json(text: &str) -> Json {
    let c: Vec<char> = text.chars().collect();
    let mut pos = 0;
    let v = value(&c, &mut pos);
    ws(&c, &mut pos);
    assert_eq!(pos, c.len(), "trailing data after json: {text:?}");
    v
}

fn ws(c: &[char], pos: &mut usize) {
    while *pos < c.len() && c[*pos].is_whitespace() {
        *pos += 1;
    }
}

fn eat(c: &[char], pos: &mut usize, want: char) {
    ws(c, pos);
    assert_eq!(c.get(*pos), Some(&want), "expected {want:?} at {pos}");
    *pos += 1;
}

fn value(c: &[char], pos: &mut usize) -> Json {
    ws(c, pos);
    match c.get(*pos).copied() {
        Some('[') => {
            *pos += 1;
            let mut items = Vec::new();
            ws(c, pos);
            if c.get(*pos) == Some(&']') {
                *pos += 1;
                return Json::Arr(items);
            }
            loop {
                items.push(value(c, pos));
                ws(c, pos);
                match c.get(*pos) {
                    Some(',') => *pos += 1,
                    Some(']') => {
                        *pos += 1;
                        return Json::Arr(items);
                    }
                    other => panic!("bad array at {pos}: {other:?}"),
                }
            }
        }
        Some('{') => {
            *pos += 1;
            let mut map = BTreeMap::new();
            ws(c, pos);
            if c.get(*pos) == Some(&'}') {
                *pos += 1;
                return Json::Obj(map);
            }
            loop {
                ws(c, pos);
                let key = match value(c, pos) {
                    Json::Str(k) => k,
                    other => panic!("object key is not a string: {other:?}"),
                };
                eat(c, pos, ':');
                let v = value(c, pos);
                assert!(map.insert(key, v).is_none(), "duplicate key");
                ws(c, pos);
                match c.get(*pos) {
                    Some(',') => *pos += 1,
                    Some('}') => {
                        *pos += 1;
                        return Json::Obj(map);
                    }
                    other => panic!("bad object at {pos}: {other:?}"),
                }
            }
        }
        Some('"') => {
            *pos += 1;
            let mut s = String::new();
            loop {
                let ch = *c.get(*pos).expect("unterminated string");
                *pos += 1;
                match ch {
                    '"' => return Json::Str(s),
                    '\\' => {
                        let e = c[*pos];
                        *pos += 1;
                        match e {
                            '"' => s.push('"'),
                            '\\' => s.push('\\'),
                            '/' => s.push('/'),
                            'b' => s.push('\u{8}'),
                            'f' => s.push('\u{c}'),
                            'n' => s.push('\n'),
                            'r' => s.push('\r'),
                            't' => s.push('\t'),
                            'u' => {
                                let hex: String = c[*pos..*pos + 4].iter().collect();
                                *pos += 4;
                                let mut cp = u32::from_str_radix(&hex, 16).unwrap();
                                if (0xD800..0xDC00).contains(&cp) {
                                    assert_eq!(&c[*pos..*pos + 2], &['\\', 'u']);
                                    let lo: String = c[*pos + 2..*pos + 6].iter().collect();
                                    *pos += 6;
                                    let lo = u32::from_str_radix(&lo, 16).unwrap();
                                    cp = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                                }
                                s.push(char::from_u32(cp).expect("valid escape"));
                            }
                            other => panic!("bad escape \\{other}"),
                        }
                    }
                    ch if (ch as u32) < 0x20 => panic!("raw control char in string"),
                    ch => s.push(ch),
                }
            }
        }
        Some('t') => word(c, pos, "true", Json::Bool(true)),
        Some('f') => word(c, pos, "false", Json::Bool(false)),
        Some('n') => word(c, pos, "null", Json::Null),
        Some(_) => {
            let start = *pos;
            while *pos < c.len() && matches!(c[*pos], '-' | '+' | '.' | 'e' | 'E' | '0'..='9') {
                *pos += 1;
            }
            let text: String = c[start..*pos].iter().collect();
            Json::Num(
                text.parse()
                    .unwrap_or_else(|_| panic!("bad number {text:?}")),
            )
        }
        None => panic!("unexpected end of json"),
    }
}

fn word(c: &[char], pos: &mut usize, w: &str, v: Json) -> Json {
    let got: String = c[*pos..(*pos + w.len()).min(c.len())].iter().collect();
    assert_eq!(got, w);
    *pos += w.len();
    v
}
