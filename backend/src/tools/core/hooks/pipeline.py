"""Sequential platform hook pipelines."""

from __future__ import annotations

import inspect
import logging
from typing import TYPE_CHECKING

from pydantic import BaseModel

from message.stream_events import SystemNotification
from tools.core.hooks.outcomes import (
    EmitStreamEvent,
    PostHookOutcome,
    PreHookOutcome,
    PreHookPipelineResult,
)
from tools.core.hooks.registry import HookEntry, ToolHookRegistry, default_registry

if TYPE_CHECKING:
    from tools.core.base import ToolExecutionContext, ToolResult

logger = logging.getLogger(__name__)


async def _invoke_pre(
    entry: HookEntry,
    tool_name: str,
    args: BaseModel,
    context: "ToolExecutionContext",
) -> PreHookOutcome:
    result = entry.target(tool_name, args, context)  # type: ignore[misc]
    if inspect.isawaitable(result):
        result = await result
    if not isinstance(result, PreHookOutcome):
        raise TypeError(f"pre-hook {entry.name!r} returned unsupported outcome: {result!r}")
    return result


async def _invoke_post(
    entry: HookEntry,
    tool_name: str,
    args: BaseModel,
    context: "ToolExecutionContext",
    result: "ToolResult",
) -> PostHookOutcome:
    outcome = entry.target(tool_name, args, context, result)  # type: ignore[misc]
    if inspect.isawaitable(outcome):
        outcome = await outcome
    if not isinstance(outcome, PostHookOutcome):
        raise TypeError(f"post-hook {entry.name!r} returned unsupported outcome: {outcome!r}")
    return outcome


async def _emit_advisories(
    *,
    phase: str,
    tool_name: str,
    advisories: tuple[str, ...],
    emit: EmitStreamEvent,
) -> None:
    label = "tip" if phase == "pre" else "advisory"
    category = f"{phase}_hook_{label}"
    for advisory in advisories:
        await emit(
            SystemNotification(
                text=f"[{phase}-hook {label}] {tool_name}: {advisory}",
                category=category,
            )
        )


async def run_pre_hooks(
    tool_name: str,
    args: BaseModel,
    context: "ToolExecutionContext",
    *,
    emit: EmitStreamEvent,
    registry: ToolHookRegistry | None = None,
) -> PreHookPipelineResult:
    """Run matching pre-hooks sequentially.

    Mutation is threaded hook-by-hook. Advisories are emitted immediately.
    Denials and hook exceptions stop the chain.
    """
    reg = registry or default_registry()
    current_args = args
    for entry in reg.matching(tool_name, "pre"):
        try:
            outcome = await _invoke_pre(entry, tool_name, current_args, context)
        except Exception as exc:
            logger.info("pre-hook failed: %s on %s", entry.name, tool_name, exc_info=True)
            return PreHookPipelineResult(
                tool_input=current_args,
                has_error=True,
                error_message=f"{entry.name}: {exc}",
            )

        logger.debug(
            "platform pre-hook invoked",
            extra={
                "tool_name": tool_name,
                "hook": entry.name,
                "phase": "pre",
                "has_error": outcome.has_error,
                "advisory_count": len(outcome.advisories),
                "mutated": outcome.tool_input is not None,
            },
        )
        if outcome.has_error:
            logger.info(
                "pre-hook denied: %s blocked %s",
                entry.name,
                tool_name,
                extra={
                    "tool_name": tool_name,
                    "hook": entry.name,
                    "phase": "pre",
                    "deny_message": outcome.error_message or "",
                },
            )
            return PreHookPipelineResult(
                tool_input=current_args,
                has_error=True,
                error_message=outcome.error_message,
            )

        await _emit_advisories(
            phase="pre",
            tool_name=tool_name,
            advisories=outcome.advisories,
            emit=emit,
        )
        if outcome.tool_input is not None:
            current_args = outcome.tool_input

    return PreHookPipelineResult(tool_input=current_args)


async def run_post_hooks(
    tool_name: str,
    args: BaseModel,
    context: "ToolExecutionContext",
    result: "ToolResult",
    *,
    emit: EmitStreamEvent,
    registry: ToolHookRegistry | None = None,
) -> PostHookOutcome:
    """Run matching post-hooks sequentially."""
    reg = registry or default_registry()
    for entry in reg.matching(tool_name, "post"):
        try:
            outcome = await _invoke_post(entry, tool_name, args, context, result)
        except Exception as exc:
            logger.info("post-hook failed: %s on %s", entry.name, tool_name, exc_info=True)
            return PostHookOutcome(
                has_error=True,
                error_message=f"{entry.name}: {exc}",
            )

        logger.debug(
            "platform post-hook invoked",
            extra={
                "tool_name": tool_name,
                "hook": entry.name,
                "phase": "post",
                "has_error": outcome.has_error,
                "advisory_count": len(outcome.advisories),
            },
        )
        if outcome.has_error:
            logger.info(
                "post-hook denied: %s blocked %s",
                entry.name,
                tool_name,
                extra={
                    "tool_name": tool_name,
                    "hook": entry.name,
                    "phase": "post",
                    "deny_message": outcome.error_message or "",
                },
            )
            return outcome

        await _emit_advisories(
            phase="post",
            tool_name=tool_name,
            advisories=outcome.advisories,
            emit=emit,
        )

    return PostHookOutcome()
