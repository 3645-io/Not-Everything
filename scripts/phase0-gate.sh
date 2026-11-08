#!/bin/sh
# Phase 0 gate runner.
#
# Runs every Phase 0 gate benchmark one at a time for one machine and one
# cache state, captures each benchmark's stdout and stderr in
# <out>/<name>.log, and finishes with `fx-bench gate-summary` into
# <out>/summary.md. The exit code is gate-summary's: 0 when every gate row
# is PASS, 1 when any row is FAIL, SKIP, UNPARSED or MISSING, 2 for bad
# arguments or an unreadable log.
#
# Safety: this script only creates the new <out> directory and files inside
# it. It never deletes, renames or moves anything, and it never flushes the
# disk cache. "Cold" means the first run after a reboot; see
# docs/gates/phase-0.md for the procedure and for the weaker, separately
# labelled `sudo purge` approximation, which this script does not perform.
#
# Benchmarks run strictly one after another, never in parallel, so no
# benchmark skews another's numbers.

set -eu

usage() {
    cat <<'EOF'
usage: scripts/phase0-gate.sh --machine LABEL --cache warm|cold --out NEW_DIR
                              [--skip-ui] [--skip-scan] [--store-dir DIR]

  --machine LABEL   machine label for the summary, e.g. "M4 Pro" or "8 GB Mac"
  --cache STATE     warm, or cold (the first run after a reboot)
  --out NEW_DIR     directory for the logs and summary.md; must not exist yet
  --skip-ui         skip the AppKit benchmarks (table_spike, make_bundle);
                    their gates are reported as SKIP
  --skip-scan       skip the whole-volume scan_bench; its gates are
                    reported as SKIP
  --store-dir DIR   keep the 5M-entry load_bench image in DIR on a warm run
                    (load_bench --dir DIR --keep), and on a cold run time that
                    kept image (load_bench --dir DIR --reuse --iters 1), so
                    the store number after a reboot is really cold; a cold run
                    without a kept image runs no load_bench and reports SKIP

Run the warm pass first (it builds the release binaries), reboot, then run
the cold pass as the first thing after logging in. The script never runs
sudo or flushes caches itself.
EOF
}

die_usage() {
    echo "phase0-gate: $1" >&2
    usage >&2
    exit 2
}

# ---------------------------------------------------------------------------
# Arguments: validated completely before anything is built or created.
# ---------------------------------------------------------------------------

MACHINE=""
CACHE=""
OUT=""
SKIP_UI=0
SKIP_SCAN=0
STORE_DIR=""

while [ $# -gt 0 ]; do
    case "$1" in
        --machine | --cache | --out | --store-dir)
            [ $# -ge 2 ] || die_usage "$1 needs a value"
            case "$1" in
                --machine) MACHINE=$2 ;;
                --cache) CACHE=$2 ;;
                --out) OUT=$2 ;;
                --store-dir) STORE_DIR=$2 ;;
            esac
            shift 2
            ;;
        --skip-ui)
            SKIP_UI=1
            shift
            ;;
        --skip-scan)
            SKIP_SCAN=1
            shift
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            die_usage "unknown argument '$1'"
            ;;
    esac
done

[ -n "$MACHINE" ] || die_usage "--machine is required"
case "$CACHE" in
    warm | cold) ;;
    "") die_usage "--cache is required (warm or cold)" ;;
    *) die_usage "--cache must be warm or cold, got '$CACHE'" ;;
esac
[ -n "$OUT" ] || die_usage "--out is required"
if [ -e "$OUT" ] || [ -L "$OUT" ]; then
    echo "phase0-gate: '$OUT' already exists; pass a new directory (nothing was changed)" >&2
    exit 2
fi
if [ "$(uname -s)" != "Darwin" ]; then
    echo "phase0-gate: the Phase 0 gate benchmarks need macOS" >&2
    exit 2
fi

# Absolute paths, resolved against the caller's directory.
case "$OUT" in
    /*) ;;
    *) OUT="$PWD/$OUT" ;;
esac
if [ -n "$STORE_DIR" ]; then
    case "$STORE_DIR" in
        /*) ;;
        *) STORE_DIR="$PWD/$STORE_DIR" ;;
    esac
fi

ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"
TARGET=${CARGO_TARGET_DIR:-target}
case "$TARGET" in
    /*) ;;
    *) TARGET="$ROOT/$TARGET" ;;
esac
BIN="$TARGET/release"
EX="$BIN/examples"

PARENT=$(dirname "$OUT")
mkdir -p "$PARENT"
mkdir "$OUT"

# Script-generated gate lines (exit codes, checksum match, skips) go here,
# in the `GATE <criterion>: PASS|FAIL|SKIP (<detail>)` format.
CHECKS="$OUT/checks.log"
: >"$CHECKS"
# Logs that carry gate lines, handed to gate-summary at the end.
GATE_LOGS=""

check() {
    printf 'GATE %s: %s\n' "$1" "$2" >>"$CHECKS"
}

note() {
    printf '%s\n' "$*" >>"$OUT/run.txt"
    printf 'phase0-gate: %s\n' "$*" >&2
}

# run_step NAME GATED CMD...: runs CMD once, appending stdout and stderr to
# <out>/NAME.log, and records its exit code in checks.log. GATED=1 adds the
# log to the gate-summary inputs. A failing benchmark does not stop the run.
run_step() {
    name=$1
    gated=$2
    shift 2
    log="$OUT/$name.log"
    note "running $name: $*"
    printf '# phase0-gate: %s\n' "$*" >>"$log"
    status=0
    "$@" >>"$log" 2>&1 || status=$?
    if [ "$status" -eq 0 ]; then
        check "$name exit code" "PASS (exit 0)"
    else
        check "$name exit code" "FAIL (exit $status, see $name.log)"
    fi
    if [ "$gated" -eq 1 ]; then
        case " $GATE_LOGS " in
            *" $name.log "*) ;;
            *) GATE_LOGS="$GATE_LOGS $name.log" ;;
        esac
    fi
    return 0
}

skip() {
    check "$1" "SKIP ($2)"
    note "skipped $1: $2"
}

{
    echo "machine: $MACHINE"
    echo "cache: $CACHE"
    echo "date: $(date -u '+%Y-%m-%dT%H:%M:%SZ')"
    echo "macOS: $(sw_vers -productVersion 2>/dev/null || echo unknown)"
    echo "model: $(sysctl -n hw.model 2>/dev/null || echo unknown)"
    echo "cpu: $(sysctl -n machdep.cpu.brand_string 2>/dev/null || echo unknown)"
    echo "memory bytes: $(sysctl -n hw.memsize 2>/dev/null || echo unknown)"
    echo "uptime: $(uptime)"
    echo "git: $(git rev-parse HEAD 2>/dev/null || echo unknown)"
    echo "rustc: $(rustc --version 2>/dev/null || echo unknown)"
    echo "skip-ui: $SKIP_UI, skip-scan: $SKIP_SCAN, store-dir: ${STORE_DIR:-none}"
} >"$OUT/run.txt"

# ---------------------------------------------------------------------------
# Build once, in release. On a cold pass the warm pass has already built
# everything, so this is a no-op freshness check. `table_spike` exists only
# with fx-ui-macos's `measure` feature, which release builds of the app
# leave off.
# ---------------------------------------------------------------------------

note "building in release"
if ! cargo build --release --locked --workspace --bins --examples --features fx-ui-macos/measure >"$OUT/build.log" 2>&1; then
    echo "phase0-gate: release build failed; see $OUT/build.log" >&2
    exit 1
fi

# ---------------------------------------------------------------------------
# Benchmarks, one at a time.
# ---------------------------------------------------------------------------

ui_launch() {
    # Launch to first paint, 20 launches, as docs/gates/phase-0.md asks.
    # Warm: one discarded warm-up launch first. Cold: no warm-up, so the first of the
    # 20 is the cold launch after the reboot.
    if [ "$CACHE" = warm ]; then
        run_step table_spike_launch_warmup 0 "$EX/table_spike" --launch-probe
    fi
    ok=0
    i=1
    while [ "$i" -le 20 ]; do
        status=0
        "$EX/table_spike" --launch-probe --json >>"$OUT/table_spike_launch.log" 2>&1 || status=$?
        if [ "$status" -eq 0 ]; then
            ok=$((ok + 1))
        fi
        i=$((i + 1))
    done
    note "table_spike --launch-probe: $ok of 20 launches exited 0"
    if [ "$ok" -eq 20 ]; then
        check "table_spike launch to first paint measured, 20 launches" "PASS (20/20, see table_spike_launch.log)"
    else
        check "table_spike launch to first paint measured, 20 launches" "FAIL ($ok/20 exited 0)"
    fi
}

ui_scroll_and_bundle() {
    # 2M-row autoscroll, 600 frames, three runs each at 5 rows per frame
    # (smooth) and 3,000 rows per frame (fast); every run prints a GATE line.
    for run in 1 2 3; do
        run_step table_spike_autoscroll_5rows 1 "$EX/table_spike" --rows 2000000 --autoscroll 600
    done
    for run in 1 2 3; do
        run_step table_spike_autoscroll_3000rows 1 "$EX/table_spike" --rows 2000000 --autoscroll 600 --rows-per-frame 3000
    done
    # .app size: make_bundle writes <out>/bundle/Table Spike.app.
    mkdir "$OUT/bundle"
    run_step make_bundle 0 "$EX/make_bundle" "$OUT/bundle" "$EX/table_spike"
    du -sk "$OUT/bundle/Table Spike.app" >>"$OUT/make_bundle.log" 2>&1 || true
    size=$(grep '^app size:' "$OUT/make_bundle.log" || true)
    if [ -n "$size" ]; then
        check ".app size measured" "PASS ($size)"
    else
        check ".app size measured" "FAIL (no app size line in make_bundle.log)"
    fi
}

scan() {
    if [ "$CACHE" = warm ]; then
        run_step scan_bench 1 "$EX/scan_bench" --root / --threads 1,8 --iterations 5 --warmup 1 --cache warm
    else
        # Only the first scan after a reboot is cold, so a cold pass scans
        # once with 8 threads; scan_bench reports the 1-thread gate as SKIP.
        run_step scan_bench 1 "$EX/scan_bench" --root / --threads 8 --iterations 1 --warmup 0 --cache cold
    fi
}

store() {
    # Only an image written before the reboot and reused as is may be timed
    # with --cache cold. A freshly written image is still in the page cache,
    # so a cold pass without one runs no load_bench and reports SKIP instead
    # of a warm number labelled cold.
    if [ -z "$STORE_DIR" ] && [ "$CACHE" = warm ]; then
        run_step load_bench 1 "$EX/load_bench" --count 5000000 --cache warm
    elif [ -z "$STORE_DIR" ]; then
        check "load_bench image cold" "SKIP (no --store-dir: a cold store measurement needs an image written before the reboot; pass --store-dir DIR on the warm pass and again on the cold pass, which runs load_bench --reuse)"
    elif [ "$CACHE" = warm ] && [ -f "$STORE_DIR/index.fxs" ]; then
        # An image kept earlier is timed as is, never replaced.
        run_step load_bench 1 "$EX/load_bench" --count 5000000 --dir "$STORE_DIR" --reuse --cache warm
    elif [ "$CACHE" = warm ]; then
        run_step load_bench 1 "$EX/load_bench" --count 5000000 --dir "$STORE_DIR" --keep --cache warm
    elif [ -f "$STORE_DIR/index.fxs" ]; then
        run_step load_bench 1 "$EX/load_bench" --count 5000000 --dir "$STORE_DIR" --reuse --cache cold --iters 1
    else
        # Writing a new image here would page-cache it; never time it as cold.
        check "load_bench image cold" "SKIP (no index.fxs in --store-dir: a cold store measurement needs an image written before the reboot; run the warm pass with the same --store-dir first, then the cold pass reuses it with load_bench --reuse)"
    fi
}

if [ "$CACHE" = cold ]; then
    # The most cache-sensitive measurements come first after the reboot.
    if [ "$SKIP_UI" -eq 0 ]; then ui_launch; else skip "table_spike launch probe" "--skip-ui"; fi
    if [ "$SKIP_SCAN" -eq 0 ]; then scan; else skip "scan_bench" "--skip-scan"; fi
    store
fi

# fx-bench generator: 5M names twice; the checksums must match.
run_step gen_5m_run1 0 "$BIN/fx-bench" gen --count 5000000 --seed 1
run_step gen_5m_run2 0 "$BIN/fx-bench" gen --count 5000000 --seed 1
sum1=$(sed -n 's/^checksum (FNV-1a 64): //p' "$OUT/gen_5m_run1.log")
sum2=$(sed -n 's/^checksum (FNV-1a 64): //p' "$OUT/gen_5m_run2.log")
if [ -n "$sum1" ] && [ "$sum1" = "$sum2" ]; then
    check "gen 5M names seed 1 checksums match" "PASS (FNV-1a 64 $sum1)"
else
    check "gen 5M names seed 1 checksums match" "FAIL (run 1 '$sum1', run 2 '$sum2')"
fi
run_step self_bench 0 "$BIN/fx-bench" self-bench --cache "$CACHE"

# fx-query: p95 <= 10 ms over 5M names on 8 threads.
run_step search_bench 1 "$EX/search_bench" --threads 8 --cache "$CACHE"

if [ "$CACHE" = warm ]; then
    store
    if [ "$SKIP_SCAN" -eq 0 ]; then scan; else skip "scan_bench" "--skip-scan"; fi
fi

# FSEvents: live latency p95 <= 50 ms, and exact offline replay.
run_step fsevents_latency 1 "$EX/fsevents_bench" latency --ops 1000 --cache "$CACHE"
run_step fsevents_replay 1 "$EX/fsevents_bench" replay --ops 10000

if [ "$SKIP_UI" -eq 0 ]; then
    ui_scroll_and_bundle
    if [ "$CACHE" = warm ]; then ui_launch; fi
else
    skip "table_spike autoscroll 2M rows" "--skip-ui"
    skip ".app size" "--skip-ui"
    if [ "$CACHE" = warm ]; then skip "table_spike launch probe" "--skip-ui"; fi
fi

# ---------------------------------------------------------------------------
# Summary.
# ---------------------------------------------------------------------------

note "writing summary.md"
# gate-summary reads the logs that carry gate lines plus checks.log. The
# informational logs (gen, self-bench, launch probes, make_bundle) have no
# GATE line of their own; their results reach the summary through the
# checks.log lines above instead of as MISSING rows.
cd "$OUT"
status=0
# GATE_LOGS is a space-separated list of log file names (no spaces) inside
# $OUT; word splitting is intended here.
# shellcheck disable=SC2086
"$BIN/fx-bench" gate-summary --machine "$MACHINE" --cache "$CACHE" $GATE_LOGS checks.log >summary.md || status=$?
note "gate-summary exit code: $status (summary in $OUT/summary.md)"
exit "$status"
