"""Sandbox tool registry."""

from __future__ import annotations

from tools._framework.core.base import BaseTool
from tools.sandbox._lib.context import SANDBOX_CONTEXT

from tools.sandbox.edit_file import edit_file
from tools.sandbox.exec_command import exec_command
from tools.sandbox.write_pty_command_stdin import write_pty_command_stdin
from tools.sandbox.check_pty_command_progress import check_pty_command_progress
from tools.sandbox.cancel_pty_command import cancel_pty_command
from tools.sandbox.multi_edit import multi_edit
from tools.sandbox.glob import glob
from tools.sandbox.grep import grep
from tools.sandbox.read_file import read_file
from tools.sandbox.shell import shell
from tools.sandbox.write_file import write_file
from tools.isolated_workspace import enter_isolated_workspace, exit_isolated_workspace


def make_sandbox_tools() -> list[BaseTool]:
    """Return sandbox tools."""
    tools: list[BaseTool] = [
        read_file,
        write_file,
        edit_file,
        multi_edit,
        exec_command,
        write_pty_command_stdin,
        check_pty_command_progress,
        cancel_pty_command,
        shell,
        glob,
        grep,
        enter_isolated_workspace,
        exit_isolated_workspace,
    ]
    for tool in tools:
        tool.context_requirements = (SANDBOX_CONTEXT,)
    return tools


__all__ = ["make_sandbox_tools"]
