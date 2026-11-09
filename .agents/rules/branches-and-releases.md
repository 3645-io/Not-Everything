# Branches and releases

## Where the repository lives

The project lives on GitHub at `3645-io/Not-Everything`, a public repository. Task PRs go into `dev` there.

## Branches

`main` is the default branch and holds released code. All development goes into `dev`, then an `rc/X.Y.Z` branch, then `main`, so `main` only ever holds what a release candidate tested.

- Task branches are cut from `dev`: `feature/<run id>-<short-slug>` for a plan run, and `bugfix/<description>`, `feature/<description>` or `chore/<description>` for anything else, with one PR each into `dev`. PR titles are a plain summary.
- Task PRs are squashed; release and back-merge PRs use merge commits.
- CI runs on pull requests, and by hand for the release-PR check. Standard GitHub runners are free for public repositories, but still run every check locally first (`runs.md`) and push a PR branch only when it is ready.
- Merge and publish only with the owner's go-ahead and after every CI check passes.

## Versions

- A release bumps the minor version (0.2.0, 0.3.0), a hotfix the patch. A hotfix's `rc/X.Y.(Z+1)` is cut from the release tag `release/vX.Y.Z`.
- The version stays X.Y.Z (the root `Cargo.toml`); release candidates and betas carry the label X.Y.Z-RC-N or X.Y.Z-beta-N, N counting from 1 per version.
- The build number is major×10000 + minor×100 + patch (0.2.0 is 200), so minor and patch stay below 100.

## A release

1. `dev` already carries version X.Y.Z. Add its `CHANGELOG.md` entry on `dev`, then cut `rc/X.Y.Z` from `dev`.
2. Only bugfix PRs (squashed) and merges of `main` go into the rc branch.
3. Each release candidate is `scripts/release-macos.sh --rc --publish target/<label>` on that branch: it tags `release/vX.Y.Z-RC-N` and makes a GitHub pre-release. Then run `scripts/perf-check.sh --tier rc --dmg <its DMG> --out <new dir>` on it (`performance.md`); a failing metric holds the release unless the owner approves it.
4. The release PR `rc/X.Y.Z` → `main` passes CI only when its head is the latest RC tag and contains `main`, and the version is new (`scripts/check-release-pr.sh`).
5. After it merges, `scripts/release-macos.sh --publish target/release-X.Y.Z` on `main` builds the release from the RC's tree: it tags `release/vX.Y.Z` and makes a full GitHub release with the `CHANGELOG.md` entry as notes.
6. Then a back-merge PR `main` → `dev`, which also moves `dev` to the next minor version, and the release's measured values become the new baseline in `docs/gates/perf-baseline.toml` (`docs/gates/performance.md`, "Changing the baseline").

The first release from a fresh repository: `main` and `dev` start identical, so a PR from `rc/X.Y.Z` into `main` would have nothing to merge. Give `rc/X.Y.Z` one commit before RC-1 (for example the changelog entry's real date) so the release PR can exist.

Never move or delete a release or a tag; that is the owner's step.

## Betas

`scripts/release-macos.sh --beta target/<label>` on `dev`: signed but not notarized, and kept locally. `--publish` only pushes the tag `beta/vX.Y.Z-beta-N`; a beta gets no GitHub page.

## The Preview app

Release candidates and betas are a separate app, so testing never touches the daily one, its settings, its index or its Full Disk Access grant: Not-Everything Preview (`io.3645.not-everything.preview`, data folder `~/Library/Application Support/Not-Everything Preview/`, command `not-everything-preview`, default hotkey Option+Shift+Space, Launch at Login off). `fx-channel` sets the identity at compile time from `NE_CHANNEL` (`release`, `rc` or `beta`) and `NE_CHANNEL_NUMBER`; a bad value fails the build.

## The release script

`scripts/release-macos.sh`, not CI, makes every build, tag and GitHub page, on the owner's Mac: the Developer ID certificate and the notary profile live only in the owner's keychain, so CI can't sign or notarize.

- It refuses to build when `THIRD-PARTY-NOTICES.md` doesn't match `Cargo.lock` (`scripts/third-party-notices.sh --check`).
- It builds in release with the home folder mapped out of the binaries' paths (`--remap-path-prefix`) and stops if a binary still holds it.
- It signs the app (`make_bundle --app --sign-identity`) and the command with the owner's Developer ID, named by `NE_SIGN_IDENTITY` in the owner's environment (never in the repository; the team ID is read from the name's closing brackets), with the hardened runtime. It stops unless the app's designated requirement is the bundle ID plus that team, so Full Disk Access carries over between builds and certificate renewals.
- It puts the app in a drag-to-install DMG (with an Applications shortcut, UDZO, signed with the same identity).
- For releases and RCs it notarizes in two uploads with the keychain profile `not-everything-notary`, in Apple's order: first the app and the command in one zip, then, once Apple's ticket covers both and the app is stapled, the DMG built from the stapled app, which it staples once its ticket covers it. One upload is not enough: an app can only be stapled after its own notarization, and `syspolicy_check distribution` rejects an unstapled app inside a DMG.
- It writes the DMG, the zip, the CLI tarball (the command, `LICENSE`, `THIRD-PARTY-NOTICES.md`, `docs/cli.md`, checked to be root:wheel with no extended attributes, ACLs or file flags) and `SHA256SUMS.txt`. It stops unless a quarantined copy of the DMG passes `hdiutil verify`, `spctl --assess --type open` as "Notarized Developer ID" and `stapler validate` with the app inside passing the app checks, and a quarantined copy of the zipped app passes `codesign --verify --deep --strict`, `spctl --assess` as "Notarized Developer ID" and `syspolicy_check distribution` (the app checks).
- Without `--publish` it tags and publishes nothing and prints the commands it would run.
- If it stops while Apple still holds an upload (lost contact, a closed terminal), finish with `scripts/release-macos.sh --resume OUT_DIR [--publish]`, which carries on at the stage it stopped in and never rebuilds or uploads again a stage that was uploaded. Check an upload with `xcrun notarytool history --keychain-profile not-everything-notary`.
- Never ship an ad hoc or unnotarized release or RC: macOS makes users approve such a download with Open Anyway, and an ad hoc signature loses the Full Disk Access grant on every update.
- Renew the certificate before it expires on 4 Dec 2027.
