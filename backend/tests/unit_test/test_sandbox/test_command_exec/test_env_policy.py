"""cwd policy tests for command-exec workspace replacement."""

from __future__ import annotations

from pathlib import Path

import pytest

from sandbox.execution.env_policy import (
    DEFAULT_COMMAND_EXEC_POLICY,
    CommandExecPolicy,
)
from sandbox.execution.workspace_environment import resolve_workspace_cwd


def test_relative_cwd_resolves_inside_mounted_workspace(tmp_path: Path) -> None:
    mounted = tmp_path / "mounted"

    resolved = resolve_workspace_cwd(
        declared_workspace_root="/testbed",
        mounted_workspace_root=mounted,
        cwd="pkg",
    )

    assert resolved == mounted / "pkg"
    assert resolved.is_dir()


def test_absolute_workspace_cwd_is_remapped_to_mount(tmp_path: Path) -> None:
    mounted = tmp_path / "mounted"

    resolved = resolve_workspace_cwd(
        declared_workspace_root="/testbed",
        mounted_workspace_root=mounted,
        cwd="/testbed/pkg",
    )

    assert resolved == mounted / "pkg"
    assert resolved.is_dir()


def test_absolute_cwd_outside_workspace_is_rejected(tmp_path: Path) -> None:
    with pytest.raises(ValueError, match="escapes workspace"):
        resolve_workspace_cwd(
            declared_workspace_root="/testbed",
            mounted_workspace_root=tmp_path / "mounted",
            cwd="/tmp",
        )


def test_default_policy_filters_loader_env() -> None:
    env = DEFAULT_COMMAND_EXEC_POLICY.command_environment(
        {"PATH": "/tmp/unsafe", "SAFE_FLAG": "1"}
    )

    assert env["SAFE_FLAG"] == "1"
    assert env["PATH"] != "/tmp/unsafe"
    assert env["GIT_OPTIONAL_LOCKS"] == "0"


def test_command_exec_policy_can_be_tightened_for_tests() -> None:
    policy = CommandExecPolicy(
        restricted_env_keys=frozenset({"SECRET"}),
        workspace_env_keys=frozenset({"WORKSPACE_DIR"}),
        forbidden_overlay_path_chars=("@",),
        command_env_defaults={"LOCKS": "off"},
    )

    env = policy.command_environment({"SECRET": "drop", "VISIBLE": "keep"})

    assert "SECRET" not in env
    assert env["VISIBLE"] == "keep"
    assert env["LOCKS"] == "off"
    with pytest.raises(ValueError, match="overlay mount path cannot contain"):
        policy.validate_overlay_path_text("/tmp/bad@path")
