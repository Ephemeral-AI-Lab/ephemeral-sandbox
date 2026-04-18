"""Unit tests for WriteCoordinator.commit_operation_against_base (atomic operation)."""

from __future__ import annotations

from unittest.mock import MagicMock

import pytest
from code_intelligence.hashing import content_hash
from code_intelligence.routing.service import (
    CodeIntelligenceService,
    dispose_all_code_intelligence,
)
from code_intelligence.types import OperationChange, SemanticFileChange


@pytest.fixture(autouse=True)
def _clear_registry() -> None:
    dispose_all_code_intelligence()
    yield
    dispose_all_code_intelligence()


def _svc(tmp_path) -> CodeIntelligenceService:
    return CodeIntelligenceService(
        sandbox_id=f"sandbox-operation-{tmp_path.name}",
        workspace_root=str(tmp_path),
    )


def _change(path: str, base: str, final: str) -> SemanticFileChange:
    return SemanticFileChange(
        file_path=path,
        base_content=base,
        base_hash=content_hash(base),
        final_content=final,
    )


def test_commits_full_operation_on_clean_bases(tmp_path) -> None:
    a = tmp_path / "a.py"
    b = tmp_path / "b.py"
    a.write_text("x = 1\n", encoding="utf-8")
    b.write_text("y = 2\n", encoding="utf-8")

    svc = _svc(tmp_path)
    result = svc.commit_operation_against_base(
        [
            _change(str(a), "x = 1\n", "x = 11\n"),
            _change(str(b), "y = 2\n", "y = 22\n"),
        ],
        edit_type="rename",
        description="test",
    )
    assert result.success is True
    assert result.status == "committed"
    assert a.read_text(encoding="utf-8") == "x = 11\n"
    assert b.read_text(encoding="utf-8") == "y = 22\n"


def test_aborts_on_overlapping_concurrent_edit(tmp_path) -> None:
    a = tmp_path / "a.py"
    a.write_text("def foo():\n    return 1\n", encoding="utf-8")
    svc = _svc(tmp_path)

    base = "def foo():\n    return 1\n"
    final = "def bar():\n    return 1\n"
    # Concurrent drift: the same first line got edited.
    a.write_text("def foo_drift():\n    return 1\n", encoding="utf-8")

    result = svc.commit_operation_against_base(
        [_change(str(a), base, final)],
        edit_type="rename",
    )
    assert result.success is False
    assert result.status in {"aborted_overlap", "aborted_version"}
    # Concurrent edit preserved, rename not applied.
    assert "foo_drift" in a.read_text(encoding="utf-8")


def test_merges_non_overlapping_concurrent_edit(tmp_path) -> None:
    a = tmp_path / "a.py"
    base = "def foo():\n    return 1\n\nZ = 0\n"
    a.write_text(base, encoding="utf-8")
    svc = _svc(tmp_path)

    # Jedi renamed foo → bar at the top; someone else appended an
    # unrelated line at the bottom after Jedi's snapshot.
    final = "def bar():\n    return 1\n\nZ = 0\n"
    a.write_text(base + "NEW = 1\n", encoding="utf-8")

    result = svc.commit_operation_against_base(
        [_change(str(a), base, final)],
        edit_type="rename",
    )
    assert result.success is True, result.conflict_reason
    text = a.read_text(encoding="utf-8")
    assert "def bar()" in text
    assert "NEW = 1" in text  # concurrent edit preserved


def test_lsp_invalidate_and_symbol_index_refresh_per_committed_path(tmp_path) -> None:
    a = tmp_path / "a.py"
    b = tmp_path / "b.py"
    a.write_text("x=1\n", encoding="utf-8")
    b.write_text("y=2\n", encoding="utf-8")

    svc = _svc(tmp_path)
    svc.lsp_client = MagicMock()
    svc.symbol_index = MagicMock()
    svc._write_coordinator._lsp_client = svc.lsp_client
    svc._write_coordinator._symbol_index = svc.symbol_index

    result = svc.commit_operation_against_base(
        [
            _change(str(a), "x=1\n", "x=10\n"),
            _change(str(b), "y=2\n", "y=20\n"),
        ],
        edit_type="rename",
    )
    assert result.success
    invalidated = sorted(
        call.args[0] for call in svc.lsp_client.invalidate.call_args_list
    )
    refreshed = sorted(
        call.args[0] for call in svc.symbol_index.refresh.call_args_list
    )
    assert invalidated == sorted([str(a), str(b)])
    assert refreshed == sorted([str(a), str(b)])


def test_locks_acquired_in_sorted_order(tmp_path) -> None:
    a = tmp_path / "zzz.py"
    b = tmp_path / "aaa.py"
    c = tmp_path / "mmm.py"
    for p in (a, b, c):
        p.write_text("x=1\n", encoding="utf-8")

    svc = _svc(tmp_path)
    order: list[str] = []
    real_acquire = svc.arbiter.acquire_file_lock

    def _spy(path, *args, **kwargs):
        order.append(path)
        return real_acquire(path, *args, **kwargs)

    svc.arbiter.acquire_file_lock = _spy  # type: ignore[assignment]

    result = svc.commit_operation_against_base(
        [
            _change(str(a), "x=1\n", "x=2\n"),
            _change(str(b), "x=1\n", "x=3\n"),
            _change(str(c), "x=1\n", "x=4\n"),
        ],
        edit_type="rename",
    )
    assert result.success
    assert order == sorted([str(a), str(b), str(c)])


def test_empty_changes_returns_committed_no_op(tmp_path) -> None:
    svc = _svc(tmp_path)
    result = svc.commit_operation_against_base([], edit_type="rename")
    assert result.success is True
    assert result.status == "committed"
    assert result.files == ()


# ---------------------------------------------------------------------------
# New tests for delete / create / mixed semantics (commit_operation_against_base)
# ---------------------------------------------------------------------------


def _delete_change(path: str, base: str) -> OperationChange:
    """Build a delete OperationChange (final_content=None)."""
    return OperationChange(
        file_path=path,
        base_content=base,
        base_hash=content_hash(base),
        final_content=None,
    )


def _create_change(path: str, content: str) -> OperationChange:
    """Build a create OperationChange (base_existed=False)."""
    return OperationChange(
        file_path=path,
        base_content="",
        base_hash=content_hash(""),
        final_content=content,
        base_existed=False,
    )


def test_delete_only_operation_removes_file(tmp_path) -> None:
    a = tmp_path / "del.py"
    a.write_text("x = 1\n", encoding="utf-8")
    svc = _svc(tmp_path)

    result = svc.commit_operation_against_base(
        [_delete_change(str(a), "x = 1\n")],
        edit_type="delete",
    )
    assert result.success is True
    assert result.status == "committed"
    assert not a.exists()


def test_create_only_operation_writes_new_file(tmp_path) -> None:
    a = tmp_path / "new.py"
    assert not a.exists()
    svc = _svc(tmp_path)

    result = svc.commit_operation_against_base(
        [_create_change(str(a), "x = 42\n")],
        edit_type="create",
    )
    assert result.success is True
    assert result.status == "committed"
    assert a.read_text(encoding="utf-8") == "x = 42\n"


def test_create_conflicts_when_file_already_exists(tmp_path) -> None:
    a = tmp_path / "existing.py"
    a.write_text("old content\n", encoding="utf-8")
    svc = _svc(tmp_path)

    result = svc.commit_operation_against_base(
        [_create_change(str(a), "new content\n")],
        edit_type="create",
    )
    assert result.success is False
    assert result.status == "aborted_version"
    assert "already exists" in result.conflict_reason
    # Original file untouched
    assert a.read_text(encoding="utf-8") == "old content\n"


def test_delete_conflicts_on_base_mismatch_no_merge(tmp_path) -> None:
    a = tmp_path / "changed.py"
    a.write_text("x = 1\n", encoding="utf-8")
    svc = _svc(tmp_path)

    # Drift: file changed after snapshot
    a.write_text("x = 999\n", encoding="utf-8")

    result = svc.commit_operation_against_base(
        [_delete_change(str(a), "x = 1\n")],  # base_hash doesn't match current
        edit_type="delete",
    )
    assert result.success is False
    assert result.status == "aborted_version"
    assert "changed before delete" in result.conflict_reason
    # File must still exist
    assert a.exists()


def test_mixed_modify_create_delete_operation(tmp_path) -> None:
    mod_file = tmp_path / "mod.py"
    del_file = tmp_path / "del.py"
    new_file = tmp_path / "new.py"

    mod_file.write_text("x = 1\n", encoding="utf-8")
    del_file.write_text("y = 2\n", encoding="utf-8")
    assert not new_file.exists()

    svc = _svc(tmp_path)
    result = svc.commit_operation_against_base(
        [
            _change(str(mod_file), "x = 1\n", "x = 10\n"),
            _delete_change(str(del_file), "y = 2\n"),
            _create_change(str(new_file), "z = 3\n"),
        ],
        edit_type="operation",
    )
    assert result.success is True
    assert result.status == "committed"
    assert mod_file.read_text(encoding="utf-8") == "x = 10\n"
    assert not del_file.exists()
    assert new_file.read_text(encoding="utf-8") == "z = 3\n"


def test_base_mismatch_non_overlapping_merges(tmp_path) -> None:
    """Modify with non-overlapping concurrent edit merges successfully."""
    a = tmp_path / "merge.py"
    base = "def foo():\n    return 1\n\nZ = 0\n"
    a.write_text(base, encoding="utf-8")
    svc = _svc(tmp_path)

    final = "def bar():\n    return 1\n\nZ = 0\n"
    # Concurrent drift at bottom — non-overlapping
    a.write_text(base + "NEW = 1\n", encoding="utf-8")

    result = svc.commit_operation_against_base(
        [_change(str(a), base, final)],
        edit_type="rename",
    )
    assert result.success is True, result.conflict_reason
    text = a.read_text(encoding="utf-8")
    assert "def bar()" in text
    assert "NEW = 1" in text


def test_base_mismatch_overlap_aborts_overlap(tmp_path) -> None:
    """Modify with overlapping concurrent edit returns aborted_overlap."""
    a = tmp_path / "overlap.py"
    base = "def foo():\n    return 1\n"
    a.write_text(base, encoding="utf-8")
    svc = _svc(tmp_path)

    final = "def bar():\n    return 1\n"
    # Concurrent drift: same first line got edited (overlapping)
    a.write_text("def foo_drift():\n    return 1\n", encoding="utf-8")

    result = svc.commit_operation_against_base(
        [_change(str(a), base, final)],
        edit_type="rename",
    )
    assert result.success is False
    assert result.status == "aborted_overlap"
    # Concurrent edit preserved
    assert "foo_drift" in a.read_text(encoding="utf-8")


def test_mid_operation_write_failure_rolls_back_prior_files(tmp_path) -> None:
    """A write failure on the second file rolls back the first file."""
    a = tmp_path / "first.py"
    b = tmp_path / "second.py"
    a.write_text("a = 1\n", encoding="utf-8")
    b.write_text("b = 2\n", encoding="utf-8")

    svc = _svc(tmp_path)

    call_count = 0
    original_write = svc._write_coordinator._content.write

    def _failing_write(path: str, content: str) -> None:
        nonlocal call_count
        call_count += 1
        if call_count == 2:
            raise OSError("simulated write failure")
        original_write(path, content)

    svc._write_coordinator._content.write = _failing_write  # type: ignore[assignment]

    result = svc.commit_operation_against_base(
        [
            _change(str(a), "a = 1\n", "a = 10\n"),
            _change(str(b), "b = 2\n", "b = 20\n"),
        ],
        edit_type="rename",
    )
    assert result.success is False
    assert result.status == "failed"
    # First file should be rolled back to its original content
    assert a.read_text(encoding="utf-8") == "a = 1\n"


# ---------------------------------------------------------------------------
# strict_base (skip merge fallback on hash mismatch)
# ---------------------------------------------------------------------------


def test_strict_base_aborts_when_merge_would_succeed(tmp_path) -> None:
    """strict_base=True skips merge_non_overlapping_edit on drift."""
    a = tmp_path / "strict.py"
    base = "def foo():\n    return 1\n\nZ = 0\n"
    a.write_text(base, encoding="utf-8")
    svc = _svc(tmp_path)

    # Non-overlapping drift — without strict_base this merges successfully
    # (see test_base_mismatch_non_overlapping_merges above).
    a.write_text(base + "NEW = 1\n", encoding="utf-8")
    final = "def bar():\n    return 1\n\nZ = 0\n"

    result = svc.commit_operation_against_base(
        [
            OperationChange(
                file_path=str(a),
                base_content=base,
                base_hash=content_hash(base),
                final_content=final,
                strict_base=True,
            ),
        ],
        edit_type="move_overwrite",
    )
    assert result.success is False
    assert result.status == "aborted_version"
    # Concurrent edit preserved verbatim — the strict write never ran.
    assert "NEW = 1" in a.read_text(encoding="utf-8")
    assert "def bar" not in a.read_text(encoding="utf-8")


def test_strict_base_commits_when_hash_matches(tmp_path) -> None:
    a = tmp_path / "strict_ok.py"
    a.write_text("x = 1\n", encoding="utf-8")
    svc = _svc(tmp_path)

    result = svc.commit_operation_against_base(
        [
            OperationChange(
                file_path=str(a),
                base_content="x = 1\n",
                base_hash=content_hash("x = 1\n"),
                final_content="x = 2\n",
                strict_base=True,
            ),
        ],
        edit_type="move_overwrite",
    )
    assert result.success is True
    assert a.read_text(encoding="utf-8") == "x = 2\n"


# ---------------------------------------------------------------------------
# Service-level delete_file / move_file (OCC-gated facade)
# ---------------------------------------------------------------------------


def test_delete_file_removes_existing_file(tmp_path) -> None:
    a = tmp_path / "d.py"
    a.write_text("x = 1\n", encoding="utf-8")
    svc = _svc(tmp_path)

    result = svc.delete_file(str(a))
    assert result.success is True
    assert result.status == "committed"
    assert not a.exists()


def test_delete_file_reports_not_found(tmp_path) -> None:
    svc = _svc(tmp_path)
    result = svc.delete_file(str(tmp_path / "missing.py"))
    assert result.success is False
    assert result.status == "failed"
    assert result.conflict_reason == "not_found"


def test_move_file_creates_new_destination(tmp_path) -> None:
    src = tmp_path / "src.py"
    dst = tmp_path / "dst.py"
    src.write_text("payload\n", encoding="utf-8")
    svc = _svc(tmp_path)

    result = svc.move_file(str(src), str(dst))
    assert result.success is True
    assert result.status == "committed"
    assert not src.exists()
    assert dst.read_text(encoding="utf-8") == "payload\n"


def test_move_file_rejects_existing_dst_without_overwrite(tmp_path) -> None:
    src = tmp_path / "src.py"
    dst = tmp_path / "dst.py"
    src.write_text("one\n", encoding="utf-8")
    dst.write_text("two\n", encoding="utf-8")
    svc = _svc(tmp_path)

    result = svc.move_file(str(src), str(dst))
    assert result.success is False
    assert result.conflict_reason == "dst_exists"
    # No partial move
    assert src.read_text(encoding="utf-8") == "one\n"
    assert dst.read_text(encoding="utf-8") == "two\n"


def test_move_file_overwrites_when_allowed(tmp_path) -> None:
    src = tmp_path / "src.py"
    dst = tmp_path / "dst.py"
    src.write_text("one\n", encoding="utf-8")
    dst.write_text("two\n", encoding="utf-8")
    svc = _svc(tmp_path)

    result = svc.move_file(str(src), str(dst), overwrite=True)
    assert result.success is True
    assert not src.exists()
    assert dst.read_text(encoding="utf-8") == "one\n"


def test_move_file_overwrite_aborts_on_dst_drift(tmp_path) -> None:
    """strict_base on the dst change forbids silent merges of concurrent dst edits."""
    src = tmp_path / "src.py"
    dst = tmp_path / "dst.py"
    src.write_text("one\n", encoding="utf-8")
    dst.write_text("two\n", encoding="utf-8")
    svc = _svc(tmp_path)

    # Read base content (what move_file will capture internally), then drift the dst.
    # Use a ContentManager-style read to grab the same snapshot semantics.
    # We inject drift by making the read happen first then corrupting dst.
    import code_intelligence.routing.service as service_mod

    original_read = svc._content.read

    reads: list[str] = []

    def _drift_read(file_path: str, *, allow_missing: bool = False):
        result = original_read(file_path, allow_missing=allow_missing)
        reads.append(file_path)
        # After the move_file helper reads dst, corrupt it before commit acquires locks
        if file_path == str(dst):
            dst.write_text("drift!\n", encoding="utf-8")
        return result

    svc._content.read = _drift_read  # type: ignore[assignment]
    try:
        result = svc.move_file(str(src), str(dst), overwrite=True)
    finally:
        svc._content.read = original_read  # type: ignore[assignment]

    assert result.success is False
    assert result.status == "aborted_version"
    # Neither src nor dst mutated: src preserved, dst has the drifted content.
    assert src.read_text(encoding="utf-8") == "one\n"
    assert dst.read_text(encoding="utf-8") == "drift!\n"
    del service_mod  # silence unused import linter if any


def test_move_file_identical_paths_rejected(tmp_path) -> None:
    svc = _svc(tmp_path)
    a = tmp_path / "same.py"
    a.write_text("x\n", encoding="utf-8")
    result = svc.move_file(str(a), str(a))
    assert result.success is False
    assert result.conflict_reason == "identical_paths"


def test_move_file_missing_src_reports_not_found(tmp_path) -> None:
    svc = _svc(tmp_path)
    result = svc.move_file(
        str(tmp_path / "missing.py"),
        str(tmp_path / "dst.py"),
    )
    assert result.success is False
    assert result.conflict_reason == "not_found"
