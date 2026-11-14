# fx-bench

Synthetic file names and file trees, an on-disk tree materializer, and a
small benchmark harness that reports nearest-rank p50 and p95, and a
`gate-summary` command that collects the benchmarks' `GATE` lines into one
results table, and a `phase1-summary` command that evaluates the Phase 1
gate from benchmark results and writes `summary.md`. For the performance
gate it adds `sample` (a process's CPU, energy, wakeups, disk writes and
memory over time), `churn` (a steady stream of file changes to measure
against) and `perf-compare` (metrics against a baseline file). The crate
uses only the Rust standard library; on macOS, `sample` also calls
libSystem through its own `extern "C"` declarations.

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
with code 1 (except in `gate-summary`, `phase1-summary` and `perf-compare`;
see [Gate summary](#gate-summary), [Phase 1 summary](#phase-1-summary) and
[Performance comparison](#performance-comparison)).

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

## Release notes: perf-release

`perf-release` writes the Performance section of a release's GitHub notes:
this release's metrics against the previous release's and the targets of
the baseline file.

```sh
cargo run --release -p fx-bench -- perf-release \
    --baseline docs/gates/perf-baseline.toml \
    --previous docs/gates/releases/0.3.2.txt --previous-label 0.3.2 \
    --label 0.4.0-RC-1 [--notes perf/notes.md] [--out performance.md] \
    perf/metrics.txt
```

The section, headed `### Performance` like the other sections of the notes, is a summary table of the IDs in the baseline's `[meta]`
`summary` (by name, without IDs), the `--notes` file's lines, and then every
metric of the baseline in a `<details>` block, grouped by the first letter
of its ID. GitHub strips text colour from release notes, so each row has a
dot beside its words: Change is 🟢 better, ⚪ the same (within 1%), 🟡
worse but within the metric's allowance, or 🔴 worse than the allowance,
measured from the previous release's value; Target met is 🟢 met, 🟡 not
yet, or 🔴 missed though the previous release met it, with "(tracked)" for
an ungated metric. A metric without a value reads "not measured". It exits
0, or 2 for bad arguments, an unreadable or malformed file, a baseline
without a `summary`, or an existing `--out` file.

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

## Sample

`sample` (macOS only) reads a running process's resource counters
(`proc_pid_rusage` with `RUSAGE_INFO_V6`) at a fixed interval, so idle CPU,
energy, wakeups, disk writes and memory can be gated like search speed. It
needs no root for a process of the same user.

```sh
cargo run --release -p fx-bench -- sample --pid 12345 --duration 1800 --out /tmp/idle-run
cargo run --release -p fx-bench -- sample --pid 12345 --duration 60 --interval 1 \
    --label churn --burst-pct 100 --out /tmp/churn-run
```

`--pid`, `--duration` (seconds) and `--out` are required; `--interval`
defaults to 10 s, `--label` to `app` (`[a-z0-9_]`) and `--burst-pct` to
150 (% of one core). `--out` must not exist; `sample` creates it and writes:

- `rusage.csv`: one row per reading, with its wall-clock time, the seconds
  since the first reading and the cumulative counters (CPU times, energy,
  wakeups, disk bytes, footprints, instructions, CPU time per QoS class).
- `intervals.csv`: per interval, its start and real length, CPU as % of
  one core, CPU energy in mW, wakeups per second (package idle plus
  interrupt), disk bytes read and written, and the footprint at its end.
- `metrics.txt`: the lines it also prints to stdout.
- `burst-<n>.txt`: when an interval's CPU passes `--burst-pct`,
  `/usr/bin/sample` records the process's stacks for 3 s, once per burst
  and at most 3 times per run, so a burst's cause can be traced.

The kernel's `ri_*_time` counters are in mach absolute time units, which
`sample` converts with `mach_timebase_info` (125/3 on Apple silicon, not
1). Every rate divides by the real time between two readings, because a
burst capture blocks the loop for several seconds. The run stops at
`--duration` or when the process exits; a process that has exited but not
yet been reaped still gives its final counters, dated at its exit.

The printed `key=value` lines, each key prefixed with the label:

| Key | Meaning |
|---|---|
| `<label>_cpu_pct` | average CPU over the run, % of one core |
| `<label>_cpu_max_interval_pct` | the busiest interval |
| `<label>_energy_mw` | average CPU energy, mW (`ri_energy_nj`) |
| `<label>_wakeups_per_s` | package-idle plus interrupt wakeups per second |
| `<label>_disk_written_bytes` | bytes written during the run |
| `<label>_disk_written_per_day_bytes` | the same, scaled to 86,400 s |
| `<label>_footprint_bytes` | physical footprint at the last reading |
| `<label>_footprint_max_bytes` | the largest footprint of any reading, or `ri_interval_max_phys_footprint` if larger |
| `<label>_duration_s` | from the first reading to the last |
| `<label>_samples` | number of readings |
| `<label>_process_exited=true` | only when the process exited early |

With a single reading (the process exited at once) the rates need two
readings, so their lines are left out and `perf-compare` reports them as
not measured. Progress notes go to stderr.

Exit codes: 0 after a run (also when the process exited early); 1 when the
process can't be read (no such process, another user's) or a file can't be
written, in which case a missing process leaves no folder behind; 2 for bad
arguments, an existing `--out` or a missing parent folder, and on any OS
other than macOS.

## Churn

`churn` keeps files changing at a steady rate, so the app's CPU and energy
can be measured while it follows the file system.

```sh
cargo run --release -p fx-bench -- churn --dir /tmp/fx-churn --duration 60 --rate 20 --seed 1
```

`--dir` and `--duration` (seconds) are required; `--rate` (operations per
second) defaults to 20, `--seed` to 1 and `--max-files` to 5000. The
parent of `--dir` must exist and `--dir` must not: `churn` creates it,
makes 16 folders in it (nested up to three levels), then runs a
deterministic stream of operations from the seed, with names from the
generator: about half create a file (empty or up to 64 bytes), a quarter
rename one (within its folder or into another) and a quarter delete one.
At `--max-files` a create becomes a delete; with no files a rename or
delete becomes a create. Every name starts with a counter (`17-report.txt`),
so names never collide and are never hidden.

Operations are paced on a fixed tick by sleeping. If the disk falls
behind, the next operation runs as soon as the last one finishes and the
missed ticks are dropped rather than made up in a burst. It only creates,
renames and deletes what it made inside `--dir`, never follows symlinks,
and leaves `--dir` in place at the end. It prints:

```text
churn_ops=1200
churn_creates=603
churn_renames=298
churn_deletes=299
churn_seconds=60.001
churn_rate=19.999
```

The creates, renames and deletes add up to the operations; `saves` projects
the index's saves from the creates and deletes.

Exit codes: 0 after a run; 1 for an I/O error; 2 for bad arguments, an
existing `--dir` or a missing parent folder.

From Rust, use the `churn` module: `plan(seed, n, max_files)` gives the
first `n` operations without touching the disk (a shorter plan is a prefix
of a longer one), `folders(seed)` the 16 folders, `Planner` the stream one
operation at a time, and `run` applies it.

## Saves

`saves` projects the index saves a background phase of the performance
gate is too short to see, for B7 ("Disk writes a day",
`docs/gates/performance.md`).

```sh
cargo run --release -p fx-bench -- saves --label hidden --entries 2000000 \
    --save-bytes 157286400 --creates 6000 --deletes 3000 --duration 600 \
    --measured-per-day 0
```

The engine's writer compacts the index when the rows added since the last
compaction (one per file created), or the entries removed since then (one
per file deleted), pass 10% of the live entries, at least 1,024; a file
renamed after it was created changes its row in place. Each compaction then
rewrites the whole store file. A 10-minute phase at 20 operations a second
adds about 6,000 rows where a whole-disk index of 2M entries compacts every
200,000, so its sampled writes never include a save. From `--entries` (the
index's live entries), `--save-bytes` (the store file's size) and the
phase's `--creates` and `--deletes` over `--duration` seconds, `saves`
prints, each key prefixed with the label:

| Key | Meaning |
|---|---|
| `<label>_entries` | the index's live entries |
| `<label>_rows_per_compaction` | the rows of one kind that bring a compaction: 10% of the entries (at least 1,024), plus one |
| `<label>_compactions_per_day` | the larger of creates and deletes a second, times 86,400, over the rows per compaction |
| `<label>_save_bytes` | what one save writes: the store file's size |
| `<label>_compaction_saves_per_day_bytes` | compactions a day times one save |
| `<label>_disk_written_with_saves_per_day_bytes` | with `--measured-per-day` (the phase's measured writes scaled to a day, `sample`'s `<label>_disk_written_per_day_bytes`): their sum, which B7 reads |

The example prints 4.320 compactions a day and 679,477,248 bytes (648 MB).
Two small effects pull the model either way: a file created and deleted
within one of the writer's batches never gets a row, though the model
counts it; and the first change after a compaction to a file already in
the base adds a row the model leaves out (with `churn`, at most its 5,000
files per compaction, 2.5% of a whole-disk index's 200,000). fx-engine's
`tests/compaction_saves.rs` drives the real writer with `churn`'s stream
and checks the model against it; from Rust, use the `saves` module.

Exit codes: 0, or 2 for bad arguments.

## Performance comparison

`perf-compare` judges the performance gate: measured metrics against a
baseline file, one verdict per metric of a tier.

```sh
cargo run --release -p fx-bench -- perf-compare --baseline docs/gates/perf-baseline.toml \
    --tier pr results/*.txt
cargo run --release -p fx-bench -- perf-compare --baseline docs/gates/perf-baseline.toml \
    --tier quick --format json --out /tmp/perf.json results/*.txt
```

`--baseline` and `--tier` (`quick`, `pr` or `rc`) and at least one metrics
file are required. `--format` is `md` (the default) or `json`; `--out FILE`
also writes the report to a new file (it refuses to overwrite);
`--print-values` adds a line `<ID> <key> = <value>` for each measured
metric after the markdown report, for writing a new baseline at a release;
`--partial` shows a metric whose key is absent from every metrics file as
`SKIPPED` instead of `NOT MEASURED`, for running one benchmark alone.

The baseline file is a strict subset of TOML, parsed by hand: `#`
comments, blank lines, `[ID]` table headers (ASCII letters, digits, `_` and
`-`, such as `S1` or `F6u`) and `key = value` lines whose value is a number
(`-?digits[.digits]`), `true`, `false` or a double-quoted string (only
`\"` and `\\` escapes). `[meta]` holds the `machine`, `cache`, `build` and
`date` strings shown in the report header. Every other table is a metric:

```toml
[S1]
name = "Engine search p95 at 2M entries"
key = "engine_2m_ms"            # metric key in the metrics files
unit = "ms"                     # shown after values; bool shows true/false
better = "lower"                # lower | higher | equal
target = 16                     # ceiling, floor or exact value
allow_pct = 10                  # allowed worsening against the baseline, %
allow_floor = 1                 # the allowance is at least this
aggregate = "median"            # median | p95 | max | min | mean | last
scale_key = "engine_2m_entries" # metric key holding the scale it ran at
scale_min = 2000000             # below this, NOT MEASURED at pr and rc
tiers = "quick pr rc"
baseline = 9.3                  # pr and rc tiers
quick_baseline = 1.1            # quick tier
gated = true                    # false: TRACKED, never fails
baseline_build = "0.3.1"
baseline_date = "2026-11-09"
exception = "Raised in #57 after the store rewrite"
```

`name`, `key`, `unit`, `better`, `aggregate` and `tiers` are required;
`gated` defaults to true; `scale_min` needs `scale_key`; an `exception`
must name the PR that accepted it as `#<number>`. Unknown or repeated keys,
repeated tables and values of the wrong type are errors naming the file and
line.

Metrics files hold `key=value` lines as the gate scripts write them; a key
may repeat, one line per sample, and `true`/`false` read as 1/0. Blank and
`#` lines are ignored; any other line is an error naming the file and line.
`median` and `p95` are nearest-rank, as in the harness (with an even count,
`median` is the lower middle sample); `last` is the last sample read, files
in argument order.

For each metric whose `tiers` include `--tier`:

1. No sample of its key: `NOT MEASURED` (`SKIPPED` with `--partial`).
2. At the pr and rc tiers, a `scale_key` value below `scale_min` (the
   smallest one counts), or none while `scale_min` is set: `NOT MEASURED`.
3. The value is the `aggregate` of every sample of the key. With `per_key`,
   it is divided by the median of that key's samples over `per_scale`
   (1000000 for per million files, 1 for a multiple of another metric);
   without samples of `per_key`, or with it at 0, `NOT MEASURED`.
4. At the pr and rc tiers a metric with a target is judged on the target
   alone: `lower` passes at or below it, `higher` at or above it, `equal`
   only at it. A miss is `FAIL (target)`, unless the metric has a
   `known_gap` (the work that fixes it): then `KNOWN GAP (<work>)`, which
   doesn't fail. A pass at 80% of the target or more reads `PASS (near
   limit: N% of target)` (not for `bool` or `equal` metrics), and a pass
   that meets a known gap's target reads `PASS (known gap met at this
   tier)`. `m1_target` is shown beside the target in the report. The quick
   tier runs at a small scale, so it skips `lower` and `higher` targets
   (shown in parentheses) but still checks `equal` targets, which state
   correctness rather than speed.
5. The regression check applies at the quick tier, against
   `quick_baseline`, and to a metric without a target, against `baseline`
   (`pr_baseline` at the pr tier when set), allowing max(baseline ×
   `allow_pct` / 100, `allow_floor`), or nothing when neither is set:
   `lower` fails above baseline + allowance, `higher` below baseline −
   allowance; `equal` has no regression check. Without a baseline for the
   tier the verdict is `NO BASELINE`, which is not a failure: the first run
   sets the baseline.

Verdicts: `PASS`, `PASS (near limit: N% of target)` or `PASS (known gap met
at this tier)`; `FAIL (target)` or `FAIL (regression)`; `KNOWN GAP
(<work>)`; `NOT MEASURED (<reason>)`; `NO BASELINE`; `TRACKED (would be
<verdict>)` for an ungated metric; `SKIPPED`.

The markdown report has a header (tier and `[meta]`), the table
`| ID | Metric | Baseline | Now | Change | Target | Verdict |` (values with
units, Change as a signed % against the tier's baseline), any exceptions,
and the line `perf-compare: <p> pass, <f> fail, <n> not measured, <b> no
baseline, <t> tracked, <s> skipped, <k> known gaps` (the known gaps last,
so readers of the earlier fields keep working). The Target cell adds the
M1 target in brackets when there is one. The JSON report is
`{"tier","meta":{"machine","cache","build","date"},"metrics":[{"id","name",
"key","unit","better","gated","baseline","now","change_pct","target",
"target_checked","m1_target","known_gap","near_limit_pct","verdict",
"failed","reason","would_be","exception"}...],
"counts":{"pass","fail","not_measured","no_baseline","tracked","skipped",
"known_gaps"},"result":"PASS"|"FAIL"}`.

Exit codes: 0 when no gated metric is `FAIL` or `NOT MEASURED`; 1
otherwise; 2 for bad arguments (usage on stderr), an unreadable or
malformed baseline or metrics file, an existing `--out` file or a failed
write. It only reads its inputs.

From Rust, use the `perf` module: `parse_baseline`, `parse_metrics`,
`compare`, `gate_passes`, `Counts`, `render_markdown`, `render_json` and
`value_lines`.
