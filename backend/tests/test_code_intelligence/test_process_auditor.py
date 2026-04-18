"""Unit tests for process-level workspace mutation auditing."""

from __future__ import annotations

import base64
import json
from types import SimpleNamespace

import pytest

from code_intelligence.routing.process_auditor import (
    ProcessAuditor,
    _sentinel,
)


def _framed(
    run_id: str,
    *,
    before: dict,
    exec_stdout: bytes,
    exit_code: int,
    after: dict,
) -> str:
    before_b64 = base64.b64encode(
        json.dumps({"ok": True, "files": before}).encode("utf-8")
    ).decode("ascii")
    after_b64 = base64.b64encode(
        json.dumps({"ok": True, "files": after}).encode("utf-8")
    ).decode("ascii")
    exec_b64 = base64.b64encode(exec_stdout).decode("ascii")
    return (
        f"\n{_sentinel(run_id, 'BEFORE', 'OPEN')}\n"
        f"{before_b64}\n"
        f"{_sentinel(run_id, 'BEFORE', 'CLOSE')}\n"
        f"{_sentinel(run_id, 'EXEC', 'OPEN')}\n"
        f"{exec_b64}\n"
        f"{_sentinel(run_id, 'EXEC', 'CLOSE')}\n"
        f"{_sentinel(run_id, 'EXIT', 'OPEN')}\n"
        f"{exit_code}\n"
        f"{_sentinel(run_id, 'EXIT', 'CLOSE')}\n"
        f"{_sentinel(run_id, 'AFTER', 'OPEN')}\n"
        f"{after_b64}\n"
        f"{_sentinel(run_id, 'AFTER', 'CLOSE')}\n"
    )


@pytest.mark.asyncio
async def test_process_auditor_records_changed_files_and_refreshes_indexes(
    monkeypatch,
) -> None:
    file_path = "/workspace/app.py"
    before = {
        file_path: {
            "rel": "app.py",
            "exists": True,
            "hash": "old-hash",
            "head_hash": "head-hash",
        }
    }
    after = {
        file_path: {
            "rel": "app.py",
            "exists": True,
            "hash": "new-hash",
            "head_hash": "head-hash",
        }
    }

    # Pin the UUID so we can construct a matching framed payload.
    fixed_run_id = "0123456789abcdef0123456789abcdef"
    monkeypatch.setattr(
        "code_intelligence.routing.process_auditor.uuid.uuid4",
        lambda: SimpleNamespace(hex=fixed_run_id),
    )

    commands: list[str] = []

    async def exec_process(sandbox, command: str, *, timeout: int | None = None):
        del sandbox, timeout
        commands.append(command)
        return SimpleNamespace(
            result=_framed(
                fixed_run_id,
                before=before,
                exec_stdout=b"ok",
                exit_code=0,
                after=after,
            ),
            exit_code=0,
        )

    arbiter = SimpleNamespace(recorded=[])

    def record_edit(**kwargs):
        arbiter.recorded.append(kwargs)
        return len(arbiter.recorded)

    arbiter.record_edit = record_edit
    content = SimpleNamespace(
        read=lambda path, *, allow_missing=False: ("value = 2\n", True),
    )
    symbol_index = SimpleNamespace(refreshed=[])
    symbol_index.refresh = lambda path, content_text: symbol_index.refreshed.append(
        (path, content_text)
    )
    lsp_client = SimpleNamespace(invalidated=[])
    lsp_client.invalidate = lambda path: lsp_client.invalidated.append(path)

    auditor = ProcessAuditor(
        workspace_root="/workspace",
        exec_process=exec_process,
        arbiter=arbiter,
        content=content,
        symbol_index=symbol_index,
        lsp_client=lsp_client,
    )

    result = await auditor.execute(
        SimpleNamespace(),
        "write app",
        timeout=12,
        description="test process",
        agent_id="developer",
        team_run_id="team-1",
        agent_run_id="agent-1",
        task_id="task-1",
    )

    assert result.result == "ok"
    assert result.exit_code == 0
    # One remote exec, not three.
    assert len(commands) == 1
    assert arbiter.recorded == [
        {
            "file_path": file_path,
            "actor_label": "developer",
            "team_run_id": "team-1",
            "agent_run_id": "agent-1",
            "task_id": "task-1",
            "old_hash": "old-hash",
            "new_hash": "new-hash",
            "description": "test process",
        }
    ]
    assert symbol_index.refreshed == [(file_path, "value = 2\n")]
    assert lsp_client.invalidated == [file_path]


@pytest.mark.asyncio
async def test_process_auditor_can_report_unattributed_changes(monkeypatch) -> None:
    file_path = "/workspace/app.py"
    before = {
        file_path: {
            "rel": "app.py",
            "exists": True,
            "hash": "old-hash",
            "head_hash": "head-hash",
        }
    }
    after = {
        file_path: {
            "rel": "app.py",
            "exists": True,
            "hash": "new-hash",
            "head_hash": "head-hash",
        }
    }
    fixed_run_id = "fedcba9876543210fedcba9876543210"
    monkeypatch.setattr(
        "code_intelligence.routing.process_auditor.uuid.uuid4",
        lambda: SimpleNamespace(hex=fixed_run_id),
    )

    async def exec_process(sandbox, command: str, *, timeout: int | None = None):
        del sandbox, command, timeout
        return SimpleNamespace(
            result=_framed(
                fixed_run_id,
                before=before,
                exec_stdout=b"ok",
                exit_code=0,
                after=after,
            ),
            exit_code=0,
        )

    arbiter = SimpleNamespace(recorded=[])
    arbiter.record_edit = lambda **kwargs: arbiter.recorded.append(kwargs)
    content = SimpleNamespace(read=lambda path, *, allow_missing=False: ("", False))
    symbol_index = SimpleNamespace(refreshed=[])
    symbol_index.refresh = lambda path, content_text: symbol_index.refreshed.append(
        (path, content_text)
    )
    lsp_client = SimpleNamespace(invalidated=[])
    lsp_client.invalidate = lambda path: lsp_client.invalidated.append(path)

    auditor = ProcessAuditor(
        workspace_root="/workspace",
        exec_process=exec_process,
        arbiter=arbiter,
        content=content,
        symbol_index=symbol_index,
        lsp_client=lsp_client,
    )

    result = await auditor.execute(
        SimpleNamespace(),
        "runtime-only command",
        description="test process",
        agent_id="developer",
        team_run_id="team-1",
        agent_run_id="agent-1",
        task_id="task-1",
        attribute_changes=False,
    )

    assert result.changed_paths == []
    assert result.ambient_changed_paths == [file_path]
    assert result.files_written == 0
    assert arbiter.recorded == []
    assert symbol_index.refreshed == []
    assert lsp_client.invalidated == []
