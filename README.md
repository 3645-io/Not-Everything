# Not-Everything

Instant file-name search for macOS first, then Windows and Ubuntu, in Rust with native UIs.

Inspired by voidtools Everything; not affiliated with voidtools.

> **Status:** a macOS preview, built through Phase 1 of the plan with the v0.3.0 interface.
> The Phase 0 and Phase 1 gates are partly measured ([`docs/gates/`](docs/gates/)).
> v0.3.0 is the first release published from this repository.
> Releases are signed with a Developer ID, notarized and come as a DMG; the Homebrew cask
> and automatic updates come in Phase 2. See [`CHANGELOG.md`](CHANGELOG.md) for what is in
> it, [`docs/PLAN.md`](docs/PLAN.md) for the build plan and [`PROGRESS.md`](PROGRESS.md) for
> run status.

## Install (macOS 13 or later, Apple silicon)

Download `Not-Everything-<version>-macos-arm64.dmg` from the
[releases page](https://github.com/3645-io/Not-Everything/releases), open it, and drag
Not-Everything onto the Applications shortcut in its window. The release also has the same app
as `Not-Everything-<version>-macos-arm64.zip`, the `not-everything` command in
`not-everything-cli-<version>-macos-arm64.tar.gz` (with its manual `cli.md`, `LICENSE` and the
third-party notices), and `SHA256SUMS.txt` with their checksums.
Release candidates install the same way as Not-Everything Preview, from
`Not-Everything-Preview-<label>-macos-arm64.dmg`.

## Run from source (macOS 13 or later)

```sh
cargo build --release -p fx-ui-macos -p fx-cli
target/release/fx-ui-macos                                  # index / and open the search window
target/release/not-everything -n 20 report pdf              # search that saved index from a shell
target/release/fx-ui-macos --root ~/Projects                # or index one folder
target/release/not-everything -root ~/Projects report pdf   # and search that folder's index
```

Started this way the app is a bare binary, not an app bundle: Launch at Login is unavailable,
and macOS gives it the Full Disk Access of the terminal you start it from. For a real app
bundle, wrap the binary with `make_bundle` and open the result (it never replaces a bundle,
so pick a new folder for each build):

```sh
cargo run --release -p fx-ui-macos --example make_bundle -- \
    target/app-bundle target/release/fx-ui-macos --app
open target/app-bundle/Not-Everything.app
```

The bundle is signed ad hoc, so each new build needs Full Disk Access granted again.

The first launch scans the disk (about 10 to 15 s for 2M files); later launches load the
saved index and keep it current with FSEvents. Not-Everything has a Dock icon, its own menu bar and
a menu-bar item; Option+Space shows the window, and closing the window keeps the app running
until you quit it. **Settings…** (Cmd+,) switches between light and dark and picks the Dock
icon from the approved designs. Without Full Disk Access the app
skips the folders macOS protects (Desktop, Documents, Downloads, Mail and so on) so no
consent prompts appear; grant Full Disk Access in System Settings › Privacy & Security to
index them. The `not-everything` command's options and exit codes are in
[`docs/cli.md`](docs/cli.md). Settings and indexes live in
`~/Library/Application Support/Not-Everything/`.

## Name

The app is called **Not-Everything** (bundle ID `io.3645.not-everything`). Crates keep the
`fx-` prefix (for example `fx-index`, `fx-query`, `fx-ui-macos`) and the command-line tool is
`not-everything` (the `fx-cli` crate). Release candidates and betas install as a separate
app, **Not-Everything Preview** (bundle ID `io.3645.not-everything.preview`, command
`not-everything-preview`, its own data folder and Option+Shift+Space), so testing one never
touches the release. Apart from these apps' names, bundle IDs, data folders and commands, the
notary profile `not-everything-notary` and the disk images' signing identifiers
(`io.3645.not-everything.dmg`, `io.3645.not-everything.preview.dmg`), no crate name, bundle
ID or Homebrew cask contains "Everything".

## Read-only MVP

The MVP only finds and shows files; it never changes them. There is no delete, rename, move
or trash action in the app or the CLI. The supported actions are:

- **Return** in the search field moves to the results; the arrow keys pick a file.
- **Enter** in the results opens the selected file.
- **Cmd+Enter** reveals it in Finder.
- **Space** shows it in Quick Look; Space again or Escape closes the panel.
- **Copy full path**.

Dragging a result out of the window offers copy and link only, never move or delete, so
dropping it on the Trash or a Finder folder cannot remove the file. Indexing never triggers
cloud downloads: dataless files are not materialized and cloud-only folders are not
recursed into. Network drives are out of scope until Phase 6.

## Layout

```
crates/fx-*        the crates, one per component (see docs/PLAN.md, "Proposed architecture")
docs/PLAN.md       research and build plan, split into runs
PROGRESS.md        run status and hand-off notes
CLEANROOM.md       clean-room provenance policy and per-run sources log
```

## Build and test

Requires stable Rust; `rust-toolchain.toml` pins the stable channel with `rustfmt` and
`clippy`.

```sh
cargo build --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

CI (`.github/workflows/ci.yml`) runs the same format, lint and test commands on macOS.

## Licensing

The project as a whole is licensed under the **MIT License**; see [`LICENSE`](LICENSE).
Crates inherit `license = "MIT"` from the workspace.

**Apache-2.0 subcomponents.** Apache-2.0 is allowed only for designated subcomponents, for
example code adapted from an Apache-2.0 project, or a crate where an explicit patent grant
matters. A designated subcomponent:

- keeps its Apache-2.0 license text, and any NOTICE file, in its own crate directory;
- sets `license = "MIT OR Apache-2.0"` or `license = "Apache-2.0"` in its `Cargo.toml`,
  whichever matches the code it contains;
- is listed by crate name in the first column of the table below. A policy test in
  `fx-testkit` fails for any crate that does not inherit the workspace license and is not
  listed here.

Apache-2.0 code may live inside this MIT project, but it is never relabelled as MIT.

### Designated Apache-2.0 subcomponents

| Crate or path | License | Origin | NOTICE file |
| --- | --- | --- | --- |
| _none yet_ | | | |

**Dependencies.** GPL dependencies are not allowed. The license of every new crate is
checked before it is added, and the result is noted in the pull request that adds it.
Every dependency's licence must be in the SPDX allowlist in [`deny.toml`](deny.toml);
`cargo deny check licenses` checks it, locally and in CI.

**Assets.** The app icons and the pictures in `docs/img/` are listed with their author,
source and licence in [`ATTRIBUTION.md`](ATTRIBUTION.md).

**Third-party notices.** [`THIRD-PARTY-NOTICES.md`](THIRD-PARTY-NOTICES.md) holds the licences
of the open-source crates the app and the command are built with. The app shows it in its
About panel, and `not-everything -licenses` prints it.

**Clean room.** The project follows a written clean-room policy: no decompiling or
reverse-engineering of voidtools software or its formats, no reading of GPL or
unclear-license code, and a per-run log of the sources consulted. See
[`CLEANROOM.md`](CLEANROOM.md).

**Name.** This project is not affiliated with or endorsed by voidtools. "Everything" is
used here only to describe what inspired the project.

## Contact

Questions, feedback and bug reports about Not-Everything: **ne@3645.io**. To report a
security problem, see [`SECURITY.md`](SECURITY.md).
