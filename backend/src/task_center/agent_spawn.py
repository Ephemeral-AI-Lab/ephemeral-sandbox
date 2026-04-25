"""TaskCenter ``SpawnFunc`` adapter that drives a real EphemeralAgent per task.

The dispatcher in :mod:`task_center.center` calls
``spawn_func(task_id, tc, sandbox_id)`` for each ``READY`` task. In production
this needs to:

1. Look up the agent definition by ``task.role`` (executor / evaluator).
2. Spawn/run via the server's ``execute_ephemeral_agent_run`` wrapper.
3. Inject ``task_center``, ``task_id``, ``role`` into the agent's tool
   metadata so the submission tools can call back into TaskCenter.
4. Forward events to the TaskCenter-owned callback (which the chat router
   connects to its SSE stream).
"""

from __future__ import annotations

import logging
from collections.abc import Awaitable, Callable
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from task_center.center import TaskCenter

logger = logging.getLogger(__name__)


def make_production_spawn(
    session_config: Any,
) -> Callable[[str, "TaskCenter", str | None], Awaitable[None]]:
    """Build a ``SpawnFunc`` bound to a session config and optional sandbox."""

    async def spawn(task_id: str, tc: "TaskCenter", sandbox_id: str | None) -> None:
        from agents.registry import get_definition
        from server.routers.core import execute_ephemeral_agent_run
        from task_center.context import build_task_prompt
        from task_center.errors import TaskCenterError
        from tools.core.base import ExecutionMetadata

        task = tc.graph.get(task_id)
        agent_def = get_definition(task.role)
        if agent_def is None:
            raise TaskCenterError(
                f"production spawn: no agent definition registered for role "
                f"{task.role!r} (expected 'executor' or 'evaluator')"
            )

        meta = ExecutionMetadata()
        meta["task_center"] = tc
        meta["task_id"] = task_id
        meta["role"] = task.role

        try:
            await execute_ephemeral_agent_run(
                session_config,
                build_task_prompt(task, tc.graph),
                on_agent_event=tc._emit_event,
                agent_def=agent_def,
                sandbox_id=sandbox_id,
                terminal_tools=set(agent_def.terminal_tools),
                extra_tool_metadata=meta,
            )
        except Exception:
            logger.exception("agent spawn: agent for %r crashed", task_id)
            raise

    return spawn
