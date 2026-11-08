//! Conformance suite, part 1 (operators, quoting, grouping, NOT, leniency,
//! wildcards and search modifiers).
//!
//! Every case restates, in our own words, behaviour described on voidtools'
//! public support pages (cited per group) or docs/PLAN.md. No help text is
//! copied. The shared table machinery lives in `tests/conformance/mod.rs`,
//! shared with `conformance_part2.rs`.
//!
//! Candidate notation (see `conformance/mod.rs`): a trailing `/` marks a
//! folder; a spec without other `/` is a bare name in `/Users/me`.
//!
//! Readings chosen where the public pages leave room:
//!
//! 1. Precedence: NOT binds tightest, then OR, then AND, so `a b|c` is
//!    `a AND (b OR c)` (docs/PLAN.md: "OR binds before AND by default").
//! 2. Spaces around `|` are allowed: `a | b` is the same as `a|b`.
//! 3. Quotes make spaces and operator characters literal but do not switch
//!    wildcards off: `"*.txt"` is still a wildcard.
//! 4. In path mode `*` and `?` do not cross `/`; only `**` does. So
//!    `path:*.txt` cannot match a full path, while `path:**.txt` can.
//! 5. `file:` and `folder:` with nothing after them still filter by kind
//!    (`folder:` alone lists every folder), unlike other bare modifiers,
//!    which match everything. `folder: proj` (with a space) is folders
//!    named like `proj`. The diagnostics of these bare forms are not
//!    checked. This does not touch tests/modifiers.rs, whose bare-modifier
//!    cases use `case:`, `ww:`, `nocase:` and `wfn:` only.
//! 6. `nodiacritics:` (the default) lets `cafe` match `café` and `café`
//!    match `cafe`: both sides have their accents stripped
//!    (`fx_query::fold`). `diacritics:` compares accents exactly. Either way
//!    a decomposed (NFD) name such as APFS may store equals its composed
//!    (NFC) spelling. Case folding is Unicode lowercase per character, so
//!    `ÉCOLE` matches `école`, but `ß` never expands to `ss`. The
//!    apostrophe rule: with diacritics folded,
//!    the apostrophes `'`, `’`, `‘` and `ʼ` count as one character. A term
//!    without an apostrophe ignores the name's, so `plumbs` finds
//!    `Plumb’s.pdf` and `ww:plumb` does not (the word is `plumbs`); a term
//!    with one finds only names that have one in that place, so `plumb's`
//!    and `plumb’s` find `Plumb’s.pdf` but not `plumbs.pdf`, and `'` finds
//!    names containing any apostrophe. Each term follows the rule on its
//!    own, wildcards included. `diacritics:` and `regex:` see the
//!    apostrophes as written. (An earlier rule dropped the apostrophes
//!    from every term, so a term with one also found names without one;
//!    those cases now expect the opposite.)
//! 7. `ww:` treats any character that is not a letter or digit (including
//!    `.`, `-` and space) as a word boundary.
//! 8. A quoted phrase glued to unquoted text (`"my docs"/x`) is one term
//!    whose text joins both parts; only the matching is checked.
//! 9. `regex:` takes the rest of its word raw, so `regex:case:x` is the
//!    pattern `case:x`. How `|` interacts with `regex:` is left open.

mod conformance;

use conformance::{CASE, Case, DEFAULT, DIACRITICS, PATH, REGEX, WW, case, run_cases};

// Search Syntax page: https://www.voidtools.com/support/everything/search_syntax/
// Space between terms means every term must be present; `|` means either.
fn operators() -> Vec<Case> {
    const G: &str = "operators";
    vec![
        case(G, "foo")
            .shape("foo")
            .yes(&["foo.txt", "a foo b", "xfooy"])
            .no(&["fo.txt", "bar"]),
        case(G, "foo bar")
            .shape("and(foo, bar)")
            .yes(&["foo bar.txt", "bar-foo.md", "foobar"])
            .no(&["foo.txt", "bar.txt"]),
        case(G, "foo   bar")
            .shape("and(foo, bar)")
            .yes(&["barfoo"])
            .no(&["foo"]),
        case(G, " foo ").shape("foo").yes(&["foo"]),
        case(G, "a b c")
            .shape("and(a, b, c)")
            .yes(&["cab"])
            .no(&["ab"]),
        case(G, "foo|bar")
            .shape("or(foo, bar)")
            .yes(&["foo.txt", "bar.txt", "foobar"])
            .no(&["baz.txt"]),
        case(G, "foo | bar")
            .shape("or(foo, bar)")
            .yes(&["bar.txt"])
            .no(&["baz.txt"]),
        case(G, "a|b|c").shape("or(a, b, c)").yes(&["c"]).no(&["d"]),
        case(G, "a b|c")
            .shape("and(a, or(b, c))")
            .yes(&["ab", "ac"])
            .no(&["a", "bc"]),
        case(G, "a|b c")
            .shape("and(or(a, b), c)")
            .yes(&["ac", "bc"])
            .no(&["ab", "c"]),
        case(G, "a|b c|d")
            .shape("and(or(a, b), or(c, d))")
            .yes(&["ad", "bc"])
            .no(&["ab", "cd"]),
        case(G, "").shape("ALL").yes(&["anything.txt", "Folder/"]),
        case(G, "   ").shape("ALL").yes(&["x"]),
        case(G, "FOO").yes(&["foo.txt", "Foo.txt"]),
    ]
}

// Search Syntax page: https://www.voidtools.com/support/everything/search_syntax/
// A `!` in front of a term or group excludes it.
fn not_operator() -> Vec<Case> {
    const G: &str = "not";
    vec![
        case(G, "!foo")
            .shape("not(foo)")
            .yes(&["bar.txt"])
            .no(&["foo.txt", "xfoo"]),
        case(G, "!!foo").shape("foo").yes(&["foo"]).no(&["bar"]),
        case(G, "!!!foo")
            .shape("not(foo)")
            .yes(&["bar"])
            .no(&["foo"]),
        case(G, "!a b")
            .shape("and(not(a), b)")
            .yes(&["b"])
            .no(&["ab", "c"]),
        case(G, "a !b")
            .shape("and(a, not(b))")
            .yes(&["a.txt"])
            .no(&["ab.txt", "b"]),
        case(G, "!a|b")
            .shape("or(not(a), b)")
            .yes(&["ab", "c"])
            .no(&["a"]),
        case(G, "a|!b")
            .shape("or(a, not(b))")
            .yes(&["c", "ab"])
            .no(&["b"]),
        case(G, "!a !b")
            .shape("and(not(a), not(b))")
            .yes(&["c"])
            .no(&["a", "b"]),
        case(G, "!<a|b>")
            .shape("not(or(a, b))")
            .yes(&["c"])
            .no(&["a", "b"]),
        case(G, "!<a b>")
            .shape("not(and(a, b))")
            .yes(&["a", "b"])
            .no(&["ab"]),
        case(G, "report !draft")
            .yes(&["report final.pdf"])
            .no(&["report draft.pdf", "notes.pdf"]),
        case(G, "!\"a b\"")
            .shape("not(\"a b\")")
            .yes(&["ab", "a-b"])
            .no(&["a b"]),
        case(G, "x !").shape("x").lenient().yes(&["x"]).no(&["y"]),
    ]
}

// Search Syntax page: https://www.voidtools.com/support/everything/search_syntax/
// `<` and `>` group terms, so operators apply to the whole group.
fn grouping() -> Vec<Case> {
    const G: &str = "grouping";
    vec![
        case(G, "<a b>").shape("and(a, b)").yes(&["ab"]).no(&["a"]),
        case(G, "<a|b> c")
            .shape("and(or(a, b), c)")
            .yes(&["ac", "bc"])
            .no(&["ab", "c"]),
        case(G, "<a b>|c")
            .shape("or(and(a, b), c)")
            .yes(&["ab", "c"])
            .no(&["a", "b"]),
        case(G, "a <b|c>")
            .shape("and(a, or(b, c))")
            .yes(&["ac"])
            .no(&["bc"]),
        case(G, "<a b> c")
            .shape("and(a, b, c)")
            .yes(&["abc"])
            .no(&["ab"]),
        case(G, "<a|b>|c")
            .shape("or(a, b, c)")
            .yes(&["c"])
            .no(&["d"]),
        case(G, "<<a>>").shape("a").yes(&["a"]).no(&["b"]),
        case(G, "<a <b|c>>|d")
            .shape("or(and(a, or(b, c)), d)")
            .yes(&["ab", "d"])
            .no(&["a", "bc"]),
        case(G, "<foo|bar> <.txt|.md>")
            .yes(&["foo.txt", "bar.md"])
            .no(&["foo.rs", "baz.txt"]),
    ]
}

// Search Syntax page: https://www.voidtools.com/support/everything/search_syntax/
// Double quotes keep spaces and operator characters as part of the term.
fn quoting() -> Vec<Case> {
    const G: &str = "quoting";
    vec![
        case(G, "\"foo bar\"")
            .shape("\"foo bar\"")
            .yes(&["my foo bar.txt"])
            .no(&["foobar.txt", "bar foo.txt"]),
        case(G, "\"a|b\"")
            .shape("a|b")
            .yes(&["a|b.txt"])
            .no(&["a.txt", "b.txt"]),
        case(G, "\"!a\"").shape("!a").yes(&["!a"]).no(&["b"]),
        case(G, "\"<a>\"").shape("<a>").yes(&["x<a>y"]).no(&["a"]),
        case(G, "\"case:foo\"")
            .shape("case:foo")
            .yes(&["case:foo"])
            .no(&["foo"]),
        case(G, "\"a b\" c")
            .shape("and(\"a b\", c)")
            .yes(&["a b c"])
            .no(&["a b"]),
        case(G, "\"a b\"|c")
            .shape("or(\"a b\", c)")
            .yes(&["c", "a b"])
            .no(&["ab"]),
        case(G, "\"Foo Bar\"").yes(&["foo bar"]),
        case(G, "\"  \"").yes(&["a  b"]).no(&["a b"]),
        case(G, "\"*.txt\"")
            .shape("*.txt")
            .yes(&["a.txt"])
            .no(&["a.txt.bak"]),
        case(G, "\"my docs\"/x")
            .any_diag()
            .yes(&["/Users/me/my docs/x.txt"])
            .no(&["/Users/me/docs/x.txt"]),
        case(G, "<\"a b\"|c>")
            .shape("or(\"a b\", c)")
            .yes(&["a b"])
            .no(&["b a"]),
    ]
}

// Search Syntax page: https://www.voidtools.com/support/everything/search_syntax/
// and docs/PLAN.md (search-as-you-type): partial input while typing must
// never fail; the parser recovers. The pages do not describe recovery, so
// the recovered shapes follow the parser's own contract.
fn leniency() -> Vec<Case> {
    const G: &str = "leniency";
    vec![
        case(G, "\"foo bar")
            .lenient()
            .shape("\"foo bar\"")
            .yes(&["foo bar"])
            .no(&["foo"]),
        case(G, "x \"foo")
            .lenient()
            .shape("and(x, foo)")
            .yes(&["x foo"]),
        case(G, "<a|b")
            .lenient()
            .shape("or(a, b)")
            .yes(&["a"])
            .no(&["c"]),
        case(G, "c <a b")
            .lenient()
            .shape("and(c, a, b)")
            .yes(&["abc"]),
        case(G, "a > b").lenient().shape("and(a, b)").yes(&["ab"]),
        case(G, "> a").lenient().shape("a").yes(&["a"]),
        case(G, "a|").lenient().shape("a").yes(&["a"]).no(&["b"]),
        case(G, "|a").lenient().shape("a").yes(&["a"]).no(&["b"]),
        case(G, "a||b")
            .lenient()
            .shape("or(a, b)")
            .yes(&["b"])
            .no(&["c"]),
        case(G, "!").lenient().shape("ALL").yes(&["x"]),
        case(G, "<>").lenient().shape("ALL").yes(&["x"]),
        case(G, "a <> b")
            .lenient()
            .shape("and(a, b)")
            .yes(&["ab"])
            .no(&["a"]),
        case(G, "\"").lenient().yes(&["anything"]),
        case(G, "<").lenient().shape("ALL").yes(&["x"]),
        case(G, "<<<a").lenient().shape("a").yes(&["a"]).no(&["b"]),
        case(G, "case:").lenient().shape("ALL").yes(&["x"]),
        case(G, "foo ww:")
            .lenient()
            .shape("foo")
            .yes(&["foo"])
            .no(&["bar"]),
        case(G, "|").lenient().shape("ALL").yes(&["x"]),
    ]
}

// Search Syntax page (wildcards): https://www.voidtools.com/support/everything/search_syntax/
// and Searching page: https://www.voidtools.com/support/everything/searching/
// `*` is any run of characters, `?` exactly one; a wildcard term must
// match the whole name, not a part of it.
fn wildcards() -> Vec<Case> {
    const G: &str = "wildcards";
    vec![
        case(G, "*.txt")
            .shape("*.txt")
            .yes(&["notes.txt", ".txt", "A.TXT"])
            .no(&["notes.txt.bak", "notes.md", "txt"]),
        case(G, "foo*")
            .yes(&["foo", "food.txt"])
            .no(&["xfoo", "fo"]),
        case(G, "*foo").yes(&["foo", "xfoo"]).no(&["foox"]),
        case(G, "*foo*").yes(&["foo", "a foo b"]).no(&["fo o"]),
        case(G, "f?o")
            .yes(&["foo", "f.o", "FOO"])
            .no(&["fo", "fooo", "xfoo"]),
        case(G, "???").yes(&["abc", "a.b"]).no(&["ab", "abcd"]),
        case(G, "?").yes(&["é", "x"]).no(&["xy"]),
        case(G, "caf?").yes(&["café", "cafe"]).no(&["caf"]),
        case(G, "*").yes(&["a", "b.txt", "Folder/"]),
        case(G, "a*b*c")
            .yes(&["abc", "a-b-c", "aXXbYYc"])
            .no(&["acb", "abcd"]),
        case(G, "*.tar.gz")
            .yes(&["x.tar.gz"])
            .no(&["x.tar.gz.part", "x.gz"]),
        case(G, "**.txt").yes(&["a.txt"]).no(&["a.md"]),
        case(G, "*.txt|*.md")
            .shape("or(*.txt, *.md)")
            .yes(&["a.md", "b.txt"])
            .no(&["c.rs"]),
        case(G, "*.txt !draft*")
            .yes(&["final.txt"])
            .no(&["draft1.txt", "final.md"]),
        case(G, "report *.pdf")
            .yes(&["report.pdf"])
            .no(&["report.doc", "summary.pdf"]),
        case(G, "<*.jpg|*.png> holiday")
            .yes(&["holiday.png"])
            .no(&["holiday.gif"]),
        case(G, "*.TXT").yes(&["a.txt"]),
        case(G, "*.txt").opts(CASE).yes(&["a.txt"]).no(&["a.TXT"]),
        // A pathological pattern must finish quickly (no exponential
        // backtracking); the whole table is one test, so a blowup hangs it.
        case(G, "*a*a*a*a*a*a*a*a*a*a*a*a*a*a*a*a*b")
            .yes(&["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaab"])
            .no(&["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"]),
        case(G, "?*?*?*?*?*?*?*?*?*?*?*?*?*?*?*?*?*?*?*?*x")
            .no(&["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaay"]),
    ]
}

// Search Syntax page (wildcards): https://www.voidtools.com/support/everything/search_syntax/
// and Searching page (Match Path): https://www.voidtools.com/support/everything/searching/
// A term with a path separator, or under path matching, is tested against
// the full path; `**` also crosses folder separators. Reading 4 applies.
fn wildcard_paths() -> Vec<Case> {
    const G: &str = "wildcard paths";
    vec![
        case(G, "/Users/me/*.txt")
            .yes(&["/Users/me/a.txt"])
            .no(&["/Users/me/sub/a.txt", "/Users/me/a.md"]),
        case(G, "/Users/**.txt")
            .yes(&["/Users/me/a.txt", "/Users/me/sub/deep/b.txt"])
            .no(&["/Users/me/a.md", "/tmp/a.txt"]),
        case(G, "**/src/*.rs")
            .yes(&["/w/proj/src/main.rs"])
            .no(&["/w/proj/src/bin/main.rs", "/w/proj/lib/main.rs"]),
        case(G, "**/src/**.rs")
            .yes(&["/w/proj/src/bin/main.rs"])
            .no(&["/w/src.rs"]),
        case(G, "path:*.txt").no(&["/Users/me/a.txt"]),
        case(G, "path:**.txt")
            .yes(&["/Users/me/a.txt"])
            .no(&["/Users/me/a.txt.bak"]),
        case(G, "path:/Users/?e/*")
            .yes(&["/Users/me/a.txt", "/Users/me/Docs/"])
            .no(&["/Users/me/x/a.txt"]),
        case(G, "**.txt")
            .opts(PATH)
            .yes(&["/Users/me/a.txt"])
            .no(&["/a.md"]),
        case(G, "*.txt").opts(PATH).no(&["/Users/me/a.txt"]),
        case(G, "/Users/*/a.txt")
            .yes(&["/Users/me/a.txt", "/Users/you/a.txt"])
            .no(&["/Users/me/x/a.txt"]),
        case(G, "nopath:*.txt")
            .opts(PATH)
            .yes(&["/Users/me/a.txt"])
            .no(&["/a.md"]),
    ]
}

// Searching page (Match Path): https://www.voidtools.com/support/everything/searching/
// and Search Syntax page: https://www.voidtools.com/support/everything/search_syntax/
// A term containing a path separator matches anywhere in the full path
// (`/` is the macOS separator standing in for `\`).
fn literal_paths() -> Vec<Case> {
    const G: &str = "literal paths";
    vec![
        case(G, "me/docs")
            .yes(&["/Users/me/docs/a.txt", "/Users/me/docs/"])
            .no(&["/Users/me/src/a.txt"]),
        case(G, "docs/ report")
            .yes(&["/Users/me/docs/report.pdf"])
            .no(&["/Users/me/report.pdf", "/Users/me/docs/notes.pdf"]),
        case(G, "/tmp/").yes(&["/tmp/x"]).no(&["/var/tmp"]),
        case(G, "src/MAIN.rs").yes(&["/p/src/main.rs"]),
        case(G, "src/MAIN.rs").opts(CASE).no(&["/p/src/main.rs"]),
    ]
}

// Search Modifiers page: https://www.voidtools.com/support/everything/search_modifiers/
// and Searching page (toggles): https://www.voidtools.com/support/everything/searching/
// `case:` / `nocase:` and the global Match Case toggle.
fn modifier_case() -> Vec<Case> {
    const G: &str = "case";
    vec![
        case(G, "case:Foo")
            .yes(&["Foo.txt", "aFoo"])
            .no(&["foo.txt", "FOO.txt"]),
        case(G, "Foo").opts(CASE).yes(&["Foo"]).no(&["foo"]),
        case(G, "nocase:Foo").opts(CASE).yes(&["foo", "FOO"]),
        case(G, "nocase:Foo").yes(&["fOO"]),
        case(G, "case:Foo bar").yes(&["Foo BAR"]).no(&["foo bar"]),
        case(G, "case:Foo nocase:BAR")
            .opts(CASE)
            .yes(&["Foo bar"])
            .no(&["foo bar"]),
        case(G, "CASE:Foo").yes(&["Foo"]).no(&["foo"]),
        case(G, "case:\"Foo Bar\"")
            .yes(&["Foo Bar.txt"])
            .no(&["foo bar.txt"]),
        case(G, "case:<Foo|Bar>")
            .yes(&["Foo", "Bar"])
            .no(&["foo", "bar"]),
        case(G, "case:<Foo nocase:bar>")
            .yes(&["Foo BAR"])
            .no(&["foo bar"]),
        case(G, "nocase:<Foo case:Bar>")
            .opts(CASE)
            .yes(&["foo Bar"])
            .no(&["foo bar"]),
        case(G, "case:nocase:Foo").yes(&["foo"]),
        case(G, "nocase:case:Foo").no(&["foo"]).yes(&["Foo"]),
        case(G, "!case:Foo").yes(&["foo", "bar"]).no(&["Foo"]),
        case(G, "case:F*.txt").yes(&["Foo.txt"]).no(&["foo.txt"]),
        case(G, "case:Foo|bar").yes(&["Foo", "BAR"]).no(&["foo"]),
        case(G, "case:É").yes(&["É.txt"]).no(&["é.txt"]),
        case(G, "é").yes(&["É.txt"]),
    ]
}

// Search Modifiers page: https://www.voidtools.com/support/everything/search_modifiers/
// `path:` / `nopath:` and the global Match Path toggle.
fn modifier_path() -> Vec<Case> {
    const G: &str = "path";
    vec![
        case(G, "path:docs")
            .yes(&["/Users/me/docs/a.txt", "/Users/me/docs/"])
            .no(&["/Users/me/src/a.txt"]),
        case(G, "docs")
            .no(&["/Users/me/docs/a.txt"])
            .yes(&["/x/docs/"]),
        case(G, "docs").opts(PATH).yes(&["/Users/me/docs/a.txt"]),
        case(G, "nopath:docs")
            .opts(PATH)
            .no(&["/Users/me/docs/a.txt"])
            .yes(&["/x/docs.md"]),
        case(G, "path:docs a.txt")
            .yes(&["/Users/me/docs/a.txt"])
            .no(&["/Users/me/src/a.txt", "/Users/me/docs/b.txt"]),
        case(G, "PATH:Docs").yes(&["/Users/me/docs/a.txt"]),
        case(G, "path:case:Docs")
            .yes(&["/x/Docs/a"])
            .no(&["/x/docs/a"]),
        case(G, "!path:docs")
            .yes(&["/Users/me/src/a.txt"])
            .no(&["/Users/me/docs/a.txt"]),
        case(G, "path:<docs|src> main")
            .yes(&["/p/src/main.rs", "/p/docs/main.md"])
            .no(&["/p/lib/main.rs"]),
        case(G, "path:\"my docs\"")
            .yes(&["/Users/me/my docs/x"])
            .no(&["/Users/me/docs/x"]),
    ]
}

// Search Modifiers page: https://www.voidtools.com/support/everything/search_modifiers/
// `file:` and `folder:` restrict a term to files or to folders. Reading 5
// applies to the bare forms.
fn modifier_kind() -> Vec<Case> {
    const G: &str = "file/folder";
    vec![
        case(G, "file:foo")
            .yes(&["foo.txt"])
            .no(&["foo/", "bar.txt"]),
        case(G, "folder:foo")
            .yes(&["foo/", "foobar/"])
            .no(&["foo.txt", "bar/"]),
        case(G, "files:foo").yes(&["foo.txt"]).no(&["foo/"]),
        case(G, "folders:foo").yes(&["foo/"]).no(&["foo.txt"]),
        case(G, "Folder:foo").yes(&["foo/"]).no(&["foo.txt"]),
        case(G, "!folder:foo")
            .yes(&["foo.txt", "bar/"])
            .no(&["foo/"]),
        case(G, "folder:foo|file:bar")
            .yes(&["foo/", "bar.txt"])
            .no(&["foo.txt", "bar/"]),
        case(G, "folder:<src|lib>")
            .yes(&["src/", "lib/"])
            .no(&["src.rs", "lib.rs"]),
        case(G, "folder:*").yes(&["a/"]).no(&["a"]),
        case(G, "file:*.txt").yes(&["a.txt"]).no(&["a.txt/"]),
        case(G, "folder:\"my docs\"")
            .yes(&["my docs/"])
            .no(&["my docs"]),
        case(G, "folder:")
            .any_diag()
            .yes(&["Projects/", "/Volumes/"])
            .no(&["notes.txt"]),
        case(G, "file:")
            .any_diag()
            .yes(&["notes.txt"])
            .no(&["Projects/"]),
        case(G, "folder: proj")
            .any_diag()
            .yes(&["proj/"])
            .no(&["proj.txt"]),
        case(G, "case:folder:Foo")
            .yes(&["Foo/"])
            .no(&["foo/", "Foo"]),
    ]
}

// Search Modifiers page: https://www.voidtools.com/support/everything/search_modifiers/
// `ww:` / `noww:` and the global Match Whole Word toggle. Reading 7.
fn modifier_whole_word() -> Vec<Case> {
    const G: &str = "whole word";
    vec![
        case(G, "ww:foo")
            .yes(&["foo", "my foo.txt", "foo-bar", "foo.txt"])
            .no(&["food.txt", "afoo", "foo2"]),
        case(G, "wholeword:foo").yes(&["a foo"]).no(&["afoo"]),
        case(G, "foo").opts(WW).yes(&["a foo"]).no(&["food"]),
        case(G, "noww:foo").opts(WW).yes(&["food"]),
        case(G, "ww:foo bar").yes(&["foo xbar"]).no(&["food bar"]),
        case(G, "ww:\"foo bar\"")
            .yes(&["a foo bar b"])
            .no(&["a foo barb"]),
        case(G, "ww:FOO").yes(&["a foo"]),
        case(G, "case:ww:Foo")
            .yes(&["a Foo b"])
            .no(&["a foo b", "aFoo"]),
        case(G, "ww:case:Foo").yes(&["a Foo b"]).no(&["a foo b"]),
        case(G, "ww:<foo|bar>")
            .yes(&["x bar"])
            .no(&["xbar", "foox"]),
        case(G, "WW:foo").yes(&["foo"]).no(&["foox"]),
        case(G, "!ww:foo").yes(&["food"]).no(&["foo bar"]),
        case(G, "ww:<foo noww:bar>")
            .yes(&["foo xbarx"])
            .no(&["food bar"]),
    ]
}

// Search Modifiers page: https://www.voidtools.com/support/everything/search_modifiers/
// `wfn:` / `nowfn:`: the term must be the whole file name.
fn modifier_whole_filename() -> Vec<Case> {
    const G: &str = "whole filename";
    vec![
        case(G, "wfn:foo.txt")
            .yes(&["foo.txt", "FOO.TXT"])
            .no(&["foo.txt.bak", "afoo.txt", "foo"]),
        case(G, "wholefilename:foo.txt")
            .yes(&["foo.txt"])
            .no(&["xfoo.txt"]),
        case(G, "nowfn:foo").yes(&["xfooy"]),
        case(G, "wfn:nowfn:foo").yes(&["food"]),
        case(G, "nowfn:wfn:foo").yes(&["foo"]).no(&["food"]),
        case(G, "case:wfn:Foo").yes(&["Foo"]).no(&["foo", "Foo2"]),
        case(G, "wfn:\"my file.txt\"")
            .yes(&["my file.txt"])
            .no(&["my file.txt.bak"]),
        case(G, "folder:wfn:src")
            .yes(&["src/"])
            .no(&["src", "srcs/"]),
        case(G, "wfn:<a|b>").yes(&["a", "b"]).no(&["ab"]),
        case(G, "!wfn:foo").yes(&["food"]).no(&["foo"]),
        case(G, "wfn:/Users/me/a.txt")
            .yes(&["/Users/me/a.txt"])
            .no(&["/Users/me/a.txt.bak", "/x/Users/me/a.txt"]),
    ]
}

// Search Modifiers page: https://www.voidtools.com/support/everything/search_modifiers/
// `start-with:` and `end-with:` anchor the term to the start or end.
fn modifier_start_end() -> Vec<Case> {
    const G: &str = "start/end";
    vec![
        case(G, "start-with:foo")
            .yes(&["foo", "food.txt", "FOO.md"])
            .no(&["afoo", "a foo"]),
        case(G, "end-with:.txt")
            .yes(&["a.txt", "A.TXT"])
            .no(&["a.txt.bak", "a.txtx"]),
        case(G, "start-with:foo end-with:.md")
            .yes(&["foo notes.md"])
            .no(&["foo.txt", "my foo.md"]),
        case(G, "case:start-with:Foo")
            .yes(&["Foobar"])
            .no(&["foobar"]),
        case(G, "!start-with:foo").yes(&["afoo"]).no(&["foo"]),
        case(G, "start-with:<a|b>").yes(&["ax", "bx"]).no(&["xa"]),
        case(G, "start-with:\"my f\"")
            .yes(&["my file"])
            .no(&["a my file"]),
        case(G, "end-with:foo|start-with:bar")
            .yes(&["xfoo", "barx"])
            .no(&["foox", "xbar"]),
        case(G, "Start-With:foo").yes(&["foo1"]).no(&["1foo"]),
        case(G, "folder:end-with:.app")
            .yes(&["Safari.app/"])
            .no(&["Safari.app", "x.apps/"]),
    ]
}

// Search Modifiers page: https://www.voidtools.com/support/everything/search_modifiers/
// `diacritics:` / `nodiacritics:` and the global Match Diacritics toggle.
// Reading 6: accents fold by default, NFD equals NFC.
fn modifier_diacritics() -> Vec<Case> {
    const G: &str = "diacritics";
    vec![
        case(G, "cafe")
            .yes(&["café.txt", "CAFÉ", "Cafe\u{301} au lait"])
            .no(&["caff"]),
        case(G, "café").yes(&["cafe.txt", "CAFE", "café"]),
        case(G, "nodiacritics:cafe")
            .opts(DIACRITICS)
            .yes(&["café", "cafe"]),
        case(G, "cafe")
            .opts(DIACRITICS)
            .yes(&["cafe"])
            .no(&["café"]),
        case(G, "diacritics:café")
            .yes(&["cafe\u{301}.txt", "CAFÉ"])
            .no(&["cafe"]),
        case(G, "ecole").yes(&["École primaire", "ÉCOLE"]),
        case(G, "straße").yes(&["STRAẞE.txt"]).no(&["strasse"]),
        case(G, "привет").yes(&["ПРИВЕТ.doc"]),
        case(G, "*.cafe").yes(&["menu.café"]).no(&["menu.café.txt"]),
        case(G, "ww:cafe").yes(&["Café au lait"]).no(&["cafés"]),
        case(G, "diacritics:café")
            .yes(&["café.txt"])
            .no(&["cafe.txt"]),
        case(G, "café")
            .opts(DIACRITICS)
            .yes(&["café"])
            .no(&["cafe"]),
        case(G, "nodiacritics:café")
            .opts(DIACRITICS)
            .yes(&["café", "CAFÉ"]),
        case(G, "nodiacritics:café").yes(&["Café au lait"]),
        case(G, "diacritics:cafe").yes(&["cafe"]).no(&["café"]),
        case(G, "Diacritics:Résumé")
            .yes(&["résumé.pdf"])
            .no(&["resume.pdf"]),
        case(G, "diacritics:<café|naïve>")
            .yes(&["naïve"])
            .no(&["naive", "cafe"]),
    ]
}

// The apostrophe rule (reading 6). A term without
// an apostrophe ignores the name's; a term with one needs one in that
// place. The four kinds ', ’, ‘ and ʼ count as one character.
fn apostrophes() -> Vec<Case> {
    const G: &str = "apostrophes";
    const PLUMB: &str = "Plumb’s Veterinary Drug Handbook 10th Edition.pdf";
    vec![
        // The plan's table: plumbs finds all three; plumb's, plumb’s and
        // PLUMB'S find the two names with an apostrophe.
        case(G, "plumbs")
            .yes(&["Plumb’s.pdf", "Plumb's.pdf", "plumbs.pdf"])
            .no(&["plumb.pdf", "plumb s.pdf"]),
        case(G, "plumb's")
            .yes(&["Plumb’s.pdf", "Plumb's.pdf"])
            .no(&["plumbs.pdf"]),
        case(G, "plumb’s")
            .yes(&["Plumb’s.pdf", "Plumb's.pdf"])
            .no(&["plumbs.pdf"]),
        case(G, "PLUMB'S")
            .yes(&["Plumb’s.pdf", "Plumb's.pdf"])
            .no(&["plumbs.pdf"]),
        case(G, "plumbs")
            .yes(&[PLUMB, "plumb's.pdf", "PLUMB‘S", "plumbʼs", "plumbs"])
            .no(&["plumb.pdf", "plumb s.pdf"]),
        case(G, "plumbʼs")
            .yes(&[PLUMB, "plumb‘s.pdf"])
            .no(&["plumbs.pdf"]),
        case(G, "plumb‘s").yes(&[PLUMB]).no(&["PLUMBS"]),
        case(G, "cafes").yes(&["Cafe\u{301}’s menu", "Café's", "cafes"]),
        case(G, "café's")
            .yes(&["Cafe\u{301}’s menu", "CAFE'S"])
            .no(&["Cafés"]),
        case(G, "obrien")
            .yes(&["OBrien.txt", "O’Brien.txt"])
            .no(&["O Brien.txt"]),
        case(G, "o'brien")
            .yes(&["O’Brien.txt", "o'brien"])
            .no(&["OBrien.txt", "O Brien.txt"]),
        // Each space-separated term on its own: the apostrophe is needed
        // in the first term only.
        case(G, "plumb's handbook")
            .yes(&[PLUMB, "plumb's handbook.pdf"])
            .no(&["plumbs handbook.pdf", "plumb's.pdf"]),
        case(G, "plumbs handbook").yes(&[PLUMB, "plumbs handbook.pdf"]),
        case(G, "plumbs plumb's").yes(&[PLUMB]).no(&["plumbs.pdf"]),
        case(G, "plumbs|plumb's").yes(&[PLUMB, "plumbs.pdf"]),
        case(G, "!plumb's").yes(&["plumbs.pdf"]).no(&[PLUMB]),
        // `diacritics:` (CLI `-a`) compares the exact text.
        case(G, "diacritics:plumbs")
            .yes(&["plumbs.pdf"])
            .no(&[PLUMB, "plumb's.pdf"]),
        case(G, "diacritics:plumb’s")
            .yes(&[PLUMB])
            .no(&["plumbs.pdf", "plumb's.pdf"]),
        case(G, "plumb's")
            .opts(DIACRITICS)
            .yes(&["plumb's.pdf"])
            .no(&[PLUMB, "plumbs.pdf"]),
        case(G, "nodiacritics:plumbs")
            .opts(DIACRITICS)
            .yes(&[PLUMB]),
        case(G, "nodiacritics:plumb's")
            .opts(DIACRITICS)
            .yes(&[PLUMB])
            .no(&["plumbs.pdf"]),
        // `regex:` matches the name as written.
        case(G, "regex:plumb.?s")
            .yes(&[PLUMB, "plumbs.pdf", "plumb's.pdf"])
            .no(&["plumb.pdf"]),
        case(G, "regex:plumbs").yes(&["plumbs.pdf"]).no(&[PLUMB]),
        case(G, "regex:plumb's").yes(&["plumb's.pdf"]).no(&[PLUMB]),
        // Whole words: `ww:plumb's` and `ww:plumbs` find "Plumb’s";
        // `ww:plumb` doesn't (without the apostrophe the word is plumbs).
        case(G, "ww:plumb").yes(&["plumb.pdf"]).no(&[PLUMB]),
        case(G, "ww:plumbs").yes(&[PLUMB, "my plumbs.pdf"]),
        case(G, "ww:plumb's")
            .yes(&[PLUMB])
            .no(&["plumbsx.pdf", "plumbs.pdf", "plumb'sx.pdf"]),
        case(G, "diacritics:ww:plumb").yes(&[PLUMB]),
        case(G, "wfn:plumbs.pdf").yes(&["Plumb’s.pdf", "plumbs.pdf"]),
        case(G, "wfn:plumb's.pdf")
            .yes(&["Plumb’s.pdf"])
            .no(&["plumbs.pdf"]),
        case(G, "start-with:its").yes(&["It’s here.txt"]),
        case(G, "start-with:it's")
            .yes(&["It’s here.txt"])
            .no(&["Its here.txt"]),
        case(G, "end-with:'s.pdf")
            .yes(&["Plumb’s.pdf"])
            .no(&["plumbs.pdf"]),
        // Wildcards follow the same rule.
        case(G, "plumbs*.pdf")
            .yes(&[PLUMB, "plumbs.pdf"])
            .no(&["plumb.pdf"]),
        case(G, "plumb's*.pdf").yes(&[PLUMB]).no(&["plumbs.pdf"]),
        case(G, "*'s.pdf")
            .yes(&["Plumb’s.pdf", "it's.pdf"])
            .no(&["Plumbs.pdf"]),
        // A term without an apostrophe sees the name without its
        // apostrophes (as under the earlier rule), so `?` never stands for
        // one.
        case(G, "plumb?s.pdf")
            .yes(&["plumbXs.pdf"])
            .no(&["Plumb’s.pdf"]),
        // Paths too.
        case(G, "path:joes").yes(&["/Users/Joe’s/notes.txt", "/Users/Joes/notes.txt"]),
        case(G, "path:joe's")
            .yes(&["/Users/Joe’s/notes.txt"])
            .no(&["/Users/Joes/notes.txt"]),
        case(G, "joe's/").yes(&["/Users/Joe’s/notes.txt"]),
        case(G, "joes/").yes(&["/Users/Joe’s/notes.txt"]),
        // A term made only of apostrophes finds names that contain one,
        // whichever apostrophe either side uses.
        case(G, "'")
            .yes(&[PLUMB, "it's.txt", "‘quoted’", "rock ʼn roll"])
            .no(&["plumbs.pdf", "Hawaiʻi.jpg"]),
        case(G, "’").yes(&["it's.txt"]).no(&["its.txt"]),
        case(G, "\"'\"").yes(&[PLUMB]).no(&["plumbs.pdf"]),
        case(G, "''").yes(&["say ‘’.txt"]).no(&["it's.txt"]),
        case(G, "*'*").yes(&[PLUMB]).no(&["plumbs.pdf"]),
        case(G, "'*").yes(&["’tis.txt"]).no(&[PLUMB]),
        case(G, "file:'").yes(&["it's.txt"]).no(&["it's/"]),
        case(G, "diacritics:'").yes(&["it's.txt"]).no(&[PLUMB]),
        case(G, "' plumbs").yes(&[PLUMB]).no(&["plumbs.pdf"]),
        case(G, "'s").yes(&[PLUMB]).no(&["Plumbs.pdf"]),
    ]
}

// Search Modifiers page: https://www.voidtools.com/support/everything/search_modifiers/
// `regex:` / `noregex:` and the global regex toggle. Regex matching is run
// 1.4's job, so these cases check the tree only (`re(...)` terms), except
// where `noregex:` turns matching back to text.
fn modifier_regex() -> Vec<Case> {
    const G: &str = "regex";
    vec![
        case(G, "regex:^foo").shape("re(^foo)"),
        case(G, "regex:a.*b$").shape("re(a.*b$)"),
        case(G, "regex:*.txt").shape("re(*.txt)"),
        case(G, "regex:case:x").shape("re(case:x)"),
        case(G, "abc").opts(REGEX).shape("re(abc)"),
        case(G, "a b").opts(REGEX).shape("and(re(a), re(b))"),
        case(G, "noregex:*.txt")
            .opts(REGEX)
            .shape("*.txt")
            .yes(&["a.txt"])
            .no(&["a.md"]),
        case(G, "noregex:foo")
            .opts(REGEX)
            .shape("foo")
            .yes(&["xfoox"])
            .no(&["bar"]),
        case(G, "noregex:foo").shape("foo").yes(&["foo"]),
        case(G, "regex:^a foo").shape("and(re(^a), foo)"),
        case(G, "!regex:x").shape("not(re(x))"),
        case(G, "REGEX:x").shape("re(x)"),
    ]
}

// Search Modifiers page: https://www.voidtools.com/support/everything/search_modifiers/
// Modifiers combine with each other, with operators and with toggles.
fn modifier_combinations() -> Vec<Case> {
    const G: &str = "combinations";
    vec![
        case(G, "case:ww:folder:Src")
            .yes(&["my Src/"])
            .no(&["my src/", "my Src", "mySrc/"]),
        case(G, "path:ww:docs")
            .yes(&["/a/docs/x"])
            .no(&["/a/mydocs/x"]),
        case(G, "file:<*.jpg|*.png> !thumb").yes(&["cat.png"]).no(&[
            "cat.png/",
            "thumb cat.png",
            "cat.gif",
        ]),
        case(G, "Foo").opts(CASE).yes(&["Foo"]).no(&["foo"]),
        case(G, "nocase:ww:foo")
            .opts(CASE_WW)
            .yes(&["a FOO"])
            .no(&["aFOO"]),
        case(G, "noww:nocase:foo").opts(CASE_WW).yes(&["aFOO"]),
        case(G, "foo !case:FOO").yes(&["foo"]).no(&["FOO"]),
        case(G, "<case:A|b> c").yes(&["A c", "b c"]).no(&["a c"]),
        case(G, "wfn:foo|*.md").yes(&["foo", "x.md"]).no(&["foox"]),
        case(G, "nopath:x").opts(DEFAULT).yes(&["x"]),
    ]
}

/// Global Match Case and Match Whole Word together.
const CASE_WW: fx_query::SearchOptions = fx_query::SearchOptions {
    match_case: true,
    whole_word: true,
    ..DEFAULT
};

fn all_cases() -> Vec<Case> {
    [
        operators(),
        not_operator(),
        grouping(),
        quoting(),
        leniency(),
        wildcards(),
        wildcard_paths(),
        literal_paths(),
        modifier_case(),
        modifier_path(),
        modifier_kind(),
        modifier_whole_word(),
        modifier_whole_filename(),
        modifier_start_end(),
        modifier_diacritics(),
        apostrophes(),
        modifier_regex(),
        modifier_combinations(),
    ]
    .into_iter()
    .flatten()
    .collect()
}

#[test]
fn conformance_part1_has_at_least_150_cases() {
    let n = all_cases().len();
    println!("conformance part 1: {n} cases");
    assert!(n >= 150, "need at least 150 conformance cases, have {n}");
}

#[test]
fn conformance_part1_all_cases_pass() {
    let n = run_cases("conformance part 1", &all_cases());
    println!("conformance part 1: {n} cases passed");
}
