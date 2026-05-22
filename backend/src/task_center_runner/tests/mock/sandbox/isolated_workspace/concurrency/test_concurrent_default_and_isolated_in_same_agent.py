"""Same agent, default ``api.write_file`` + isolated workspace coexist.

The isolated upperdir is the agent's sandbox; the default flow continues
to advance the layer-stack tip via the existing peer-publish pathway. The
isolated view (snapshot-at-enter) is UNCHANGED by a concurrent default
write — pinning is the load-bearing property here.
"""

from __future__ import annotations

import asyncio
import uuid

import pytest

from benchmarks.sweevo.models import _REPO_DIR
from sandbox.host.daemon_client import call_daemon_api
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
async def test_concurrent_default_and_isolated_in_same_agent(
    iws_clean_sandbox,
) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    opened = await _iws_rpc.enter(
        sandbox_id, "agent-A", layer_stack_root=_REPO_DIR,
    )
    assert opened.get("success") is True, opened
    initial_manifest = opened["manifest_version"]
    token = uuid.uuid4().hex[:8]
    iso_path = f"/testbed/iso-{token}.txt"
    default_path = f"/testbed/default-{token}.txt"
    try:
        # Race a default-flow write and an isolated shell write.
        async def isolated_write() -> dict:
            return await _iws_rpc.shell(
                sandbox_id, "agent-A",
                f"echo isolated-{token} > {iso_path} && cat {iso_path}",
            )

        async def default_write() -> dict:
            return await call_daemon_api(
                sandbox_id,
                "api.write_file",
                {"path": default_path, "content": f"default-{token}\n"},
                timeout=30,
            )

        iso_res, def_res = await asyncio.gather(isolated_write(), default_write())
        assert iso_res.get("success") is True, iso_res
        assert def_res.get("success") is True, def_res
        assert f"isolated-{token}" in (iso_res.get("stdout", "") or ""), iso_res

        # The isolated view stays pinned — default's iso write isn't visible
        # to the next read in the isolated shell.
        # The manifest_version on the isolated handle stays unchanged.
        st = await _iws_rpc.status(sandbox_id, "agent-A")
        assert st.get("manifest_version") == initial_manifest, st

        # Inside isolated ws, the default-path write is invisible.
        peek = await _iws_rpc.shell(
            sandbox_id, "agent-A", f"cat {default_path} 2>&1 || echo MISSING",
        )
        assert "MISSING" in (peek.get("stdout", "") or ""), peek
    finally:
        await _iws_rpc.exit_(sandbox_id, "agent-A")
