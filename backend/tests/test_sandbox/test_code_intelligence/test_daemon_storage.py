"""Unit tests for ``sandbox.occ.state.ledger_store``."""

from __future__ import annotations

import stat
from pathlib import Path

import pytest

from sandbox.occ.state.ledger_store import (
    StoragePathEscape,
    StorageUnavailable,
    _confine,
    state_dir,
    workspace_root_hash,
)


@pytest.fixture
def home_in_tmp(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    """Re-home ``$HOME`` under ``tmp_path`` so state_dir lands in a sandboxed dir."""
    home = tmp_path / "home"
    home.mkdir()
    monkeypatch.setenv("HOME", str(home))
    return home


def test_workspace_root_hash_is_deterministic(tmp_path: Path) -> None:
    workspace = tmp_path / "ws"
    workspace.mkdir()
    h1 = workspace_root_hash(str(workspace))
    h2 = workspace_root_hash(str(workspace))
    assert h1 == h2
    assert len(h1) == 16
    assert all(c in "0123456789abcdef" for c in h1)


def test_workspace_root_hash_resolves_through_realpath(tmp_path: Path) -> None:
    target = tmp_path / "real_ws"
    target.mkdir()
    link = tmp_path / "link_ws"
    link.symlink_to(target)
    assert workspace_root_hash(str(link)) == workspace_root_hash(str(target))


def test_state_dir_creates_directory(home_in_tmp: Path) -> None:
    workspace = home_in_tmp.parent / "ws"
    workspace.mkdir()
    sd = state_dir(str(workspace))
    assert sd.exists()
    assert sd.is_dir()
    expected_suffix = (
        Path(".cache") / "eos-ci" / workspace_root_hash(str(workspace)) / "v1"
    )
    assert sd.relative_to(home_in_tmp) == expected_suffix


def test_state_dir_idempotent(home_in_tmp: Path) -> None:
    workspace = home_in_tmp.parent / "ws"
    workspace.mkdir()
    a = state_dir(str(workspace))
    b = state_dir(str(workspace))
    assert a == b


def test_state_dir_raises_on_unwritable_home(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    home = tmp_path / "ro_home"
    home.mkdir()
    cache = home / ".cache"
    cache.mkdir()
    # Drop write/search bits on .cache so mkdir cannot create eos-ci/
    cache.chmod(stat.S_IRUSR)
    monkeypatch.setenv("HOME", str(home))
    workspace = tmp_path / "ws"
    workspace.mkdir()
    try:
        with pytest.raises(StorageUnavailable) as exc:
            state_dir(str(workspace))
        assert exc.value.errno != 0
        assert ".cache/eos-ci" in exc.value.path
        assert str(home) in exc.value.message
    finally:
        cache.chmod(stat.S_IRUSR | stat.S_IWUSR | stat.S_IXUSR)


def test_confine_accepts_legal_name(tmp_path: Path) -> None:
    state = tmp_path / "state"
    state.mkdir()
    target = _confine(state, "ok.bin")
    assert target == (state / "ok.bin").resolve()


def test_confine_rejects_relative_traversal(tmp_path: Path) -> None:
    state = tmp_path / "state"
    state.mkdir()
    with pytest.raises(StoragePathEscape):
        _confine(state, "../escape.bin")


def test_confine_rejects_absolute_path(tmp_path: Path) -> None:
    state = tmp_path / "state"
    state.mkdir()
    with pytest.raises(StoragePathEscape):
        _confine(state, "/etc/passwd")


def test_confine_rejects_symlink_escape(tmp_path: Path) -> None:
    state = tmp_path / "state"
    state.mkdir()
    outside = tmp_path / "outside"
    outside.mkdir()
    (state / "evil").symlink_to(outside / "evil_target")
    with pytest.raises(StoragePathEscape):
        _confine(state, "evil")


def test_confine_rejects_state_itself(tmp_path: Path) -> None:
    state = tmp_path / "state"
    state.mkdir()
    with pytest.raises(StoragePathEscape):
        _confine(state, ".")


def test_storage_unavailable_carries_context(tmp_path: Path) -> None:
    err = StorageUnavailable(errno=13, path=str(tmp_path), message="permission denied")
    assert err.errno == 13
    assert err.path == str(tmp_path)
    assert err.message == "permission denied"
    assert "permission denied" in str(err)
