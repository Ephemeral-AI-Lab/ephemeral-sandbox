"""Events yielded by the query engine."""

from dataclasses import dataclass
from typing import Any

from providers.types import UsageSnapshot
from message.messages import ConversationMessage


@dataclass(frozen=True)
class ThinkingDelta:
    """Incremental thinking/reasoning content from the model."""

    text: str


@dataclass(frozen=True)
class AssistantTextDelta:
    """Incremental assistant text."""

    text: str


@dataclass(frozen=True)
class AssistantTurnComplete:
    """Completed assistant turn."""

    message: ConversationMessage
    usage: UsageSnapshot


@dataclass(frozen=True)
class ToolExecutionStarted:
    """The engine is about to execute a tool."""

    tool_name: str
    tool_input: dict[str, Any]
    task_note: str = ""


@dataclass(frozen=True)
class ToolExecutionCompleted:
    """A tool has finished executing."""

    tool_name: str
    output: str
    is_error: bool = False
    tool_id: str = ""


@dataclass(frozen=True)
class ToolExecutionProgress:
    """Progress update from a running tool.

    Emitted during long-running tool execution (e.g., bash commands,
    test runners) so the LLM can see partial output and decide
    whether to continue or abort.
    """

    tool_id: str
    tool_name: str
    output: str


@dataclass(frozen=True)
class ToolExecutionCancelled:
    """A tool was cancelled by LLM abort signal."""

    tool_id: str
    tool_name: str
    reason: str


@dataclass(frozen=True)
class BackgroundTaskStarted:
    """A tool has been launched as a background task."""

    task_id: str
    tool_name: str
    tool_input: dict[str, Any]


@dataclass(frozen=True)
class BackgroundTaskCompleted:
    """A background task has finished."""

    task_id: str
    tool_name: str
    output: str
    is_error: bool = False


StreamEvent = (
    ThinkingDelta
    | AssistantTextDelta
    | AssistantTurnComplete
    | ToolExecutionStarted
    | ToolExecutionCompleted
    | ToolExecutionProgress
    | ToolExecutionCancelled
    | BackgroundTaskStarted
    | BackgroundTaskCompleted
)
