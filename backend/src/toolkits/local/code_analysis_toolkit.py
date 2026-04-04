"""Code analysis toolkit — language server protocol tools."""

from ephemeralos.tools.base import BaseToolkit
from ephemeralos.tools.lsp_tool import LspTool


class CodeAnalysisToolkit(BaseToolkit):
    """Code analysis via language server protocol."""

    def __init__(self) -> None:
        super().__init__(
            name="code_analysis",
            description="Code analysis via language server protocol",
            tools=[LspTool()],
        )
