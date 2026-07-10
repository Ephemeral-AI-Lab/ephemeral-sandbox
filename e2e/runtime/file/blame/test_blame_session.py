"""Live e2e: File Blame — Session (4 cases)."""

import pytest

from runtime.file.helpers import (
    assert_blame_ranges,
    assert_blame_tiling,
    assert_content,
    assert_error,
    assert_ok,
    assert_single_owner,
    create_workspace_session,
    destroy_workspace_session,
    edit,
    exec_command,
    file_blame,
    file_edit,
    file_read,
    file_write,
    owners_by_line,
)


def test_live_session_changes_are_invisible_to_blame(sandbox):
    """Live-session changes are invisible to blame."""
    assert_ok(file_write(sandbox, "seed.txt", "one\ntwo\nthree"))
    before = assert_blame_tiling(sandbox, "seed.txt")
    owner_a = assert_single_owner(sandbox, "seed.txt", prefix="operation:")
    session_id = create_workspace_session(sandbox)
    try:
        assert_ok(
            file_write(
                sandbox,
                "s/draft.txt",
                "draft",
                workspace_session_id=session_id,
            )
        )
        assert_ok(
            file_edit(
                sandbox,
                "seed.txt",
                [edit("two", "TWO")],
                workspace_session_id=session_id,
            )
        )
        assert_content(
            file_read(sandbox, "seed.txt", workspace_session_id=session_id),
            "one\nTWO\nthree",
        )
        assert_error(
            file_blame(sandbox, "s/draft.txt"),
            "not_found",
            "no auditability record for path: s/draft.txt",
        )
        during = assert_blame_tiling(sandbox, "seed.txt")
        assert during == before
        assert_blame_ranges(sandbox, "seed.txt", [(1, 3, owner_a)], during)
    finally:
        destroy_workspace_session(sandbox, session_id, grace_s=1)

    assert_error(
        file_blame(sandbox, "s/draft.txt"),
        "not_found",
        "no auditability record for path: s/draft.txt",
    )
    assert_blame_ranges(sandbox, "seed.txt", [(1, 3, owner_a)])


def test_capture_insertion_shifts_without_reassigning(sandbox):
    """Capture insertion shifts without reassigning."""
    path = "blame/capture-insert.txt"
    assert_ok(file_write(sandbox, path, "\n".join(f"line-{i}" for i in range(1, 6))))
    owner_a = assert_single_owner(sandbox, path, prefix="operation:")

    result = exec_command(sandbox, f"sed -i '3i marker' {path}")
    assert result["status"] == "ok"
    assert result["exit_code"] == 0
    assert_content(
        file_read(sandbox, path),
        "line-1\nline-2\nmarker\nline-3\nline-4\nline-5",
    )
    blame = assert_blame_tiling(sandbox, path)
    exec_owner = owners_by_line(blame)[2]
    assert exec_owner.startswith("workspace_session:")
    assert_blame_ranges(
        sandbox,
        path,
        [(1, 2, owner_a), (3, 1, exec_owner), (4, 3, owner_a)],
        blame,
    )


def test_capture_deletion_mints_no_ownership(sandbox):
    """Capture deletion mints no ownership."""
    path = "blame/capture-delete.txt"
    assert_ok(file_write(sandbox, path, "line-1\nline-2\nline-3\nline-4"))
    owner_a = assert_single_owner(sandbox, path, prefix="operation:")
    assert_ok(file_edit(sandbox, path, [edit("line-4", "LINE-4")]))
    owner_b = owners_by_line(assert_blame_tiling(sandbox, path))[3]
    assert owner_b.startswith("operation:")
    assert owner_b != owner_a

    result = exec_command(sandbox, f"sed -i '2d' {path}")
    assert result["status"] == "ok"
    assert result["exit_code"] == 0
    assert_content(file_read(sandbox, path), "line-1\nline-3\nLINE-4")
    blame = assert_blame_tiling(sandbox, path)
    assert all(not owner.startswith("workspace_session:") for owner in owners_by_line(blame))
    assert_blame_ranges(
        sandbox,
        path,
        [(1, 2, owner_a), (3, 1, owner_b)],
        blame,
    )


@pytest.mark.slow
def test_complex_deep_prepend_history_across_20_captures(sandbox):
    """[complex] Deep prepend history across 20 captures."""
    path = "blame/deep-prepend.txt"
    assert_ok(file_write(sandbox, path, "\n".join(f"seed-{i}" for i in range(1, 6))))
    seed_owner = assert_single_owner(sandbox, path, prefix="operation:")
    exec_owners = []

    for index in range(1, 21):
        result = exec_command(sandbox, f"sed -i '1i gen-{index}' {path}")
        assert result["status"] == "ok"
        assert result["exit_code"] == 0
        owner = owners_by_line(assert_blame_tiling(sandbox, path))[0]
        assert owner.startswith("workspace_session:")
        assert owner not in exec_owners
        exec_owners.append(owner)
        if index in {10, 20}:
            expected = list(reversed(exec_owners)) + [seed_owner] * 5
            blame = assert_blame_tiling(sandbox, path)
            assert owners_by_line(blame) == expected
            expected_ranges = [
                (line, 1, owner) for line, owner in enumerate(reversed(exec_owners), 1)
            ]
            expected_ranges.append((index + 1, 5, seed_owner))
            assert_blame_ranges(sandbox, path, expected_ranges, blame)
