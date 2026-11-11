# fx-engine

Owns one volume's live index: the writer thread that applies change
batches, lock-free snapshots published through `arc-swap`, the query API
(`Engine::search`, and `Engine::search_grouped` for "Your files first":
the entries in `EngineConfig::with_priority_folders`, hidden ones aside,
listed first; `Engine::positions` and `Engine::positions_grouped` for the
rows of known files in those results) and the checkpoint policy through
fx-store. Every search takes the search field's `QueryOptions` (match
case, match path, and the whole query as one regular expression; the
default is the plain search) and fails with `SearchError::InvalidRegex`
when a regular expression does not compile, or `SearchError::Cancelled`.
See the crate documentation in `src/lib.rs` for the full design.

## Tests

```text
cargo test -p fx-engine
```

- `tests/writer.rs`, `tests/moves.rs`, `tests/query.rs`,
  `tests/search_options.rs`, `tests/priority.rs` and `tests/checkpoint.rs` run on every platform with fake change sources,
  path stats and enumerators (`tests/common/mod.rs`).
- `tests/macos_e2e.rs` (macOS only) runs the engine with the real
  `FsEventsSource`, `MacPathStat` and `MacScanner` from fx-fs-macos over a
  private temporary directory. It checks that a create, a file rename, a
  folder move (carrying its contents), a folder rename and deletes each
  show up in `Engine::search` (within a generous 10 s, so a loaded machine
  does not fail them; the 1 s gate is the freshness benchmark's below),
  and that an offline catch-up (checkpoint, shut down, about 200 scripted
  changes, reopen from the saved `ResumeCursor::FsEvents`) leaves the
  index equal to a fresh scan with every merged sort order equal to
  `Order::build`. It only creates and removes its own temporary
  directories and usually finishes in a few seconds in debug builds.

- `tests/gate_metrics.rs` checks the Phase 1 gate paths at CI scale: the
  memory report (≤100 B per entry at 100k entries), an offline catch-up of
  500 changes equal to a fresh scan, and a first result from a reopened
  store (through `StoreReader` and `Engine::open`).

fx-fs-macos is a dev-dependency under
`[target.'cfg(target_os = "macos")'.dev-dependencies]` only; the library
itself stays platform-independent.

## Freshness benchmark

```text
cargo run --release -p fx-engine --example freshness_bench -- --ops 1000 --cache warm
```

`examples/freshness_bench.rs` (macOS only; on other systems it prints a
message and exits with 0) starts the engine with the real FSEvents source
over a fresh directory under `std::env::temp_dir()` and runs `N` scripted
operations in rotation: create, rename, delete, each file with its own
names. For every operation it keeps a search for that file's name open and
times from just before the syscall to the announcement (through
`Engine::subscribe`) of the first published generation whose search
results show the change. It reports p50, p95, p99 and max overall and per
operation kind, using `fx_bench::Stats` (nearest rank), plus the machine
(`fx_bench::MachineInfo`), the cache label and the batch window.

| Flag          | Default   | Meaning                                          |
| ------------- | --------- | ------------------------------------------------ |
| `--ops N`     | `1000`    | scripted operations, 1..=1000000                 |
| `--window MS` | `100`     | engine batch window in ms, 50..=200              |
| `--cache`     | `unknown` | `warm`, `cold` or `unknown`: a label only        |

`--cache` only labels the report; the benchmark does not flush anything.
Pass `--cache cold` after a reboot or `sudo purge`, `--cache warm`
otherwise. Run benchmarks one at a time and record the machine with the
numbers.

Gates (printed as `GATE` lines, readable by
`cargo run -p fx-bench -- gate-summary`):

- `GATE freshness p99 <= 1 s`: the freshness gate. Every operation must
  also reach search (none missed within 5 s).
- `GATE stretch freshness p95 <= 250 ms`: the stretch goal.

The benchmark is read-only with respect to the user's files: it only
creates, renames and removes entries inside its own temporary directory
and removes that directory at the end. Release numbers are recorded in
`docs/gates/phase-1.md` (goal 3).

## Phase 1 gate benchmarks

### Output format

`startup_bench`, `catchup_bench` and `first_run_bench` print one result per
line, nothing else on stdout (progress and errors go to stderr):

- `metric=value`: a metric name (ASCII letters, digits, `_`, `.`), `=`,
  then the value up to the end of the line. Durations end in `_ms`
  (milliseconds, three decimals) or `_seconds`; percentiles are
  `<name>_p50_ms`, `<name>_p95_ms`, `<name>_max_ms` and `<name>_n` (nearest
  rank, `fx_bench::Stats`). Every run starts with `bench=<name>`,
  `machine=<os arch, cpu>`, `threads=<n>` and `cache=<warm|cold|unknown>`.
- `GATE <criterion>: PASS|FAIL (<detail>)`: a verdict, in the format
  fx-bench `gate-summary` already reads.

`--cache` labels the output; for `startup_bench` `--cache cold` also
changes what is timed (see below). Run benchmarks one at a time, in
release builds, and record the machine with the numbers.

### Scale keys

Every gate bench writes the scale it ran at as a gate metric, so a
summary always shows the entry or operation count behind a number:

| Key                 | Bench              | Value                                          |
| ------------------- | ------------------ | ---------------------------------------------- |
| `startup_entries`   | `startup_bench`    | synthetic entries in the store (`--entries`)   |
| `catchup_changes`   | `catchup_bench`    | offline changes replayed (`--changes`)         |
| `first_run_entries` | `first_run_bench`  | live entries after the first full scan         |
| `freshness_ops`     | `freshness_bench`  | scripted operations run (`--ops`)              |

For example `startup_entries=2000000`, `catchup_changes=10000`,
`freshness_ops=1000`. `freshness_bench` also prints `freshness_samples`
and `freshness_missed`.

### Memory report

`EngineSnapshot::memory_report()` returns a `MemoryReport`:
`live_entries`, `id_slots`, `total_bytes`, `columns` (`(label, bytes)`)
and `bytes_per_entry()` (`total_bytes / live_entries`). Its `Display`
output is the format above: `live_entries`, `id_slots`, `total_bytes`,
`bytes_per_entry` (two decimals), then `column.<label>=<bytes>` for
`names` (NUL-separated UTF-8), `name_offsets`, `parents`,
`size_kind_flags`, `times` (modified and created), `ext_ids`,
`ext_table`, `file_keys` (volume and file ID), `key_table`,
`deleted_bitset`, `delta_overlay` (uncompacted updates and inserts: rows
and maps; their names are counted under `names`), `folded_search_copy`,
`sort_orders` (the four pre-sorted orders at their allocated
capacity) and `priority_group` (the "Your files first" bitset, one bit
per entry ID; 0 without priority folders). This is real heap accounting: the index columns come from
`fx_index::Snapshot::heap_columns()`, whose sum is
`fx_index::Snapshot::heap_bytes()`, the same accounting as
`Index::heap_bytes()` (allocated capacities, not lengths) without the
writer-only parts. The engine keeps no folded search copy (the matcher
folds while scanning), so that column is 0. Writer-only structures (the
index's children index, a pending compaction remap) and the query
refinement cache are not part of a snapshot and are not counted. Since
run 2.17 the writer keeps no copy of the entries of its own: it finds the
entry a changed path names, compares entries and lists subtrees through
the index (`Index::child`, `Index::get`, `Index::subtree`).
The benchmarks print the report with a `memory_` prefix and
`GATE memory <= 100 B per entry`. `tests/gate_metrics.rs` checks
≤100 B per entry for 100k fx-bench entries in CI, that the total covers
`Snapshot::heap_bytes()` plus the order capacities, and that an
uncompacted delta makes it grow. Full-scale numbers are recorded in
`docs/gates/phase-1.md`.

### startup_bench

```text
cargo run --release -p fx-engine --example startup_bench -- --entries 2000000 --runs 20 --cache warm
```

Generates N synthetic entries (fx-bench's tree generator), saves them as
an engine store with a resume cursor, then `--runs` times opens the store
from the file alone and runs a first query: `engine_open` (`Engine::open`
until the first snapshot is published) and `engine_open_to_first_result`,
timed first in every run, then `reader_open_to_first_result`
(`StoreReader::open`, the CLI path). It prints `startup_entries=<N>`.
Gates: `store open to first result p95 <= 2 s` and its 1 s stretch, plus
the memory gate. App launch and first paint are measured by fx-ui-macos.

| Flag              | Default    | Meaning                                       |
| ----------------- | ---------- | --------------------------------------------- |
| `--entries N`     | `2000000`  | synthetic entries, 1..=50000000               |
| `--runs N`        | `20`       | timed opens per path (1 when cold)            |
| `--store-dir DIR` | temp dir   | where the store `startup-<N>.fxdb` is kept    |
| `--reuse`         | off        | reuse DIR's store for N entries if it exists  |
| `--query TEXT`    | empty      | the first query (empty: every entry by name)  |
| `--cache`         | `unknown`  | `warm`, `cold` or `unknown`; `cold` changes what is timed |

**Which number is cold.** Only a `--cache cold` run gives the cold store
open to first result: write the store with `--store-dir DIR` (warm pass),
reboot, then run `--store-dir DIR --reuse --cache cold`. In that run the
bench times exactly one `Engine::open` to first result before anything
else has read the store file, skips the `StoreReader` pass (it would warm
the page cache) and ignores `--runs` (`runs=1`); its
`engine_open_to_first_result_p95_ms` and the `GATE store open to first
result` lines are the cold number. It refuses `--cache cold` without a
reusable store, because writing the store would cache it. In warm and
unknown runs every number (engine and reader) is a warm number: from the
second open on, the store is in the page cache.

### catchup_bench

```text
cargo run --release -p fx-engine --example catchup_bench -- --changes 10000 --cache warm
```

Indexes a tree of `--entries` files (folders of 100) over a missing store,
publishes a cursor with one live change and closes the engine (saving the
store). With the engine stopped it makes `--changes` scripted changes (per
ten: 3 creates, 2 deletes, 2 renames across folders, 2 size changes, 1 new
folder), reopens from the saved cursor and reports `search_usable_ms` and
`equal_ms` (first snapshot equal to a fresh scan), with
`catchup_changes=<N>` for the changes made; a run that never
reaches equality exits with 1. Gates: `catch-up search usable <= 2 s`,
`catch-up equal to a fresh scan <= 10 s`. `--source fsevents` (macOS, the
default there) uses a real temporary directory with FSEvents, `MacPathStat`
and `MacScanner`; `--source fake` (default elsewhere) uses the test fakes
from `tests/common/mod.rs`. Flags: `--entries N` (default 50000),
`--changes N` (default 10000), `--source`, `--cache`.

### first_run_bench

```text
cargo run --release -p fx-engine --example first_run_bench -- --root / --cache warm
```

macOS only (prints a message and exits 0 elsewhere). Starts the engine over
an empty index with `MacScanner`, `MacPathStat` and FSEvents for `--root`
(default `/`) and reports `entries` (also as `first_run_entries`),
`scan_seconds` (until the scan is
published and the engine is live), `entries_per_second`,
`first_result_ms`, `progressive_snapshots` (non-empty snapshots published
up to the end of the scan) and the memory report. Gates: `first run scan
<= 15 s`, `first run results shown progressively` (at least two non-empty
snapshots) and the memory gate. Read-only: it only lists directories and
writes no store. Flags: `--root DIR`, `--timeout SECS` (default 600),
`--cache`.

Exit codes for all three: 0 after a completed run (gate verdicts do not
change it, except a failed catch-up equality), 1 on an error, 2 for bad
arguments. They write only inside their own temporary directories (or
`--store-dir`) and remove the temporary ones at the end.

## Selection lookup benchmark

```text
cargo run --release -p fx-engine --example positions_bench -- --entries 2700000 --selected 1000 --runs 20 --cache warm
```

`examples/positions_bench.rs` times `Engine::positions`, the call a window
makes after every refresh to keep its selection on the same files (with
"Your files first" on, the window calls `Engine::positions_grouped`, which
walks the whole order when a selected file is outside the group, as the
grouped search's first pass does; the benchmark's worst case already walks
it all). It
indexes `--entries` synthetic entries in an engine without a change
source, runs `--query` once in Name order (the window's worker has just
run that search, so the query is cached), picks `--selected` results
spread from the first row to the last (the worst case: the walk covers the
whole order) and times the lookup of their paths in both directions,
checking every answer against the search. It also reports
`find_path_all` (the path lookups alone) and `positions_one_top` (one file
on the first row). Output follows the format above, with
`positions_entries` and `positions_selected`, and the target line `GATE
selection lookup p95 <= 20 ms` (the slower direction). Flags: `--entries
N` (default 2700000), `--selected N` (default 1000), `--runs N` (default
20), `--query TEXT` (default empty), `--cache`. Exit codes: 0 after a
completed run, 1 on an error or a wrong answer, 2 for bad arguments. It
writes nothing to disk.

## Search re-run benchmark

```text
cargo run --release -p fx-engine --example rerun_bench -- --entries 2000000 --cache warm
```

`examples/rerun_bench.rs` times what a window pays for each new
generation while it shows a search: a full scan of the published
snapshot, since a new snapshot can't be answered from the refinement
cache. fx-query's `matcher_bench` measures that scan over its own view,
which keeps a folded copy of every name and has no delta; this one
measures it over the engine's snapshot. It builds `--entries` synthetic
entries into a compacted base, saves it as a store and serves the base
from the file's memory map, as the app does (`--heap-base` keeps it on
the heap), then applies a delta of `--delta` percent of the live entries
(default 10, the writer's compaction limit): half new files, half
updates of base entries, every other one a rename. Each sample is one
search with a fresh `Searcher` and the default options (case and
diacritics folded) on rayon's global pool (`RAYON_NUM_THREADS` sets its
size), for each of `matcher_bench`'s engine queries. Output follows the
format above: the setup (`base_entries`, `delta_rows`, the share of names
folding changes and what a folded copy of just those would take), then
`rerun_<query>_p50_ms`, `_p95_ms`, `_max_ms` and `_n`, `rerun_worst_p95_ms`,
`rerun_mean_p50_ms`, and the snapshot's `memory_*` report. Flags:
`--entries N` (default 2000000), `--delta PCT` (0 to 10), `--iters K`
(default 30), `--warmup W` (default 3), `--seed S`, `--heap-base`,
`--cache`. Exit codes: 0 after a completed run, 1 on an error or a hit
count that changes between runs, 2 for bad arguments. It writes only its
store, in a temporary directory it removes at the end.
