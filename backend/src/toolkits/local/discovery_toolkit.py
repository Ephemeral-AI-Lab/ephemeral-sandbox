"""Discovery toolkit — tool and skill lookup tools."""

from ephemeralos.tools.base import BaseToolkit
from ephemeralos.tools.skill_tool import SkillTool
from ephemeralos.tools.tool_search_tool import ToolSearchTool


class DiscoveryToolkit(BaseToolkit):
    """Tool and skill discovery and lookup."""

    def __init__(self) -> None:
        super().__init__(
            name="discovery",
            description="Tool and skill discovery and lookup",
            tools=[SkillTool(), ToolSearchTool()],
        )
