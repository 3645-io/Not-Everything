# Licensing

- The project is MIT (`LICENSE`). Apache-2.0 is allowed only for designated subcomponents, which keep their Apache-2.0 license text and any NOTICE file and are listed in the README. Never relabel Apache-2.0 code as MIT.
- Code from an MIT- or Apache-licensed project, which the clean-room rules allow, keeps its attribution, and `CLEANROOM.md` records the source.

## Dependencies

- No GPL dependencies. Check the license of every new crate before adding it.
- Dependencies use SPDX licence identifiers, and an expression is allowed only when every identifier is in `deny.toml`'s allowlist. `cargo deny check licenses` checks it, locally and in CI. A new identifier needs the owner's approval.

## Third-party notices

The crates' licences ask for their notices to travel with every copy of the app and the command.

- `THIRD-PARTY-NOTICES.md` holds the licences of every crate any build uses, and lists the optional dependencies `Cargo.lock` holds that no build turns on.
- A crate published without a copyright line gets its holders named in `about.hbs`.
- The app shows the notices in its About panel and bundle, `not-everything -licenses` prints them, and the CLI tarball holds them.
- After any dependency change, run `sh scripts/third-party-notices.sh` (needs cargo-about at the version the script pins: `cargo install cargo-about --locked --features cli --version 0.9.3`) and commit the result. `--check` writes nothing and fails while the file is stale; `crates/fx-testkit/tests/third_party_notices.rs` fails too, and the release script refuses to build.

## Attribution

Every asset file (anything under a crate's `assets/` or `docs/img/`) is original or suitably licensed and has a row in `ATTRIBUTION.md` (author, source, licence), added in the same change as the file (`crates/fx-testkit/tests/attribution.rs`).
