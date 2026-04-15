"""Tests for tools.daytona_toolkit.codeact_tool."""

from __future__ import annotations

import json
from pathlib import Path
from unittest.mock import AsyncMock, MagicMock

import pytest

from tools.core.base import ToolExecutionContext
from tools.daytona_toolkit import codeact_tool as codeact_tool_module
from tools.daytona_toolkit.codeact_tool import (
    _build_exec_command,
    _build_wrapper,
    daytona_codeact,
)
from tools.daytona_toolkit.codeact_transaction import (
    CodeActTransaction,
    CommitReport,
    FileCommitResult,
    RepoChange,
)

pytestmark = pytest.mark.asyncio


def _ctx(metadata=None) -> ToolExecutionContext:
    return ToolExecutionContext(cwd=Path("/tmp"), metadata=metadata or {})


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


def _tx() -> CodeActTransaction:
    return CodeActTransaction(
        repo_root="/repo",
        scratch_root="/scratch",
        base_tree="deadbeef",
        patch_path="/tmp/codeact.patch",
    )


async def test_codeact_no_sandbox_returns_error():
    ctx = _ctx()
    result = await daytona_codeact.execute(daytona_codeact.input_model(code="print('hi')"), ctx)
    assert result.is_error
    assert "No Daytona sandbox" in result.output


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
        enforce_team_shell_policy=True,
    )
    assert 'with open(resolved, "w", encoding="utf-8")' in wrapper
    assert "_guarded_import" in wrapper
    assert "_BLOCKED_MODULES" in wrapper
    assert "_ENFORCE_TEAM_SHELL_POLICY = True" in wrapper


async def test_build_exec_command_runs_wrapper_from_repo_cwd():
    command = _build_exec_command("/tmp/codeact-wrapper-abcd1234.py", cwd="/repo")
    assert "bash -o pipefail -lc" in command
    assert 'cd "/repo" && python3 /tmp/codeact-wrapper-abcd1234.py' in command


async def test_shell_mode_runs_command_directly():
    sb = _make_sandbox(exec_stdout=_shell_exec_output("LIVE_BASH_OK", 0))
    ctx = _ctx({"daytona_sandbox": sb, "daytona_cwd": "/repo"})

    result = await daytona_codeact.execute(
        daytona_codeact.input_model(command="echo LIVE_BASH_OK", timeout=25),
        ctx,
    )

    data = _assert_ok(result)
    assert data["status"] == "ok"
    assert data["shells_run"] == 1
    assert data["files_written"] == 0
    assert "LIVE_BASH_OK" in data["shell_outputs"][0]["stdout"]


async def test_shell_mode_reports_nonzero_exit_as_error():
    sb = _make_sandbox(exec_stdout=_shell_exec_output("cat: missing", 1))
    ctx = _ctx({"daytona_sandbox": sb, "daytona_cwd": "/repo"})

    result = await daytona_codeact.execute(
        daytona_codeact.input_model(command="cat /missing"),
        ctx,
    )

    assert result.is_error
    data = json.loads(result.output)
    assert data["status"] == "error"
    assert data["shells_run"] == 1


async def test_coordinated_read_only_shell_uses_transaction_without_writes(monkeypatch):
    sb = _make_sandbox(exec_stdout=_shell_exec_output("ok", 0))
    ctx = _ctx(
        {
            "daytona_sandbox": sb,
            "daytona_cwd": "/repo",
            "agent_name": "developer",
            "team_mode_enabled": True,
        }
    )
    tx = _tx()
    monkeypatch.setattr(
        codeact_tool_module,
        "_resolve_repo_root",
        AsyncMock(return_value=("/repo", sb, None)),
    )
    create_tx = AsyncMock(return_value=tx)
    collect_changes = AsyncMock(return_value=[])
    commit_changes = AsyncMock(return_value=CommitReport())
    cleanup = AsyncMock()
    monkeypatch.setattr(codeact_tool_module, "create_codeact_transaction", create_tx)
    monkeypatch.setattr(codeact_tool_module, "collect_transaction_changes", collect_changes)
    monkeypatch.setattr(codeact_tool_module, "commit_transaction_changes", commit_changes)
    monkeypatch.setattr(codeact_tool_module, "cleanup_codeact_transaction", cleanup)

    result = await daytona_codeact.execute(
        daytona_codeact.input_model(command="pytest tests/unit/test_x.py -q"),
        ctx,
    )

    data = _assert_ok(result)
    assert data["status"] == "ok"
    assert data["files_written"] == 0
    create_tx.assert_awaited_once()
    collect_changes.assert_awaited_once()
    commit_changes.assert_awaited_once()
    cleanup.assert_awaited_once()


async def test_coordinated_mutating_shell_uses_transaction(monkeypatch):
    sb = _make_sandbox(exec_stdout=_shell_exec_output("", 0))
    ctx = _ctx(
        {
            "daytona_sandbox": sb,
            "daytona_cwd": "/repo",
            "agent_name": "developer",
            "team_mode_enabled": True,
        }
    )
    tx = _tx()
    monkeypatch.setattr(
        codeact_tool_module,
        "_resolve_repo_root",
        AsyncMock(return_value=("/repo", sb, None)),
    )
    monkeypatch.setattr(
        codeact_tool_module,
        "create_codeact_transaction",
        AsyncMock(return_value=tx),
    )
    monkeypatch.setattr(
        codeact_tool_module,
        "collect_transaction_changes",
        AsyncMock(
            return_value=[
                RepoChange(
                    path="changed.py",
                    status="modified",
                    base_content="x = 1\n",
                    final_content="x = 2\n",
                )
            ]
        ),
    )
    monkeypatch.setattr(
        codeact_tool_module,
        "commit_transaction_changes",
        AsyncMock(
            return_value=CommitReport(
                committed=[FileCommitResult(path="changed.py", status="ok")],
                warnings=["outside write_scope (advisory)"],
            )
        ),
    )
    cleanup = AsyncMock()
    monkeypatch.setattr(codeact_tool_module, "cleanup_codeact_transaction", cleanup)

    result = await daytona_codeact.execute(
        daytona_codeact.input_model(command="echo hi > changed.py"),
        ctx,
    )

    data = _assert_ok(result)
    assert data["files_written"] == 1
    assert data["warnings"] == ["outside write_scope (advisory)"]
    cleanup.assert_awaited_once()


async def test_coordinated_shell_skips_commit_on_command_failure(monkeypatch):
    sb = _make_sandbox(exec_stdout=_shell_exec_output("boom", 2))
    ctx = _ctx(
        {
            "daytona_sandbox": sb,
            "daytona_cwd": "/repo",
            "agent_name": "developer",
            "team_mode_enabled": True,
        }
    )
    tx = _tx()
    monkeypatch.setattr(
        codeact_tool_module,
        "_resolve_repo_root",
        AsyncMock(return_value=("/repo", sb, None)),
    )
    monkeypatch.setattr(
        codeact_tool_module,
        "create_codeact_transaction",
        AsyncMock(return_value=tx),
    )
    collect_changes = AsyncMock()
    commit_changes = AsyncMock()
    cleanup = AsyncMock()
    monkeypatch.setattr(codeact_tool_module, "collect_transaction_changes", collect_changes)
    monkeypatch.setattr(codeact_tool_module, "commit_transaction_changes", commit_changes)
    monkeypatch.setattr(codeact_tool_module, "cleanup_codeact_transaction", cleanup)

    result = await daytona_codeact.execute(
        daytona_codeact.input_model(command="false"),
        ctx,
    )

    assert result.is_error
    data = json.loads(result.output)
    assert data["files_written"] == 0
    collect_changes.assert_not_awaited()
    commit_changes.assert_not_awaited()
    cleanup.assert_awaited_once()


async def test_python_mode_preserves_script_stdout_before_manifest_line():
    manifest = _make_manifest()
    exec_stdout = 'hello from codeact\n{"manifest": "/tmp/codeact-xxx.json", "status": "ok"}'
    sb = _make_sandbox(exec_stdout=exec_stdout, manifest=manifest)
    ctx = _ctx({"daytona_sandbox": sb})

    result = await daytona_codeact.execute(
        daytona_codeact.input_model(code="print('hello from codeact')"),
        ctx,
    )

    data = _assert_ok(result)
    assert data["script_stdout"] == "hello from codeact"


async def test_python_mode_counts_coalesced_write_through_paths():
    manifest = _make_manifest(
        writes=[
            {"path": "/repo/a.py", "content": "a = 1\n"},
            {"path": "/repo/a.py", "content": "a = 2\n"},
            {"path": "/repo/b.py", "content": "b = 1\n"},
        ]
    )
    sb = _make_sandbox(manifest=manifest)
    ctx = _ctx({"daytona_sandbox": sb, "daytona_cwd": "/repo"})

    result = await daytona_codeact.execute(
        daytona_codeact.input_model(code="write('a.py', 'a = 2\\n')"),
        ctx,
    )

    data = _assert_ok(result)
    assert data["files_written"] == 2
    assert sb.fs.upload_file.call_count == 1


async def test_python_mode_error_uses_updated_guidance():
    error_result = json.dumps({"manifest": "/tmp/xxx.json", "status": "error"})
    manifest = _make_manifest(
        status="error",
        error="ImportError: import 'subprocess' is blocked in codeact.",
    )
    sb = _make_sandbox(exec_stdout=error_result, manifest=manifest)
    ctx = _ctx({"daytona_sandbox": sb})

    result = await daytona_codeact.execute(
        daytona_codeact.input_model(code="raise RuntimeError('boom')"),
        ctx,
    )

    assert result.is_error
    assert "ImportError" in result.output
    assert "daytona_codeact(command=" in result.output


def _patch_coordinated_transaction(monkeypatch, sb):
    tx = _tx()
    monkeypatch.setattr(
        codeact_tool_module,
        "_resolve_repo_root",
        AsyncMock(return_value=("/repo", sb, None)),
    )
    monkeypatch.setattr(codeact_tool_module, "create_codeact_transaction", AsyncMock(return_value=tx))
    collect_changes = AsyncMock()
    commit_changes = AsyncMock()
    cleanup = AsyncMock()
    monkeypatch.setattr(codeact_tool_module, "collect_transaction_changes", collect_changes)
    monkeypatch.setattr(codeact_tool_module, "commit_transaction_changes", commit_changes)
    monkeypatch.setattr(codeact_tool_module, "cleanup_codeact_transaction", cleanup)
    return collect_changes, commit_changes, cleanup


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
        (
            "shell('pytest -q 2>&1')",
            "RuntimeError: CodeAct policy error: do not append `2>&1`; stdout/stderr are already captured.",
            "2>&1",
            False,
        ),
    ],
)
async def test_coordinated_python_mode_enforces_runtime_shell_policy(
    monkeypatch,
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
            "team_mode_enabled": True,
        }
    )
    collect_changes, commit_changes, cleanup = _patch_coordinated_transaction(monkeypatch, sb)

    result = await daytona_codeact.execute(
        daytona_codeact.input_model(code=code),
        ctx,
    )

    assert result.is_error
    assert expected_fragment in result.output
    if expect_guidance:
        assert "daytona_codeact(command=" in result.output
    collect_changes.assert_not_awaited()
    commit_changes.assert_not_awaited()
    cleanup.assert_awaited_once()
    sb.fs.upload_file.assert_awaited_once()


async def test_shell_mode_blocks_stderr_merge_for_team_agents():
    sb = _make_sandbox()
    ctx = _ctx(
        {
            "daytona_sandbox": sb,
            "agent_name": "developer",
            "team_mode_enabled": True,
        }
    )

    result = await daytona_codeact.execute(
        daytona_codeact.input_model(command="pytest tests/unit/test_x.py -q 2>&1"),
        ctx,
    )

    assert result.is_error
    assert "2>&1" in result.output
    sb.process.exec.assert_not_called()


async def test_coordinated_python_mode_uses_transaction_and_surfaces_report(monkeypatch):
    manifest = _make_manifest(
        writes=[{"path": "/scratch/pkg.py", "content": "x = 2\n"}],
        shells=[{"command": "pytest -q", "stdout": "ok\n", "stderr": "", "exit_code": 0}],
    )
    exec_stdout = json.dumps({"manifest": "/tmp/codeact-xxx.json", "status": "ok"})
    sb = _make_sandbox(exec_stdout=exec_stdout, manifest=manifest)
    ctx = _ctx(
        {
            "daytona_sandbox": sb,
            "daytona_cwd": "/repo",
            "agent_name": "developer",
            "team_mode_enabled": True,
        }
    )
    tx = _tx()
    monkeypatch.setattr(
        codeact_tool_module,
        "_resolve_repo_root",
        AsyncMock(return_value=("/repo", sb, None)),
    )
    monkeypatch.setattr(codeact_tool_module, "create_codeact_transaction", AsyncMock(return_value=tx))
    monkeypatch.setattr(
        codeact_tool_module,
        "collect_transaction_changes",
        AsyncMock(
            return_value=[
                RepoChange(
                    path="pkg.py",
                    status="modified",
                    base_content="x = 1\n",
                    final_content="x = 2\n",
                )
            ]
        ),
    )
    monkeypatch.setattr(
        codeact_tool_module,
        "commit_transaction_changes",
        AsyncMock(
            return_value=CommitReport(
                committed=[FileCommitResult(path="pkg.py", status="ok")],
                conflicts=[FileCommitResult(path="conflict.py", status="conflict", message="overlap")],
                errors=[FileCommitResult(path="bin.dat", status="unsupported", message="binary unsupported")],
                warnings=["write scope warning"],
            )
        ),
    )
    cleanup = AsyncMock()
    monkeypatch.setattr(codeact_tool_module, "cleanup_codeact_transaction", cleanup)

    result = await daytona_codeact.execute(
        daytona_codeact.input_model(code="write('pkg.py', 'x = 2\\n')"),
        ctx,
    )

    assert result.is_error
    data = json.loads(result.output)
    assert data["files_written"] == 1
    assert data["write_conflicts"] == ["/repo/conflict.py"]
    assert data["write_errors"] == ["binary unsupported"]
    assert data["warnings"] == ["write scope warning"]
    assert 'cd "/scratch" && python3 /tmp/codeact-wrapper-' in sb.process.exec.await_args.args[0]
    cleanup.assert_awaited_once()


async def test_coordinated_python_mode_does_not_commit_on_wrapper_error(monkeypatch):
    error_stdout = json.dumps({"manifest": "/tmp/codeact-xxx.json", "status": "error"})
    sb = _make_sandbox(
        exec_stdout=error_stdout,
        manifest=_make_manifest(status="error", error="Traceback: boom"),
    )
    ctx = _ctx(
        {
            "daytona_sandbox": sb,
            "daytona_cwd": "/repo",
            "agent_name": "developer",
            "team_mode_enabled": True,
        }
    )
    tx = _tx()
    monkeypatch.setattr(
        codeact_tool_module,
        "_resolve_repo_root",
        AsyncMock(return_value=("/repo", sb, None)),
    )
    monkeypatch.setattr(codeact_tool_module, "create_codeact_transaction", AsyncMock(return_value=tx))
    collect_changes = AsyncMock()
    commit_changes = AsyncMock()
    cleanup = AsyncMock()
    monkeypatch.setattr(codeact_tool_module, "collect_transaction_changes", collect_changes)
    monkeypatch.setattr(codeact_tool_module, "commit_transaction_changes", commit_changes)
    monkeypatch.setattr(codeact_tool_module, "cleanup_codeact_transaction", cleanup)

    result = await daytona_codeact.execute(
        daytona_codeact.input_model(code="raise RuntimeError('boom')"),
        ctx,
    )

    assert result.is_error
    collect_changes.assert_not_awaited()
    commit_changes.assert_not_awaited()
    cleanup.assert_awaited_once()
