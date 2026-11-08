# not-everything: searching the index from a terminal

`not-everything` searches the index that the app saves to disk. It loads
the saved store, runs one query and prints the matches. It is meant for
scripts and shells; the app does not need to be running.

```
not-everything [options] [search words...]
```

The search words are joined with single spaces into one query in the
app's search syntax, so operators pass through as typed:

```
not-everything report pdf              # names containing "report" and "pdf"
not-everything "main.rs|lib.rs"        # either name
not-everything -sort size -sort-descending -n 10 ext:mov
not-everything -csv -name -size -dm -export-csv ~/movies.csv ext:mov
not-everything -0 ext:mov | xargs -0 du -h   # names with spaces or newlines are safe
```

## Read-only

`not-everything` never changes anything on disk except the export file you name
with `-export-*`:

- The store is opened read-only. It is not rewritten, renamed or
  repaired, and no lock or temp file is created next to it, so a store in
  a read-only folder works.
- Nothing is rescanned and no file is opened or downloaded; results come
  from the saved index only, so they are as fresh as the app's last save.
- There is no option to delete, rename, move or trash a file.

## Which store is read

- `-store <file>` reads that store file, and only that file.
- Otherwise the app's store for `-root <dir>` (default `/`) is read from
  `$HOME/Library/Application Support/Not-Everything/stores/`. The file is
  named after the root's last component and a hash of the root's path, the
  same way the app names it.
- If there is no such file, the store the app built without Full Disk
  Access is read instead: the file of the same name in
  `$HOME/Library/Application Support/Not-Everything/stores-limited/`.
  This limited store leaves out the protected folders macOS only lets an
  app read with Full Disk Access, so files in them are missing from the
  results. A note on standard error says when
  the limited store is used. A full store that exists but is rejected
  exits with 7; the limited store is not tried then.
- No other folder is read: if neither file is there, the run exits
  with 8.
- The Preview command, `not-everything-preview`, which comes with release
  candidates and betas, reads the Preview app's folder,
  `$HOME/Library/Application Support/Not-Everything Preview/`, the same
  way. Its `-version` prints the build's label, such as `0.2.0-RC-1`.

The root is resolved the way the app resolves it before the store is
looked up: a relative path is taken from the current folder, and trailing
slashes, `.`, `..` and symbolic links are resolved, so `-root ~/proj/`,
`-root ../proj` and a symlink to the folder all find the store the app
saved for `~/proj`. A root that doesn't exist is used as given (and
usually has no store, so the run exits with 8).

When a root other than `/` was indexed, the store holds paths relative to
the folder that contains the root. Pass `-root` (also together with
`-store`) to have full paths printed from `/`; the resolved root's parent
folder is the prefix.

## Options

Every option works with one or two leading dashes (`-n` or `--n`).
Options and search words may be mixed in any order. `--` ends the
options: everything after it is a search word, even if it starts with a
dash. A repeated option keeps its last value.

### Search

| Option | Effect |
|---|---|
| `-i`, `-case` | Match upper and lower case exactly. |
| `-w`, `-ww`, `-whole-word` | Terms match whole words only. |
| `-p`, `-match-path` | Terms are tested against the full path, not just the name. |
| `-a`, `-diacritics` | Accented letters and apostrophes must match exactly (`é` does not match `e`, `plumbs` does not match `Plumb’s`, and `plumb's` does not match `Plumb’s`). Without it, accents are ignored and the apostrophes `'`, `’`, `‘` and `ʼ` count as one character: a search word without an apostrophe ignores them (`plumbs` finds `Plumb’s.pdf` and `plumbs.pdf`), and a word with one finds only names with an apostrophe in that place (`plumb's` finds `Plumb’s.pdf` but not `plumbs.pdf`). |
| `-r <pattern>`, `-regex <pattern>` | Search with a regular expression (see below). |

Modifiers inside the query (`case:`, `ww:`, `path:`, `regex:`, ...) still
work and override the toggles for the term they prefix.

### Results

| Option | Effect |
|---|---|
| `-n <num>`, `-max-results <num>` | Print at most `<num>` results. |
| `-o <num>`, `-offset <num>` | Skip the first `<num>` results. |
| `-sort <field>` | Sort by `name` (the default), `path`, `size`, `date-modified` or `date-created`. |
| `-s` | Sort by path; the same as `-sort path`. |
| `-sort-ascending` | Smallest, oldest or A first (the default). |
| `-sort-descending` | Reverse the sort. |
| `-get-result-count` | Print only the number of matches. |
| `-no-result-error` | Exit with code 9 when nothing matches. |

Paging applies after sorting: `-sort size -o 20 -n 10` prints the 21st to
30th smallest matches. `<num>` is a non-negative whole number. Equal
values are ordered by name, so the output order is always the same for
the same store and query.

A `count:N` in the query caps the total number of results at the first
`N` in sort order. Paging works inside that cap: `-o` skips results
within the first `N` (it does not move the cap), and `-n` limits further.
For example `-sort size -o 1 -n 5 count:3` prints the 2nd and 3rd
smallest matches, and `-o 3 count:3` prints nothing. With
`-get-result-count`, the count printed is the smaller of the number of
matches and `N`.

### Output

| Option | Effect |
|---|---|
| `-txt` | Plain text (the default). |
| `-0` | End each plain-text result with a NUL character instead of a newline, for `xargs -0` and other tools that read NUL-separated lists. Plain text only: with csv or json output (`-csv`, `-json`, `-export-csv`, `-export-json`) the run exits with 4. |
| `-csv` | Comma-separated values. |
| `-json` | A JSON array. |
| `-full-path-and-name` | Add the full path column. |
| `-name` | Add the name column. |
| `-path-column` | Add the containing-folder column. |
| `-size` | Add the size column. |
| `-dm`, `-date-modified` | Add the date-modified column. |
| `-dc`, `-date-created` | Add the date-created column. |
| `-export-txt <file>` | Write plain text to `<file>` instead of standard output. |
| `-export-csv <file>` | Write csv to `<file>` instead of standard output. |
| `-export-json <file>` | Write JSON to `<file>` instead of standard output. |

Columns appear in the order their options are given. Without any column
option the only column is the full path. An export file is created (or
replaced) only after the store has been read and the search has run.

### Index and other

| Option | Effect |
|---|---|
| `-store <file>` | Read this store file. |
| `-root <dir>` | Read the app's store for this root (default `/`); see "Which store is read". |
| `-h`, `-help` | Print the usage and exit. |
| `-version` | Print the version and exit. |
| `-licenses` | Print the third-party notices: the licences of the open-source crates the command is built with (`THIRD-PARTY-NOTICES.md`), and exit. |

## Output formats

All output is UTF-8. A name that is not valid UTF-8 is printed with
U+FFFD in place of the bad bytes.

**txt** prints one result per line, each line ending in `\n`, or in a
NUL character (`\0`) with `-0`. With several columns the values on a line
are separated by tabs. There is no header. No matches print nothing. A
file name can hold a newline or a tab but never a NUL, so scripts that
must handle every name should use `-0` (or json).

When standard output is a terminal, each control character in a name or
path (U+0000 to U+001F and U+007F to U+009F, such as a newline, a tab or
an escape) is shown as `?`, as `ls` does, so a file name can't move the
cursor, change colours or look like more than one result. Output to a
pipe or a file, including `-export-txt`, is never changed: it holds the
exact names.

**csv** follows RFC 4180: a header row of column keys, then one record
per result, each record ending in CRLF. A field holding a comma, double
quote, CR or LF is enclosed in double quotes, with inner quotes doubled.
No matches print the header only.

When standard output is a terminal, csv shows each control character in
a field as `?`, the same as txt, and then quotes the field by the rule
above, so a name whose only special character was a newline is not quoted
there. Output to a pipe or a file, including `-export-csv`, is never
changed: it holds the exact names.

> **Opening csv in a spreadsheet.** Excel, Numbers, LibreOffice and
> similar apps may run a field that starts with `=`, `+`, `-` or `@` as a
> formula. Full paths and containing folders start with `/` (unless a
> folder root's store is read with `-store` and no `-root`, when they
> start with the root folder's name), but a file name can start with any
> of these characters, so the name column can carry a formula someone
> planted in a file name. Import such a file with every column set to
> text, or don't open it in a spreadsheet unless you trust the names.

**json** prints an array of objects, one per result, keyed by the column
keys. `size` is a JSON number; every other value is a string. Strings
escape `"`, `\`, U+2028/U+2029 and every control character, U+0000 to
U+001F and U+007F to U+009F: `\n`, `\r`, `\t`, `\b` and `\f` for those
five, and `\u` escapes such as `\u001b` and `\u009b` for the rest. The
output is the same on a terminal, in a pipe and in an `-export-json`
file: no string holds a raw control character, and a JSON parser reads
back the exact names. No matches print `[]`.

Column keys, used for the csv header and the JSON object keys:

| Column | Key | Value |
|---|---|---|
| full path | `full_path` | `/Users/me/docs/a.txt` |
| name | `name` | `a.txt` |
| containing folder | `path` | `/Users/me/docs` |
| size | `size` | bytes; `0` for folders |
| date modified | `date_modified` | ISO 8601 UTC, `2020-09-13T12:26:40Z` |
| date created | `date_created` | ISO 8601 UTC |

`-get-result-count` prints the total number of matches (before `-o` and
`-n`, capped by any `count:N`) followed by a newline (also with `-0`), to
standard output, in any format; no export file is written.

Warnings about a query the parser had to repair (such as an unclosed
quote) go to standard error; the results are those of the repaired
query, as in the app. A regular expression that does not compile is an
error, not a warning (see below).

## Regular expressions

With `-r <pattern>` the whole search text is one regular expression,
including spaces and the characters `|`, `<`, `>` and `!`, which are not
treated as query operators. Matching is unanchored (use `^` and `$`) and
ignores case unless `-i` is given. To mix a regular expression with other
search syntax, leave out `-r` and use the `regex:` modifier inside the
query instead.

The dialect is the one of the Rust [`regex`](https://docs.rs/regex)
crate. It runs in linear time and therefore has **no look-around**
(`(?=...)`, `(?!...)`, `(?<=...)`, `(?<!...)`) and **no
back-references** (`\1`). A pattern that does not compile (the `-r`
pattern, or any `regex:` term in the query, even next to valid terms) is
an error: nothing is printed on standard output (no csv header, no JSON
`[]`, no count), a message naming the pattern and the problem is printed
on standard error, and the exit code is 7, in every output mode and also
with `-get-result-count` or `-no-result-error`.

With `-r` the pattern reaches the regex engine unchanged. A `"` in it is
a literal quote, and so is an escaped `\"`; a run of backslashes keeps
its regex meaning (`\\"` is a literal backslash followed by a literal
quote). For example `not-everything -r 'say "hi"'` finds `say "hi".txt`, and
`not-everything -r 'wow b'` finds `wow b.txt` but not `wow!<b>.txt`.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Success: results (possibly none), a count, the usage, the version or the notices were printed. |
| 1 | Not used. |
| 2 | Not used. |
| 3 | Out of memory. Reserved: a Rust program aborts instead. |
| 4 | An option is missing its argument, or the argument is invalid (for example `-n abc` or `-sort colour`), or `-0` is given with csv or json output. |
| 5 | The export file can't be created or written (missing folder, a folder in its place, no permission), or the results can't be written to standard output. |
| 6 | Unknown option. |
| 7 | The store was rejected (corrupt, truncated, unreadable or of another format version), or the query could not be run: a regular expression (the `-r` pattern or a `regex:` term) does not compile. A message is printed on standard error and nothing on standard output. |
| 8 | No store was found: the app has not indexed this root yet (there is neither a full nor a limited store for it), the `-store` file does not exist, or `$HOME` is not set. A message is printed on standard error. |
| 9 | Nothing matched and `-no-result-error` was given. Nothing is printed, except the count `0` with `-get-result-count`. |

The numbering follows the exit-code contract of the `es` command-line
tool, so scripts that check those codes keep working. Codes 1 and 2 mean
window-related failures there and have no equivalent here.

Malformed query syntax (an unclosed quote or `<` group, a dangling `|`)
is not an error: the parser repairs it and warns on standard error, and
the run exits with 0 (or 9 with `-no-result-error`). An invalid regular
expression is an error and exits with 7.

Output closed early by the reader (for example `not-everything foo | head`) is
not an error.
