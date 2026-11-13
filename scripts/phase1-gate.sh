#!/bin/sh
# Phase 1 gate runner.
#
# Runs every Phase 1 gate benchmark one at a time, in release builds, for one
# machine and one cache state. Each step's stdout and stderr go to
# <out>/<name>.log. The script then turns the benchmark output into the
# `metric=value` lines fx-bench reads (<out>/gate-metrics.txt) and finishes
# with `fx-bench phase1-summary`, which writes <out>/summary.md. The exit code
# is phase1-summary's: 0 when every criterion is PASS, 1 when any is FAIL or
# not measured, 2 for bad arguments or an unreadable results directory.
#
# Steps, in order (a cold pass runs the cache-sensitive ones first):
#   fx-query    matcher_bench at 2M and 5M entries (Goal 2 engine, regex,
#               full path)
#   fx-engine   startup_bench (store open to first result, memory),
#               catchup_bench (10,000 offline changes), first_run_bench
#               (scan of /, memory) and freshness_bench (1,000 operations)
#   fx-ui-macos --gate-probe, 20 launches warm, 1 launch cold (Goal 1
#               launch to first result painted, Goal 2 keystroke to repaint)
#   fx-query    conformance tests (case count, failures)
#   fx-cli      tests (csv, json and txt output, exit codes)
#
# gate-metrics.txt also records the scale each step ran at (matcher entry
# counts, the UI probe's index size); phase1-summary reports a row measured
# below the gate scale (e.g. --quick) as NOT MEASURED, never PASS or FAIL.
#
# Safety: this script only creates the new <out> directory and files inside
# it, plus the startup_bench store and the gate probe's data folder
# (app-data) in --store-dir when that is given. The app's launches never
# open the daily app's data folder. It
# never deletes, renames or moves anything and never flushes the disk cache.
# "Cold" means the first run after a reboot; see docs/gates/phase-1.md.
#
# Benchmarks run strictly one after another, never in parallel, so no
# benchmark skews another's numbers.

set -eu

usage() {
    cat <<'EOF'
usage: scripts/phase1-gate.sh --machine LABEL --cache warm|cold --out NEW_DIR
                              [--store-dir DIR [--reuse]] [--skip-ui] [--quick]

  --machine LABEL   machine label for the summary, e.g. "M4 Pro" or "8 GB Mac"
  --cache STATE     warm, or cold (the first run after a reboot)
  --out NEW_DIR     directory for the logs and summary.md; must not exist yet
  --store-dir DIR   keep the 2M-entry startup_bench store in DIR
                    (startup_bench --store-dir DIR) and the gate probe's
                    index in DIR/app-data, for the cold pass
  --reuse           time the store already kept in --store-dir as is
                    (startup_bench --reuse); a cold pass needs it, because
                    only a store written before the reboot is really cold.
                    A cold pass without a kept store runs no startup_bench
                    and records why
  --skip-ui         skip the fx-ui-macos gate probe (no window server, or
                    not macOS); Goal 1 and keystroke are then not measured
  --quick           small entry counts for CI and smoke runs (20k/50k
                    matcher entries, 20k startup entries, 1,000 offline
                    changes, 100 freshness operations, 3 launches, the
                    repository's crates/ folder instead of /); never gate
                    evidence

Run the warm pass first (it builds the release binaries and, with
--store-dir, writes the startup store), reboot, then run the cold pass with
the same --store-dir and --reuse as the first thing after logging in. The
script never runs sudo or flushes caches itself.
EOF
}

die_usage() {
    echo "phase1-gate: $1" >&2
    usage >&2
    exit 2
}

# ---------------------------------------------------------------------------
# Arguments: validated completely before anything is built or created.
# ---------------------------------------------------------------------------

MACHINE=""
CACHE=""
OUT=""
STORE_DIR=""
REUSE=0
SKIP_UI=0
QUICK=0

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
        --reuse)
            REUSE=1
            shift
            ;;
        --skip-ui)
            SKIP_UI=1
            shift
            ;;
        --quick)
            QUICK=1
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
if [ "$REUSE" -eq 1 ] && [ -z "$STORE_DIR" ]; then
    die_usage "--reuse needs --store-dir"
fi
if [ -e "$OUT" ] || [ -L "$OUT" ]; then
    echo "phase1-gate: '$OUT' already exists; pass a new directory (nothing was changed)" >&2
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

OS=$(uname -s)
if [ "$OS" != "Darwin" ] && [ "$SKIP_UI" -eq 0 ]; then
    SKIP_UI=1
    UI_SKIP_REASON="not macOS ($OS)"
else
    UI_SKIP_REASON="--skip-ui"
fi

# Scale: the gate values, or small counts with --quick.
ENTRIES_2M=2000000
ENTRIES_5M=5000000
MATCHER_FLAGS=""
STARTUP_ENTRIES=2000000
STARTUP_RUNS=20
CATCHUP_ENTRIES=50000
CATCHUP_CHANGES=10000
FRESH_OPS=1000
LAUNCHES=20
SCAN_ROOT=/
if [ "$QUICK" -eq 1 ]; then
    ENTRIES_2M=20000
    ENTRIES_5M=50000
    MATCHER_FLAGS="--quick"
    STARTUP_ENTRIES=20000
    STARTUP_RUNS=3
    CATCHUP_ENTRIES=2000
    CATCHUP_CHANGES=1000
    FRESH_OPS=100
    LAUNCHES=3
    SCAN_ROOT="$ROOT/crates"
fi
if [ "$CACHE" = cold ]; then
    # Only the first open and the first launch after a reboot are cold; any
    # later one would be warm. fx-bench phase1-summary judges a cold pass by
    # its worst launch_ms sample, so the one cold launch always counts.
    STARTUP_RUNS=1
    LAUNCHES=1
fi

PARENT=$(dirname "$OUT")
mkdir -p "$PARENT"
mkdir "$OUT"
if [ -n "$STORE_DIR" ]; then
    mkdir -p "$STORE_DIR"
fi

# Script-generated lines (exit codes, skips) in the
# `GATE <step>: PASS|FAIL|SKIP (<detail>)` format.
CHECKS="$OUT/checks.log"
: >"$CHECKS"

check() {
    printf 'GATE %s: %s\n' "$1" "$2" >>"$CHECKS"
}

note() {
    printf '%s\n' "$*" >>"$OUT/run.txt"
    printf 'phase1-gate: %s\n' "$*" >&2
}

skip() {
    check "$1" "SKIP ($2)"
    note "skipped $1: $2"
}

# run_step NAME CMD...: runs CMD once, appending stdout and stderr to
# <out>/NAME.log, records its exit code in checks.log and leaves it in
# STEP_STATUS. A failing step does not stop the run.
STEP_STATUS=0
run_step() {
    name=$1
    shift
    log="$OUT/$name.log"
    note "running $name: $*"
    printf '# phase1-gate: %s\n' "$*" >>"$log"
    STEP_STATUS=0
    "$@" >>"$log" 2>&1 || STEP_STATUS=$?
    if [ "$STEP_STATUS" -eq 0 ]; then
        check "$name exit code" "PASS (exit 0)"
    else
        check "$name exit code" "FAIL (exit $STEP_STATUS, see $name.log)"
    fi
    return 0
}

{
    echo "machine: $MACHINE"
    echo "cache: $CACHE"
    echo "date: $(date -u '+%Y-%m-%dT%H:%M:%SZ')"
    echo "os: $OS $(uname -r)"
    echo "macOS: $(sw_vers -productVersion 2>/dev/null || echo unknown)"
    echo "model: $(sysctl -n hw.model 2>/dev/null || echo unknown)"
    echo "cpu: $(sysctl -n machdep.cpu.brand_string 2>/dev/null || echo unknown)"
    echo "memory bytes: $(sysctl -n hw.memsize 2>/dev/null || echo unknown)"
    echo "uptime: $(uptime)"
    echo "git: $(git rev-parse HEAD 2>/dev/null || echo unknown)"
    echo "rustc: $(rustc --version 2>/dev/null || echo unknown)"
    echo "quick: $QUICK, skip-ui: $SKIP_UI, store-dir: ${STORE_DIR:-none}, reuse: $REUSE"
} >"$OUT/run.txt"

# ---------------------------------------------------------------------------
# Build once, in release. On a cold pass the warm pass has already built
# everything, so this is a no-op freshness check. The app's `--gate-probe`
# exists only with fx-ui-macos's `measure` feature, which release builds of
# the app leave off.
# ---------------------------------------------------------------------------

note "building in release"
if ! cargo build --release --locked --workspace --bins --examples --features fx-ui-macos/measure >"$OUT/build.log" 2>&1; then
    echo "phase1-gate: release build failed; see $OUT/build.log" >&2
    exit 1
fi

# ---------------------------------------------------------------------------
# Steps, one at a time.
# ---------------------------------------------------------------------------

matcher() {
    # Goal 2: engine p95 <= 16 ms at 2M and <= 30 ms at 5M; regex and full
    # path p95 <= 100 ms at 2M.
    # shellcheck disable=SC2086
    run_step matcher_bench_2m "$EX/matcher_bench" --entries "$ENTRIES_2M" --cache "$CACHE" $MATCHER_FLAGS
    # shellcheck disable=SC2086
    run_step matcher_bench_5m "$EX/matcher_bench" --entries "$ENTRIES_5M" --cache "$CACHE" $MATCHER_FLAGS
}

startup() {
    # Store open to first result and the memory report. A cold number needs
    # a store written before the reboot and reused as is.
    store_file="$STORE_DIR/startup-$STARTUP_ENTRIES.fxdb"
    if [ -z "$STORE_DIR" ] && [ "$CACHE" = warm ]; then
        run_step startup_bench "$EX/startup_bench" --entries "$STARTUP_ENTRIES" --runs "$STARTUP_RUNS" --cache warm
    elif [ -z "$STORE_DIR" ]; then
        skip "startup_bench cold" "no --store-dir: a cold startup number needs a store written before the reboot; run the warm pass with --store-dir DIR, then the cold pass with --store-dir DIR --reuse"
    elif [ "$REUSE" -eq 1 ] && [ -f "$store_file" ]; then
        run_step startup_bench "$EX/startup_bench" --entries "$STARTUP_ENTRIES" --runs "$STARTUP_RUNS" --store-dir "$STORE_DIR" --reuse --cache "$CACHE"
    elif [ "$CACHE" = warm ]; then
        run_step startup_bench "$EX/startup_bench" --entries "$STARTUP_ENTRIES" --runs "$STARTUP_RUNS" --store-dir "$STORE_DIR" --cache warm
    else
        # Writing a new store now would page-cache it; never time it as cold.
        skip "startup_bench cold" "no kept store $store_file (or no --reuse): run the warm pass with the same --store-dir first, then the cold pass with --reuse"
    fi
}

catchup() {
    # 10,000 changes while quit; index equals a fresh scan within 10 s,
    # search usable within 2 s.
    run_step catchup_bench "$EX/catchup_bench" --entries "$CATCHUP_ENTRIES" --changes "$CATCHUP_CHANGES" --cache "$CACHE"
}

first_run() {
    # First run: ~2M entries (the whole of /) in <= 15 s warm, progressive.
    run_step first_run_bench "$EX/first_run_bench" --root "$SCAN_ROOT" --cache "$CACHE"
}

freshness() {
    # Goal 3: shown in an open result list <= 1 s p99 over 1,000 operations.
    run_step freshness_bench "$EX/freshness_bench" --ops "$FRESH_OPS" --cache "$CACHE"
}

ui_probe() {
    # Goal 1 (launch to first result painted, p95 over 20 launches) and
    # keystroke to repaint, over the real index for the scan root. Each
    # launch writes its own report. Warm: one discarded warm-up launch
    # first (it also creates the index if there is none). Cold: no warm-up
    # and LAUNCHES=1, so the one launch is the cold launch after the reboot.
    if [ "$SKIP_UI" -eq 1 ]; then
        skip "fx-ui-macos gate probe" "$UI_SKIP_REASON"
        return 0
    fi
    # The launches keep their index and settings in a data folder of their
    # own (--data-dir), never the daily app's, and leave its shortcut alone.
    # With --store-dir the folder is kept there, so the cold pass after the
    # reboot opens the index the warm pass wrote.
    if [ -n "$STORE_DIR" ]; then
        app_data="$STORE_DIR/app-data"
    else
        app_data="$OUT/app-data"
    fi
    if [ "$CACHE" = cold ] && [ ! -d "$app_data" ]; then
        skip "fx-ui-macos gate probe cold" "no kept index in $app_data: run the warm pass with the same --store-dir first"
        return 0
    fi
    mkdir -p "$app_data"
    mkdir "$OUT/gate_probe"
    if [ "$CACHE" = warm ]; then
        run_step gate_probe_warmup "$BIN/fx-ui-macos" --root "$SCAN_ROOT" --data-dir "$app_data" --gate-probe "$OUT/gate_probe/warmup.txt"
    fi
    ok=0
    i=1
    while [ "$i" -le "$LAUNCHES" ]; do
        status=0
        printf '# phase1-gate: launch %s of %s\n' "$i" "$LAUNCHES" >>"$OUT/gate_probe.log"
        "$BIN/fx-ui-macos" --root "$SCAN_ROOT" --data-dir "$app_data" --gate-probe "$OUT/gate_probe/launch-$i.txt" >>"$OUT/gate_probe.log" 2>&1 || status=$?
        if [ "$status" -eq 0 ] && [ -f "$OUT/gate_probe/launch-$i.txt" ]; then
            ok=$((ok + 1))
        fi
        i=$((i + 1))
    done
    note "fx-ui-macos --gate-probe: $ok of $LAUNCHES launches wrote a report"
    if [ "$ok" -eq "$LAUNCHES" ]; then
        check "fx-ui-macos gate probe, $LAUNCHES launches" "PASS ($ok/$LAUNCHES, see gate_probe/)"
    else
        check "fx-ui-macos gate probe, $LAUNCHES launches" "FAIL ($ok/$LAUNCHES wrote a report, see gate_probe.log)"
    fi
}

CONFORMANCE_STATUS=0
conformance() {
    # Syntax: >= 300 conformance cases pass 100%. Every
    # crates/fx-query/tests/conformance*.rs suite runs.
    targets=""
    for f in crates/fx-query/tests/conformance*.rs; do
        [ -f "$f" ] || continue
        targets="$targets --test $(basename "$f" .rs)"
    done
    # shellcheck disable=SC2086
    run_step conformance cargo test --release --locked -p fx-query $targets
    CONFORMANCE_STATUS=$STEP_STATUS
}

CLI_STATUS=0
cli() {
    # The CLI: csv, json and txt output and the documented exit codes.
    run_step fx_cli_tests cargo test --release --locked -p fx-cli
    CLI_STATUS=$STEP_STATUS
}

if [ "$CACHE" = cold ]; then
    # The most cache-sensitive measurements come first after the reboot.
    ui_probe
    startup
    first_run
    matcher
    catchup
    freshness
else
    matcher
    startup
    catchup
    first_run
    freshness
    ui_probe
fi
conformance
cli

# ---------------------------------------------------------------------------
# Gate metrics: the benchmark output as the metric=value lines fx-bench
# phase1-summary reads (see crates/fx-bench/src/gate1.rs).
# ---------------------------------------------------------------------------

METRICS="$OUT/gate-metrics.txt"
: >"$METRICS"

emit() {
    # emit KEY VALUE...: one KEY=VALUE line per non-empty value.
    key=$1
    shift
    for v in "$@"; do
        [ -n "$v" ] && printf '%s=%s\n' "$key" "$v" >>"$METRICS"
    done
    return 0
}

log_of() {
    if [ -f "$OUT/$1.log" ]; then
        printf '%s\n' "$OUT/$1.log"
    else
        printf '%s\n' /dev/null
    fi
}

# matcher_bench: the worst p95 of each budget group, from its GATE lines.
worst_p95() {
    sed -n "s/^GATE $1 p95 <=.*(worst p95 \\([0-9.]*\\) ms.*/\\1/p" "$2"
}
emit engine_2m_ms $(worst_p95 engine "$(log_of matcher_bench_2m)")
emit regex_2m_ms $(worst_p95 regex "$(log_of matcher_bench_2m)")
emit fullpath_2m_ms $(worst_p95 full-path "$(log_of matcher_bench_2m)")
emit engine_5m_ms $(worst_p95 engine "$(log_of matcher_bench_5m)")

# Scale: the entry counts matcher_bench ran at, so phase1-summary can mark a
# row measured below the gate scale (e.g. --quick) as NOT MEASURED. The
# fx-engine benches print their own scale (startup_entries,
# catchup_changes, first_run_entries, freshness_ops) into their logs.
if [ -f "$OUT/matcher_bench_2m.log" ]; then
    emit engine_2m_entries "$ENTRIES_2M"
    emit regex_2m_entries "$ENTRIES_2M"
    emit fullpath_2m_entries "$ENTRIES_2M"
fi
if [ -f "$OUT/matcher_bench_5m.log" ]; then
    emit engine_5m_entries "$ENTRIES_5M"
fi

# Memory: the snapshot report of startup_bench and first_run_bench.
emit bytes_per_entry $(sed -n 's/^memory_bytes_per_entry=//p' "$(log_of startup_bench)" "$(log_of first_run_bench)")

# catchup_bench.
CATCHUP_LOG=$(log_of catchup_bench)
emit catchup_equal $(sed -n 's/^equal_to_fresh_scan=//p' "$CATCHUP_LOG")
emit catchup_ms $(sed -n 's/^equal_ms=//p' "$CATCHUP_LOG")
emit catchup_usable_ms $(sed -n 's/^search_usable_ms=//p' "$CATCHUP_LOG")

# first_run_bench: scan seconds to milliseconds.
emit first_run_ms $(sed -n 's/^scan_seconds=//p' "$(log_of first_run_bench)" | awk '{ printf "%.3f\n", $1 * 1000 }')

# freshness_bench: the overall p99, only when no operation was missed (a
# missed operation leaves Goal 3 not measured, never passed).
emit freshness_ms $(sed -n 's/^all: .* p99 \([0-9.]*\) ms,.*missed 0)$/\1/p' "$(log_of freshness_bench)")

# Gate probe: one launch sample, one keystroke p95 and the index size it
# ran over (ui_entries, the Goal 1 scale) per report.
if [ -d "$OUT/gate_probe" ]; then
    for r in "$OUT"/gate_probe/launch-*.txt; do
        [ -f "$r" ] || continue
        emit ui_entries $(sed -n 's/^fx-ui-macos gate probe: \([0-9][0-9]*\) entries$/\1/p' "$r")
        emit launch_ms $(sed -n 's/^GATE ui launch to first result painted[^(]*(\([0-9.]*\) ms,.*/\1/p' "$r")
        emit keystroke_ms $(sed -n 's/^GATE ui keystroke to repaint[^(]*(p95=\([0-9.]*\)ms.*/\1/p' "$r")
    done
fi

# Syntax: case entries in the conformance suites (calls to `case(`, not the
# `fn case(` definition or comment lines), and the failures.
CASES=$(find crates/fx-query/tests -path 'crates/fx-query/tests/conformance*' -name '*.rs' -type f -exec cat {} + |
    grep -v '^[[:space:]]*//' | grep -v 'fn case(' | grep -oE '(^|[^A-Za-z0-9_])case\(' | wc -l | tr -d ' ')
emit syntax_cases "$CASES"
if [ "$CONFORMANCE_STATUS" -eq 0 ]; then
    emit syntax_failures 0
else
    # The suites report "<n> problem(s) in <m> cases"; at least 1 otherwise.
    FAILS=$(sed -n 's/.* \([0-9][0-9]*\) problem(s) in [0-9]* cases.*/\1/p' "$(log_of conformance)" | awk '{ s += $1 } END { print s + 0 }')
    [ "$FAILS" -gt 0 ] || FAILS=1
    emit syntax_failures "$FAILS"
fi

# CLI.
if [ "$CLI_STATUS" -eq 0 ]; then emit cli_ok true; else emit cli_ok false; fi

# ---------------------------------------------------------------------------
# Summary.
# ---------------------------------------------------------------------------

note "writing summary.md"
status=0
"$BIN/fx-bench" phase1-summary --machine "$MACHINE" --cache "$CACHE" "$OUT" >"$OUT/phase1-summary.out" 2>&1 || status=$?
if [ -f "$OUT/summary.md" ]; then
    {
        echo
        echo "## Run checks"
        echo
        echo "Step exit codes and skips from scripts/phase1-gate.sh (checks.log):"
        echo
        sed 's/^/- /' "$CHECKS"
        if [ "$QUICK" -eq 1 ]; then
            echo
            echo "Quick run (--quick): small entry counts, not gate evidence."
        fi
    } >>"$OUT/summary.md"
fi
note "phase1-summary exit code: $status (summary in $OUT/summary.md)"
exit "$status"
