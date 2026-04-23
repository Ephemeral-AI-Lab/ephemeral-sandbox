"""Tests for tools.daytona_toolkit.shell_tool."""

from __future__ import annotations

import json
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

from message.stream_events import StreamEvent, SystemNotification
from tools.core.base import ToolExecutionContext, run_tool_safely
from tools.core.hooks.execution import execute_tool_with_hooks
from tools.daytona_toolkit import shell_tool as shell_tool_module
from tools.daytona_toolkit.shell_tool import (
    daytona_shell,
)

pytestmark = pytest.mark.asyncio


def _ctx(metadata=None) -> ToolExecutionContext:
    return ToolExecutionContext(cwd=Path("/tmp"), metadata=metadata or {})


def _ci_service():
    """Fixture service exposing :meth:`svc.cmd` for the daytona_shell tool."""
    svc = MagicMock()
    svc.cmd = AsyncMock(side_effect=_svc_cmd_passthrough)
    return svc


async def _svc_cmd_passthrough(
    sandbox,
    command,
    *,
    timeout=None,
    description="",
    agent_id="",
    team_run_id="",
    agent_run_id="",
    task_id="",
    stdin=None,
    attribute_changes=True,
):
    """Dispatch through the sandbox's process.exec so existing fakes still work.

    Production :meth:`svc.cmd` adds OCC audit, but tests only need the
    exec-through behavior; the audit layer is covered by
    ``test_overlay_auditor.py``.
    """
    del description, agent_id, team_run_id, agent_run_id, task_id, stdin, attribute_changes
    response = await sandbox.process.exec(command, timeout=timeout)
    stdout = getattr(response, "result", "") or ""
    _, exit_code = shell_tool_module._extract_exit_code(stdout, fallback_exit_code=0)
    return SimpleNamespace(
        result=stdout,
        exit_code=exit_code,
        changed_paths=[],
        ambient_changed_paths=[],
        files_written=0,
        git_commit_status=None,
        git_conflict_file=None,
        git_conflict_reason=None,
    )


def _make_manifest(
    status="ok",
    writes=None,
    shells=None,
    error="",
    reads=None,
):
    return {
        "status": status,
        "writes": writes or [],
        "shells": shells or [],
        "reads": reads or [],
        "error": error,
    }


def _inline_manifest_output(manifest=None, *, prefix: str = "") -> str:
    payload = manifest or _make_manifest()
    rendered = json.dumps({"manifest": payload, "status": payload.get("status", "ok")})
    return f"{prefix}\n{rendered}" if prefix else rendered


def _make_sandbox(
    *,
    upload_exc=None,
    upload_side_effect=None,
    exec_stdout=None,
    exec_exc=None,
    manifest=None,
    download_exc=None,
):
    sb = MagicMock()

    async def _exec(command, timeout=None):
        del timeout
        if exec_exc:
            raise exec_exc
        if "path.write_text" in command:
            if upload_side_effect is not None:
                result = upload_side_effect(command)
                if result is not None:
                    return result
            if upload_exc:
                raise upload_exc
            return MagicMock(result="", exit_code=0)
        if "path.read_text" in command:
            if download_exc:
                raise download_exc
            payload = json.dumps(
                {
                    "exists": True,
                    "content": json.dumps(manifest or _make_manifest()),
                }
            )
            return MagicMock(result=payload, exit_code=0)
        default_exec = exec_stdout or _inline_manifest_output(manifest)
        return MagicMock(result=default_exec, exit_code=0)

    sb.process.exec = AsyncMock(side_effect=_exec)

    return sb


def _assert_ok(result) -> dict:
    assert not result.is_error, result.output
    return json.loads(result.output)


async def _capture_emit(events: list[StreamEvent], event: StreamEvent) -> None:
    events.append(event)


async def _run_with_events(tool, payload, ctx):
    events: list[StreamEvent] = []
    result = await execute_tool_with_hooks(
        tool,
        payload,
        ctx,
        emit=lambda event: _capture_emit(events, event),
        emit_started=False,
    )
    return result, events


def _notification_texts(events: list[StreamEvent]) -> list[str]:
    return [event.text for event in events if isinstance(event, SystemNotification)]


def _shell_exec_output(stdout: str, exit_code: int = 0) -> str:
    return f"{stdout}\n__CODEX_EXIT_CODE__={exit_code}\n"




async def test_build_tool_output_ok_when_no_failures():
    """Sanity check: clean execution stays status='ok', is_error=False."""
    result = shell_tool_module._build_tool_output(
        context=_ctx(),
        status="ok",
        files_written=1,
        shells=[],
        warnings=[],
    )
    assert result.is_error is False
    payload = json.loads(result.output)
    assert payload["status"] == "ok"








async def test_shell_input_model_accepts_command():
    inp = daytona_shell.input_model(command="echo hi", timeout=30)
    assert inp.command == "echo hi"
    assert inp.timeout == 30










async def test_shell_mode_requires_ci_service():
    sb = _make_sandbox(exec_stdout=_shell_exec_output("LIVE_BASH_OK", 0))
    ctx = _ctx({"daytona_sandbox": sb, "daytona_cwd": "/repo"})

    result = await daytona_shell.execute(
        daytona_shell.input_model(command="echo LIVE_BASH_OK", timeout=25),
        ctx,
    )

    assert result.is_error
    assert "Code intelligence service is unavailable" in result.output
    assert result.metadata["ci_required"] is True


async def test_coordinated_shell_requires_ci_service():
    sb = _make_sandbox(exec_stdout=_shell_exec_output("LIVE_BASH_OK", 0))
    ctx = _ctx(
        {
            "daytona_sandbox": sb,
            "daytona_cwd": "/repo",
            "agent_name": "developer",
        }
    )

    result = await daytona_shell.execute(
        daytona_shell.input_model(command="echo LIVE_BASH_OK", timeout=25),
        ctx,
    )

    assert result.is_error
    assert "Code intelligence service is unavailable" in result.output
    assert result.metadata["ci_required"] is True


async def test_shell_mode_with_ci_runs_single_audited_process_op():
    sb = _make_sandbox(exec_stdout=_shell_exec_output("LIVE_BASH_OK", 0))
    ctx = _ctx({"daytona_sandbox": sb, "daytona_cwd": "/repo", "ci_service": _ci_service()})

    result = await daytona_shell.execute(
        daytona_shell.input_model(command="echo LIVE_BASH_OK", timeout=25),
        ctx,
    )

    data = _assert_ok(result)
    assert data["status"] == "ok"
    assert data["files_written"] == 0
    assert "LIVE_BASH_OK" in data["shell_outputs"][0]["stdout"]


async def test_shell_mode_reports_nonzero_exit_as_error():
    sb = _make_sandbox(exec_stdout=_shell_exec_output("cat: missing", 1))
    ctx = _ctx({"daytona_sandbox": sb, "daytona_cwd": "/repo", "ci_service": _ci_service()})

    result = await daytona_shell.execute(
        daytona_shell.input_model(command="cat /missing"),
        ctx,
    )

    assert result.is_error
    data = json.loads(result.output)
    assert data["status"] == "error"
    assert data["shells_run"] == 1


async def test_shell_mode_transport_failure_includes_fallback_context():
    sb = _make_sandbox(exec_exc=RuntimeError("Failed to execute command: "))
    ctx = _ctx({"daytona_sandbox": sb, "daytona_cwd": "/repo", "ci_service": _ci_service()})

    result = await daytona_shell.execute(
        daytona_shell.input_model(command="pytest -q", timeout=600),
        ctx,
    )

    assert result.is_error
    assert "Execution failed:" in result.output
    assert (
        "Failed to execute command: (no additional detail from Daytona SDK)"
        in result.output
    )
    assert "[exception_type=RuntimeError]" in result.output
    assert "operation=daytona_shell" in result.output
    assert "timeout=600s" in result.output
    assert "command='pytest -q'" in result.output




async def test_shell_mode_blocks_audited_test_suite_write_with_policy_message():
    sb = _make_sandbox()
    svc = MagicMock()
    svc.cmd = AsyncMock(
        return_value=SimpleNamespace(
            result=_shell_exec_output("patched", 0),
            exit_code=0,
            changed_paths=["/testbed/dask/tests/test_cli.py"],
            files_written=1,
        )
    )
    ctx = _ctx(
        {
            "daytona_sandbox": sb,
            "daytona_cwd": "/testbed",
            "agent_name": "developer",
            "write_scope": ["dask/cli.py"],
            "ci_service": svc,
        }
    )

    result = await run_tool_safely(
        daytona_shell,
        {"command": "sed -i s/old/new/ dask/tests/test_cli.py"},
        ctx,
    )

    assert result.is_error
    assert result.output.startswith("post-hook failed daytona_shell: BLOCKED_TEST_FILE_EDIT")
    assert "dask/tests/test_cli.py" in result.output
    assert "read/verify-only" in result.output
    assert result.metadata["blocked_by"] == "post_hook"
    assert result.metadata["original_tool_is_error"] is False


@pytest.mark.parametrize(
    ("command", "expected_fragments"),
    [
        (
            "sed -i s/old/new/ dask/core.py",
            [
                "BLOCKED: daytona_shell is for runtime commands",
                "in-place sed",
                "daytona_edit_file",
                "daytona_delete_file",
                "daytona_move_file",
            ],
        ),
        (
            "python -c \"from pathlib import Path; Path('dask/core.py').write_text('x')\"",
            [
                "BLOCKED: daytona_shell is for runtime commands",
                "inline Python file mutation",
                "daytona_edit_file",
                "daytona_delete_file",
                "daytona_move_file",
            ],
        ),
        (
            "mv dask/core.py dask/new_core.py",
            [
                "BLOCKED: daytona_shell is for runtime commands",
                "filesystem mutation command",
                "daytona_edit_file",
                "daytona_delete_file",
                "daytona_move_file",
            ],
        ),
        (
            "git rm dask/core.py",
            [
                "BLOCKED: daytona_shell is for runtime commands",
                "filesystem mutation command",
                "daytona_edit_file",
                "daytona_delete_file",
                "daytona_move_file",
            ],
        ),
        (
            "git mv dask/core.py dask/new_core.py",
            [
                "BLOCKED: daytona_shell is for runtime commands",
                "filesystem mutation command",
                "daytona_edit_file",
                "daytona_delete_file",
                "daytona_move_file",
            ],
        ),
    ],
)
async def test_team_shell_mode_blocks_file_edit_side_channels_before_exec(
    command,
    expected_fragments,
):
    sb = _make_sandbox(exec_stdout=_shell_exec_output("patched", 0))
    svc = _ci_service()
    ctx = _ctx(
        {
            "daytona_sandbox": sb,
            "daytona_cwd": "/testbed",
            "agent_name": "developer",
            "team_run_id": "run-1",
            "work_item_id": "task-1",
            "ci_service": svc,
        }
    )

    result = await run_tool_safely(
        daytona_shell,
        {"command": command},
        ctx,
    )

    assert result.is_error
    for fragment in expected_fragments:
        assert fragment in result.output
    svc.cmd.assert_not_awaited()
    sb.process.exec.assert_not_awaited()


@pytest.mark.parametrize(
    ("command", "sanitized"),
    [
        (
            "find . -maxdepth 1 -type f 2>/dev/null|head -n 1",
            "find . -maxdepth 1 -type f",
        ),
        (
            "files=$(find . -name '*.py' 2>/dev/null); printf '%s\\n' \"$files\"",
            "files=$(find . -name '*.py'); printf '%s\\n' \"$files\"",
        ),
        (
            "files=$(find . -name '*.py' 2>/dev/null | head -1); printf '%s\\n' \"$files\"",
            "files=$(find . -name '*.py'); printf '%s\\n' \"$files\"",
        ),
        ("command -v rg >/dev/null 2>&1; echo ok", "command -v rg ; echo ok"),
        ("optional-probe &>/dev/null", "optional-probe"),
        ("printf x > dask/core.py", "printf x"),
        ("pytest 2>/tmp/errors.log", "pytest"),
        ("printf x >/dev/null.log", "printf x"),
        ("printf x | tee dask/core.py", "printf x"),
    ],
)
async def test_team_shell_mode_sanitizes_output_pipeline_before_exec(
    command,
    sanitized,
):
    sb = _make_sandbox(exec_stdout=_shell_exec_output("ok", 0))
    svc = _ci_service()
    ctx = _ctx(
        {
            "daytona_sandbox": sb,
            "daytona_cwd": "/testbed",
            "agent_name": "developer",
            "team_run_id": "run-1",
            "work_item_id": "task-1",
            "ci_service": svc,
        }
    )

    result, events = await _run_with_events(
        daytona_shell,
        {"command": command},
        ctx,
    )

    data = _assert_ok(result)
    assert data["shell_outputs"][0]["command"] == sanitized
    texts = _notification_texts(events)
    assert any("sanitized daytona_shell command" in text for text in texts)
    svc.cmd.assert_awaited_once()
    sb.process.exec.assert_awaited_once()




@pytest.mark.parametrize(
    "command",
    [
        "rm dask/core.py",
        "rmdir dask/empty",
    ],
)
async def test_team_shell_mode_allows_removals_for_overlay_audit(command):
    sb = _make_sandbox(exec_stdout=_shell_exec_output("removed", 0))
    svc = _ci_service()
    ctx = _ctx(
        {
            "daytona_sandbox": sb,
            "daytona_cwd": "/testbed",
            "agent_name": "developer",
            "team_run_id": "run-1",
            "work_item_id": "task-1",
            "ci_service": svc,
        }
    )

    result = await run_tool_safely(
        daytona_shell,
        {"command": command},
        ctx,
    )

    _assert_ok(result)
    svc.cmd.assert_awaited_once()


async def test_shell_mode_truncates_large_stdout_before_tool_result():
    large = "\n".join(f"line-{i:05d}" for i in range(2000))
    sb = _make_sandbox(exec_stdout=_shell_exec_output(large, 1))
    svc = _ci_service()
    ctx = _ctx(
        {
            "daytona_sandbox": sb,
            "daytona_cwd": "/testbed",
            "ci_service": svc,
        }
    )

    result = await daytona_shell.execute(
        daytona_shell.input_model(command="pytest --continue-on-collection-errors"),
        ctx,
    )

    data = json.loads(result.output)
    stdout = data["shell_outputs"][0]["stdout"]
    stderr = data["shell_outputs"][0]["stderr"]
    assert len(stdout) < len(large)
    assert "truncated" in stdout
    assert "line-01999" in stdout
    assert stderr == stdout


@pytest.mark.parametrize(
    "command",
    [
        "cat /testbed/dask/dataframe/io/tests/test_hdf.py | head -50",
        "head -50 dask/dataframe/io/json.py",
        "sed -n '1,20p' dask/dataframe/io/json.py",
        "grep -n read_json dask/dataframe/io/json.py",
        "python -c \"print(open('dask/dataframe/io/json.py').read())\"",
        "python -c \"import inspect; print(inspect.getsource(object))\"",
    ],
)
async def test_team_shell_mode_allows_file_read_side_channels(command):
    sb = _make_sandbox(exec_stdout=_shell_exec_output("contents", 0))
    svc = _ci_service()
    ctx = _ctx(
        {
            "daytona_sandbox": sb,
            "daytona_cwd": "/testbed",
            "agent_name": "developer",
            "team_run_id": "run-1",
            "work_item_id": "task-1",
            "ci_service": svc,
        }
    )

    result = await daytona_shell.execute(
        daytona_shell.input_model(command=command),
        ctx,
    )

    data = _assert_ok(result)
    assert data["shell_outputs"][0]["stdout"] == "contents"
    svc.cmd.assert_awaited_once()
    sb.process.exec.assert_awaited_once()




async def test_team_shell_mode_still_allows_runtime_commands():
    sb = _make_sandbox(exec_stdout=_shell_exec_output("ok", 0))
    svc = _ci_service()
    ctx = _ctx(
        {
            "daytona_sandbox": sb,
            "daytona_cwd": "/testbed",
            "agent_name": "developer",
            "team_run_id": "run-1",
            "work_item_id": "task-1",
            "ci_service": svc,
        }
    )

    result = await daytona_shell.execute(
        daytona_shell.input_model(command='python -c "print(1 > 0)"'),
        ctx,
    )

    data = _assert_ok(result)
    assert data["shell_outputs"][0]["stdout"] == "ok"
    svc.cmd.assert_awaited_once()


async def test_team_shell_mode_still_allows_pytest_file_arguments():
    sb = _make_sandbox(exec_stdout=_shell_exec_output("1 passed", 0))
    svc = _ci_service()
    ctx = _ctx(
        {
            "daytona_sandbox": sb,
            "daytona_cwd": "/testbed",
            "agent_name": "developer",
            "team_run_id": "run-1",
            "work_item_id": "task-1",
            "ci_service": svc,
        }
    )

    result = await daytona_shell.execute(
        daytona_shell.input_model(
            command="pytest dask/dataframe/io/tests/test_json.py::test_read_json_engine_str -q"
        ),
        ctx,
    )

    data = _assert_ok(result)
    assert data["shell_outputs"][0]["stdout"] == "1 passed"
    svc.cmd.assert_awaited_once()


async def test_team_shell_mode_treats_audited_changes_as_ambient():
    sb = _make_sandbox()

    async def _svc_cmd_with_ambient_changes(
        sandbox,
        command,
        *,
        timeout=None,
        description="",
        agent_id="",
        team_run_id="",
        agent_run_id="",
        task_id="",
        stdin=None,
        attribute_changes=True,
    ):
        del sandbox, command, timeout, description, agent_id, team_run_id, agent_run_id, task_id, stdin
        assert attribute_changes is True
        return SimpleNamespace(
            result=_shell_exec_output("ujson ok", 0),
            exit_code=0,
            changed_paths=[],
            ambient_changed_paths=["/testbed/dask/_compatibility.py"],
            files_written=0,
        )

    svc = MagicMock()
    svc.cmd = AsyncMock(side_effect=_svc_cmd_with_ambient_changes)
    ctx = _ctx(
        {
            "daytona_sandbox": sb,
            "daytona_cwd": "/testbed",
            "agent_name": "developer",
            "team_run_id": "run-1",
            "work_item_id": "task-1",
            "write_scope": ["dask/dataframe/io/json.py"],
            "ci_service": svc,
        }
    )

    result, events = await _run_with_events(
        daytona_shell,
        {"command": "python -c 'print(\"ujson ok\")'"},
        ctx,
    )

    data = _assert_ok(result)
    assert data["files_written"] == 0
    assert result.metadata.get("ambient_changed_paths") == ["/testbed/dask/_compatibility.py"]
    # Ambient paths surface through a user-only post-hook advisory; the tool
    # output JSON no longer embeds the warning text directly.
    assert any("ambient concurrent edits" in text for text in _notification_texts(events))
    # ``audited_write_policy`` only fires on ``changed_paths``, and ambient-only
    # responses leave that empty — so the absence of an "outside write_scope"
    # advisory is expected and not a negative-signal regression.




async def test_shell_mode_emits_post_advisory_for_audited_outside_scope_write():
    sb = _make_sandbox()
    svc = MagicMock()
    svc.cmd = AsyncMock(
        return_value=SimpleNamespace(
            result=_shell_exec_output("patched", 0),
            exit_code=0,
            changed_paths=["/testbed/dask/_compatibility.py"],
            files_written=1,
        )
    )
    ctx = _ctx(
        {
            "daytona_sandbox": sb,
            "daytona_cwd": "/testbed",
            "agent_name": "developer",
            "write_scope": ["dask/config.py"],
            "ci_service": svc,
        }
    )

    result, events = await _run_with_events(
        daytona_shell,
        {"command": "python - <<'PY'\nprint('patched')\nPY"},
        ctx,
    )

    data = _assert_ok(result)
    assert data["files_written"] == 1
    assert data["warnings"] == []
    assert any("outside write_scope" in text for text in _notification_texts(events))














async def test_shell_mode_sanitizes_legacy_cd_and_stderr_merge_for_team_agents():
    sb = _make_sandbox(exec_stdout=_shell_exec_output("ok", 0))
    svc = _ci_service()
    ctx = _ctx(
        {
            "daytona_sandbox": sb,
            "daytona_cwd": "/testbed",
            "agent_name": "developer",
            "ci_service": svc,
        }
    )

    result, events = await _run_with_events(
        daytona_shell,
        {"command": "cd /testbed && pytest tests/unit/test_x.py -q 2>&1"},
        ctx,
    )

    data = _assert_ok(result)
    assert data["shell_outputs"][0]["command"] == "pytest tests/unit/test_x.py -q"
    svc.cmd.assert_awaited_once()
    sb.process.exec.assert_awaited_once()
    texts = _notification_texts(events)
    assert any("sanitized daytona_shell command" in text for text in texts)


