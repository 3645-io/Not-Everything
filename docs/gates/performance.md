# Performance gate

Every change that ships is measured before its pull request against the metrics below. A change that misses a target is held until it is fixed, or the owner records the miss as a known gap. Agents follow `.agents/rules/performance.md`; this page holds the definitions.

- `scripts/perf-check.sh --tier quick|pr|rc --out <new dir>` runs the measurements and writes `summary.md`.
- `fx-bench perf-compare` judges them against `docs/gates/perf-baseline.toml`: one table per metric with its key, its target on the measuring Mac and on the slowest supported Mac, any known gap, its tiers and its baseline.
- The targets are the owner's of 14 Nov 2026: what makes the app feel instant and light on the slowest Mac it supports, from perception and resource research, set out in the owner's targets document.
- The baseline is 0.6.0, the latest release, from its release candidates' rc tier on the M4 Pro (24 GB, macOS 27) with a warm cache. It is shown beside every value, and judges only the quick tier and any metric without a target. It moves only at a release.
- The pr tier runs its background phases under file activity and the rc tier without, so the background metrics (B1 to B6, M1 and M2) also carry a `pr_baseline`, shown at the pr tier. It is still 0.5.0's, measured at the pr tier on 14 Nov 2026.

## Tiers

| Tier | When | Where | Time | What runs | On a failure |
| --- | --- | --- | --- | --- | --- |
| `quick` | After each change to a crate on the hot path | The owner's Mac | 1–3 min | The crate's benchmarks at small scale (200,000 entries), against the quick baseline; targets are not checked at small scale | Fixed before the next change |
| `pr` | When the checks in `.agents/rules/runs.md` pass, before pushing | The M4 Pro, plugged in; the Mac can be in use, but the window steps need the screen | About 30 min | Every metric marked P below at gate scale; 10 min with the window hidden and 10 min showing a search, under 20 file operations a second (`fx-bench churn`) | Re-run the step alone; still missing its target → fix it, or ask the owner to record a known gap. The summary goes in the PR |
| CI | Every pull request | GitHub | Seconds | `scripts/check-perf-report.sh`: a PR that changes shipped code must carry a passing `pr` summary, or the owner's approval to skip | The PR can't merge |
| `rc` | Each release candidate, before it is built | The M4 Pro (there is no budget for a dedicated M1, the owner's decision of 14 Nov 2026) | About 1.5 h | As `pr`, with 20 launches, and 30 min hidden and 30 min showing a search on the whole-disk index of a Mac in daily use; plus `scripts/phase1-gate.sh` warm and cold. The release script adds the DMG size after its build | No release |

GitHub's shared macOS runners vary too much between runs to time anything, and have no real disk, Full Disk Access or daily file activity, so the timing tiers run on the owner's Mac. Every app launch uses a data folder inside the run's output (`--data-dir`), never the daily app's.

Keep the Mac unlocked and the display awake for the `pr` and `rc` tiers. On a locked screen the app's window can't come forward, so every probe that summons it times out: on 13 Nov 2026 the screen locked two minutes into an rc run, and all 50 shortcut presses of F3 timed out at 5 s. `perf-check.sh` refuses to start on a locked screen (exit 2, nothing changed), and checks again once it holds the lock. `run.txt` records the screen at the start and at the end, and notes each step after which it was locked.

## How the gate judges

At the pr and rc tiers a metric with a target is judged on it alone: it fails only when it misses the target, whatever its baseline. The owner chose this on 14 Nov 2026, after two days in which the old check, worse than the baseline by more than an allowance, tripped on the measuring Mac's state rather than the code.

- **Two targets per metric.** The M1 target is what the slowest supported Mac, a base M1 MacBook Air with 8 GB, must meet. The gate's target, on the M4 Pro, is 0.6 of it for work on one core and 0.4 for search spread over all cores, from public Geekbench 6 scores (approximately: M1 2,350 single-core and 8,600 multi-core, the 12-core M4 Pro 3,840 and 22,300). Sizes, counts, memory per file and correctness are the same on both.
- **Known gaps.** A target the current build misses, which the owner accepted until the work that fixes it lands, carries `known_gap` with that work. It reads `KNOWN GAP (<work>)`, is counted on the summary line, and doesn't fail. A pass that meets a known gap's target reads `PASS (known gap met at this tier)`: once every tier meets it, the owner removes the gap and the target blocks again.
- **Near the limit.** A pass at 80% of its target or more reads `PASS (near limit: N% of target)`, so a slow drift shows before it fails.
- **Per file.** `per_key` and `per_scale` divide a metric by another key: memory and the first scan per million files of the index, or the first scan's memory peak as a multiple of the hidden memory.
- **One re-run.** Small numbers vary between identical runs, so a failing step is re-run alone once, and only a second failure counts.

The quick tier runs at a small scale where the targets don't apply, so it compares with `quick_baseline` within an allowance, as a developer's aid. A metric without a target would be compared with its baseline the same way; every gated metric has a target.

| Class | Metrics | Allowance at the quick tier |
| --- | --- | --- |
| Correctness | C1–C3 | None |
| Sizes and memory | M4, M5 | 5% |
| Timings | S1, S3–S7, F1, F3, F6, F6u | 10%, and at least the floor in the baseline file |
| Launch | F2 | 10% of the median of the launches |

## Metrics

Tiers: Q quick, P pr, R rc. MB means 1,048,576 bytes, as Activity Monitor counts them.

### Background cost

| ID | Metric | Key | Target (M4 Pro) | M1 target | Known gap | Tiers |
| --- | --- | --- | --- | --- | --- | --- |
| B1 | CPU, window hidden, average % of one core | `hidden_cpu_pct` | ≤0.06% | ≤0.1% | Quiet background (run 2.21) | P R |
| B2 | CPU, window showing a search | `search_cpu_pct` | ≤0.6% | ≤1% | Quiet background (run 2.21) | P R |
| B3 | CPU, busiest 10 s interval (tracked until the file-change storm metric replaces it) | `cpu_max_interval_pct` | ≤100% | – | – | P R |
| B4 | CPU energy, window hidden (tracked: it follows B1) | `hidden_energy_mw` | ≤50 mW | – | – | P R |
| B5 | CPU energy, window showing a search (tracked: it follows B2) | `search_energy_mw` | ≤250 mW | – | – | P R |
| B6 | Wakeups a second, window hidden | `hidden_wakeups_per_s` | ≤1 | ≤1 | Quiet background (run 2.21) | P R |
| B7 | Disk writes a day: those measured, scaled to a day, plus the projected index saves (below) | `disk_written_per_day_mb` | ≤250 MB | ≤250 MB | Incremental saves (run 2.22) | P R |

B7 adds the index saves a phase is too short to see. After each compaction the app saves its whole index to the store file, about 150 MB for a whole-disk index of 2M entries. The writer compacts when the files created since the last compaction, or the files deleted since then, pass 10% of the index's entries (at least 1,024): about every 200,000 at that scale, where a 10-minute phase at 20 operations a second creates about 6,000. So after each phase `perf-check.sh` runs `fx-bench saves`. It takes the store file the run's app saved (its size, and its entry count from `not-everything -get-result-count`) and the files `fx-bench churn` created and deleted in the phase, scales the larger of the two counts to a day, and projects one save of the store's size for every 10% of the entries in it. B7 is the measured writes plus that projection. At the pr tier's 20 operations a second, about half of them creates, that comes to about 4.3 saves a day: about 650 MB on the 2M-entry index. The summary shows the parts by phase, and fx-engine's `tests/compaction_saves.rs` checks the model against the real writer.

The projection counts only the scripted activity: the rc tier's phases, with none, project no saves, though a Mac in daily use compacts too, and the daily save, which comes only after a day without one, is not projected either.

The compaction saves add to the daily save, with an upper bound. The daily save comes 24 hours after the store's last write, whatever made it, so a compaction's save restarts the day: a day with a compaction often has no daily save, and a disk that compacts more than once a day makes no daily save at all. So "one save a day" means at most one daily save plus one save per compaction, plus one per quit (fx-ui-macos's `tests/daily_save_day.rs` simulates weeks of it). How often a whole disk in daily use compacts is still to be measured: fx-engine's `day_replay_bench` counts the files a day's FSEvents history created and deleted and projects the compactions a day with the same model (numbers to come). B7's target, and whether it gates, stay the owner's call once those numbers are in.

### Memory

| ID | Metric | Key | Target (M4 Pro) | M1 target | Known gap | Tiers |
| --- | --- | --- | --- | --- | --- | --- |
| M1 | Footprint at the end of the hidden phase, per million files of its index | `hidden_footprint_mb` ÷ `hidden_index_entries` | ≤60 MB | ≤60 MB | – | P R |
| M2 | Footprint at the end of the search phase, per million files | `search_footprint_mb` ÷ `search_index_entries` | ≤80 MB | ≤80 MB | Quiet background, under file activity (run 2.21) | P R |
| M3 | Footprint peak during the app's first scan, as a multiple of M1's footprint | `first_scan_footprint_max_mb` ÷ `hidden_footprint_mb` | ≤2× | ≤2× | Lower scan peak (run 2.25) | P R |
| M4 | Index columns per entry (a diagnostic, tracked) | `bytes_per_entry` | ≤100 B | – | – | Q P R |
| M5 | Saved index on disk per entry | `store_bytes_per_entry` | ≤45 B | ≤45 B | Smaller index file, a later target (run 2.26) | Q P R |

### Search speed

| ID | Metric | Key | Target (M4 Pro) | M1 target | Known gap | Tiers |
| --- | --- | --- | --- | --- | --- | --- |
| S1 | Engine search p95 at 2M entries (worst query; all cores) | `engine_ms` | ≤6 ms | ≤16 ms | Faster engine search (run 2.27) | Q P R |
| S3 | Regex search p95 at 2M; the next keystroke cancels it | `regex_ms` | ≤100 ms | ≤250 ms | – | Q P R |
| S4 | Full-path search p95 at 2M | `fullpath_ms` | ≤100 ms | ≤250 ms | – | Q P R |
| S5 | Keystroke to repaint p95 | `keystroke_p95_ms` | ≤30 ms | ≤50 ms | – | Q P R |
| S6 | Re-sort to repaint p95 | `sort_p95_ms` | ≤20 ms | ≤50 ms | – | Q P R |
| S7 | Scroll frame time p95 | `scroll_p95_ms` | ≤5 ms | ≤8.3 ms (one 120 Hz frame) | – | Q P R |

S2, the engine search at 5M entries, is gone; the 10M stress set, a later PR, replaces it.

### Freshness and start-up

| ID | Metric | Key | Target (M4 Pro) | M1 target | Known gap | Tiers |
| --- | --- | --- | --- | --- | --- | --- |
| F1 | Create, rename or delete shown in an open result list, p99 | `freshness_p99_ms` | ≤500 ms | ≤500 ms | – | Q P R |
| F2 | Launch to first result painted | `launch_ms` | ≤600 ms | ≤1 s | – | Q P R |
| F3 | Shortcut to window ready, p95 (timed from the app's hotkey handler) | `summon_p95_ms` | ≤30 ms | ≤50 ms | – | Q P R |
| F4 | First scan of the whole disk, per million files | `first_run_ms` ÷ `first_run_entries` | ≤7.5 s | ≤15 s | – | P R |
| F5 | First results shown during the first scan | `first_result_ms` | ≤1 s | ≤2 s | Progressive first scan (run 2.24) | P R |
| F6 | Catch-up after a relaunch: index equal to a fresh scan (10,000 changes; it becomes catch-up after a crash once saves are incremental) | `catchup_ms` | ≤3 s | ≤5 s | – | Q P R |
| F6u | Catch-up after a relaunch: search usable | `catchup_usable_ms` | ≤150 ms | ≤250 ms | – | Q P R |

### Correctness

| ID | Metric | Key | Target | Tiers |
| --- | --- | --- | --- | --- |
| C1 | Operations missed in the freshness test | `freshness_missed` | 0 | Q P R |
| C2 | Catch-up equals a fresh scan | `catchup_equal` | true | Q P R |
| C3 | Differences after a 24 h soak with 100,000 random file operations (tracked: it runs only with `--step soak`; see [The soak](#the-soak)) | `soak_differences` | 0 | R |

### Optional

| ID | Metric | Key | Target (M4 Pro) | Tiers |
| --- | --- | --- | --- | --- |
| O1 | DMG size (tracked) | `dmg_bytes` | none | R |
| O2 | `not-everything` query time over a 2M-entry saved index | `cli_query_ms` | ≤200 ms | Q P |

## The soak

C3 comes from `crates/fx-engine/examples/soak.rs`. It runs the engine, with the real FSEvents source, path stat and scanner set up as the app sets them up, over a fresh folder in `$TMPDIR`, under a seeded stream of random file operations (`fx_testkit::soak`) at 10 a second: creates, rewrites, saves that replace a file with a new one, renames and case-only renames, moves of files and folders, deletes of files and whole folders, folders moved in from outside and out of it (as to the Trash), name swaps, deep trees and bursts.

- Every 30 s it stops and waits for the index to equal a fresh scan (the same paths, each with the same file key, kind and file size, as C2 compares) and every merged sort order to equal a fresh build, for 250 ms in a row.
- Every 10th check it first closes the engine, which saves the index, makes changes while it is down, and opens it again from the saved cursor, so the FSEvents history replay is soaked too.
- `soak_differences` counts the paths and sort orders that still differ after 30 s. Each one is printed with the operations before it, and the engine restarts from a fresh scan, so the next check counts only new differences.
- It also reports the settle times, modification times that differ on otherwise equal entries (`soak_metadata_differences`, not counted: FSEvents reports no event for a folder whose contents change), and the memory growth from the first check with the tree near its 10,000-entry cap to the end (`soak_memory_growth_pct`; the plan allows 5%).

Run it for 24 h on each release candidate, on a quiet Mac, plugged in. It holds the measurement lock for the whole run:

```text
scripts/perf-check.sh --tier rc --step soak --out <new dir>
```

`--soak-minutes N` runs it for N minutes instead of 1440. Outside the gate, `cargo run --release -p fx-engine --example soak -- --duration 24h` runs it alone (`--help` lists its settings). C3 stays tracked, so it never fails the gate, because the soak is not part of every rc run. The fuzz test `crates/fx-engine/tests/fuzz.rs` runs the same operation stream against the fake file system in every `cargo test`, with events delivered late, out of order, twice or folded into subtree rescans.

## In release notes

Every release candidate and release has a Performance section in its GitHub release notes. It compares the release with the previous release and the targets: a summary table (the IDs in the baseline file's `[meta]` `summary`), any hand-written notes, then every metric in a folded `<details>` table. `fx-bench perf-release` writes it, and `scripts/release-macos.sh --perf <perf-check rc folder>` adds it to the notes. The script refuses to publish a release candidate or a release without an rc-tier run of the same code that has no failing metric. The code is the same for the same commit, one with the same tree (so a release can use its candidate's run), or one that differs only in files no build compiles: documents (but `THIRD-PARTY-NOTICES.md`, which the app and the command embed), `docs/`, `scripts/`, `.github/`, `.agents/` and crates' tests. Once it has built the DMG, it adds the DMG's size (O1) and judges the whole rc tier again, so a failing or unmeasured metric stops the release then. With the owner's approval, `--targets-only` judges the run on targets only: a metric that failed only its regression check (beyond its allowance, within its target) doesn't stop the release, and the notes name it; a missed target or an unmeasured metric still does.

GitHub strips text colour from release notes, so each row carries a dot next to its words:

- Change: 🟢 better · ⚪ the same, within 1% · 🟡 worse but within the allowed regression · 🔴 worse than the allowed regression.
- Target met: 🟢 met · 🟡 a known gap, or not yet · 🔴 missed, though the previous release met it. An ungated metric adds "(tracked)".

Each release's measured values are kept in `docs/gates/releases/<version>.txt` (the release script writes them to its output folder as `metrics-<label>.txt`), so the next release has its "previous" column. Hand-written notes for the section go in `notes.md` in the perf-check folder.

## Changing a target, a known gap or the baseline

- **A target** changes only in a PR the owner approves, with the measurements behind it.
- **A known gap** is added or removed only with the owner's approval, naming the work that fixes it. Remove it once every tier meets its target.
- **The baseline**, at a release: run `scripts/perf-check.sh --tier rc` on the release candidate, then `fx-bench perf-compare --print-values` on its `metrics.txt`, and copy the values into `perf-baseline.toml` with the build and date. Per-file metrics print their per-file value.
- **Never** to make a failing check pass.
