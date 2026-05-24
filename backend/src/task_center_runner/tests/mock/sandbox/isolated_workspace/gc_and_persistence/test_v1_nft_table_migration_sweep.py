"""Daemon ``initialize`` sweeps pre-v2 nft tables (``eos_pinws_*``).

Pre-create the legacy-named tables; trigger daemon initialization (via
``enter()``); verify the legacy tables are gone and the current
``eos_iws_*`` tables exist.
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
@pytest.mark.timeout(240)
async def test_v1_nft_table_migration_sweep(iws_clean_sandbox) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    # Pre-create the legacy-named tables before any iws enter.
    await raw_exec(
        sandbox_id,
        "nft add table inet eos_pinws_nat 2>/dev/null || true && "
        "nft add table inet eos_pinws_filter 2>/dev/null || true",
        cwd="/",
        timeout=15,
    )

    # SIGKILL + respawn so initialize() (and therefore the migration sweep)
    # fires deterministically before the next enter.
    await daemon_kill_and_respawn(sandbox_id, layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT)

    # Bootstrap enter so initialize ran for sure.
    await _iws_rpc.enter(sandbox_id, "agent-A", layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT)
    try:
        listing = await raw_exec(
            sandbox_id, "nft list tables 2>/dev/null || true",
            cwd="/", timeout=15,
        )
        tables = getattr(listing, "stdout", "") or ""
        assert "eos_pinws_nat" not in tables, (
            "initialize() must sweep the legacy nat table", tables,
        )
        assert "eos_pinws_filter" not in tables, (
            "initialize() must sweep the legacy filter table", tables,
        )
        # v2 tables are present.
        assert "eos_iws_nat" in tables and "eos_iws_filter" in tables, tables
    finally:
        await _iws_rpc.exit_(sandbox_id, "agent-A")
