"""Daytona pre-hook registration."""

from __future__ import annotations

from tools.core.hooks import ToolHookRegistry
from tools.daytona_toolkit.hooks.prehook import (
    shell_destructive_git,
    shell_destructive_shell,
    shell_file_edit_policy,
    shell_output_pipeline_policy,
    shell_package_mutation_policy,
    shell_stderr_suppression_policy,
    move_dst_scope_advisory,
    move_src_hard_block,
    move_src_scope_deny,
    repo_operation_guard,
    write_scope_advisory,
    write_scope_deny,
    write_scope_hard_block,
)

_MODULES = (
    repo_operation_guard,
    write_scope_hard_block,
    write_scope_advisory,
    write_scope_deny,
    move_src_hard_block,
    move_src_scope_deny,
    move_dst_scope_advisory,
    shell_destructive_git,
    shell_destructive_shell,
    shell_package_mutation_policy,
    shell_stderr_suppression_policy,
    shell_output_pipeline_policy,
    shell_file_edit_policy,
)


def register_all(registry: ToolHookRegistry | None = None) -> None:
    for module in _MODULES:
        module.register(registry)
