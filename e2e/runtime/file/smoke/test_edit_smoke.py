"""Live e2e: Edit Smoke (5 cases)."""

from runtime.file.helpers import (
    assert_blame_owners,
    assert_content,
    assert_error,
    assert_ok,
    assert_single_owner,
    edit,
    file_edit,
    file_read,
    file_write,
    owner_for_line,
    workspace_session,
)


def test_sessionless_edit_unique_replacement_and_read_sees_result(sandbox):
    """Sessionless edit performs one unique replacement and sessionless read sees
    the result."""
    assert_ok(file_write(sandbox, "edit-smoke/unique.txt", "alpha\nbeta"))
    seed_owner = owner_for_line(sandbox, "edit-smoke/unique.txt", 1)
    result = file_edit(
        sandbox, "edit-smoke/unique.txt", [edit("beta", "BETA")]
    )
    assert result["type"] == "edit"
    assert result["replacements"] == 1
    assert_content(file_read(sandbox, "edit-smoke/unique.txt"), "alpha\nBETA")
    edit_owner = owner_for_line(sandbox, "edit-smoke/unique.txt", 2)
    assert edit_owner.startswith("operation:")
    assert_blame_owners(sandbox, "edit-smoke/unique.txt", [seed_owner, edit_owner])


def test_sessionless_edit_replace_all_true_replaces_multiple_occurrences(sandbox):
    """Sessionless edit with `replace_all=true` replaces multiple occurrences."""
    assert_ok(file_write(sandbox, "edit-smoke/all.txt", "a b a"))
    result = file_edit(
        sandbox,
        "edit-smoke/all.txt",
        [edit("a", "A", replace_all=True)],
    )
    assert result["replacements"] == 2
    assert_content(file_read(sandbox, "edit-smoke/all.txt"), "A b A")
    assert_single_owner(sandbox, "edit-smoke/all.txt", prefix="operation:")


def test_sessionless_edit_missing_old_string_returns_edit_not_found(sandbox):
    """Sessionless edit with missing `old_string` returns edit-not-found."""
    assert_ok(file_write(sandbox, "edit-smoke/missing-old.txt", "alpha"))
    assert_error(
        file_edit(sandbox, "edit-smoke/missing-old.txt", [edit("beta", "BETA")]),
        "invalid_request",
    )


def test_session_edit_visible_with_workspace_session_id_and_invisible_sessionless(
    sandbox, workspace_session
):
    """Session edit is visible with `workspace_session_id` and invisible to
    sessionless read before capture."""
    assert_ok(
        file_write(
            sandbox,
            "edit-smoke/session.txt",
            "draft alpha",
            workspace_session_id=workspace_session,
        )
    )
    result = file_edit(
        sandbox,
        "edit-smoke/session.txt",
        [edit("alpha", "beta")],
        workspace_session_id=workspace_session,
    )
    assert result["replacements"] == 1
    assert_content(
        file_read(sandbox, "edit-smoke/session.txt", workspace_session_id=workspace_session),
        "draft beta",
    )
    assert_error(file_read(sandbox, "edit-smoke/session.txt"), "not_found")


def test_ordered_multi_edit_applies_against_evolving_content(sandbox):
    """Ordered multi-edit applies against evolving content."""
    assert_ok(file_write(sandbox, "edit-smoke/ordered.txt", "one"))
    result = file_edit(
        sandbox,
        "edit-smoke/ordered.txt",
        [edit("one", "two"), edit("two", "three")],
    )
    assert result["edits_applied"] == 2
    assert result["replacements"] == 2
    assert_content(file_read(sandbox, "edit-smoke/ordered.txt"), "three")
    assert_single_owner(sandbox, "edit-smoke/ordered.txt", prefix="operation:")
