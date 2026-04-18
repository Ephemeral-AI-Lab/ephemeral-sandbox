"""Tests for tools.daytona_toolkit.codeact_tool."""

from __future__ import annotations

import json
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

from tools.core.base import ToolExecutionContext
from tools.daytona_toolkit import codeact_tool as codeact_tool_module
from tools.daytona_toolkit.codeact_tool import (
    _build_exec_command,
    _build_wrapper,
    _normalize_team_shell_command,
    daytona_codeact,
)

pytestmark = pytest.mark.asyncio


def _ctx(metadata=None) -> ToolExecutionContext:
    return ToolExecutionContext(cwd=Path("/tmp"), metadata=metadata or {})


def _ci_service():
    svc = MagicMock()
    svc.exec_process_operation = AsyncMock(side_effect=_exec_process_operation)
    return svc


async def _exec_process_operation(
    sandbox,
    command,
    *,
    timeout=None,
    description="",
    agent_id="",
    team_run_id="",
    agent_run_id="",
    task_id="",
    attribute_changes=True,
):
    del description, agent_id, team_run_id, agent_run_id, task_id, attribute_changes
    return await sandbox.process.exec(command, timeout=timeout)


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

    if upload_side_effect is not None:
        sb.fs.upload_file = AsyncMock(side_effect=upload_side_effect)
    elif upload_exc:
        sb.fs.upload_file = AsyncMock(side_effect=upload_exc)
    else:
        sb.fs.upload_file = AsyncMock()

    if exec_exc:
        sb.process.exec = AsyncMock(side_effect=exec_exc)
    else:
        default_exec = exec_stdout or json.dumps({"manifest": "/tmp/codeact-xxx.json", "status": "ok"})
        sb.process.exec = AsyncMock(return_value=MagicMock(result=default_exec))

    if download_exc:
        sb.fs.download_file = AsyncMock(side_effect=download_exc)
    else:
        payload = json.dumps(manifest or _make_manifest()).encode()
        sb.fs.download_file = AsyncMock(return_value=payload)

    return sb


def _assert_ok(result) -> dict:
    assert not result.is_error, result.output
    return json.loads(result.output)


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


async def test_build_wrapper_uses_write_through_and_guarded_imports():
    wrapper = _build_wrapper(
        "write('file.txt', 'ok')",
        run_id="abcd1234",
        cwd="/repo",
        repo_root="/repo",
        enforce_team_shell_policy=True,
        disable_codeact_file_edits=False,
    )
    assert 'with open(resolved, "w", encoding="utf-8")' in wrapper
    assert "_guarded_import" in wrapper
    assert "_BLOCKED_MODULES" in wrapper
    assert "_ENFORCE_TEAM_SHELL_POLICY = True" in wrapper
    assert "_DISABLE_CODEACT_FILE_EDITS = False" in wrapper


async def test_build_wrapper_can_disable_python_file_edits():
    wrapper = _build_wrapper(
        "write('file.txt', 'ok')",
        run_id="abcd1234",
        cwd="/repo",
        repo_root="/repo",
        enforce_team_shell_policy=True,
        disable_codeact_file_edits=True,
    )
    assert "_DISABLE_CODEACT_FILE_EDITS = True" in wrapper
    assert "raise RuntimeError(_CODEACT_FILE_EDIT_POLICY_MESSAGE)" in wrapper
    assert '_sandbox_builtins["open"] = _guarded_open' in wrapper
    assert "_codeact_shell_file_edit_error" in wrapper
    assert "_CODEACT_SHELL_FILE_EDIT_PATTERNS" in wrapper
    assert "_codeact_shell_file_read_error" not in wrapper
    assert "CodeAct read() helper" not in wrapper
    assert "Python open() file inspection" not in wrapper
    assert "linecache.getlines" not in wrapper
    assert "inspect.getsource" not in wrapper
    assert "pathlib.Path.read_text" not in wrapper
    assert "io.open = _guarded_io_open" in wrapper


async def test_build_exec_command_runs_wrapper_from_repo_cwd():
    command = _build_exec_command("/tmp/codeact-wrapper-abcd1234.py", cwd="/repo")
    assert "bash -o pipefail -lc" in command
    assert 'cd "/repo" && python3 /tmp/codeact-wrapper-abcd1234.py' in command


async def test_normalize_team_shell_command_strips_repo_cd_and_capture_plumbing():
    command, warnings = _normalize_team_shell_command(
        "cd /testbed && pytest dask/tests/test_cli.py -q 2>&1 | head -100",
        repo_root="/testbed",
    )

    assert command == "pytest dask/tests/test_cli.py -q | head -100"
    assert any("cd <repo-root>" in warning for warning in warnings)
    assert any("2>&1" in warning for warning in warnings)


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


async def test_shell_mode_allows_audited_test_suite_write_with_warning():
    sb = _make_sandbox()
    svc = MagicMock()
    svc.exec_process_operation = AsyncMock(
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

    result = await daytona_codeact.execute(
        daytona_codeact.input_model(command="sed -i s/old/new/ dask/tests/test_cli.py"),
        ctx,
    )

    assert not result.is_error
    data = json.loads(result.output)
    assert data["status"] == "ok"
    assert data["files_written"] == 1
    assert any("outside write_scope" in warning for warning in data["warnings"])


@pytest.mark.parametrize(
    ("command", "expected_fragment"),
    [
        ("sed -i s/old/new/ dask/core.py", "in-place sed"),
        (
            "python -c \"from pathlib import Path; Path('dask/core.py').write_text('x')\"",
            "inline Python file mutation",
        ),
        ("printf x > dask/core.py", "shell output redirection"),
        ("printf x | tee dask/core.py", "tee file write"),
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

    result = await daytona_codeact.execute(
        daytona_codeact.input_model(command=command),
        ctx,
    )

    assert result.is_error
    assert "BLOCKED: daytona_codeact is for runtime commands" in result.output
    assert expected_fragment in result.output
    assert "daytona_edit_file" in result.output
    svc.exec_process_operation.assert_not_awaited()
    sb.process.exec.assert_not_awaited()


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
    svc.exec_process_operation.assert_awaited_once()
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
    sb.fs.upload_file.assert_awaited_once()
    svc.exec_process_operation.assert_awaited_once()


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
    svc.exec_process_operation.assert_awaited_once()


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
    svc.exec_process_operation.assert_awaited_once()


async def test_team_shell_mode_treats_audited_changes_as_ambient():
    sb = _make_sandbox()

    async def exec_process_operation(
        sandbox,
        command,
        *,
        timeout=None,
        description="",
        agent_id="",
        team_run_id="",
        agent_run_id="",
        task_id="",
        attribute_changes=True,
    ):
        del sandbox, command, timeout, description, agent_id, team_run_id, agent_run_id, task_id
        assert attribute_changes is False
        return SimpleNamespace(
            result=_shell_exec_output("ujson ok", 0),
            exit_code=0,
            changed_paths=[],
            ambient_changed_paths=["/testbed/dask/_compatibility.py"],
            files_written=0,
        )

    svc = MagicMock()
    svc.exec_process_operation = AsyncMock(side_effect=exec_process_operation)
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

    result = await daytona_codeact.execute(
        daytona_codeact.input_model(command="python -c 'print(\"ujson ok\")'"),
        ctx,
    )

    data = _assert_ok(result)
    assert data["files_written"] == 0
    assert any("ambient concurrent edits" in warning for warning in data["warnings"])
    assert not any("outside write_scope" in warning for warning in data["warnings"])


async def test_shell_mode_warns_for_audited_outside_scope_write():
    sb = _make_sandbox()
    svc = MagicMock()
    svc.exec_process_operation = AsyncMock(
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

    result = await daytona_codeact.execute(
        daytona_codeact.input_model(command="python - <<'PY'\nprint('patched')\nPY"),
        ctx,
    )

    data = _assert_ok(result)
    assert data["files_written"] == 1
    assert any("outside write_scope" in warning for warning in data["warnings"])


@pytest.mark.parametrize(
    ("code", "expected_fragment"),
    [
        ("write('dask/core.py', 'x')", "CodeAct write() helper"),
        ("open('dask/core.py', 'w').write('x')", "write-mode open()"),
        ("from pathlib import Path\nPath('dask/core.py').write_text('x')", "Path.write_text"),
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

    result = await daytona_codeact.execute(
        daytona_codeact.input_model(code=code),
        ctx,
    )

    assert result.is_error
    assert "BLOCKED: daytona_codeact is for runtime commands" in result.output
    assert expected_fragment in result.output
    sb.fs.upload_file.assert_not_called()
    svc.exec_process_operation.assert_not_awaited()


async def test_python_mode_preserves_script_stdout_before_manifest_line():
    manifest = _make_manifest()
    exec_stdout = 'hello from codeact\n{"manifest": "/tmp/codeact-xxx.json", "status": "ok"}'
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
    assert sb.fs.upload_file.call_count == 1


async def test_python_mode_error_uses_updated_guidance():
    error_result = json.dumps({"manifest": "/tmp/xxx.json", "status": "error"})
    manifest = _make_manifest(
        status="error",
        error="ImportError: import 'subprocess' is blocked in codeact.",
    )
    sb = _make_sandbox(exec_stdout=error_result, manifest=manifest)
    ctx = _ctx({"daytona_sandbox": sb, "daytona_cwd": "/repo", "ci_service": _ci_service()})

    result = await daytona_codeact.execute(
        daytona_codeact.input_model(code="raise RuntimeError('boom')"),
        ctx,
    )

    assert result.is_error
    assert "ImportError" in result.output
    assert "daytona_codeact(command=" in result.output


@pytest.mark.parametrize(
    ("code", "manifest_error", "expected_fragment", "expect_guidance"),
    [
        (
            "import subprocess\nsubprocess.run(['python', '-m', 'pytest'])",
            "ImportError: import 'subprocess' is blocked in codeact.",
            "ImportError",
            True,
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
async def test_coordinated_python_mode_enforces_runtime_shell_policy(
    code,
    manifest_error,
    expected_fragment,
    expect_guidance,
):
    error_result = json.dumps({"manifest": "/tmp/codeact-xxx.json", "status": "error"})
    sb = _make_sandbox(
        exec_stdout=error_result,
        manifest=_make_manifest(status="error", error=manifest_error),
    )
    ctx = _ctx(
        {
            "daytona_sandbox": sb,
            "daytona_cwd": "/repo",
            "agent_name": "developer",
            "ci_service": _ci_service(),
        }
    )

    result = await daytona_codeact.execute(
        daytona_codeact.input_model(code=code),
        ctx,
    )

    assert result.is_error
    assert expected_fragment in result.output
    if expect_guidance:
        assert "daytona_codeact(command=" in result.output
    sb.fs.upload_file.assert_awaited_once()


async def test_shell_mode_normalizes_stderr_merge_for_team_agents():
    sb = _make_sandbox(exec_stdout=_shell_exec_output("ok", 0))
    ctx = _ctx(
        {
            "daytona_sandbox": sb,
            "daytona_cwd": "/testbed",
            "agent_name": "developer",
            "ci_service": _ci_service(),
        }
    )

    result = await daytona_codeact.execute(
        daytona_codeact.input_model(command="cd /testbed && pytest tests/unit/test_x.py -q 2>&1"),
        ctx,
    )

    data = _assert_ok(result)
    assert data["shell_outputs"][0]["command"] == "pytest tests/unit/test_x.py -q"
    assert any("2>&1" in warning for warning in data["warnings"])
    assert any("cd <repo-root>" in warning for warning in data["warnings"])
