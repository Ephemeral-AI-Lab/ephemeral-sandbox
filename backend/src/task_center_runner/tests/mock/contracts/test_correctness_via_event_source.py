"""Phase 1 completion proof: CorrectnessTesting through the event-source runner.

Runs the full ``correctness_testing`` scenario — multi-iteration, eval-failure
retry, partial-plan defer, and the sandbox_integrity / final probes — through
``ScenarioLoopRunner`` + the real query loop (``EOS_MOCK_EVENT_SOURCE_RUNNER=1``)
instead of the imperative ``MockSquadRunner``. Asserts via real store state
(``graph_summary``) + re-homed sandbox checks, NOT lifecycle events (those
migrate in Phase 2). Proves the ported probe coroutines + ProbeContext work.
"""

from __future__ import annotations

import uuid
from collections.abc import Iterator
from pathlib import Path

import pytest

from runtime.app_factory import model_store
from task_center_runner.benchmarks.sweevo.models import SWEEvoInstance
from task_center_runner.core.stores import TaskCenterStoreBundle
from task_center_runner.environments.sweevo_image.fixtures import (
    run_scenario_on_sweevo_image,
)
from task_center_runner.scenarios.correctness_testing import CorrectnessTesting
from task_center_runner.tests._live_config import database_configured

pytestmark = pytest.mark.asyncio


@pytest.fixture
def _active_mock_model(stores: TaskCenterStoreBundle) -> Iterator[None]:
    prior_sf = model_store._session_factory  # noqa: SLF001
    model_store.initialize(stores.session_factory)
    key = f"test/mock-loop-{uuid.uuid4().hex[:8]}"
    model_store.register(
        key=key,
        label="Mock Loop Runner",
        class_path="providers.clients.anthropic_native:AnthropicClient",
        kwargs={"model": "mock-loop", "max_tokens": 4096},
        activate=True,
    )
    try:
        yield
    finally:
        try:
            model_store.delete(key)
        except Exception:
            pass
        model_store._session_factory = prior_sf  # noqa: SLF001


@pytest.mark.skipif(not database_configured(), reason="database URL not configured")
async def test_correctness_testing_through_event_source(
    sweevo_image_instance: SWEEvoInstance,
    workspace: dict[str, object],
    audit_dir: Path,
    stores: TaskCenterStoreBundle,
    _active_mock_model: None,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("EOS_MOCK_EVENT_SOURCE_RUNNER", "1")

    report = await run_scenario_on_sweevo_image(
        CorrectnessTesting(),
        instance=sweevo_image_instance,
        sandbox_id=str(workspace["sandbox_id"]),
        audit_dir=audit_dir,
        stores=stores,
    )

    # --- outcome via real store state --------------------------------------
    assert report.task_center_status == "done", report.metrics
    # The re-homed probe sandbox checks (write/read/edit/shell/batch/conflict)
    # all passed — published by ProbeContext, collected by ScenarioLifecycle.
    assert report.sandbox_checks, "no sandbox checks recorded by probes"
    assert report.passed_sandbox_checks, [
        c for c in report.sandbox_checks if not c.passed
    ]

    delegated = [
        goal
        for goal in report.graph_summary["workflows"]
        if any(it["attempts"] for it in goal["iterations"])
    ]
    assert delegated, "no goal with attempts in graph"
    assert delegated[-1]["status"] == "succeeded", delegated[-1]

    # --- the executor probe actually ran sandbox tools through real dispatch
    tool_names = {tc.tool_name for tc in report.tool_calls}
    assert {"write_file", "read_file", "edit_file", "shell"}.issubset(tool_names), (
        sorted(tool_names)
    )

    # --- iteration shape: eval-failure retry then a deferred continuation ----
    root = delegated[-1]
    assert len(root["iterations"]) >= 2, root
    iter1 = root["iterations"][0]
    # iteration 1 had >1 attempt (attempt 1 eval-failed, attempt 2 deferred).
    assert len(iter1["attempts"]) >= 2, iter1
    assert iter1["attempts"][-1]["deferred_goal_for_next_iteration"], iter1
