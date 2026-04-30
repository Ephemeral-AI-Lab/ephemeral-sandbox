"""Tests for workspace path normalization helpers."""

from __future__ import annotations

from sandbox.code_intelligence.core.path_utils import relativize_workspace_path


def test_relativize_workspace_path_preserves_dot_prefixed_segments() -> None:
    assert relativize_workspace_path("./.config/settings.py") == ".config/settings.py"


def test_relativize_workspace_path_preserves_parent_relative_prefix() -> None:
    assert relativize_workspace_path("../src/app.py") == "../src/app.py"
