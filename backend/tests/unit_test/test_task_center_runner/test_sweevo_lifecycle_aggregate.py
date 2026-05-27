"""Tests for the ``aggregate_jsonl_path`` kwarg added to ``SweevoLifecycle``.

The kwarg is additive (default ``None``) — the existing
``run_sweevo_real_agent`` shim keeps its current behavior. When set, the
lifecycle appends one JSON line per ``after_run`` invocation.
"""

from __future__ import annotations

import json
from pathlib import Path
from unittest.mock import AsyncMock

import pytest

from task_center_runner.benchmarks.sweevo.models import SWEEvoInstance, SWEEvoResult
from task_center_runner.benchmarks.sweevo import eval as lifecycle_mod
from task_center_runner.core.report import PipelineReport


def _instance() -> SWEEvoInstance:
    return SWEEvoInstance(
        instance_id="dask__dask_2023.3.2_2023.4.0",
        repo="dask/dask",
        base_commit="abc",
        problem_statement="",
        patch="",
        fail_to_pass=[],
        pass_to_pass=[],
        docker_image="img",
        test_cmds="pytest",
        environment_setup_commit="",
    )


def _report(run_dir: Path, *, task_center_run_id: str = "tcr-1") -> PipelineReport:
    return PipelineReport(
        status="completed",
        task_center_run_id=task_center_run_id,
        request_id="req",
        sandbox_id="sbx-77",
        instance_id="dask__dask_2023.3.2_2023.4.0",
        run_dir=run_dir,
        task_center_status="done",
        duration_s=12.5,
        task_count=7,
        tasks_completed=7,
        tasks_failed=0,
        metrics={},
        aborted_by_timeout=False,
    )


def _stub_evaluate(monkeypatch: pytest.MonkeyPatch) -> None:
    """Replace ``evaluate_sweevo_result`` with a deterministic stub.

    The real evaluator runs F2P/P2P inside a sandbox; for unit tests we
    just need a known SWEEvoResult shape on the ``lifecycle_extras``.
    """

    async def fake_evaluate(
        _instance: SWEEvoInstance,
        result: SWEEvoResult,
        _sandbox_id: str,
        _repo_dir: str,
    ) -> SWEEvoResult:
        result.resolved = True
        result.fix_rate = 1.0
        result.fail_to_pass_passed = 3
        result.fail_to_pass_total = 3
        result.pass_to_pass_broken = 0
        result.pass_to_pass_total = 12
        return result

    async def fake_apply_layerstack(_sandbox_id: str, _repo_dir: str) -> None:
        return None

    monkeypatch.setattr(lifecycle_mod, "evaluate_sweevo_result", fake_evaluate)
    monkeypatch.setattr(lifecycle_mod, "apply_layerstack_to_repo", fake_apply_layerstack)


@pytest.mark.asyncio
async def test_lifecycle_no_aggregate_when_path_none(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """Default (aggregate_jsonl_path=None) → no JSONL side-effect."""
    _stub_evaluate(monkeypatch)
    run_dir = tmp_path / "run"
    run_dir.mkdir()

    lifecycle = lifecycle_mod.SweevoLifecycle(_instance(), repo_dir="/testbed")
    await lifecycle.after_run(ctx=AsyncMock(), report=_report(run_dir))

    # No aggregate.jsonl anywhere under tmp_path.
    assert not list(tmp_path.rglob("aggregate.jsonl"))
    # sweevo_result.json still gets written.
    assert (run_dir / "sweevo_result.json").exists()


@pytest.mark.asyncio
async def test_lifecycle_aggregate_single_line(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    _stub_evaluate(monkeypatch)
    run_dir = tmp_path / "run"
    run_dir.mkdir()
    aggregate = tmp_path / "aggregate.jsonl"

    lifecycle = lifecycle_mod.SweevoLifecycle(
        _instance(), repo_dir="/testbed", aggregate_jsonl_path=aggregate
    )
    await lifecycle.after_run(ctx=AsyncMock(), report=_report(run_dir))

    text = aggregate.read_text(encoding="utf-8")
    assert text.endswith("\n")
    lines = text.splitlines()
    assert len(lines) == 1

    payload = json.loads(lines[0])
    assert payload["instance_id"] == "dask__dask_2023.3.2_2023.4.0"
    assert payload["run_id"] == "tcr-1"
    assert payload["sandbox_id"] == "sbx-77"
    assert payload["resolved"] is True
    assert payload["fix_rate"] == 1.0
    assert payload["fail_to_pass_passed"] == 3
    assert payload["fail_to_pass_total"] == 3
    assert payload["pass_to_pass_broken"] == 0
    assert payload["pass_to_pass_total"] == 12
    assert payload["duration_s"] == 12.5
    assert payload["status"] == "completed"
    assert isinstance(payload["timestamp_utc"], str)
    assert payload["timestamp_utc"].endswith("Z")


@pytest.mark.asyncio
async def test_lifecycle_aggregate_two_lines_append(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    _stub_evaluate(monkeypatch)
    aggregate = tmp_path / "aggregate.jsonl"

    for i in range(2):
        run_dir = tmp_path / f"run-{i}"
        run_dir.mkdir()
        lifecycle = lifecycle_mod.SweevoLifecycle(
            _instance(), repo_dir="/testbed", aggregate_jsonl_path=aggregate
        )
        await lifecycle.after_run(
            ctx=AsyncMock(), report=_report(run_dir, task_center_run_id=f"tcr-{i}")
        )

    lines = aggregate.read_text(encoding="utf-8").splitlines()
    assert len(lines) == 2
    payloads = [json.loads(line) for line in lines]
    assert {p["run_id"] for p in payloads} == {"tcr-0", "tcr-1"}


@pytest.mark.asyncio
async def test_lifecycle_aggregate_creates_parent_dirs(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    _stub_evaluate(monkeypatch)
    run_dir = tmp_path / "run"
    run_dir.mkdir()
    aggregate = tmp_path / "deeply" / "nested" / "dir" / "aggregate.jsonl"

    lifecycle = lifecycle_mod.SweevoLifecycle(
        _instance(), repo_dir="/testbed", aggregate_jsonl_path=aggregate
    )
    await lifecycle.after_run(ctx=AsyncMock(), report=_report(run_dir))

    assert aggregate.exists()
    assert len(aggregate.read_text(encoding="utf-8").splitlines()) == 1


@pytest.mark.asyncio
async def test_lifecycle_aggregate_records_failure_status(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """When the run aborts on timeout, aggregate captures status='failed' + error."""
    # No evaluate stub — aborted runs skip evaluation.
    run_dir = tmp_path / "run"
    run_dir.mkdir()
    aggregate = tmp_path / "aggregate.jsonl"

    report = _report(run_dir)
    report.aborted_by_timeout = True
    report.task_center_status = "running"

    lifecycle = lifecycle_mod.SweevoLifecycle(
        _instance(), repo_dir="/testbed", aggregate_jsonl_path=aggregate
    )
    await lifecycle.after_run(ctx=AsyncMock(), report=report)

    payload = json.loads(aggregate.read_text(encoding="utf-8").strip())
    assert payload["status"] == "failed"
    assert payload["error"] == "timeout"
    assert payload["resolved"] is False
    assert payload["fix_rate"] == 0.0
