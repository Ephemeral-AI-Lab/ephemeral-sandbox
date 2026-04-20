"""Tests for tools.daytona_toolkit.codeact_tool."""

from __future__ import annotations

import base64
import json
import re
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

from message.stream_events import StreamEvent, SystemNotification
from tools.core.base import ToolExecutionContext, run_tool_safely
from tools.core.hooks.execution import execute_tool_with_hooks
from tools.daytona_toolkit import codeact_tool as codeact_tool_module
from tools.daytona_toolkit.codeact_tool import (
    _build_exec_command,
    _build_wrapper,
    daytona_codeact,
)

pytestmark = pytest.mark.asyncio


def _ctx(metadata=None) -> ToolExecutionContext:
    return ToolExecutionContext(cwd=Path("/tmp"), metadata=metadata or {})


def _ci_service():
    """Fixture service exposing :meth:`svc.cmd` for the codeact tool."""
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
    _, exit_code = codeact_tool_module._extract_exit_code(stdout, fallback_exit_code=0)
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


def _submitted_wrapper(svc) -> str:
    wrapper = svc.cmd.await_args.kwargs.get("stdin")
    assert isinstance(wrapper, str)
    return wrapper


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


async def test_codeact_no_sandbox_returns_error():
    ctx = _ctx()
    result = await daytona_codeact.execute(daytona_codeact.input_model(code="print('hi')"), ctx)
    assert result.is_error
    assert "No Daytona sandbox" in result.output


async def test_build_tool_output_ok_when_no_failures():
    """Sanity check: clean execution stays status='ok', is_error=False."""
    result = codeact_tool_module._build_tool_output(
        context=_ctx(),
        status="ok",
        files_written=1,
        shells=[],
        script_stdout="",
        warnings=[],
    )
    assert result.is_error is False
    payload = json.loads(result.output)
    assert payload["status"] == "ok"


async def test_codeact_requires_code_or_command():
    sb = _make_sandbox()
    ctx = _ctx({"daytona_sandbox": sb})
    result = await daytona_codeact.execute(daytona_codeact.input_model(), ctx)
    assert result.is_error
    assert "Provide `code`" in result.output


async def test_codeact_rejects_both_code_and_command():
    sb = _make_sandbox()
    ctx = _ctx({"daytona_sandbox": sb})
    result = await daytona_codeact.execute(
        daytona_codeact.input_model(code="print('x')", command="pwd"),
        ctx,
    )
    assert result.is_error
    assert "either `code` or `command`" in result.output


async def test_codeact_rejects_explicit_mode_mismatch():
    sb = _make_sandbox()
    ctx = _ctx({"daytona_sandbox": sb})
    result = await daytona_codeact.execute(
        daytona_codeact.input_model(mode="shell", code="print('x')"),
        ctx,
    )
    assert result.is_error
    assert '`mode="shell"`' in result.output


async def test_codeact_input_model_accepts_shell_contract():
    inp = daytona_codeact.input_model(command="echo hi", timeout=30)
    assert inp.command == "echo hi"
    assert inp.timeout == 30
    assert inp.mode is None


async def test_codeact_api_schema_requires_one_of_command_or_code():
    schema = daytona_codeact.to_api_schema()["input_schema"]

    assert schema["oneOf"] == [{"required": ["command"]}, {"required": ["code"]}]
    assert schema["properties"]["command"]["type"] == "string"
    assert schema["properties"]["command"]["minLength"] == 1
    assert "anyOf" not in schema["properties"]["command"]
    assert schema["properties"]["code"]["type"] == "string"
    assert schema["properties"]["code"]["minLength"] == 1
    assert "anyOf" not in schema["properties"]["code"]
    assert schema["properties"]["mode"]["enum"] == ["python", "shell"]
    assert "anyOf" not in schema["properties"]["mode"]


async def test_build_wrapper_uses_write_through_without_inline_guardrails():
    wrapper = _build_wrapper(
        "write('file.txt', 'ok')",
        run_id="abcd1234",
        cwd="/repo",
    )
    assert 'with open(resolved, "w", encoding="utf-8")' in wrapper
    assert "_guarded_import" not in wrapper
    assert "_BLOCKED_MODULES" not in wrapper
    assert "_ENFORCE_TEAM_SHELL_POLICY" not in wrapper
    assert "_DISABLE_CODEACT_FILE_EDITS" not in wrapper


async def test_build_wrapper_has_no_inline_file_edit_guards():
    wrapper = _build_wrapper(
        "write('file.txt', 'ok')",
        run_id="abcd1234",
        cwd="/repo",
    )
    assert "_DISABLE_CODEACT_FILE_EDITS" not in wrapper
    assert "raise RuntimeError(_FILE_EDIT_POLICY_MESSAGE)" not in wrapper
    assert '_sandbox_builtins["open"] = _guarded_open' not in wrapper
    assert "_codeact_shell_file_edit_error" not in wrapper
    assert "_CODEACT_SHELL_FILE_EDIT_PATTERNS" not in wrapper
    assert "_codeact_shell_file_read_error" not in wrapper
    assert "CodeAct read() helper" not in wrapper
    assert "Python open() file inspection" not in wrapper
    assert "linecache.getlines" not in wrapper
    assert "inspect.getsource" not in wrapper
    assert "pathlib.Path.read_text" not in wrapper
    assert "io.open = _guarded_io_open" not in wrapper


async def test_build_exec_command_runs_wrapper_from_repo_cwd():
    command = _build_exec_command(cwd="/repo")
    assert "bash -o pipefail -lc" in command
    assert 'cd "/repo" && python3 -' in command


async def test_shell_mode_requires_ci_service():
    sb = _make_sandbox(exec_stdout=_shell_exec_output("LIVE_BASH_OK", 0))
    ctx = _ctx({"daytona_sandbox": sb, "daytona_cwd": "/repo"})

    result = await daytona_codeact.execute(
        daytona_codeact.input_model(command="echo LIVE_BASH_OK", timeout=25),
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

    result = await daytona_codeact.execute(
        daytona_codeact.input_model(command="echo LIVE_BASH_OK", timeout=25),
        ctx,
    )

    assert result.is_error
    assert "Code intelligence service is unavailable" in result.output
    assert result.metadata["ci_required"] is True


async def test_shell_mode_with_ci_runs_single_audited_process_op():
    sb = _make_sandbox(exec_stdout=_shell_exec_output("LIVE_BASH_OK", 0))
    ctx = _ctx({"daytona_sandbox": sb, "daytona_cwd": "/repo", "ci_service": _ci_service()})

    result = await daytona_codeact.execute(
        daytona_codeact.input_model(command="echo LIVE_BASH_OK", timeout=25),
        ctx,
    )

    data = _assert_ok(result)
    assert data["status"] == "ok"
    assert data["files_written"] == 0
    assert "LIVE_BASH_OK" in data["shell_outputs"][0]["stdout"]


async def test_shell_mode_reports_nonzero_exit_as_error():
    sb = _make_sandbox(exec_stdout=_shell_exec_output("cat: missing", 1))
    ctx = _ctx({"daytona_sandbox": sb, "daytona_cwd": "/repo", "ci_service": _ci_service()})

    result = await daytona_codeact.execute(
        daytona_codeact.input_model(command="cat /missing"),
        ctx,
    )

    assert result.is_error
    data = json.loads(result.output)
    assert data["status"] == "error"
    assert data["shells_run"] == 1


async def test_shell_mode_transport_failure_includes_fallback_context():
    sb = _make_sandbox(exec_exc=RuntimeError("Failed to execute command: "))
    ctx = _ctx({"daytona_sandbox": sb, "daytona_cwd": "/repo", "ci_service": _ci_service()})

    result = await daytona_codeact.execute(
        daytona_codeact.input_model(command="pytest -q", timeout=600),
        ctx,
    )

    assert result.is_error
    assert "Execution failed:" in result.output
    assert (
        "Failed to execute command: (no additional detail from Daytona SDK)"
        in result.output
    )
    assert "[exception_type=RuntimeError]" in result.output
    assert "operation=daytona_codeact shell" in result.output
    assert "timeout=600s" in result.output
    assert "command='pytest -q'" in result.output


async def test_python_mode_reports_sandbox_commit_abort_as_error():
    manifest = _make_manifest()
    exec_stdout = _inline_manifest_output(manifest)
    sb = _make_sandbox(exec_stdout=exec_stdout, manifest=manifest)
    svc = MagicMock()
    svc.cmd = AsyncMock(
        return_value=SimpleNamespace(
            result=exec_stdout,
            exit_code=0,
            changed_paths=["/repo/a.py"],
            ambient_changed_paths=[],
            files_written=1,
            git_commit_status="aborted_version",
            git_conflict_reason="version drift",
        )
    )
    ctx = _ctx({"daytona_sandbox": sb, "daytona_cwd": "/repo", "ci_service": svc})

    result = await daytona_codeact.execute(
        daytona_codeact.input_model(code="write('a.py', 'x')"),
        ctx,
    )

    assert result.is_error
    data = json.loads(result.output)
    assert data["status"] == "error"
    assert data["files_written"] == 1
    assert data["error"] == "sandbox commit aborted: version drift"
    assert result.metadata["changed_paths"] == ["/repo/a.py"]


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
        daytona_codeact,
        {"command": "sed -i s/old/new/ dask/tests/test_cli.py"},
        ctx,
    )

    assert result.is_error
    assert result.output.startswith("post-hook failed daytona_codeact: BLOCKED_TEST_FILE_EDIT")
    assert "dask/tests/test_cli.py" in result.output
    assert "read/verify-only" in result.output
    assert result.metadata["blocked_by"] == "post_hook"
    assert result.metadata["original_tool_is_error"] is False


@pytest.mark.parametrize(
    ("command", "expected_fragment"),
    [
        ("sed -i s/old/new/ dask/core.py", "in-place sed"),
        (
            "python -c \"from pathlib import Path; Path('dask/core.py').write_text('x')\"",
            "inline Python file mutation",
        ),
        ("printf x > dask/core.py", "shell output redirection"),
        ("pytest 2>/tmp/errors.log", "shell output redirection"),
        ("printf x >/dev/null.log", "shell output redirection"),
        ("printf x | tee dask/core.py", "tee file write"),
        ("mv dask/core.py dask/new_core.py", "filesystem mutation command"),
        ("git rm dask/core.py", "filesystem mutation command"),
        ("git mv dask/core.py dask/new_core.py", "filesystem mutation command"),
    ],
)
async def test_team_shell_mode_blocks_file_edit_side_channels_before_exec(
    command,
    expected_fragment,
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
        daytona_codeact,
        {"command": command},
        ctx,
    )

    assert result.is_error
    assert "BLOCKED: daytona_codeact is for runtime commands" in result.output
    assert expected_fragment in result.output
    assert "daytona_edit_file" in result.output
    assert "daytona_delete_file" in result.output
    assert "daytona_move_file" in result.output
    svc.cmd.assert_not_awaited()
    sb.process.exec.assert_not_awaited()


@pytest.mark.parametrize(
    "command",
    [
        "find . -maxdepth 1 -type f 2>/dev/null|head -n 1",
        "files=$(find . -name '*.py' 2>/dev/null); printf '%s\\n' \"$files\"",
        "command -v rg >/dev/null 2>&1; echo ok",
        "optional-probe &>/dev/null",
    ],
)
async def test_team_shell_mode_blocks_stderr_suppression_before_exec(command):
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

    result = await run_tool_safely(
        daytona_codeact,
        {"command": command},
        ctx,
    )

    assert result.is_error
    assert "CodeAct policy error: CodeAct commands must preserve stderr" in result.output
    svc.cmd.assert_not_awaited()
    sb.process.exec.assert_not_awaited()


async def test_team_python_mode_blocks_shell_helper_stderr_suppression_before_exec():
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

    result = await run_tool_safely(
        daytona_codeact,
        {"code": 'shell("find . -name *.py 2>/dev/null")'},
        ctx,
    )

    assert result.is_error
    assert "CodeAct policy error: CodeAct commands must preserve stderr" in result.output
    svc.cmd.assert_not_awaited()
    sb.process.exec.assert_not_awaited()


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
        daytona_codeact,
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

    result = await daytona_codeact.execute(
        daytona_codeact.input_model(command="pytest --continue-on-collection-errors"),
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

    result = await daytona_codeact.execute(
        daytona_codeact.input_model(command=command),
        ctx,
    )

    data = _assert_ok(result)
    assert data["shell_outputs"][0]["stdout"] == "contents"
    svc.cmd.assert_awaited_once()
    sb.process.exec.assert_awaited_once()


@pytest.mark.parametrize(
    "code",
    [
        "import inspect\nprint(inspect.getsource(object))",
        "import linecache\nprint(linecache.getlines('dask/dataframe/io/json.py'))",
        "from pathlib import Path\nPath('dask/dataframe/io/json.py').read_text()",
        "import io\nio.open('dask/dataframe/io/json.py').read()",
    ],
)
async def test_team_python_mode_allows_file_read_side_channels(code):
    sb = _make_sandbox()
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

    result = await daytona_codeact.execute(
        daytona_codeact.input_model(code=code),
        ctx,
    )

    _assert_ok(result)
    assert sb.process.exec.await_count >= 1
    svc.cmd.assert_awaited_once()


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

    result = await daytona_codeact.execute(
        daytona_codeact.input_model(command='python -c "print(1 > 0)"'),
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

    result = await daytona_codeact.execute(
        daytona_codeact.input_model(
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
        daytona_codeact,
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
        daytona_codeact,
        {"command": "python - <<'PY'\nprint('patched')\nPY"},
        ctx,
    )

    data = _assert_ok(result)
    assert data["files_written"] == 1
    assert data["warnings"] == []
    assert any("outside write_scope" in text for text in _notification_texts(events))


@pytest.mark.parametrize(
    ("code", "expected_fragment"),
    [
        ("write('dask/core.py', 'x')", "CodeAct write() helper"),
        ("open('dask/core.py', 'w').write('x')", "write-mode open()"),
        ("from pathlib import Path\nPath('dask/core.py').write_text('x')", "Path.write_text"),
        ("import os\nos.rename('dask/core.py', 'dask/new_core.py')", "Python filesystem mutation"),
        (
            "from pathlib import Path\nPath('dask/core.py').rename('dask/new_core.py')",
            "Python filesystem mutation",
        ),
        (
            "import shutil\nshutil.move('dask/core.py', 'dask/new_core.py')",
            "shutil file mutation",
        ),
    ],
)
async def test_team_python_mode_blocks_file_edits_before_upload(code, expected_fragment):
    sb = _make_sandbox()
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
        daytona_codeact,
        {"code": code},
        ctx,
    )

    assert result.is_error
    assert "BLOCKED: daytona_codeact is for runtime commands" in result.output
    assert expected_fragment in result.output
    sb.process.exec.assert_not_awaited()
    svc.cmd.assert_not_awaited()


@pytest.mark.parametrize(
    "code",
    [
        "import os\nos.remove('dask/core.py')",
        "from pathlib import Path\nPath('dask/core.py').unlink()",
        "import shutil\nshutil.rmtree('dask/__pycache__')",
    ],
)
async def test_team_python_mode_allows_removals_for_overlay_audit(code):
    sb = _make_sandbox()
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
        daytona_codeact,
        {"code": code},
        ctx,
    )

    _assert_ok(result)
    svc.cmd.assert_awaited_once()


async def test_python_mode_preserves_script_stdout_before_manifest_line():
    manifest = _make_manifest()
    exec_stdout = _inline_manifest_output(manifest, prefix="hello from codeact")
    sb = _make_sandbox(exec_stdout=exec_stdout, manifest=manifest)
    ctx = _ctx({"daytona_sandbox": sb, "daytona_cwd": "/repo", "ci_service": _ci_service()})

    result = await daytona_codeact.execute(
        daytona_codeact.input_model(code="print('hello from codeact')"),
        ctx,
    )

    data = _assert_ok(result)
    assert data["script_stdout"] == "hello from codeact"


async def test_python_mode_counts_manifest_writes():
    manifest = _make_manifest(
        writes=[
            {"path": "/repo/a.py", "content": "a = 1\n"},
            {"path": "/repo/a.py", "content": "a = 2\n"},
            {"path": "/repo/b.py", "content": "b = 1\n"},
        ]
    )
    sb = _make_sandbox(manifest=manifest)
    ctx = _ctx({"daytona_sandbox": sb, "daytona_cwd": "/repo", "ci_service": _ci_service()})

    result = await daytona_codeact.execute(
        daytona_codeact.input_model(code="write('a.py', 'a = 2\\n')"),
        ctx,
    )

    data = _assert_ok(result)
    assert data["files_written"] == 3
    assert sb.process.exec.await_count >= 1


async def test_python_mode_error_reports_wrapper_manifest_without_inline_guidance():
    manifest = _make_manifest(
        status="error",
        error="ImportError: import 'subprocess' is blocked in codeact.",
    )
    sb = _make_sandbox(exec_stdout=_inline_manifest_output(manifest), manifest=manifest)
    ctx = _ctx({"daytona_sandbox": sb, "daytona_cwd": "/repo", "ci_service": _ci_service()})

    result = await daytona_codeact.execute(
        daytona_codeact.input_model(code="raise RuntimeError('boom')"),
        ctx,
    )

    assert result.is_error
    assert "ImportError" in result.output
    assert "daytona_codeact(command=" not in result.output


@pytest.mark.parametrize(
    ("code", "manifest_error", "expected_fragment", "expect_guidance"),
    [
        (
            "import subprocess\nsubprocess.run(['python', '-m', 'pytest'])",
            "ImportError: import 'subprocess' is blocked in codeact.",
            "ImportError",
            False,
        ),
        (
            "import os\nos.system('pwd')",
            (
                "RuntimeError: CodeAct policy error: coordinated team lanes must use "
                "`daytona_codeact` shell mode or `shell(\"...\")` inside Python mode "
                "for repo commands. Replace `os.system()`/`os.popen()` wrappers."
            ),
            "os.system",
            True,
        ),
    ],
)
async def test_coordinated_python_mode_blocks_os_process_wrappers_before_runtime(
    code,
    manifest_error,
    expected_fragment,
    expect_guidance,
):
    manifest = _make_manifest(status="error", error=manifest_error)
    sb = _make_sandbox(
        exec_stdout=_inline_manifest_output(manifest),
        manifest=manifest,
    )
    ctx = _ctx(
        {
            "daytona_sandbox": sb,
            "daytona_cwd": "/repo",
            "agent_name": "developer",
            "ci_service": _ci_service(),
        }
    )

    if "os.system" in code:
        result = await run_tool_safely(daytona_codeact, {"code": code}, ctx)
    else:
        result = await daytona_codeact.execute(
            daytona_codeact.input_model(code=code),
            ctx,
        )

    assert result.is_error
    assert expected_fragment in result.output
    if expect_guidance:
        assert "daytona_codeact(command=" in result.output
    if "os.system" in code:
        assert result.metadata["blocked_by"] == "pre_hook"
        sb.process.exec.assert_not_awaited()
    else:
        assert sb.process.exec.await_count >= 1


async def test_shell_mode_preserves_command_for_team_agents():
    sb = _make_sandbox(exec_stdout=_shell_exec_output("ok", 0))
    ctx = _ctx(
        {
            "daytona_sandbox": sb,
            "daytona_cwd": "/testbed",
            "agent_name": "developer",
            "ci_service": _ci_service(),
        }
    )

    result, events = await _run_with_events(
        daytona_codeact,
        {"command": "cd /testbed && pytest tests/unit/test_x.py -q 2>&1"},
        ctx,
    )

    data = _assert_ok(result)
    assert (
        data["shell_outputs"][0]["command"]
        == "cd /testbed && pytest tests/unit/test_x.py -q 2>&1"
    )
    assert data["warnings"] == []
    texts = _notification_texts(events)
    assert not any("pre-hook advisory" in text for text in texts)


async def test_python_mode_preserves_literal_shell_calls_for_team_agents():
    original_code = 'shell("cd /testbed && pytest tests/unit/test_x.py -q 2>&1")'
    manifest = _make_manifest(
        shells=[
            {
                "command": "cd /testbed && pytest tests/unit/test_x.py -q 2>&1",
                "stdout": "ok",
                "stderr": "",
                "exit_code": 0,
            }
        ]
    )
    sb = _make_sandbox(manifest=manifest)
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
        daytona_codeact,
        {"code": original_code},
        ctx,
    )

    data = _assert_ok(result)
    assert (
        data["shell_outputs"][0]["command"]
        == "cd /testbed && pytest tests/unit/test_x.py -q 2>&1"
    )
    texts = _notification_texts(events)
    assert not any("pre-hook advisory" in text for text in texts)
    wrapper = _submitted_wrapper(svc)
    match = re.search(r'_CODE = base64\.b64decode\("([^"]+)"\)', wrapper)
    assert match is not None
    submitted_code = base64.b64decode(match.group(1)).decode("utf-8")
    assert submitted_code == original_code
