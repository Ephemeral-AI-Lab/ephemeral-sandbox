"""Tests for sandbox shell prehooks."""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from tools._framework.core.base import ToolExecutionContextService
from tools._hooks.destructive_shell import (
    destructive_git_command_error,
    destructive_shell_command_error,
)
from tools.sandbox.shell import shell

from ._helpers import run_tool_safely


def _ctx(services=None) -> ToolExecutionContextService:
    return ToolExecutionContextService(cwd=Path("/tmp"), services=services or {})


@pytest.mark.parametrize(
    "command",
    [
        "git add app.py",
        "git commit -m fix",
        "git reset --hard",
        "git restore app.py",
        "git checkout -- app.py",
        "git clean -fd",
        "git clean -xdf",
        "git apply patch.diff",
        "git apply --cached patch.diff",
        "command git stash",
        "git -C /repo checkout feature",
        "echo ok; git rm old.py",
    ],
)
def test_destructive_git_guard_matches_deleted_prehook_rules(command: str) -> None:
    assert destructive_git_command_error(command) is not None


@pytest.mark.parametrize(
    "command",
    [
        "git status",
        "git diff",
        "git log --oneline",
        "git grep pattern",
        "git apply --check patch.diff",
        "git clean -nfd",
        "git clean --dry-run -fd",
    ],
)
def test_destructive_git_guard_allows_read_only_git_commands(command: str) -> None:
    assert destructive_git_command_error(command) is None


@pytest.mark.parametrize(
    "command",
    [
        "rm -rf /testbed/dask",
        "rm -rF /testbed",
        "rm --recursive /workspace/project",
        "mv /testbed/dask /tmp/trash",
        "mv /home/user /tmp",
        "chmod -R 777 /usr",
        "chown -R root:root /etc",
        "rm -rf .",
        "mkfs.ext4 /dev/sda1",
        "dd if=/dev/zero of=/dev/sda",
        "rm -rf /tmp/important",
        "echo ok; rm -rf /testbed/dask",
        "true && mv /workspace/project /nowhere",
    ],
)
def test_destructive_shell_guard_matches_deleted_prehook_regex(command: str) -> None:
    assert destructive_shell_command_error(command) is not None


@pytest.mark.parametrize(
    "command",
    [
        "rm /testbed/dask/file.py",
        "rm -f /testbed/dask/file.py",
        "mv /testbed/dask/file.py /testbed/dask/new.py",
        "cp -r /testbed/dask /testbed/backup",
        "chmod 644 /testbed/dask/file.py",
        "pytest /testbed/dask/tests",
        "python -c 'import os'",
    ],
)
def test_destructive_shell_guard_allows_deleted_prehook_safe_cases(command: str) -> None:
    assert destructive_shell_command_error(command) is None


@pytest.mark.asyncio
async def test_shell_prehook_blocks_git_mutation_before_ci_requirement() -> None:
    result = await run_tool_safely(
        shell,
        {"command": "git reset --hard"},
        context=_ctx(),
    )

    assert result.is_error
    payload = json.loads(result.output)
    assert payload["hookName"] == "sandbox_shell:destructive_git"
    assert payload["phase"] == "pre"
    assert "git mutation commands are forbidden" in result.metadata["hook_failure"]["reason"]


@pytest.mark.asyncio
async def test_shell_prehook_blocks_destructive_shell_before_ci_requirement() -> None:
    result = await run_tool_safely(
        shell,
        {"command": "rm -rf /testbed/dask"},
        context=_ctx(),
    )

    assert result.is_error
    payload = json.loads(result.output)
    assert payload["hookName"] == "sandbox_shell:destructive_shell"
    assert payload["phase"] == "pre"
    assert "destructive shell command" in result.metadata["hook_failure"]["reason"]


@pytest.mark.asyncio
async def test_shell_prehook_allows_safe_command_to_reach_sandbox_requirement() -> None:
    result = await run_tool_safely(
        shell,
        {"command": "git status"},
        context=_ctx(),
    )

    assert result.is_error
    assert result.metadata.get("sandbox_required") is True
    assert "Sandbox id is unavailable" in result.output
