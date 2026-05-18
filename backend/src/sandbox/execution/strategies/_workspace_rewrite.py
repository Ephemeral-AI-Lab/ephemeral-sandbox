"""Argv/env path rewriting for the copy-backed strategy.

When a command lands on the copy-backed merged tree we cannot replace the
declared workspace path at the kernel mount layer, so any argv element or
env value that literally names the declared workspace must be rewritten to
point at the temporary merged tree.
"""

from __future__ import annotations

from collections.abc import Mapping
from typing import AbstractSet

from sandbox.execution.env_policy import DEFAULT_COMMAND_EXEC_POLICY


def rewrite_declared_workspace_refs(
    command: tuple[str, ...],
    workspace_root: str,
    mounted_workspace_root: str,
) -> tuple[str, ...]:
    """Map path-like workspace references to the copy-backed mounted tree."""
    root = str(workspace_root).rstrip("/") or "/"
    if root == "/":
        return command
    return tuple(
        _rewrite_workspace_paths(
            part,
            workspace_root=root,
            mounted_workspace_root=str(mounted_workspace_root),
        )
        for part in command
    )


def rewrite_declared_workspace_env(
    env: Mapping[str, str],
    *,
    workspace_root: str,
    mounted_workspace_root: str,
    workspace_env_keys: AbstractSet[str] = DEFAULT_COMMAND_EXEC_POLICY.workspace_env_keys,
) -> dict[str, str]:
    """Rewrite env values that explicitly name the assigned workspace."""
    root = str(workspace_root).rstrip("/") or "/"
    rewritten: dict[str, str] = {}
    for key, value in env.items():
        env_key = str(key)
        env_value = str(value)
        if env_key in workspace_env_keys:
            env_value = _rewrite_path_token(
                env_value,
                workspace_root=root,
                mounted_workspace_root=mounted_workspace_root,
            )
        rewritten[env_key] = env_value
    return rewritten


def _rewrite_workspace_paths(
    value: str,
    *,
    workspace_root: str,
    mounted_workspace_root: str,
) -> str:
    result: list[str] = []
    index = 0
    while index < len(value):
        if _path_starts_at(value, index, workspace_root):
            result.append(mounted_workspace_root)
            index += len(workspace_root)
            continue
        result.append(value[index])
        index += 1
    return "".join(result)


def _rewrite_path_token(
    value: str,
    *,
    workspace_root: str,
    mounted_workspace_root: str,
) -> str:
    if value == workspace_root:
        return mounted_workspace_root
    if value.startswith(workspace_root + "/"):
        return mounted_workspace_root + value[len(workspace_root):]
    return value


def _path_starts_at(value: str, index: int, workspace_root: str) -> bool:
    if not value.startswith(workspace_root, index):
        return False
    before = value[index - 1] if index > 0 else ""
    after_index = index + len(workspace_root)
    after = value[after_index] if after_index < len(value) else ""
    if before and before not in " \t\n\r=:;,&|>(\"'":
        return False
    return not after or after in "/ \t\n\r:;,&|)<\"'"


__all__ = [
    "rewrite_declared_workspace_env",
    "rewrite_declared_workspace_refs",
]
