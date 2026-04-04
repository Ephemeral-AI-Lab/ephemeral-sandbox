"""MCP toolkit — Model Context Protocol integration tools."""

from __future__ import annotations

from typing import Any

from ephemeralos.tools.base import BaseToolkit
from ephemeralos.tools.list_mcp_resources_tool import ListMcpResourcesTool
from ephemeralos.tools.mcp_auth_tool import McpAuthTool
from ephemeralos.tools.mcp_tool import McpToolAdapter
from ephemeralos.tools.read_mcp_resource_tool import ReadMcpResourceTool


class McpToolkit(BaseToolkit):
    """MCP integration: auth, resources, and dynamic MCP tool adapters."""

    def __init__(self, mcp_manager: Any = None) -> None:
        super().__init__(
            name="mcp",
            description="MCP integration: auth, resources, and dynamic tool adapters",
            tools=[McpAuthTool()],
        )
        if mcp_manager is not None:
            self.register(ListMcpResourcesTool(mcp_manager))
            self.register(ReadMcpResourceTool(mcp_manager))
            for tool_info in mcp_manager.list_tools():
                self.register(McpToolAdapter(mcp_manager, tool_info))
