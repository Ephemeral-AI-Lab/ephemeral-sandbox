"""Orphan layer-stack leases are released so the OCC path can advance.

Lease state outlives the daemon process (it lives in the layer-stack
registry, not the iws manager). After SIGKILL + restart the daemon must
release every persisted lease — otherwise default-mode flushes
would block forever.

Verification: emit a ``gc_orphan`` event with ``kind=lease`` AND drive a
default-mode ``api.overlay.flush`` after GC; if a stale lease still
pinned an outdated tip, the flush would error.
"""

from __future__ import annotations

import pytest

from sandbox.host.daemon_client import call_daemon_api
from task_center_runner.tests._live_config import (
    database_configured,
    live_e2e_heavy_enabled,
)
from task_center_runner.tests.mock.sandbox.isolated_workspace import (
    _iws_invariants,
    _iws_rpc,
)
from task_center_runner.tests.mock.sandbox.isolated_workspace._iws_fixtures import (
    daemon_kill_and_respawn,
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
@pytest.mark.timeout(300)
async def test_daemon_restart_releases_orphan_lease(
    iws_clean_sandbox, iws_audit_jsonl
) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    enter = await _iws_rpc.enter(
        sandbox_id, "agent-A", layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
    )
    assert enter.get("success") is True, enter

    await daemon_kill_and_respawn(sandbox_id, layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT)

    jsonl = await iws_audit_jsonl()
    gc_events = _iws_invariants.events_of_type(
        jsonl, "sandbox_isolated_workspace_gc_orphan",
    )
    lease_events = [
        row for row in gc_events
        if (row.get("payload") or {}).get("kind") == "lease"
    ]
    assert lease_events, (
        "expected at least one gc_orphan event with kind=lease after restart",
        gc_events,
    )

    # The OCC default path must still work — proves no stale lease is pinned.
    flush_resp = await call_daemon_api(
        sandbox_id, "api.overlay.flush", {}, timeout=30,
    )
    assert flush_resp.get("success") is not False, flush_resp
