"""Background tool dispatch plumbing used by the query loop."""

from __future__ import annotations

from collections.abc import Awaitable, Callable
from pathlib import Path
from typing import TYPE_CHECKING, Any

from pydantic import ValidationError

from engine.runtime.background_tasks import BackgroundTaskManager
from engine.runtime.tool_trace import record_tool_trace
from message.messages import ToolResultBlock, ToolUseBlock
from message.stream_events import (
    BackgroundTaskStarted,
    StreamEvent,
    ToolExecutionCompleted,
)
from providers.types import UsageSnapshot
from tools.core.base import BaseTool, ToolExecutionContext, ToolRegistry, ToolResult
from tools.core.runtime import ExecutionMetadata, merge_runtime_metadata

if TYPE_CHECKING:
    from engine.core.query import QueryContext

ToolCallExecutor = Callable[
    [str, str, dict[str, object], ExecutionMetadata | dict[str, Any] | None],
    Awaitable[ToolResultBlock],
]


def run_background_preflight(
    *,
    cwd: Path,
    tool_registry: ToolRegistry,
    tool_metadata: ExecutionMetadata | None,
    tool_def: BaseTool,
    tool_use_id: str,
    clean_input: dict[str, object],
) -> tuple[ToolResult | None, ExecutionMetadata]:
    metadata = tool_metadata.copy() if tool_metadata is not None else ExecutionMetadata()
    metadata.tool_registry = tool_registry
    metadata.tool_id = tool_use_id

    preflight = getattr(tool_def, "background_preflight", None)
    if not callable(preflight):
        return None, metadata
    try:
        parsed_input = tool_def.input_model.model_validate(clean_input)
    except ValidationError as exc:
        errors = "; ".join(
            f"{'.'.join(str(p) for p in e['loc'])}: {e['msg']}" for e in exc.errors()
        )
        return (
            ToolResult(
                output=(
                    f"Invalid input for {tool_def.name}: {errors}. "
                    "Please retry the tool call with valid arguments."
                ),
                is_error=True,
            ),
            metadata,
        )
    except Exception as exc:
        return (
            ToolResult(
                output=f"Invalid input for {tool_def.name}: {exc}",
                is_error=True,
            ),
            metadata,
        )
    try:
        return (
            preflight(
                parsed_input,
                ToolExecutionContext(cwd=cwd, metadata=metadata),
            ),
            metadata,
        )
    except Exception as exc:
        return (
            ToolResult(
                output=f"Tool background preflight failed: {exc}",
                is_error=True,
            ),
            metadata,
        )


def launch_background_tool(
    *,
    tool_registry: ToolRegistry,
    tool_metadata: ExecutionMetadata | None,
    cwd: Path,
    background_manager: BackgroundTaskManager,
    tool_use: ToolUseBlock,
    task_note: str,
    execute_tool_call: ToolCallExecutor,
) -> tuple[ToolResultBlock, BackgroundTaskStarted | None, ToolExecutionCompleted | None]:
    """Dispatch a single tool use through the background path."""
    clean_input = {k: v for k, v in tool_use.input.items() if k not in ("background", "task_note")}

    tool_def = tool_registry.get(tool_use.name)
    if tool_def is None or getattr(tool_def, "background", "forbidden") == "forbidden":
        msg = f"Tool '{tool_use.name}' does not support background execution."
        return (
            ToolResultBlock(tool_use_id=tool_use.id, content=msg, is_error=True),
            None,
            ToolExecutionCompleted(tool_name=tool_use.name, output=msg, is_error=True),
        )

    kill_callback = None
    preflight_result, preflight_metadata = run_background_preflight(
        cwd=cwd,
        tool_registry=tool_registry,
        tool_metadata=tool_metadata,
        tool_def=tool_def,
        tool_use_id=tool_use.id,
        clean_input=clean_input,
    )
    if preflight_result is not None:
        merge_runtime_metadata(
            original=tool_metadata,
            updated=preflight_metadata,
            result_metadata=preflight_result.metadata,
        )
        return (
            ToolResultBlock(
                tool_use_id=tool_use.id,
                content=preflight_result.output,
                is_error=preflight_result.is_error,
                metadata=preflight_result.metadata,
            ),
            None,
            ToolExecutionCompleted(
                tool_name=tool_use.name,
                output=preflight_result.output,
                is_error=preflight_result.is_error,
                tool_id=tool_use.id,
                metadata=dict(preflight_result.metadata or {}),
            ),
        )

    bg_alias = background_manager.next_alias()

    async def _bg_wrapper(alias: str = bg_alias) -> ToolResult:
        bg_overrides = ExecutionMetadata(
            on_progress_line=background_manager.make_progress_callback(alias),
            background_task_id=alias,
        )
        block = await execute_tool_call(
            tool_use.name,
            tool_use.id,
            clean_input,
            bg_overrides,
        )
        return ToolResult(output=block.content, is_error=block.is_error)

    bg_event = background_manager.launch(
        bg_alias,
        tool_use.name,
        clean_input,
        _bg_wrapper(),
        task_note=task_note,
        kill_callback=kill_callback,
        task_type=getattr(tool_def, "task_type", "agent"),
    )
    record_tool_trace(tool_metadata, tool_use.name, clean_input, tool_use_id=tool_use.id)
    tool_result = ToolResultBlock(
        tool_use_id=tool_use.id,
        content=(
            f'[BACKGROUND LAUNCHED] task_id="{bg_alias}" tool={tool_use.name}\n'
            f"Use this task_id with "
            f'check_background_progress(task_id="{bg_alias}"), '
            f'wait_for_background_task(task_id="{bg_alias}"), or '
            f'cancel_background_task(task_id="{bg_alias}"). '
            f"Keep using the current turn on other ready work first; do not "
            f"wait immediately unless this task is the only blocker left. "
            f"A [BACKGROUND {bg_alias} COMPLETED] message will arrive automatically."
        ),
        is_error=False,
    )
    return tool_result, bg_event, None


def launch_and_collect_bg_events(
    context: QueryContext,
    background_manager: BackgroundTaskManager,
    tc: ToolUseBlock,
    task_note: str,
    tool_results: list[ToolResultBlock],
) -> list[tuple[StreamEvent, UsageSnapshot | None]]:
    async def _execute_in_context(
        tool_name: str,
        tool_use_id: str,
        tool_input: dict[str, object],
        extra_metadata: ExecutionMetadata | dict[str, Any] | None = None,
    ) -> ToolResultBlock:
        from tools.core.tool_execution import execute_tool_call

        return await execute_tool_call(
            context,
            tool_name,
            tool_use_id,
            tool_input,
            extra_metadata=extra_metadata,
        )

    tool_result, bg_event, reject_event = launch_background_tool(
        tool_registry=context.tool_registry,
        tool_metadata=context.tool_metadata,
        cwd=context.cwd,
        background_manager=background_manager,
        tool_use=tc,
        task_note=task_note,
        execute_tool_call=_execute_in_context,
    )
    tool_results.append(tool_result)
    events: list[tuple[StreamEvent, UsageSnapshot | None]] = []
    if bg_event is not None:
        events.append((bg_event, None))
    if reject_event is not None:
        events.append((reject_event, None))
    return events
