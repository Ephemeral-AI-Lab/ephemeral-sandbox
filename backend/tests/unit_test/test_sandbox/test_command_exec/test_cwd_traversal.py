"""cwd traversal hardening tests for command-exec workspace replacement.

Covers the BL-01 fix in two layers:
1. ``CommandExecRequest`` rejects ``..`` segments at the request boundary.
2. ``resolve_workspace_cwd`` verifies the resolved cwd stays inside the
   mounted workspace root, even for relative cwd inputs.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from sandbox._shared.shell_contract import CommandExecRequest
from sandbox.overlay.subprocess_runner import resolve_workspace_cwd


def test_relative_cwd_with_parent_escape_is_rejected(tmp_path: Path) -> None:
    mounted = tmp_path / "mounted"
    mounted.mkdir()

    with pytest.raises(ValueError, match="escapes workspace"):
        resolve_workspace_cwd(
            declared_workspace_root="/testbed",
            mounted_workspace_root=mounted,
            cwd="../../../etc",
        )


def test_relative_cwd_with_embedded_parent_escape_is_rejected(tmp_path: Path) -> None:
    mounted = tmp_path / "mounted"
    mounted.mkdir()

    with pytest.raises(ValueError, match="escapes workspace"):
        resolve_workspace_cwd(
            declared_workspace_root="/testbed",
            mounted_workspace_root=mounted,
            cwd="pkg/../../escape",
        )


def test_resolve_workspace_cwd_does_not_create_dirs_outside_mounted_root(
    tmp_path: Path,
) -> None:
    mounted = tmp_path / "mounted"
    mounted.mkdir()
    sibling = tmp_path / "sibling-should-not-exist"

    with pytest.raises(ValueError, match="escapes workspace"):
        resolve_workspace_cwd(
            declared_workspace_root="/testbed",
            mounted_workspace_root=mounted,
            cwd="../sibling-should-not-exist",
        )
    assert not sibling.exists()


def test_command_exec_request_rejects_dotdot_cwd() -> None:
    with pytest.raises(ValueError, match="cwd"):
        CommandExecRequest(
            invocation_id="req-1",
            workspace_ref="ref",
            workspace_root="/testbed",
            command=("true",),
            cwd="../../../etc",
        )


def test_command_exec_request_rejects_embedded_dotdot_cwd() -> None:
    with pytest.raises(ValueError, match="cwd"):
        CommandExecRequest(
            invocation_id="req-1",
            workspace_ref="ref",
            workspace_root="/testbed",
            command=("true",),
            cwd="pkg/../../escape",
        )


def test_command_exec_request_accepts_clean_relative_cwd() -> None:
    request = CommandExecRequest(
        invocation_id="req-1",
        workspace_ref="ref",
        workspace_root="/testbed",
        command=("true",),
        cwd="pkg/sub",
    )
    assert request.cwd == "pkg/sub"
