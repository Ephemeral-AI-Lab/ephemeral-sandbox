"""Core engine exports."""

from __future__ import annotations

from typing import TYPE_CHECKING

if TYPE_CHECKING:  # pragma: no cover
    from engine.runtime.agent import EphemeralAgent, spawn_agent
    from message.messages import (
        ConversationMessage,
        TextBlock,
        ThinkingBlock,
        ToolResultBlock,
        ToolUseBlock,
    )

    from message.stream_events import (
        AssistantTextDelta,
        AssistantTurnComplete,
        BackgroundTaskCompleted,
        BackgroundTaskStarted,
        StreamEvent,
        ThinkingDelta,
        ToolExecutionCancelled,
        ToolExecutionCompleted,
        ToolExecutionProgress,
        ToolExecutionStarted,
    )
    from engine.core.query import QueryContext, run_query
    from engine.core.streaming_executor import StreamingToolExecutor, TrackedTool
    from engine.runtime.background_tasks import BackgroundTaskManager, TrackedBackgroundTask

__all__ = [
    "AssistantTextDelta",
    "AssistantTurnComplete",
    "BackgroundTaskCompleted",
    "BackgroundTaskManager",
    "BackgroundTaskStarted",
    "ConversationMessage",
    "EphemeralAgent",
    "QueryContext",
    "run_query",
    "StreamingToolExecutor",
    "StreamEvent",
    "TextBlock",
    "ThinkingBlock",
    "ThinkingDelta",
    "ToolExecutionCancelled",
    "ToolExecutionCompleted",
    "ToolExecutionProgress",
    "ToolExecutionStarted",
    "ToolResultBlock",
    "ToolUseBlock",
    "TrackedBackgroundTask",
    "TrackedTool",
    "spawn_agent",
]

_SUBMODULES = {
    "EphemeralAgent": ("engine.runtime.agent", "EphemeralAgent"),
    "spawn_agent": ("engine.runtime.agent", "spawn_agent"),
    "BackgroundTaskManager": ("engine.runtime.background_tasks", "BackgroundTaskManager"),
    "TrackedBackgroundTask": ("engine.runtime.background_tasks", "TrackedBackgroundTask"),
    "ConversationMessage": ("message.messages", "ConversationMessage"),
    "TextBlock": ("message.messages", "TextBlock"),
    "ThinkingBlock": ("message.messages", "ThinkingBlock"),
    "ToolResultBlock": ("message.messages", "ToolResultBlock"),
    "ToolUseBlock": ("message.messages", "ToolUseBlock"),
    "AssistantTextDelta": ("message.stream_events", "AssistantTextDelta"),
    "AssistantTurnComplete": ("message.stream_events", "AssistantTurnComplete"),
    "BackgroundTaskCompleted": ("message.stream_events", "BackgroundTaskCompleted"),
    "BackgroundTaskStarted": ("message.stream_events", "BackgroundTaskStarted"),
    "StreamEvent": ("message.stream_events", "StreamEvent"),
    "ThinkingDelta": ("message.stream_events", "ThinkingDelta"),
    "ToolExecutionCancelled": ("message.stream_events", "ToolExecutionCancelled"),
    "ToolExecutionCompleted": ("message.stream_events", "ToolExecutionCompleted"),
    "ToolExecutionProgress": ("message.stream_events", "ToolExecutionProgress"),
    "ToolExecutionStarted": ("message.stream_events", "ToolExecutionStarted"),
    "QueryContext": ("engine.core.query", "QueryContext"),
    "run_query": ("engine.core.query", "run_query"),
    "StreamingToolExecutor": ("engine.core.streaming_executor", "StreamingToolExecutor"),
    "TrackedTool": ("engine.core.streaming_executor", "TrackedTool"),
}


def __getattr__(name: str):
    if entry := _SUBMODULES.get(name):
        module_path, attr_name = entry
        from importlib import import_module

        mod = import_module(module_path)
        return getattr(mod, attr_name)
    raise AttributeError(name)
