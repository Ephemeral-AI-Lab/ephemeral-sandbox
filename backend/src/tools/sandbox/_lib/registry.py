"""Sandbox tool registry."""

from __future__ import annotations

from tools._framework.core.base import BaseTool
from tools.sandbox._lib.context import SANDBOX_CONTEXT

from tools.sandbox.edit_file import edit_file
from tools.sandbox.glob import glob
from tools.sandbox.grep import grep
from tools.sandbox.read_file import read_file
from tools.sandbox.shell import shell
from tools.sandbox.write_file import write_file


def make_sandbox_tools() -> list[BaseTool]:
    """Return sandbox tools."""
    tools: list[BaseTool] = [
        read_file,
        write_file,
        edit_file,
        shell,
        glob,
        grep,
    ]
    for tool in tools:
        tool.context_requirements = (SANDBOX_CONTEXT,)
    return tools


__all__ = ["make_sandbox_tools"]
