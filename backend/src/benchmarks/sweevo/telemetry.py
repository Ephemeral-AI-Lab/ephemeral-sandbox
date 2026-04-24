"""Benchmark telemetry for :class:`TeamAgentRunner` (SweeVO harness).

Bundles the per-agent and per-run observability code used by the SweeVO
benchmark runner: printer banners, structured JSONL events, token/compaction
stats, external-hook formatting, and the default ``on_complete`` /
``on_event`` / ``on_spawned`` callbacks for :class:`TeamAgentRunner`.

Callers instantiate :class:`BenchmarkTelemetry` and pass its methods into
:class:`TeamAgentRunner`. Benchmark-specific work (domain prompts, repo
snapshot capture, custom budget logic) stays in the caller.
"""

from __future__ import annotations

import json
import logging
import re
from collections import Counter
from collections.abc import Callable
from dataclasses import asdict, dataclass, field, is_dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from message.messages import ConversationMessage, ToolUseBlock
from message.stream_events import ToolExecutionCompleted
from team.runtime.runner import AgentRunState

logger = logging.getLogger(__name__)
_SAFE_AGENT_LOG_NAME_RE = re.compile(r"[^A-Za-z0-9_.-]+")


def utc_iso_now() -> str:
    return datetime.now(timezone.utc).isoformat()


def append_event(team_metrics: dict[str, Any] | None, event: dict[str, Any]) -> None:
    """Append one JSONL line to the structured log declared in ``team_metrics``."""
    if not team_metrics:
        return
    path_value = team_metrics.get("structured_log_path")
    if not path_value:
        return
    path = Path(str(path_value))
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a", encoding="utf-8") as handle:
        handle.write(json.dumps({"ts": utc_iso_now(), **event}, default=str) + "\n")


def tool_names_from_messages(messages: list[ConversationMessage]) -> list[str]:
    names: list[str] = []
    for msg in messages:
        for block in getattr(msg, "content", []):
            if isinstance(block, ToolUseBlock):
                names.append(block.name)
    return names


def background_tool_names_from_messages(
    messages: list[ConversationMessage],
) -> list[str]:
    names: list[str] = []
    for msg in messages:
        for block in getattr(msg, "content", []):
            if (
                isinstance(block, ToolUseBlock)
                and isinstance(block.input, dict)
                and block.input.get("background") is True
            ):
                names.append(block.name)
    return names


def estimate_final_context(messages: list[ConversationMessage] | None) -> int:
    """Best-effort token estimate for the final compacted provider context."""
    if not messages:
        return 0
    try:
        from compaction import estimate_message_tokens

        return estimate_message_tokens(messages)
    except Exception:
        logger.debug("Failed to estimate final compacted context", exc_info=True)
        return 0


def persist_session_snapshot(*, session_config: Any, agent: Any, summary_text: str) -> None:
    """Persist the agent's history into the shared session row (best-effort)."""
    try:
        from server.app_factory import session_store
    except Exception:
        return
    if session_store is None or not getattr(session_store, "is_ready", False):
        return
    qc = getattr(agent, "query_context", None)
    try:
        session_store.upsert(
            session_id=getattr(session_config, "session_id", ""),
            cwd=session_config.cwd,
            model=agent.model,
            system_prompt=getattr(qc, "system_prompt", None),
            messages=[m.model_dump(mode="json") for m in agent.display_messages],
            full_messages=[m.model_dump(mode="json") for m in agent.display_messages],
            usage=agent.total_usage.model_dump() if agent.total_usage else {},
            session_state=qc.session_state.to_dict()
            if qc is not None and getattr(qc, "session_state", None) is not None
            else None,
            summary=summary_text[:80],
            message_count=len(agent.display_messages),
        )
    except Exception:
        logger.debug("Failed to persist session snapshot", exc_info=True)


def format_external_hook_line(payload: dict[str, Any]) -> str:
    """Render an ``[external_hook]`` printer line from a hook payload."""
    hook = str(payload.get("hook") or "hook")
    parts = [f"[external_hook] {hook}"]
    task_id = str(payload.get("work_item_id") or "")[:8]
    if task_id:
        parts.append(f"task={task_id}")
    trigger = str(payload.get("trigger") or "")
    if trigger:
        parts.append(f"trigger={trigger}")
    answer = str(payload.get("answer") or "")
    if answer:
        parts.append(f"answer={answer}")
    parts.append(f"status={payload.get('status') or 'unknown'}")
    error = str(payload.get("error") or "")
    if error:
        parts.append(f"error={error}")
    return " ".join(parts)


def default_team_metrics() -> dict[str, Any]:
    """Build an empty team_metrics dict for a run."""
    return {
        "agent_runs": 0,
        "agent_counts": Counter(),
        "structured_log_path": None,
        "agent_run_log_dir": None,
        "agent_run_log_paths": [],
    }


def _safe_agent_log_part(value: object, fallback: str = "run") -> str:
    raw = str(value or "").strip()
    if not raw:
        return fallback
    return _SAFE_AGENT_LOG_NAME_RE.sub("_", raw).strip("._") or fallback


def _jsonable_model(value: Any) -> Any:
    model_dump = getattr(value, "model_dump", None)
    if callable(model_dump):
        try:
            return model_dump(mode="json", by_alias=True)
        except TypeError:
            return model_dump()
    if is_dataclass(value) and not isinstance(value, type):
        return asdict(value)
    return value


def _serialize_agent_definition(defn: Any) -> dict[str, Any]:
    dumped = _jsonable_model(defn)
    if isinstance(dumped, dict):
        return {str(k): v for k, v in dumped.items()}
    attrs = getattr(defn, "__dict__", None)
    if isinstance(attrs, dict):
        return {
            str(k): v
            for k, v in attrs.items()
            if not str(k).startswith("_") and not callable(v)
        }
    return {"repr": repr(defn)}


def _terminal_submission_payload(metadata: Any) -> dict[str, Any]:
    if metadata is None:
        return {}
    payload: dict[str, Any] = {}
    for key in ("task_summary_type", "task_summary", "plan_is_replan"):
        value = metadata.get(key)
        if value is not None:
            payload[key] = value
    resolved_plan = metadata.get("resolved_plan")
    if resolved_plan is not None:
        payload["resolved_plan"] = _jsonable_model(resolved_plan)
    return payload


def _serialize_message_list(messages: Any) -> list[Any]:
    if not messages:
        return []
    serialized: list[Any] = []
    for message in messages:
        serialized.append(_jsonable_model(message))
    return serialized


def write_agent_run_log(
    team_metrics: dict[str, Any] | None,
    state: AgentRunState,
    *,
    status: str,
    stats: dict[str, Any],
    prompt_tokens: int,
    completion_tokens: int,
    tool_names: list[str],
    bg_tool_names: list[str],
) -> str | None:
    """Persist a human-inspectable JSON artifact for one completed agent run."""
    if not team_metrics:
        return None
    dir_value = team_metrics.get("agent_run_log_dir")
    if not dir_value:
        return None

    try:
        log_dir = Path(str(dir_value))
        log_dir.mkdir(parents=True, exist_ok=True)
        ctx = state.ctx
        qc = getattr(state.agent, "query_context", None)
        agent_name = str(getattr(state.defn, "name", "") or "agent")
        agent_run_id = str(
            state.tracker.run_id
            or ctx.tool_metadata.get("agent_run_id")
            or "unpersisted"
        )
        work_item_id = str(
            ctx.tool_metadata.get("work_item_id") or state.work_item_id or "work-item"
        )
        time_prefix = datetime.now(timezone.utc).strftime("%Y-%m-%d-%H-%M")
        path = log_dir / (
            f"{time_prefix}_"
            f"{_safe_agent_log_part(agent_name, 'agent')}_"
            f"{_safe_agent_log_part(work_item_id)}.json"
        )
        prompt_usage = {
            "prompt_tokens": prompt_tokens,
            "completion_tokens": completion_tokens,
            "total_tokens": prompt_tokens + completion_tokens,
        }
        payload = {
            "schema_version": 1,
            "team_run_id": ctx.tool_metadata.get("team_run_id"),
            "work_item_id": ctx.tool_metadata.get("work_item_id"),
            "agent_run_id": agent_run_id,
            "agent": agent_name,
            "role": getattr(state.defn, "role", None),
            "status": status,
            "model": getattr(state.agent, "model", None),
            "started_at": ctx.tool_metadata.get("work_item_started_at"),
            "completed_at": utc_iso_now(),
            "agent_definition": _serialize_agent_definition(state.defn),
            "system_prompt": getattr(qc, "system_prompt", None),
            "user_prompt": ctx.user_message,
            "assistant_response": state.final_text,
            "terminal_submission": _terminal_submission_payload(ctx.tool_metadata),
            "usage": prompt_usage,
            "token_trackers": {
                **prompt_usage,
                **stats,
            },
            "tools": {
                "tool_names": tool_names,
                "tool_counts": dict(Counter(tool_names)),
                "background_tool_names": bg_tool_names,
                "background_tool_counts": dict(Counter(bg_tool_names)),
            },
            "display_messages": _serialize_message_list(
                getattr(state.agent, "display_messages", None)
            ),
            "api_messages": _serialize_message_list(
                getattr(qc, "api_messages_snapshot", None)
            ),
        }
        path.write_text(
            json.dumps(payload, indent=2, ensure_ascii=False, default=str) + "\n",
            encoding="utf-8",
        )
        paths = team_metrics.setdefault("agent_run_log_paths", [])
        if isinstance(paths, list):
            paths.append(str(path))
        return str(path)
    except Exception:
        logger.warning("Failed to write agent run log artifact", exc_info=True)
        return None


def make_external_hook_emitter(
    *,
    printer: Any = None,
    team_metrics: dict[str, Any] | None = None,
) -> Callable[[dict[str, Any]], None]:
    """Return a callable that logs + prints external_hook payloads."""

    def _emit(payload: dict[str, Any]) -> None:
        append_event(team_metrics, payload)
        if printer is not None:
            printer.raw_line("team", format_external_hook_line(payload))

    return _emit


def emit_planning_budget_banner(printer: Any, *, budgets: Any) -> None:
    """Print a ``[planning_budget] …`` banner."""
    if printer is None:
        return
    printer.raw_line(
        "team",
        f"[planning_budget] max_plan_size={budgets.max_plan_size} "
        f"max_depth={budgets.max_depth} max_tasks={budgets.max_tasks}",
    )


def emit_dispatcher_dag(printer: Any, team_run: Any, *, trigger_agent: str) -> None:
    """Print the current task-graph for debugging, sorted by depth/created/id."""
    if printer is None:
        return
    graph = team_run.task_center.graph
    printer.raw_line("team", f"[dag] after={trigger_agent} nodes={len(graph)}")
    for wi in sorted(graph.values(), key=lambda w: (w.depth, w.created_at, w.id)):
        deps = [d[:8] for d in wi.deps]
        printer.raw_line(
            "team",
            f"[dag] {wi.id[:8]} agent={wi.agent_name} status={wi.status.value} "
            f"depth={wi.depth} deps={deps or []}",
        )


@dataclass
class BenchmarkTelemetry:
    """Bundle of ``TeamAgentRunner`` hooks for standard benchmark observability.

    Fields:
      * ``printer`` — optional :class:`MultiAgentEventPrinter` for live output.
      * ``team_metrics`` — mutable dict; receives counters + structured events.
      * ``session_config`` — for :func:`persist_session_snapshot`.
      * ``banner_agent`` — agent name that gets [runtime_limits] / [runtime_budget].
      * ``success_hook`` — optional callback invoked on clean completion.
      * ``extra_event_fields`` — extra fields merged into every agent_complete event.
    """

    printer: Any = None
    team_metrics: dict[str, Any] | None = None
    session_config: Any = None
    banner_agent: str = ""
    success_hook: Callable[[AgentRunState], None] | None = None
    extra_event_fields: dict[str, Any] = field(default_factory=dict)

    # ------------------------------------------------------------------
    # TeamAgentRunner hooks

    def on_spawned(self, state: AgentRunState) -> None:
        if self.printer is None or state.defn.name != self.banner_agent:
            return
        self.printer.raw_line(
            state.defn.name,
            f"[runtime_limits] tool_call_limit={state.agent.query_context.tool_call_limit}",
        )

    def on_event(self, event: Any, state: AgentRunState) -> None:
        if self.printer is None:
            return
        try:
            object.__setattr__(event, "agent_name", state.defn.name)
        except Exception:
            pass
        try:
            self.printer.emit(event)
        except Exception:
            logger.debug("printer.emit failed", exc_info=True)
        if state.defn.name == self.banner_agent and isinstance(event, ToolExecutionCompleted):
            self.printer.raw_line(
                state.defn.name,
                (
                    "[runtime_budget] "
                    f"used={state.agent.query_context.tool_calls_used} "
                    f"limit={state.agent.query_context.tool_call_limit}"
                ),
            )

    async def on_complete(self, state: AgentRunState) -> None:
        agent, ctx = state.agent, state.ctx
        qc = getattr(agent, "query_context", None)
        session_state = getattr(qc, "session_state", None)
        compacted_total = int(getattr(session_state, "compacted", 0) or 0)
        new_compactions = (
            compacted_total - state.compacted_before
            if session_state is not None and state.compacted_before is not None
            else 0
        )
        display_messages = list(agent.display_messages)
        tool_names = tool_names_from_messages(display_messages)
        bg_tool_names = background_tool_names_from_messages(display_messages)
        prompt_tokens = int(getattr(agent.total_usage, "input_tokens", 0) or 0)
        completion_tokens = int(getattr(agent.total_usage, "output_tokens", 0) or 0)
        stats = {
            "tool_calls_used": int(getattr(qc, "tool_calls_used", 0) or 0),
            "tool_call_limit": getattr(qc, "tool_call_limit", None),
            "final_context_tokens": estimate_final_context(
                getattr(qc, "api_messages_snapshot", None)
            ),
            "compactions_added": new_compactions,
            "compacted": compacted_total,
        }
        status = "cancelled" if state.cancelled else "failed" if state.error else "completed"

        state.tracker.finish(
            status=status,
            display_messages=display_messages,
            api_messages_snapshot=getattr(qc, "api_messages_snapshot", None),
            response={"final_text": state.final_text, **stats},
            error=state.error,
            final_text=state.final_text,
            event_count=0,
        )
        persist_session_snapshot(
            session_config=self.session_config,
            agent=agent,
            summary_text=state.final_text or ctx.user_message or "",
        )
        try:
            from server.app_factory import usage_store
            from token_tracker.runtime import persist_run_usage
        except Exception:
            usage_store = None
        if usage_store is not None:
            persist_run_usage(
                usage_store=usage_store,
                session_id=getattr(self.session_config, "session_id", None),
                run_id=state.tracker.run_id,
                agent_name=state.defn.name,
                model_id=agent.model,
                usage=agent.total_usage,
            )

        agent_run_log_path = write_agent_run_log(
            self.team_metrics,
            state,
            status=status,
            stats=stats,
            prompt_tokens=prompt_tokens,
            completion_tokens=completion_tokens,
            tool_names=tool_names,
            bg_tool_names=bg_tool_names,
        )

        if self.printer is not None:
            line = (
                f"[usage] prompt={prompt_tokens} "
                f"completion={completion_tokens} "
                f"total={prompt_tokens + completion_tokens} "
                f"tool_calls={stats['tool_calls_used']}"
            )
            if stats["tool_call_limit"] is not None:
                line += f"/{stats['tool_call_limit']}"
            line += f" final_context={stats['final_context_tokens']}"
            if bg_tool_names:
                bg = ", ".join(f"{n}={c}" for n, c in sorted(Counter(bg_tool_names).items()))
                line += f" background_tools={bg}"
            if state.compacted_before is not None:
                delta = f"+{new_compactions}" if new_compactions > 0 else str(new_compactions)
                line += f" compactions={delta}(total={compacted_total})"
            self.printer.raw_line(state.defn.name, line)
            if agent_run_log_path:
                self.printer.raw_line(
                    state.defn.name,
                    f"[agent_run_log] path={agent_run_log_path}",
                )

        append_event(
            self.team_metrics,
            {
                "event": "agent_complete",
                "team_run_id": ctx.tool_metadata.get("team_run_id"),
                "work_item_id": ctx.tool_metadata.get("work_item_id"),
                "agent_run_id": state.tracker.run_id,
                "agent": state.defn.name,
                "status": status,
                "prompt_tokens": prompt_tokens,
                "completion_tokens": completion_tokens,
                "total_tokens": prompt_tokens + completion_tokens,
                "tool_names": tool_names,
                "tool_counts": dict(Counter(tool_names)),
                "background_tool_names": bg_tool_names,
                "background_tool_counts": dict(Counter(bg_tool_names)),
                "agent_run_log_path": agent_run_log_path,
                **stats,
                **self.extra_event_fields,
            },
        )

        if state.error is None:
            if self.success_hook is not None:
                self.success_hook(state)
            if self.team_metrics is not None:
                self.team_metrics["agent_runs"] = int(self.team_metrics.get("agent_runs", 0)) + 1
                self.team_metrics.setdefault("agent_counts", Counter())[state.defn.name] += 1


def finalize_team_run(
    *,
    tr: Any,
    session_config: Any,
    team_metrics: dict[str, Any],
    budgets: Any,
    printer: Any = None,
) -> dict[str, Any]:
    """Emit the closing banners + team_result event and return a result dict.

    Generic team-run summary. Works with any TeamRun that exposes ``task_center``,
    ``budget_state``, ``id``, ``sandbox_id``, ``status`` (enum with ``.value``).
    """
    from team.core.models import TeamRunStatus

    status = tr.status
    task_count = len(tr.task_center.graph)
    logger.info(
        "team run %s finished: status=%s tasks=%d",
        tr.id, getattr(status, "value", status), task_count,
    )
    if status != TeamRunStatus.SUCCEEDED:
        for wi in tr.task_center.graph.values():
            if wi.status.value != "failed":
                continue
            logger.warning(
                "failed task: id=%s agent=%s reason=%s",
                wi.id, wi.agent_name, wi.failure_reason,
            )
            if printer is not None:
                printer.raw_line(
                    "team",
                    f"[failed_task] agent={wi.agent_name} id={wi.id[:8]} "
                    f"reason={wi.failure_reason or 'unknown'}",
                )

    max_depth = max((wi.depth for wi in tr.task_center.graph.values()), default=0)
    replans = int(getattr(tr.budget_state, "replans_used", 0) or 0)
    usage_summary = None
    usage_by_model: list[dict[str, Any]] = []
    try:
        from server.app_factory import usage_store

        if usage_store is not None and getattr(usage_store, "is_ready", False):
            usage_summary = usage_store.get_session_usage(session_config.session_id)
            usage_by_model = usage_store.get_usage_by_model(session_config.session_id)
    except Exception:
        logger.debug("Failed to load token usage summary", exc_info=True)

    if printer is not None and usage_summary is not None:
        printer.raw_line(
            "team",
            f"[team_usage] prompt={usage_summary['prompt_tokens']} "
            f"completion={usage_summary['completion_tokens']} "
            f"total={usage_summary['total_tokens']} "
            f"run_rows={usage_summary.get('run_count', usage_summary.get('call_count', 0))}",
        )
        printer.raw_line(
            "team",
            f"[team_stats] tasks={task_count} max_depth={max_depth} "
            f"agent_runs={team_metrics['agent_runs']} "
            f"replans={replans}",
        )

    common = {
        "team_name": team_metrics.get("team_name"),
        "team_run_id": tr.id,
        "sandbox_id": tr.sandbox_id,
        "session_id": session_config.session_id,
        "work_items": task_count,
        "max_depth_reached": max_depth,
        "agent_runs": int(team_metrics["agent_runs"]),
        "agent_counts": dict(team_metrics["agent_counts"]),
        "replans_used": replans,
        "usage": usage_summary,
        "usage_by_model": usage_by_model,
        "agent_run_log_dir": team_metrics.get("agent_run_log_dir"),
        "agent_run_log_paths": list(team_metrics.get("agent_run_log_paths") or []),
    }
    append_event(
        team_metrics,
        {"event": "team_result", "status": getattr(status, "value", status), **common},
    )
    return {
        "status": status,
        "structured_log_path": team_metrics.get("structured_log_path"),
        "budgets": {
            "max_tasks": budgets.max_tasks,
            "max_depth": budgets.max_depth,
            "max_plan_size": budgets.max_plan_size,
        },
        **common,
    }
