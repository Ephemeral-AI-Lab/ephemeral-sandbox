"""Tests for tools.daytona_toolkit.codeact_tool."""

from __future__ import annotations

import json
from pathlib import Path
from unittest.mock import AsyncMock, MagicMock

import pytest

from tools.core.base import ToolExecutionContext
from tools.daytona_toolkit.codeact_tool import daytona_codeact


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


def _make_sandbox(*, upload_exc=None, exec_stdout=None, exec_exc=None, manifest=None, download_exc=None):
    """Return a sandbox mock configured for codeact scenarios."""
    sb = MagicMock()

    if upload_exc:
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


# ---------------------------------------------------------------------------
# No sandbox
# ---------------------------------------------------------------------------

async def test_codeact_no_sandbox_returns_error():
    ctx = _ctx()
    result = await daytona_codeact.execute(
        daytona_codeact.input_model(code="print('hi')"), ctx
    )
    assert result.is_error
    assert "No Daytona sandbox" in result.output


# ---------------------------------------------------------------------------
# Upload failure
# ---------------------------------------------------------------------------

async def test_codeact_upload_failure():
    sb = _make_sandbox(upload_exc=RuntimeError("disk full"))
    ctx = _ctx({"daytona_sandbox": sb})
    result = await daytona_codeact.execute(
        daytona_codeact.input_model(code="x = 1"), ctx
    )
    assert result.is_error
    assert "Failed to upload script" in result.output


# ---------------------------------------------------------------------------
# Execution failure
# ---------------------------------------------------------------------------

async def test_codeact_exec_failure():
    sb = _make_sandbox(exec_exc=RuntimeError("timeout"))
    ctx = _ctx({"daytona_sandbox": sb})
    result = await daytona_codeact.execute(
        daytona_codeact.input_model(code="x = 1"), ctx
    )
    assert result.is_error
    assert "Execution failed" in result.output


# ---------------------------------------------------------------------------
# Bad JSON output from script
# ---------------------------------------------------------------------------

async def test_codeact_bad_json_stdout():
    sb = _make_sandbox(exec_stdout="not json at all")
    ctx = _ctx({"daytona_sandbox": sb})
    result = await daytona_codeact.execute(
        daytona_codeact.input_model(code="x = 1"), ctx
    )
    # Returns non-error with raw output
    assert not result.is_error
    assert "Script output" in result.output


async def test_codeact_empty_stdout():
    sb = _make_sandbox(exec_stdout="")
    ctx = _ctx({"daytona_sandbox": sb})
    result = await daytona_codeact.execute(
        daytona_codeact.input_model(code="x = 1"), ctx
    )
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
    result = await daytona_codeact.execute(
        daytona_codeact.input_model(code="x = 1"), ctx
    )
    assert "Script output" in result.output


# ---------------------------------------------------------------------------
# Manifest unreadable
# ---------------------------------------------------------------------------

async def test_codeact_manifest_download_failure():
    sb = _make_sandbox(download_exc=RuntimeError("gone"))
    ctx = _ctx({"daytona_sandbox": sb})
    result = await daytona_codeact.execute(
        daytona_codeact.input_model(code="x = 1"), ctx
    )
    assert "manifest unreadable" in result.output


# ---------------------------------------------------------------------------
# Successful run — no writes, no shells
# ---------------------------------------------------------------------------

async def test_codeact_success_no_writes():
    manifest = _make_manifest()
    sb = _make_sandbox(manifest=manifest)
    ctx = _ctx({"daytona_sandbox": sb, "daytona_cwd": "/ws"})
    result = await daytona_codeact.execute(
        daytona_codeact.input_model(code="x = 1 + 1"), ctx
    )
    assert not result.is_error
    data = json.loads(result.output)
    assert data["status"] == "ok"
    assert data["files_written"] == 0
    assert data["shells_run"] == 0
    assert data["cwd"] == "/ws"


# ---------------------------------------------------------------------------
# Successful run — with writes committed
# ---------------------------------------------------------------------------

async def test_codeact_success_with_writes():
    manifest = _make_manifest(writes=[
        {"path": "/ws/out.py", "content": "x = 42\n"},
        {"path": "/ws/other.py", "content": "y = 1\n"},
    ])
    sb = _make_sandbox(manifest=manifest)
    ctx = _ctx({"daytona_sandbox": sb})
    result = await daytona_codeact.execute(
        daytona_codeact.input_model(code="write('/ws/out.py', 'x = 42\\n')"), ctx
    )
    assert not result.is_error
    data = json.loads(result.output)
    assert data["files_written"] == 2
    # upload_file called once for the script upload + twice for the writes
    assert sb.fs.upload_file.call_count == 3


# ---------------------------------------------------------------------------
# Write commit failure (partial writes)
# ---------------------------------------------------------------------------

async def test_codeact_write_commit_failure():
    manifest = _make_manifest(writes=[{"path": "/ws/bad.py", "content": "oops"}])
    sb = MagicMock()
    sb.fs.download_file = AsyncMock(return_value=json.dumps(manifest).encode())
    # First upload (script) succeeds, second (write commit) fails
    sb.fs.upload_file = AsyncMock(side_effect=[None, RuntimeError("commit fail")])
    result_line = json.dumps({"manifest": "/tmp/codeact-xxx.json", "status": "ok"})
    sb.process.exec = AsyncMock(return_value=MagicMock(result=result_line))
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
    manifest = _make_manifest(shells=[
        {"command": "ls -la", "exit_code": 0, "stdout": "file-a\nfile-b\n", "stderr": ""},
        {"command": "pytest", "exit_code": 1, "stdout": "", "stderr": "assertion failed"},
    ])
    sb = _make_sandbox(manifest=manifest)
    ctx = _ctx({"daytona_sandbox": sb})
    result = await daytona_codeact.execute(
        daytona_codeact.input_model(code="shell('ls -la')"), ctx
    )
    data = json.loads(result.output)
    assert data["shells_run"] == 2
    assert len(data["shell_summaries"]) == 2
    assert "ls -la" in data["shell_summaries"][0]
    assert len(data["shell_outputs"]) == 2
    assert data["shell_outputs"][0]["stdout"] == "file-a\nfile-b\n"
    assert data["shell_outputs"][1]["stderr"] == "assertion failed"


async def test_codeact_preserves_script_stdout_before_manifest_line():
    manifest = _make_manifest()
    exec_stdout = 'hello from codeact\n{"manifest": "/tmp/codeact-xxx.json", "status": "ok"}'
    sb = _make_sandbox(exec_stdout=exec_stdout, manifest=manifest)
    ctx = _ctx({"daytona_sandbox": sb})
    result = await daytona_codeact.execute(
        daytona_codeact.input_model(code="print('hello from codeact')"), ctx
    )
    data = json.loads(result.output)
    assert data["script_stdout"] == "hello from codeact"


async def test_codeact_rejects_raw_subprocess_calls_for_team_developer():
    sb = _make_sandbox()
    ctx = _ctx(
        {
            "daytona_sandbox": sb,
            "agent_name": "developer",
            "coordination_mode": "ultra",
        }
    )

    result = await daytona_codeact.execute(
        daytona_codeact.input_model(
            code="import subprocess\nsubprocess.run(['pytest'], check=False)"
        ),
        ctx,
    )

    assert result.is_error
    assert "shell(\"...\")" in result.output
    sb.process.exec.assert_not_called()


async def test_codeact_rejects_repo_writes_from_validator():
    manifest = _make_manifest(writes=[{"path": "/testbed/pkg/core.py", "content": "x = 1\n"}])
    sb = _make_sandbox(manifest=manifest)
    ctx = _ctx(
        {
            "daytona_sandbox": sb,
            "daytona_cwd": "/testbed",
            "agent_name": "validator",
            "coordination_mode": "ultra",
        }
    )

    result = await daytona_codeact.execute(
        daytona_codeact.input_model(code="write('/testbed/pkg/core.py', 'x = 1\\n')"),
        ctx,
    )

    assert result.is_error
    assert "must not write repository files" in result.output
    assert sb.fs.upload_file.call_count == 1


async def test_codeact_rejects_verify_surface_write_outside_owned_scope():
    manifest = _make_manifest(writes=[{"path": "/testbed/dask/tests/test_cli.py", "content": "patched\n"}])
    sb = _make_sandbox(manifest=manifest)
    ctx = _ctx(
        {
            "daytona_sandbox": sb,
            "daytona_cwd": "/testbed",
            "agent_name": "developer",
            "coordination_mode": "ultra",
            "owned_files": ["dask/cli.py"],
            "owned_failures": ["dask/tests/test_cli.py"],
            "verify": ["pytest dask/tests/test_cli.py -q"],
        }
    )

    result = await daytona_codeact.execute(
        daytona_codeact.input_model(code="write('/testbed/dask/tests/test_cli.py', 'patched\\n')"),
        ctx,
    )

    assert result.is_error
    assert "verification surfaces read-only" in result.output
    assert sb.fs.upload_file.call_count == 1


async def test_codeact_rejects_install_commands_for_team_developer():
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
            "coordination_mode": "ultra",
        }
    )

    result = await daytona_codeact.execute(
        daytona_codeact.input_model(code="shell('python -m pip install pytest')"),
        ctx,
    )

    assert result.is_error
    assert "ambient runtime environment" in result.output


# ---------------------------------------------------------------------------
# CI integration: prime_cache and record_edit called on successful write
# ---------------------------------------------------------------------------

async def test_codeact_calls_ci_helpers_on_write():
    manifest = _make_manifest(writes=[{"path": "/ws/f.py", "content": "content"}])
    sb = _make_sandbox(manifest=manifest)
    svc = MagicMock()
    ctx = _ctx({"daytona_sandbox": sb, "ci_service": svc})

    await daytona_codeact.execute(
        daytona_codeact.input_model(code="write('/ws/f.py', 'content')"), ctx
    )
    svc.tree_cache.put_content.assert_called_once_with("/ws/f.py", "content")
    svc.ledger.record.assert_called_once()


# ---------------------------------------------------------------------------
# Error field included in output when manifest has error
# ---------------------------------------------------------------------------

async def test_codeact_error_field_in_output():
    manifest = _make_manifest(status="error", error="Traceback: ...")
    sb = _make_sandbox(manifest=manifest)
    ctx = _ctx({"daytona_sandbox": sb})
    result = await daytona_codeact.execute(
        daytona_codeact.input_model(code="raise"), ctx
    )
    # status is "error" in manifest but we already parsed past the exec check
    # the manifest path is returned, so we get here
    data = json.loads(result.output)
    assert data["error"] == "Traceback: ..."
