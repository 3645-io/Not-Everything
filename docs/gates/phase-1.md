# Phase 1 gate record

**Verdict: gate passed on the M4 Pro, 13 Nov 2026.** Every criterion is
inside its gate on the M4 Pro (24 GB, macOS 27.2.1), warm and in the first
run after a reboot, with the code of 0.5.0. Two deviations, both the
owner's decisions of 13 Nov 2026: the gate was closed on the M4 Pro alone,
so the 8 GB Mac was not measured; and showing results progressively during
the first scan moved to Phase 2 (`docs/PLAN.md`), so criterion 6 is the
scan time only. See "Results".

This record tracks the Phase 1 checklist in `docs/PLAN.md` ("Phase 1: macOS
MVP"). Run 1.12 wrote it together with the runner script
`scripts/phase1-gate.sh`, the summary command `fx-bench phase1-summary` and
the CI-scale checks. No agent could run release benchmarks on the M4 Pro or
the 8 GB Mac, drive the AppKit GUI for 20 launches or reboot a machine, so
only the items verified in CI were ticked then. On 13 Nov 2026 an agent ran
the steps at the end on the M4 Pro, warm and, after the owner rebooted it,
cold, and filled in the results tables.

Status values:

- **Met**: measured on both gate machines, warm and cold, inside the gate.
  The owner closed the gate on the M4 Pro alone on 13 Nov 2026, so the
  statuses below are the M4 Pro's, warm and cold.
- **Missed**: measured and outside the gate.
- **Verified in CI**: checked by `cargo test --workspace`; no machine-specific
  number is needed.
- **Pending owner measurement**: the code, benchmark and script step exist,
  but there are no release numbers from the gate machines yet.

## Gate checklist

| # | Criterion (`docs/PLAN.md`) | Command (run for you by `scripts/phase1-gate.sh`) | `summary.md` row(s) | Status |
| --- | --- | --- | --- | --- |
| 1 | Syntax: ≥300 conformance cases pass 100% | `cargo test --release -p fx-query --test conformance_part1 --test conformance_part2`; case count by `cargo test -p fx-testkit --test phase1_gate` | `syntax` | Met: 421 cases, 0 failures, warm and cold; also verified in CI |
| 2 | Goal 1: launch to first result painted ≤2.0 s p95 over 20 launches at 2M entries (stretch ≤1.0 s), also after a reboot | `target/release/fx-ui-macos --root / --gate-probe OUT` × 20 launches warm, × 1 cold (judged by the worst sample); store side: `startup_bench --entries 2000000 --runs 20` | `goal1_launch` | Met: warm p95 478 ms over 20 launches at 2.08M entries, cold 668 ms (stretch met both) |
| 3 | Goal 2: engine p95 ≤16 ms at 2M and ≤30 ms at 5M; keystroke to repaint p95 ≤50 ms; regex and full-path p95 ≤100 ms at 2M | `matcher_bench --entries 2000000` and `matcher_bench --entries 5000000` (fx-query); keystroke from the fx-ui-macos `--gate-probe` | `goal2_engine_2m`, `goal2_engine_5m`, `goal2_keystroke`, `goal2_regex`, `goal2_fullpath` | Met: engine 6.9 and 16.8 ms warm, 7.0 and 17.7 ms cold; keystroke 38.5 and 33.7 ms; regex 69.9 and 45.6 ms; full-path 34.0 and 27.0 ms |
| 4 | Goal 3: create, rename or delete shown in an open result list ≤1 s p99 over 1,000 scripted operations (stretch ≤250 ms p95) | `freshness_bench --ops 1000` (fx-engine) | `goal3_freshness` | Met: p99 187.9 ms warm, 187.6 ms cold, 0 missed (stretch met: p95 187.1 ms) |
| 5 | Offline catch-up: 10,000 changes while quit; index equals a fresh scan within 10 s, search usable within 2 s | `catchup_bench --changes 10000` (fx-engine); small scale in CI by `cargo test -p fx-engine --test gate_metrics` | `catchup_equal`, `catchup_time`, `catchup_usable` | Met: equal to a fresh scan in 489 ms warm and 488 ms cold; search usable in 4.8 and 4.9 ms |
| 6 | First run: ~2M entries in ≤15 s warm (results shown progressively moved to Phase 2 by the owner on 13 Nov 2026) | `first_run_bench --root /` (fx-engine) | `first_run` (progressive: the bench's own `GATE first run results shown progressively` line in `first_run_bench.log`) | Met: 2.90M entries in 13.9 s warm (13.6 s cold); progressive results are Phase 2's |
| 7 | Memory: ≤100 B per entry with default columns | `memory_bytes_per_entry` from `startup_bench` and `first_run_bench`; 100k entries in CI by `cargo test -p fx-engine --test gate_metrics` | `memory` | Met: 88.0 B per entry warm, 87.8 cold, at 2.9M entries |
| 8 | `es`-style CLI with csv, json and txt output and documented exit codes | `cargo test --release -p fx-cli` | `cli` | Met, warm and cold; also verified in CI |

The benchmark binaries are the release builds under `target/release/` and
`target/release/examples/`. Every command above can be re-run by hand to
re-measure a single number; the script records the exact invocation at the
top of each `<name>.log`. The app's measurement options (`--gate-probe`,
`--launch-probe`, `--synthetic`) exist only in a build with the `measure`
feature, as the script makes it: `cargo build --release --locked --workspace
--bins --examples --features fx-ui-macos/measure`.

## Items verified in CI

These pass in `cargo test --workspace` and are ticked on that basis:

- [x] **Syntax, ≥300 conformance cases**: `crates/fx-testkit/tests/phase1_gate.rs`
  counts the case entries in `crates/fx-query/tests/conformance*` (370 at
  run 1.12: 208 in part 1, 162 in part 2) and requires at least 300;
  `conformance_part1` and `conformance_part2` run every case and require 100%
  to pass.
- [x] **CLI formats**: the fx-cli tests (`crates/fx-cli/tests/output.rs`,
  `cli.rs`) check csv, json and txt output.
- [x] **CLI exit codes**: the fx-cli tests (`args.rs`, `cli.rs`) check the
  documented exit code values (`docs/cli.md`) and that each error case
  exits with its code.
- [x] **Memory B/entry estimate at small scale**: `crates/fx-engine/tests/gate_metrics.rs`
  checks ≤100 B/entry with default columns for 100k fx-bench entries (the
  2M figure stays an owner measurement).
- [x] **Catch-up at small scale**: the same test checks that an offline
  catch-up equals a fresh scan and that a reopened store gives a first result.
- [x] **Summary rendering**: `crates/fx-bench/tests/phase1_gate_summary.rs`
  checks percentiles, verdicts and the `summary.md` layout;
  `crates/fx-ui-macos/tests/gate_probe.rs` checks the probe's report.

## Results

The values come from each pass's `summary.md` and the logs next to it.

Run notes, M4 Pro, 13 Nov 2026: Mac16,8 (Apple M4 Pro, 24 GB), macOS
27.2.1, AC power, the code of 0.5.0 (`dev` at
2876754, the same as `rc/0.5.0`'s), `scripts/phase1-gate.sh` at full scale
with `--store-dir`, so the cold pass reused the warm pass's startup store
and the app's index. The app's index of `/` is its own (`--data-dir`, not
the daily app's) and the disk held about 2.9M entries.

- Warm: 22:59, load average 2.3, at 44c1e66, after the warm-up launch
  stopped being counted as a sample. An earlier warm pass at 16:23 (at
  5c82501) also passed every criterion; it counted each launch twice and
  the warm-up launch, whose first scan of the new data folder (8.8 s)
  became its p99.
- Cold: 18:41, two minutes after the owner rebooted the Mac and logged in
  with nothing else started (load average 41.5 from the boot's own work,
  so the cold rows are on the pessimistic side), at 5c82501. Its one
  launch was counted twice, which leaves the worst sample unchanged.

| Measure | M4 Pro warm | M4 Pro cold | 8 GB Mac warm | 8 GB Mac cold |
| --- | --- | --- | --- | --- |
| Conformance cases / failures | 421 / 0 | 421 / 0 | not measured | not measured |
| fx-cli tests (`cli_ok`) | pass | pass | not measured | not measured |

### Goal 1: launch to first result painted

| Measure | M4 Pro warm | M4 Pro cold | 8 GB Mac warm | 8 GB Mac cold |
| --- | --- | --- | --- | --- |
| `launch_ms` (≤2000; stretch ≤1000): warm p95 over 20 launches; cold the single first launch after the reboot (worst sample) | 478 ms (p50 464, p99 486) | 668 ms | not measured | not measured |
| Gate probe index size (`ui_entries`, ≥2M) | 2,076,882 | 2,075,380 | | |
| `startup_bench` engine open to first result p95 (store side) | 164 ms at 2M synthetic (reader 288 ms) | 342 ms (one run) | | |

### Goal 2: search latency

| Measure | M4 Pro warm | M4 Pro cold | 8 GB Mac warm | 8 GB Mac cold |
| --- | --- | --- | --- | --- |
| `matcher_bench` engine p95 at 2M (≤16 ms) | 6.9 ms | 7.0 ms | not measured | not measured |
| `matcher_bench` engine p95 at 5M (≤30 ms) | 16.8 ms | 17.7 ms | | |
| Keystroke to repaint p95 (≤50 ms) | 38.5 ms (p50 28.3) | 33.7 ms | | |
| Regex p95 at 2M (≤100 ms) | 69.9 ms | 45.6 ms | | |
| Full-path p95 at 2M (≤100 ms) | 34.0 ms | 27.0 ms | | |

### Goal 3: freshness

| Measure | M4 Pro warm | M4 Pro cold | 8 GB Mac warm | 8 GB Mac cold |
| --- | --- | --- | --- | --- |
| `freshness_bench` p99 over 1,000 ops (≤1000 ms) | 187.9 ms | 187.6 ms | not measured | not measured |
| p95 (stretch ≤250 ms) and missed operations | 187.1 ms, 0 missed | 187.1 ms, 0 missed | | |

### Offline catch-up, first run and memory

| Measure | M4 Pro warm | M4 Pro cold | 8 GB Mac warm | 8 GB Mac cold |
| --- | --- | --- | --- | --- |
| Catch-up equals a fresh scan (`catchup_equal`) | yes | yes | not measured | not measured |
| Catch-up equal within (≤10000 ms) | 489 ms | 488 ms | | |
| Catch-up search usable within (≤2000 ms) | 4.8 ms | 4.9 ms | | |
| First-run scan of `/` (≤15 s warm), entries | 13.9 s, 2,895,061 entries | 13.6 s, 2,891,171 entries | | |
| Memory B/entry (≤100) | 88.0 | 87.8 | | |

### Earlier passes

- M4 Pro warm, 4 Nov 2026, a v0.1 build from the earlier private
  repository, macOS 27.0.1, without Full Disk Access (1.96M entries in the
  app's index, 2.32M scanned), on a busy machine (load average 12 to 39):
  launch p95 1,277 ms; engine 16.74 ms at 2M (7.5 and 9.3 ms re-run alone)
  and 25.3 ms at 5M; keystroke 57.4 ms; regex 72.8 ms; full-path 187 ms
  (missed then); freshness p99 131 ms; catch-up 456 ms, usable in 14 ms;
  first run 14.6 s, not progressive; 86.7 B per entry; 370 conformance
  cases.
- M4 Pro warm, 8 Nov 2026, v0.3.0, measured outside the script: full-path
  24.1 ms; keystroke to repaint 28.5 ms (35.2 ms over a real 1.92M-entry
  index).

## How the script maps benchmark output to the summary

`fx-bench phase1-summary` reads `metric=value` lines from every file in the
results directory (`crates/fx-bench/src/gate1.rs`), but not from its
subfolders. The app's launches write their output to logs in `gate_probe/`
beside their reports, so each launch counts once and the warm-up launch not
at all. The benchmarks print their own names, so `scripts/phase1-gate.sh`
writes `gate-metrics.txt`:

| Summary metric | Taken from |
| --- | --- |
| `engine_2m_ms`, `regex_2m_ms`, `fullpath_2m_ms` | worst p95 in the `GATE engine/regex/full-path` lines of `matcher_bench_2m.log` |
| `engine_5m_ms` | worst p95 in the `GATE engine` line of `matcher_bench_5m.log` |
| `launch_ms`, `keystroke_ms` | one value per launch from `gate_probe/launch-<n>.txt` (first result painted; keystroke p95) |
| `freshness_ms` | the overall p99 of `freshness_bench.log`, only if no operation was missed (the stretch row is then judged on the p99, which is stricter; check the p95 in the log) |
| `catchup_equal`, `catchup_ms`, `catchup_usable_ms` | `equal_to_fresh_scan`, `equal_ms`, `search_usable_ms` in `catchup_bench.log` |
| `first_run_ms` | `scan_seconds` × 1000 in `first_run_bench.log` |
| `bytes_per_entry` | `memory_bytes_per_entry` in `startup_bench.log` and `first_run_bench.log` (worst) |
| `syntax_cases`, `syntax_failures` | `case(` entries in `crates/fx-query/tests/conformance*`; failures from the conformance test run |
| `cli_ok` | exit code of `cargo test --release -p fx-cli` |
| `engine_2m_entries`, `regex_2m_entries`, `fullpath_2m_entries`, `engine_5m_entries` | the `--entries` counts the script passed to `matcher_bench` |
| `ui_entries` | the index size in the header of each `gate_probe/launch-<n>.txt` |
| `startup_entries`, `catchup_changes`, `first_run_entries`, `freshness_ops` | printed by the fx-engine benches into their own logs |

### Cold passes and gate scale

On a cold pass the script makes one gate-probe launch and one
`startup_bench` run (`LAUNCHES=1`, `STARTUP_RUNS=1`), because only the
first launch after a reboot is cold, and `phase1-summary --cache cold`
judges `goal1_launch` by the worst `launch_ms` sample instead of the p95,
so a slow cold launch can never be dropped into a PASS.

Each timed row is judged only at the gate's scale: Goal 1 at ≥2M entries
(`ui_entries`) and, warm, ≥20 launches; the Goal 2 matcher rows at ≥2M or
≥5M entries; Goal 3 at ≥1,000 operations; catch-up at ≥10,000 changes;
first run at ~2M entries (≥1,800,000). A row whose scale is missing or
smaller is `NOT MEASURED` with a "below gate scale (N)" note, never PASS
or FAIL, and the detail column of every scaled row shows the measured
scale. Syntax, CLI, keystroke and the memory B/entry estimate have no
scale requirement. A `--quick` run therefore leaves every scaled row not
measured.

`checks.log` records every step's exit code and every skip, and is
appended to `summary.md` under "Run checks".

## Owner steps

Run benchmarks one at a time on an otherwise idle machine, plugged in.
For each of the two gate machines (M4 Pro 24 GB, 8 GB Apple Silicon Mac):

1. Warm pass (builds the release binaries, writes the startup store and,
   through the warm-up launch, the app's index for `/` in
   `<store-dir>/app-data`, never the daily app's data folder):

   ```sh
   scripts/phase1-gate.sh --machine "M4 Pro" --cache warm \
       --store-dir ~/fx-gate-store --out ~/fx-gate/m4-warm
   ```

2. Reboot, log in, start nothing else, then the cold pass, reusing the
   store written before the reboot:

   ```sh
   scripts/phase1-gate.sh --machine "M4 Pro" --cache cold \
       --store-dir ~/fx-gate-store --reuse --out ~/fx-gate/m4-cold
   ```

   `--store-dir` keeps the 2M-entry `startup_bench` store and the app's
   index for the gate probe; `--reuse` times that store as is. A cold pass without a kept store skips `startup_bench`
   rather than time a freshly written (page-cached) store as cold. The cold
   pass runs the gate probe launch, `startup_bench` and `first_run_bench`
   first and uses one launch and one startup run, judged as is (worst
   sample, not p95).
3. Paste the values from each `summary.md` into the tables above, attach or
   link the results directories, and set each row's status. Tick an item
   only when it is met on both machines, warm and cold.
4. Update the verdict at the top: "gate passed" only when every item is met.

`scripts/phase1-gate.sh --machine ci --cache warm --quick --out DIR` runs
the same steps at small entry counts (a smoke run; its numbers are not gate
evidence, and `summary.md` marks every scaled row NOT MEASURED, "below gate
scale"). `cargo test -p fx-testkit --test phase1_gate_smoke -- --ignored`
runs it with `--skip-ui` in its own target directory and checks that
`summary.md` and `gate-metrics.txt` are written; at run 1.12 it produced
every row except the two gate-probe rows (`goal1_launch`,
`goal2_keystroke`: not measured with `--skip-ui`). `--skip-ui` skips the gate probe on a machine without a window
server. The script never deletes, moves or purges anything and never flushes
caches; "cold" always means the first run after a reboot.
