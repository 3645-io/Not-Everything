#!/bin/sh
# Release pull request checks.
#
# CI runs this on every pull request into main (.github/workflows/ci.yml),
# and by hand on an rc/X.Y.Z branch, on a checkout of the head commit with
# the full history and tags. It fails unless:
#
# - Version: the root Cargo.toml version at HEAD is higher than every
#   released version, the release/vX.Y.Z tags (release candidates and other
#   tags do not count). A PR that changes only docs and CI files (*.md,
#   docs/, .github/) needs no new version.
# - RC gate: HEAD is the commit of the version's latest release candidate
#   tag, release/v<version>-RC-N (made by scripts/release-macos.sh --rc
#   --publish), and contains BASE, so main gets exactly what was tested.
#
# BASE defaults to origin/main and HEAD to HEAD. The script only reads the
# repository.

set -eu

usage() {
    echo "usage: scripts/check-release-pr.sh [BASE [HEAD]]" >&2
    exit 2
}

fail() {
    echo "check-release-pr: $*" >&2
    exit 1
}

[ $# -le 2 ] || usage
case ${1-} in -*) usage ;; esac
case ${2-} in -*) usage ;; esac
BASE=${1:-origin/main}
HEAD_REF=${2:-HEAD}
ROOT=$(cd "$(dirname "$0")/.." && pwd)

git -C "$ROOT" rev-parse -q --verify "$BASE^{commit}" >/dev/null ||
    fail "no commit $BASE; fetch it first (the CI checkout needs fetch-depth: 0)"
HEAD_SHA=$(git -C "$ROOT" rev-parse -q --verify "$HEAD_REF^{commit}") ||
    fail "no commit $HEAD_REF"

VERSION=$(git -C "$ROOT" show "$HEAD_SHA:Cargo.toml" |
    sed -n 's/^version = "\(.*\)"$/\1/p' | head -n 1)
echo "$VERSION" | grep -Eq '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]?)\.(0|[1-9][0-9]?)$' ||
    fail "the Cargo version ${VERSION:-(none)} is not X.Y.Z with the minor and patch parts below 100"

# X.Y.Z as one number that sorts like the version.
key() {
    major=${1%%.*}
    rest=${1#*.}
    echo $((major * 10000 + ${rest%.*} * 100 + ${rest#*.}))
}

# Version check.
CHANGED=$(git -C "$ROOT" diff --name-only "$BASE...$HEAD_SHA") ||
    fail "cannot compare $BASE with the PR head"
CODE=$(printf '%s\n' "$CHANGED" | grep -Ev '^$|\.md$|^docs/|^\.github/' || true)
LAST=$(git -C "$ROOT" tag -l 'release/v*' |
    sed -nE 's#^release/v((0|[1-9][0-9]*)\.(0|[1-9][0-9]?)\.(0|[1-9][0-9]?))$#\1#p' |
    sort -t . -k1,1n -k2,2n -k3,3n | tail -n 1)
if [ -z "$CODE" ]; then
    echo "Version: only docs and CI files change, so $VERSION needs no bump"
elif [ -n "$LAST" ] && [ "$(key "$VERSION")" -le "$(key "$LAST")" ]; then
    fail "the version $VERSION must be higher than the last release, $LAST; bump it in Cargo.toml"
else
    echo "Version: $VERSION is higher than the last release (${LAST:-none})"
fi

# RC gate.
N=$(git -C "$ROOT" tag -l "release/v$VERSION-RC-*" |
    sed -n "s|^release/v$VERSION-RC-\([1-9][0-9]*\)\$|\1|p" | sort -n | tail -n 1)
[ -n "$N" ] ||
    fail "no release candidate of $VERSION: tag the PR head as release/v$VERSION-RC-N with scripts/release-macos.sh --rc --publish"
RC_TAG=release/v$VERSION-RC-$N
RC_SHA=$(git -C "$ROOT" rev-parse "$RC_TAG^{commit}")
[ "$RC_SHA" = "$HEAD_SHA" ] ||
    fail "the PR head $HEAD_SHA is not the latest release candidate $RC_TAG ($RC_SHA); make a new release candidate of the head"
git -C "$ROOT" merge-base --is-ancestor "$BASE" "$HEAD_SHA" ||
    fail "the PR head does not contain $BASE; merge $BASE into the release branch and make a new release candidate"
echo "RC gate: the PR head is $RC_TAG and contains $BASE"
