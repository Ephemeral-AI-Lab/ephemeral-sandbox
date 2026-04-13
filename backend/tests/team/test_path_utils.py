"""Unit tests for team._path_utils."""

from __future__ import annotations

from team._path_utils import (
    normalize_path_list,
    paths_overlap,
)


# ---------------------------------------------------------------------------
# normalize_path_list
# ---------------------------------------------------------------------------


def test_normalize_path_list_string_input():
    result = normalize_path_list("src/auth/")
    assert result == ["src/auth/"]


def test_normalize_path_list_list_of_strings():
    result = normalize_path_list(["src/auth/", "src/billing/"])
    assert result == ["src/auth/", "src/billing/"]


def test_normalize_path_list_strips_whitespace():
    result = normalize_path_list(["  src/auth/  ", "  src/billing/  "])
    assert result == ["src/auth/", "src/billing/"]


def test_normalize_path_list_skips_empty_strings():
    result = normalize_path_list(["src/auth/", "", "  "])
    assert result == ["src/auth/"]


def test_normalize_path_list_empty_list():
    result = normalize_path_list([])
    assert result == []


def test_normalize_path_list_none_returns_empty():
    result = normalize_path_list(None)
    assert result == []


def test_normalize_path_list_integer_returns_empty():
    result = normalize_path_list(42)
    assert result == []


def test_normalize_path_list_dict_returns_empty():
    result = normalize_path_list({"a": "b"})
    assert result == []


# ---------------------------------------------------------------------------
# paths_overlap
# ---------------------------------------------------------------------------


def test_paths_overlap_exact_match():
    assert paths_overlap("src/auth", "src/auth") is True


def test_paths_overlap_parent_contains_child():
    assert paths_overlap("src/auth", "src/auth/session.py") is True


def test_paths_overlap_child_contained_by_parent():
    assert paths_overlap("src/auth/session.py", "src/auth") is True


def test_paths_overlap_distinct_paths_no_overlap():
    assert paths_overlap("src/auth", "src/billing") is False


def test_paths_overlap_none_left():
    assert paths_overlap(None, "src/auth") is False


def test_paths_overlap_none_right():
    assert paths_overlap("src/auth", None) is False


def test_paths_overlap_both_none():
    assert paths_overlap(None, None) is False


def test_paths_overlap_sibling_directories_no_overlap():
    assert paths_overlap("src/auth/login", "src/auth/logout") is False
