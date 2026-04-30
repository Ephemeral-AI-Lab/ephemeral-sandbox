"""Tests for the typed batch mutation APIs on :class:`CodeIntelligenceService`.

These exercise the shape contract (single tool call -> one OCC batch ->
one :class:`OperationResult`) for ``svc.write_file``, ``svc.edit_file``,
and the broadened ``svc.delete_file`` / ``svc.move_file``. Low-level commit
semantics (drift, strict-base, sorted locking) stay covered by
``test_write_coordinator_batch.py``; here we check how the service layer feeds
the coordinator.
"""

from __future__ import annotations

import pytest
from sandbox.code_intelligence.mutations.patcher import SearchReplaceEdit
from sandbox.code_intelligence.service import (
    CodeIntelligenceService,
    dispose_all_code_intelligence,
)
from sandbox.code_intelligence.core.types import (
    DeleteSpec,
    EditRequest,
    EditSpec,
    MoveSpec,
    WriteSpec,
)


@pytest.fixture(autouse=True)
def _clear_registry() -> None:
    dispose_all_code_intelligence()
    yield
    dispose_all_code_intelligence()


def _svc(tmp_path) -> CodeIntelligenceService:
    return CodeIntelligenceService(
        sandbox_id=f"sandbox-batch-{tmp_path.name}",
        workspace_root=str(tmp_path),
    )


# ---------------------------------------------------------------------------
# write_file
# ---------------------------------------------------------------------------


def test_write_file_creates_new_file(tmp_path) -> None:
    target = tmp_path / "new.py"
    svc = _svc(tmp_path)

    result = svc.write_file(
        [WriteSpec(file_path=str(target), content="x = 1\n", overwrite=False)],
    )

    assert result.success
    assert result.status == "committed"
    assert target.read_text(encoding="utf-8") == "x = 1\n"


def test_undo_create_removes_created_file(tmp_path) -> None:
    target = tmp_path / "new.py"
    svc = _svc(tmp_path)

    result = svc.write_file(
        [WriteSpec(file_path=str(target), content="x = 1\n", overwrite=False)],
    )
    undo = svc.undo_last_edit(str(target))

    assert result.success
    assert undo.success
    assert not target.exists()


def test_write_file_refuses_to_clobber_when_overwrite_false(tmp_path) -> None:
    target = tmp_path / "exists.py"
    target.write_text("old\n", encoding="utf-8")
    svc = _svc(tmp_path)

    result = svc.write_file(
        [WriteSpec(file_path=str(target), content="new\n", overwrite=False)],
    )

    assert not result.success
    assert result.status == "aborted_version"
    assert target.read_text(encoding="utf-8") == "old\n"


def test_write_file_overwrites_existing_with_strict_base(tmp_path) -> None:
    target = tmp_path / "update.py"
    target.write_text("old\n", encoding="utf-8")
    svc = _svc(tmp_path)

    result = svc.write_file(
        [WriteSpec(file_path=str(target), content="new\n", overwrite=True)],
    )

    assert result.success
    assert target.read_text(encoding="utf-8") == "new\n"


def test_write_file_accepts_bare_spec(tmp_path) -> None:
    """Ergonomic: a single WriteSpec may be passed without wrapping in a list."""
    target = tmp_path / "solo.py"
    svc = _svc(tmp_path)

    result = svc.write_file(
        WriteSpec(file_path=str(target), content="ok\n", overwrite=False),
    )

    assert result.success
    assert target.read_text(encoding="utf-8") == "ok\n"


def test_write_file_batch_is_atomic(tmp_path) -> None:
    """Existing file in a create-only batch aborts every slot."""
    new_path = tmp_path / "new.py"
    clobber = tmp_path / "clobber.py"
    clobber.write_text("original\n", encoding="utf-8")
    svc = _svc(tmp_path)

    result = svc.write_file(
        [
            WriteSpec(file_path=str(new_path), content="x\n", overwrite=False),
            WriteSpec(file_path=str(clobber), content="y\n", overwrite=False),
        ],
    )

    assert not result.success
    assert result.status == "aborted_version"
    assert not new_path.exists(), "first slot must not land when second aborts"
    assert clobber.read_text(encoding="utf-8") == "original\n"


# ---------------------------------------------------------------------------
# edit_file
# ---------------------------------------------------------------------------


def test_edit_file_applies_search_replace(tmp_path) -> None:
    target = tmp_path / "config.py"
    target.write_text("debug = False\n", encoding="utf-8")
    svc = _svc(tmp_path)

    result = svc.edit_file(
        [
            EditSpec(
                file_path=str(target),
                edits=[SearchReplaceEdit(old_text="False", new_text="True")],
            ),
        ],
    )

    assert result.success
    assert target.read_text(encoding="utf-8") == "debug = True\n"


def test_apply_edit_resolves_relative_path_under_workspace_root(tmp_path) -> None:
    target = tmp_path / "config.py"
    target.write_text("debug = False\n", encoding="utf-8")
    svc = _svc(tmp_path)

    result = svc.apply_edit(
        EditRequest(
            file_path="config.py",
            old_text="False",
            new_text="True",
        )
    )

    assert result.success
    assert target.read_text(encoding="utf-8") == "debug = True\n"


def test_edit_file_not_found_is_surfaced(tmp_path) -> None:
    svc = _svc(tmp_path)

    result = svc.edit_file(
        [
            EditSpec(
                file_path=str(tmp_path / "missing.py"),
                edits=[SearchReplaceEdit(old_text="a", new_text="b")],
            ),
        ],
    )

    assert not result.success
    assert result.status == "failed"
    assert result.conflict_reason == "not_found"


def test_edit_file_missing_search_text_aborts_before_commit(tmp_path) -> None:
    target = tmp_path / "c.py"
    target.write_text("alpha\n", encoding="utf-8")
    svc = _svc(tmp_path)

    result = svc.edit_file(
        [
            EditSpec(
                file_path=str(target),
                edits=[SearchReplaceEdit(old_text="beta", new_text="gamma")],
            ),
        ],
    )

    assert not result.success
    assert result.conflict_reason == "patch_failed"
    assert target.read_text(encoding="utf-8") == "alpha\n"


def test_edit_file_batch_is_atomic_across_specs(tmp_path) -> None:
    """One spec with unfindable text aborts; the other file stays untouched."""
    good = tmp_path / "good.py"
    bad = tmp_path / "bad.py"
    good.write_text("apple\n", encoding="utf-8")
    bad.write_text("banana\n", encoding="utf-8")
    svc = _svc(tmp_path)

    result = svc.edit_file(
        [
            EditSpec(
                file_path=str(good),
                edits=[SearchReplaceEdit(old_text="apple", new_text="apricot")],
            ),
            EditSpec(
                file_path=str(bad),
                edits=[SearchReplaceEdit(old_text="cherry", new_text="blueberry")],
            ),
        ],
    )

    assert not result.success
    assert good.read_text(encoding="utf-8") == "apple\n"
    assert bad.read_text(encoding="utf-8") == "banana\n"


# ---------------------------------------------------------------------------
# delete_file
# ---------------------------------------------------------------------------


def test_delete_file_single_item_list(tmp_path) -> None:
    """Single-item list is the canonical shape for one-file delete."""
    target = tmp_path / "gone.py"
    target.write_text("goodbye\n", encoding="utf-8")
    svc = _svc(tmp_path)

    result = svc.delete_file([str(target)])

    assert result.success
    assert not target.exists()


def test_delete_file_batch_is_atomic(tmp_path) -> None:
    a = tmp_path / "a.py"
    b = tmp_path / "b.py"
    a.write_text("1", encoding="utf-8")
    b.write_text("2", encoding="utf-8")
    svc = _svc(tmp_path)

    result = svc.delete_file([str(a), str(b)])

    assert result.success
    assert not a.exists()
    assert not b.exists()


def test_delete_file_batch_aborts_on_missing_sibling(tmp_path) -> None:
    """A missing path in a batch doesn't half-delete the surviving paths."""
    present = tmp_path / "present.py"
    present.write_text("1", encoding="utf-8")
    svc = _svc(tmp_path)

    result = svc.delete_file([str(present), str(tmp_path / "missing.py")])

    assert not result.success
    assert result.conflict_reason == "not_found"
    assert present.exists(), "surviving path must not be deleted"


def test_delete_file_folder_spec_expands_members_in_service(tmp_path) -> None:
    pkg = tmp_path / "pkg"
    nested = pkg / "sub"
    nested.mkdir(parents=True)
    a = pkg / "a.py"
    b = nested / "b.py"
    a.write_text("1", encoding="utf-8")
    b.write_text("2", encoding="utf-8")
    svc = _svc(tmp_path)

    result = svc.delete_file([DeleteSpec(path=str(pkg), is_folder=True)])

    assert result.success
    assert not a.exists()
    assert not b.exists()


def test_commit_specs_many_delete_folder_spec_expands_members(tmp_path) -> None:
    pkg = tmp_path / "pkg"
    pkg.mkdir()
    target = pkg / "a.py"
    target.write_text("1", encoding="utf-8")
    svc = _svc(tmp_path)

    results = svc.commit_specs_many(
        [{"op": "delete", "specs": [DeleteSpec(path=str(pkg), is_folder=True)]}],
    )

    assert len(results) == 1
    assert results[0].success
    assert not target.exists()


def test_delete_file_folder_spec_rejects_regular_file(tmp_path) -> None:
    target = tmp_path / "not_a_dir.py"
    target.write_text("x", encoding="utf-8")
    svc = _svc(tmp_path)

    result = svc.delete_file([DeleteSpec(path=str(target), is_folder=True)])

    assert not result.success
    assert result.conflict_reason == "not_a_directory"
    assert target.exists()


# ---------------------------------------------------------------------------
# move_file
# ---------------------------------------------------------------------------


def test_move_file_single_item_list(tmp_path) -> None:
    """Single-item list is the canonical shape for one-file move."""
    src = tmp_path / "src.py"
    dst = tmp_path / "dst.py"
    src.write_text("content\n", encoding="utf-8")
    svc = _svc(tmp_path)

    result = svc.move_file([MoveSpec(src_path=str(src), dst_path=str(dst))])

    assert result.success
    assert not src.exists()
    assert dst.read_text(encoding="utf-8") == "content\n"


def test_move_file_batch_accepts_movespec_list(tmp_path) -> None:
    src_a = tmp_path / "a.py"
    src_b = tmp_path / "b.py"
    dst_a = tmp_path / "moved_a.py"
    dst_b = tmp_path / "moved_b.py"
    src_a.write_text("A", encoding="utf-8")
    src_b.write_text("B", encoding="utf-8")
    svc = _svc(tmp_path)

    result = svc.move_file(
        [
            MoveSpec(src_path=str(src_a), dst_path=str(dst_a)),
            MoveSpec(src_path=str(src_b), dst_path=str(dst_b)),
        ],
    )

    assert result.success
    assert not src_a.exists() and not src_b.exists()
    assert dst_a.read_text(encoding="utf-8") == "A"
    assert dst_b.read_text(encoding="utf-8") == "B"


def test_move_file_overwrite_uses_strict_base(tmp_path) -> None:
    src = tmp_path / "src.py"
    dst = tmp_path / "dst.py"
    src.write_text("NEW\n", encoding="utf-8")
    dst.write_text("OLD\n", encoding="utf-8")
    svc = _svc(tmp_path)

    result = svc.move_file(
        [MoveSpec(src_path=str(src), dst_path=str(dst), overwrite=True)],
    )

    assert result.success
    assert dst.read_text(encoding="utf-8") == "NEW\n"


def test_move_file_batch_rejects_identical_paths(tmp_path) -> None:
    src = tmp_path / "self.py"
    src.write_text("x", encoding="utf-8")
    svc = _svc(tmp_path)

    result = svc.move_file([MoveSpec(src_path=str(src), dst_path=str(src))])

    assert not result.success
    assert result.conflict_reason == "identical_paths"
    assert src.exists()


def test_move_file_folder_spec_expands_members_in_service(tmp_path) -> None:
    src = tmp_path / "src_pkg"
    nested = src / "sub"
    nested.mkdir(parents=True)
    a = src / "a.py"
    b = nested / "b.py"
    a.write_text("A", encoding="utf-8")
    b.write_text("B", encoding="utf-8")
    dst = tmp_path / "dst_pkg"
    svc = _svc(tmp_path)

    result = svc.move_file(
        [MoveSpec(src_path=str(src), dst_path=str(dst), is_folder=True)],
    )

    assert result.success
    assert not a.exists()
    assert not b.exists()
    assert (dst / "a.py").read_text(encoding="utf-8") == "A"
    assert (dst / "sub" / "b.py").read_text(encoding="utf-8") == "B"


def test_commit_specs_many_move_folder_spec_expands_members(tmp_path) -> None:
    src = tmp_path / "src_pkg"
    src.mkdir()
    target = src / "a.py"
    target.write_text("A", encoding="utf-8")
    dst = tmp_path / "dst_pkg"
    svc = _svc(tmp_path)

    results = svc.commit_specs_many(
        [
            {
                "op": "move",
                "specs": [MoveSpec(src_path=str(src), dst_path=str(dst), is_folder=True)],
            }
        ],
    )

    assert len(results) == 1
    assert results[0].success
    assert not target.exists()
    assert (dst / "a.py").read_text(encoding="utf-8") == "A"


def test_move_file_folder_spec_rejects_regular_file(tmp_path) -> None:
    src = tmp_path / "src.py"
    dst = tmp_path / "dst.py"
    src.write_text("x", encoding="utf-8")
    svc = _svc(tmp_path)

    result = svc.move_file(
        [MoveSpec(src_path=str(src), dst_path=str(dst), is_folder=True)],
    )

    assert not result.success
    assert result.conflict_reason == "not_a_directory"
    assert src.exists()
    assert not dst.exists()


def test_commit_specs_many_batches_mixed_disjoint_ops(tmp_path) -> None:
    edit_target = tmp_path / "edit.py"
    delete_target = tmp_path / "delete.py"
    move_src = tmp_path / "move_src.py"
    move_dst = tmp_path / "move_dst.py"
    write_target = tmp_path / "write.py"
    edit_target.write_text("flag = False\n", encoding="utf-8")
    delete_target.write_text("remove me\n", encoding="utf-8")
    move_src.write_text("move me\n", encoding="utf-8")
    svc = _svc(tmp_path)

    results = svc.commit_specs_many(
        [
            {
                "op": "write",
                "specs": [
                    WriteSpec(file_path=str(write_target), content="created\n"),
                ],
                "agent_id": "writer",
            },
            {
                "op": "edit",
                "specs": [
                    EditSpec(
                        file_path=str(edit_target),
                        edits=[SearchReplaceEdit(old_text="False", new_text="True")],
                    ),
                ],
                "agent_id": "editor",
            },
            {
                "op": "delete",
                "specs": [str(delete_target)],
                "agent_id": "deleter",
            },
            {
                "op": "move",
                "specs": [MoveSpec(src_path=str(move_src), dst_path=str(move_dst))],
                "agent_id": "mover",
            },
        ]
    )

    assert [result.success for result in results] == [True, True, True, True]
    assert write_target.read_text(encoding="utf-8") == "created\n"
    assert edit_target.read_text(encoding="utf-8") == "flag = True\n"
    assert not delete_target.exists()
    assert not move_src.exists()
    assert move_dst.read_text(encoding="utf-8") == "move me\n"
