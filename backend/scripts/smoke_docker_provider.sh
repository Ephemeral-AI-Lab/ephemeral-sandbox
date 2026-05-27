#!/usr/bin/env bash
# Manual SWE-EVO smoke test for PLAN_v4 §6 Step 6.
#
# Runs one SWE-EVO instance end-to-end under EOS_SANDBOX_PROVIDER=docker on a
# Linux host with a local docker daemon, then grep-checks the mount-mode
# coverage ratio: PRIVATE_NAMESPACE for ≥95% of `attempt`-strategy execs.
#
# This script is the manual counterpart to
# `backend/tests/integration_test/test_benchmarks/test_sweevo_docker_smoke.py`
# (which is auto-skipped on darwin via pytest.mark.skipif).
#
# Bails cleanly on non-Linux.

set -euo pipefail

if [ "$(uname -s)" != "Linux" ]; then
    echo "smoke_docker_provider: non-Linux host ($(uname -s)); skipping." >&2
    exit 0
fi

if ! command -v docker >/dev/null 2>&1; then
    echo "smoke_docker_provider: docker not on PATH." >&2
    exit 2
fi

INSTANCE_ID="${EOS_SWEEVO_INSTANCE:-}"
if [ -z "$INSTANCE_ID" ]; then
    echo "smoke_docker_provider: set EOS_SWEEVO_INSTANCE before running." >&2
    exit 2
fi

LOG_DIR="${SMOKE_LOG_DIR:-.planning/ralplan-docker-provider/smoke-logs}"
mkdir -p "$LOG_DIR"
LOG_FILE="$LOG_DIR/smoke_docker_provider_${INSTANCE_ID}.log"

export EOS_SANDBOX_PROVIDER=docker

PYTHON="${PYTHON:-.venv/bin/python}"
echo "smoke: running sweevo on $INSTANCE_ID under EOS_SANDBOX_PROVIDER=docker" | tee "$LOG_FILE"
"$PYTHON" -m task_center_runner.benchmarks.sweevo --instance-id "$INSTANCE_ID" 2>&1 | tee -a "$LOG_FILE" || true

TOTAL=$(grep -c "mount_mode=" "$LOG_FILE" || true)
PRIVATE=$(grep -c "mount_mode=PRIVATE_NAMESPACE" "$LOG_FILE" || true)

if [ "$TOTAL" -eq 0 ]; then
    echo "smoke: no mount_mode= lines in log; provider observability log missing?" >&2
    exit 3
fi

RATIO_NUM=$(( PRIVATE * 100 / TOTAL ))
echo "smoke: PRIVATE_NAMESPACE / total = $PRIVATE / $TOTAL = ${RATIO_NUM}%" | tee -a "$LOG_FILE"

THRESHOLD="${SMOKE_RATIO_THRESHOLD:-95}"
if [ "$RATIO_NUM" -lt "$THRESHOLD" ]; then
    echo "smoke: FAIL — ratio ${RATIO_NUM}% below threshold ${THRESHOLD}%" >&2
    exit 4
fi

echo "smoke: PASS"
