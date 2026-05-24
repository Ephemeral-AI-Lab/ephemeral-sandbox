"""Upperdir is ephemeral: re-enter after writing leaves nothing behind.

Sequence: enter agent-A → write /testbed/scratch.txt → exit → re-enter →
cat /testbed/scratch.txt fails. Also asserts host-side that no
``upper/`` directory survives under the scratch root for the closed handle.

The host-side directory check uses ``raw_exec(find …)`` so the test catches
the subtler "exit didn't rmtree" regression that a pure in-ws ``cat`` test
would miss (re-enter creates a fresh scratch dir; a stranded one belonging
to the previous handle_id would not be visible from inside the new ws).
"""

from __future__ import annotations

import pytest

from sandbox.api import raw_exec
from task_center_runner.tests._live_config import (
    database_configured,
    live_e2e_heavy_enabled,
)
from task_center_runner.tests.mock.sandbox.isolated_workspace import _iws_rpc
from task_center_runner.tests.mock.sandbox.isolated_workspace._iws_fixtures import (
    iws_scratch_root,
)


pytestmark = pytest.mark.asyncio


@pytest.mark.skipif(
    not database_configured(),
    reason="database URL not configured",
)
@pytest.mark.skipif(
    not live_e2e_heavy_enabled(),
    reason="heavy live e2e disabled in runner.live_e2e.heavy_enabled",
)
@pytest.mark.timeout(240)
async def test_upperdir_discarded_on_exit(iws_clean_sandbox) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    agent_id = "agent-A"

    # Cycle 1: write a scratch file, capture its content, exit.
    first = await _iws_rpc.enter(sandbox_id, agent_id, layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT)
    assert first.get("success") is True, first
    try:
        write = await _iws_rpc.shell(
            sandbox_id, agent_id, "echo cycle-1 > /testbed/scratch.txt",
        )
        assert write.get("success") is True, write
        readback = await _iws_rpc.read_file(
            sandbox_id, agent_id, "/testbed/scratch.txt",
        )
        assert readback.get("success") is True, readback
        assert "cycle-1" in readback.get("stdout", ""), readback
    finally:
        await _iws_rpc.exit_(sandbox_id, agent_id)

    # Host-side: every scratch_root/runtime/isolated-workspace/<handle>/upper/
    # must be empty. We don't know the closed handle_id, so check the whole
    # subtree — every remaining file is an exit-cleanup bug.
    scratch = await iws_scratch_root(sandbox_id)
    assert scratch, "iws scratch_root not discovered after enter+exit"
    find = await raw_exec(
        sandbox_id,
        f"find {scratch} -type f -not -name manager.json 2>/dev/null || true",
        cwd="/",
        timeout=20,
    )
    leftover = (getattr(find, "stdout", "") or "").strip()
    assert leftover == "", (
        f"exit must rmtree the entire handle scratch dir; leftover files:\n{leftover}"
    )

    # Cycle 2: re-enter, the scratch file is gone (fresh upperdir).
    second = await _iws_rpc.enter(sandbox_id, agent_id, layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT)
    assert second.get("success") is True, second
    try:
        miss = await _iws_rpc.read_file(
            sandbox_id, agent_id, "/testbed/scratch.txt",
        )
        # File doesn't exist; cat exits non-zero. The success flag and
        # stderr together are enough to prove the upperdir was discarded.
        assert miss.get("success") is False, miss
        assert "No such file" in miss.get("stderr", "") or miss.get("exit_code", 0) != 0, miss
    finally:
        await _iws_rpc.exit_(sandbox_id, agent_id)
