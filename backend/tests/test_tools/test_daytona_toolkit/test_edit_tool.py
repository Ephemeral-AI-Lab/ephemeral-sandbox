"""Tests for tools.daytona_toolkit.edit_tool."""

from __future__ import annotations

import base64
import json
import re
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

from tools.core.base import ToolExecutionContext, run_tool_safely
from tools.daytona_toolkit import edit_tool as edit_tool_module
from tools.daytona_toolkit.edit_tool import (
    _scope_overlap_warning,
    daytona_edit_file,
)


# pytest-asyncio runs in auto mode — async tests are handled
# automatically. A module-level `pytestmark = pytest.mark.asyncio` would
# emit a warning for every sync test in this file.


def _ctx(metadata=None) -> ToolExecutionContext:
    return ToolExecutionContext(cwd=Path("/tmp"), metadata=metadata or {})


def _make_sandbox(*, download_content: str = "original content"):
    sb = MagicMock()
    state = {"content": download_content}

    async def download_file(_path: str):
        return state["content"].encode("utf-8")

    sb.fs.download_file = AsyncMock(side_effect=download_file)
    sb.fs.upload_file = AsyncMock()
    sb._content_state = state

    async def exec_side_effect(command: str, timeout=None):
        payload_match = re.search(r"DAYTONA_EDIT_PAYLOAD=([^ ]+)", command)
        file_match = re.search(r"DAYTONA_EDIT_FILE=([^ ]+)", command)
        if payload_match is None or file_match is None:
            return MagicMock(result="", exit_code=0)
        file_path = file_match.group(1).strip("'")
        try:
            current = state["content"]
        except FileNotFoundError:
            return MagicMock(
                result=json.dumps(
                    {"ok": False, "error": f"Path does not exist: {file_path}"}
                ),
                exit_code=1,
            )
        except Exception as exc:
            return MagicMock(
                result=json.dumps({"ok": False, "error": f"Cannot read file: {exc}"}),
                exit_code=1,
            )

        edits = json.loads(base64.b64decode(payload_match.group(1)).decode("utf-8"))
        result = current
        errors: list[str] = []
        for index, edit in enumerate(edits, start=1):
            old_text = str(edit.get("old_text", ""))
            new_text = str(edit.get("new_text", ""))
            if old_text not in result:
                errors.append(f"Edit {index}: search text not found")
                continue
            result = result.replace(old_text, new_text, 1)

        if errors:
            return MagicMock(result=json.dumps({"ok": False, "errors": errors}), exit_code=2)

        payload = {
            "ok": True,
            "file_path": file_path,
            "applied_edits": len(edits),
            "warnings": [],
        }
        if "DAYTONA_EDIT_DRY_RUN=1" in command:
            payload["status"] = "dry_run"
            payload["would_edit"] = True
        else:
            state["content"] = result
            payload["status"] = "edited"
        return MagicMock(result=json.dumps(payload), exit_code=0)

    sb.process.exec = AsyncMock(side_effect=exec_side_effect)
    return sb


def _ci_service_for_content(content: str, *, file_path: str = "/file.py"):
    del content, file_path
    return SimpleNamespace(
        exec_process_operation=AsyncMock(side_effect=_exec_process_operation)
    )


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


# ---------------------------------------------------------------------------
# No sandbox in context
# ---------------------------------------------------------------------------

async def test_edit_no_sandbox_returns_error():
    ctx = _ctx()
    result = await daytona_edit_file.execute(
        daytona_edit_file.input_model(
            file_path="/file.py", old_text="old", new_text="new"
        ),
        ctx,
    )
    assert result.is_error
    assert "No Daytona sandbox" in result.output


# ---------------------------------------------------------------------------
# Read failure
# ---------------------------------------------------------------------------

async def test_edit_file_read_failure():
    sb = _make_sandbox()
    sb.process.exec = AsyncMock(
        return_value=MagicMock(
            result=json.dumps({"ok": False, "error": "Path does not exist: /missing.py"}),
            exit_code=1,
        )
    )
    ctx = _ctx({"daytona_sandbox": sb, "ci_service": _ci_service_for_content("")})
    result = await daytona_edit_file.execute(
        daytona_edit_file.input_model(
            file_path="/missing.py", old_text="old", new_text="new", dry_run=True
        ),
        ctx,
    )
    assert result.is_error
    assert "does not exist" in result.output


async def test_edit_file_read_generic_exception():
    sb = _make_sandbox()
    sb.process.exec = AsyncMock(
        return_value=MagicMock(
            result=json.dumps({"ok": False, "error": "Cannot read file: network"}),
            exit_code=1,
        )
    )
    ctx = _ctx({"daytona_sandbox": sb, "ci_service": _ci_service_for_content("")})
    result = await daytona_edit_file.execute(
        daytona_edit_file.input_model(
            file_path="/file.py", old_text="old", new_text="new", dry_run=True
        ),
        ctx,
    )
    assert result.is_error
    assert "network" in result.output


# ---------------------------------------------------------------------------
# Text not found
# ---------------------------------------------------------------------------

async def test_edit_old_text_not_found():
    sb = _make_sandbox(download_content="hello world")
    ctx = _ctx({"daytona_sandbox": sb, "ci_service": _ci_service_for_content("hello world")})
    result = await daytona_edit_file.execute(
        daytona_edit_file.input_model(
            file_path="/file.py", old_text="MISSING", new_text="new"
        ),
        ctx,
    )
    assert result.is_error
    assert "Search text not found" in result.output


# ---------------------------------------------------------------------------
# Dry run
# ---------------------------------------------------------------------------

async def test_edit_dry_run_reports_validation_without_diff():
    sb = _make_sandbox(download_content="def foo():\n    pass\n")
    ctx = _ctx(
        {
            "daytona_sandbox": sb,
            "daytona_cwd": "/ws",
            "ci_service": _ci_service_for_content(""),
        }
    )
    result = await daytona_edit_file.execute(
        daytona_edit_file.input_model(
            file_path="/ws/file.py",
            old_text="    pass",
            new_text="    return 42",
            dry_run=True,
        ),
        ctx,
    )
    assert not result.is_error
    data = json.loads(result.output)
    assert data["status"] == "dry_run"
    assert "diff" not in data
    assert data["applied_edits"] == 1
    assert result.metadata.get("dry_run") is True
    # File should NOT have been written
    sb.fs.upload_file.assert_not_called()
    assert sb._content_state["content"] == "def foo():\n    pass\n"


async def test_edit_dry_run_no_actual_write():
    sb = _make_sandbox(download_content="original text here")
    ctx = _ctx({"daytona_sandbox": sb, "ci_service": _ci_service_for_content("")})
    await daytona_edit_file.execute(
        daytona_edit_file.input_model(
            file_path="/file.py",
            old_text="original",
            new_text="replaced",
            dry_run=True,
        ),
        ctx,
    )
    sb.fs.upload_file.assert_not_called()
    assert sb._content_state["content"] == "original text here"


# ---------------------------------------------------------------------------
# CI-required writes
# ---------------------------------------------------------------------------

async def test_edit_requires_ci_service_for_write():
    sb = _make_sandbox(download_content="hello world\nfoo bar\n")
    ctx = _ctx({"daytona_sandbox": sb, "daytona_cwd": "/ws"})
    result = await daytona_edit_file.execute(
        daytona_edit_file.input_model(
            file_path="/ws/file.py",
            old_text="hello world",
            new_text="goodbye world",
        ),
        ctx,
    )
    assert result.is_error
    assert "Code intelligence service is unavailable" in result.output
    assert result.metadata["ci_required"] is True
    sb.fs.upload_file.assert_not_called()


async def test_edit_warns_write_outside_write_scope():
    """Write-scope is advisory — out-of-scope writes succeed with a warning."""
    sb = _make_sandbox(download_content="original")
    ctx = _ctx(
        {
            "daytona_sandbox": sb,
            "daytona_cwd": "/testbed",
            "agent_name": "developer",
            "write_scope": ["dask/config.py"],
            "ci_service": _ci_service_for_content(
                "original",
                file_path="/testbed/dask/_compatibility.py",
            ),
        }
    )

    result = await run_tool_safely(
        daytona_edit_file,
        {
            "file_path": "/testbed/dask/_compatibility.py",
            "old_text": "original",
            "new_text": "patched",
        },
        ctx,
    )

    assert not result.is_error
    data = json.loads(result.output)
    assert data["warnings"]
    assert any("outside write_scope" in w for w in data["warnings"])


async def test_edit_invalid_input_includes_outside_scope_warning():
    sb = _make_sandbox(download_content="original")
    ctx = _ctx(
        {
            "daytona_sandbox": sb,
            "daytona_cwd": "/testbed",
            "agent_name": "developer",
            "write_scope": ["dask/config.py"],
        }
    )

    result = await run_tool_safely(
        daytona_edit_file,
        {
            "file_path": "/testbed/dask/_compatibility.py",
            "new_text": "from dask.compatibility import *\n",
        },
        ctx,
    )

    assert result.is_error
    assert "Provide `old_text`" in result.output
    assert "outside write_scope" in result.output
    assert "submit_task_summary(type='fail')" in result.output
    assert sb._content_state["content"] == "original"


async def test_edit_allows_write_inside_write_scope():
    sb = _make_sandbox(download_content="original")
    ctx = _ctx(
        {
            "daytona_sandbox": sb,
            "daytona_cwd": "/testbed",
            "agent_name": "developer",
            "write_scope": ["dask/config.py"],
            "ci_service": _ci_service_for_content(
                "original",
                file_path="/testbed/dask/config.py",
            ),
        }
    )

    result = await run_tool_safely(
        daytona_edit_file,
        {
            "file_path": "/testbed/dask/config.py",
            "old_text": "original",
            "new_text": "patched",
        },
        ctx,
    )

    assert not result.is_error
    data = json.loads(result.output)
    assert data["status"] == "edited"
    assert sb._content_state["content"] == "patched"


async def test_edit_blocks_test_file_with_policy_message():
    sb = _make_sandbox(download_content="original")
    ctx = _ctx(
        {
            "daytona_sandbox": sb,
            "daytona_cwd": "/testbed",
            "agent_name": "developer",
            "write_scope": ["dask/dataframe/io/tests/test_hdf.py"],
            "team_run_id": "run-1",
            "work_item_id": "task-1",
            "ci_service": _ci_service_for_content(
                "original",
                file_path="/testbed/dask/dataframe/io/tests/test_hdf.py",
            ),
        }
    )

    result = await run_tool_safely(
        daytona_edit_file,
        {
            "file_path": "/testbed/dask/dataframe/io/tests/test_hdf.py",
            "old_text": "original",
            "new_text": "patched",
        },
        ctx,
    )

    assert result.is_error
    assert "BLOCKED_TEST_FILE_EDIT" in result.output
    assert "dask/dataframe/io/tests/test_hdf.py" in result.output
    assert "read/verify-only" in result.output
    assert "submit_task_summary(type='fail'" in result.output
    sb.process.exec.assert_not_awaited()


async def test_edit_allows_authorized_test_file_edit():
    sb = _make_sandbox(download_content="original")
    ctx = _ctx(
        {
            "daytona_sandbox": sb,
            "daytona_cwd": "/testbed",
            "agent_name": "developer",
            "write_scope": ["dask/tests/test_cli.py"],
            "allow_test_file_edits": True,
            "ci_service": _ci_service_for_content(
                "original",
                file_path="/testbed/dask/tests/test_cli.py",
            ),
        }
    )

    result = await run_tool_safely(
        daytona_edit_file,
        {
            "file_path": "/testbed/dask/tests/test_cli.py",
            "old_text": "original",
            "new_text": "patched",
        },
        ctx,
    )

    assert not result.is_error
    data = json.loads(result.output)
    assert data["status"] == "edited"


async def test_edit_warns_non_verify_surface_write_in_warn_mode():
    """Write-scope is advisory — non-verify-surface writes also succeed with a warning."""
    sb = _make_sandbox(download_content="original")
    ctx = _ctx(
        {
            "daytona_sandbox": sb,
            "daytona_cwd": "/testbed",
            "agent_name": "developer",
            "write_scope": ["dask/compatibility.py"],
            "verification_surface_write_enforcement": "warn",
            "owned_failures": ["dask/tests/test_cli.py"],
            "verify": ["pytest dask/tests/test_cli.py -q"],
            "ci_service": _ci_service_for_content(
                "original",
                file_path="/testbed/dask/_compatibility.py",
            ),
        }
    )

    result = await run_tool_safely(
        daytona_edit_file,
        {
            "file_path": "/testbed/dask/_compatibility.py",
            "old_text": "original",
            "new_text": "patched",
        },
        ctx,
    )

    assert not result.is_error
    data = json.loads(result.output)
    assert data["warnings"]
    assert any("outside write_scope" in w for w in data["warnings"])


def test_scope_overlap_warning_ignores_same_agent_run_id():
    own_change = SimpleNamespace(
        file_path="dask/config.py",
        edit_type="edit",
        agent_run_id="run-1",
        task_id="task-own",
        created_at=SimpleNamespace(timestamp=lambda: 0),
    )
    other_change = SimpleNamespace(
        file_path="dask/compatibility.py",
        edit_type="edit",
        agent_run_id="run-2",
        task_id="task-peer",
        created_at=SimpleNamespace(timestamp=lambda: 0),
    )
    ctx = _ctx(
        {
            "arbiter": SimpleNamespace(
                initialized=True,
                changes_since=lambda _since, team_run_id=None: [own_change, other_change],
            ),
            "agent_run_id": "run-1",
            "write_scope": ["dask/"],
            "work_item_started_at": 1.0,
        }
    )

    warning = _scope_overlap_warning(ctx, "dask/config.py")

    assert "dask/compatibility.py" in warning
    assert "dask/config.py (" not in warning


async def test_edit_allows_repo_write_from_validator():
    sb = _make_sandbox(download_content="original")
    ctx = _ctx(
        {
            "daytona_sandbox": sb,
            "daytona_cwd": "/testbed",
            "agent_name": "validator",
            "ci_service": _ci_service_for_content(
                "original",
                file_path="/testbed/dask/config.py",
            ),
        }
    )

    result = await run_tool_safely(
        daytona_edit_file,
        {
            "file_path": "/testbed/dask/config.py",
            "old_text": "original",
            "new_text": "patched",
        },
        ctx,
    )

    assert not result.is_error


async def test_edit_no_raw_write_after_ci_unavailable():
    sb = _make_sandbox(download_content="content here")
    sb.fs.upload_file = AsyncMock(side_effect=RuntimeError("write fail"))
    ctx = _ctx(
        {
            "daytona_sandbox": sb,
            "agent_name": "developer",
        }
    )
    result = await daytona_edit_file.execute(
        daytona_edit_file.input_model(
            file_path="/file.py",
            old_text="content here",
            new_text="new content",
        ),
        ctx,
    )
    assert result.is_error
    assert "Code intelligence service is unavailable" in result.output
    assert result.metadata["ci_required"] is True
    sb.fs.upload_file.assert_not_called()


async def test_edit_replaces_only_first_occurrence():
    sb = _make_sandbox(download_content="x x x")
    svc = _ci_service_for_content("x x x")
    ctx = _ctx({"daytona_sandbox": sb, "ci_service": svc})
    await daytona_edit_file.execute(
        daytona_edit_file.input_model(
            file_path="/file.py", old_text="x", new_text="y"
        ),
        ctx,
    )
    # Only first x replaced → "y x x"
    assert sb._content_state["content"] == "y x x"


async def test_edit_line_range_rejected():
    sb = _make_sandbox(download_content="a\nb\nc\n")
    ctx = _ctx({"daytona_sandbox": sb})
    result = await daytona_edit_file.execute(
        daytona_edit_file.input_model(
            file_path="/file.py",
            edits=[
                {
                    "strategy": "line_range",
                    "start_line": 2,
                    "end_line": 2,
                    "new_content": "beta",
                }
            ],
        ),
        ctx,
    )
    assert result.is_error
    assert "unknown strategy" in result.output


async def test_edit_multi_replace_process_success():
    sb = _make_sandbox(download_content="alpha\nbeta\ngamma\n")
    svc = _ci_service_for_content("alpha\nbeta\ngamma\n")
    ctx = _ctx({"daytona_sandbox": sb, "ci_service": svc})
    result = await daytona_edit_file.execute(
        daytona_edit_file.input_model(
            file_path="/file.py",
            edits=[
                {"strategy": "search_replace", "search": "alpha", "replace": "ALPHA"},
                {"strategy": "search_replace", "search": "gamma", "replace": "GAMMA"},
            ],
        ),
        ctx,
    )
    assert not result.is_error
    assert sb._content_state["content"] == "ALPHA\nbeta\nGAMMA\n"


async def test_edit_batch_runs_through_exec_ci_process_operation(monkeypatch):
    sb = _make_sandbox(download_content="alpha\nbeta\ngamma\n")
    svc = _ci_service_for_content("alpha\nbeta\ngamma\n")
    ctx = _ctx({"daytona_sandbox": sb, "ci_service": svc})
    calls: list[tuple[str, str]] = []

    async def fake_exec_ci_process_operation(
        context,
        sandbox,
        command,
        *,
        timeout=None,
        description,
    ):
        calls.append((command, description))
        return await sandbox.process.exec(command, timeout=timeout)

    monkeypatch.setattr(
        edit_tool_module,
        "exec_ci_process_operation",
        fake_exec_ci_process_operation,
    )

    result = await daytona_edit_file.execute(
        daytona_edit_file.input_model(
            file_path="/file.py",
            edits=[
                {"strategy": "search_replace", "search": "alpha", "replace": "ALPHA"},
                {"strategy": "search_replace", "search": "gamma", "replace": "GAMMA"},
            ],
        ),
        ctx,
    )

    assert not result.is_error
    assert calls
    assert "DAYTONA_EDIT_PAYLOAD" in calls[0][0]
    assert calls[0][1] == "daytona_edit_file"


async def test_edit_rejects_mixed_legacy_and_batch_inputs():
    sb = _make_sandbox(download_content="alpha\n")
    ctx = _ctx({"daytona_sandbox": sb})
    result = await daytona_edit_file.execute(
        daytona_edit_file.input_model(
            file_path="/file.py",
            old_text="alpha",
            new_text="beta",
            edits=[{"strategy": "search_replace", "search": "alpha", "replace": "beta"}],
        ),
        ctx,
    )
    assert result.is_error
    assert "Provide either `old_text`/`new_text` or `edits`" in result.output


# ---------------------------------------------------------------------------
# Audited process path
# ---------------------------------------------------------------------------

async def test_edit_process_path_success():
    sb = _make_sandbox(download_content="old content\n")
    svc = _ci_service_for_content("old content\n")
    ctx = _ctx({"daytona_sandbox": sb, "ci_service": svc})

    result = await daytona_edit_file.execute(
        daytona_edit_file.input_model(
            file_path="/file.py",
            old_text="old content",
            new_text="new content",
        ),
        ctx,
    )
    assert not result.is_error
    data = json.loads(result.output)
    assert data["status"] == "edited"
    assert data["applied_edits"] == 1
    assert sb._content_state["content"] == "new content\n"


async def test_edit_resolves_relative_path():
    sb = _make_sandbox(download_content="stuff")
    svc = _ci_service_for_content("stuff", file_path="/workspace/relative.py")
    ctx = _ctx({"daytona_sandbox": sb, "daytona_cwd": "/workspace", "ci_service": svc})
    result = await daytona_edit_file.execute(
        daytona_edit_file.input_model(
            file_path="relative.py", old_text="stuff", new_text="other"
        ),
        ctx,
    )
    assert not result.is_error
    command = sb.process.exec.call_args.args[0]
    assert "DAYTONA_EDIT_FILE=/workspace/relative.py" in command
