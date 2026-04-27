from __future__ import annotations

import os
from typing import Any

import pytest

from benchmarks.sweevo.evaluation import evaluate_sweevo_result
from benchmarks.sweevo.models import SWEEvoInstance
from benchmarks.sweevo.models import SWEEvoResult
from benchmarks.sweevo.sandbox import (
    _exec as _sweevo_exec,
    _upload_file_with_fallback,
    create_sweevo_test_sandbox,
)
from benchmarks.sweevo.task_center_runner import (
    build_sweevo_user_prompt,
    load_pr_description_overrides,
    run_sweevo_with_task_center,
)


_DASK_INSTANCE_ID = "dask__dask_2023.3.2_2023.4.0"


def _daytona_api_reachable() -> bool:
    import socket
    from urllib.parse import urlparse

    from config.settings import load_settings

    settings = load_settings()
    raw_url = settings.daytona_api_url or os.environ.get("DAYTONA_API_URL", "")
    parsed = urlparse(raw_url)
    host = parsed.hostname
    if not host:
        return False
    if parsed.port is not None:
        port = parsed.port
    elif parsed.scheme == "https":
        port = 443
    else:
        port = 80
    try:
        with socket.create_connection((host, port), timeout=1.0):
            return True
    except OSError:
        return False


def _instance(**overrides: Any) -> SWEEvoInstance:
    base = {
        "instance_id": _DASK_INSTANCE_ID,
        "repo": "dask/dask",
        "base_commit": "abc123",
        "problem_statement": "Dataset fallback problem statement",
        "patch": "",
        "fail_to_pass": ["dask/tests/test_cli.py::test_config_get"],
        "pass_to_pass": ["dask/tests/test_config.py::test_collect"],
        "docker_image": "example/image",
        "test_cmds": "pytest -q",
        "environment_setup_commit": "",
        "instance_id_swe": _DASK_INSTANCE_ID,
    }
    base.update(overrides)
    return SWEEvoInstance(**base)


def test_sweevo_user_prompt_uses_checked_in_pr_description_csv() -> None:
    load_pr_description_overrides.cache_clear()

    prompt = build_sweevo_user_prompt(_instance(), "/testbed")

    assert prompt.startswith("<Workspace Root>\n/testbed\n<Workspace Root>\n\n")
    assert "<pr_description>\n2023.4.0\n--------" in prompt
    assert "Add a CLI command to ``list`` and ``get`` a value from dask config" in prompt
    assert "Dataset fallback problem statement" not in prompt
    assert "minimal changes to non-tests files in the /testbed directory" in prompt


@pytest.mark.asyncio
async def test_task_center_spawn_passes_sweevo_repo_metadata(monkeypatch: pytest.MonkeyPatch) -> None:
    from agents.builtins import register_builtin_agents
    from server.app_factory import RuntimeConfig
    from task_center.model import Status
    from task_center.runtime import TaskCenter, build_production_spawn

    captured: dict[str, Any] = {}

    async def fake_execute_ephemeral_agent_run(
        _runtime_config: RuntimeConfig,
        _input_message: str,
        *,
        extra_tool_metadata: Any,
        **_kwargs: Any,
    ) -> bool:
        captured["metadata"] = extra_tool_metadata
        captured["sandbox_id"] = _kwargs["sandbox_id"]
        extra_tool_metadata["task_center"].submit_task_success(
            extra_tool_metadata["task_id"],
            "completed",
        )
        return True

    register_builtin_agents()
    monkeypatch.setattr(
        "server.routers.core.execute_ephemeral_agent_run",
        fake_execute_ephemeral_agent_run,
    )

    runtime_config = RuntimeConfig(cwd=".")
    tc = TaskCenter(
        runtime_config,
        spawn_func=build_production_spawn(
            runtime_config,
            extra_tool_metadata={
                "repo_root": "/testbed",
                "exec_cwd": "/testbed",
                "ci_workspace_root": "/testbed",
            },
        ),
    )

    root = await tc.run_query("repair the benchmark", sandbox_id="sbx-1")

    assert root.status is Status.DONE
    metadata = captured["metadata"]
    assert metadata["repo_root"] == "/testbed"
    assert metadata["exec_cwd"] == "/testbed"
    assert metadata["ci_workspace_root"] == "/testbed"
    assert captured["sandbox_id"] == "sbx-1"
    assert metadata["role"] == "executor"


@pytest.mark.e2e
@pytest.mark.live
@pytest.mark.asyncio
async def test_live_task_center_runs_one_sweevo_instance() -> None:
    """Run with:

    SWEEVO_INSTANCE_ID=dask__dask_2023.3.2_2023.4.0 \
      PYTHONPATH=backend/src uv run pytest \
      backend/tests/test_benchmarks/test_sweevo_task_center_runner.py::test_live_task_center_runs_one_sweevo_instance \
      -s -v -o addopts=''
    """
    from engine.testing.eval_agent import EvalAgent
    from message.event_printer import MultiAgentEventPrinter

    if not EvalAgent.has_all():
        pytest.skip("SWE-EVO live run requires an active model and Daytona credentials")

    instance_id = os.environ.get("SWEEVO_INSTANCE_ID") or _DASK_INSTANCE_ID
    evaluate = os.environ.get("SWEEVO_EVALUATE", "1") != "0"
    register_snapshot = os.environ.get("SWEEVO_REGISTER_SNAPSHOT", "1") != "0"
    sandbox_name = os.environ.get("SWEEVO_SANDBOX_NAME", "")

    result = await run_sweevo_with_task_center(
        printer=MultiAgentEventPrinter(color=False, timestamps=True),
        instance_id=instance_id,
        sandbox_name=sandbox_name,
        register_snapshot=register_snapshot,
        evaluate=evaluate,
    )

    assert result["instance"]["instance_id"] == instance_id
    assert result["task_count"] >= 1
    assert result["task_center_status"] in {"done", "failed"}
    if evaluate:
        assert result["grading"] is not None


@pytest.mark.e2e
@pytest.mark.live
@pytest.mark.asyncio
async def test_live_sweevo_sandbox_prompt_and_grading_without_task_center_agent() -> None:
    """Run with:

    PYTHONPATH=backend/src uv run pytest \
      backend/tests/test_benchmarks/test_sweevo_task_center_runner.py::test_live_sweevo_sandbox_prompt_and_grading_without_task_center_agent \
      -s -v -o addopts=''

    This does not spawn a TaskCenter agent. It provisions the real SWE-EVO
    sandbox, verifies the CSV-backed prompt, fakes a TaskCenter terminal
    status, then applies the test patch through the grader and runs F2P/P2P.
    """
    from benchmarks.sweevo.dataset import select_sweevo_instance
    from engine.testing.eval_agent import EvalAgent

    if not EvalAgent.has_daytona():
        pytest.skip("SWE-EVO sandbox smoke requires Daytona credentials")
    if not _daytona_api_reachable():
        pytest.skip("SWE-EVO sandbox smoke requires a reachable Daytona API")

    instance_id = os.environ.get("SWEEVO_INSTANCE_ID") or _DASK_INSTANCE_ID
    sandbox_name = os.environ.get("SWEEVO_SANDBOX_NAME", "")
    register_snapshot = os.environ.get("SWEEVO_REGISTER_SNAPSHOT", "1") != "0"
    fake_status = os.environ.get("SWEEVO_FAKE_TASK_CENTER_STATUS", "completed")
    f2p_limit = int(os.environ.get("SWEEVO_F2P_LIMIT", "1"))
    p2p_limit = int(os.environ.get("SWEEVO_P2P_LIMIT", "1"))

    instance = select_sweevo_instance(instance_id=instance_id)
    prompt = build_sweevo_user_prompt(instance, "/testbed")

    assert "<pr_description>\n2023.4.0\n--------" in prompt
    assert "Add a CLI command to ``list`` and ``get`` a value from dask config" in prompt

    sandbox_result = await create_sweevo_test_sandbox(
        instance,
        sandbox_name=sandbox_name,
        register_snapshot=register_snapshot,
        repo_dir="/testbed",
    )
    sandbox_id = sandbox_result["sandbox_id"]

    repo_probe = await _sweevo_exec(
        sandbox_id,
        "cd /testbed && pwd && test -d .git && git rev-parse --show-toplevel && "
        "git rev-parse --abbrev-ref HEAD",
    )
    assert "/testbed" in repo_probe
    assert "sweevo-work" in repo_probe

    if instance.test_patch:
        patch_path = "/tmp/sweevo_test_patch_probe.patch"
        await _upload_file_with_fallback(
            sandbox_id,
            patch_path,
            instance.test_patch.encode("utf-8"),
        )
        patch_state = await _sweevo_exec(
            sandbox_id,
            (
                "cd /testbed && "
                f"if git apply --check {patch_path} >/dev/null 2>&1; then "
                "echo APPLYABLE_NOT_YET_APPLIED; "
                f"elif git apply -R --check {patch_path} >/dev/null 2>&1; then "
                "echo ALREADY_APPLIED; "
                "else echo NOT_APPLYABLE; fi"
            ),
            check=False,
        )
        assert patch_state.strip() == "APPLYABLE_NOT_YET_APPLIED"

    fake_task_center_result = {
        "status": fake_status,
        "root_task_id": "fake-task-center-root",
        "root_summary": f"fake TaskCenter run marked {fake_status}",
    }
    assert fake_task_center_result["status"] in {"completed", "failed"}

    limited_instance = _instance(
        instance_id=instance.instance_id,
        repo=instance.repo,
        base_commit=instance.base_commit,
        problem_statement=instance.problem_statement,
        patch=instance.patch,
        fail_to_pass=instance.fail_to_pass[:f2p_limit],
        pass_to_pass=instance.pass_to_pass[:p2p_limit],
        docker_image=instance.docker_image,
        test_cmds=instance.test_cmds,
        environment_setup_commit=instance.environment_setup_commit,
        test_patch=instance.test_patch,
        start_version=instance.start_version,
        end_version=instance.end_version,
        instance_id_swe=instance.instance_id_swe,
        pr_description=instance.pr_description,
    )
    assert limited_instance.fail_to_pass
    assert limited_instance.pass_to_pass

    grading = await evaluate_sweevo_result(
        limited_instance,
        SWEEvoResult(
            plan_id="fake-task-center",
            instance_id=limited_instance.instance_id,
            status=fake_status,
        ),
        sandbox_id,
        repo_dir="/testbed",
    )

    assert grading.fail_to_pass_total == len(limited_instance.fail_to_pass)
    assert grading.pass_to_pass_total == len(limited_instance.pass_to_pass)
    assert grading.status == fake_status
