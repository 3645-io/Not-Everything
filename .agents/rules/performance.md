# Performance

Speed, a small footprint and a quiet background are the product. Every change that ships is measured before its PR, against the targets in `docs/gates/performance.md` and `docs/gates/perf-baseline.toml`: what the slowest supported Mac must meet, and the measuring Mac's share of it. At the pr and rc tiers a metric fails only when it misses its target; known gaps the owner accepted are reported, not failed. `scripts/perf-check.sh` runs the measurements and `fx-bench perf-compare` judges them.

## When to measure

- **While building.** After each change to a crate on the hot path, run its quick steps before the next change: `scripts/perf-check.sh --tier quick --only <crate> --out <new dir>` (1 to 3 minutes). Fix a failure before going on.

  | Crate changed | Quick steps |
  | --- | --- |
  | `fx-query`, `fx-model` | `matcher` |
  | `fx-index` | `matcher`, `startup`, `freshness` |
  | `fx-engine` | `startup`, `catchup`, `freshness` |
  | `fx-store` | `startup`, `catchup` |
  | `fx-sort` | `sort` |
  | `fx-fs`, `fx-fs-macos` | `freshness`, `catchup` |
  | `fx-ui-macos` | `gate-probe`, `summon`, `sort` |
  | `fx-cli` | `startup`, `cli` |

  `matcher` is `matcher_bench` (S1, S3, S4); `startup` is `startup_bench` (M4, M5); `catchup` is `catchup_bench` (F6, C2); `freshness` is `freshness_bench` (F1, C1); `gate-probe`, `summon` and `sort` are the app's probes (F2, S5, S7; F3; S6); `cli` times the `not-everything` command (O2). Any other crate: run the quick tier without `--only`.

- **Before the PR.** When every check in `runs.md` passes, run `scripts/perf-check.sh --tier pr --out <new dir>` on the M4 Pro, plugged in. The Mac doesn't need to be quiet, but the window steps bring the app forward, so start it when the owner is away from the screen or says it's free. It takes about 30 minutes. Push only when it passes, and paste its `summary.md`, including the `perf-compare: …` line, into the PR description. When a step failed and passed when re-run alone, paste both summaries: CI fails a PR that changes shipped code unless its description has a full run's line (`0 skipped`) and a passing line, the same one or the re-run's (`scripts/check-perf-report.sh`).
- **Each release candidate and release.** `scripts/perf-check.sh --tier rc --out <new dir>` on the commit being released, then `scripts/release-macos.sh ... --perf <that dir> --publish` (a release reuses its candidate's run: same code), which adds the DMG size, judges the rc tier again with it, and puts the Performance section (`fx-bench perf-release`: summary table, then every metric in a folded table, with 🟢 ⚪ 🟡 🔴 dots) in the release notes (`branches-and-releases.md`). Write any notes the section needs, such as why a metric moved, in `<that dir>/notes.md`. At the release, commit the script's `metrics-<version>.txt` as `docs/gates/releases/<version>.txt`.
- A change that touches no shipped code (docs, rules, scripts, CI, tests, benchmarks) skips the first two and says so in the PR.

## When a metric fails

1. Re-run that step alone once: `scripts/perf-check.sh --tier <tier> --step <step> --out <new dir>`. Only a second failure counts.
2. A second miss is a real miss of a target users feel: find the cause (a stack sample, `fx-bench sample`) and fix it.
3. If the miss buys something the owner wants, or its fix is a run of its own, stop and ask, with the measurements. Go on only with their approval, quoted in the PR and recorded in the gate file as the metric's `known_gap` (the work that fixes it). A PR that goes without the check at all needs the line `Performance: skipped, approved by the owner (<reason>)` in its description.
4. A pass near its limit (`PASS (near limit: …)`) is worth a line in the PR: it shows a drift before it fails.

Never edit a target, a known gap or the baseline to make a check pass. A target or a known gap changes only with the owner's approval; the baseline, shown beside each value and judging only the quick tier, moves only at a release (from the release candidate's run, with `fx-bench perf-compare --print-values`).

## Every measurement

- One benchmark at a time: `perf-check.sh` takes a lock in `$TMPDIR` before it builds, so agents working in parallel queue instead of overlapping. Don't run benchmarks outside it while it runs.
- Release builds only; the script builds with the `measure` feature the probes need.
- Keep the Mac unlocked and the display awake for the pr and rc tiers: on a locked screen the app's window can't come forward and its probes time out. `perf-check.sh` refuses to start on a locked screen, and `run.txt` records the screen at the start and the end, and each step after which it was locked.
- The machine and the cache state go next to every number (`run.txt` records them, with the displays by size).
- The script removes its scratch folder in `$TMPDIR` when it ends. A run killed with `kill -9` can't, and its folder (`not-everything-perf-<date>-<pid>`) grows every later run's index: remove it before the next run.
- Background and memory metrics run on the whole-disk index with file activity (`fx-bench churn`), never on a small folder: 0.3.0's idle check used the /Applications index and missed 35% of a core.
- The app runs on a data folder inside the run's output (`--data-dir`), never on the daily app's or the Preview app's.
- Numbers only in the repository and PRs: no paths, user names or screenshots of real files (`privacy.md`).
