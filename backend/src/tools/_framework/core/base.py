"""Core tool abstraction."""

from __future__ import annotations

from abc import ABC, abstractmethod
from typing import Any

from pydantic import BaseModel

from sandbox._shared.models import Intent
from tools._framework.core.context import ToolExecutionContextService
from tools._framework.core.results import TextToolOutput, ToolResult
from tools._framework.core.runtime import ExecutionMetadata

__all__ = [
    "BaseTool",
    "ExecutionMetadata",
    "TextToolOutput",
    "ToolExecutionContextService",
    "ToolResult",
]


class BaseTool(ABC):
    """Base class for all EphemeralOS tools."""

    name: str
    description: str
    short_description: str | None = None
    input_model: type[BaseModel]
    output_model: type[BaseModel] = TextToolOutput
    # Foreground execution intent — propagated through context["__intent"]
    # to plugin handlers and the daemon dispatcher so READ_ONLY plugin ops
    # skip overlay allocation while WRITE_ALLOWED ops keep the OCC publish
    # path. Required on every @tool callsite; missing intent raises at
    # import time (see tools._framework.core.decorator).
    intent: Intent
    # Discriminator for monitoring/UI/audit.
    task_type: str = "agent"
    # When True, a successful invocation ends the agent run. The
    # engine stamps is_terminal=True on the resulting ToolResult and the
    # query loop exits with TOOL_STOP after the response completes.
    is_terminal_tool: bool = False
    # Tool-specific hooks. These are intentionally explicit per tool and do
    # not affect the LLM-facing schema.
    pre_hooks: tuple[Any, ...] = ()
    post_hooks: tuple[Any, ...] = ()
    # Runtime context dependencies declared by tools. Runtime assembly uses
    # these markers to attach provider-specific context preparers without the
    # core query loop sniffing tool names.
    context_requirements: tuple[str, ...] = ()

    @abstractmethod
    async def execute(
        self,
        arguments: BaseModel,
        context: ToolExecutionContextService,
    ) -> ToolResult:
        """Execute the tool."""

    def output_schema(self) -> dict[str, Any]:
        """Return the output JSON Schema for successful tool output."""
        return self.output_model.model_json_schema()

    def to_api_schema(self) -> dict[str, Any]:
        """Return the tool schema expected by the Anthropic Messages API."""
        schema: dict[str, Any] = {
            "name": self.name,
            "description": self.description,
            "input_schema": self.input_model.model_json_schema(),
        }
        schema["output_schema"] = self.output_schema()
        return schema
