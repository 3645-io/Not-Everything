# Phase 1 gate record

**Verdict: pending owner measurement.** (Gate not passed yet. The M4 Pro
warm pass was measured at the v0.1.0 release test on 2026-11-04, and
full-path search and keystroke to repaint again with v0.3.0 on 2026-11-08,
when both came inside their budgets. The cold pass, the 8 GB Mac and
progressive first-run results are still open. See "Results".)

This record tracks the Phase 1 checklist in `docs/PLAN.md` ("Phase 1: macOS
MVP"). Run 1.12 wrote it together with the runner script
`scripts/phase1-gate.sh`, the summary command `fx-bench phase1-summary` and
the CI-scale checks. No agent could run release benchmarks on the M4 Pro or
the 8 GB Mac, drive the AppKit GUI for 20 launches or reboot a machine, so
only the items verified in CI are ticked. The owner fills in the results
tables with the steps at the end, warm and cold (after a reboot) on both
machines, and never ticks an unmeasured item.

Status values:

- **Met**: measured on both gate machines, warm and cold, inside the gate.
- **Missed**: measured and outside the gate.
- **Verified in CI**: checked by `cargo test --workspace`; no machine-specific
  number is needed.
- **Pending owner measurement**: the code, benchmark and script step exist,
  but there are no release numbers from the gate machines yet.

## Gate checklist

| # | Criterion (`docs/PLAN.md`) | Command (run for you by `scripts/phase1-gate.sh`) | `summary.md` row(s) | Status |
| --- | --- | --- | --- | --- |
| 1 | Syntax: ≥300 conformance cases pass 100% | `cargo test --release -p fx-query --test conformance_part1 --test conformance_part2`; case count by `cargo test -p fx-testkit --test phase1_gate` | `syntax` | Verified in CI |
| 2 | Goal 1: launch to first result painted ≤2.0 s p95 over 20 launches at 2M entries (stretch ≤1.0 s), also after a reboot | `target/release/fx-ui-macos --root / --gate-probe OUT` × 20 launches warm, × 1 cold (judged by the worst sample); store side: `startup_bench --entries 2000000 --runs 20` | `goal1_launch` | M4 Pro warm inside (1.28 s p95 at 1.96M entries); cold and 8 GB Mac pending |
| 3 | Goal 2: engine p95 ≤16 ms at 2M and ≤30 ms at 5M; keystroke to repaint p95 ≤50 ms; regex and full-path p95 ≤100 ms at 2M | `matcher_bench --entries 2000000` and `matcher_bench --entries 5000000` (fx-query); keystroke from the fx-ui-macos `--gate-probe` | `goal2_engine_2m`, `goal2_engine_5m`, `goal2_keystroke`, `goal2_regex`, `goal2_fullpath` | M4 Pro warm inside with v0.3.0: full-path p95 24.1 ms (187 ms at v0.1.0) and keystroke to repaint p95 28.5 ms (57 ms at v0.1.0); engine and regex inside; cold and 8 GB Mac pending |
| 4 | Goal 3: create, rename or delete shown in an open result list ≤1 s p99 over 1,000 scripted operations (stretch ≤250 ms p95) | `freshness_bench --ops 1000` (fx-engine) | `goal3_freshness` | M4 Pro warm inside (p99 131 ms); cold and 8 GB Mac pending |
| 5 | Offline catch-up: 10,000 changes while quit; index equals a fresh scan within 10 s, search usable within 2 s | `catchup_bench --changes 10000` (fx-engine); small scale in CI by `cargo test -p fx-engine --test gate_metrics` | `catchup_equal`, `catchup_time`, `catchup_usable` | M4 Pro warm inside; cold and 8 GB Mac pending (equality at small scale verified in CI) |
| 6 | First run: ~2M entries in ≤15 s warm, results shown progressively | `first_run_bench --root /` (fx-engine) | `first_run` (progressive: the bench's own `GATE first run results shown progressively` line in `first_run_bench.log`) | Missed on M4 Pro warm: 2.32M entries in 14.6 s is inside, but results are not shown progressively |
| 7 | Memory: ≤100 B per entry with default columns | `memory_bytes_per_entry` from `startup_bench` and `first_run_bench`; 100k entries in CI by `cargo test -p fx-engine --test gate_metrics` | `memory` | M4 Pro warm inside (86.7 B/entry at 2.32M); 8 GB Mac pending (≤100 B/entry at 100k verified in CI) |
| 8 | `es`-style CLI with csv, json and txt output and documented exit codes | `cargo test --release -p fx-cli` | `cli` | Verified in CI |

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

One table per criterion group; the owner pastes values from each run's
`summary.md` (and the logs next to it). Leave a cell empty until it is
measured. Record the machine, macOS version and the date in the run notes.

Run notes, M4 Pro warm: 2026-11-04, Mac16,8 (Apple M4 Pro, 24 GB), macOS
27.0.1, a v0.1 build from the earlier private repository,
`scripts/phase1-gate.sh --cache warm` at full
scale. The app had no Full Disk Access, so its index of `/` (1.96M entries)
leaves out the protected folders; `first_run_bench` scans everything it can
read (2.32M). The machine was busy throughout (load average 39 at the start
and 12 to 16 later, mostly `fileproviderd` and Spotlight), so the latency
rows are on the pessimistic side: re-run alone, `matcher_bench` gave an
engine p95 of 7.5 and 9.3 ms at 2M where the gate run gave 16.74 ms.

Run notes, M4 Pro warm, v0.3.0: 2026-11-08, the same machine, macOS
27.0.1, warm cache. These rows were measured with v0.3.0, not by a full
`scripts/phase1-gate.sh` pass. Full-path search: `matcher_bench --entries
2000000`, load average 3.4. Keystroke to repaint: 28.5 ms from the v0.3.0
performance comparison against v0.2.0 (beta builds, load average about
2–3), and 35.2 ms from the gate probe over a real index of 1.92M entries
(44 keystrokes).

### Syntax and CLI

| Measure | M4 Pro warm | M4 Pro cold | 8 GB Mac warm | 8 GB Mac cold |
| --- | --- | --- | --- | --- |
| Conformance cases / failures | 370 / 0 | | | |
| fx-cli tests (`cli_ok`) | pass | | | |

### Goal 1: launch to first result painted

| Measure | M4 Pro warm | M4 Pro cold | 8 GB Mac warm | 8 GB Mac cold |
| --- | --- | --- | --- | --- |
| `launch_ms` (≤2000; stretch ≤1000): warm p95 over 20 launches; cold the single first launch after the reboot (worst sample) | 1277 ms (p50 1227, p99 1303) | | | |
| Gate probe index size (`ui_entries`, ≥2M) | 1,959,273 (`/` without Full Disk Access) | | | |
| `startup_bench` engine open to first result p95 (store side) | 1701 ms at 2M synthetic (reader 809 ms) | | | |

### Goal 2: search latency

| Measure | M4 Pro warm | M4 Pro cold | 8 GB Mac warm | 8 GB Mac cold |
| --- | --- | --- | --- | --- |
| `matcher_bench` engine p95 at 2M (≤16 ms) | 16.74 ms in the gate run; 7.5 and 9.3 ms re-run alone | | | |
| `matcher_bench` engine p95 at 5M (≤30 ms) | 25.3 ms | | | |
| Keystroke to repaint p95 (≤50 ms) | v0.3.0: 28.5 ms (p50 16.0 ms); 35.2 ms over the real 1.92M-entry index. v0.1.0: 57.4 ms (p95 of the 20 launches' p95s; per launch 40 to 62 ms, p50 about 30 ms) | | | |
| Regex p95 at 2M (≤100 ms) | 72.8 ms | | | |
| Full-path p95 at 2M (≤100 ms) | v0.3.0: 24.1 ms. v0.1.0: 187 ms (missed; 223 to 231 ms re-run alone) | | | |

### Goal 3: freshness

| Measure | M4 Pro warm | M4 Pro cold | 8 GB Mac warm | 8 GB Mac cold |
| --- | --- | --- | --- | --- |
| `freshness_bench` p99 over 1,000 ops (≤1000 ms) | 131 ms | | | |
| p95 (stretch ≤250 ms) and missed operations | 117 ms, 0 missed | | | |

### Offline catch-up, first run and memory

| Measure | M4 Pro warm | M4 Pro cold | 8 GB Mac warm | 8 GB Mac cold |
| --- | --- | --- | --- | --- |
| Catch-up equals a fresh scan (`catchup_equal`) | yes | | | |
| Catch-up equal within (≤10000 ms) | 456 ms | | | |
| Catch-up search usable within (≤2000 ms) | 14 ms | | | |
| First-run scan of `/` (≤15 s warm), entries, progressive snapshots | 14.6 s, 2,319,622 entries, 1 snapshot (not progressive) | | | |
| Memory B/entry (≤100) | 86.7 | | | |

## How the script maps benchmark output to the summary

`fx-bench phase1-summary` reads `metric=value` lines from every file in the
results directory (`crates/fx-bench/src/gate1.rs`). The benchmarks print
their own names, so `scripts/phase1-gate.sh` writes `gate-metrics.txt`:

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
   through the warm-up launch, the app's index for `/`):

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

   `--store-dir` keeps the 2M-entry `startup_bench` store; `--reuse` times
   that store as is. A cold pass without a kept store skips `startup_bench`
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
