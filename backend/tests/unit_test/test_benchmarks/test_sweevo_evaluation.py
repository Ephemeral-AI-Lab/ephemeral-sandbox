from __future__ import annotations

import pytest

from task_center_runner.benchmarks.sweevo import eval as sweevo_evaluation
from task_center_runner.benchmarks.sweevo.models import SWEEvoInstance, SWEEvoResult


def _instance() -> SWEEvoInstance:
    return SWEEvoInstance(
        instance_id="dask__dask_2023.3.2_2023.4.0",
        repo="dask/dask",
        base_commit="abc",
        problem_statement="",
        patch="",
        test_patch="diff --git a/test b/test\n",
        fail_to_pass=["tests/test_fix.py::test_case"],
        pass_to_pass=[],
        docker_image="example/image",
        test_cmds="pytest -q",
        environment_setup_commit="",
    )


@pytest.mark.asyncio
async def test_evaluate_runs_extract_then_patch_then_tests(monkeypatch):
    """evaluate_sweevo_result no longer materializes — the lifecycle does.

    Per Phase 3b of the migration plan, ``SweevoLifecycle.after_run`` calls
    ``apply_layerstack_to_repo`` *before* dispatching to ``evaluate_sweevo_result``
    and asserts the projected workspace has ``.git``. The evaluator can
    therefore assume the bytes are already on disk.
    """
    calls: list[str] = []

    async def fake_extract_patch(_sandbox_id: str, _repo_dir: str) -> str:
        calls.append("extract_patch")
        return "diff --git a/dask/config.py b/dask/config.py\n"

    async def fake_ensure_patch(_instance, _sandbox_id: str, _repo_dir: str) -> None:
        calls.append("test_patch")

    async def fake_run_tests(
        _sandbox_id: str,
        _repo_dir: str,
        test_ids: list[str],
        _test_cmds: str,
    ) -> int:
        calls.append("run_tests")
        return len(test_ids)

    monkeypatch.setattr(sweevo_evaluation, "_extract_combined_patch", fake_extract_patch)
    monkeypatch.setattr(sweevo_evaluation, "ensure_sweevo_test_patch", fake_ensure_patch)
    monkeypatch.setattr(sweevo_evaluation, "_run_test_set", fake_run_tests)

    result = await sweevo_evaluation.evaluate_sweevo_result(
        _instance(),
        SWEEvoResult(plan_id="plan", instance_id="dask__dask_2023.3.2_2023.4.0"),
        "sbx-1",
        "/testbed",
    )

    assert calls == ["extract_patch", "test_patch", "run_tests"]
    assert result.agent_patch.startswith("diff --git")
    assert result.resolved is True


@pytest.mark.asyncio
async def test_run_test_set_uses_python_subprocess_for_weird_test_ids(monkeypatch):
    captured: dict[str, str] = {}

    async def fake_exec(_sandbox_id: str, cmd: str, *, timeout: int, check: bool = False) -> str:
        captured["cmd"] = cmd
        return "EXIT_CODE=0"

    monkeypatch.setattr(sweevo_evaluation, "_exec", fake_exec)

    passed = await sweevo_evaluation._run_test_set(
        "sbx-1",
        "/testbed",
        ['tests/test_networks.py::test_address_invalid[\n@example.com-None]'],
        "pytest -q",
    )

    assert passed == 1
    assert "subprocess.run(argv" in captured["cmd"]
    assert 'tests/test_networks.py::test_address_invalid[\\n@example.com-None]' in captured["cmd"]


@pytest.mark.asyncio
async def test_run_test_set_counts_passed_tests_from_pytest_summary(monkeypatch):
    async def fake_exec(_sandbox_id: str, cmd: str, *, timeout: int, check: bool = False) -> str:
        return "2 failed, 3 passed\nEXIT_CODE=1"

    monkeypatch.setattr(sweevo_evaluation, "_exec", fake_exec)

    passed = await sweevo_evaluation._run_test_set(
        "sbx-1",
        "/testbed",
        ["a", "b", "c", "d", "e"],
        "pytest -q",
    )

    assert passed == 3
