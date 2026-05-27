from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest

from task_center_runner.benchmarks.sweevo.models import SWEEvoInstance
from task_center_runner.core import real_agent_run


def _instance() -> SWEEvoInstance:
    return SWEEvoInstance(
        instance_id="dask__dask_2023.3.2_2023.4.0",
        repo="dask/dask",
        base_commit="abc",
        problem_statement="",
        patch="",
        fail_to_pass=[],
        pass_to_pass=[],
        docker_image="example/image",
        test_cmds="pytest",
        environment_setup_commit="",
    )


@pytest.mark.asyncio
async def test_run_sweevo_real_agent_uses_host_cwd_for_runtime_config(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    captured: dict[str, Any] = {}

    async def fake_run_pipeline(config: Any) -> SimpleNamespace:
        captured["config"] = config
        return SimpleNamespace(
            lifecycle_extras={},
            task_center_run_id="run-1",
            sandbox_id="sbx-1",
            run_dir=tmp_path / "run",
            task_center_status="failed",
            duration_s=1.0,
            task_count=0,
            tasks_completed=0,
            tasks_failed=0,
            aborted_by_timeout=False,
            performance_report_task=None,
        )

    monkeypatch.setattr(real_agent_run, "run_pipeline", fake_run_pipeline)

    await real_agent_run.run_sweevo_real_agent(
        instance=_instance(),
        sandbox_id="sbx-1",
        audit_dir=tmp_path,
        stores=SimpleNamespace(),
    )

    config = captured["config"]
    assert config.repo_dir == "/testbed"
    assert config.extras["runtime_config"].cwd == str(Path.cwd())
