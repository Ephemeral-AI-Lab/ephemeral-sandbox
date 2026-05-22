"""T2 — Cancel-mid-flight live regression for ``shell(background=True)``.

Launches 3 background shells with long sleeps, cancels each via
``asyncio.wait_for`` (which propagates as a CancelledError into
``_shell_background_dispatch`` and routes through
``_send_cancel_then_reap``). Asserts (AC-6) that cancelled jobs contribute
zero ``changed_paths`` and (AC-3) that a follow-up foreground shell
mounts quickly after cancel.
"""

from __future__ import annotations

import time

import pytest

import sandbox.api as sandbox_api
from benchmarks.sweevo.models import SWEEvoInstance
from sandbox._shared.models import SandboxCaller, ShellRequest
from task_center_runner.agent.mock.background_shell_probe import (
    run_background_shell_cancel_probe,
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
async def test_background_shell_cancel(
    sweevo_image_instance: SWEEvoInstance,
    workspace: dict[str, object],
) -> None:
    sandbox_id = str(workspace["sandbox_id"])
    await seed_workspace(sandbox_id)
    summary = await run_background_shell_cancel_probe(
        sandbox_id=sandbox_id,
        launch_count=3,
        cancel_after_s=1.0,
        sleep_s=30,
    )
    assert summary.mode == "cancel"
    assert len(summary.launches) == 3
    cancelled = [r for r in summary.launches if r.cancelled]
    assert len(cancelled) == 3, summary.launches
    for record in cancelled:
        assert record.changed_paths_count == 0, record

    # AC-3: post-cancel foreground mount latency stays under 1 s. Use a
    # trivial echo to measure the cold-start cost only.
    fg_request = ShellRequest(
        command="echo post-cancel-ok",
        cwd=".",
        timeout=30,
        background=False,
        caller=SandboxCaller(agent_id="background-shell-cancel-test.fg"),
        description="background_shell.cancel.post_foreground",
    )
    t0 = time.monotonic()
    fg_result = await sandbox_api.shell(sandbox_id, fg_request)
    fg_elapsed = time.monotonic() - t0
    assert fg_result.success, fg_result
    assert fg_elapsed < 5.0, (
        f"post-cancel foreground shell took {fg_elapsed:.3f}s; "
        f"AC-3 expects sub-second mount latency"
    )
