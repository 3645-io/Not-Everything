# Not-Everything: instructions for agents

The project's instructions for coding agents live in [`.agents/CLAUDE.md`](.agents/CLAUDE.md), an index of the rule files in `.agents/rules/`. Read the index before working in this repository, then the rules it names for your task. Always read the privacy and clean-room rules: Claude Code loads them with the index, other tools may not.

- [`.agents/rules/runs.md`](.agents/rules/runs.md): doing a run, bug fixes and owner requests, and the checks.
- [`.agents/rules/branches-and-releases.md`](.agents/rules/branches-and-releases.md): where the repository lives, branches, versions, releases, betas and the release script.
- [`.agents/rules/privacy.md`](.agents/rules/privacy.md): what never goes into the repository or anything it ships.
- [`.agents/rules/clean-room.md`](.agents/rules/clean-room.md): the allowed sources, and what must never be read.
- [`.agents/rules/product.md`](.agents/rules/product.md): what the app and the command may do, and their names.
- [`.agents/rules/licensing.md`](.agents/rules/licensing.md): licences, the dependency allowlist, third-party notices and attribution.
- [`.agents/rules/conventions.md`](.agents/rules/conventions.md): Rust and the macOS interface, guards, measuring and benchmarks, agents.
