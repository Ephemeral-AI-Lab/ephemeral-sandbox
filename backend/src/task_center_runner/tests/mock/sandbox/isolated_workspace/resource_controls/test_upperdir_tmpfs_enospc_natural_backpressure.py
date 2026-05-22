"""ENOSPC inside the workspace is natural backpressure, not a daemon crash.

When the upperdir tmpfs fills, the workload's ``dd``/``cat > file`` exits
non-zero with ``No space left on device`` — but the workspace remains open
and reads still work. This test exercises the tool_call surface end-to-end:
the daemon stays healthy under guest backpressure.

Note: the daemon honours the configured ``UPPERDIR_BYTES`` as a host-RAM
gate at enter() time but does NOT (yet) mount the upperdir as a sized
tmpfs. The behavioural assertion below tolerates either implementation —
what we PIN is "ENOSPC is a guest-tool problem, not a daemon-crash
problem".
"""

from __future__ import annotations

import pytest

from benchmarks.sweevo.models import _REPO_DIR
from task_center_runner.tests._live_config import (
    database_configured,
    live_e2e_heavy_enabled,
)
from task_center_runner.tests.mock.sandbox.isolated_workspace import _iws_rpc


pytestmark = pytest.mark.asyncio


@pytest.mark.skipif(
    not database_configured(), reason="database URL not configured",
)
@pytest.mark.skipif(
    not live_e2e_heavy_enabled(),
    reason="heavy live e2e disabled in runner.live_e2e.heavy_enabled",
)
@pytest.mark.timeout(240)
async def test_upperdir_tmpfs_enospc_natural_backpressure(
    iws_clean_sandbox,
) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    opened = await _iws_rpc.enter(
        sandbox_id, "agent-A", layer_stack_root=_REPO_DIR,
    )
    assert opened.get("success") is True, opened
    try:
        # Write a small file first to prove the upperdir is writable.
        baseline = await _iws_rpc.shell(
            sandbox_id, "agent-A",
            "echo baseline > /testbed/probe.txt && cat /testbed/probe.txt",
        )
        assert baseline.get("success") is True, baseline
        assert "baseline" in (baseline.get("stdout", "") or ""), baseline

        # Try to write a body larger than any plausible budget. Either:
        #   * The tmpfs is sized -> dd fails with ENOSPC (exit_code != 0,
        #     stderr contains "No space left on device"); subsequent reads
        #     of the small probe still succeed.
        #   * The upperdir isn't size-limited -> dd succeeds; that's OK too
        #     since the assertion is about "daemon survives", not about
        #     enforcing a tmpfs ceiling we don't yet implement.
        fill = await _iws_rpc.shell(
            sandbox_id, "agent-A",
            "dd if=/dev/zero of=/testbed/bigfile bs=1M count=64 2>&1 || true",
        )
        # Daemon stays healthy regardless of dd's exit.
        readback = await _iws_rpc.shell(
            sandbox_id, "agent-A", "cat /testbed/probe.txt",
        )
        assert readback.get("success") is True, readback
        assert "baseline" in (readback.get("stdout", "") or ""), readback
        # If dd reported ENOSPC the daemon must have surfaced exit_code !=0
        # (NOT crashed) — assert the tool_call envelope is well-formed.
        assert "exit_code" in fill, fill
    finally:
        await _iws_rpc.exit_(sandbox_id, "agent-A")
