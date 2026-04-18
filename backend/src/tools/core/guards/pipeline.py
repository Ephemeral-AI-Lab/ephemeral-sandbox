"""Execution pipeline for registered tool guards."""

from __future__ import annotations

import inspect
import logging
from dataclasses import dataclass, field
from typing import TYPE_CHECKING

from pydantic import BaseModel

from tools.core.guards.registry import GuardEntry, ToolGuardRegistry, default_registry
from tools.core.guards.types import Advisory, Allow, Deny, GuardOutcome, MutateArgs

if TYPE_CHECKING:
    from tools.core.base import ToolExecutionContext, ToolResult

logger = logging.getLogger(__name__)


@dataclass
class PreResult:
    """Outcome of the pre-phase pipeline."""

    args: BaseModel
    deny: Deny | None = None
    warnings: list[str] = field(default_factory=list)


@dataclass
class PostResult:
    """Outcome of the post-phase pipeline (advisory-only today)."""

    warnings: list[str] = field(default_factory=list)


async def _invoke(entry: GuardEntry, *call_args) -> GuardOutcome:
    result = entry.guard(*call_args)
    if inspect.isawaitable(result):
        result = await result
    return result


async def run_pre(
    tool_name: str,
    args: BaseModel,
    context: "ToolExecutionContext",
    registry: ToolGuardRegistry | None = None,
) -> PreResult:
    """Run every pre-guard whose glob matches ``tool_name``.

    Short-circuits on ``Deny``. ``MutateArgs`` threads new args to
    subsequent guards. ``Advisory`` accumulates warnings.
    """
    reg = registry or default_registry()
    current_args = args
    warnings: list[str] = []
    for entry in reg.matching(tool_name, "pre"):
        outcome = await _invoke(entry, tool_name, current_args, context)
        logger.debug(
            "tool guard invoked",
            extra={
                "tool_name": tool_name,
                "guard": entry.name,
                "phase": "pre",
                "outcome": type(outcome).__name__,
            },
        )
        if isinstance(outcome, Deny):
            logger.info(
                "tool guard denied: %s blocked %s",
                entry.name,
                tool_name,
                extra={
                    "tool_name": tool_name,
                    "guard": entry.name,
                    "phase": "pre",
                    "outcome": "Deny",
                    "deny_message": outcome.message,
                },
            )
            return PreResult(args=current_args, deny=outcome, warnings=warnings)
        if isinstance(outcome, MutateArgs):
            logger.info(
                "tool guard mutated args: %s on %s",
                entry.name,
                tool_name,
                extra={
                    "tool_name": tool_name,
                    "guard": entry.name,
                    "phase": "pre",
                    "outcome": "MutateArgs",
                    "warning_count": len(outcome.warnings),
                },
            )
            current_args = outcome.new_args
            warnings.extend(outcome.warnings)
        elif isinstance(outcome, Advisory):
            warnings.extend(outcome.warnings)
        elif isinstance(outcome, Allow):
            continue
        else:  # defensive: unknown outcome type
            raise TypeError(
                f"Tool guard {entry.name!r} returned unsupported outcome: {outcome!r}"
            )
    return PreResult(args=current_args, warnings=warnings)


async def run_post(
    tool_name: str,
    args: BaseModel,
    context: "ToolExecutionContext",
    result: "ToolResult",
    registry: ToolGuardRegistry | None = None,
) -> PostResult:
    """Run every post-guard whose glob matches ``tool_name``.

    Post-phase is advisory-only today: ``Deny`` / ``MutateArgs`` outcomes
    are ignored (a follow-up phase will decide whether CodeAct's audited
    write policy justifies a post-deny escape hatch).
    """
    reg = registry or default_registry()
    warnings: list[str] = []
    for entry in reg.matching(tool_name, "post"):
        outcome = await _invoke(entry, tool_name, args, context, result)
        logger.debug(
            "tool guard invoked",
            extra={
                "tool_name": tool_name,
                "guard": entry.name,
                "phase": "post",
                "outcome": type(outcome).__name__,
            },
        )
        if isinstance(outcome, Advisory):
            warnings.extend(outcome.warnings)
        # Allow / Deny / MutateArgs: silently ignored in post-phase for now.
    return PostResult(warnings=warnings)
