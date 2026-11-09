#!/bin/sh
# Performance check: runs one tier of the performance gate
# (docs/gates/performance.md) and compares the results with the baseline in
# docs/gates/perf-baseline.toml through `fx-bench perf-compare`.
#
# Tiers (.agents/rules/performance.md says when each one runs):
#   quick  while building: small-scale benchmarks for the crates named with
#          --only, or for all of them, in 1 to 3 minutes. Compared with the
#          quick baseline; targets are not checked at small scale.
#   pr     before a PR: every metric marked P at gate scale, about 30
#          minutes, including 10 minutes with the window hidden and 10 with
#          it showing a search, under scripted file activity (fx-bench churn).
#   rc     each release candidate: as pr, with 20 launches and 30 minutes
#          hidden and 30 showing a search with no scripted activity (the Mac
#          in daily use), plus the DMG size with --dmg.
#
# Output in <out>: one <step>.log per step, metrics.txt (the metric=value
# lines perf-compare reads), summary.md (the table that goes in the PR) and
# run.txt (machine, build, load). The exit code is perf-compare's: 0 when
# every gated metric passes, 1 when one fails or was not measured, 2 for bad
# arguments.
#
# Safety: the script creates only <out>, a lock folder and a scratch folder
# in $TMPDIR. The app's data (--data-dir) and the startup store go inside
# <out>, so the daily app's and the Preview app's data folders are never
# touched. The scripted file activity (fx-bench churn) and the quick tier's
# copy of crates/ go in the scratch folder, because an app without Full Disk
# Access skips Documents, Desktop and Downloads, where <out> may be: activity
# there would cost the app nothing. The scratch folder is left for macOS to
# clear with the rest of $TMPDIR. The script never deletes, runs sudo or
# flushes the disk cache, and it stops only processes it started.
#
# One benchmark at a time (.agents/rules/conventions.md): the lock makes a
# second run, for example an agent's working in parallel, wait for this one,
# from before the build to the end.
#
# Command substitutions passed to `emit` stay unquoted on purpose: a log can
# hold several samples, and each becomes its own metric line.
# shellcheck disable=SC2046

set -eu

usage() {
    cat <<'EOF'
usage: scripts/perf-check.sh --tier quick|pr|rc --out NEW_DIR
                             [--only CRATE]... [--step STEP]...
                             [--machine LABEL] [--cache warm|cold]
                             [--baseline FILE] [--bg-minutes N]
                             [--churn-rate N] [--query TEXT] [--dmg FILE]

  --tier TIER       quick, pr or rc (see docs/gates/performance.md)
  --out NEW_DIR     directory for the logs and summary.md; must not exist yet
  --only CRATE      quick tier: run only the steps for this crate (repeatable),
                    e.g. --only fx-query. Unmeasured metrics are then SKIPPED
  --step STEP       run only this step (repeatable), to re-run one benchmark
                    alone: matcher, startup, catchup, freshness, first-run,
                    cli, gate-probe, summon, sort, background, dmg
  --machine LABEL   machine label for run.txt (default: the CPU and memory)
  --cache STATE     warm (default) or cold (the first run after a reboot)
  --baseline FILE   default docs/gates/perf-baseline.toml
  --bg-minutes N    minutes per background phase (default 10; 30 at rc)
  --churn-rate N    scripted file operations a second during the background
                    phases (default 20; 0 at rc, where real use is measured)
  --query TEXT      the search shown in the "showing a search" phase
                    (default "report")
  --dmg FILE        rc tier: the release candidate's DMG, for its size

Run it from a quiet Mac: plugged in, nothing heavy running. cargo must be on
PATH. The script never runs sudo or flushes caches itself.
EOF
}

die_usage() {
    echo "perf-check: $1" >&2
    usage >&2
    exit 2
}

# ---------------------------------------------------------------------------
# Arguments: validated completely before anything is built or created.
# ---------------------------------------------------------------------------

TIER=""
OUT=""
ONLY=""
STEPS=""
MACHINE=""
CACHE=warm
BASELINE=""
BG_MINUTES=""
CHURN_RATE=""
QUERY=report
DMG=""

while [ $# -gt 0 ]; do
    case "$1" in
        --tier | --out | --only | --step | --machine | --cache | --baseline | --bg-minutes | --churn-rate | --query | --dmg)
            [ $# -ge 2 ] || die_usage "$1 needs a value"
            case "$1" in
                --tier) TIER=$2 ;;
                --out) OUT=$2 ;;
                --only) ONLY="$ONLY $2" ;;
                --step) STEPS="$STEPS $2" ;;
                --machine) MACHINE=$2 ;;
                --cache) CACHE=$2 ;;
                --baseline) BASELINE=$2 ;;
                --bg-minutes) BG_MINUTES=$2 ;;
                --churn-rate) CHURN_RATE=$2 ;;
                --query) QUERY=$2 ;;
                --dmg) DMG=$2 ;;
            esac
            shift 2
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

case "$TIER" in
    quick | pr | rc) ;;
    "") die_usage "--tier is required (quick, pr or rc)" ;;
    *) die_usage "--tier must be quick, pr or rc, got '$TIER'" ;;
esac
case "$CACHE" in
    warm | cold) ;;
    *) die_usage "--cache must be warm or cold, got '$CACHE'" ;;
esac
[ -n "$OUT" ] || die_usage "--out is required"
if [ -n "$ONLY" ] && [ "$TIER" != quick ]; then
    die_usage "--only is for the quick tier; use --step to re-run one benchmark"
fi
is_count() {
    case "$1" in
        "" | *[!0-9]*) return 1 ;;
        *) return 0 ;;
    esac
}
if [ -n "$BG_MINUTES" ] && ! is_count "$BG_MINUTES"; then
    die_usage "--bg-minutes must be a whole number, got '$BG_MINUTES'"
fi
if [ -n "$CHURN_RATE" ] && ! is_count "$CHURN_RATE"; then
    die_usage "--churn-rate must be a whole number, got '$CHURN_RATE'"
fi
if [ -n "$DMG" ] && [ ! -f "$DMG" ]; then
    die_usage "--dmg '$DMG' is not a file"
fi
for s in $STEPS; do
    case "$s" in
        matcher | startup | catchup | freshness | first-run | cli | gate-probe | summon | sort | background | dmg) ;;
        *) die_usage "unknown --step '$s'" ;;
    esac
done
if [ -e "$OUT" ] || [ -L "$OUT" ]; then
    echo "perf-check: '$OUT' already exists; pass a new directory (nothing was changed)" >&2
    exit 2
fi
if ! command -v cargo >/dev/null 2>&1; then
    echo "perf-check: cargo is not on PATH" >&2
    exit 2
fi
if [ "$(uname -s)" != Darwin ]; then
    echo "perf-check: the gate measures the macOS app; this is $(uname -s)" >&2
    exit 2
fi

# Absolute paths, resolved against the caller's directory.
abs() {
    case "$1" in
        /*) printf '%s\n' "$1" ;;
        *) printf '%s\n' "$PWD/$1" ;;
    esac
}
OUT=$(abs "$OUT")
[ -z "$DMG" ] || DMG=$(abs "$DMG")
ROOT=$(cd "$(dirname "$0")/.." && pwd)
if [ -n "$BASELINE" ]; then
    BASELINE=$(abs "$BASELINE")
else
    BASELINE="$ROOT/docs/gates/perf-baseline.toml"
fi
[ -f "$BASELINE" ] || die_usage "no baseline file at $BASELINE"
cd "$ROOT"
TARGET=${CARGO_TARGET_DIR:-target}
TARGET=$(abs "$TARGET")
BIN="$TARGET/release"
EX="$BIN/examples"

# ---------------------------------------------------------------------------
# Scale per tier.
# ---------------------------------------------------------------------------

case "$TIER" in
    quick)
        MATCHER_ENTRIES=200000
        MATCHER_RUNS=1
        STARTUP_ENTRIES=200000
        STARTUP_RUNS=5
        CATCHUP_ENTRIES=2000
        CATCHUP_CHANGES=1000
        FRESH_OPS=100
        LAUNCHES=3
        PROBE_ROWS=200000
        # A copy of crates/ in the scratch folder (made below): the
        # repository itself may sit in a folder the app skips.
        SCAN_ROOT=quick-copy
        ;;
    pr)
        MATCHER_ENTRIES=2000000
        MATCHER_RUNS=3
        STARTUP_ENTRIES=2000000
        STARTUP_RUNS=20
        CATCHUP_ENTRIES=50000
        CATCHUP_CHANGES=10000
        FRESH_OPS=1000
        LAUNCHES=5
        PROBE_ROWS=2000000
        SCAN_ROOT=/
        : "${BG_MINUTES:=10}"
        : "${CHURN_RATE:=20}"
        ;;
    rc)
        MATCHER_ENTRIES=2000000
        MATCHER_RUNS=3
        STARTUP_ENTRIES=2000000
        STARTUP_RUNS=20
        CATCHUP_ENTRIES=50000
        CATCHUP_CHANGES=10000
        FRESH_OPS=1000
        LAUNCHES=20
        PROBE_ROWS=2000000
        SCAN_ROOT=/
        : "${BG_MINUTES:=30}"
        : "${CHURN_RATE:=0}"
        ;;
esac
BG_SECONDS=$((${BG_MINUTES:-0} * 60))

# The steps to run. The quick tier maps each crate a change touched to the
# benchmarks that cover it (.agents/rules/performance.md lists the same map).
steps_for_crate() {
    case "$1" in
        fx-query | fx-model) echo "matcher" ;;
        fx-index) echo "matcher startup freshness" ;;
        fx-engine) echo "startup catchup freshness" ;;
        fx-store) echo "startup catchup" ;;
        fx-sort) echo "sort" ;;
        fx-fs | fx-fs-macos) echo "freshness catchup" ;;
        fx-ui-macos) echo "gate-probe summon sort" ;;
        fx-cli) echo "startup cli" ;;
        *) echo "unknown" ;;
    esac
}
PARTIAL=0
if [ -n "$STEPS" ]; then
    PARTIAL=1
elif [ "$TIER" = quick ] && [ -n "$ONLY" ]; then
    PARTIAL=1
    for c in $ONLY; do
        s=$(steps_for_crate "$c")
        [ "$s" != unknown ] || die_usage "--only: no quick steps for '$c'; run the quick tier without --only"
        STEPS="$STEPS $s"
    done
elif [ "$TIER" = quick ]; then
    STEPS="matcher startup catchup freshness cli gate-probe summon sort"
else
    STEPS="matcher startup catchup freshness first-run cli gate-probe summon sort background"
    [ "$TIER" != rc ] || [ -z "$DMG" ] || STEPS="$STEPS dmg"
fi
wants() {
    case " $STEPS " in
        *" $1 "*) return 0 ;;
        *) return 1 ;;
    esac
}
if wants background && [ "$BG_SECONDS" -eq 0 ]; then
    die_usage "the background step needs --bg-minutes above 0"
fi

# ---------------------------------------------------------------------------
# The lock, then the output folder. The lock is taken before the build:
# compiling would disturb another run's measurements as much as a benchmark.
# ---------------------------------------------------------------------------

LOCK="${TMPDIR:-/tmp}/not-everything-perf.lock"
HAVE_LOCK=0
APP_PID=""
CHURN_PID=""

# shellcheck disable=SC2329 # called by the EXIT trap
cleanup() {
    # Only processes this script started, by their PID.
    for p in $CHURN_PID $APP_PID; do
        kill -TERM "$p" 2>/dev/null || true
    done
    if [ "$HAVE_LOCK" -eq 1 ]; then
        rm -f "$LOCK/pid"
        rmdir "$LOCK" 2>/dev/null || true
    fi
}
trap cleanup EXIT
trap 'exit 130' INT TERM

waited=0
while ! mkdir "$LOCK" 2>/dev/null; do
    holder=$(cat "$LOCK/pid" 2>/dev/null || true)
    if [ -n "$holder" ] && ! kill -0 "$holder" 2>/dev/null; then
        # A run that died without its trap: its PID is gone, so the lock is
        # stale. Only this script's own lock file and folder are removed.
        echo "perf-check: removing a stale lock left by PID $holder" >&2
        rm -f "$LOCK/pid"
        rmdir "$LOCK" 2>/dev/null || true
        continue
    fi
    if [ $((waited % 60)) -eq 0 ]; then
        echo "perf-check: waiting for the run holding $LOCK (PID ${holder:-unknown})" >&2
    fi
    sleep 15
    waited=$((waited + 15))
done
HAVE_LOCK=1
echo $$ >"$LOCK/pid"

mkdir -p "$(dirname "$OUT")"
mkdir "$OUT"
SCRATCH="${TMPDIR:-/tmp}/not-everything-perf-$(date -u '+%Y%m%dT%H%M%SZ')-$$"
mkdir "$SCRATCH"
if [ "$SCAN_ROOT" = quick-copy ]; then
    SCAN_ROOT="$SCRATCH/crates"
    cp -R "$ROOT/crates" "$SCAN_ROOT"
fi

note() {
    printf '%s\n' "$*" >>"$OUT/run.txt"
    printf 'perf-check: %s\n' "$*" >&2
}

{
    echo "tier: $TIER"
    echo "steps:$STEPS"
    echo "machine: ${MACHINE:-$(sysctl -n machdep.cpu.brand_string 2>/dev/null || echo unknown), $(($(sysctl -n hw.memsize 2>/dev/null || echo 0) / 1073741824)) GB}"
    echo "cache: $CACHE"
    echo "date: $(date -u '+%Y-%m-%dT%H:%M:%SZ')"
    echo "macOS: $(sw_vers -productVersion 2>/dev/null || echo unknown)"
    echo "model: $(sysctl -n hw.model 2>/dev/null || echo unknown)"
    echo "power: $(pmset -g batt 2>/dev/null | sed -n "1s/^Now drawing from '\\(.*\\)'.*/\\1/p")"
    echo "load before: $(sysctl -n vm.loadavg 2>/dev/null || echo unknown)"
    echo "git: $(git rev-parse HEAD 2>/dev/null || echo unknown)$(git diff --quiet 2>/dev/null || echo ' (uncommitted changes)')"
    echo "rustc: $(rustc --version 2>/dev/null || echo unknown)"
    echo "baseline: $BASELINE"
    echo "scratch: $SCRATCH (scan root $SCAN_ROOT)"
    if wants background; then
        echo "background: $BG_MINUTES min per phase, churn $CHURN_RATE ops/s, query '$QUERY'"
    fi
} >"$OUT/run.txt"

LOAD1=$(sysctl -n vm.loadavg 2>/dev/null | awk '{ print int($2) }')
if [ -n "$LOAD1" ] && [ "$LOAD1" -ge 4 ]; then
    note "warning: load average $LOAD1 before the run; numbers may be noisy"
fi

# ---------------------------------------------------------------------------
# Build once, in release, with the app's measurement flags (the `measure`
# feature, which release builds of the app leave off).
# ---------------------------------------------------------------------------

note "building in release"
if ! cargo build --release --locked --workspace --bins --examples --features fx-ui-macos/measure >"$OUT/build.log" 2>&1; then
    echo "perf-check: release build failed; see $OUT/build.log" >&2
    exit 2
fi

# ---------------------------------------------------------------------------
# Helpers.
# ---------------------------------------------------------------------------

METRICS="$OUT/metrics.txt"
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

# run_step NAME CMD...: runs CMD once, appending its stdout and stderr to
# <out>/NAME.log. A failing step is noted and the run goes on: its metrics
# are then missing, which perf-compare reports as NOT MEASURED.
run_step() {
    name=$1
    shift
    note "running $name: $*"
    printf '# perf-check: %s\n' "$*" >>"$OUT/$name.log"
    status=0
    "$@" >>"$OUT/$name.log" 2>&1 || status=$?
    [ "$status" -eq 0 ] || note "$name exited with $status (see $name.log)"
    return 0
}

value_of() {
    # value_of KEY FILE...: the values of KEY=... lines.
    key=$1
    shift
    sed -n "s/^$key=\\([-0-9.eE+]*\\)\$/\\1/p" "$@" 2>/dev/null || true
}

mb() {
    # Bytes to MB of 1,048,576 bytes, as Activity Monitor shows them.
    for v in "$@"; do
        awk -v b="$v" 'BEGIN { printf "%.1f\n", b / 1048576 }'
    done
}

worst_p95() {
    # matcher_bench: the worst p95 of one gate group, from its GATE line.
    sed -n "s/^GATE $1 p95 .*(worst p95 \\([0-9.]*\\) ms.*/\\1/p" "$2" 2>/dev/null || true
}

# ---------------------------------------------------------------------------
# Steps, one at a time.
# ---------------------------------------------------------------------------

step_matcher() {
    # S1, S3, S4 (and S2 at 5M entries outside the quick tier).
    i=1
    while [ "$i" -le "$MATCHER_RUNS" ]; do
        log="matcher_$i"
        run_step "$log" "$EX/matcher_bench" --entries "$MATCHER_ENTRIES" --cache "$CACHE"
        emit engine_ms $(worst_p95 engine "$OUT/$log.log")
        emit regex_ms $(worst_p95 regex "$OUT/$log.log")
        emit fullpath_ms $(worst_p95 full-path "$OUT/$log.log")
        i=$((i + 1))
    done
    emit engine_entries "$MATCHER_ENTRIES"
    if [ "$TIER" != quick ]; then
        run_step matcher_5m "$EX/matcher_bench" --entries 5000000 --cache "$CACHE"
        emit engine_5m_ms $(worst_p95 engine "$OUT/matcher_5m.log")
        emit engine_5m_entries 5000000
    fi
}

step_startup() {
    # M4 and M5; the store it keeps feeds the cli step.
    mkdir -p "$OUT/store"
    run_step startup "$EX/startup_bench" --entries "$STARTUP_ENTRIES" --runs "$STARTUP_RUNS" --store-dir "$OUT/store" --cache "$CACHE"
    emit bytes_per_entry $(value_of memory_bytes_per_entry "$OUT/startup.log")
    bytes=$(value_of store_bytes "$OUT/startup.log" | tail -1)
    entries=$(value_of startup_entries "$OUT/startup.log" | tail -1)
    if [ -n "$bytes" ] && [ -n "$entries" ] && [ "$entries" -gt 0 ]; then
        emit store_bytes_per_entry "$(awk -v b="$bytes" -v e="$entries" 'BEGIN { printf "%.1f\n", b / e }')"
    fi
    emit startup_entries "$entries"
}

step_catchup() {
    # F6 and C2.
    run_step catchup "$EX/catchup_bench" --entries "$CATCHUP_ENTRIES" --changes "$CATCHUP_CHANGES" --cache "$CACHE"
    emit catchup_ms $(value_of equal_ms "$OUT/catchup.log")
    emit catchup_usable_ms $(value_of search_usable_ms "$OUT/catchup.log")
    equal=$(sed -n -E 's/^equal_to_fresh_scan=(true|false)$/\1/p' "$OUT/catchup.log" 2>/dev/null || true)
    emit catchup_equal "$equal"
    emit catchup_changes $(value_of catchup_changes "$OUT/catchup.log")
}

step_freshness() {
    # F1 and C1.
    run_step freshness "$EX/freshness_bench" --ops "$FRESH_OPS" --cache "$CACHE"
    emit freshness_p99_ms $(sed -n 's/^all: .* p99 \([0-9.]*\) ms,.*/\1/p' "$OUT/freshness.log" 2>/dev/null || true)
    emit freshness_missed $(value_of freshness_missed "$OUT/freshness.log")
    emit freshness_ops $(value_of freshness_ops "$OUT/freshness.log")
}

step_first_run() {
    # F4, F5 and M4, scanning / with the engine alone.
    run_step first_run "$EX/first_run_bench" --root "$SCAN_ROOT" --cache "$CACHE"
    emit first_run_ms $(value_of scan_seconds "$OUT/first_run.log" | awk '{ printf "%.1f\n", $1 * 1000 }')
    emit first_result_ms $(value_of first_result_ms "$OUT/first_run.log")
    emit first_run_entries $(value_of first_run_entries "$OUT/first_run.log")
    emit bytes_per_entry $(value_of memory_bytes_per_entry "$OUT/first_run.log")
}

step_cli() {
    # O2: the command searching the store startup_bench kept, timed with
    # perl's high-resolution clock (macOS date has no sub-second format).
    store="$OUT/store/startup-$STARTUP_ENTRIES.fxdb"
    if [ ! -f "$store" ]; then
        note "cli: no store at $store (the startup step makes it); skipped"
        return 0
    fi
    note "running cli: not-everything -store <startup store> report, 1 + 5 runs"
    "$BIN/not-everything" -store "$store" report >/dev/null 2>>"$OUT/cli.log" || true
    i=1
    while [ "$i" -le 5 ]; do
        # The command's own output goes to /dev/null so only the time is
        # captured.
        emit cli_query_ms "$(perl -MTime::HiRes=time -e '
            open(my $out, ">&", \*STDOUT) or exit 1;
            open(STDOUT, ">", "/dev/null") or exit 1;
            my $t = time;
            my $rc = system(@ARGV);
            my $ms = (time - $t) * 1000;
            open(STDOUT, ">&", $out) or exit 1;
            exit 1 if $rc != 0;
            printf "%.2f\n", $ms;
        ' "$BIN/not-everything" -store "$store" report 2>>"$OUT/cli.log" || true)"
        i=$((i + 1))
    done
    emit cli_entries "$STARTUP_ENTRIES"
}

probe_lines() {
    # The key=value lines of a probe report.
    grep -E '^[a-z][a-z0-9_]*=[-0-9.eE+]+$' "$1" 2>/dev/null >>"$METRICS" || true
}

step_gate_probe() {
    # S5, S7 and F2 over the real index of the scan root, in the run's own
    # data folder. One discarded launch first builds the index.
    mkdir -p "$OUT/app-data" "$OUT/gate_probe"
    run_step gate_probe "$BIN/fx-ui-macos" --root "$SCAN_ROOT" --data-dir "$OUT/app-data" --gate-probe "$OUT/gate_probe/warmup.txt"
    i=1
    while [ "$i" -le "$LAUNCHES" ]; do
        run_step gate_probe "$BIN/fx-ui-macos" --root "$SCAN_ROOT" --data-dir "$OUT/app-data" --gate-probe "$OUT/gate_probe/launch-$i.txt"
        probe_lines "$OUT/gate_probe/launch-$i.txt"
        i=$((i + 1))
    done
}

step_summon() {
    # F3.
    mkdir -p "$OUT/probe-data"
    run_step summon "$BIN/fx-ui-macos" --synthetic "$PROBE_ROWS" --data-dir "$OUT/probe-data" --summon-probe "$OUT/summon.txt"
    probe_lines "$OUT/summon.txt"
}

step_sort() {
    # S6.
    mkdir -p "$OUT/probe-data"
    run_step sort "$BIN/fx-ui-macos" --synthetic "$PROBE_ROWS" --data-dir "$OUT/probe-data" --sort-probe "$OUT/sort.txt"
    probe_lines "$OUT/sort.txt"
}

stop_app() {
    if [ -n "$APP_PID" ]; then
        kill -TERM "$APP_PID" 2>/dev/null || true
        wait "$APP_PID" 2>/dev/null || true
        APP_PID=""
    fi
}

# background_phase LABEL WINDOW-ARGS...: launches the app on the run's data
# folder, runs churn (when the rate is above 0) and samples the app for the
# phase's length.
background_phase() {
    label=$1
    shift
    note "background $label: $BG_MINUTES min, churn $CHURN_RATE ops/s"
    "$BIN/fx-ui-macos" --root "$SCAN_ROOT" --data-dir "$OUT/bg-data" "$@" >>"$OUT/bg-app-$label.log" 2>&1 &
    APP_PID=$!
    # The index loads from the store the first-scan phase saved; give the
    # app time to settle before the samples count.
    sleep 30
    if [ "$CHURN_RATE" -gt 0 ]; then
        "$BIN/fx-bench" churn --dir "$SCRATCH/churn-$label" --duration "$BG_SECONDS" --rate "$CHURN_RATE" --seed 1 >>"$OUT/churn-$label.log" 2>&1 &
        CHURN_PID=$!
    fi
    run_step "sample_$label" "$BIN/fx-bench" sample --pid "$APP_PID" --interval 10 --duration "$BG_SECONDS" --label "$label" --out "$OUT/sample-$label"
    if [ -n "$CHURN_PID" ]; then
        wait "$CHURN_PID" 2>/dev/null || true
        CHURN_PID=""
    fi
    stop_app
}

step_background() {
    # M3: the app's first scan of the root into an empty data folder,
    # sampled every second for two minutes (about 15 s of scanning on 2M
    # entries, then the save and the settled index).
    mkdir -p "$OUT/bg-data"
    note "background first scan: 120 s, sampled every second"
    "$BIN/fx-ui-macos" --root "$SCAN_ROOT" --data-dir "$OUT/bg-data" --window hidden >>"$OUT/bg-app-first-scan.log" 2>&1 &
    APP_PID=$!
    run_step sample_first_scan "$BIN/fx-bench" sample --pid "$APP_PID" --interval 1 --duration 120 --label first_scan --out "$OUT/sample-first-scan"
    stop_app

    # B1, B3, B4, B6, B7, M1; then B2, B3, B5, B7, M2.
    background_phase hidden --window hidden
    background_phase search --window search --query "$QUERY"

    m="$OUT/sample-first-scan/metrics.txt"
    emit first_scan_footprint_max_mb $(mb $(value_of first_scan_footprint_max_bytes "$m"))
    for label in hidden search; do
        m="$OUT/sample-$label/metrics.txt"
        emit "${label}_cpu_pct" $(value_of "${label}_cpu_pct" "$m")
        emit "${label}_energy_mw" $(value_of "${label}_energy_mw" "$m")
        emit "${label}_wakeups_per_s" $(value_of "${label}_wakeups_per_s" "$m")
        emit "${label}_footprint_mb" $(mb $(value_of "${label}_footprint_bytes" "$m"))
        emit cpu_max_interval_pct $(value_of "${label}_cpu_max_interval_pct" "$m")
        emit disk_written_per_day_mb $(mb $(value_of "${label}_disk_written_per_day_bytes" "$m"))
        emit "${label}_minutes" "$BG_MINUTES"
    done
}

step_dmg() {
    # O1.
    emit dmg_bytes "$(stat -f %z "$DMG")"
}

for s in matcher startup catchup freshness first-run cli gate-probe summon sort background dmg; do
    wants "$s" || continue
    case "$s" in
        matcher) step_matcher ;;
        startup) step_startup ;;
        catchup) step_catchup ;;
        freshness) step_freshness ;;
        first-run) step_first_run ;;
        cli) step_cli ;;
        gate-probe) step_gate_probe ;;
        summon) step_summon ;;
        sort) step_sort ;;
        background) step_background ;;
        dmg) [ -n "$DMG" ] && step_dmg ;;
    esac
done

echo "load after: $(sysctl -n vm.loadavg 2>/dev/null || echo unknown)" >>"$OUT/run.txt"

# ---------------------------------------------------------------------------
# Compare with the baseline.
# ---------------------------------------------------------------------------

set -- --baseline "$BASELINE" --tier "$TIER" --out "$OUT/summary.md"
[ "$PARTIAL" -eq 0 ] || set -- "$@" --partial
status=0
"$BIN/fx-bench" perf-compare "$@" "$METRICS" || status=$?
note "perf-compare exit code: $status (summary in $OUT/summary.md)"
exit "$status"
