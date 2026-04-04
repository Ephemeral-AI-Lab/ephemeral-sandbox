"""Web toolkit — internet access tools."""

from ephemeralos.tools.base import BaseToolkit
from ephemeralos.tools.web_fetch_tool import WebFetchTool
from ephemeralos.tools.web_search_tool import WebSearchTool


class WebToolkit(BaseToolkit):
    """Internet access: web fetch and search."""

    def __init__(self) -> None:
        super().__init__(
            name="web",
            description="Internet access: web fetch and search",
            tools=[WebFetchTool(), WebSearchTool()],
        )
