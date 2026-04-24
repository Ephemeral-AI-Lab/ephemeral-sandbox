"""Daytona tool exports."""

from __future__ import annotations

from tools.core.base import BaseTool

from tools.daytona_toolkit.tools import (
    daytona_glob,
    daytona_grep,
    daytona_read_file,
    daytona_write_file,
)
from tools.daytona_toolkit.edit_tool import daytona_edit_file
from tools.daytona_toolkit.shell_tool import daytona_shell
from tools.daytona_toolkit.delete_move_tool import (
    daytona_delete_file,
    daytona_move_file,
)

def make_daytona_tools(*, include_shell: bool = True) -> list[BaseTool]:
    """Return Daytona sandbox tools."""
    tools: list[BaseTool] = [
        daytona_grep,
        daytona_glob,
        daytona_read_file,
        daytona_write_file,
        daytona_edit_file,
        daytona_delete_file,
        daytona_move_file,
    ]
    if include_shell:
        tools.append(daytona_shell)
    return tools
