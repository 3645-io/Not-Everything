# Phase 0 gate report

**Verdict: Gate not passed: measurements pending.**

This report tracks the Phase 0 checklist in `docs/PLAN.md` ("Phase 0:
foundations and spikes"). It has one section per checklist item, with the
criterion, its current status, the command that measures it and an empty
results table for the two gate machines. Run 0.8 wrote this report and the
runner script `scripts/phase0-gate.sh`. No agent could run release
benchmarks, the AppKit GUI or reboot a machine, so nothing below is ticked
yet. The owner fills in the tables with the steps at the end.

Status values:

- **Met**: measured on the gate machines and inside the gate.
- **Missed**: measured (formally or informally) and outside the gate.
- **Unmeasured**: the code and the benchmark exist, but there are no gate
  numbers yet.
- **Pending owner**: needs a decision or an action only the owner can take.

Statuses are taken from `PROGRESS.md` as of run 0.8; item 1 was updated on
8 Nov 2026, when the name and the trademark question were settled. Informal
numbers are labelled as such and are not gate results.

## Summary

| # | Item | Status |
| --- | --- | --- |
| 1 | Licensing, clean-room policy, name, trademark | Met |
| 2 | Scanner throughput | Missed (informal) |
| 3 | Search latency | Unmeasured |
| 4 | Store load to first result | Unmeasured |
| 5 | Change source: replay and live latency | Unmeasured |
| 6 | UI spike | Unmeasured |
| 7 | Re-run on both gate machines, warm and cold | Unmeasured |

All benchmark commands below are run for you by `scripts/phase0-gate.sh`;
they are listed so that a single number can be re-measured by hand. The
binaries are the release builds under `target/release/`.

## Checklist items

### 1. Licensing and clean-room provenance

**Criterion:** MIT license at the root, Apache-2.0 for designated
subcomponents, `CLEANROOM` provenance policy merged; working name chosen;
trademark question settled.

**Status: Met.** The licence, the provenance policy, the name and the
trademark question are all settled.

| Part | Status | Where |
| --- | --- | --- |
| MIT `LICENSE` at the root, Apache-2.0 subcomponent policy in the README | Met (run 0.1) | `LICENSE`, `README.md`, enforced by `crates/fx-testkit/tests/workspace_policy.rs` |
| `CLEANROOM.md` provenance policy | Met (run 0.1, sources added per run) | `CLEANROOM.md` |
| Working product name (must not contain "Everything") | Met by the owner's override: Not-Everything (4 Nov 2026); the product's own names are the only ones allowed to contain the word | `docs/PLAN.md` ("Naming"; risk #1) |
| Trademark knockout search | Settled: the owner settled on 7 Nov 2026 that "Everything" is not trademarked, so no search was done; counsel should review the name before Phase 5 closes (run 5.2) | `docs/PLAN.md` ("Naming"; risk #1) |

**Command:** none; this item is not a benchmark.

| Result | M4 Pro warm | M4 Pro cold | 8 GB Mac warm | 8 GB Mac cold |
| --- | --- | --- | --- | --- |
| Not machine-dependent | n/a | n/a | n/a | n/a |

### 2. Scanner throughput

**Criterion:** Rust `getattrlistbulk` scanner: ≥180k entries/s on one
thread; whole `/` (~1.9M entries) in ≤10 s warm; scales cleanly to 4–8
workers.

**Status: Missed** (informal run 0.3 numbers, not yet a formal gate result).
Informal, M4 Pro, warm cache: `/` had about 2.2M entries; 1 thread about
65k entries/s against the 180k gate; 8 threads about 10.4 s against the 10 s
gate. Tuning the attribute set is a follow-up.

A missed scanner gate does not block Phase 1: it is carried into run 1.7
(the production scanner: mount points, firmlinks, exclusions, cloud-file
policy), which re-tunes and re-measures it. The Phase 1 target that
depends on it is the first-run goal: ~2M entries indexed in ≤15 s warm,
with results shown progressively.

**Command** (warm; the script runs this):

```sh
target/release/examples/scan_bench --root / --threads 1,8 --iterations 5 --warmup 1 --cache warm
```

Cold (first thing after a reboot; only the first scan after boot is cold,
so the script scans once with 8 threads and scan_bench reports the 1-thread
gate as SKIP; a cold 1-thread number needs a second reboot and
`--threads 1` by hand):

```sh
target/release/examples/scan_bench --root / --threads 8 --iterations 1 --warmup 0 --cache cold
```

To check "scales cleanly to 4–8 workers", also run `--threads 1,2,4,6,8`
warm by hand and paste the speedup column. Give Terminal Full Disk Access
first, or the scan skips protected folders and the entry count is low.

| Result | M4 Pro warm | M4 Pro cold | 8 GB Mac warm | 8 GB Mac cold |
| --- | --- | --- | --- | --- |
| Entries scanned under `/` | | | | |
| 1 thread, entries/s at p50 (gate ≥180,000) | | | | |
| Best thread count, whole `/` p50 (s) (gate ≤10) | | | | |
| Speedup at 4 / 8 threads | | | | |
| Gate lines (PASS / FAIL / SKIP) | | | | |

### 3. Search latency

**Criterion:** Rust `memchr` + `rayon` scan: p95 ≤10 ms over 5M synthetic
names on 8 performance cores.

**Status: Unmeasured.** `fx-query::substring` and the `search_bench`
example exist (run 0.4); no agent could run release binaries.

**Command:**

```sh
target/release/examples/search_bench --threads 8 --cache warm
```

| Result | M4 Pro warm | M4 Pro cold | 8 GB Mac warm | 8 GB Mac cold |
| --- | --- | --- | --- | --- |
| Worst case-sensitive query p95 (ms) (gate ≤10) | | | | |
| Case-insensitive query p95 (ms) (reported, not gated) | | | | |
| Gate line (PASS / FAIL) | | | | |

### 4. Store load to first result

**Criterion:** Store: mmap load of a 5M-entry image to first result ≤50 ms
warm.

**Status: Unmeasured.** `fx-store` and the `load_bench` example exist
(run 0.5). The measured path runs a full search and takes the first ID, so
the number is an upper bound.

**Command:**

```sh
target/release/examples/load_bench --count 5000000 --cache warm
```

Writing the image puts it in the page cache. For a real cold number pass
`--store-dir DIR` to the script on the warm pass (it runs `load_bench --dir
DIR --keep`) and the same `--store-dir DIR` on the cold pass after the
reboot (it runs `load_bench --dir DIR --reuse --cache cold --iters 1`).
Only a reused image is ever timed with `--cache cold`. On a cold pass
without `--store-dir`, or with a `--store-dir` that holds no `index.fxs`,
the script runs no `load_bench` at all (a freshly written image would still
be in the page cache, so its number would be warm) and the cold summary
carries only a SKIP row explaining that a cold store measurement needs an
image written before the reboot (`--store-dir DIR` on both passes, reused
with `--reuse`). Warm passes are unchanged.

| Result | M4 Pro warm | M4 Pro cold | 8 GB Mac warm | 8 GB Mac cold |
| --- | --- | --- | --- | --- |
| Load to first result p95 (ms) (gate ≤50 warm) | | | | |
| Image size (MB) and bytes per entry | | | | |
| Gate line (PASS / FAIL) | | | | |

### 5. Change source: replay and live latency

**Criterion:** FSEvents: per-volume stream with saved UUID and event ID
replays a scripted offline change set exactly; live latency p95 ≤50 ms.

**Status: Unmeasured.** The own binding, `VolumeStream` and the cursor exist
(run 0.6) and the offline replay tests pass, but the benchmark gate numbers
are not recorded.

**Commands:**

```sh
target/release/examples/fsevents_bench latency --ops 1000 --cache warm
target/release/examples/fsevents_bench replay --ops 10000
```

Also record how many scripted paths were covered only by the subtree
rescans the stream adds for directory renames (printed by `replay`).

| Result | M4 Pro warm | M4 Pro cold | 8 GB Mac warm | 8 GB Mac cold |
| --- | --- | --- | --- | --- |
| Live latency p95 (ms) (gate ≤50) | | | | |
| Replay exact (PASS / FAIL), events, time to HistoryDone | | | | |
| Paths covered only by added rescans | | | | |

### 6. UI spike

**Criterion:** UI spike (`objc2` NSTableView vs Swift shell vs Tauri):
2M-row table scrolls at ≤16.7 ms per frame; launch time and `.app` size
measured; decision recorded.

**Status: Unmeasured.** The objc2 spike exists (run 0.7), and its shell
decision, AppKit through `objc2`, stays provisional until these numbers
exist. The app has been built on it since run 1.9. An informal agent run
(warm, unverified machine) is not a gate result.

**Commands** (in a logged-in Terminal session; the window server is needed; the
measurement flags need the `measure` feature):

```sh
cargo build --release --locked -p fx-ui-macos --features measure --examples
target/release/examples/table_spike --rows 2000000 --autoscroll 600
target/release/examples/table_spike --rows 2000000 --autoscroll 600 --rows-per-frame 3000
target/release/examples/table_spike --launch-probe --json   # 20 times
target/release/examples/make_bundle OUT_DIR target/release/examples/table_spike
```

The script runs each autoscroll variant three times, 20 launch probes (warm:
after one discarded warm-up; cold: as the first measurement after the
reboot, so the first probe is the cold launch) and `make_bundle` into
`<out>/bundle`.

| Result | M4 Pro warm | M4 Pro cold | 8 GB Mac warm | 8 GB Mac cold |
| --- | --- | --- | --- | --- |
| Autoscroll p95, 5 rows/frame, worst of 3 (ms) (gate ≤16.7) | | | | |
| Autoscroll p95, 3,000 rows/frame, worst of 3 (ms) | | | | |
| Launch to first paint, p50 / p95 of 20 (ms) | | | | |
| First launch after reboot (ms) | n/a | | n/a | |
| `.app` size (bytes) | | | | |
| Shell decision final (yes / no) | | | | |

### 7. Re-run on both gate machines, warm and cold

**Criterion:** All benchmarks re-run in Rust on two machines, including
after a cold boot.

**Status: Unmeasured.** The runner script exists; none of the four passes
has been run. The gate machines are the M4 Pro and the 8 GB Mac.

**Command:** one pass per machine and cache state:

```sh
scripts/phase0-gate.sh --machine "M4 Pro" --cache warm --out ~/fx-gate/m4pro-warm --store-dir ~/fx-gate/m4pro-store
scripts/phase0-gate.sh --machine "M4 Pro" --cache cold --out ~/fx-gate/m4pro-cold --store-dir ~/fx-gate/m4pro-store
scripts/phase0-gate.sh --machine "8 GB Mac" --cache warm --out ~/fx-gate/8gb-warm --store-dir ~/fx-gate/8gb-store
scripts/phase0-gate.sh --machine "8 GB Mac" --cache cold --out ~/fx-gate/8gb-cold --store-dir ~/fx-gate/8gb-store
```

| Result | M4 Pro warm | M4 Pro cold | 8 GB Mac warm | 8 GB Mac cold |
| --- | --- | --- | --- | --- |
| Pass run (date, macOS version, git commit from `run.txt`) | | | | |
| gate-summary exit code (0 all PASS, 1 otherwise) | | | | |
| `summary.md` pasted below (yes / no) | | | | |

## The runner script

`scripts/phase0-gate.sh --machine LABEL --cache warm|cold --out NEW_DIR
[--skip-ui] [--skip-scan] [--store-dir DIR]`

- If the file is not executable in your checkout, run it as
  `sh scripts/phase0-gate.sh ...`; it is plain POSIX `sh` with `set -eu`.
- It validates its arguments first and refuses an `--out` directory that
  already exists. It only creates `--out` and files inside it; it never
  deletes, renames or moves anything and never runs `sudo`.
- It builds once in release (`cargo build --release --locked --workspace
  --bins --examples --features fx-ui-macos/measure`), then runs every benchmark one at a time, never in
  parallel, passing the cache label to each benchmark that accepts one.
- Each benchmark's stdout and stderr go to `<out>/<name>.log`. `run.txt`
  records the machine, macOS version, model, memory, uptime (which shows
  whether the machine was just rebooted), git commit and rustc version.
- `checks.log` holds the script's own gate lines: every benchmark's exit
  code, whether the two `fx-bench gen --count 5000000 --seed 1` checksums
  match, whether all 20 launch probes ran, the `.app` size, and a SKIP line
  for anything skipped with `--skip-ui` or `--skip-scan`.
- Finally it runs `fx-bench gate-summary --machine LABEL --cache STATE` over
  the logs that carry gate lines plus `checks.log`, writes `<out>/summary.md`
  and exits with gate-summary's code: 0 when every row is PASS, 1 when any
  row is FAIL, SKIP, UNPARSED or MISSING, 2 for bad arguments or an
  unreadable log. Informational logs (gen, self-bench, launch probes,
  make_bundle) have no GATE line of their own, so they are reported through
  `checks.log` instead of as MISSING rows.
- A cold pass scans `/` once with 8 threads, so its summary always has a
  SKIP row for the 1-thread scanner gate and exits 1 even when everything
  else passes. Read the rows rather than only the exit code.
- On a cold pass the launch probes, the scan and the store load run first,
  before the in-memory benchmarks, while the cache is still cold.

## Steps for the owner

1. **Prepare each machine.** Plug in the power, quit other apps, and give
   Terminal Full Disk Access (System Settings > Privacy & Security). Use a
   logged-in session; the UI benchmarks open a window. Do not touch the
   mouse while the UI benchmarks run.
2. **Warm pass.** From the repository root, run
   `scripts/phase0-gate.sh --machine "M4 Pro" --cache warm --out ~/fx-gate/m4pro-warm --store-dir ~/fx-gate/m4pro-store`.
   This also builds the release binaries and keeps the store image for the
   cold pass. The whole pass takes several minutes.
3. **Reboot procedure for the cold pass.** Restart the Mac (Apple menu >
   Restart; do not reopen windows), log in, wait about two minutes for
   login items and Spotlight to settle, open Terminal, `cd` to the
   repository and, as the first thing, run
   `scripts/phase0-gate.sh --machine "M4 Pro" --cache cold --out ~/fx-gate/m4pro-cold --store-dir ~/fx-gate/m4pro-store`.
   The release build is then a no-op, and the launch probes, the scan and
   the store load run first, while the cache is still cold.
4. **Repeat** steps 1–3 on the 8 GB Mac with `--machine "8 GB Mac"`.
5. **Paste** each pass's `summary.md` into the matching subsection under
   "Results" below, copy the numbers into the item tables above, set each
   item's status to Met or Missed, and copy the numbers into the run rows
   of `PROGRESS.md`. The UI numbers settle the shell decision.
6. **Verdict.** Change the verdict at the top to "Gate passed" only when
   items 2–7 are Met (or a miss is explicitly carried forward, as the
   scanner is into run 1.7) and item 1 is complete.

`sudo purge` is a weaker approximation of a cold cache: it flushes the disk
cache but may not evict all cached file-system metadata, and it leaves
processes and system frameworks resident. The script never runs it. A
purged measurement does not count as a cold result; if you take one, run the
single benchmark command by hand right after `sudo purge` with `--cache
unknown` and record it in a separate row labelled "purged", not "cold".

## Results

Paste each `summary.md` here, unedited.

### M4 Pro, warm

_Not yet run._

### M4 Pro, cold (after reboot)

_Not yet run._

### 8 GB Mac, warm

_Not yet run._

### 8 GB Mac, cold (after reboot)

_Not yet run._
