"""Daytona pre-hook registration."""

from __future__ import annotations

from tools.core.hooks import ToolHookRegistry
from tools.daytona_toolkit.hooks.prehook import (
    shell_destructive_git,
    shell_destructive_shell,
    shell_package_mutation_policy,
    shell_pytest_override_policy,
    shell_stderr_suppression_policy,
    repo_operation_guard,
)

_MODULES = (
    repo_operation_guard,
    shell_destructive_git,
    shell_destructive_shell,
    shell_package_mutation_policy,
    shell_pytest_override_policy,
    shell_stderr_suppression_policy,
)


def register_all(registry: ToolHookRegistry | None = None) -> None:
    for module in _MODULES:
        module.register(registry)
