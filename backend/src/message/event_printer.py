"""Shared stream-event printer for single- and multi-agent runs.

Mirrors the dense column-aligned log style used by the e2e conftest's
eval harness (``tests/test_e2e/conftest.py``) but keyed on
``(agent_name, run_id)`` so concurrent agents can coexist without
interleaving mid-sentence.

Key ideas:

- **Per-agent message buffers.** ``ThinkingDelta`` / ``AssistantTextDelta``
  events from different agents are buffered independently and printed
  once per completed assistant message, so two workers streaming at once
  don't clobber each other's prose and the console shows full blocks.
- **Lineage via bg task_id.** A ``BackgroundTaskStarted`` whose
  ``tool_name == "run_subagent"`` is treated as a spawn; its
  ``task_id`` becomes the child's run_id so the child's own events
  indent one level deeper than the dispatching parent.
- **Color per agent.** Each distinct ``agent_name`` is assigned a
  stable ANSI color from an 8-color palette (deterministic via hash)
  so the eye can follow one worker down a wall of output.
- **Summary.** ``summary()`` returns per-agent counts (tool calls,
  subagents spawned) plus totals for a closing one-liner.
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field
from typing import Any

from message.stream_events import (
    AssistantMessageComplete,
    AssistantTextDelta,
    BackgroundTaskCompleted,
    BackgroundTaskStarted,
    StreamEvent,
    ThinkingDelta,
    ToolExecutionCancelled,
    ToolExecutionCompleted,
    ToolExecutionProgress,
    ToolExecutionStarted,
)
from notification.events import SystemNotification


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
_YELLOW = "\033[33m"
_BLUE = "\033[34m"
_MAGENTA = "\033[35m"
_CYAN = "\033[36m"


def _full_text(text: str) -> str:
    text = text or ""
    return text



def _parse_shell_structured_error(
    tool_name: str,
    output: str,
    is_error: bool,
) -> dict[str, Any] | None:
    if tool_name != "shell" or not is_error:
        return None
    try:
        payload = json.loads(output)
    except json.JSONDecodeError:
        return None
    if (
        isinstance(payload, dict)
        and payload.get("status") == "error"
        and "cwd" in payload
        and {"command", "exit_code"} <= set(payload)
    ):
        return payload
    return None


def _append_detail(lines: list[str], value: object) -> None:
    text = str(value or "").strip()
    if text and text not in lines:
        lines.append(text)


def _format_shell_structured_error(payload: dict[str, Any]) -> str:
    lines: list[str] = []
    command = str(payload.get("command") or "").strip()
    exit_code = payload.get("exit_code", "?")
    if command:
        _append_detail(lines, f"$ {command} -> exit {exit_code}")
    stderr = str(payload.get("stderr") or "").strip()
    stdout = str(payload.get("stdout") or "").strip()
    if stderr and stderr != stdout:
        _append_detail(lines, stderr)
    elif stdout:
        _append_detail(lines, stdout)

    _append_detail(lines, payload.get("error"))
    _append_detail(lines, payload.get("script_stdout"))

    warnings = payload.get("warnings")
    if isinstance(warnings, list):
        for warning in warnings:
            _append_detail(lines, f"warning: {warning}")

    if lines:
        return "\n".join(lines)
    changed = payload.get("changed_paths")
    if isinstance(changed, list):
        return f"status=error changed_paths={len(changed)}"
    return "status=error"


def _format_tool_completion_output(
    *,
    tool_name: str,
    output: str,
    is_error: bool,
) -> str:
    payload = _parse_shell_structured_error(tool_name, output, is_error)
    if payload is not None:
        output = _format_shell_structured_error(payload)
    rendered = output or ""
    return f" {rendered}" if rendered else ""


def _json_log_value(value: object) -> str:
    return json.dumps(str(value), ensure_ascii=True)


def format_background_start_detail(tool_name: str, tool_input: dict[str, Any]) -> str:
    """Return compact launch context for background-start log lines."""
    if tool_name != "run_subagent":
        return ""

    parts: list[str] = []
    agent_name = tool_input.get("agent_name")
    if agent_name is not None:
        parts.append(f"agent_name={_json_log_value(agent_name)}")
    if "prompt" in tool_input:
        parts.append(f"prompt={_json_log_value(tool_input.get('prompt'))}")
    return f" {' '.join(parts)}" if parts else ""


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
    that same agent completes an assistant message.
    """

    def __init__(
        self,
        *,
        color: bool = True,
        tag_width: int = 14,
        sink: "Any" = None,
        timestamps: bool = False,
    ) -> None:
        import time as _time

        self._color = color
        self._tag_width = tag_width
        self._sink = sink  # callable taking a line; default = print
        self._timestamps = timestamps
        self._start = _time.monotonic()
        self._agent_totals: dict[str, _AgentTotals] = {}
        self._lanes: dict[tuple[str, str], _LaneState] = {}
        self._depth: dict[str, int] = {}  # run_id -> depth
        self._run_to_agent: dict[str, str] = {}  # run_id -> agent_name
        self._palette_idx = 0

    # ------------------------------------------------------------------
    # Public API
    # ------------------------------------------------------------------

    def emit(self, event: StreamEvent) -> None:
        agent = getattr(event, "agent_name", "") or "?"
        run_id = getattr(event, "run_id", "")
        totals = self._agent_totals_for(agent)
        lane = self._lane_for(agent, run_id)

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
                run_id,
                f"{self._c('cyan', '-> tool_start:')} {event.tool_name}"
                f"({event.tool_input})",
            )
        elif isinstance(event, ToolExecutionCompleted):
            status = self._c("red", "ERROR") if event.is_error else self._c("green", "ok")
            output = _format_tool_completion_output(
                tool_name=event.tool_name,
                output=event.output,
                is_error=event.is_error,
            )
            self._line(
                agent,
                run_id,
                f"{self._c('green' if not event.is_error else 'red', '<- tool_done:')}  {event.tool_name} [{status}]"
                f"{output}",
            )
        elif isinstance(event, ToolExecutionProgress):
            self._line(
                agent,
                run_id,
                f"{self._c('yellow', '.. progress:')}   {event.tool_name} {_full_text(event.output)}",
            )
        elif isinstance(event, ToolExecutionCancelled):
            self._line(
                agent,
                run_id,
                f"{self._c('red', 'x  cancelled:')}  {event.tool_name} {_full_text(event.reason)}",
            )
        elif isinstance(event, BackgroundTaskStarted):
            detail = format_background_start_detail(event.tool_name, event.tool_input)
            self._line(
                agent,
                run_id,
                f"{self._c('blue', '>> bg_start:')}   {event.tool_name} task_id={event.task_id}{detail}",
            )
        elif isinstance(event, BackgroundTaskCompleted):
            status = self._c("red", "ERROR") if event.is_error else self._c("green", "ok")
            output = _format_tool_completion_output(
                tool_name=event.tool_name,
                output=event.output,
                is_error=event.is_error,
            )
            self._line(
                agent,
                run_id,
                f"{self._c('blue', '<< bg_done:')}    {event.tool_name} [{status}]"
                f"{output}",
            )
        elif isinstance(event, AssistantMessageComplete):
            # Print full thinking/text blocks once per completed message.
            self._flush_buffers(agent, run_id)
        elif isinstance(event, SystemNotification):
            self._line(agent, run_id, f"[system] {_full_text(event.text)}")

    def raw_line(self, agent: str, body: str) -> None:
        """Print a free-form line with the same column/tag/color treatment.

        Used by callers that don't produce ``StreamEvent``s (e.g. the sweevo
        CLI tailing pytest output in a sandbox) so the visual style stays
        consistent with agent-driven runs.
        """
        self._flush_buffers(agent)
        self._line(agent, "", body)

    def flush(self) -> None:
        for agent, run_id in list(self._lanes):
            self._flush_buffers(agent, run_id)

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

    def _lane_for(self, agent: str, run_id: str) -> _LaneState:
        key = (agent, run_id or "")
        lane = self._lanes.get(key)
        if lane is None:
            self._agent_totals_for(agent)
            lane = _LaneState()
            self._lanes[key] = lane
        return lane

    def _flush_lane(self, agent: str, run_id: str) -> None:
        lane = self._lanes.get((agent, run_id or ""))
        if lane is None:
            return
        if lane.thinking_buf:
            self._line(
                agent,
                run_id,
                f"[thinking] {_full_text(''.join(lane.thinking_buf))}",
            )
            lane.thinking_buf.clear()
        if lane.text_buf:
            self._line(
                agent,
                run_id,
                f"[text] {_full_text(''.join(lane.text_buf))}",
            )
            lane.text_buf.clear()

    def _flush_buffers(self, agent: str, run_id: str = "") -> None:
        if run_id:
            self._flush_lane(agent, run_id)
            return
        for lane_agent, lane_run_id in list(self._lanes):
            if lane_agent == agent:
                self._flush_lane(lane_agent, lane_run_id)

    def _line(self, agent: str, run_id: str, body: str) -> None:
        import time as _time

        depth = self._depth.get(run_id, 0) if run_id else 0
        indent = "  " * depth
        tag = self._agent_tag(agent, run_id)
        lines = body.splitlines() or [""]
        continuation = self._c("dim", "│ ") if self._color else "│ "
        for idx, segment in enumerate(lines):
            prefix = continuation if idx else ""
            if self._timestamps:
                elapsed = _time.monotonic() - self._start
                stamp = f"{_DIM}[{elapsed:7.1f}s]{_RESET}" if self._color else f"[{elapsed:7.1f}s]"
                line = f"{stamp} {tag} {indent}{prefix}{segment}"
            else:
                line = f"{tag} {indent}{prefix}{segment}"
            if self._sink is not None:
                self._sink(line)
            else:
                print(line, flush=True)

    def _agent_tag(self, agent: str, run_id: str = "") -> str:
        st = self._agent_totals_for(agent)
        name = agent[: self._tag_width].ljust(self._tag_width)
        raw = f"[{name}]"
        if run_id:
            raw += f" [{self._format_run_id(run_id)}]"
        if self._color and st.color:
            return f"{st.color}{raw}{_RESET}"
        return raw

    def _format_run_id(self, run_id: str) -> str:
        return run_id

    def _c(self, key: str, text: str) -> str:
        if not self._color:
            return text
        code = {
            "red": _RED,
            "green": _GREEN,
            "yellow": _YELLOW,
            "blue": _BLUE,
            "magenta": _MAGENTA,
            "cyan": _CYAN,
            "dim": _DIM,
            "bold": "\033[1m",
        }.get(key, "")
        return f"{code}{text}{_RESET}" if code else text
