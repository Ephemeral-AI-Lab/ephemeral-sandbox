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
    daytona_codeact,
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
    """Return a sandbox mock configured for codeact scenarios."""
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
        result_line = json.dumps({"manifest": "/tmp/codeact-xxx.json", "status": "ok"})
        sb.process.exec = AsyncMock(return_value=MagicMock(result=exec_stdout or result_line))

    if download_exc:
        sb.fs.download_file = AsyncMock(side_effect=download_exc)
    elif manifest is not None:
        sb.fs.download_file = AsyncMock(return_value=json.dumps(manifest).encode())
    else:
        default_manifest = _make_manifest()
        sb.fs.download_file = AsyncMock(return_value=json.dumps(default_manifest).encode())

    return sb


def _assert_ok(result) -> dict:
    """Assert result is not an error and return the parsed JSON output."""
    assert not result.is_error
    return json.loads(result.output)


def _patch_ci_write_helpers(monkeypatch, *, prepare_fn, prepare_intent_fn, finalize_fn):
    """Apply the standard set of CI-write monkeypatches used across coordinated-write tests."""
    monkeypatch.setattr(codeact_tool_module, "prepare_ci_write", prepare_fn)
    monkeypatch.setattr(codeact_tool_module, "prepare_ci_edit_intent", prepare_intent_fn)
    monkeypatch.setattr(codeact_tool_module, "finalize_ci_write", finalize_fn)
    monkeypatch.setattr(codeact_tool_module, "release_ci_edit_intent", lambda *args: None)
    monkeypatch.setattr(codeact_tool_module, "abort_ci_write", lambda *args: None)


# ---------------------------------------------------------------------------
# No sandbox
# ---------------------------------------------------------------------------


async def test_codeact_no_sandbox_returns_error():
    ctx = _ctx()
    result = await daytona_codeact.execute(daytona_codeact.input_model(code="print('hi')"), ctx)
    assert result.is_error
    assert "No Daytona sandbox" in result.output


# ---------------------------------------------------------------------------
# Upload failure
# ---------------------------------------------------------------------------


async def test_codeact_upload_failure():
    sb = _make_sandbox(upload_exc=RuntimeError("disk full"))
    ctx = _ctx({"daytona_sandbox": sb})
    result = await daytona_codeact.execute(daytona_codeact.input_model(code="x = 1"), ctx)
    assert result.is_error
    assert "Failed to upload script" in result.output


# ---------------------------------------------------------------------------
# Execution failure
# ---------------------------------------------------------------------------


async def test_codeact_exec_failure():
    sb = _make_sandbox(exec_exc=RuntimeError("timeout"))
    ctx = _ctx({"daytona_sandbox": sb})
    result = await daytona_codeact.execute(daytona_codeact.input_model(code="x = 1"), ctx)
    assert result.is_error
    assert "Execution failed" in result.output


# ---------------------------------------------------------------------------
# Bad JSON output from script
# ---------------------------------------------------------------------------


async def test_codeact_bad_json_stdout():
    sb = _make_sandbox(exec_stdout="not json at all")
    ctx = _ctx({"daytona_sandbox": sb})
    result = await daytona_codeact.execute(daytona_codeact.input_model(code="x = 1"), ctx)
    # Returns non-error with raw output
    assert not result.is_error
    assert "Script output" in result.output


async def test_codeact_empty_stdout():
    sb = _make_sandbox(exec_stdout="")
    ctx = _ctx({"daytona_sandbox": sb})
    result = await daytona_codeact.execute(daytona_codeact.input_model(code="x = 1"), ctx)
    assert not result.is_error  # empty stdout → json.loads("{}") → no status key


# ---------------------------------------------------------------------------
# Script reports error status
# ---------------------------------------------------------------------------


async def test_codeact_script_error_status():
    error_result = json.dumps({"manifest": "/tmp/xxx.json", "status": "error"})
    sb = _make_sandbox(exec_stdout=error_result)
    ctx = _ctx({"daytona_sandbox": sb})
    result = await daytona_codeact.execute(
        daytona_codeact.input_model(code="raise ValueError('oops')"), ctx
    )
    assert result.is_error
    assert "CodeAct execution error" in result.output


# ---------------------------------------------------------------------------
# Missing manifest path
# ---------------------------------------------------------------------------


async def test_codeact_no_manifest_path():
    no_manifest = json.dumps({"status": "ok"})  # no "manifest" key
    sb = _make_sandbox(exec_stdout=no_manifest)
    ctx = _ctx({"daytona_sandbox": sb})
    result = await daytona_codeact.execute(daytona_codeact.input_model(code="x = 1"), ctx)
    assert "Script output" in result.output


# ---------------------------------------------------------------------------
# Manifest unreadable
# ---------------------------------------------------------------------------


async def test_codeact_manifest_download_failure():
    sb = _make_sandbox(download_exc=RuntimeError("gone"))
    ctx = _ctx({"daytona_sandbox": sb})
    result = await daytona_codeact.execute(daytona_codeact.input_model(code="x = 1"), ctx)
    assert "manifest unreadable" in result.output


# ---------------------------------------------------------------------------
# Successful run — no writes, no shells
# ---------------------------------------------------------------------------


async def test_codeact_success_no_writes():
    manifest = _make_manifest()
    sb = _make_sandbox(manifest=manifest)
    ctx = _ctx({"daytona_sandbox": sb, "daytona_cwd": "/ws"})
    result = await daytona_codeact.execute(daytona_codeact.input_model(code="x = 1 + 1"), ctx)
    data = _assert_ok(result)
    assert data["status"] == "ok"
    assert data["files_written"] == 0
    assert data["shells_run"] == 0
    assert data["cwd"] == "/ws"


async def test_build_wrapper_uses_bash_and_repo_cwd_for_shell_helper():
    wrapper = _build_wrapper("shell('pytest -q')", run_id="abcd1234", cwd="/testbed")

    assert '["env", "-u", "LC_ALL", "bash", "-o", "pipefail", "-lc", wrapped]' in wrapper
    assert "cwd=_CODEACT_CWD or None" in wrapper
    assert '_CODEACT_CWD = "/testbed"' in wrapper
    assert 'export PATH="$HOME/.local/bin:$PATH"' in wrapper
    assert 'if [ -d .venv/bin ]; then export PATH="$PWD/.venv/bin:$PATH"; fi' in wrapper


async def test_build_wrapper_embeds_declared_shell_output_guard():
    wrapper = _build_wrapper(
        "shell(\"sed -i 's/a/b/' out.py\")",
        run_id="abcd1234",
        cwd="/testbed",
        require_declared_shell_outputs=True,
        declared_output_paths=[],
    )

    assert "_REQUIRE_DECLARED_SHELL_OUTPUTS = True" in wrapper
    assert "Mutating shell calls must declare `declared_output_paths`" in wrapper


async def test_build_exec_command_runs_wrapper_from_repo_cwd():
    command = _build_exec_command("/tmp/codeact-wrapper-abcd1234.py", cwd="/testbed")

    assert "bash -o pipefail -lc" in command
    assert 'export PATH="$HOME/.local/bin:$PATH"' in command
    assert 'cd "/testbed" && python3 /tmp/codeact-wrapper-abcd1234.py' in command


# ---------------------------------------------------------------------------
# Successful run — with writes committed
# ---------------------------------------------------------------------------


async def test_codeact_success_with_writes():
    manifest = _make_manifest(
        writes=[
            {"path": "/ws/out.py", "content": "x = 42\n"},
            {"path": "/ws/other.py", "content": "y = 1\n"},
        ]
    )
    sb = _make_sandbox(manifest=manifest)
    ctx = _ctx({"daytona_sandbox": sb})
    result = await daytona_codeact.execute(
        daytona_codeact.input_model(code="write('/ws/out.py', 'x = 42\\n')"), ctx
    )
    data = _assert_ok(result)
    assert data["files_written"] == 2
    # upload_file called once for the script upload + twice for the writes
    assert sb.fs.upload_file.call_count == 3


async def test_codeact_uses_ci_write_flow_for_helper_staged_writes(monkeypatch):
    manifest = _make_manifest(
        reads=[{"path": "/ws/out.py", "hash": "read-hash-1234"}],
        writes=[{"path": "/ws/out.py", "content": "x = 42\n"}],
    )
    sb = _make_sandbox(manifest=manifest)
    ctx = _ctx({"daytona_sandbox": sb})
    prepared = SimpleNamespace(file_path="/ws/out.py")
    seen: dict[str, object] = {}

    def fake_prepare_ci_write(context, path, *, expected_hash="", allow_scope_drift=False):
        seen["path"] = path
        seen["expected_hash"] = expected_hash
        seen["allow_scope_drift"] = allow_scope_drift
        return prepared, {"scope_paths": [path], "coherence_token": "tok"}, None

    def fake_prepare_ci_edit_intent(context, prepared_write, *, content):
        seen["intent_content"] = content
        return prepared_write, "intent-1"

    def fake_finalize_ci_write(context, prepared_write, *, content, edit_type, description):
        seen["edit_type"] = edit_type
        seen["description"] = description
        return SimpleNamespace(success=True)

    _patch_ci_write_helpers(
        monkeypatch,
        prepare_fn=fake_prepare_ci_write,
        prepare_intent_fn=fake_prepare_ci_edit_intent,
        finalize_fn=fake_finalize_ci_write,
    )

    result = await daytona_codeact.execute(
        daytona_codeact.input_model(code="write('/ws/out.py', 'x = 42\\n')"),
        ctx,
    )

    data = _assert_ok(result)
    assert data["files_written"] == 1
    assert data["write_errors"] == []
    assert sb.fs.upload_file.call_count == 1
    assert seen == {
        "path": "/ws/out.py",
        "expected_hash": "read-hash-1234",
        "allow_scope_drift": True,
        "intent_content": "x = 42\n",
        "edit_type": "codeact",
        "description": "daytona_codeact",
    }


async def test_codeact_surfaces_ci_conflicts_for_helper_staged_writes(monkeypatch):
    manifest = _make_manifest(
        reads=[{"path": "/ws/out.py", "hash": "read-hash-1234"}],
        writes=[{"path": "/ws/out.py", "content": "x = 42\n"}],
    )
    sb = _make_sandbox(manifest=manifest)
    ctx = _ctx({"daytona_sandbox": sb})
    prepared = SimpleNamespace(file_path="/ws/out.py")

    _patch_ci_write_helpers(
        monkeypatch,
        prepare_fn=lambda *args, **kwargs: (prepared, {"scope_paths": ["/ws/out.py"]}, None),
        prepare_intent_fn=lambda context, prepared_write, *, content: (prepared_write, "intent-1"),
        finalize_fn=lambda *args, **kwargs: SimpleNamespace(
            success=False,
            conflict=True,
            message="Write precheck failed: stale_reservation",
        ),
    )

    result = await daytona_codeact.execute(
        daytona_codeact.input_model(code="write('/ws/out.py', 'x = 42\\n')"),
        ctx,
    )

    assert result.is_error
    data = json.loads(result.output)
    assert data["files_written"] == 0
    assert data["write_conflicts"] == ["/ws/out.py"]
    assert "stale_reservation" in data["write_errors"][0]
    assert result.metadata["conflict"] is True
    assert sb.fs.upload_file.call_count == 1


async def test_codeact_executes_wrapper_from_repo_cwd():
    manifest = _make_manifest()
    sb = _make_sandbox(manifest=manifest)
    ctx = _ctx({"daytona_sandbox": sb, "daytona_cwd": "/testbed"})

    result = await daytona_codeact.execute(daytona_codeact.input_model(code="print('hi')"), ctx)

    _assert_ok(result)
    command = sb.process.exec.await_args.args[0]
    assert "bash -o pipefail -lc" in command
    assert 'cd "/testbed" && python3 /tmp/codeact-wrapper-' in command


# ---------------------------------------------------------------------------
# Write commit failure (partial writes)
# ---------------------------------------------------------------------------


async def test_codeact_write_commit_failure():
    manifest = _make_manifest(writes=[{"path": "/ws/bad.py", "content": "oops"}])
    # First upload (script) succeeds, second (write commit) fails
    sb = _make_sandbox(manifest=manifest, upload_side_effect=[None, RuntimeError("commit fail")])
    ctx = _ctx({"daytona_sandbox": sb})

    result = await daytona_codeact.execute(
        daytona_codeact.input_model(code="write('/ws/bad.py', 'oops')"), ctx
    )
    assert result.is_error
    data = json.loads(result.output)
    assert len(data["write_errors"]) == 1
    assert "commit fail" in data["write_errors"][0]


# ---------------------------------------------------------------------------
# Shell summaries
# ---------------------------------------------------------------------------


async def test_codeact_shell_summaries():
    manifest = _make_manifest(
        shells=[
            {"command": "ls -la", "exit_code": 0, "stdout": "file-a\nfile-b\n", "stderr": ""},
            {"command": "pytest", "exit_code": 1, "stdout": "", "stderr": "assertion failed"},
        ]
    )
    sb = _make_sandbox(manifest=manifest)
    ctx = _ctx({"daytona_sandbox": sb})
    result = await daytona_codeact.execute(daytona_codeact.input_model(code="shell('ls -la')"), ctx)
    data = _assert_ok(result)
    assert data["shells_run"] == 2
    assert len(data["shell_summaries"]) == 2
    assert "ls -la" in data["shell_summaries"][0]
    assert len(data["shell_outputs"]) == 2
    assert data["shell_outputs"][0]["stdout"] == "file-a\nfile-b\n"
    assert data["shell_outputs"][1]["stderr"] == "assertion failed"


async def test_codeact_reserves_and_syncs_declared_shell_outputs(monkeypatch):
    manifest = _make_manifest(
        shells=[
            {
                "command": "sed -i 's/a/b/' /ws/out.py",
                "exit_code": 0,
                "stdout": "",
                "stderr": "",
            }
        ]
    )
    sb = _make_sandbox(manifest=manifest)
    ctx = _ctx({"daytona_sandbox": sb, "daytona_cwd": "/ws"})
    prepared_item = SimpleNamespace(file_path="/ws/out.py")
    seen: dict[str, object] = {}

    def fake_prepare_declared_shell_outputs(context, *, declared_output_paths):
        seen["prepared_paths"] = declared_output_paths
        return [prepared_item], {"scope_paths": declared_output_paths}, None

    async def fake_sync_shell_mutations(context, *, command, declared_output_paths=None, limit=64):
        seen["sync_command"] = command
        seen["sync_paths"] = declared_output_paths
        return {
            "enabled": True,
            "files": 1,
            "truncated": False,
            "declared_output_paths": declared_output_paths,
        }

    def fake_release_declared_shell_outputs(context, prepared_items):
        seen["released_paths"] = [item.file_path for item in prepared_items]

    monkeypatch.setattr(
        codeact_tool_module,
        "prepare_declared_shell_outputs",
        fake_prepare_declared_shell_outputs,
    )
    monkeypatch.setattr(codeact_tool_module, "sync_shell_mutations", fake_sync_shell_mutations)
    monkeypatch.setattr(
        codeact_tool_module,
        "release_declared_shell_outputs",
        fake_release_declared_shell_outputs,
    )

    result = await daytona_codeact.execute(
        daytona_codeact.input_model(
            code="shell(\"sed -i 's/a/b/' out.py\")",
            declared_output_paths=["out.py"],
        ),
        ctx,
    )

    data = _assert_ok(result)
    assert data["shell_ci_sync"]["files"] == 1
    assert seen == {
        "prepared_paths": ["/ws/out.py"],
        "sync_command": "sed -i 's/a/b/' /ws/out.py",
        "sync_paths": ["/ws/out.py"],
        "released_paths": ["/ws/out.py"],
    }


async def test_codeact_preserves_script_stdout_before_manifest_line():
    manifest = _make_manifest()
    exec_stdout = 'hello from codeact\n{"manifest": "/tmp/codeact-xxx.json", "status": "ok"}'
    sb = _make_sandbox(exec_stdout=exec_stdout, manifest=manifest)
    ctx = _ctx({"daytona_sandbox": sb})
    result = await daytona_codeact.execute(
        daytona_codeact.input_model(code="print('hello from codeact')"), ctx
    )
    data = _assert_ok(result)
    assert data["script_stdout"] == "hello from codeact"


# ---------------------------------------------------------------------------
# Team-mode agnostic: codeact does NOT enforce team constraints
# ---------------------------------------------------------------------------


async def test_codeact_allows_subprocess_calls_in_team_mode():
    """CodeAct is team-agnostic — subprocess calls are not blocked."""
    manifest = _make_manifest()
    sb = _make_sandbox(manifest=manifest)
    ctx = _ctx(
        {
            "daytona_sandbox": sb,
            "agent_name": "developer",
            "team_mode_enabled": True,
        }
    )

    result = await daytona_codeact.execute(
        daytona_codeact.input_model(
            code="import subprocess\nsubprocess.run(['pytest'], check=False)"
        ),
        ctx,
    )

    assert not result.is_error


async def test_codeact_allows_writes_from_validator():
    """CodeAct is team-agnostic — validators can write files."""
    manifest = _make_manifest(writes=[{"path": "/testbed/pkg/core.py", "content": "x = 1\n"}])
    sb = _make_sandbox(manifest=manifest)
    ctx = _ctx(
        {
            "daytona_sandbox": sb,
            "daytona_cwd": "/testbed",
            "agent_name": "validator",
            "team_mode_enabled": True,
        }
    )

    result = await daytona_codeact.execute(
        daytona_codeact.input_model(code="write('/testbed/pkg/core.py', 'x = 1\\n')"),
        ctx,
    )

    data = _assert_ok(result)
    assert data["files_written"] == 1


async def test_codeact_allows_verify_surface_writes_in_team_mode():
    """CodeAct is team-agnostic — verification surface writes are not blocked."""
    manifest = _make_manifest(
        writes=[{"path": "/testbed/dask/tests/test_cli.py", "content": "patched\n"}]
    )
    sb = _make_sandbox(manifest=manifest)
    ctx = _ctx(
        {
            "daytona_sandbox": sb,
            "daytona_cwd": "/testbed",
            "agent_name": "developer",
            "team_mode_enabled": True,
            "verification_surface_write_enforcement": "error",
            "owned_files": ["dask/cli.py"],
            "owned_failures": ["dask/tests/test_cli.py"],
            "verify": ["pytest dask/tests/test_cli.py -q"],
        }
    )

    result = await daytona_codeact.execute(
        daytona_codeact.input_model(code="write('/testbed/dask/tests/test_cli.py', 'patched\\n')"),
        ctx,
    )

    data = _assert_ok(result)
    assert data["files_written"] == 1
    assert data["warnings"] == []


async def test_codeact_records_scope_warning_on_advisory_write():
    manifest = _make_manifest(
        writes=[{"path": "/testbed/dask/_compatibility.py", "content": "patched\n"}]
    )
    sb = _make_sandbox(manifest=manifest)
    ctx = _ctx(
        {
            "daytona_sandbox": sb,
            "daytona_cwd": "/testbed",
            "agent_name": "developer",
            "team_mode_enabled": True,
            "write_scope": ["dask/compatibility.py"],
            "verification_surface_write_enforcement": "warn",
        }
    )

    result = await daytona_codeact.execute(
        daytona_codeact.input_model(
            code="write('/testbed/dask/_compatibility.py', 'patched\\n')"
        ),
        ctx,
    )

    data = _assert_ok(result)
    assert data["files_written"] == 1
    assert any("outside write_scope" in warning for warning in data["warnings"])
    warnings = ctx.metadata["coordination_warnings"]
    assert warnings
    assert "outside write_scope" in warnings[0]["message"]


async def test_codeact_allows_install_commands_in_team_mode():
    """CodeAct is team-agnostic — pip install is allowed."""
    manifest = _make_manifest(
        shells=[
            {
                "command": "python -m pip install pytest",
                "exit_code": 0,
                "stdout": "",
                "stderr": "",
            }
        ]
    )
    sb = _make_sandbox(manifest=manifest)
    ctx = _ctx(
        {
            "daytona_sandbox": sb,
            "agent_name": "developer",
            "team_mode_enabled": True,
        }
    )

    result = await daytona_codeact.execute(
        daytona_codeact.input_model(code="shell('python -m pip install pytest')"),
        ctx,
    )

    data = _assert_ok(result)
    assert data["shells_run"] == 1


# ---------------------------------------------------------------------------
# CI integration: helper writes use the coordinated CI commit path
# ---------------------------------------------------------------------------


async def test_codeact_calls_ci_helpers_on_write():
    manifest = _make_manifest(writes=[{"path": "/ws/f.py", "content": "content"}])
    sb = _make_sandbox(manifest=manifest)
    svc = MagicMock()
    ctx = _ctx({"daytona_sandbox": sb, "ci_service": svc})

    await daytona_codeact.execute(
        daytona_codeact.input_model(code="write('/ws/f.py', 'content')"), ctx
    )
    svc.prepare_write.assert_called_once()
    svc.commit_prepared_write.assert_called_once()
    assert sb.fs.upload_file.call_count == 1


# ---------------------------------------------------------------------------
# Error field included in output when manifest has error
# ---------------------------------------------------------------------------


async def test_codeact_error_field_in_output():
    manifest = _make_manifest(status="error", error="Traceback: ...")
    sb = _make_sandbox(manifest=manifest)
    ctx = _ctx({"daytona_sandbox": sb})
    result = await daytona_codeact.execute(daytona_codeact.input_model(code="raise"), ctx)
    # status is "error" in manifest but we already parsed past the exec check
    # the manifest path is returned, so we get here
    data = json.loads(result.output)
    assert data["error"] == "Traceback: ..."
