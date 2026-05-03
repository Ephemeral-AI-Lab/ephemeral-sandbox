"""OCC-owned routing for raw overlay changes."""

from __future__ import annotations

import subprocess
from pathlib import Path

import pytest

from sandbox.occ.changeset import ChangesetResult
from sandbox.overlay.types import UpperChange
from sandbox.runtime.registry import dispose_all_code_intelligence
from sandbox.runtime.service import CodeIntelligenceService


@pytest.fixture(autouse=True)
def _registry() -> None:
    dispose_all_code_intelligence()
    yield
    dispose_all_code_intelligence()


def _init_repo(path: Path) -> None:
    subprocess.run(["git", "-C", str(path), "init", "-q"], check=True)
    subprocess.run(
        ["git", "-C", str(path), "config", "user.email", "t@example.invalid"],
        check=True,
    )
    subprocess.run(
        ["git", "-C", str(path), "config", "user.name", "Test"], check=True
    )


def _svc(repo: Path) -> CodeIntelligenceService:
    return CodeIntelligenceService(
        sandbox_id=f"changeset-{repo.name}",
        workspace_root=str(repo),
    )


def _regular(
    rel: str,
    *,
    base: bytes | None = None,
    upper: bytes,
    existed: bool | None = None,
) -> UpperChange:
    return UpperChange(
        rel=rel,
        kind="regular",
        base_bytes=base,
        upper_bytes=upper,
        base_existed=(base is not None if existed is None else existed),
    )


def test_apply_changeset_drops_dotgit_writes_and_ledgers_tracked_file(
    tmp_path: Path,
) -> None:
    repo = tmp_path / "repo"
    repo.mkdir()
    _init_repo(repo)
    (repo / "src").mkdir()
    (repo / "src" / "foo.py").write_text("old\n", encoding="utf-8")
    head = repo / ".git" / "HEAD"
    before_head = head.read_bytes()

    result = _svc(repo)._write_coordinator.apply_changeset(
        [
            _regular("src/foo.py", base=b"old\n", upper=b"new\n"),
            _regular(".git/HEAD", base=before_head, upper=b"bad\n"),
            _regular(".git/index", base=None, upper=b"index"),
        ],
        agent_id="agent",
        edit_type="test",
        description="changeset",
    )

    assert result.success is True
    assert result.ledgered == (str(repo / "src" / "foo.py"),)
    assert result.direct_merged == ()
    assert (repo / "src" / "foo.py").read_text(encoding="utf-8") == "new\n"
    assert head.read_bytes() == before_head


def test_apply_changeset_mixes_ledger_and_gitignored_binary_direct_merge(
    tmp_path: Path,
) -> None:
    repo = tmp_path / "repo"
    repo.mkdir()
    _init_repo(repo)
    (repo / ".gitignore").write_text(".venv/\n*.pyc\n", encoding="utf-8")
    (repo / "src").mkdir()
    (repo / "src" / "foo.py").write_text("old\n", encoding="utf-8")

    result = _svc(repo)._write_coordinator.apply_changeset(
        [
            _regular("src/foo.py", base=b"old\n", upper=b"new\n"),
            _regular(".venv/x", base=None, upper=b"venv-bytes"),
            _regular("pkg/__pycache__/a.pyc", base=None, upper=b"\x00\xffpyc"),
        ],
        agent_id="agent",
        edit_type="test",
        description="changeset",
    )

    assert result.success is True
    assert result.ledgered == (str(repo / "src" / "foo.py"),)
    assert sorted(result.direct_merged) == sorted(
        [str(repo / ".venv" / "x"), str(repo / "pkg" / "__pycache__" / "a.pyc")]
    )
    assert (repo / ".venv" / "x").read_bytes() == b"venv-bytes"
    assert (repo / "pkg" / "__pycache__" / "a.pyc").read_bytes() == b"\x00\xffpyc"


def test_apply_changeset_conflict_on_gitinclude_binary_after_direct_merge(
    tmp_path: Path,
) -> None:
    repo = tmp_path / "repo"
    repo.mkdir()
    _init_repo(repo)
    (repo / ".gitignore").write_text(".venv/\n", encoding="utf-8")

    result = _svc(repo)._write_coordinator.apply_changeset(
        [
            _regular(".venv/x", base=None, upper=b"direct"),
            _regular("src/bin.dat", base=None, upper=b"\xff\xfe"),
        ],
        agent_id="agent",
        edit_type="test",
        description="changeset",
    )

    assert result.success is False
    assert result.conflict_reason == "patch_failed"
    assert result.conflict_file == str(repo / "src" / "bin.dat")
    assert result.direct_merged == (str(repo / ".venv" / "x"),)
    assert (repo / ".venv" / "x").read_bytes() == b"direct"
    assert not (repo / "src" / "bin.dat").exists()


def test_apply_changeset_conflicts_on_tracked_symlink(tmp_path: Path) -> None:
    repo = tmp_path / "repo"
    repo.mkdir()
    _init_repo(repo)

    result = _svc(repo)._write_coordinator.apply_changeset(
        [
            UpperChange(
                rel="src/link",
                kind="symlink",
                base_bytes=None,
                upper_bytes=b"target",
                base_existed=False,
            )
        ],
        agent_id="agent",
        edit_type="test",
        description="changeset",
    )

    assert result.success is False
    assert result.conflict_reason == "patch_failed"
    assert result.conflict_file == str(repo / "src" / "link")
    assert not (repo / "src" / "link").exists()


def test_apply_changeset_narrow_prunes_gitignored_opaque_dir(tmp_path: Path) -> None:
    repo = tmp_path / "repo"
    repo.mkdir()
    _init_repo(repo)
    (repo / ".gitignore").write_text(".cache/\n", encoding="utf-8")
    (repo / ".cache").mkdir()
    (repo / ".cache" / "old").write_text("old\n", encoding="utf-8")

    result = _svc(repo)._write_coordinator.apply_changeset(
        [
            UpperChange(
                rel=".cache",
                kind="opaque_dir",
                base_bytes=None,
                upper_bytes=None,
                base_existed=True,
            ),
            _regular(".cache/keep", base=None, upper=b"keep\n"),
        ],
        agent_id="agent",
        edit_type="test",
        description="changeset",
    )

    assert result.success is True
    assert not (repo / ".cache" / "old").exists()
    assert (repo / ".cache" / "keep").read_bytes() == b"keep\n"


def test_apply_changeset_maps_argv_overflow_to_result(tmp_path: Path) -> None:
    repo = tmp_path / "repo"
    repo.mkdir()
    _init_repo(repo)

    def _raise(_changes):
        raise RuntimeError("checked batch apply failed: argument list too long")

    from sandbox.occ.changeset import apply_changeset
    from sandbox.occ.content.manager import ContentManager

    result = apply_changeset(
        [_regular("src/foo.py", base=None, upper=b"x\n")],
        workspace_root=str(repo),
        content=ContentManager(str(repo)),
        commit=_raise,
    )

    assert isinstance(result, ChangesetResult)
    assert result.success is False
    assert result.conflict_reason == "argv_too_large"
    assert result.conflict_file == str(repo / "src" / "foo.py")
