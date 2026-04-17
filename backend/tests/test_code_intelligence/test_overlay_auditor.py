"""Unit tests for :mod:`code_intelligence.routing.overlay_auditor`."""

from __future__ import annotations

import base64
import io
import tarfile
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest

from code_intelligence.routing.overlay_auditor import OverlayAuditor
from code_intelligence.routing.overlay_exec import _sentinel


def _framed(run_id: str, section: str, payload: str) -> str:
    return (
        f"{_sentinel(run_id, section, 'OPEN')}\n"
        f"{payload}\n"
        f"{_sentinel(run_id, section, 'CLOSE')}\n"
    )


def _make_tar(tmp_path: Path, name: str, members: list[tuple[str, bytes]]) -> Path:
    path = tmp_path / name
    with tarfile.open(path, mode="w", format=tarfile.PAX_FORMAT) as tf:
        for member_path, content in members:
            info = tarfile.TarInfo(name=member_path)
            info.size = len(content)
            info.mode = 0o644
            info.type = tarfile.REGTYPE
            tf.addfile(info, io.BytesIO(content))
    return path


class _FakeArbiter:
    def __init__(self) -> None:
        self.records: list[dict[str, Any]] = []

    def record_edit(self, **kwargs: Any) -> int:
        self.records.append(kwargs)
        return len(self.records)


class _FakeContent:
    def __init__(self) -> None:
        self.writes: dict[str, str] = {}
        self.deletes: list[str] = []

    def write(self, path: str, text: str) -> None:
        self.writes[path] = text

    def delete(self, path: str) -> None:
        self.deletes.append(path)


class _FakeSymbolIndex:
    def __init__(self) -> None:
        self.refreshed: list[tuple[str, str]] = []

    def refresh(self, path: str, content: str) -> None:
        self.refreshed.append((path, content))


class _FakeLsp:
    def __init__(self) -> None:
        self.invalidated: list[str] = []

    def invalidate(self, path: str) -> None:
        self.invalidated.append(path)


async def _lowerdir_for(repo_root: str) -> str:
    return f"{repo_root}/.overlay-lower"


def _build_exec_reply(run_id: str, tar_path: str, stdout: bytes = b"ok\n") -> str:
    return (
        _framed(run_id, "EXEC", base64.b64encode(stdout).decode("ascii"))
        + _framed(run_id, "EXIT", "0")
        + _framed(run_id, "TAR", f"{tar_path}|0")
        + _framed(run_id, "MOUNT_ERR", "")
    )


@pytest.mark.asyncio
async def test_execute_applies_modify_and_records_ledger(tmp_path: Path) -> None:
    tar_path = _make_tar(tmp_path, "audit.tar", [("./pkg/new.py", b"x = 42\n")])

    captured: dict[str, Any] = {}

    async def fake_exec(sandbox: Any, command: str, *, timeout: Any) -> Any:
        if "__OVERLAYAUDIT_" in command:
            prefix = "__OVERLAYAUDIT_"
            start = command.index(prefix) + len(prefix)
            run_id = command[start : start + 32]
            captured["run_id"] = run_id
            return SimpleNamespace(result=_build_exec_reply(run_id, str(tar_path)))
        if command.startswith("if [ -f") and "base64 <" in command:
            # Host-side download of the remote tar. Stream real bytes.
            with open(tar_path, "rb") as fh:
                return SimpleNamespace(result=base64.b64encode(fh.read()).decode("ascii"))
        if command.startswith("rm -rf"):
            return SimpleNamespace(result="")
        return SimpleNamespace(result="")

    arbiter = _FakeArbiter()
    content = _FakeContent()
    symbol_index = _FakeSymbolIndex()
    lsp = _FakeLsp()

    auditor = OverlayAuditor(
        workspace_root="/workspace",
        exec_process=fake_exec,
        arbiter=arbiter,
        content=content,
        symbol_index=symbol_index,
        lsp_client=lsp,
        lowerdir_provider=_lowerdir_for,
    )

    result = await auditor.execute(
        sandbox=object(),
        command="echo",
        agent_id="agent-A",
        team_run_id="team-1",
        agent_run_id="run-1",
        task_id="task-42",
    )

    assert result.exit_code == 0
    assert result.changed_paths == ["/workspace/pkg/new.py"]
    assert result.files_written == 1

    assert content.writes == {"/workspace/pkg/new.py": "x = 42\n"}

    assert len(arbiter.records) == 1
    record = arbiter.records[0]
    assert record["file_path"] == "/workspace/pkg/new.py"
    assert record["actor_label"] == "agent-A"
    assert record["agent_run_id"] == "run-1"
    assert record["task_id"] == "task-42"
    assert record["new_hash"]  # non-empty — we computed a content hash

    assert (
        "/workspace/pkg/new.py",
        "x = 42\n",
    ) in symbol_index.refreshed
    assert lsp.invalidated == ["/workspace/pkg/new.py"]

    # The local downloaded copy gets cleaned; the simulated "remote" tar
    # is the test fixture and persists in tmp_path until teardown.
    assert tar_path.exists()


@pytest.mark.asyncio
async def test_execute_two_agents_different_files_attributes_separately(
    tmp_path: Path,
) -> None:
    """The regression guard: each agent's audit names only its own writes."""
    tar_a = _make_tar(tmp_path, "a.tar", [("./pkg/agent_a.py", b"a=1\n")])
    tar_b = _make_tar(tmp_path, "b.tar", [("./pkg/agent_b.py", b"b=2\n")])
    tars = iter([str(tar_a), str(tar_b)])
    current_tar = {"path": ""}

    async def fake_exec(sandbox: Any, command: str, *, timeout: Any) -> Any:
        if "__OVERLAYAUDIT_" in command:
            prefix = "__OVERLAYAUDIT_"
            start = command.index(prefix) + len(prefix)
            run_id = command[start : start + 32]
            current_tar["path"] = next(tars)
            return SimpleNamespace(result=_build_exec_reply(run_id, current_tar["path"]))
        if command.startswith("if [ -f") and "base64 <" in command:
            with open(current_tar["path"], "rb") as fh:
                return SimpleNamespace(result=base64.b64encode(fh.read()).decode("ascii"))
        if command.startswith("rm -rf"):
            return SimpleNamespace(result="")
        return SimpleNamespace(result="")

    arbiter = _FakeArbiter()
    content = _FakeContent()
    auditor = OverlayAuditor(
        workspace_root="/workspace",
        exec_process=fake_exec,
        arbiter=arbiter,
        content=content,
        symbol_index=_FakeSymbolIndex(),
        lsp_client=_FakeLsp(),
        lowerdir_provider=_lowerdir_for,
    )

    res_a = await auditor.execute(
        sandbox=object(), command="a", agent_id="A",
    )
    res_b = await auditor.execute(
        sandbox=object(), command="b", agent_id="B",
    )

    assert res_a.changed_paths == ["/workspace/pkg/agent_a.py"]
    assert res_b.changed_paths == ["/workspace/pkg/agent_b.py"]

    by_actor = {r["actor_label"]: r["file_path"] for r in arbiter.records}
    assert by_actor == {
        "A": "/workspace/pkg/agent_a.py",
        "B": "/workspace/pkg/agent_b.py",
    }
