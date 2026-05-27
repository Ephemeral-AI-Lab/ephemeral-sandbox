"""State-vocabulary regression test for the persistent-sandbox resume path.

Per migration acceptance criterion 15, ``_resume_sandbox`` must:

(a) return the existing ``sandbox_id`` for ``status="exited"`` after calling
    ``start_sandbox`` on it,
(b) return the existing ``sandbox_id`` directly for ``status="running"``
    without restarting it, and
(c) recreate when ``status="dead"`` (or any non-recoverable status).

The Docker provider's serialized dict uses the key ``status`` with Docker
vocabulary; this test pins that contract so a future provider migration
cannot silently re-introduce the Daytona ``state`` key.
"""

from __future__ import annotations

from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest

from task_center_runner.benchmarks.sweevo import _provision
from task_center_runner.benchmarks.sweevo.models import SWEEvoInstance


def _instance() -> SWEEvoInstance:
    return SWEEvoInstance(
        instance_id="dask__dask_2023.3.2_2023.4.0",
        repo="dask/dask",
        base_commit="abc",
        problem_statement="",
        patch="",
        fail_to_pass=[],
        pass_to_pass=[],
        docker_image="example/image:1.0",
        test_cmds="pytest",
        environment_setup_commit="",
    )


@pytest.mark.asyncio
async def test_resume_running_returns_id_without_restart(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    started: list[str] = []
    service = SimpleNamespace(
        start_sandbox=lambda sid: started.append(sid),
        delete_sandbox=lambda _sid: pytest.fail("must not delete a running sandbox"),
    )
    monkeypatch.setattr(_provision, "_service", lambda: service)

    existing = {"id": "sbx-running", "status": "running"}
    sandbox_id = await _provision._resume_sandbox(
        existing, "sweevo-x", _instance(), "/testbed"
    )

    assert sandbox_id == "sbx-running"
    assert started == []


@pytest.mark.asyncio
async def test_resume_exited_starts_and_returns_id(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    started: list[str] = []
    service = SimpleNamespace(
        start_sandbox=lambda sid: started.append(sid),
        delete_sandbox=lambda _sid: pytest.fail("must not delete a resumable sandbox"),
    )
    monkeypatch.setattr(_provision, "_service", lambda: service)

    existing = {"id": "sbx-exited", "status": "exited"}
    sandbox_id = await _provision._resume_sandbox(
        existing, "sweevo-x", _instance(), "/testbed"
    )

    assert sandbox_id == "sbx-exited"
    assert started == ["sbx-exited"]


@pytest.mark.asyncio
async def test_resume_dead_recreates_after_delete(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    deleted: list[str] = []
    service = SimpleNamespace(
        start_sandbox=lambda _sid: pytest.fail("must not start a dead sandbox"),
        delete_sandbox=lambda sid: deleted.append(sid),
    )
    monkeypatch.setattr(_provision, "_service", lambda: service)

    fake_create = AsyncMock(return_value="sbx-fresh")
    monkeypatch.setattr(_provision, "_create_sandbox", fake_create)

    existing = {"id": "sbx-dead", "status": "dead"}
    sandbox_id = await _provision._resume_sandbox(
        existing, "sweevo-x", _instance(), "/testbed"
    )

    assert deleted == ["sbx-dead"]
    assert sandbox_id == "sbx-fresh"
    fake_create.assert_awaited_once()
