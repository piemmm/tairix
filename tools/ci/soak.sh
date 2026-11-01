#!/usr/bin/env bash
# soak.sh — run the soaks (fuzz, proptest, the filesystem and generator soaks,
# and the repeated-test soak) with every harness, model, and the test matrix
# running IN PARALLEL, one log per job.
#
# `cargo xtask fuzz --soak` and `cargo xtask proptest --soak` already run
# their registries concurrently in-process (bounded by host parallelism).
# This script still fans the registry out into one process per target so the
# nightly soak gets per-target isolation and one log file per target: a crash
# in one harness leaves the others' logs intact. Both orchestrators expose
# `--target NAME` (and `--list`), and the registries stay the single source of
# truth — this script never hard-codes the target list.
#
# Each job's budget counts from its own start (24 h, or `--secs N`), so the
# soak outlasts it by however long the slowest job waited to be built — every
# job compiles behind one build lock. `--window N` instead hands every job the
# close of a window opened as this script starts (`--until`): builds come out
# of it, and no job starts soaking work past it. What is in flight at the close
# still finishes; tools/ci/README.md says what that can be. `--scheduled` is
# the window every scheduled soak runs.
#
# `cargo xtask test --soak` is the test-matrix counterpart: it repeats the
# matrix for the same budget so a flake too rare to surface in the per-PR
# single-pass run (where `ci` runs each test once) still gets a full night of
# exposure. It is a single job (the matrix is one unit), launched alongside the
# fuzz/proptest fan-out.
#
# Scheduling priority: the test matrix is the ONLY job with a hard, no-retry
# wall-clock deadline per job (its QEMU verticals fail closed on timeout); the
# fuzz/proptest/fssoak/rngsoak soaks are throughput jobs with no per-pass
# deadline —
# they merely run more passes the more CPU they get. Every job fans out to all
# cores, so launching them all together oversubscribes the host many-fold; the
# QEMU guests then starve for TCG cycles and time out (only the deadline-bound
# test job can fail this way, while the deadline-free soaks just run slower).
# We therefore run the throughput soaks at a lowered scheduling priority (a
# positive `nice`) so they only ever consume CPU the timed test matrix is not
# using: the kernel hands the guests their cores whenever they are runnable,
# keeping the QEMU runner's "owns the host" assumption — and thus every
# guest's wall-clock deadline — true under the full parallel fan-out. Lowering
# one's own niceness never requires privilege, so this works on any runner.
#
# This `nice` split is a STRUCTURAL measure that keeps the host from
# oversubscribing the timed guests — it is NOT a licence to tolerate a timeout.
# A QEMU vertical's deadline is sized to the actual work with headroom; if a
# guest still misses it, that is a genuine defect (a too-tight budget, a missing
# completion signal, an unsynchronised wait) to be fixed, not tolerated. It is
# NEVER dismissed as a "machine load" flake and NEVER "resolved" by re-running
# the vertical on its own until it passes — every such timeout has turned out to
# be a real bug the load merely exposed.
#
# Usage:
#   tools/ci/soak.sh [fuzz|proptest|fssoak|rngsoak|test|both|all] [--sequential] \
#                    [--secs N | --window N | --scheduled [--window N]] [--dry-run]
#
#   (no kind)      same as `both`: the fuzz and proptest soaks
#   fuzz           run only the fuzz harnesses
#   proptest       run only the proptest models
#   fssoak         run only the docs/src/filesystem/soak.md filesystem soak
#   rngsoak        run only the statistical random-generator soak
#   test           run only the repeated-test soak
#   both           run the fuzz and proptest soaks
#   all            run fuzz, proptest, fssoak, rngsoak, and the test soak
#   --sequential   run jobs one at a time (default is parallel)
#   --secs N       give each job N seconds of its own run (smoke runs; default 24 h)
#   --window N     stop every job's soaking at the close of an N-second window
#   --scheduled    the scheduled soak's window; with --window N, at most that
#   --dry-run      print the planned jobs and exit without running anything
#
# Exit status is non-zero if any job fails, or a registry listing fails or
# names no target — fail closed.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib.sh
. "$HERE/lib.sh"

usage="usage: soak.sh [fuzz|proptest|fssoak|rngsoak|test|both|all] [--sequential] [--secs N | --window N | --scheduled [--window N]] [--dry-run]"
kind="both"
sequential=0
dry_run=0
scheduled=0
secs=""
window=""

# The scheduled soak's window, seven hours: the nightly workflow's and every
# standalone scheduler's, whose periods and the workflow's timeout are sized
# to it.
scheduled_window=25200

# whole_seconds <flag> <value>: refuse a duration that is not whole seconds
# before anything starts. An empty --secs would read as none given (the 24 h
# default), a malformed one would be refused by every job only once launched,
# and a window too long for 64-bit shell arithmetic would wrap.
whole_seconds() {
    case "$2" in
        '' | *[!0-9]*)
            echo "soak: $1 expects a whole number of seconds, got '$2'" >&2
            exit 2
            ;;
    esac
    if [ "${#2}" -gt 18 ]; then
        echo "soak: $1 $2 is longer than any soak" >&2
        exit 2
    fi
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        fuzz | proptest | fssoak | rngsoak | test) kind="$1" ;;
        both | all) kind="$1" ;;
        --sequential) sequential=1 ;;
        --dry-run) dry_run=1 ;;
        --scheduled) scheduled=1 ;;
        --secs)
            shift
            secs="${1-}"
            whole_seconds --secs "$secs"
            ;;
        --window)
            shift
            window="${1-}"
            whole_seconds --window "$window"
            ;;
        *)
            echo "soak: unexpected argument '$1'" >&2
            echo "$usage" >&2
            exit 2
            ;;
    esac
    shift
done

if [ "$scheduled" -eq 1 ]; then
    if [ -n "$secs" ]; then
        echo "soak: --secs and --scheduled are mutually exclusive" >&2
        exit 2
    fi
    if [ -z "$window" ]; then
        window="$scheduled_window"
    elif [ "$window" -gt "$scheduled_window" ]; then
        echo "soak: --window $window is longer than the ${scheduled_window} s scheduled window its schedulers are sized to" >&2
        exit 2
    fi
fi

# The window opens before anything else runs, so preparing the checkout and
# building the orchestrator come out of it too.
budget_args=()
if [ -n "$window" ]; then
    if [ -n "$secs" ]; then
        echo "soak: --secs and --window are mutually exclusive" >&2
        exit 2
    fi
    if [ "$sequential" -eq 1 ]; then
        echo "soak: --window needs the jobs to overlap; with --sequential every job after the first would find it closed" >&2
        exit 2
    fi
    until_epoch=$(( $(date +%s) + 10#$window ))
    budget_args=(--until "$until_epoch")
elif [ -n "$secs" ]; then
    budget_args=(--secs "$secs")
fi

ci_prepare

stamp="$(ci_stamp)"
soak_dir="$TAIRIX_CI_LOGDIR/soak-$stamp"
mkdir -p "$soak_dir"
if [ -n "$window" ]; then
    echo "soak: soaking stops at the close of the ${window} s window (Unix time $until_epoch)"
fi

# Parallel arrays describing each launched job (bash 3.2 has no associative
# arrays / `wait -n`, so we track index-aligned plain arrays).
job_labels=()
job_logs=()
job_pids=()

# Niceness applied to the deadline-free throughput soaks (fuzz/proptest/fssoak/rngsoak)
# so they yield CPU to the hard-deadline QEMU `test` matrix (see the priority
# note in the header). 19 is the lowest scheduling priority, the largest
# possible yield.
throughput_nice=19

# launch_raw <niceness|-> <label> <xtask-args...>: start (or, when
# --sequential, run) one `cargo xtask <args...>` job, logging to
# "$soak_dir/<label>.log". A numeric <niceness> runs the job at that lowered
# scheduling priority (via `nice`); `-` runs it at normal priority.
launch_raw() {
    local niceness="$1"
    local label="$2"
    shift 2
    local logf="$soak_dir/$label.log"
    echo "soak: $label -> $logf"
    job_labels+=("$label")
    job_logs+=("$logf")
    if [ "$dry_run" -eq 1 ]; then
        return 0
    fi
    # Prefix `nice -n <niceness>` for the deadline-free throughput soaks so
    # they yield CPU to the hard-deadline QEMU test matrix (see the header).
    local run_prefix=()
    if [ "$niceness" != "-" ]; then
        run_prefix=(nice -n "$niceness")
    fi
    # A job's stdin is /dev/null: a sequential job would otherwise read the
    # rest of the listing the launch loop is reading.
    if [ "$sequential" -eq 1 ]; then
        local rc=0
        ${run_prefix[@]+"${run_prefix[@]}"} cargo xtask "$@" </dev/null >"$logf" 2>&1 || rc=$?
        job_pids+=("done:$rc")
    else
        ${run_prefix[@]+"${run_prefix[@]}"} cargo xtask "$@" </dev/null >"$logf" 2>&1 &
        job_pids+=("$!")
    fi
}

# launch <label> <xtask-subcommand> <target>: a fuzz/proptest/fssoak/rngsoak
# soak job for one registry target, under the soak's budget. These are the
# deadline-free throughput soaks, so they run at the lowered
# `$throughput_nice` priority.
launch() {
    local label="$1" subcmd="$2" target="$3"
    launch_raw "$throughput_nice" "$label" "$subcmd" --soak --target "$target" \
        ${budget_args[@]+"${budget_args[@]}"}
}

case "$kind" in
    both) registries=(fuzz proptest) ;;
    all) registries=(fuzz proptest fssoak rngsoak) ;;
    test) registries=() ;;
    *) registries=("$kind") ;;
esac

# Every registry's `--list` names its targets, the first column each one's
# `--target`. All are read before any job starts: a listing that failed after
# the first launch would leave the jobs already started unreported.
listings=()
for registry in ${registries[@]+"${registries[@]}"}; do
    if ! listing="$(cargo xtask "$registry" --list | awk 'NF { print $1 }')"; then
        echo "soak: \`cargo xtask $registry --list\` failed; no job was started" >&2
        exit 1
    fi
    if [ -z "$listing" ]; then
        echo "soak: \`cargo xtask $registry --list\` named no target; no job was started" >&2
        exit 1
    fi
    listings+=("$listing")
done

for i in ${registries[@]+"${!registries[@]}"}; do
    while IFS= read -r target; do
        launch "${registries[$i]}-$target" "${registries[$i]}" "$target"
    done <<< "${listings[$i]}"
done

# The repeated-test soak: one job that repeats the whole test matrix
# (host + the bare-metal QEMU verticals) for the soak's budget. `cargo xtask
# test` owns the repeat loop, so the budget covers the matrix as a unit.
# The test matrix runs at normal priority (`-`): it is the deadline-bound job
# the throughput soaks above yield to.
if [ "$kind" = "all" ] || [ "$kind" = "test" ]; then
    launch_raw - "test" test --qemu --soak ${budget_args[@]+"${budget_args[@]}"}
fi

if [ "$dry_run" -eq 1 ]; then
    echo "soak: dry run — ${#job_labels[@]} job(s) planned under $soak_dir"
    exit 0
fi

# Collect every job's result. For parallel jobs `wait <pid>` yields that job's
# exit status; sequential jobs already carry "done:<rc>".
failed=0
i=0
while [ "$i" -lt "${#job_pids[@]}" ]; do
    pid="${job_pids[$i]}"
    label="${job_labels[$i]}"
    logf="${job_logs[$i]}"
    rc=0
    case "$pid" in
        done:*) rc="${pid#done:}" ;;
        *) wait "$pid" || rc=$? ;;
    esac
    if [ "$rc" -eq 0 ]; then
        echo "soak: PASS $label"
    else
        echo "soak: FAIL $label (rc=$rc, see $logf)"
        failed=$((failed + 1))
    fi
    i=$((i + 1))
done

ci_collect_qemu_artefacts "$soak_dir/qemu"

echo "soak: ${#job_labels[@]} job(s), $failed failed; logs in $soak_dir"
[ "$failed" -eq 0 ] || exit 1
