#!/bin/sh
# Writes THIRD-PARTY-NOTICES.md: the notices of the open-source crates the
# app and the command are built with, which their licences ask to be kept
# with every copy. cargo-about makes them from Cargo.lock with about.toml
# and about.hbs, offline (--frozen), so the licence texts come from the
# crates themselves and every run writes the same file; `cargo fetch
# --locked` first brings the locked crates into cargo's cache, on a fresh
# machine too. Run it after any dependency change and commit the result.
#
#   scripts/third-party-notices.sh           write THIRD-PARTY-NOTICES.md
#   scripts/third-party-notices.sh --check   write nothing; fail if the file
#                                            differs from what it would write
#
# Cargo.lock also locks optional dependencies of other crates that no build
# turns on. They ship in nothing, so they need no notice; the file lists
# them under "Locked but not built", after `cargo tree` confirms that no
# feature on any target builds them, so every crate in Cargo.lock is
# accounted for (crates/fx-testkit/tests/third_party_notices.rs).
#
# The release script runs --check before building, so a release never
# ships stale notices. Needs cargo-about at the pinned version below
# (another version words or orders the file differently, so --check would
# fail on current notices):
#   cargo install cargo-about --locked --features cli --version 0.9.3

set -eu

ABOUT_VERSION=0.9.3

fail() {
    echo "third-party-notices: $*" >&2
    exit 1
}

case ${1:-} in
"" | --check) ;;
*)
    echo "usage: scripts/third-party-notices.sh [--check]" >&2
    exit 2
    ;;
esac

ROOT=$(cd "$(dirname "$0")/.." && pwd)
OUT=$ROOT/THIRD-PARTY-NOTICES.md
INSTALL="cargo install cargo-about --locked --features cli --version $ABOUT_VERSION"
# Ask cargo, not the shell: cargo finds cargo-about in ~/.cargo/bin even
# when that folder isn't on PATH.
about=$(cargo about --version 2>/dev/null) || about=
[ -n "$about" ] ||
    fail "cargo-about is not installed (cargo about --version failed): $INSTALL"
[ "$about" = "cargo-about $ABOUT_VERSION" ] ||
    fail "the notices are written with cargo-about $ABOUT_VERSION, not $about: $INSTALL"
cd "$ROOT"
cargo fetch --locked --quiet ||
    fail "cargo fetch --locked failed, so the locked crates' licences can't be read; check Cargo.lock and the network"

# The crates.io packages in Cargo.lock, as "name version", sorted. A
# package's name and version come before its source; workspace crates have
# no source.
locked() {
    awk '
        /^\[\[package\]\]/ { name = ""; version = "" }
        /^name = /    { gsub(/"/, "", $3); name = $3 }
        /^version = / { gsub(/"/, "", $3); version = $3 }
        /^source = /  { print name " " version }
    ' Cargo.lock | sort -u
}

# The whole file, on standard output.
notices() {
    text=$(cargo about generate --frozen --fail -c about.toml about.hbs) || return 1
    listed=$(printf '%s\n' "$text" | sed -n 's/^- `\(.*\)`$/\1/p' | sort -u)
    unbuilt=$(locked | while read -r name version; do
        printf '%s\n' "$listed" | grep -qxF "$name $version" || echo "$name $version"
    done)
    printf '%s\n' "$text"
    [ -n "$unbuilt" ] || return 0
    printf '\n## Locked but not built\n\n'
    printf '%s\n\n' "\`Cargo.lock\` also holds these optional dependencies of other crates. No build of Not-Everything turns them on, so they ship in nothing and need no notice:"
    printf '%s\n' "$unbuilt" | while read -r name version; do
        # A crate cargo-about left out must really be unreachable: with every
        # feature, on every target, as a normal, build or dev dependency.
        # cargo tree prints nothing, and succeeds, for an unreachable crate;
        # an error is not taken for "not built".
        built=$(cargo tree -q --frozen -i "$name@$version" --all-features --target all \
            -e normal,build,dev 2>/dev/null) ||
            fail "cargo tree could not check $name $version"
        [ -z "$built" ] ||
            fail "$name $version is built but has no notice; check about.toml"
        echo "- \`$name $version\`"
    done
}

# The whole text first: a failure anywhere (in a pipeline's subshell too)
# fails this assignment, so THIRD-PARTY-NOTICES.md is never left half
# written. Both modes compare or write the same bytes.
text=$(notices) || fail "could not write the notices"

case ${1:-} in
"")
    printf '%s\n' "$text" >"$OUT"
    ;;
--check)
    printf '%s\n' "$text" | cmp -s - "$OUT" ||
        fail "THIRD-PARTY-NOTICES.md is out of date; run scripts/third-party-notices.sh and commit it"
    ;;
esac
