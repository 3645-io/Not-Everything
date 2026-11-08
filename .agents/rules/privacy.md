# Privacy

The repository is public (`3645-io/Not-Everything`). No private information goes into it, or into anything it builds and ships, ever: once pushed, a file stays in every clone and fork.

- No personal or work email addresses. Commits use a GitHub noreply address; the project's public contact is `ne@3645.io`.
- No real home folder paths, user names or machine-specific notes: tests and docs use sample users such as `/Users/me`. No keychain contents, certificate fingerprints, team IDs or notarization submission IDs.
- No internal company names or links, and no links readers can't open (private Claude, Atlassian, Figma, Google Docs, Notion or Slack pages). Instructions must be readable by everyone.
- The owner's local notes are not in the repository: `docs/decisions/`, `.claude/research/` and `.agents/local/` are gitignored. Tracked files never cite a decision record, by number or by path; they give the reason in plain words.
- Screenshots and logs for docs, PRs and checks show the app's window only, with synthetic data (`--synthetic`, in a build with the `measure` feature), never the owner's real files or the whole desktop. Keep them in `.agents/local/screenshots/`.
- `crates/fx-testkit/tests/privacy_policy.rs` checks every tracked file; on the owner's Mac it also checks the exact strings in `.agents/local/private-patterns.txt`. The release script keeps the builder's home folder out of the binaries.
