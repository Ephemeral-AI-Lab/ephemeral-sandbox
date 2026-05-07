"""Tool execution context preparation hooks."""

from __future__ import annotations

import logging
from typing import TYPE_CHECKING

from tools import ToolExecutionContextService

if TYPE_CHECKING:
    from engine.query.loop import QueryContext


logger = logging.getLogger(__name__)


async def prepare_tool_execution_context(
    context: QueryContext,
    execution_context: ToolExecutionContextService,
) -> None:
    """Run runtime/toolkit-specific context hooks before tool dispatch."""
    metadata = context.tool_metadata
    if metadata is None:
        return

    for preparer in metadata.context_preparers:
        prepare = getattr(preparer, "prepare_context_async", None)
        if prepare is None:
            continue
        try:
            await prepare(execution_context)
            metadata.update(execution_context.services_copy())
        except Exception as exc:
            logger.debug(
                "Tool context preparation skipped for %s: %s",
                type(preparer).__name__,
                exc,
            )
