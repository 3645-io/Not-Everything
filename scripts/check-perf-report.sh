#!/bin/sh
# Performance report check for pull requests.
#
# CI runs this on every pull request (.github/workflows/perf-report.yml) on
# a checkout with the full history. A pull request that changes code the
# macOS app or the command ships must carry the summary line of
# `scripts/perf-check.sh --tier pr` in its description, from a run of every
# step, showing no failed and no unmeasured metric
# (.agents/rules/performance.md):
#
#     perf-compare: 27 pass, 0 fail, 0 not measured, 0 no baseline, 2 tracked, 0 skipped
#
# A run limited with --step counts its other metrics as skipped. It passes
# only beside a full run's line (0 skipped): a step that failed in the full
# run and passed when re-run alone (only a second failure counts).
#
# or a line saying the owner approved going without it:
#
#     Performance: skipped, approved by the owner (<reason>)
#
# Shipped code is the workspace manifests and lock file, and the `src/` and
# `build.rs` of every crate except the benchmark and test crates (fx-bench,
# fx-testkit) and the ports that the macOS app doesn't build (fx-ui-win32,
# fx-ui-gtk, fx-fs-windows, fx-fs-linux). Tests, examples, docs, scripts and
# CI files need no report.
#
# The description comes in $PR_BODY, never on the command line, so its text
# can't be read as shell. The script only reads the repository.

set -eu

usage() {
    echo "usage: PR_BODY=<description> scripts/check-perf-report.sh BASE HEAD" >&2
    exit 2
}

[ $# -eq 2 ] || usage
case $1 in -*) usage ;; esac
case $2 in -*) usage ;; esac
BASE=$1
HEAD_REF=$2
BODY=${PR_BODY-}
ROOT=$(cd "$(dirname "$0")/.." && pwd)

for ref in "$BASE" "$HEAD_REF"; do
    if ! git -C "$ROOT" rev-parse -q --verify "$ref^{commit}" >/dev/null; then
        echo "check-perf-report: no commit $ref; fetch it first (the CI checkout needs fetch-depth: 0)" >&2
        exit 2
    fi
done

SHIPPED=$(git -C "$ROOT" diff --name-only "$BASE...$HEAD_REF" |
    grep -E '^(Cargo\.toml|Cargo\.lock|crates/[^/]+/(Cargo\.toml|build\.rs|src/.*))$' |
    grep -v -E '^crates/(fx-bench|fx-testkit|fx-ui-win32|fx-ui-gtk|fx-fs-windows|fx-fs-linux)/' || true)

if [ -z "$SHIPPED" ]; then
    echo "check-perf-report: no shipped code changed; no performance report needed"
    exit 0
fi

echo "check-perf-report: shipped code changed:"
printf '%s\n' "$SHIPPED" | sed 's/^/  /'

FULL_RUN='perf-compare: [0-9]+ pass, [0-9]+ fail, [0-9]+ not measured, [0-9]+ no baseline, [0-9]+ tracked, 0 skipped'
PASSING='perf-compare: [0-9]+ pass, 0 fail, 0 not measured, [0-9]+ no baseline, [0-9]+ tracked, [0-9]+ skipped'
if printf '%s\n' "$BODY" | grep -q -E "$FULL_RUN" &&
    printf '%s\n' "$BODY" | grep -q -E "$PASSING"; then
    echo "check-perf-report: the description carries a full run's summary and a passing one"
    exit 0
fi
if printf '%s\n' "$BODY" | grep -q -E '^Performance: skipped, approved by the owner \(.+\)'; then
    echo "check-perf-report: the description records the owner's approval to skip the performance check"
    exit 0
fi

cat >&2 <<'EOF'
check-perf-report: this PR changes shipped code, but its description has no
passing performance summary. Run `scripts/perf-check.sh --tier pr --out <dir>`
and paste <dir>/summary.md, including its "perf-compare: ... 0 fail, 0 not
measured, ... 0 skipped" line, into the description. A step re-run alone
(--step) counts only beside the full run's summary (.agents/rules/performance.md).
EOF
exit 1
