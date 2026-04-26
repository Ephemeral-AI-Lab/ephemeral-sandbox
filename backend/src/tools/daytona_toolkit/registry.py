"""Sandbox tool registry."""

from __future__ import annotations

from tools.core.base import BaseTool

from tools.daytona_toolkit.delete_file import delete_file
from tools.daytona_toolkit.edit_file import edit_file
from tools.daytona_toolkit.glob import glob
from tools.daytona_toolkit.grep import grep
from tools.daytona_toolkit.move_file import move_file
from tools.daytona_toolkit.read_file import read_file
from tools.daytona_toolkit.shell import shell
from tools.daytona_toolkit.write_file import write_file


def make_daytona_tools(*, include_shell: bool = True) -> list[BaseTool]:
    """Return sandbox tools."""
    tools: list[BaseTool] = [
        grep,
        glob,
        read_file,
        write_file,
        edit_file,
        delete_file,
        move_file,
    ]
    if include_shell:
        tools.append(shell)
    return tools
