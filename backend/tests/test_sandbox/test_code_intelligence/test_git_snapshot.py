"""Tests for ``git_snapshot.build_live_snapshot``.

PR 1 acceptance criteria from
``docs/architecture/overlay-sandbox-plan.md`` §8:

* snapshot of a clean tree equals ``HEAD``'s tree
* snapshot captures a dirty (unstaged) tracked file
* snapshot captures an untracked file
* snapshot respects ``.gitignore``
* live ``.git/index`` is byte-identical before/after
* no ref is moved (``for-each-ref`` equality)
* ``pre-commit`` / ``commit-msg`` hooks do not fire
"""

from __future__ import annotations

import asyncio
import os
import subprocess
from pathlib import Path
from types import SimpleNamespace

import pytest

from sandbox.code_intelligence.overlay.git_snapshot import (
    GitSnapshotError,
    build_live_snapshot,
    build_live_snapshot_details,
)


# ---------------------------------------------------------------------------
# Sandbox shim that runs the exec transport directly on the host.
# ---------------------------------------------------------------------------


class _AsyncLocalProcess:
    """Mimics the Daytona ``sandbox.process`` object for the exec transport."""

    async def exec(self, command: str, timeout: int | None = None):
        completed = await asyncio.to_thread(
            subprocess.run,
            command,
            shell=True,
            text=True,
            capture_output=True,
            timeout=timeout,
            check=False,
        )
        return SimpleNamespace(
            result=completed.stdout + completed.stderr,
            exit_code=completed.returncode,
        )


async def _exec_process(sandbox, command, *, timeout=None):
    return await sandbox.process.exec(command, timeout=timeout)


def _git(*args: str, cwd: Path) -> str:
    return subprocess.check_output(
        ["git", "-C", str(cwd), *args],
        text=True,
    )


def _init_repo(path: Path) -> None:
    subprocess.run(["git", "-C", str(path), "init", "-q"], check=True)
    subprocess.run(
        ["git", "-C", str(path), "config", "user.email", "test@example.invalid"],
        check=True,
    )
    subprocess.run(
        ["git", "-C", str(path), "config", "user.name", "Test User"],
        check=True,
    )
    # Ensure a predictable initial branch name across host git versions.
    subprocess.run(
        ["git", "-C", str(path), "symbolic-ref", "HEAD", "refs/heads/main"],
        check=True,
    )


def _commit_all(path: Path, message: str = "seed") -> str:
    subprocess.run(["git", "-C", str(path), "add", "-A"], check=True)
    subprocess.run(
        ["git", "-C", str(path), "commit", "-q", "-m", message],
        check=True,
    )
    return _git("rev-parse", "HEAD", cwd=path).strip()


@pytest.fixture()
def sandbox() -> SimpleNamespace:
    return SimpleNamespace(process=_AsyncLocalProcess())


# ---------------------------------------------------------------------------
# Happy-path invariants
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_snapshot_of_clean_tree_equals_head_tree(
    tmp_path: Path, sandbox: SimpleNamespace
) -> None:
    repo = tmp_path / "clean"
    repo.mkdir()
    _init_repo(repo)
    (repo / "app.py").write_text("print('hi')\n", encoding="utf-8")
    _commit_all(repo)

    head_tree = _git("rev-parse", "HEAD^{tree}", cwd=repo).strip()

    snap = await build_live_snapshot(sandbox, _exec_process, str(repo))

    snap_tree = _git("rev-parse", f"{snap}^{{tree}}", cwd=repo).strip()
    assert snap_tree == head_tree


@pytest.mark.asyncio
async def test_snapshot_captures_dirty_tracked_file(
    tmp_path: Path, sandbox: SimpleNamespace
) -> None:
    repo = tmp_path / "dirty"
    repo.mkdir()
    _init_repo(repo)
    target = repo / "app.py"
    target.write_text("committed\n", encoding="utf-8")
    _commit_all(repo)
    target.write_text("dirty-edit\n", encoding="utf-8")

    snap = await build_live_snapshot(sandbox, _exec_process, str(repo))

    shown = _git("show", f"{snap}:app.py", cwd=repo)
    assert shown == "dirty-edit\n"


@pytest.mark.asyncio
async def test_snapshot_captures_staged_file(
    tmp_path: Path, sandbox: SimpleNamespace
) -> None:
    repo = tmp_path / "staged"
    repo.mkdir()
    _init_repo(repo)
    target = repo / "app.py"
    target.write_text("committed\n", encoding="utf-8")
    _commit_all(repo)
    target.write_text("staged-content\n", encoding="utf-8")
    subprocess.run(["git", "-C", str(repo), "add", "app.py"], check=True)

    snap = await build_live_snapshot(sandbox, _exec_process, str(repo))

    shown = _git("show", f"{snap}:app.py", cwd=repo)
    assert shown == "staged-content\n"


@pytest.mark.asyncio
async def test_snapshot_captures_untracked_file(
    tmp_path: Path, sandbox: SimpleNamespace
) -> None:
    repo = tmp_path / "untracked"
    repo.mkdir()
    _init_repo(repo)
    (repo / "app.py").write_text("seed\n", encoding="utf-8")
    _commit_all(repo)
    (repo / "new.txt").write_text("brand new\n", encoding="utf-8")

    snap = await build_live_snapshot(sandbox, _exec_process, str(repo))

    shown = _git("show", f"{snap}:new.txt", cwd=repo)
    assert shown == "brand new\n"


@pytest.mark.asyncio
async def test_snapshot_details_include_git_phase_timings(
    tmp_path: Path, sandbox: SimpleNamespace
) -> None:
    repo = tmp_path / "timed"
    repo.mkdir()
    _init_repo(repo)
    (repo / "app.py").write_text("seed\n", encoding="utf-8")
    _commit_all(repo)
    (repo / "new.txt").write_text("brand new\n", encoding="utf-8")

    details = await build_live_snapshot_details(sandbox, _exec_process, str(repo))

    assert details.snap
    assert details.tree
    assert details.parent
    for key in (
        "validate_repo",
        "temp_index",
        "prepare_env",
        "rev_parse_head",
        "read_tree",
        "git_add",
        "write_tree",
        "commit_tree",
        "total",
    ):
        assert key in details.timings
        assert details.timings[key] >= 0


@pytest.mark.asyncio
async def test_snapshot_respects_gitignore(
    tmp_path: Path, sandbox: SimpleNamespace
) -> None:
    repo = tmp_path / "ignored"
    repo.mkdir()
    _init_repo(repo)
    (repo / "app.py").write_text("seed\n", encoding="utf-8")
    (repo / ".gitignore").write_text(".venv/\nnode_modules/\n", encoding="utf-8")
    _commit_all(repo)
    (repo / ".venv").mkdir()
    (repo / ".venv" / "pyvenv.cfg").write_text("home=/usr\n", encoding="utf-8")
    (repo / "node_modules").mkdir()
    (repo / "node_modules" / "pkg").mkdir()
    (repo / "node_modules" / "pkg" / "index.js").write_text("x\n", encoding="utf-8")

    snap = await build_live_snapshot(sandbox, _exec_process, str(repo))

    for ignored_path in (".venv/pyvenv.cfg", "node_modules/pkg/index.js"):
        proc = subprocess.run(
            ["git", "-C", str(repo), "show", f"{snap}:{ignored_path}"],
            capture_output=True,
            text=True,
        )
        assert proc.returncode != 0, f"{ignored_path} leaked into snapshot"


@pytest.mark.asyncio
async def test_snapshot_captures_deletion(
    tmp_path: Path, sandbox: SimpleNamespace
) -> None:
    repo = tmp_path / "deleted"
    repo.mkdir()
    _init_repo(repo)
    (repo / "gone.py").write_text("bye\n", encoding="utf-8")
    (repo / "keep.py").write_text("hi\n", encoding="utf-8")
    _commit_all(repo)
    (repo / "gone.py").unlink()

    snap = await build_live_snapshot(sandbox, _exec_process, str(repo))

    gone = subprocess.run(
        ["git", "-C", str(repo), "show", f"{snap}:gone.py"],
        capture_output=True,
        text=True,
    )
    assert gone.returncode != 0, "deleted file leaked into snapshot"
    kept = _git("show", f"{snap}:keep.py", cwd=repo)
    assert kept == "hi\n"


# ---------------------------------------------------------------------------
# Non-mutation invariants
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_live_index_is_byte_identical_after_snapshot(
    tmp_path: Path, sandbox: SimpleNamespace
) -> None:
    repo = tmp_path / "index"
    repo.mkdir()
    _init_repo(repo)
    (repo / "a.py").write_text("a\n", encoding="utf-8")
    _commit_all(repo)
    # Introduce some dirty/staged/untracked state so the index has
    # meaningful content that the snapshot could hypothetically mutate.
    (repo / "a.py").write_text("a-dirty\n", encoding="utf-8")
    (repo / "b.py").write_text("b-new\n", encoding="utf-8")
    subprocess.run(["git", "-C", str(repo), "add", "b.py"], check=True)

    index_path = repo / ".git" / "index"
    before = index_path.read_bytes()
    before_mtime_ns = index_path.stat().st_mtime_ns

    await build_live_snapshot(sandbox, _exec_process, str(repo))

    after = index_path.read_bytes()
    after_mtime_ns = index_path.stat().st_mtime_ns
    assert before == after, "live .git/index content changed"
    assert before_mtime_ns == after_mtime_ns, "live .git/index mtime changed"


@pytest.mark.asyncio
async def test_snapshot_does_not_move_any_ref(
    tmp_path: Path, sandbox: SimpleNamespace
) -> None:
    repo = tmp_path / "refs"
    repo.mkdir()
    _init_repo(repo)
    (repo / "a.py").write_text("a\n", encoding="utf-8")
    _commit_all(repo)
    subprocess.run(
        ["git", "-C", str(repo), "branch", "feature"], check=True
    )
    subprocess.run(
        ["git", "-C", str(repo), "tag", "v0"], check=True
    )
    (repo / "a.py").write_text("dirty\n", encoding="utf-8")

    before = _git("for-each-ref", "--sort=refname", cwd=repo)

    await build_live_snapshot(sandbox, _exec_process, str(repo))

    after = _git("for-each-ref", "--sort=refname", cwd=repo)
    assert before == after


@pytest.mark.asyncio
async def test_snapshot_does_not_fire_pre_commit_or_commit_msg_hooks(
    tmp_path: Path, sandbox: SimpleNamespace
) -> None:
    repo = tmp_path / "hooks"
    repo.mkdir()
    _init_repo(repo)
    (repo / "a.py").write_text("a\n", encoding="utf-8")
    _commit_all(repo)

    hooks_dir = repo / ".git" / "hooks"
    marker_dir = tmp_path / "hook-markers"
    marker_dir.mkdir()

    def _install_hook(name: str, marker: Path) -> None:
        path = hooks_dir / name
        path.write_text(
            "#!/bin/sh\n"
            f"touch {marker!s}\n"
            "exit 0\n",
            encoding="utf-8",
        )
        path.chmod(0o755)

    pre_marker = marker_dir / "pre-commit.marker"
    msg_marker = marker_dir / "commit-msg.marker"
    _install_hook("pre-commit", pre_marker)
    _install_hook("commit-msg", msg_marker)

    (repo / "a.py").write_text("dirty\n", encoding="utf-8")

    await build_live_snapshot(sandbox, _exec_process, str(repo))

    assert not pre_marker.exists(), "pre-commit hook fired during snapshot"
    assert not msg_marker.exists(), "commit-msg hook fired during snapshot"


# ---------------------------------------------------------------------------
# Reachability + error paths
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_snapshot_sha_is_reachable_via_git_show(
    tmp_path: Path, sandbox: SimpleNamespace
) -> None:
    repo = tmp_path / "reach"
    repo.mkdir()
    _init_repo(repo)
    (repo / "src" / "pkg").mkdir(parents=True)
    (repo / "src" / "pkg" / "mod.py").write_text("m = 1\n", encoding="utf-8")
    _commit_all(repo)
    (repo / "src" / "pkg" / "mod.py").write_text("m = 2\n", encoding="utf-8")

    snap = await build_live_snapshot(sandbox, _exec_process, str(repo))

    # The classifier needs this exact call shape to load base_content.
    shown = _git("show", f"{snap}:src/pkg/mod.py", cwd=repo)
    assert shown == "m = 2\n"


@pytest.mark.asyncio
async def test_snapshot_works_on_empty_repo_without_head(
    tmp_path: Path, sandbox: SimpleNamespace
) -> None:
    repo = tmp_path / "empty"
    repo.mkdir()
    _init_repo(repo)  # no commits yet
    (repo / "first.py").write_text("hello\n", encoding="utf-8")

    snap = await build_live_snapshot(sandbox, _exec_process, str(repo))

    shown = _git("show", f"{snap}:first.py", cwd=repo)
    assert shown == "hello\n"


@pytest.mark.asyncio
async def test_snapshot_rejects_non_repo_directory(
    tmp_path: Path, sandbox: SimpleNamespace
) -> None:
    plain_dir = tmp_path / "plain"
    plain_dir.mkdir()
    (plain_dir / "file.txt").write_text("hi\n", encoding="utf-8")

    with pytest.raises(GitSnapshotError):
        await build_live_snapshot(sandbox, _exec_process, str(plain_dir))


@pytest.mark.asyncio
async def test_snapshot_rejects_linked_git_worktree(
    tmp_path: Path, sandbox: SimpleNamespace
) -> None:
    repo = tmp_path / "repo"
    repo.mkdir()
    _init_repo(repo)
    (repo / "a.py").write_text("a\n", encoding="utf-8")
    _commit_all(repo)

    linked_worktree = tmp_path / "linked-worktree"
    subprocess.run(
        ["git", "-C", str(repo), "worktree", "add", "-q", str(linked_worktree), "HEAD"],
        check=True,
    )
    try:
        assert (linked_worktree / ".git").is_file()

        with pytest.raises(GitSnapshotError, match="linked worktrees are not supported"):
            await build_live_snapshot(sandbox, _exec_process, str(linked_worktree))
    finally:
        subprocess.run(
            ["git", "-C", str(repo), "worktree", "remove", "--force", str(linked_worktree)],
            check=False,
        )


@pytest.mark.asyncio
async def test_snapshot_rejects_empty_repo_root(sandbox: SimpleNamespace) -> None:
    with pytest.raises(GitSnapshotError):
        await build_live_snapshot(sandbox, _exec_process, "")


@pytest.mark.asyncio
async def test_snapshot_surfaces_subprocess_failure(
    tmp_path: Path, sandbox: SimpleNamespace
) -> None:
    missing = tmp_path / "does-not-exist"

    with pytest.raises(GitSnapshotError):
        await build_live_snapshot(sandbox, _exec_process, str(missing))


@pytest.mark.asyncio
async def test_snapshot_does_not_mutate_working_tree(
    tmp_path: Path, sandbox: SimpleNamespace
) -> None:
    repo = tmp_path / "worktree"
    repo.mkdir()
    _init_repo(repo)
    (repo / "a.py").write_text("a\n", encoding="utf-8")
    _commit_all(repo)
    (repo / "a.py").write_text("dirty\n", encoding="utf-8")
    (repo / "new.txt").write_text("untracked\n", encoding="utf-8")

    before_status = _git("status", "--porcelain", cwd=repo)
    before_file = (repo / "a.py").read_text(encoding="utf-8")

    await build_live_snapshot(sandbox, _exec_process, str(repo))

    after_status = _git("status", "--porcelain", cwd=repo)
    after_file = (repo / "a.py").read_text(encoding="utf-8")
    assert before_status == after_status
    assert before_file == after_file
    # Untracked file must still be present.
    assert (repo / "new.txt").read_text(encoding="utf-8") == "untracked\n"


# ---------------------------------------------------------------------------
# Guard: the tempfile index must be cleaned up
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_snapshot_does_not_leave_temp_index_files(
    tmp_path: Path, sandbox: SimpleNamespace, monkeypatch: pytest.MonkeyPatch
) -> None:
    # Point TMPDIR at a scratch dir so we can scan it for stray indexes.
    scratch = tmp_path / "tmp"
    scratch.mkdir()
    monkeypatch.setenv("TMPDIR", str(scratch))

    repo = tmp_path / "tmpidx"
    repo.mkdir()
    _init_repo(repo)
    (repo / "a.py").write_text("a\n", encoding="utf-8")
    _commit_all(repo)

    await build_live_snapshot(sandbox, _exec_process, str(repo))

    leftovers = [
        name for name in os.listdir(scratch) if name.startswith("git-snapshot-idx-")
    ]
    assert leftovers == [], f"stray tempfile indexes left: {leftovers}"
