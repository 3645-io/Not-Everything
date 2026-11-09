# Runs, fixes and owner requests

## Doing a run

`docs/PLAN.md` lists the runs under "Runs: one session each". One run per session. When asked to "do run X.Y":

1. Read `PROGRESS.md`, then only the `docs/PLAN.md` sections the run needs.
2. Propose the run's plan (files, tests, open questions) and wait for approval before writing code.
3. Write failing tests first, then code until they pass. After each change to a crate on the hot path, run its quick performance steps (`performance.md`).
4. Run the checks below, the performance check last.
5. Self-review the diff against the rules in `.agents/rules/`, privacy included.
6. Commit on a branch `feature/<run id>-<short-slug>`, cut from `dev`, and open a PR into `dev` covering scope, tests, results and any benchmark numbers, with the performance check's `summary.md` in the description. `branches-and-releases.md` says which repository the PR goes to.
7. Update `PROGRESS.md` (status, measured numbers, follow-ups), then stop. Don't start the next run.

If a run is cut short, commit the work in progress on its branch and note in `PROGRESS.md` exactly where to resume.

## Bug fixes and owner requests

Between runs, bug fixes and the owner's requests follow the same steps: reproduce a bug first and write down how, then plan, approval, tests first, the checks, self-review. Each goes on a branch `bugfix/<description>`, `feature/<description>` or `chore/<description>` cut from `dev`, with one PR into `dev`.

## The checks

Run them all locally before pushing a PR branch. CI runs only some of them, and only on pull requests.

1. The targeted tests.
2. `cargo fmt --check`
3. `cargo clippy --workspace --all-targets -- -D warnings`, and again for the measurement build: `cargo clippy -p fx-ui-macos --all-targets --features measure -- -D warnings`
4. `cargo test --workspace`, and `cargo test -p fx-ui-macos --features measure`
5. `RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links" cargo doc --workspace --no-deps --document-private-items`, so no doc comment links to something that doesn't exist.
6. `cargo deny check licenses`, so every dependency's licence is on `deny.toml`'s allowlist (`licensing.md`).
7. After any dependency change, `sh scripts/third-party-notices.sh --check`. If it fails, run `sh scripts/third-party-notices.sh` and commit the new `THIRD-PARTY-NOTICES.md` (`licensing.md`).
8. When the change touches shipped code: `scripts/perf-check.sh --tier pr --out <new dir>`, last, on a quiet Mac (`performance.md`). Push only when it passes, or with the owner's approval of a regression.

## Measuring

Benchmarks and gate runs follow `conventions.md`: one at a time, with the machine and the cache state next to every number. The measurement flags need a build with the cargo feature `measure`; the gate scripts turn it on. `performance.md` says when to measure and what to do when a metric fails.
