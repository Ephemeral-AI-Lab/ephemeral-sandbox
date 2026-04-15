"""Tests for transactional CodeAct helpers."""

from __future__ import annotations

import json
import subprocess
from pathlib import Path
from types import SimpleNamespace

import pytest

from code_intelligence.routing.service import CodeIntelligenceService
from tools.core.base import ToolExecutionContext
from tools.daytona_toolkit.codeact_tool import daytona_codeact
from tools.daytona_toolkit.codeact_transaction import (
    collect_transaction_changes,
    commit_transaction_changes,
    create_codeact_transaction,
)

pytestmark = pytest.mark.asyncio


class _LocalFs:
    async def download_file(self, path: str):
        return Path(path).read_bytes()

    async def upload_file(self, content_or_path, path_or_content=None):
        if isinstance(content_or_path, bytes):
            content, path = content_or_path, path_or_content
        else:
            path, content = content_or_path, path_or_content
        target = Path(path)
        target.parent.mkdir(parents=True, exist_ok=True)
        if isinstance(content, bytes):
            target.write_bytes(content)
        else:
            target.write_text(str(content), encoding="utf-8")


class _LocalProcess:
    async def exec(self, command: str, timeout: int = 120):
        completed = subprocess.run(
            command,
            shell=True,
            text=True,
            executable="/bin/bash",
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            timeout=timeout,
            check=False,
        )
        return SimpleNamespace(result=completed.stdout, exit_code=completed.returncode)


class _LocalSandbox:
    def __init__(self) -> None:
        self.fs = _LocalFs()
        self.process = _LocalProcess()


def _git(repo: Path, *args: str) -> None:
    subprocess.run(
        ["git", *args],
        cwd=repo,
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )


def _make_repo(tmp_path: Path) -> Path:
    repo = tmp_path / "repo"
    repo.mkdir()
    _git(repo, "init")
    _git(repo, "config", "user.email", "test@example.com")
    _git(repo, "config", "user.name", "Test User")
    (repo / "app.py").write_text("value = 1\n", encoding="utf-8")
    (repo / "delete_me.py").write_text("delete = True\n", encoding="utf-8")
    _git(repo, "add", "app.py", "delete_me.py")
    _git(repo, "commit", "-m", "init")
    return repo


async def test_create_codeact_transaction_seeds_dirty_workspace(tmp_path: Path):
    repo = _make_repo(tmp_path)
    (repo / "app.py").write_text("value = 2\n", encoding="utf-8")
    (repo / "new_file.py").write_text("created = True\n", encoding="utf-8")

    sandbox = _LocalSandbox()
    tx = await create_codeact_transaction(
        sandbox,
        str(repo),
    )
    try:
        assert Path(tx.scratch_root).exists()
        assert (Path(tx.scratch_root) / "app.py").read_text(encoding="utf-8") == "value = 2\n"
        assert (Path(tx.scratch_root) / "new_file.py").read_text(encoding="utf-8") == "created = True\n"
        assert tx.base_tree
    finally:
        from tools.daytona_toolkit.codeact_transaction import cleanup_codeact_transaction

        await cleanup_codeact_transaction(sandbox, tx)

    assert not Path(tx.scratch_root).exists()


async def test_collect_transaction_changes_detects_create_modify_delete(tmp_path: Path):
    repo = _make_repo(tmp_path)
    sandbox = _LocalSandbox()
    tx = await create_codeact_transaction(
        sandbox,
        str(repo),
    )
    try:
        scratch = Path(tx.scratch_root)
        (scratch / "app.py").write_text("value = 3\n", encoding="utf-8")
        (scratch / "delete_me.py").unlink()
        (scratch / "created.py").write_text("created = True\n", encoding="utf-8")

        changes = await collect_transaction_changes(sandbox, tx)
    finally:
        from tools.daytona_toolkit.codeact_transaction import cleanup_codeact_transaction

        await cleanup_codeact_transaction(sandbox, tx)

    by_path = {change.path: change for change in changes}
    assert by_path["app.py"].status == "modified"
    assert by_path["app.py"].base_content == "value = 1\n"
    assert by_path["app.py"].final_content == "value = 3\n"
    assert by_path["delete_me.py"].status == "deleted"
    assert by_path["delete_me.py"].final_content is None
    assert by_path["created.py"].status == "created"
    assert by_path["created.py"].base_content is None


async def test_collect_transaction_changes_marks_binary_files_unsupported(tmp_path: Path):
    repo = _make_repo(tmp_path)
    sandbox = _LocalSandbox()
    tx = await create_codeact_transaction(
        sandbox,
        str(repo),
    )
    try:
        (Path(tx.scratch_root) / "blob.bin").write_bytes(b"\xff\x00\x01")
        changes = await collect_transaction_changes(sandbox, tx)
    finally:
        from tools.daytona_toolkit.codeact_transaction import cleanup_codeact_transaction

        await cleanup_codeact_transaction(sandbox, tx)

    assert len(changes) == 1
    assert changes[0].status == "unsupported"
    assert "binary" in str(changes[0].message).lower()


async def test_commit_transaction_changes_applies_repo_diff_via_ci(tmp_path: Path):
    repo = _make_repo(tmp_path)
    sandbox = _LocalSandbox()
    ctx = ToolExecutionContext(
        cwd=repo,
        metadata={
            "daytona_cwd": str(repo),
            "ci_service": CodeIntelligenceService(
                sandbox_id="local-ci",
                workspace_root=str(repo),
            ),
        },
    )
    tx = await create_codeact_transaction(sandbox, str(repo))
    try:
        scratch = Path(tx.scratch_root)
        (scratch / "app.py").write_text("value = 4\n", encoding="utf-8")
        (scratch / "delete_me.py").unlink()
        changes = await collect_transaction_changes(sandbox, tx)
        report = await commit_transaction_changes(ctx, tx, changes)
    finally:
        from tools.daytona_toolkit.codeact_transaction import cleanup_codeact_transaction

        await cleanup_codeact_transaction(sandbox, tx)

    assert len(report.committed) == 2
    assert (repo / "app.py").read_text(encoding="utf-8") == "value = 4\n"
    assert not (repo / "delete_me.py").exists()


async def test_coordinated_python_mode_captures_native_file_api_writes(tmp_path: Path):
    repo = _make_repo(tmp_path)
    sandbox = _LocalSandbox()
    ctx = ToolExecutionContext(
        cwd=repo,
        metadata={
            "daytona_sandbox": sandbox,
            "daytona_cwd": str(repo),
            "ci_service": CodeIntelligenceService(
                sandbox_id="local-ci-codeact",
                workspace_root=str(repo),
            ),
            "agent_name": "developer",
            "team_mode_enabled": True,
        },
    )

    result = await daytona_codeact.execute(
        daytona_codeact.input_model(
            code=(
                "from pathlib import Path\n"
                "Path('native.txt').write_text('native write\\n', encoding='utf-8')\n"
                "with open('app.py', 'w', encoding='utf-8') as handle:\n"
                "    handle.write('value = 5\\n')\n"
            )
        ),
        ctx,
    )

    assert not result.is_error, result.output
    data = json.loads(result.output)
    assert data["files_written"] == 2
    assert (repo / "native.txt").read_text(encoding="utf-8") == "native write\n"
    assert (repo / "app.py").read_text(encoding="utf-8") == "value = 5\n"
