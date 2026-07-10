import pytest

from core.root import find_repo_root


def test_find_repo_root_skips_partial_marker_directory(tmp_path):
    root = tmp_path / "repo"
    nested = root / "nested" / "deeper"
    nested.mkdir(parents=True)
    (root / "Cargo.toml").touch()
    (root / "CLAUDE.md").touch()
    (nested.parent / "Cargo.toml").touch()

    assert find_repo_root(nested) == root


def test_find_repo_root_fails_without_markers(tmp_path):
    nested = tmp_path / "nested"
    nested.mkdir()

    with pytest.raises(FileNotFoundError):
        find_repo_root(nested)
