"""Tests for sandbox shell routing policy."""

from __future__ import annotations

import pytest

from sandbox.api.utils.shell_routing import is_read_only_pipeline


@pytest.mark.parametrize(
    "command",
    [
        "cat pyproject.toml | grep pytest | wc -l",
        "git status | head -20",
        "git -C /workspace diff -- app.py | sed -n 1,20p",
        "find . -maxdepth 2 -type f | sort",
        "LC_ALL=C rg pytest backend/tests | head",
    ],
)
def test_read_only_pipeline_matches_explicit_allowlist(command: str) -> None:
    assert is_read_only_pipeline(command) is True


@pytest.mark.parametrize(
    "command",
    [
        "cat pyproject.toml | tee copied.txt",
        "git status && rm -rf /tmp/project",
        "git add app.py",
        "find . -delete",
        "sort pyproject.toml -o sorted.txt",
        'grep "$(rm -rf /tmp/project)" pyproject.toml | wc -l',
        "cat pyproject.toml > copy.txt",
    ],
)
def test_unsafe_or_mutating_pipeline_does_not_match(command: str) -> None:
    assert is_read_only_pipeline(command) is False
