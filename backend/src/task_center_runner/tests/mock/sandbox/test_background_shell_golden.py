"""T1 — Golden path live regression for ``shell(background=True)``.

Launches 3 concurrent background shells, waits for natural exit, asserts
all return ``finished`` with non-empty stdout. AC-2 (``wait_completed`` /
full stdout) is exercised implicitly because :func:`sandbox_api.shell`
returns the post-reap result on the background path.

Gated by ``database_configured()`` + ``live_e2e_heavy_enabled()`` to keep
CI cheap; runs against a real SWE-EVO sandbox.
"""

from __future__ import annotations

import pytest

from benchmarks.sweevo.models import SWEEvoInstance
from task_center_runner.agent.mock.background_shell_probe import (
    run_background_shell_golden_probe,
    seed_workspace,
)
from task_center_runner.tests._live_config import (
    database_configured,
    live_e2e_heavy_enabled,
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
async def test_background_shell_golden(
    sweevo_image_instance: SWEEvoInstance,
    workspace: dict[str, object],
) -> None:
    sandbox_id = str(workspace["sandbox_id"])
    await seed_workspace(sandbox_id)
    summary = await run_background_shell_golden_probe(
        sandbox_id=sandbox_id,
        launch_count=3,
        sleep_s=5,
    )
    assert summary.mode == "golden"
    assert len(summary.launches) == 3
    for record in summary.launches:
        assert record.status == "ok", record
        assert record.exit_code == 0, record
        assert record.error is None, record
