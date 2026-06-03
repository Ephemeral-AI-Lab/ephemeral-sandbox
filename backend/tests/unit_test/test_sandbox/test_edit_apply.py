"""Unit tests for the shared ``apply_search_replace`` helper."""

from __future__ import annotations

import pytest

from sandbox._shared.edit_apply import SearchReplaceError, apply_search_replace


def test_unique_match_replaced_once() -> None:
    assert (
        apply_search_replace("a foo b", "foo", "bar", replace_all=False) == "a bar b"
    )


def test_count_zero_raises_anchor_not_found() -> None:
    with pytest.raises(SearchReplaceError, match="anchor not found"):
        apply_search_replace("alpha", "missing", "x", replace_all=False)


def test_count_mismatch_raises_occurrence_count_mismatch() -> None:
    with pytest.raises(SearchReplaceError, match="anchor occurrence count mismatch"):
        apply_search_replace("a a a", "a", "b", replace_all=False)


def test_replace_all_replaces_every_occurrence() -> None:
    assert apply_search_replace("a a a", "a", "b", replace_all=True) == "b b b"


def test_replace_all_count_zero_still_raises_anchor_not_found() -> None:
    with pytest.raises(SearchReplaceError, match="anchor not found"):
        apply_search_replace("alpha", "missing", "x", replace_all=True)


def test_empty_old_raises() -> None:
    with pytest.raises(SearchReplaceError, match="must be non-empty"):
        apply_search_replace("alpha", "", "x", replace_all=False)


def test_error_message_attribute_is_preserved() -> None:
    try:
        apply_search_replace("alpha", "missing", "x", replace_all=False)
    except SearchReplaceError as exc:
        assert exc.message == "anchor not found"
    else:  # pragma: no cover - the call above must raise
        raise AssertionError("expected SearchReplaceError")
