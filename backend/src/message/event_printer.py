"""Shared stream-event printer for single- and multi-agent runs.

Mirrors the dense column-aligned log style used by the e2e conftest's
eval harness (``tests/test_e2e/conftest.py``) but keyed on
``(agent_name, work_id)`` so concurrent agents can coexist without
interleaving mid-sentence.

Key ideas:

- **Per-agent turn buffers.** ``ThinkingDelta`` / ``AssistantTextDelta``
  events from different agents are buffered independently and printed
  once per completed assistant turn, so two workers streaming at once
  don't clobber each other's prose and the console shows full blocks.
- **Lineage via bg task_id.** A ``BackgroundTaskStarted`` whose
  ``tool_name == "run_subagent"`` is treated as a spawn; its
  ``task_id`` becomes the child's work_id so the child's own events
  indent one level deeper than the dispatching parent.
- **Color per agent.** Each distinct ``agent_name`` is assigned a
  stable ANSI color from an 8-color palette (deterministic via hash)
  so the eye can follow one worker down a wall of output.
- **Summary.** ``summary()`` returns per-agent counts (tool calls,
  subagents spawned) plus totals for a closing one-liner.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any

from message.stream_events import (
    AssistantTextDelta,
    AssistantTurnComplete,
    BackgroundTaskCompleted,
    BackgroundTaskStarted,
    StreamEvent,
    SystemNotification,
    ThinkingDelta,
    ToolExecutionCancelled,
    ToolExecutionCompleted,
    ToolExecutionProgress,
    ToolExecutionStarted,
)


_PALETTE = (
    "\033[36m",  # cyan
    "\033[33m",  # yellow
    "\033[35m",  # magenta
    "\033[32m",  # green
    "\033[34m",  # blue
    "\033[91m",  # bright red
    "\033[94m",  # bright blue
    "\033[95m",  # bright magenta
)
_RESET = "\033[0m"
_DIM = "\033[2m"
_RED = "\033[31m"
_GREEN = "\033[32m"


def _truncate(text: str, limit: int | None) -> str:
    text = text or ""
    if limit is None:
        return text
    return text if len(text) <= limit else text[: limit - 1] + "…"


@dataclass
class _AgentTotals:
    color: str = ""
    tool_calls: int = 0
    subagents_spawned: int = 0


@dataclass
class _LaneState:
    thinking_buf: list[str] = field(default_factory=list)
    text_buf: list[str] = field(default_factory=list)


class MultiAgentEventPrinter:
    """Format and print ``StreamEvent``s to stdout (or any sink).

    Pass the result of a ``run_query`` stream into :meth:`emit` per event.
    Buffers thinking/text deltas per-agent and prints them once when
    that same agent completes an assistant turn.
    """

    def __init__(
        self,
        *,
        color: bool = True,
        tag_width: int = 14,
        truncate: int | None = 500,
        sink: "Any" = None,
        timestamps: bool = False,
    ) -> None:
        import time as _time

        self._color = color
        self._tag_width = tag_width
        self._truncate_n = truncate
        self._sink = sink  # callable taking a line; default = print
        self._timestamps = timestamps
        self._start = _time.monotonic()
        self._agent_totals: dict[str, _AgentTotals] = {}
        self._lanes: dict[tuple[str, str], _LaneState] = {}
        self._depth: dict[str, int] = {}  # work_id -> depth
        self._work_to_agent: dict[str, str] = {}  # work_id -> agent_name
        self._palette_idx = 0

    # ------------------------------------------------------------------
    # Public API
    # ------------------------------------------------------------------

    def emit(self, event: StreamEvent) -> None:
        agent = getattr(event, "agent_name", "") or "?"
        work_id = getattr(event, "work_id", "")
        totals = self._agent_totals_for(agent)
        lane = self._lane_for(agent, work_id)

        # Stream deltas into per-agent buffers; do not print yet.
        if isinstance(event, ThinkingDelta):
            lane.thinking_buf.append(event.text)
            return
        if isinstance(event, AssistantTextDelta):
            lane.text_buf.append(event.text)
            return

        if isinstance(event, ToolExecutionStarted):
            totals.tool_calls += 1
            self._line(
                agent,
                work_id,
                f"-> tool_start: {event.tool_name}"
                f"({_truncate(str(event.tool_input), 120)})",
            )
        elif isinstance(event, ToolExecutionCompleted):
            status = self._c("red", "ERROR") if event.is_error else self._c("green", "ok")
            limit = 500 if event.is_error else 120
            self._line(
                agent,
                work_id,
                f"<- tool_done:  {event.tool_name} [{status}] "
                f"{_truncate(event.output, limit)}",
            )
        elif isinstance(event, ToolExecutionProgress):
            self._line(
                agent,
                work_id,
                f".. progress:   {event.tool_name} {_truncate(event.output, 120)}",
            )
        elif isinstance(event, ToolExecutionCancelled):
            self._line(
                agent,
                work_id,
                f"x  cancelled:  {event.tool_name} {_truncate(event.reason, 240)}",
            )
        elif isinstance(event, BackgroundTaskStarted):
            # run_subagent is a regular background tool — the only thing that
            # makes it a "spawn" is its name. Treat it specially so the printed
            # log reads as team coordination rather than generic bg plumbing.
            if event.tool_name == "run_subagent":
                totals.subagents_spawned += 1
                child = str(event.tool_input.get("agent_name") or "subagent")
                task_text = str(event.tool_input.get("prompt") or event.tool_input.get("task_note") or "")
                # Record lineage so the child's own events indent one level
                # deeper when they arrive (keyed on bg task_id = child work_id).
                parent_depth = self._depth.get(work_id, 0) if work_id else 0
                self._depth[event.task_id] = parent_depth + 1
                self._work_to_agent[event.task_id] = child
                self._line(
                    agent,
                    work_id,
                    f"~> spawn:      {self._c('bold', child)} "
                    f"task_id={event.task_id} task={_truncate(task_text, 120)}",
                )
            else:
                self._line(
                    agent,
                    work_id,
                    f">> bg_start:   {event.tool_name} task_id={event.task_id}",
                )
        elif isinstance(event, BackgroundTaskCompleted):
            status = self._c("red", "ERROR") if event.is_error else self._c("green", "ok")
            limit = 500 if event.is_error else 120
            if event.tool_name == "run_subagent":
                child = self._work_to_agent.get(event.task_id, "subagent")
                self._line(
                    agent,
                    work_id,
                    f"<~ return:     {self._c('bold', child)} "
                    f"task_id={event.task_id} [{status}] "
                    f"{_truncate(event.output, limit)}",
                )
            else:
                self._line(
                    agent,
                    work_id,
                    f"<< bg_done:    {event.tool_name} [{status}] "
                    f"{_truncate(event.output, limit)}",
                )
        elif isinstance(event, AssistantTurnComplete):
            # Print full thinking/text blocks once per completed turn.
            self._flush_buffers(agent, work_id)
        elif isinstance(event, SystemNotification):
            tag = f"[system{':' + event.category if event.category else ''}]"
            if self._truncate_n is None:
                limit = None
            elif event.category == "background_progress":
                limit = None
            elif event.category == "budget_warning":
                limit = 400
            else:
                limit = 200
            self._line(agent, work_id, f"{tag} {_truncate(event.text, limit)}")

    def raw_line(self, agent: str, body: str) -> None:
        """Print a free-form line with the same column/tag/color treatment.

        Used by callers that don't produce ``StreamEvent``s (e.g. the sweevo
        CLI tailing pytest output in a sandbox) so the visual style stays
        consistent with agent-driven runs.
        """
        self._flush_buffers(agent)
        self._line(agent, "", body)

    def flush(self) -> None:
        for agent, work_id in list(self._lanes):
            self._flush_buffers(agent, work_id)

    def summary(self) -> dict[str, Any]:
        per_agent = {
            name: {
                "tool_calls": st.tool_calls,
                "subagents_spawned": st.subagents_spawned,
            }
            for name, st in self._agent_totals.items()
        }
        totals = {
            "agents": len(self._agent_totals),
            "tool_calls": sum(st.tool_calls for st in self._agent_totals.values()),
            "subagents_spawned": sum(
                st.subagents_spawned for st in self._agent_totals.values()
            ),
        }
        return {"per_agent": per_agent, "totals": totals}

    # ------------------------------------------------------------------
    # Internals
    # ------------------------------------------------------------------

    def _agent_totals_for(self, agent: str) -> _AgentTotals:
        st = self._agent_totals.get(agent)
        if st is None:
            color = _PALETTE[self._palette_idx % len(_PALETTE)] if self._color else ""
            self._palette_idx += 1
            st = _AgentTotals(color=color)
            self._agent_totals[agent] = st
        return st

    def _lane_for(self, agent: str, work_id: str) -> _LaneState:
        key = (agent, work_id or "")
        lane = self._lanes.get(key)
        if lane is None:
            self._agent_totals_for(agent)
            lane = _LaneState()
            self._lanes[key] = lane
        return lane

    def _flush_lane(self, agent: str, work_id: str) -> None:
        lane = self._lanes.get((agent, work_id or ""))
        if lane is None:
            return
        if lane.thinking_buf:
            self._line(
                agent,
                work_id,
                f"[thinking] {_truncate(''.join(lane.thinking_buf), self._truncate_n)}",
            )
            lane.thinking_buf.clear()
        if lane.text_buf:
            self._line(
                agent,
                work_id,
                f"[text] {_truncate(''.join(lane.text_buf), self._truncate_n)}",
            )
            lane.text_buf.clear()

    def _flush_buffers(self, agent: str, work_id: str = "") -> None:
        if work_id:
            self._flush_lane(agent, work_id)
            return
        for lane_agent, lane_work_id in list(self._lanes):
            if lane_agent == agent:
                self._flush_lane(lane_agent, lane_work_id)

    def _line(self, agent: str, work_id: str, body: str) -> None:
        import time as _time

        depth = self._depth.get(work_id, 0) if work_id else 0
        indent = "  " * depth
        tag = self._agent_tag(agent, work_id)
        if self._timestamps:
            elapsed = _time.monotonic() - self._start
            stamp = f"{_DIM}[{elapsed:7.1f}s]{_RESET}" if self._color else f"[{elapsed:7.1f}s]"
            line = f"{stamp} {tag} {indent}{body}"
        else:
            line = f"{tag} {indent}{body}"
        if self._sink is not None:
            self._sink(line)
        else:
            print(line, flush=True)

    def _agent_tag(self, agent: str, work_id: str = "") -> str:
        st = self._agent_totals_for(agent)
        name = agent[: self._tag_width].ljust(self._tag_width)
        raw = f"[{name}]"
        if work_id:
            raw += f" [{self._format_work_id(work_id)}]"
        if self._color and st.color:
            return f"{st.color}{raw}{_RESET}"
        return raw

    def _format_work_id(self, work_id: str) -> str:
        return work_id

    def _c(self, key: str, text: str) -> str:
        if not self._color:
            return text
        code = {
            "red": _RED,
            "green": _GREEN,
            "bold": "\033[1m",
        }.get(key, "")
        return f"{code}{text}{_RESET}" if code else text
