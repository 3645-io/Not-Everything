# Not-Everything

Instant file-name search for macOS first, then Windows and Ubuntu, written in Rust with native UIs. A clean-room project inspired by voidtools Everything; not affiliated with voidtools.

These are the instructions for coding agents. The root `CLAUDE.md` imports this file and `AGENTS.md` points to it. The rules live in `.agents/rules/`: before you act, read the ones the table below names. The privacy and clean-room rules apply to everything, so they load with this file.

## Where things are

- `PROGRESS.md`: run status and hand-off notes. Read it first in every session; update it last.
- `docs/PLAN.md`: the research and build plan; runs are listed under "Runs: one session each". The owner also keeps it as a private document and mirrors changes between the two. Edit it only when asked, keep the header line under the title without a link, and keep pictures as `docs/img/` links.
- `CHANGELOG.md`: one entry per release, newest first; the GitHub release notes are copied from it.
- `docs/cli.md`: the manual of the command-line tool; the CLI tarball ships it.
- `docs/gates/performance.md` and `docs/gates/perf-baseline.toml`: the performance gate's metrics, targets and allowances, and the baseline every change is measured against with `scripts/perf-check.sh`.
- `CLEANROOM.md`: where the design and code come from, and the external sources each run or change consulted.
- `ATTRIBUTION.md`: the author, source and licence of every asset file.
- `THIRD-PARTY-NOTICES.md`: the licences of every crate the builds use, written by `scripts/third-party-notices.sh`.
- `SECURITY.md`: how to report a security problem, and what the app does and doesn't do.
- `crates/fx-ui-macos/assets/`: the app icons (the approved Glass set of the owner's private icon designs), built by `scripts/build-icons.sh` and listed in `ATTRIBUTION.md`. The script needs Xcode 26 or later, Node and Google Chrome; the owner reruns it only when the designs change and commits its output.
- Not in the repository: `docs/decisions/` (decision records), `.claude/research/` (research notes) and `.agents/local/` (screenshots, notes and `private-patterns.txt`) are the owner's local, gitignored notes. Read `rules/clean-room.md` before opening anything in `.claude/research/`.

## When you…, read…

| When you… | Read |
| --- | --- |
| do a run, a bug fix or an owner request | `rules/runs.md`, `rules/branches-and-releases.md` and `rules/performance.md` |
| change code that ships, or open a PR | `rules/performance.md` |
| make a release, a release candidate or a beta | `rules/branches-and-releases.md` |
| add or update a dependency | `rules/licensing.md` |
| add an asset (an icon, a picture) | `rules/licensing.md` |
| work on the interface | `rules/product.md` and `rules/conventions.md` |
| change what the app or the command does | `rules/product.md` |
| work on the index, sorting or the store's file format | `rules/clean-room.md` |
| write code, measure, or set up agents | `rules/conventions.md` (and `rules/performance.md` to measure) |
| do anything | `rules/privacy.md` and `rules/clean-room.md` (loaded below) |

A new rule file goes in `.agents/rules/` and is named in this table and in `AGENTS.md`.

## Always loaded

@rules/privacy.md

@rules/clean-room.md
