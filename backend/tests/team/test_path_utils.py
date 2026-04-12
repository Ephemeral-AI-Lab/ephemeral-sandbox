"""Unit tests for team._path_utils."""

from __future__ import annotations

import pytest

from team._path_utils import (
    normalize_path_list,
    path_to_ltree,
    paths_overlap,
)


# ---------------------------------------------------------------------------
# path_to_ltree
# ---------------------------------------------------------------------------


def test_path_to_ltree_directory_trailing_slash():
    assert path_to_ltree("src/auth/") == "src.auth"


def test_path_to_ltree_file_with_extension():
    assert path_to_ltree("src/auth/session.py") == "src.auth.sessionX2epy"


def test_path_to_ltree_hyphenated_directory():
    assert path_to_ltree("src/my-module/foo.py") == "src.myX2dmodule.fooX2epy"


def test_path_to_ltree_single_segment():
    assert path_to_ltree("src") == "src"


def test_path_to_ltree_leading_slash_stripped():
    assert path_to_ltree("/src/auth/") == "src.auth"


def test_path_to_ltree_empty_string():
    import pytest
    with pytest.raises(ValueError):
        path_to_ltree("")


def test_path_to_ltree_dot_in_filename():
    # dots get escaped as X2e
    result = path_to_ltree("src/foo.bar")
    assert "X2e" in result


def test_path_to_ltree_nested_deep():
    result = path_to_ltree("a/b/c/d")
    assert result == "a.b.c.d"


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


