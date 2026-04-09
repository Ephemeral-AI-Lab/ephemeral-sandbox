#!/usr/bin/env bash
# Run a SWE-EVO benchmark instance end-to-end (sandbox + required tests).
# Defaults to direct image sandboxes (no snapshot registration) unless the
# caller explicitly passes --register-snapshot.
# Usage: ./scripts/run_benchmark.sh <instance-id>        # run a specific instance
#        ./scripts/run_benchmark.sh list                 # list all instances
#        ./scripts/run_benchmark.sh pick [size] [target] # auto-pick an instance
#
# Examples:
#   ./scripts/run_benchmark.sh pydantic__pydantic_v2.6.0b1_v2.6.0
#   ./scripts/run_benchmark.sh pydantic__pydantic_v2.6.0b1_v2.6.0 --register-snapshot
#   ./scripts/run_benchmark.sh list
#   ./scripts/run_benchmark.sh pick medium 10
#   ./scripts/run_benchmark.sh pick large

set -euo pipefail
cd "$(dirname "$0")/.."

PY=".venv/bin/python"
BENCH_MOD="benchmarks.sweevo"

if [[ ! -x "$PY" ]]; then
    echo "error: $PY not found. Create the venv first."
    exit 1
fi

export PYTHONPATH="backend/src${PYTHONPATH:+:$PYTHONPATH}"

if [[ $# -eq 0 ]]; then
    cat <<EOF
Usage: $0 <instance-id|command>

Commands:
  list                      List every SWE-EVO instance with size/bullet count
  pick [size] [bullets]     Auto-pick an instance (size: small|medium|large|any,
                            default medium; bullets: target count, default 10)
  <instance-id>             Run that instance end-to-end

Examples:
  $0 list
  $0 pick medium 10
  $0 pydantic__pydantic_v2.6.0b1_v2.6.0
EOF
    exit 0
fi

NAME="$1"
shift || true

BENCH_ARGS=()
ensure_snapshot_mode_default() {
    BENCH_ARGS=("$@")
    for arg in "$@"; do
        if [[ "$arg" == "--no-register-snapshot" || "$arg" == "--register-snapshot" ]]; then
            return
        fi
    done
    BENCH_ARGS+=("--no-register-snapshot")
}

case "$NAME" in
    list)
        exec "$PY" -m "$BENCH_MOD" --list
        ;;
    pick)
        SIZE="${1:-medium}"
        TARGET="${2:-10}"
        shift || true
        if [[ $# -gt 0 ]]; then
            shift || true
        fi
        ensure_snapshot_mode_default "$@"
        echo "Auto-picking instance: size=$SIZE target-bullets=$TARGET"
        exec "$PY" -m "$BENCH_MOD" --size "$SIZE" --target-bullets "$TARGET" "${BENCH_ARGS[@]}"
        ;;
esac

ensure_snapshot_mode_default "$@"

exec "$PY" -m "$BENCH_MOD" --instance-id "$NAME" "${BENCH_ARGS[@]}"
