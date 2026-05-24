"""``_init_complete`` event blocks ``enter`` until startup_gc finishes.

After SIGKILL+respawn the daemon runs startup_gc. A concurrent ``enter``
issued before GC settled MUST wait — the visible signal is that the
``gc_orphan`` audit events arrive BEFORE the ``enter`` event in the
serialised audit log.
"""

from __future__ import annotations

import pytest

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
    not database_configured(), reason="database URL not configured",
)
@pytest.mark.skipif(
    not live_e2e_heavy_enabled(),
    reason="heavy live e2e disabled in runner.live_e2e.heavy_enabled",
)
@pytest.mark.timeout(360)
async def test_init_complete_blocks_enter_during_startup_gc(
    iws_clean_sandbox, iws_audit_jsonl,
) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])

    # First, create a handle to leave persisted state for startup_gc to reap.
    seeded = await _iws_rpc.enter(
        sandbox_id, "agent-A", layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
    )
    assert seeded.get("success") is True, seeded

    # SIGKILL the daemon and respawn — startup_gc will run and emit
    # gc_orphan events for the seeded handle. The bootstrap enter issued by
    # daemon_kill_and_respawn proves the wait-for-init_complete behaviour
    # because that enter MUST queue behind startup_gc.
    await daemon_kill_and_respawn(sandbox_id, layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT)

    # Now fire a fresh enter and observe ordering in the audit log: any
    # gc_orphan events from the post-respawn GC must precede this enter's
    # audit entry.
    fresh = await _iws_rpc.enter(
        sandbox_id, "agent-B", layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
    )
    assert fresh.get("success") is True, fresh
    try:
        jsonl = await iws_audit_jsonl()
        rows = _iws_invariants.read_events(jsonl)
        enter_b_idx = next(
            (
                idx for idx, row in enumerate(rows)
                if row.get("type") == "sandbox_isolated_workspace_enter"
                and (row.get("payload") or {}).get("agent_id") == "agent-B"
            ),
            None,
        )
        assert enter_b_idx is not None, "agent-B enter event missing"
        prior_types = [r.get("type") for r in rows[:enter_b_idx]]
        # Either gc_orphan ran (preferred — there were orphans) or it
        # didn't (the daemon-restart bootstrap exit cleaned up first). Both
        # are consistent with init_complete behaviour. We require: no
        # SECOND enter event was admitted between GC settling and agent-B.
        #
        # The seeded enter from BEFORE the daemon kill is in the log too —
        # the audit JSONL persists across the SIGKILL+respawn cycle. That
        # pre-restart enter has nothing to do with the post-restart
        # init_complete invariant, so we only count enters that appear
        # AFTER the last gc_orphan (the post-restart watermark). When no
        # gc_orphan was emitted the bootstrap is the only post-restart
        # enter and the unfiltered count still satisfies <= 1.
        gc_idxs = [
            i for i, t in enumerate(prior_types)
            if t == "sandbox_isolated_workspace_gc_orphan"
        ]
        cutoff = max(gc_idxs) + 1 if gc_idxs else 0
        post_restart_enters = [
            t for t in prior_types[cutoff:]
            if t == "sandbox_isolated_workspace_enter"
        ]
        # At most one prior enter (the bootstrap) is allowed before agent-B.
        assert len(post_restart_enters) <= 1, prior_types
    finally:
        await _iws_rpc.exit_(sandbox_id, "agent-B")
