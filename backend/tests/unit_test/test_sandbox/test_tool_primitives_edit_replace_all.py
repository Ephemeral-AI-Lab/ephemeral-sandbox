"""Site B (tool_primitives) ``replace_all`` behavior and Site A/Site B parity."""

from __future__ import annotations

from pathlib import Path

import pytest

from sandbox.occ.changeset import EditChange, FileResult
from sandbox.occ.path_staging import _apply_edit_content
from sandbox._shared.tool_primitives.edit import edit_file


def test_edit_primitive_replace_all_replaces_every_occurrence(tmp_path: Path) -> None:
    target = tmp_path / "probe.txt"
    target.write_text("a a a\n", encoding="utf-8")

    result = edit_file(
        {
            "path": str(target),
            "edits": [{"old_text": "a", "new_text": "b", "replace_all": True}],
        }
    )

    assert result.success is True
    assert result.applied_edits == 1
    assert target.read_text(encoding="utf-8") == "b b b\n"


def test_edit_primitive_replace_all_anchor_absent_raises(tmp_path: Path) -> None:
    target = tmp_path / "probe.txt"
    target.write_text("alpha\n", encoding="utf-8")

    with pytest.raises(ValueError, match="anchor not found"):
        edit_file(
            {
                "path": str(target),
                "edits": [{"old_text": "missing", "new_text": "x", "replace_all": True}],
            }
        )

    assert target.read_text(encoding="utf-8") == "alpha\n"


@pytest.mark.parametrize(
    ("text", "old", "new", "replace_all"),
    [
        ("a foo b", "foo", "bar", False),
        ("a a a", "a", "b", True),
        ("x x x x", "x", "y", True),
        ("only-one", "only-one", "two", False),
    ],
)
def test_site_a_site_b_parity_single_edit(
    tmp_path: Path, text: str, old: str, new: str, replace_all: bool
) -> None:
    """Same (text, old, new, replace_all) yields identical content at both sites."""
    # Site A (OCC).
    site_a = _apply_edit_content(
        "f.txt",
        text.encode("utf-8"),
        EditChange(path="f.txt", old_text=old, new_text=new, replace_all=replace_all),
    )
    assert not isinstance(site_a, FileResult)

    # Site B (tool_primitives).
    target = tmp_path / "probe.txt"
    target.write_text(text, encoding="utf-8")
    edit_file(
        {
            "path": str(target),
            "edits": [{"old_text": old, "new_text": new, "replace_all": replace_all}],
        }
    )

    assert site_a.decode("utf-8") == target.read_text(encoding="utf-8")


def test_site_a_site_b_parity_multi_edit_evolving_content(tmp_path: Path) -> None:
    """A 2-edit evolving sequence (edit 2 anchors on edit 1's output) produces
    identical final content at both sites."""
    text = "alpha alpha\nbeta\n"
    edits = [
        {"old_text": "alpha", "new_text": "ALPHA", "replace_all": True},
        {"old_text": "ALPHA ALPHA", "new_text": "merged", "replace_all": False},
    ]

    # Site A (OCC): apply sequentially against evolving content.
    current = text.encode("utf-8")
    for spec in edits:
        current = _apply_edit_content(
            "f.txt",
            current,
            EditChange(
                path="f.txt",
                old_text=spec["old_text"],
                new_text=spec["new_text"],
                replace_all=spec["replace_all"],
            ),
        )
        assert not isinstance(current, FileResult)
    site_a_final = current.decode("utf-8")

    # Site B (tool_primitives): the primitive loops over the edits internally.
    target = tmp_path / "probe.txt"
    target.write_text(text, encoding="utf-8")
    edit_file({"path": str(target), "edits": edits})

    assert site_a_final == target.read_text(encoding="utf-8") == "merged\nbeta\n"
