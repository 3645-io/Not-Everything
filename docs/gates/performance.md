# Performance gate

Every change that ships is measured before its pull request against the metrics below. A change that misses a target, or is worse than the baseline by more than its allowance, is held until it is fixed or the owner approves it. Agents follow `.agents/rules/performance.md`; this page holds the definitions.

- `scripts/perf-check.sh --tier quick|pr|rc --out <new dir>` runs the measurements and writes `summary.md`.
- `fx-bench perf-compare` judges them against `docs/gates/perf-baseline.toml`: one table per metric with its key, target, allowance, tiers and baseline.
- The baseline was set from 0.3.2, the latest release, on the M4 Pro (24 GB, macOS 27) with a warm cache. It moves only at a release, or by an exception the owner approved, which the baseline file records with its reason and pull request.

## Tiers

| Tier | When | Where | Time | What runs | On a failure |
| --- | --- | --- | --- | --- | --- |
| `quick` | After each change to a crate on the hot path | The owner's Mac | 1–3 min | The crate's benchmarks at small scale (200,000 entries), against the quick baseline; targets are not checked at small scale | Fixed before the next change |
| `pr` | When the checks in `.agents/rules/runs.md` pass, before pushing | The M4 Pro, plugged in, nothing heavy running | About 30 min | Every metric marked P below at gate scale; 10 min with the window hidden and 10 min showing a search, under 20 file operations a second (`fx-bench churn`) | Re-run the step alone; still failing → fix it, or ask the owner. The summary goes in the PR |
| CI | Every pull request | GitHub | Seconds | `scripts/check-perf-report.sh`: a PR that changes shipped code must carry a passing `pr` summary, or the owner's approval to skip | The PR can't merge |
| `rc` | Each release candidate, before it is built | The M4 Pro, and the 8 GB Mac when there is one | About 1.5 h | As `pr`, with 20 launches, and 30 min hidden and 30 min showing a search on the whole-disk index of a Mac in daily use; plus `scripts/phase1-gate.sh` warm and cold. The release script adds the DMG size after its build | No release |

GitHub's shared macOS runners vary more between runs than the allowances, and have no real disk, Full Disk Access or daily file activity, so the timing tiers run on the owner's Mac. Every app launch uses a data folder inside the run's output (`--data-dir`), never the daily app's.

## Allowances

A metric fails when it misses its target or is worse than the baseline by more than its allowance. While the baseline itself misses the target (0.3.2's CPU, energy, wakeups and memory, and its first scan), the allowance alone holds the metric, so it can't get worse while its fix is pending; the report shows `PASS (target not yet met)`. Once a release meets the target, the target holds it too.

| Class | Metrics | Allowance |
| --- | --- | --- |
| Correctness | C1–C3 | None |
| Sizes and memory | M1–M5, B7, O1 | 5% |
| Timings | S1–S7, F1, F3–F6 | 10% of the median of the runs, and at least the floor below |
| Launch | F2 | 10% of the median of the launches |
| Background | B1, B2, B4–B6 | 10%, and at least 0.05 points of a core, 5 mW or 0.5 wakeups a second |
| Busiest interval | B3 | 25% |

Small numbers vary a lot between identical runs: in 0.3.0, idle CPU ranged from 0.24% to 0.42% across three runs of one build, and engine search at 2M took 16.7 ms p95 in one run and 7.5 ms in another. So a failing step is re-run alone once, and only a second failure counts.

M1 and M2 vary as much as their allowance. On 10 Nov, two runs of one build an hour apart read 1,383 and 1,317 MB with the window hidden, and 1,421 and 1,337 MB showing a search: the app's footprint starts each launch at a different level, and can step by about 70 MB in the middle of a phase without any CPU or disk activity. A failing M1 or M2 gets the same single re-run.

## Metrics

Tiers: Q quick, P pr, R rc. MB means 1,048,576 bytes, as Activity Monitor counts them.

### Background cost

| ID | Metric | Key | Target | Allowance | Tiers |
| --- | --- | --- | --- | --- | --- |
| B1 | CPU, window hidden, average % of one core | `hidden_cpu_pct` | ≤1% | 10%, ≥0.05 points | P R |
| B2 | CPU, window showing a search | `search_cpu_pct` | ≤5% | 10%, ≥0.05 points | P R |
| B3 | CPU, busiest 10 s interval | `cpu_max_interval_pct` | ≤100% | 25% (it varies a lot between runs) | P R |
| B4 | CPU energy, window hidden | `hidden_energy_mw` | ≤50 mW | 10%, ≥5 mW | P R |
| B5 | CPU energy, window showing a search | `search_energy_mw` | ≤250 mW | 10%, ≥5 mW | P R |
| B6 | Wakeups a second, window hidden | `hidden_wakeups_per_s` | ≤2 | 10%, ≥0.5 | P R |
| B7 | Disk writes, scaled to a day | `disk_written_per_day_mb` | ≤512 MB | 5%, ≥10 MB | P R |

### Memory

| ID | Metric | Key | Target | Allowance | Tiers |
| --- | --- | --- | --- | --- | --- |
| M1 | Footprint at the end of the hidden phase | `hidden_footprint_mb` | ≤200 MB | 5% | P R |
| M2 | Footprint at the end of the search phase | `search_footprint_mb` | ≤250 MB | 5% | P R |
| M3 | Footprint peak during the app's first scan | `first_scan_footprint_max_mb` | ≤500 MB | 5% | P R |
| M4 | Index columns per entry | `bytes_per_entry` | ≤100 B | 5% | Q P R |
| M5 | Saved index on disk per entry | `store_bytes_per_entry` | ≤100 B | 5% | Q P R |

### Search speed

| ID | Metric | Key | Target | Allowance | Tiers |
| --- | --- | --- | --- | --- | --- |
| S1 | Engine search p95 at 2M entries (worst query) | `engine_ms` | ≤16 ms | 10%, ≥1 ms | Q P R |
| S2 | Engine search p95 at 5M entries | `engine_5m_ms` | ≤30 ms | 10%, ≥1 ms | P R |
| S3 | Regex search p95 at 2M | `regex_ms` | ≤100 ms | 10%, ≥1 ms | Q P R |
| S4 | Full-path search p95 at 2M | `fullpath_ms` | ≤100 ms | 10%, ≥1 ms | Q P R |
| S5 | Keystroke to repaint p95 | `keystroke_p95_ms` | ≤50 ms | 10%, ≥1 ms | Q P R |
| S6 | Re-sort to repaint p95 | `sort_p95_ms` | ≤50 ms | 10%, ≥1 ms | Q P R |
| S7 | Scroll frame time p95 | `scroll_p95_ms` | ≤8.3 ms (one 120 Hz frame) | 10%, ≥1 ms | Q P R |

### Freshness and start-up

| ID | Metric | Key | Target | Allowance | Tiers |
| --- | --- | --- | --- | --- | --- |
| F1 | Create, rename or delete shown in an open result list, p99 | `freshness_p99_ms` | ≤1 s | 10%, ≥10 ms | Q P R |
| F2 | Launch to first result painted | `launch_ms` | ≤2 s | 10% of the median | Q P R |
| F3 | Shortcut to window ready, p95 (timed from the app's hotkey handler) | `summon_p95_ms` | ≤100 ms | 10%, ≥2 ms | Q P R |
| F4 | First scan of the whole disk | `first_run_ms` | ≤15 s | 10% | P R |
| F5 | First results shown during the first scan (tracked until progressive results are built) | `first_result_ms` | ≤2 s | tracked | P R |
| F6 | Catch-up after a relaunch: index equal to a fresh scan | `catchup_ms` | ≤10 s | 10%, ≥10 ms | Q P R |
| F6u | Catch-up after a relaunch: search usable | `catchup_usable_ms` | ≤2 s | 10%, ≥10 ms | Q P R |

### Correctness

| ID | Metric | Key | Target | Tiers |
| --- | --- | --- | --- | --- |
| C1 | Operations missed in the freshness test | `freshness_missed` | 0 | Q P R |
| C2 | Catch-up equals a fresh scan | `catchup_equal` | true | Q P R |
| C3 | Differences after a 24 h soak with 100,000 random file operations (tracked until the soak test is built) | `soak_differences` | 0 | R |

### Optional

| ID | Metric | Key | Target | Allowance | Tiers |
| --- | --- | --- | --- | --- | --- |
| O1 | DMG size | `dmg_bytes` | none | 5% | R |
| O2 | `not-everything` query time over a 2M-entry saved index (tracked) | `cli_query_ms` | ≤100 ms | tracked | Q P |

## In release notes

Every release candidate and release has a Performance section in its GitHub release notes. It compares the release with the previous release and the targets: a summary table (the IDs in the baseline file's `[meta]` `summary`), any hand-written notes, then every metric in a folded `<details>` table. `fx-bench perf-release` writes it, and `scripts/release-macos.sh --perf <perf-check rc folder>` adds it to the notes. The script refuses to publish a release candidate or a release without an rc-tier run of the same code (the same commit, or one with the same tree, so a release can use its candidate's run) that has no failing metric. Once it has built the DMG, it adds the DMG's size (O1) and judges the whole rc tier again, so a failing or unmeasured metric stops the release then.

GitHub strips text colour from release notes, so each row carries a dot next to its words:

- Change: 🟢 better · ⚪ the same, within 1% · 🟡 worse but within the allowed regression · 🔴 worse than the allowed regression.
- Target met: 🟢 met · 🟡 not yet · 🔴 missed, though the previous release met it. An ungated metric adds "(tracked)".

Each release's measured values are kept in `docs/gates/releases/<version>.txt` (the release script writes them to its output folder as `metrics-<label>.txt`), so the next release has its "previous" column. Hand-written notes for the section go in `notes.md` in the perf-check folder.

## Changing the baseline

- **At a release:** run `scripts/perf-check.sh --tier rc` on the release candidate, then `fx-bench perf-compare --print-values` on its `metrics.txt`, and copy the values into `perf-baseline.toml` with the build and date.
- **By exception:** when the owner accepts a regression, the metric's table gets `exception = "<reason>, approved in #<PR>"` and the new value as its baseline. `perf-compare` refuses an exception without a pull request number.
- **Never** to make a failing check pass.
