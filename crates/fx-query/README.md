# fx-query

Search queries for Not-Everything. This crate holds the search-syntax
parser and its reference evaluator, and the fast matchers, the refinement
cache and query cancellation. It also holds the substring scan: a
parallel scan over one contiguous name buffer, in `src/substring.rs`,
with the cancellation tokens the other searches use.

- `src/syntax/`: the search syntax. `ast.rs` (query tree, options,
  diagnostics), `lexer.rs` (tokens) and `parser.rs` (`parse`).
  `functions.rs` holds the parsed form of search functions (`Filter`)
  and their argument parsing; `size.rs` reads size values and constants,
  `date.rs` date values and keywords, and `macros.rs` holds the type
  macros (`pic:`, `doc:`, ...).
- `src/eval.rs`: the reference evaluator, `eval::matches` and
  `eval::matches_in`.
- `src/fold.rs`: Unicode case and diacritic folding (`fold_str`) and
  the folded search copy of a name column (`FoldedColumn`), tested in
  `tests/fold.rs`.
- `src/matcher.rs`: the compiled parallel matcher,
  `CompiledQuery` over a `SearchView`, plus the owned `VecView`; tested
  in `tests/matcher.rs` against the eval oracle. `src/paths.rs`
  (private): how path terms are decided per folder.
- `src/options.rs`: `QueryOptions`, the search field's chips;
  tested in `tests/query_options.rs`.
- `src/highlight.rs`: `Highlighter`, the ranges to highlight in
  a result's name and folder; tested in `tests/highlight.rs`, measured by
  `examples/highlight_bench.rs`.
- `src/refine.rs`: the refinement cache for search-as-you-type,
  `Searcher`; tested in `tests/refine.rs`.
- `src/substring.rs`: `NameColumn`, `SubstringQuery`, `search`, `count`,
  `Generation` / `CancelToken` and `fold_ascii_lowercase`.
- `tests/parse_operators.rs`: operator parsing, precedence, quotes,
  recovery diagnostics and the evaluator on literal terms.
- `tests/conformance_part1.rs` (operators, quoting, wildcards and
  modifiers, accent folding included; 218 cases) and
  `tests/conformance_part2.rs` (search functions and macros; 152 cases,
  with a test that keeps it at ≥ 92 so the total stays ≥ 300):
  table-driven conformance suites
  sharing `tests/conformance/mod.rs`. Each suite header lists the
  numbered readings chosen where the public pages leave room (1–9 in
  part 1, 10–20 in part 2).
- `tests/modifiers.rs`, `tests/wildcards.rs`, `tests/size_functions.rs`,
  `tests/date_functions.rs`, `tests/functions_api.rs`, `tests/macros.rs`:
  focused suites per feature.
- `tests/substring.rs`: checks `search` and `count` against a naive
  per-name `contains` on generated and hand-built names, across thread
  counts and chunk sizes, plus case folding, cancellation and column
  validation.
- `examples/search_bench.rs`: the substring scan's latency benchmark and
  its Phase 0 gate, described below.
- `examples/matcher_bench.rs`: the benchmark of `CompiledQuery`
  and `Searcher` with the Phase 1 search-budget `GATE` lines, described
  below.

## Search syntax

The syntax module uses `std` only. Its behaviour is written from
`docs/PLAN.md` and voidtools' public Search Syntax, Searching and Search
Modifiers pages, restated in our own words.

`parse(input, &SearchOptions) -> Parsed { query, diagnostics }` turns a
query string into a `Node` tree:

| Input       | Meaning                                    | Tree                 |
|-------------|--------------------------------------------|----------------------|
| `a b`       | whitespace is AND                          | `And([a, b])`        |
| `a\|b`      | `\|` is OR                                 | `Or([a, b])`         |
| `!a`        | a leading `!` is NOT                       | `Not(a)`             |
| `<a b>\|c`  | `<` `>` group                              | `Or([And([a, b]), c])` |
| `"a b"`     | quotes make spaces and operators literal   | `Term("a b")`        |
| `foo" bar"` | a quoted part joins unquoted text          | `Term("foo bar")`    |
| (empty)     | an empty or whitespace-only query          | `MatchAll`           |

NOT binds tightest, then OR, then AND, so `a b|c` is `a AND (b OR c)`.
A `!` inside a word (`a!b`) is literal, and a run of `!` keeps only its
parity (`!!a` is `a`). Nested ANDs and ORs of the same kind are flattened
and a one-child group collapses to its child.

Each `Term` carries its unquoted `text`, a `Pattern` (`Literal`, `Wildcard` or `Regex`),
its byte `span` in the input and `TermOptions`. `TermOptions` holds the
five global toggles of `SearchOptions` (`match_case`, `whole_word`,
`match_path`, `match_diacritics`, `regex`; all off by default) plus
`kind` (`TermKind::Any`, `FilesOnly`, `FoldersOnly`), `whole_filename`,
`start_with` and `end_with`, which search modifiers set per term.

Parsing never fails, because search-as-you-type sees partial input. Each
recovered problem gives a `Diagnostic { kind, span }` with a byte span on
char boundaries:

| Problem              | Recovery                                     |
|----------------------|----------------------------------------------|
| unclosed `"`         | the quote runs to the end of input           |
| unclosed `<`         | the group closes at the end of input         |
| stray `>`            | ignored                                      |
| empty `\|` operand   | dropped (`a\|` is `a`, `a\|\|b` is `a\|b`)   |
| dangling `!`         | dropped                                      |
| empty `<>`           | dropped                                      |
| nesting over 128     | the extra `<` is ignored (`MAX_GROUP_DEPTH`) |

### Reference evaluator

`eval::matches(&node, &Candidate { name, parent_path, is_dir, .. })` returns
`Result<bool, Unsupported>`. It is a naive oracle for tests, not the fast
path. A literal is a substring test on the name, or on the full path
(`parent_path + '/' + name`) when `match_path` is set or the literal
contains `/` (the macOS stand-in for Everything's backslash rule).
Both sides are folded with `fold::fold_str` first (see below):
normalized so NFD and NFC spellings compare equal, lowercased per `char`
without `match_case`, and accent-stripped without `match_diacritics` (the
default), so `resume` matches `Résumé.pdf` but `diacritics:resume` does
not. Wildcard terms fold their literal pieces the same way. `whole_word`,
`start_with`, `end_with`, `whole_filename` and `kind` narrow the match.
Regex terms are compiled with `regex_term::RegexTerm` and matched (see
below); an invalid regex matches nothing. The evaluator no longer returns
`Unsupported`; the type stays public for API stability.

## Regex terms

`src/regex_term.rs` compiles a `Pattern::Regex` term (from `regex:` or
`SearchOptions::regex`) into `RegexTerm` with the `regex` crate (MIT OR
Apache-2.0; its dependencies `regex-syntax`, `regex-automata`,
`aho-corasick` and `memchr` are MIT OR Apache-2.0 or Unlicense OR MIT).
`RegexTerm::compile(pattern, &TermOptions)` returns a `RegexError`
carrying the crate's message for an invalid pattern.

- Unanchored, as in Everything: the user writes `^` and `$`.
- Case-insensitive (Unicode simple case folding) unless the term matches
  case (`case:` or `SearchOptions::match_case`).
- In path mode (`path:` or `match_path`) the expression sees the full
  path, otherwise the name. A `/` in the expression does not switch to
  the path.
- **The diacritics option does not apply to regex terms**: `regex:resume`
  does not match `Résumé.pdf`. Pattern and haystack are put in NFC, so an
  NFD-stored name still matches a composed pattern and `.` matches one
  accented letter.
- The parser keeps the text after `regex:` raw, so `regex:case:^FOO`
  stores the pattern `case:^FOO`; `RegexTerm::compile` peels a leading
  run of known modifier names off the pattern and applies them
  (`RegexTerm::options()` gives the result).
- `noregex:` turns a global regex toggle off for one term, which then
  takes the literal or wildcard path.

**Dialect difference from PCRE.** The `regex` crate guarantees linear-time
matching and so has no look-around (`(?=`, `(?!`, `(?<=`, `(?<!`) and no
back-references (`\1`); patterns using them fail to compile and match
nothing. Possessive quantifiers and atomic groups are also absent. Inline
flags such as `(?i)`/`(?-i)` and Unicode classes (`\p{L}`) work.

## Compiled matcher

`src/matcher.rs` turns a `Parsed` tree into a `CompiledQuery` once and
runs it over any `SearchView`. The eval oracle stays the specification:
for every live entry the result equals `eval::matches_in`, which the
seeded differential tests in `tests/matcher.rs` check across literals,
wildcards, regexes, `path:`, functions, boolean structure and modifiers,
with accented (NFC and NFD) names.

- `SearchView` (`Sync`) needs `len`, `name`, `parent`, `is_folder` and
  `is_live`. Metadata (`size`, `modified`, `created`, `accessed`,
  `child_count`, `hidden`, `symlink`, `read_only`) defaults to unknown /
  not set, and `folded(id, FoldOptions)` may return a prebuilt folded
  name (otherwise names are folded per entry). fx-query does not depend
  on fx-index; fx-engine implements the trait over a snapshot.
- `VecView` is an owned view for tests and benchmarks: `push(name,
  parent, is_folder, Metadata) -> id`, `mark_deleted(id)`, and
  `build_folded(FoldOptions)` for a `FoldedColumn`.
- `CompiledQuery::compile(&parsed, &opts, EvalContext)` lowers terms:
  literals get one `memmem::Finder` on the folded needle, run over the
  folded name, with start/end/whole-word/whole-name checks on the found
  positions; wildcards fold their pieces once, and a name-only `*.ext`
  becomes a suffix compare on the folded name; regexes compile once (an
  invalid one matches nothing); `ext:` and date functions precompute
  their lists and periods, the rest go through `eval::filter_matches`;
  `file:` / `folder:` are checked before the text. `And` children run
  cheapest first.
- Paths are never stored, and most path terms never build one
  (`src/paths.rs`). A plain path literal (no `ww:`, `start-with:`,
  `end-with:` or `wfn:`) is decided per folder once per search: the
  needle occurs in an entry's folded path exactly when it occurs in its
  folder's path or in the short window of the folder path's last
  `len - 1` bytes, the separator and the entry's folded name (just the
  name when the needle has no `/`). A per-search memo of one byte per
  entry holds each answer, filled lazily from the roots down and shared
  by the search threads, so an entry costs one lookup and a search of its
  name. Folding distributes over the join (`/` never composes), so the
  result is the same as folding the whole path. Other path terms
  (wildcards, regexes, constrained literals, `parent:`, `depth:`) build
  the path from the folder's cached path, and its cached folded path per
  folding, plus the entry's name, in a per-thread cache for one search.
  A root's name is its full path. Building and folding every full path,
  as an earlier version did, cost about 280 ms p95 over 2M entries;
  allocation and copying took most of it, the substring search about 1%.
- `search` scans fixed chunks of 16,384 entries with rayon and returns
  ascending IDs; `count` counts; `search_within(candidates)` refines an
  earlier result. All three check the `CancelToken` before each chunk
  (at least every 65,536 entries) and return `Cancelled` once the
  generation moves on. `limit()` exposes `count:N` without applying it.

## Refinement cache

`src/refine.rs` holds `Searcher`, which wraps `CompiledQuery` searches
for one view generation, tested in `tests/refine.rs` against fresh
searches.

- `search(text, &opts, &view, &cancel) -> Result<Arc<[u32]>, Cancelled>`
  returns the live matching IDs in ascending order, identical to a fresh
  `CompiledQuery::search`.
- An LRU of recent `(text, options)` result sets (`DEFAULT_CAPACITY` = 8,
  `Searcher::with_capacity(n)` otherwise) answers repeats and backspace
  with the cached `Arc`.
- When the new query provably narrows a cached one, it runs
  `search_within` over the cached hits (the smallest qualifying set).
  The rule works on the parsed tree: same `SearchOptions`, both queries a
  positive AND of plain literal terms (no OR, NOT, wildcard, regex,
  function, whole-word, `start-with:`, `end-with:` or `wfn:`), and every
  old term's folded text a substring of some new term's folded text with
  the same folding, the same haystack (name or full path) and a
  compatible `file:`/`folder:` kind. Substring is transitive, so this
  covers typing on (`re` → `rep`), appending terms (`rep` → `rep txt`)
  and merging them (`re port` → `report`).
- `stats()` gives `SearchStats { full_scans, refined, cache_hits }`;
  every successful search bumps exactly one.
- `set_epoch(e)` clears the cache when the view epoch changes;
  `invalidate()` clears it always; `set_context(EvalContext)` clears it
  when the clock for date functions changes. A cancelled search leaves
  the cache and counters untouched.

## Search field options

`QueryOptions { match_case, match_path, regex }` are the window's three
chips (Aa, Path, .*), all off by default. `search_options()` gives the
`SearchOptions` they stand for. With `regex` the whole input is one
regular expression, spaces and operator characters included:
`query_text(input)` wraps it with `whole_regex_text` (in double quotes,
a `"` written as `\x22`, as the command line's `-r` does) and `parse`
parses that under the regex toggle, so the engine, its refinement cache
and the highlighter all see one regex term. An empty input stays empty
and matches everything.

## Match highlighting

`Highlighter::new(&parsed)` or `Highlighter::for_query(input, options)`
prepares a query once; `highlight(folder, name, is_folder)` returns
`Highlights { name, folder }`, the ranges to highlight in the entry's
name and in the full path of its folder (`""` for a root), in **UTF-16
code units** of those strings (what `NSRange` and `NSAttributedString`
count), sorted, overlapping and touching ranges merged.

- Literal terms not under `!`: every occurrence, found with the
  matcher's folding (case, accents, the apostrophe rule) and mapped back
  to the original text, so a match covers decomposed marks, letters that
  folding changed in length and dropped apostrophes. `ww:`, `start-with:`,
  `end-with:` and `wfn:` keep only the occurrences they accept; `file:`
  and `folder:` terms highlight only entries of their kind.
- Regex terms (and the whole input with the regex chip): each non-empty
  match on the NFC text, mapped back.
- Wildcards: their literal pieces where the pattern matches (first at the
  start unless the pattern starts with `*`, last at the end unless it ends
  with `*`, the others at their first place after the one before);
  patterns with `?` are skipped, since what `?` stood for is not mapped.
  Functions highlight nothing.
- Path terms are found in the joined full path and split between folder
  and name; the separator is in neither.
- `replace_prefix(ranges, prefix, replacement)` moves folder ranges onto a
  display string whose first `prefix` units became `replacement` units
  (the home folder shown as `~`).

Mapping: ASCII maps byte for byte; other text is folded in segments (a
character and the combining marks after it) and each folded byte maps to
its segment, unless characters compose across segments (Hangul jamo), in
which case the cut is coarser (before ASCII characters), and failing
that the whole string is one segment.

`cargo run --release -p fx-query --example highlight_bench` times
preparing each of a set of queries and highlighting 10,000 synthetic
rows (`--names`, `--passes`, `--seed`), as generated and with an accent
and a curly apostrophe in every name.

## Case and diacritic folding

`src/fold.rs` holds `fold_str(s, FoldOptions { case, diacritics })` and
`FoldedColumn`. Dependency: `unicode-normalization` (MIT OR Apache-2.0)
for the NFC/NFD tables and combining-mark data, which std lacks.

- The output is always canonically normalized, so an NFD name as APFS
  may store it (`e` + U+0301) equals its NFC spelling (`é`). With
  `diacritics: false` the output is NFC.
- `case` lowercases per `char` with std's mapping: no expansion (`ß`
  stays `ß`, `strasse` does not match `Straße`), no Greek final-sigma
  rule, Turkish `İ` becomes `i` + U+0307 as in std, dotless `ı` stays.
- `diacritics` decomposes to NFD and drops attached combining marks
  (nonzero canonical combining class), then recomposes what is left
  (only Hangul-style starter pairs, never an accent). Spacing vowel
  signs (Mc) and enclosing marks (Me) are kept, so Indic words keep
  their vowels. Letters without a decomposition (`æ`, `ø`, `œ`, `ı`)
  stay as they are.
- ASCII input is borrowed unchanged (or lowercased byte by byte when
  folding case).
- The apostrophe rule: with
  `diacritics`, the apostrophes `'`, `’`, `‘` and `ʼ` are written as
  `'` when `keep_apostrophes` is set and dropped otherwise. Names are
  folded with them kept (`FoldOptions::for_match`); a term is folded with
  `FoldOptions::for_term`, which keeps them only when its text has an
  apostrophe, and the name is compared in the term's folding. So a term
  without an apostrophe ignores the name's (`plumbs` finds `Plumb’s.pdf`,
  `ww:plumb` does not, `plumb?s` does not), and a term with one finds
  only names with an apostrophe in that place (`plumb's` finds
  `Plumb’s.pdf` but not `plumbs.pdf`; `'` finds every name with one).
  `diacritics:` and `regex:` see the apostrophes as written. (An earlier
  rule dropped the apostrophes from every term, so a term with one could
  not tell `Plumb’s.pdf` from `plumbs.pdf`.)

`FoldedColumn::build(&NameColumn, FoldOptions)` builds the folded search
copy of a name column in parallel (rayon): its own NUL-separated buffer
and `u32` offsets, entry `i` folded from entry `i`. Folded names may
change length, so offsets are recomputed; all-ASCII columns are a byte
copy. `FoldedColumn::column()` gives it back as a `NameColumn` for the
substring scan. Built with apostrophes kept, it also holds a sparse side
table with the apostrophe-free form of each name that has an apostrophe
(a bit per entry marks them; nothing is allocated when no name has one),
and `folded_name(i, opts)` serves both foldings; `side_table_bytes()`
and `heap_bytes()` report its size.

Search functions keep their own comparisons: `ext:` and
`parent:` lowercase per `char` without normalizing or stripping accents,
and `len:` counts the stored `char`s.

## Search functions

`src/syntax/functions.rs` holds the parsed form of search functions such
as `size:>1mb`, `dm:today` or `ext:txt;md`: `Node::Function(Filter)`.
`Filter` (non-exhaustive) has `Ext`, `Size`, `Date`, `Parent`, `Depth`,
`Len`, `Empty` and `Attrib` variants, built from the shared comparison
types `Cmp` (`:`/`=` → `Eq`, `==` → `ExactEq`, `!=`, `<`, `<=`, `>`, `>=`)
and `Range<T>` (`a..b`, both ends included), combined in
`ValueTest<T>`. Date values (`DateValue`: an ISO 8601 `CalendarDate` or a
`DateKeyword`) are periods resolved at evaluation time against the clock.
`Filter::name()` and `Filter::args()` give a canonical text form. A
`count:` result limit goes to `Parsed::count` instead of the tree.

`Candidate` carries optional metadata: `size`, `modified`, `created`,
`accessed` (Unix seconds), `child_count`, and the `hidden`, `symlink` and
`read_only` flags. It implements `Default`, so callers write
`Candidate { name, parent_path, is_dir, ..Default::default() }`.
`eval::matches_in(&node, &cand, &EvalContext { now, utc_offset_secs })`
evaluates against a given clock and time zone; `eval::matches` uses the
fixed `EvalContext::default()` (2026-11-03T10:00:00Z, UTC). Missing
metadata never makes a function match, and an unknown size matches only
`size:unknown`.

### Size and entry functions

Parsed by `functions::parse_function` (hooked into the parser for any
word whose unquoted `name:` is a known function; names are
case-insensitive). The readings below are recorded in the module docs of
`src/syntax/functions.rs` and `src/syntax/size.rs`.

| Function               | Meaning                                                     |
|------------------------|-------------------------------------------------------------|
| `size:`                | comparison or range over sizes; never matches folders       |
| `ext:a;b`              | extension after the last `.`, case-insensitive; `ext:` alone = none |
| `parent:` `infolder:`  | direct children of exactly that folder                      |
| `depth:`               | full-path components (`/Volumes` is 1)                      |
| `len:`                 | name length in `char`s                                      |
| `empty:`               | folders with a known child count of 0                       |
| `count:N`              | sets `Parsed::count`; filters nothing                       |
| `attrib:`              | `D` folder, `H` hidden, `L` symlink, `R` read-only; others never match |

`size:`, `depth:`, `len:` (and the date functions) share the comparison
grammar: an optional `=`, `==`, `!=`, `<`, `<=`, `>`, `>=`, then a value,
or a range `a..b`. Right after such a function's `:` the lexer keeps `<`
and `>` in the word, so `<size:<1kb ext:txt>` is a group of two functions.

Sizes take `b`, `kb`, `mb`, `gb`, `tb` (any case, steps of 1024, optional
decimals); a bare number is bytes. A size stands for a bucket at its
written precision: `size:1kb` is 1024..=2047, `size:==1kb` exactly 1024,
`<`/`>=` compare with the bucket's start, `<=`/`>` with its end, `!=` is
outside it, and `a..b` runs from `a`'s start to `b`'s end. Constants:
`empty` 0, `tiny` ≤ 10 KB, `small` ≤ 100 KB, `medium` ≤ 1 MB, `large`
≤ 16 MB, `huge` ≤ 128 MB, `gigantic` above, `unknown` for an unknown size.

A missing or malformed argument (`size:`, `size:abc`, `size:>`,
`count:-1`) drops the word with a `MissingArgument` or `InvalidArgument`
diagnostic, so other terms still apply while the user types.

### Date functions

`dm:` / `datemodified:`, `dc:` / `datecreated:` and `da:` /
`dateaccessed:` test `Candidate::modified`, `created` and `accessed`.
Values are parsed in `src/syntax/date.rs`, which also holds the readings
and the hand-written calendar arithmetic (days-from-civil and its inverse,
proleptic Gregorian, std only).

- Local time is `EvalContext::now` plus the fixed `utc_offset_secs`; there
  is no time-zone database and no daylight saving.
- ISO 8601 `YYYY`, `YYYY-MM`, `YYYY-MM-DD`, `YYYY-MM-DDTHH:MM` and
  `YYYY-MM-DDTHH:MM:SS` (exact field widths). A partial date is its whole
  period: `dm:2026-11` is all of November 2026.
- Keywords (any case): `today`, `yesterday`, `thisweek`, `lastweek`,
  `thismonth`, `lastmonth`, `thisyear`, `lastyear`. Weeks run Monday
  00:00 to the next Monday; `thisweek` is the whole current week, days
  still to come included, like `thismonth`.
- Comparisons are against the period: `=` inside, `!=` outside, `<`
  before its start, `<=` before its end, `>` from its end on, `>=` from
  its start on, `==` its first instant. `dm:2026-01..2026-03` runs from
  the start of January to the end of March.
- Invalid dates (`dm:2026-13`, `dm:2026-02-30`, `T24:00`, an hour without
  minutes) give `InvalidArgument` and the word is dropped; periods that do
  not fit in `i64` seconds at evaluation time match nothing.

### Type macros

`audio:`, `doc:`, `exe:`, `pic:`, `video:` and `zip:` (any case) stand
for an `ext:` list, kept in `src/syntax/macros.rs` and returned by
`syntax::macros::macro_extensions(name)`. The six categories follow the
public Searching page; the lists are our own, with macOS types added:

| Macro    | Covers (examples)                                                   |
|----------|---------------------------------------------------------------------|
| `audio:` | `mp3`, `wav`, `flac`, `aac`, `m4a`, `ogg`, `aiff`, `caf`, ...       |
| `doc:`   | `pdf`, `doc(x)`, `xls(x)`, `ppt(x)`, `pages`, `numbers`, `key`, `txt`, `md`, `odt`, `epub`, ... |
| `exe:`   | `exe`, `bat`, `msi`, `app`, `pkg`, `mpkg`, `command`, ...           |
| `pic:`   | `jpg`, `png`, `gif`, `tiff`, `webp`, `heic`, `avif`, camera raw, `psd`, `svg`, ... |
| `video:` | `mp4`, `mkv`, `avi`, `mov`, `m4v`, `webm`, `wmv`, ...               |
| `zip:`   | `zip`, `rar`, `7z`, `tar`, `gz`, `xz`, `zst`, `dmg`, `iso`, ...     |

- A bare macro (`pic:`) is exactly `Node::Function(Filter::Ext(list))`.
- With an argument (`pic:holiday`) it is `And([ext filter, Term])`; the
  argument is an ordinary term, so wildcards (`pic:IMG_*`) and quotes
  (`pic:"summer holiday"`) work, and modifiers before the macro apply to
  it.
- Macros combine with `!`, `|` and groups like any function:
  `!pic:holiday` negates the pair, `<pic: | doc:> holiday` works.
- `"pic:"` (quoted) and unknown names (`pics:`) are literal text.

### Supported syntax at a glance

| Kind           | Syntax                                                                 |
|----------------|------------------------------------------------------------------------|
| operators      | space (AND), `\|` (OR), `!` (NOT), `<` `>` (group), `"` (literal)      |
| wildcards      | `*`, `?`, `**` (whole name, or whole path when matching paths)         |
| modifiers      | `case:`, `path:`, `ww:`/`wholeword:`, `wfn:`/`wholefilename:`, `regex:`, `diacritics:` (each with a `no` form), `file(s):`, `folder(s):`, `start-with:`, `end-with:` |
| functions      | `ext:`, `size:`, `dm:`/`dc:`/`da:` (and long names), `parent:`/`infolder:`, `depth:`, `len:`, `empty:`, `count:`, `attrib:` |
| comparisons    | `=`, `==`, `!=`, `<`, `<=`, `>`, `>=`, ranges `a..b`                   |
| size values    | bytes, `kb`/`mb`/`gb`/`tb` with decimals; `empty`, `tiny`, `small`, `medium`, `large`, `huge`, `gigantic`, `unknown` |
| date values    | ISO 8601 `YYYY[-MM[-DD[THH:MM[:SS]]]]`; `today`, `yesterday`, `thisweek`, `lastweek`, `thismonth`, `lastmonth`, `thisyear`, `lastyear` |
| macros         | `audio:`, `doc:`, `exe:`, `pic:`, `video:`, `zip:`                     |

## The substring scan

Names live in one buffer, each followed by a NUL byte, with a parallel
array of `u32` start offsets (the layout of `fx_bench::Names`).
`NameColumn::new` checks that layout once: the first offset is 0, offsets
strictly increase, the byte before every later offset is NUL, and the
buffer ends with NUL.

A query builds one `memchr::memmem::Finder` and reuses it for the whole
scan. The finder runs over the bytes of a whole run of entries at once
instead of once per name, which keeps memchr's vectorised search on long
inputs. An empty needle matches every entry and a needle containing NUL
matches nothing.

### Chunking

The entry range is split into chunks of `DEFAULT_CHUNK_ENTRIES` (16,384)
entries, so chunks always start and end on entry boundaries. 5M names make
about 300 chunks, enough for rayon to balance the load across threads.
The current rayon pool scans the chunks in parallel; call
`ThreadPool::install` to choose the thread count.

Inside a chunk, each hit offset is mapped back to its entry ID by galloping
forward through the offsets and then binary-searching. Dense hits cost a
step or two, sparse hits a logarithmic search. After a hit the scan
resumes at the start of the next entry, so an entry is reported at most
once. A needle never contains NUL, so a hit can never span two names. Chunk
results are concatenated in chunk order, so IDs come back in ascending
order whatever the thread count or chunk size.

### Case folding

ASCII case-insensitive search (`CaseMode::AsciiInsensitive`) runs against
a copy of the buffer made by `fold_ascii_lowercase`, with the needle
lowercased too. The copy has the same length as the original, so the same
offsets work for both. Non-ASCII bytes must match exactly; Unicode case
folding and normalization belong to the compiled matcher (`fold.rs`,
`matcher.rs`).

### Cancellation

Search-as-you-type needs to drop a running search as soon as the user types
the next key. A `Generation` is an atomic counter that the caller bumps for
each new query. `Generation::token()` captures the current value. The scan
checks the token before each chunk and at least every `CHECK_INTERVAL`
(65,536) entries, and returns `Err(Cancelled)` once the generation has
moved on. `CancelToken::never()` is a token that is never cancelled.

## Dependencies and licences

| Crate    | Version | Licence                                    |
|----------|---------|--------------------------------------------|
| `memchr` | 2.x     | Unlicense OR MIT (used under MIT)          |
| `rayon`  | 1.x     | MIT OR Apache-2.0                          |

rayon's transitive dependencies (`rayon-core`, `crossbeam-deque`,
`crossbeam-epoch`, `crossbeam-utils`, `either`) are all MIT OR Apache-2.0.
None are GPL. `fx-bench` (this workspace, MIT) is a dev-dependency only,
used for synthetic names and the benchmark harness.

## Benchmark: `search_bench`

The Phase 0 search gate is p95 ≤ 10 ms over 5M synthetic names on 8
threads (warm cache):

```sh
cargo run --release -p fx-query --example search_bench -- --threads 8 --cache warm
```

| Flag           | Default     | Meaning                                         |
|----------------|-------------|-------------------------------------------------|
| `--count`      | `5000000`   | synthetic names to generate (1..=100,000,000)   |
| `--seed`       | `1`         | `fx_bench::generate_names` seed                 |
| `--threads`    | `8`         | rayon pool size for the search (1..=256)        |
| `--warmup`     | `10`        | untimed searches per query first                |
| `--iterations` | `100`       | timed searches per query                        |
| `--cache`      | `unknown`   | `warm`, `cold` or `unknown`: a label only       |
| `--json`       | off         | print one JSON object instead of text           |

The names are generated once, before any timing, and an ASCII-folded copy
is built from them. The benchmark then times `search` (with a
never-cancelled token) through the `fx-bench` harness, one
`ThreadPool::install` call per sample. It uses this fixed query set:

| Query      | Needle      | Case              | Gated |
|------------|-------------|-------------------|-------|
| 1-char     | `a`         | sensitive         | yes   |
| 2-char     | `re`        | sensitive         | yes   |
| 3-char     | `por`       | sensitive         | yes   |
| stem+ext   | `config.rs` | sensitive         | yes   |
| rare       | `zqxj`      | sensitive         | yes   |
| folded     | `ReadMe`    | ASCII-insensitive | no    |

`config.rs` joins a word from the generator's vocabulary with the `.rs`
extension. `zqxj` has almost no hits, so it measures the pure scan cost.
The folded query runs on the folded copy and matches both `readme` and
`README`.

For each query it reports the hit count and p50, p95, p99 and max in
milliseconds, along with the machine (OS, architecture, logical CPUs, CPU
brand) and the cache label. The last line is the gate:

```text
gate p95<=10ms over 5000000 names on 8 threads: PASS (worst p95 X ms, query "a")
```

Only the case-sensitive queries count towards the gate; the folded query is
for information. Gate results don't change the exit code. The exit code is
0 after a completed run, 1 if setup fails, and 2 for bad arguments.
Progress lines go to stderr, so `--json` output on stdout can be piped as
is.

The search works only on memory, so "warm" here means that the name buffer
is resident and the warmup iterations have run. The `--cache` flag is just a
label.

## Benchmark: `matcher_bench`

`examples/matcher_bench.rs` measures the compiled matcher and the
refinement cache against the Phase 1 search budgets in `docs/PLAN.md`
(Goal 2). `search_bench` above stays as the Phase 0 record.

```sh
# Gate evidence: release builds on a real machine.
cargo run --release -p fx-query --example matcher_bench -- --entries 2000000 --cache warm
cargo run --release -p fx-query --example matcher_bench -- --entries 5000000 --cache warm
# Smoke test: runs in seconds, debug builds included; not gate evidence.
cargo run -p fx-query --example matcher_bench -- --quick
```

| Flag        | Default                   | Meaning                                          |
|-------------|---------------------------|--------------------------------------------------|
| `--entries` | `2000000` (`50000` quick) | synthetic tree entries (1..=50,000,000)          |
| `--threads` | `8`                       | rayon pool size for the search (1..=256)         |
| `--iters`   | `50` (`3` quick)          | timed samples per query and type-ahead sequence  |
| `--warmup`  | `5` (`1` quick)           | untimed runs first                               |
| `--seed`    | `1`                       | `fx_bench::generate_tree` seed                   |
| `--cache`   | `unknown`                 | `warm`, `cold` or `unknown`: a label only        |
| `--apostrophes` | `0`                   | names per million given `’s` before the extension, so the folded copy's side table has entries |
| `--quick`   | off                       | 50k entries, 3 iterations, 1 warmup; explicit flags still win |

Setup happens before any timing. `fx_bench::generate_tree` builds the
tree (about 19% folders) and the benchmark loads it into a `VecView`.
The root is named `/` so full paths look like `/Library/...`. Files get
a deterministic synthetic size (2% empty, otherwise log-uniform up to
256 MB) so `size:` has metadata to test. Then the default folded copy
(case and diacritics folded, apostrophes kept as `'`, plus the side table
of apostrophe-free forms) is built; the report gives its heap bytes and
the side table's.

Each sample is what the engine does for one keystroke: `parse`,
`CompiledQuery::compile` and `CompiledQuery::search` with a
never-cancelled token, in one `ThreadPool::install`. The query set:

| Label       | Class    | Query                    | Gate      |
|-------------|----------|--------------------------|-----------|
| 1-char      | literal  | `a`                      | engine    |
| 2-char      | literal  | `re`                     | engine    |
| 3-char      | literal  | `por`                    | engine    |
| 4-char      | literal  | `conf`                   | engine    |
| rare        | literal  | `zqxj`                   | engine    |
| and         | and      | `data json`              | engine    |
| ext         | ext      | `*.png`                  | engine    |
| function    | function | `size:>1mb`              | engine    |
| regex       | regex    | `regex:^img_\d+\.jpg$`   | regex     |
| path        | path     | `path:resources/`        | full-path |
| path-rare   | path     | `path:zqxj`              | full-path |

`zqxj` measures the pure scan cost. `path:zqxj` tests every entry's full
path (through the folder memo), and `path:resources/`
crosses a separator, so its window includes the end of the folder's
path.

The type-ahead row types `config js` one keystroke at a time and then
backspaces to `co`, with a fresh `Searcher` for each iteration. It
reports p50/p95/max per keystroke, split by how the searcher answered
(full scan, refined, cache hit), plus the whole sequence. Afterwards it
runs the sequence once more, untimed, and compares every answer with a
fresh search. A mismatch is an error (exit code 1).

Output: a header with the entry and folder counts, threads, cache label
and build profile, the machine line (OS, architecture, logical CPUs, CPU
brand), one row per query with hits and p50/p95/max in milliseconds, the
type-ahead table, and then one `GATE` line per budget group in the
format `fx-bench gate-summary` parses:

```text
GATE engine p95 <= 16 ms at 2000000 entries: PASS (worst p95 X ms, query "a"; 8 threads, cache warm; 2M budget)
GATE regex p95 <= 100 ms at 2000000 entries: PASS (worst p95 X ms, query "regex:^img_\\d+\\.jpg$"; ...)
GATE full-path p95 <= 100 ms at 2000000 entries: FAIL (worst p95 X ms, query "path:zqxj"; ...)
```

| Gate        | Queries                          | Budget (p95)                               |
|-------------|----------------------------------|--------------------------------------------|
| `engine`    | literals, AND, `*.ext`, `size:`  | ≤ 16 ms up to 2M entries, ≤ 30 ms up to 5M |
| `regex`     | the regex query                  | ≤ 100 ms up to 2M entries                  |
| `full-path` | both `path:` queries             | ≤ 100 ms up to 2M entries                  |

Each gate checks the worst p95 in its group. The budgets are never
scaled: a run below 2M entries is held to the 2M budget, a run between
2M and 5M gets the 5M engine budget, and a size the plan sets no budget
for prints `SKIP` (regex and full-path above 2M, everything above 5M).
The engine group is stricter than "literal queries" alone: `*.ext` and
`size:` are engine queries too. A missed budget prints `FAIL`. Lines
from `--quick` runs or debug builds end in `not gate evidence: quick
mode, debug build`. Gate results don't change the exit code: 0 after a
completed run, 1 if setup or the type-ahead check fails, 2 for bad
arguments. Progress lines go to stderr.

### Recording the gate

`scripts/phase1-gate.sh` runs both sizes with the other Phase 1 gate
benchmarks, and the results are kept in `docs/gates/phase-1.md` (Goal
2). To run `matcher_bench` on its own for the gate item "search budgets
met in benchmarks":

1. Use a release build, on AC power, with other heavy apps closed.
2. Run `--entries 2000000` and then `--entries 5000000`, one at a time,
   with the default 8 threads and `--cache warm`. The index is in
   memory, so "warm" means the generated view is resident and the warmup
   runs have finished. For a cold-cache record, run once right after a
   reboot and label it `--cache cold`.
3. Save each run's stdout to a log (for example
   `... > logs/matcher_bench_2m.log`), and optionally summarize them with
   `cargo run --release -p fx-bench -- gate-summary --machine m4pro
   --cache warm logs/matcher_bench_2m.log logs/matcher_bench_5m.log`.
4. Record the `GATE` lines, the machine line and the cache state with
   the results in `docs/gates/phase-1.md`, FAIL lines included.
