"""Tests for the OCC changeset builders."""

from __future__ import annotations

from sandbox.occ.changeset import (
    Change,
    ChangeSource,
    build_api_write_change,
    build_overlay_delete_change,
    build_overlay_write_change,
)
from sandbox.occ.changeset import WriteChange


def test_api_write_builder_tags_api_source_and_bytes_payload() -> None:
    change = build_api_write_change(
        path="src/a.py",
        final_content="hello",
        base_hash="abc",
    )

    assert isinstance(change, WriteChange)
    assert change.source == "api_write"
    assert change.final_content == b"hello"
    assert change.base_hash == "abc"


def test_overlay_builders_defer_base_hash_to_preparation() -> None:
    write = build_overlay_write_change(path="src/a.py", final_content=b"new")
    delete = build_overlay_delete_change(path="src/gone.py")

    assert write.source == "overlay_capture"
    assert write.base_hash is None
    assert write.final_content == b"new"
    assert delete.source == "overlay_capture"
    assert delete.base_hash is None


def test_change_source_normalizes_to_closed_string_enum() -> None:
    change = Change(path="src/a.py", source="overlay_capture")

    assert change.source is ChangeSource.OVERLAY_CAPTURE
    assert str(change.source) == "overlay_capture"
    assert change.source == "overlay_capture"
