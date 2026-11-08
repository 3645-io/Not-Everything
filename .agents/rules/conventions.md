# Conventions

## Rust

- Stable Rust (`rust-toolchain.toml`) in a Cargo workspace. Crates keep the `fx-` prefix; the product name is not used in crate names.
- Guards say why: a check that looks removable says what it prevents. Read that before changing it.

## The macOS interface

- AppKit through `objc2`, drawn to the 3645 design. The windows keep AppKit's structure and behaviour (windows, views, the view-based table, text editing, the responder chain, keys, drag-out, accessibility) and draw their own look, because the design's raised and pressed surfaces can't be had from standard controls.
- A screen matches the design 1:1 wherever macOS allows. Every colour, type style, spacing step, radius and shadow comes from `crates/fx-ui-macos/src/theme.rs`, which transcribes the design's tokens; its tests pin each value. The design files are private, so the owner supplies what a change needs.

## Measuring

- The measurement flags (`--synthetic`, `--launch-probe`, `--gate-probe`, `--autoscroll`, `--page-cost-ms`) are compiled only with the cargo feature `measure`. It is off in release builds, so no shipped app carries them, and on in the gate scripts (`scripts/phase0-gate.sh`, `scripts/phase1-gate.sh`). Put any new flag that only measures or fakes data behind the same feature.
- Run benchmarks one at a time, never in parallel, and record the machine and whether the cache was cold or warm next to every number.

## Agents

- Don't spawn parallel agents unless the owner asks for them.
- `.claude/settings.json` is shared, and a checkout may be a stranger's pull request. So it allows without asking only commands that write nothing and run nothing from the checked-out branch: `git status`, `git diff`, `git log`, `git show`, `git branch --show-current` and `git worktree list`. It denies git's `--output`, with which `git diff`, `git log` and `git show` write a file, keeps the clean-room deny rule, and holds nothing else. A rule matches the command as written, so a quoted or split `--output` (`"--output=x"`) gets past the deny: it stops the plain form only. `rustc --version` and `rustup show` aren't allowed: through rustup they run the toolchain the branch's `rust-toolchain.toml` names.
- Build, test and other cargo commands aren't pre-approved, because they run code from the checked-out branch: build scripts, proc macros and tests. Even `cargo fmt --check`, `cargo metadata` and `cargo tree` run a program the branch's `.cargo/config.toml` names. If you trust the branch, allow them in your own `.claude/settings.local.json`, which is gitignored, along with the commands that change the repository (`git add`, `git commit`, `git switch`).
- A new rule file goes in `.agents/rules/` and is named in `.agents/CLAUDE.md` and `AGENTS.md`. `crates/fx-testkit/tests/agent_rules.rs` checks both, and the settings.
