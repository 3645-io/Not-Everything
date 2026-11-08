# Clean-room rules

Not-Everything is inspired by voidtools Everything, which is closed-source. The project must be able to show at any time that its code was written independently (`CLEANROOM.md`).

- Allowed sources: voidtools' public support pages, FAQ and forum; Apple, Microsoft and Linux documentation; MIT- or Apache-licensed code such as Cardinal, with attribution.
- Never decompile or disassemble Everything binaries, reverse-engineer `Everything.db` or its service protocol, or copy voidtools help text.
- Never read or copy GPL or unclear-license code: FSearch, Cling, Orange, ANGRYsearch, everything-sdk, and any repository without a clear license.
- The owner's local research notes in `.claude/research/` (gitignored, not in the repository) are off limits to the restricted modules: the index layout (`fx-model`, `fx-index`), the query matchers (`fx-query`), sorting (`fx-sort`) and the store's file format (`fx-store`). Work on them uses `docs/PLAN.md` and the modules' own code only. `.claude/settings.json` blocks the one note that summarises a GPL project's internals.
- Record the external sources each run or change consulted in `CLEANROOM.md`.
