"""Phase 1 staged proof: a planner submission flows through the REAL loop.

Before adapting the probe-heavy executor, prove the riskiest *new* path: the
``ScenarioLoopRunner`` + ``extras["runtime_config"]`` injection driving a real
TaskCenter submission terminal (``submit_plan_closes_goal`` →
``submit_execution_success`` → ``submit_evaluation_success``) through
``run_pipeline`` → ``start_task_center_run`` → launcher → ``run_ephemeral_agent``,
landing a closed goal in store state. Asserted via ``graph_summary`` /
``task_center_status`` (real store), not lifecycle events.
"""

from __future__ import annotations

import uuid
from collections.abc import Iterator, Sequence
from pathlib import Path

import pytest

from runtime.app_factory import model_store
from task_center_runner.core.runner import run_scenario
from task_center_runner.core.stores import TaskCenterStoreBundle
from task_center_runner.scenarios.base import ScenarioBase, ScenarioContext, ToolCallSpec
from task_center_runner.tests._live_config import database_configured
from tools.submission.evaluator import submit_evaluation_success
from tools.submission.planner import submit_plan_closes_goal

pytestmark = pytest.mark.asyncio


class _PlannerSubmitProof(ScenarioBase):
    """Planner closes the goal with one trivial executor task; evaluator passes."""

    name = "planner_submit_proof"

    def planner_response(self, ctx: ScenarioContext) -> ToolCallSpec:
        return ToolCallSpec(
            submit_plan_closes_goal,
            {
                "plan_spec": "Proof: planner submission through the real loop.",
                "evaluation_criteria": ["Proof criterion satisfied."],
                "tasks": [{"id": "t1", "agent_name": "executor", "deps": []}],
                "task_specs": {"t1": "Trivial executor task (no probe)."},
            },
        )

    def executor_actions(self, ctx: ScenarioContext) -> Sequence[str]:
        return ()

    def evaluator_response(self, ctx: ScenarioContext) -> ToolCallSpec:
        return ToolCallSpec(
            submit_evaluation_success,
            {
                "summary": "Proof accepted.",
                "passed_criteria": list(ctx.attempt.evaluation_criteria),
            },
        )


@pytest.fixture
def _active_mock_model(stores: TaskCenterStoreBundle) -> Iterator[None]:
    """Throwaway active model row so ``spawn_agent`` resolves a model id (the
    mock path now goes through the real spawn). Mirrors the real_agent suite."""
    prior_sf = model_store._session_factory  # noqa: SLF001 — restored on teardown
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
async def test_planner_submission_through_real_loop(
    workspace: dict[str, object],
    audit_dir: Path,
    stores: TaskCenterStoreBundle,
    _active_mock_model: None,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("EOS_MOCK_EVENT_SOURCE_RUNNER", "1")

    report = await run_scenario(
        _PlannerSubmitProof(),
        sandbox_id=str(workspace["sandbox_id"]),
        audit_dir=audit_dir,
        repo_dir=str(workspace["repo_dir"]),
        entry_prompt="proof entry prompt",
        stores=stores,
    )

    # The goal closed — which required planner + executor + evaluator terminals
    # to dispatch through the real loop and mutate TaskCenter store state.
    assert report.task_center_status == "done", report.metrics
    goals = report.graph_summary["workflows"]
    assert len(goals) == 1, goals
    goal = goals[0]
    assert goal["origin_kind"] == "entry", goal
    # The executor task landed done in real store state.
    task_statuses = [
        task.get("status")
        for iteration in goal["iterations"]
        for attempt in iteration["attempts"]
        for task in attempt["tasks"]
    ]
    assert task_statuses, goal
    assert all(status == "done" for status in task_statuses), task_statuses
