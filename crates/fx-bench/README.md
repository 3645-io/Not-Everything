# fx-bench

Synthetic file names and file trees, an on-disk tree materializer, and a
small benchmark harness that reports nearest-rank p50 and p95, and a
`gate-summary` command that collects the benchmarks' `GATE` lines into one
results table, and a `phase1-summary` command that evaluates the Phase 1
gate from benchmark results and writes `summary.md`. The crate uses only
the Rust standard library.

## Usage

Always build in release mode for numbers you intend to record.

```sh
# 5M names from seed 1: count, total bytes, mean length, non-ASCII count,
# FNV-1a checksum, `generation time` (the generator call only, the same
# label `materialize` uses) and `checksum time` (the FNV-1a pass on its own).
cargo run --release -p fx-bench -- gen --count 5000000 --seed 1

# A tree of 1M entries (about 19% directories), with its directory count.
cargo run --release -p fx-bench -- gen --count 1000000 --seed 1 --tree

# Write a 100k-entry tree as directories and empty files. DIR must already
# exist and be empty; the command only creates entries, never deletes.
mkdir /tmp/fx-tree
cargo run --release -p fx-bench -- materialize --count 100000 --seed 1 --out /tmp/fx-tree

# Time name generation and a naive str::contains scan (p50/p95 per bench).
cargo run --release -p fx-bench -- self-bench --count 1000000 --iterations 10 --cache warm
cargo run --release -p fx-bench -- self-bench --json --cache warm
```

`self-bench` defaults to `--count 1000000 --iterations 10 --cache unknown`.
It runs two benchmarks, each with one untimed warmup call and then
`--iterations` timed calls:

- `generate_names`: one sample is a full `generate_names(count, 1)` call,
  that is, generating every name and appending it to a fresh `Names`
  buffer. Freeing the returned buffer is not timed (see below).
- `str::contains`: the names from seed 1 are generated once, untimed. One
  sample is one pass over all `count` names that counts the names
  containing `"re"`: for each name, `Names::get` slices it out of the
  buffer by its offsets (no allocation, no UTF-8 re-validation) and
  `str::contains` searches it. This is a naive, single-threaded baseline
  to compare the index's search against, not a tuned search.

The harness stops the timer as soon as the closure returns; the closure's
result is then passed through `std::hint::black_box` and dropped outside
the timed region, so deallocation is never part of a sample.
Bad arguments print usage to stderr and exit with code 2; I/O errors exit
with code 1 (except in `gate-summary` and `phase1-summary`; see
[Gate summary](#gate-summary) and [Phase 1 summary](#phase-1-summary)).

From Rust, use `generate_names`, `generate_tree`, `materialize` and `run`
(see the crate docs).

## Seed stability

The generator uses a hand-written xoshiro256** seeded with SplitMix64 and
integer arithmetic only, so a given `(count, seed)` (and tree shape)
produces the same bytes on every OS, architecture and toolchain. The
`checksum` line of `gen` is the FNV-1a 64 hash of the NUL-separated name
buffer (for `--tree`, continued over the parent indices and directory
flags), so two machines can compare outputs by checksum.

Tests pin golden values in `tests/generator.rs`: the FNV-1a checksum of
`generate_names(1000, 20240601)`, and for `generate_tree(1000, 20240601,
TreeShape::default())` the structure checksum above, the directory count
and the paths of a few fixed entries. `tests/cli.rs` checks that
`gen --tree` prints the same tree checksum. Changing the output for an
existing seed is a breaking change; add new behaviour behind new
parameters instead.

## Recording numbers

Each harness report includes the OS, architecture, thread count and CPU
brand, plus the cache state you pass with `--cache`. When you record
numbers (for example in `docs/gates/`):

- Run benchmarks one at a time on an otherwise idle machine, from a release
  build.
- Note the machine: model, CPU, RAM, OS version and storage.
- Pass `--cache warm` after a previous run has touched the same data, or
  `--cache cold` after a reboot or after dropping caches (`sudo purge` on
  macOS). Use `unknown` if you cannot tell.
- Record the exact command, including `--count`, `--seed` and
  `--iterations`, alongside p50 and p95.

## Gate summary

`gate-summary` turns the logs of the Phase 0 gate benchmarks into one
results table. It only reads the log files; it never writes, deletes or
moves anything.

```sh
cargo run --release -p fx-bench -- gate-summary --machine m4pro --cache warm \
    logs/search_bench.log logs/load_bench.log logs/scan_bench.log
cargo run --release -p fx-bench -- gate-summary --machine m4pro --cache cold \
    --format json logs/*.log
```

`--machine` (any non-empty label) and `--cache` (`warm`, `cold` or
`unknown`) are required and label every row; `--format` is `md` (the
default) or `json`. At least one log file is required.

It recognises these gate lines (each line is trimmed first):

| Printed by | Line |
|---|---|
| fx-fs-macos `fsevents_bench`, fx-store `load_bench` | `GATE <criterion>: PASS\|FAIL[ (<detail>)]` |
| fx-ui-macos `frame_stats` (table_spike) | `GATE: PASS\|FAIL p95=<x> budget=<y>ms` (criterion: the text after the verdict, or `frame p95`) |
| fx-query `search_bench` | `gate <criterion>: PASS\|FAIL (<detail>)` |
| fx-fs-macos `scan_bench` | `PASS\|FAIL\|SKIP: <criterion> (<detail>)` |

Each gate line gives one row with the log's file name, criterion, verdict,
detail, machine and cache. A line that starts with the word `GATE` but has
no verdict gives an `UNPARSED` row, and a log with no gate line at all gives
a `MISSING` row, so a crashed benchmark is never silently left out.
Lowercase `gate` lines count only when they carry a verdict.

The markdown output is the table followed by a `Result:` line; the JSON
output is `{"result":"PASS"|"FAIL","rows":[{"source","criterion","verdict",
"detail","machine","cache"}...]}` with `detail` null when absent.

Exit codes: 0 when every row is `PASS`; 1 when any row is `FAIL`, `SKIP`,
`UNPARSED` or `MISSING`; 2 for bad arguments (usage on stderr) or an
unreadable log file.

From Rust, use `parse_gate_lines` and the `gate` module.

## Phase 1 summary

`phase1-summary` evaluates the Phase 1 gate (see `docs/PLAN.md` and
`docs/gates/phase-1.md`) from a directory of benchmark results and writes
`summary.md` into that directory. It reads every regular file in the
directory except `summary.md`, so re-running it gives the same summary.

```sh
cargo run --release -p fx-bench -- phase1-summary --machine m4pro --cache warm results/
```

`--machine` (any non-empty label) and `--cache` (`warm`, `cold` or
`unknown`) are required and are recorded in the summary as
`Machine: <label>` and `Cache: <state>`. Exactly one directory is required.

Benchmarks report one sample per line, in either form (lines are trimmed;
everything else, including Phase 0 `GATE` lines, is ignored; repeated keys
accumulate):

```text
launch_ms=812.5
{"metric":"launch_ms","value":812.5,"cache":"warm"}
catchup_equal=true
```

Keys are `[a-z0-9_]`; values are finite numbers or `true`/`false`. Timing
metrics are in milliseconds.

| Criterion | Metric(s) | Rule |
|---|---|---|
| `syntax` | `syntax_cases`, `syntax_failures` | max cases >= 300 and failures 0 (absent = 0) |
| `goal1_launch` | `launch_ms` | p95 <= 2000 (stretch p95 <= 1000) |
| `goal2_engine_2m` | `engine_2m_ms` | p95 <= 16 |
| `goal2_engine_5m` | `engine_5m_ms` | p95 <= 30 |
| `goal2_keystroke` | `keystroke_ms` | p95 <= 50 |
| `goal2_regex` | `regex_2m_ms` | p95 <= 100 |
| `goal2_fullpath` | `fullpath_2m_ms` | p95 <= 100 |
| `goal3_freshness` | `freshness_ms` | p99 <= 1000 (stretch p95 <= 250) |
| `catchup_equal` | `catchup_equal` | every value true |
| `catchup_time` | `catchup_ms` | worst <= 10000 |
| `catchup_usable` | `catchup_usable_ms` | worst <= 2000 |
| `first_run` | `first_run_ms` | worst <= 15000 |
| `memory` | `bytes_per_entry` | worst <= 100 |
| `cli` | `cli_ok` | every value true |

Percentiles are nearest-rank (as in the harness) over the exact sample
values, and thresholds are inclusive. Each criterion is `PASS`, `FAIL` or
`NOT MEASURED` (no value for its metric). Stretch goals are shown in the
detail column but do not change the verdict.

The gate verdict adds two checks to these rules:

- **Cold launches.** With `--cache cold`, `goal1_launch` is judged by the
  worst `launch_ms` sample (max), not the p95: only the first launch after
  a reboot is cold, and a percentile could drop a slow one into a PASS.
- **Gate scale.** Each row with a scale reads the scale its benchmark ran
  at. If that value is missing or below the gate scale, the row is
  `NOT MEASURED` with a `below gate scale (N ...)` note (never `PASS` or
  `FAIL`); otherwise the detail names the measured scale. With several
  values the smallest counts.

| Criterion | Scale metric | Gate scale |
|---|---|---|
| `goal1_launch` | `ui_entries` (the gate probe's index size) | >= 2,000,000 |
| `goal1_launch`, warm or unknown cache | number of `launch_ms` samples | >= 20 launches |
| `goal2_engine_2m` | `engine_2m_entries` | >= 2,000,000 |
| `goal2_engine_5m` | `engine_5m_entries` | >= 5,000,000 |
| `goal2_regex` | `regex_2m_entries` | >= 2,000,000 |
| `goal2_fullpath` | `fullpath_2m_entries` | >= 2,000,000 |
| `goal3_freshness` | `freshness_ops` | >= 1,000 operations |
| `catchup_equal`, `catchup_time`, `catchup_usable` | `catchup_changes` | >= 10,000 changes |
| `first_run` | `first_run_entries` | ~2M: >= 1,800,000 |

`syntax`, `memory` (the B/entry estimate), `cli` and `goal2_keystroke` have
no scale requirement, so a `--quick` run still judges them.
`scripts/phase1-gate.sh` writes the matcher entry counts and `ui_entries`
into `gate-metrics.txt`; the fx-engine benches print `startup_entries`,
`catchup_changes`, `first_run_entries` and `freshness_ops` in their logs.

Exit codes: 0 when every criterion passes; 1 when `summary.md` was written
but a criterion failed or was not measured; 2 for bad arguments (usage on
stderr), an unreadable directory or a failed write. Apart from
`summary.md` it only reads files.

From Rust, use the `gate1` module: `parse_results`, `read_results_dir`,
`evaluate` (the rules alone), `evaluate_gate` (the verdict
`phase1-summary` reports, with the cold and scale checks), `gate_scale`,
`all_pass` and `render_summary`.
