"""TaskCenter ``SpawnFunc`` adapter that drives a real EphemeralAgent per task.

The dispatcher in :mod:`task_center.runtime.orchestrator` calls
``spawn_func(task_id, tc, sandbox_id)`` for each ``READY`` task. In production
this needs to:

1. Look up the agent definition by ``task.role`` (executor / evaluator).
2. Spawn/run via the server's ``execute_ephemeral_agent_run`` wrapper.
3. Inject ``task_center``, ``task_id``, ``role`` into the agent's tool
   metadata so terminal tools can call back into TaskCenter.
4. Forward events to the TaskCenter-owned callback (which the chat router
   connects to its SSE stream).
"""

from __future__ import annotations

import logging
from collections.abc import Awaitable, Callable
from typing import TYPE_CHECKING, Any, Mapping

if TYPE_CHECKING:
    from task_center.runtime.orchestrator import TaskCenter

logger = logging.getLogger(__name__)


def build_production_spawn(
    runtime_config: Any,
    *,
    extra_tool_metadata: Mapping[str, Any] | None = None,
) -> Callable[[str, "TaskCenter", str | None], Awaitable[None]]:
    """Build a ``SpawnFunc`` bound to runtime config and optional sandbox."""

    async def spawn(task_id: str, tc: "TaskCenter", sandbox_id: str | None) -> None:
        from agents.registry import get_definition
        from server.routers.core import execute_ephemeral_agent_run
        from task_center.errors import TaskCenterError
        from task_center.harness_agents.prompts import build_task_prompt
        from tools.core.base import ExecutionMetadata

        task = tc.graph.get(task_id)
        agent_def = get_definition(task.role)
        if agent_def is None:
            raise TaskCenterError(
                f"production spawn: no agent definition registered for role "
                f"{task.role!r} (expected 'executor', 'planner', or 'evaluator')"
            )

        meta = ExecutionMetadata()
        meta["task_center"] = tc
        meta["task_id"] = task_id
        meta["persisted_task_id"] = tc.persisted_task_id(task_id)
        if tc.request_id is not None:
            meta["request_id"] = tc.request_id
        if tc.run_id is not None:
            meta["task_center_run_id"] = tc.run_id
        meta["role"] = task.role
        meta["agent_type"] = agent_def.agent_type
        if extra_tool_metadata:
            meta.update(extra_tool_metadata)

        try:
            await execute_ephemeral_agent_run(
                runtime_config,
                build_task_prompt(task, tc.graph),
                on_agent_event=tc._emit_event,
                agent_def=agent_def,
                sandbox_id=sandbox_id,
                task_id=tc.persisted_task_id(task_id),
                extra_tool_metadata=meta,
            )
        except Exception:
            logger.exception("agent spawn: agent for %r crashed", task_id)
            raise

    return spawn
