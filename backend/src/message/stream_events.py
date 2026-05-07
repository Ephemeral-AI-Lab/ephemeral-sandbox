"""Events yielded by the query engine."""

from dataclasses import dataclass, field
from typing import Any

from providers.types import UsageSnapshot
from message.messages import ConversationMessage
from notification._runtime import SystemNotification


# Identity fields carried by every StreamEvent:
#   agent_name — short label of the emitting agent ("coordinator",
#                "developer-1", "eval_agent", ...). Empty string for
#                standalone single-agent callers.
#   run_id    — stable identifier for the unit of work that produced the
#                event. For a coordinator's own response this is its run_id;
#                for a dispatched subagent it is the subagent's run_id
#                (distinct from the parent). Lets printers group and
#                indent events by work unit even when agents interleave.


@dataclass(frozen=True)
class ThinkingDelta:
    """Incremental thinking/reasoning content from the model."""

    text: str
    agent_name: str = ""
    run_id: str = ""


@dataclass(frozen=True)
class AssistantTextDelta:
    """Incremental assistant text."""

    text: str
    agent_name: str = ""
    run_id: str = ""


@dataclass(frozen=True)
class AssistantMessageComplete:
    """Completed assistant message."""

    message: ConversationMessage
    usage: UsageSnapshot
    agent_name: str = ""
    run_id: str = ""


@dataclass(frozen=True)
class ToolExecutionStarted:
    """The engine is about to execute a tool."""

    tool_name: str
    tool_input: dict[str, Any]
    agent_name: str = ""
    run_id: str = ""


@dataclass(frozen=True)
class ToolExecutionCompleted:
    """A tool has finished executing."""

    tool_name: str
    output: str
    is_error: bool = False
    tool_id: str = ""
    metadata: dict[str, Any] = field(default_factory=dict)
    does_terminate: bool = False
    agent_name: str = ""
    run_id: str = ""


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
    agent_name: str = ""
    run_id: str = ""


@dataclass(frozen=True)
class ToolExecutionCancelled:
    """A tool was cancelled by LLM abort signal."""

    tool_id: str
    tool_name: str
    reason: str
    agent_name: str = ""
    run_id: str = ""


@dataclass(frozen=True)
class BackgroundTaskStarted:
    """A tool has been launched as a background task."""

    task_id: str
    tool_name: str
    tool_input: dict[str, Any]
    agent_name: str = ""
    run_id: str = ""


@dataclass(frozen=True)
class BackgroundTaskCompleted:
    """A background task has finished."""

    task_id: str
    tool_name: str
    output: str
    is_error: bool = False
    agent_name: str = ""
    run_id: str = ""


StreamEvent = (
    ThinkingDelta
    | AssistantTextDelta
    | AssistantMessageComplete
    | ToolExecutionStarted
    | ToolExecutionCompleted
    | ToolExecutionProgress
    | ToolExecutionCancelled
    | BackgroundTaskStarted
    | BackgroundTaskCompleted
    | SystemNotification
)
