"""Native team agent runner.

Provides :class:`TeamAgentRunner`, the standard implementation of the
``QueryRunner`` callable expected by :class:`team.runtime.executor.Executor`.
It spawns an :class:`EphemeralAgent`, wires ``tool_metadata`` and
``terminal_tools`` into the agent's ``QueryContext``, drives the event loop,
observes tool completions for coordination (``TaskCenter.on_edit`` /
``on_submission``), and schedules ``tc_note`` checkpoints.
"""

from __future__ import annotations

import asyncio
import logging
from collections.abc import Awaitable, Callable
from dataclasses import dataclass, field
from typing import Any

from agents.run_tracker import AgentRunTracker
from code_intelligence._async_bridge import configure_default_executor
from engine.core.query import (
    MAX_TERMINAL_NUDGE_RETRIES,
    TERMINAL_NUDGE_BUDGET_BONUS,
    QueryExitReason,
    build_terminal_nudge_text,
)
from engine.runtime.agent import spawn_agent
from message.stream_events import ToolExecutionCompleted, ToolExecutionStarted
from team.runtime.context_builder import TeamAgentContext

logger = logging.getLogger(__name__)

_DEFAULT_EXECUTOR_READY = False
"""One-shot latch so we don't re-create the executor per agent run."""


def _ensure_default_executor_raised() -> None:
    """Raise the running loop's default ThreadPoolExecutor once per process.

    Bulk sandbox-bound svc ops (delete/move/write/edit/rename/shell)
    fan out via ``asyncio.to_thread``; Python's default executor is
    ``min(32, cpu+4)`` which becomes the bottleneck under team-parallel
    load. The loop-aware ``run_sync`` bridge (see
    :mod:`code_intelligence._async_bridge`) requires enough worker
    threads that ``to_thread`` dispatches don't queue behind unrelated
    sandbox reads.
    """
    global _DEFAULT_EXECUTOR_READY
    if _DEFAULT_EXECUTOR_READY:
        return
    try:
        configure_default_executor()
    except RuntimeError:
        # No running loop yet — pytest collection can hit this path.
        return
    _DEFAULT_EXECUTOR_READY = True

# Tools whose completion should tick ``ActivityTracker.on_submission`` for coordination.
SUBMISSION_TOOL_NAMES = frozenset({
    "submit_task_success",
    "request_replan",
    "submit_task_note",
    "submit_plan",
    "submit_replan",
})
# Tools whose completion should record a scoped edit via ``TaskCenter.on_edit``.
EDIT_TOOL_NAMES = frozenset({"daytona_edit_file", "daytona_write_file"})


def _coerce_terminal_tools(value: Any) -> set[str]:
    if isinstance(value, (set, frozenset)):
        return set(value)
    if isinstance(value, list):
        return set(value)
    return set()


def _copy_terminal_nudge_state(source: Any, target: Any) -> None:
    target.tool_calls_used = getattr(source, "tool_calls_used", 0)
    target.last_budget_warning_remaining = getattr(
        source,
        "last_budget_warning_remaining",
        None,
    )
    target.terminal_nudge_retries_used = getattr(
        source,
        "terminal_nudge_retries_used",
        0,
    )
    target.terminal_nudge_budget_extended = getattr(
        source,
        "terminal_nudge_budget_extended",
        False,
    )
    if getattr(source, "tool_call_limit", None) is not None:
        target.tool_call_limit = source.tool_call_limit


def _carry_forward_usage(source: Any, target: Any) -> None:
    source_usage = getattr(source, "total_usage", None)
    target_usage = getattr(target, "total_usage", None)
    if source_usage is None or target_usage is None:
        return
    target_usage.input_tokens += int(getattr(source_usage, "input_tokens", 0) or 0)
    target_usage.output_tokens += int(getattr(source_usage, "output_tokens", 0) or 0)


@dataclass
class AgentRunState:
    """Mutable state handed to :class:`TeamAgentRunner` hooks."""

    defn: Any
    ctx: TeamAgentContext
    agent: Any
    tracker: Any
    team_run_id: str
    work_item_id: str
    compacted_before: int | None = None
    final_text: str = ""
    error: str | None = None
    cancelled: bool = False
    pending_tool_inputs: dict[str, list[dict[str, Any]]] = field(default_factory=dict)


def extract_final_text(messages: list[Any]) -> str:
    """Return the last assistant text emitted during an agent run."""
    for msg in reversed(messages):
        if getattr(msg, "role", None) != "assistant":
            continue
        text = getattr(msg, "text", "")
        if text:
            return str(text).strip()
    return ""


class TeamAgentRunner:
    """Standard team runner — spawn agent, wire metadata, drive event loop.

    Responsibilities (always performed):
      * ``AgentRunTracker`` lifecycle
      * ``spawn_agent`` + tool_metadata wiring
      * ``terminal_tools`` wiring from metadata to QueryContext
      * ``on_turn`` callback (``task_center.tick``)
      * Tool completion observation (``on_edit`` / submission tools)
      * ``tc_note`` checkpoint scheduling (per agent's ``allowed_triggers``)

    Hooks (optional extension points):
      * ``on_spawned(state)`` — synchronous, after spawn, before ``agent.run``
      * ``on_event(event, state)`` — synchronous, per stream event
      * ``on_complete(state)`` — awaitable, after the event loop returns
      * ``on_checkpoint_event(payload)`` — synchronous, on tc_note lifecycle
    """

    def __init__(
        self,
        session_config: Any,
        sandbox_id: str,
        *,
        agent_overrides: dict[str, dict[str, Any]] | None = None,
        on_spawned: Callable[[AgentRunState], None] | None = None,
        on_event: Callable[[Any, AgentRunState], None] | None = None,
        on_complete: Callable[[AgentRunState], Awaitable[None]] | None = None,
        on_checkpoint_event: Callable[[dict[str, Any]], None] | None = None,
    ) -> None:
        self.session_config = session_config
        self.sandbox_id = sandbox_id
        self.agent_overrides = agent_overrides
        self.on_spawned = on_spawned
        self.on_event = on_event
        self.on_complete = on_complete
        self.on_checkpoint_event = on_checkpoint_event

    def _effective_defn(self, defn: Any) -> Any:
        if not self.agent_overrides:
            return defn
        overrides = self.agent_overrides.get(defn.name)
        return defn.model_copy(update=overrides) if overrides else defn

    async def __call__(self, defn: Any, ctx: TeamAgentContext) -> dict[str, Any]:
        _ensure_default_executor_raised()
        effective_defn = self._effective_defn(defn)
        prompt = ctx.user_message or ""

        tracker = AgentRunTracker.create(
            session_id=getattr(self.session_config, "session_id", None),
            run_id=getattr(ctx.tool_metadata, "agent_run_id", None),
            agent_name=effective_defn.name,
            input_query=prompt,
        )
        if tracker.run_id is not None:
            ctx.tool_metadata.agent_run_id = tracker.run_id

        terminal_tools_raw = ctx.tool_metadata.get("terminal_tools")
        terminal_tools = _coerce_terminal_tools(terminal_tools_raw)

        def _wire_agent(next_agent: Any, previous_qc: Any | None = None) -> None:
            # Merge spawn_agent's tool_metadata into ctx and redirect agent to ctx's metadata
            # so team tools (submit_plan / submit_replan / submit_task_success / request_replan / …)
            # write into the correct slot.
            spawned_meta = next_agent.query_context.tool_metadata
            if (
                spawned_meta is not None
                and getattr(spawned_meta, "session_config", None) is not None
            ):
                ctx.tool_metadata.session_config = spawned_meta.session_config
            sb = getattr(spawned_meta, "sandbox_id", None) if spawned_meta is not None else ""
            if sb:
                ctx.tool_metadata["sandbox_id"] = sb
            ctx.tool_metadata.agent_name = effective_defn.name
            next_agent.query_context.tool_metadata = ctx.tool_metadata
            next_agent.query_context.run_id = tracker.run_id or ""
            next_agent.query_context.terminal_tools = set(terminal_tools)

            if previous_qc is not None:
                _copy_terminal_nudge_state(previous_qc, next_agent.query_context)

        agent = spawn_agent(
            self.session_config,
            messages=list(ctx.initial_messages),
            agent_def=effective_defn,
            latest_user_prompt=prompt,
            sandbox_id=self.sandbox_id,
            terminal_tools=terminal_tools_raw,
        )
        _wire_agent(agent)

        compacted_before: int | None = None
        if getattr(agent.query_context, "session_state", None) is not None:
            compacted_before = int(agent.query_context.session_state.compacted)

        team_run_id = str(ctx.tool_metadata.get("team_run_id") or "")
        work_item_id = str(ctx.tool_metadata.get("work_item_id") or "")

        state = AgentRunState(
            defn=effective_defn,
            ctx=ctx,
            agent=agent,
            tracker=tracker,
            team_run_id=team_run_id,
            work_item_id=work_item_id,
            compacted_before=compacted_before,
        )
        checkpoint_task: asyncio.Task[None] | None = None

        def _snapshot() -> list[dict[str, Any]]:
            return [m.model_dump(mode="json") for m in agent.display_messages]

        def _schedule_checkpoint(snapshot: list[dict[str, Any]] | None = None) -> None:
            nonlocal checkpoint_task
            if not team_run_id or not work_item_id:
                return
            if "tc_note" not in getattr(effective_defn, "allowed_triggers", []):
                return
            try:
                from team.runtime.registry import get as get_team_run

                team_run = get_team_run(team_run_id)
                if team_run is None:
                    return
                frozen = snapshot if snapshot is not None else _snapshot()
                trigger = team_run.task_center.activity.should_take_note(work_item_id)
                if trigger is None:
                    return
                if checkpoint_task is not None and not checkpoint_task.done():
                    return
                checkpoint_api_client = agent.query_context.api_client
                checkpoint_model = agent.model

                async def _run_checkpoint() -> None:
                    event_base = {
                        "event": "external_hook",
                        "hook": "tc_note",
                        "team_run_id": team_run_id,
                        "work_item_id": work_item_id,
                        "agent": effective_defn.name,
                        "trigger": trigger,
                    }
                    if self.on_checkpoint_event is not None:
                        self.on_checkpoint_event({**event_base, "status": "started"})
                    try:
                        posted = await team_run.task_center.activity.check(
                            work_item_id,
                            snapshot=frozen,
                            api_client=checkpoint_api_client,
                            model=checkpoint_model,
                        )
                    except Exception as exc:
                        if self.on_checkpoint_event is not None:
                            self.on_checkpoint_event(
                                {**event_base, "status": "failed", "error": str(exc)}
                            )
                        raise
                    status = "completed" if posted else "skipped"
                    if self.on_checkpoint_event is not None:
                        self.on_checkpoint_event({**event_base, "status": status})

                checkpoint_task = asyncio.create_task(_run_checkpoint())
            except Exception:
                logger.debug(
                    "Failed to schedule task-center checkpoint for %s",
                    work_item_id,
                    exc_info=True,
                )

        def _on_turn(display_messages: list[Any]) -> None:
            if not team_run_id or not work_item_id:
                return
            try:
                from team.runtime.registry import get as get_team_run

                team_run = get_team_run(team_run_id)
                if team_run is None:
                    return
                snap = [m.model_dump(mode="json") for m in display_messages]
                team_run.task_center.activity.tick(work_item_id)
                _schedule_checkpoint(snap)
            except Exception:
                logger.debug("Failed to observe turn for %s", work_item_id, exc_info=True)

        agent.query_context.on_turn = _on_turn

        async def _run_agent_once(run_prompt: str) -> None:
            async for event in agent.run(run_prompt):
                if isinstance(event, ToolExecutionStarted):
                    state.pending_tool_inputs.setdefault(event.tool_name, []).append(
                        event.tool_input
                    )
                elif (
                    isinstance(event, ToolExecutionCompleted)
                    and team_run_id
                    and work_item_id
                ):
                    try:
                        from team.runtime.registry import get as get_team_run

                        team_run = get_team_run(team_run_id)
                        inputs = state.pending_tool_inputs.get(event.tool_name) or []
                        tool_input = inputs.pop(0) if inputs else {}
                        if team_run is not None and not event.is_error:
                            if event.tool_name in EDIT_TOOL_NAMES:
                                file_path = str(
                                    tool_input.get("file_path")
                                    or tool_input.get("path")
                                    or ""
                                ).strip()
                                if file_path:
                                    team_run.task_center.activity.on_edit(work_item_id, file_path)
                            if event.tool_name in SUBMISSION_TOOL_NAMES:
                                team_run.task_center.activity.on_submission(work_item_id)
                            _schedule_checkpoint()
                    except Exception:
                        logger.debug(
                            "Failed to observe tool completion for %s",
                            work_item_id,
                            exc_info=True,
                        )
                if self.on_event is not None:
                    self.on_event(event, state)

        if self.on_spawned is not None:
            self.on_spawned(state)

        try:
            try:
                await _run_agent_once(prompt)

                terminal_tools = set(agent.query_context.terminal_tools or set())
                while (
                    terminal_tools
                    and agent.query_context.exit_reason == QueryExitReason.RESOURCE_LIMIT
                    and agent.query_context.terminal_nudge_retries_used
                    < MAX_TERMINAL_NUDGE_RETRIES
                ):
                    qc = agent.query_context
                    qc.terminal_nudge_retries_used += 1
                    if (
                        qc.tool_call_limit is not None
                        and not qc.terminal_nudge_budget_extended
                    ):
                        qc.tool_call_limit += TERMINAL_NUDGE_BUDGET_BONUS
                        qc.terminal_nudge_budget_extended = True

                    nudge_prompt = build_terminal_nudge_text(
                        terminal_tools,
                        qc.terminal_nudge_retries_used,
                    )
                    previous_agent = agent
                    previous_qc = qc
                    agent = spawn_agent(
                        self.session_config,
                        messages=list(previous_agent.display_messages),
                        agent_def=effective_defn,
                        latest_user_prompt=nudge_prompt,
                        session_state=getattr(previous_qc, "session_state", None),
                        sandbox_id=self.sandbox_id,
                        terminal_tools=terminal_tools_raw,
                    )
                    _wire_agent(agent, previous_qc)
                    agent.query_context.on_turn = _on_turn
                    state.agent = agent
                    await _run_agent_once(nudge_prompt)
                    _carry_forward_usage(previous_agent, agent)
            except asyncio.CancelledError:
                state.cancelled = True
                state.error = "cancelled"
                raise
            except Exception as exc:
                state.error = str(exc)
                logger.exception("team agent %s crashed", effective_defn.name)
                raise

            qc = agent.query_context
            terminal_tools = set(qc.terminal_tools or set())
            if terminal_tools and qc.exit_reason != QueryExitReason.TOOL_STOP:
                logger.warning(
                    "Agent %s did not submit for %s",
                    effective_defn.name,
                    work_item_id,
                )
                ctx.tool_metadata["task_summary"] = (
                    "Agent did not call a terminal submission tool."
                )
                ctx.tool_metadata["task_summary_type"] = "request_replan"
        finally:
            if checkpoint_task is not None:
                await asyncio.gather(checkpoint_task, return_exceptions=True)
            state.final_text = extract_final_text(agent.display_messages)
            if state.final_text:
                ctx.tool_metadata["work_result"] = state.final_text
            if self.on_complete is not None:
                await self.on_complete(state)

        return {
            "agent": effective_defn.name,
            "final_text": state.final_text,
            "team_run_id": team_run_id,
            "work_item_id": work_item_id,
            "agent_run_id": ctx.tool_metadata.get("agent_run_id"),
        }
