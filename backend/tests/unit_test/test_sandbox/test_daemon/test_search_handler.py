"""``api.glob`` / ``api.grep`` daemon handler tests."""

from __future__ import annotations

from pathlib import Path

import pytest

from sandbox.daemon import occ_backend, workspace_server
from sandbox.daemon.handler.glob import DEFAULT_GLOB_LIMIT, _glob_sync
from sandbox.daemon.handler.grep import (
    MAX_GREP_CONTENT_BYTES,
    MAX_GREP_FILE_BYTES,
    _grep_sync,
)
from sandbox.daemon.occ_backend import build_occ_backend
from sandbox.layer_stack.workspace_base import build_workspace_base


@pytest.fixture(autouse=True)
def _clear_runtime_caches() -> None:
    occ_backend.clear_backend_cache()
    workspace_server.clear_layer_stack_server_caches_for_tests()
    try:
        yield
    finally:
        occ_backend.clear_backend_cache()
        workspace_server.clear_layer_stack_server_caches_for_tests()


def _seed_workspace(
    tmp_path: Path,
    *,
    files: dict[str, str],
) -> tuple[str, str]:
    """Build a workspace + bound layer stack pre-populated with ``files``."""
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    for rel, content in files.items():
        target = workspace.joinpath(*rel.split("/"))
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(content, encoding="utf-8")
    stack = tmp_path / "layer-stack"
    build_workspace_base(workspace_root=workspace, layer_stack_root=stack)
    return stack.as_posix(), workspace.as_posix()


def _args(stack: str, **extra: object) -> dict[str, object]:
    return {"layer_stack_root": stack, **extra}


def test_glob_basic_pattern_returns_sorted_matches(tmp_path: Path) -> None:
    stack, _ = _seed_workspace(
        tmp_path,
        files={
            "a.py": "alpha",
            "pkg/b.py": "beta",
            "pkg/c.txt": "ctext",
            "notes.md": "n",
        },
    )

    result = _glob_sync(_args(stack, pattern="*.py"))

    assert result["success"] is True
    assert result["filenames"] == ["a.py", "pkg/b.py"]
    assert result["num_files"] == 2
    assert result["truncated"] is False


def test_glob_subpath_filter_restricts_scope(tmp_path: Path) -> None:
    stack, _ = _seed_workspace(
        tmp_path,
        files={
            "outer.py": "o",
            "pkg/inner.py": "i",
            "pkg/nested/leaf.py": "l",
        },
    )

    result = _glob_sync(_args(stack, pattern="*.py", path="pkg"))

    assert sorted(result["filenames"]) == ["pkg/inner.py", "pkg/nested/leaf.py"]


def test_glob_excludes_vcs_directories(tmp_path: Path) -> None:
    stack, _ = _seed_workspace(
        tmp_path,
        files={
            ".git/config": "x",
            ".git/HEAD": "y",
            "src/main.py": "z",
        },
    )

    result = _glob_sync(_args(stack, pattern="*"))

    assert "src/main.py" in result["filenames"]
    assert all(not name.startswith(".git/") for name in result["filenames"])


def test_glob_truncates_at_one_hundred(tmp_path: Path) -> None:
    files = {f"item_{i:03d}.txt": str(i) for i in range(105)}
    stack, _ = _seed_workspace(tmp_path, files=files)

    result = _glob_sync(_args(stack, pattern="item_*.txt"))

    assert result["num_files"] == DEFAULT_GLOB_LIMIT
    assert len(result["filenames"]) == DEFAULT_GLOB_LIMIT
    assert result["truncated"] is True


def test_glob_missing_pattern_raises(tmp_path: Path) -> None:
    stack, _ = _seed_workspace(tmp_path, files={"a.txt": "a"})

    with pytest.raises(ValueError, match="pattern is required"):
        _glob_sync(_args(stack))


def test_glob_acquires_and_releases_lease(tmp_path: Path) -> None:
    stack, _ = _seed_workspace(tmp_path, files={"a.txt": "a"})
    services = build_occ_backend(stack)
    before = services.manager.active_lease_count()

    _glob_sync(_args(stack, pattern="*.txt"))

    assert services.manager.active_lease_count() == before


def test_glob_releases_lease_when_validation_rejects_path(
    tmp_path: Path,
) -> None:
    """Pre-lease validation failures (out-of-workspace path) raise before any
    lease is acquired, so the counter is trivially balanced."""
    stack, _ = _seed_workspace(tmp_path, files={"a.txt": "a"})
    services = build_occ_backend(stack)
    before = services.manager.active_lease_count()

    with pytest.raises(ValueError, match="search path must be inside the workspace"):
        _glob_sync(_args(stack, pattern="*.txt", path="/etc"))

    assert services.manager.active_lease_count() == before


def test_glob_releases_lease_when_scan_raises_mid_flight(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """The try/finally guard must release the lease when iter_paths raises
    after the lease has been acquired."""
    stack, _ = _seed_workspace(tmp_path, files={"a.txt": "a"})
    services = build_occ_backend(stack)
    before = services.manager.active_lease_count()

    def boom(_manifest):
        raise RuntimeError("synthetic mid-scan failure")

    monkeypatch.setattr(services.layer_stack, "iter_paths", boom)

    with pytest.raises(RuntimeError, match="synthetic mid-scan failure"):
        _glob_sync(_args(stack, pattern="*.txt"))

    assert services.manager.active_lease_count() == before


def test_grep_files_with_matches_mode(tmp_path: Path) -> None:
    stack, _ = _seed_workspace(
        tmp_path,
        files={
            "a.py": "hello world\n",
            "b.py": "goodbye\n",
            "c.txt": "hello again\n",
        },
    )

    result = _grep_sync(_args(stack, pattern="hello"))

    assert result["success"] is True
    assert result["output_mode"] == "files_with_matches"
    assert sorted(result["filenames"]) == ["a.py", "c.txt"]
    assert result["num_files"] == 2
    assert result["truncated"] is False


def test_grep_count_mode_returns_per_file_counts(tmp_path: Path) -> None:
    stack, _ = _seed_workspace(
        tmp_path,
        files={
            "a.py": "hello hello world\n",
            "b.py": "hello\nnope\nhello\n",
            "c.py": "miss\n",
        },
    )

    result = _grep_sync(
        _args(stack, pattern="hello", output_mode="count")
    )

    assert result["output_mode"] == "count"
    assert set(result["filenames"]) == {"a.py", "b.py"}
    assert result["num_matches"] == 4
    content_pairs = dict(
        line.split(":", 1) for line in result["content"].splitlines()
    )
    assert content_pairs == {"a.py": "2", "b.py": "2"}


def test_grep_content_mode_emits_filename_line_pairs(tmp_path: Path) -> None:
    stack, _ = _seed_workspace(
        tmp_path,
        files={
            "a.py": "alpha\nhello world\nbeta\n",
        },
    )

    result = _grep_sync(
        _args(stack, pattern="hello", output_mode="content", line_numbers=True)
    )

    assert result["output_mode"] == "content"
    assert "a.py:2:hello world\n" in result["content"]
    assert result["num_lines"] == 1
    assert result["num_matches"] == 1


def test_grep_case_insensitive_flag(tmp_path: Path) -> None:
    stack, _ = _seed_workspace(
        tmp_path,
        files={"a.py": "Hello there\n", "b.py": "miss\n"},
    )

    result = _grep_sync(
        _args(stack, pattern="hello", case_insensitive=True)
    )

    assert result["filenames"] == ["a.py"]


def test_grep_glob_filter_narrows_files(tmp_path: Path) -> None:
    stack, _ = _seed_workspace(
        tmp_path,
        files={
            "a.py": "needle\n",
            "b.txt": "needle\n",
            "c.py": "needle\n",
        },
    )

    result = _grep_sync(
        _args(stack, pattern="needle", glob_filter="*.py")
    )

    assert sorted(result["filenames"]) == ["a.py", "c.py"]


def test_grep_head_limit_truncates(tmp_path: Path) -> None:
    files = {f"f{i:03d}.txt": "needle\n" for i in range(10)}
    stack, _ = _seed_workspace(tmp_path, files=files)

    result = _grep_sync(
        _args(stack, pattern="needle", head_limit=5)
    )

    assert len(result["filenames"]) == 5
    assert result["truncated"] is True
    assert result["applied_limit"] == 5


def test_grep_exact_head_limit_does_not_mark_truncated(
    tmp_path: Path,
) -> None:
    files = {f"f{i:03d}.txt": "needle\n" for i in range(5)}
    stack, _ = _seed_workspace(tmp_path, files=files)

    files_mode = _grep_sync(
        _args(stack, pattern="needle", head_limit=5)
    )
    count_mode = _grep_sync(
        _args(stack, pattern="needle", output_mode="count", head_limit=5)
    )
    content_mode = _grep_sync(
        _args(stack, pattern="needle", output_mode="content", head_limit=5)
    )

    assert files_mode["filenames"] == [f"f{i:03d}.txt" for i in range(5)]
    assert files_mode["truncated"] is False
    assert count_mode["num_files"] == 5
    assert count_mode["truncated"] is False
    assert content_mode["num_lines"] == 5
    assert content_mode["truncated"] is False


def test_grep_zero_head_limit_is_unlimited(tmp_path: Path) -> None:
    """``head_limit=0`` is the documented unlimited sentinel — scanning must
    not stop at the 250-entry default and ``applied_limit`` must surface as
    ``None``."""
    files = {f"f{i:03d}.txt": "needle\n" for i in range(10)}
    stack, _ = _seed_workspace(tmp_path, files=files)

    result = _grep_sync(
        _args(stack, pattern="needle", head_limit=0)
    )

    assert len(result["filenames"]) == 10
    assert result["truncated"] is False
    assert result["applied_limit"] is None


def test_grep_content_cap_at_twenty_kb(tmp_path: Path) -> None:
    """Content output must be capped at the 20 KB ceiling regardless of head_limit."""
    line = "needle " * 100 + "\n"  # ~700 bytes after filename prefix
    # 50 matching files × ~10 lines each comfortably exceeds 20 KB.
    files = {
        f"file_{i:03d}.txt": (line * 10)
        for i in range(50)
    }
    stack, _ = _seed_workspace(tmp_path, files=files)

    result = _grep_sync(
        _args(
            stack,
            pattern="needle",
            output_mode="content",
            head_limit=0,  # unlimited entries; content cap must still kick in
        )
    )

    assert len(result["content"].encode("utf-8")) <= MAX_GREP_CONTENT_BYTES
    assert result["truncated"] is True


def test_grep_skips_files_larger_than_max(tmp_path: Path) -> None:
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    huge = workspace / "huge.txt"
    huge.write_bytes(b"needle " * (MAX_GREP_FILE_BYTES // 6))
    (workspace / "small.txt").write_text("needle\n", encoding="utf-8")
    stack = tmp_path / "layer-stack"
    build_workspace_base(
        workspace_root=workspace, layer_stack_root=stack
    )

    result = _grep_sync(
        _args(stack.as_posix(), pattern="needle")
    )

    assert result["filenames"] == ["small.txt"]
    assert result["timings"]["api.grep.skipped_large"] >= 1


def test_grep_skips_non_utf8_files(tmp_path: Path) -> None:
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    binary = workspace / "blob.bin"
    binary.write_bytes(b"\xff\xfe\x00\x01needle\x00")
    (workspace / "ok.txt").write_text("needle\n", encoding="utf-8")
    stack = tmp_path / "layer-stack"
    build_workspace_base(
        workspace_root=workspace, layer_stack_root=stack
    )

    result = _grep_sync(
        _args(stack.as_posix(), pattern="needle")
    )

    assert result["filenames"] == ["ok.txt"]
    assert result["timings"]["api.grep.skipped_binary"] >= 1


def test_grep_rejects_invalid_output_mode(tmp_path: Path) -> None:
    stack, _ = _seed_workspace(tmp_path, files={"a.txt": "needle"})

    with pytest.raises(ValueError, match="output_mode must be one of"):
        _grep_sync(
            _args(stack, pattern="needle", output_mode="bogus")
        )


def test_grep_rejects_invalid_regex(tmp_path: Path) -> None:
    stack, _ = _seed_workspace(tmp_path, files={"a.txt": "x"})

    with pytest.raises(ValueError, match="invalid regex pattern"):
        _grep_sync(_args(stack, pattern="("))


def test_grep_releases_lease_when_scan_raises_mid_flight(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """The try/finally guard must release the lease when iter_paths raises
    after the lease is held."""
    stack, _ = _seed_workspace(tmp_path, files={"a.txt": "needle"})
    services = build_occ_backend(stack)
    before = services.manager.active_lease_count()

    def boom(_manifest):
        raise RuntimeError("synthetic mid-scan failure")

    monkeypatch.setattr(services.layer_stack, "iter_paths", boom)

    with pytest.raises(RuntimeError, match="synthetic mid-scan failure"):
        _grep_sync(_args(stack, pattern="needle"))

    assert services.manager.active_lease_count() == before


def test_grep_excludes_vcs(tmp_path: Path) -> None:
    stack, _ = _seed_workspace(
        tmp_path,
        files={
            ".git/HEAD": "needle\n",
            "src/main.py": "needle\n",
        },
    )

    result = _grep_sync(_args(stack, pattern="needle"))

    assert ".git/HEAD" not in result["filenames"]
    assert "src/main.py" in result["filenames"]


def test_grep_handler_does_not_call_occ_client() -> None:
    """Read-only by construction: grep.py and glob.py must not touch the OCC
    mutation gate.
    """
    import sandbox.daemon.handler.grep as grep_module
    import sandbox.daemon.handler.glob as glob_module

    for module in (grep_module, glob_module):
        source = Path(module.__file__).read_text(encoding="utf-8")
        code_lines = [
            line for line in source.splitlines()
            if line.strip() and not line.strip().startswith(("#", '"', "'"))
        ]
        code = "\n".join(code_lines)
        name = module.__name__
        assert "occ_client." not in code, f"{name} must not access occ_client"
        assert "OccClient" not in code, f"{name} must not reference OccClient"
        assert ".commit_" not in code, f"{name} must not call commit_* methods"
        assert ".apply_" not in code, f"{name} must not call apply_* methods"
