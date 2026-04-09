"""run_subagent — spawn a focused worker subagent as a background task.

The subagent is built and executed via the same `spawn_agent` /
`EphemeralAgent` machinery used for top-level agents. The only difference is
the loaded `AgentDefinition` carries `agent_type="subagent"`, which causes
the engine to:
  - skip registering the background-management toolkit (subagents cannot
    launch their own background tasks),
  - use the subagent's focused-worker system prompt.

The tool is declared with ``background="always"``, so the engine ALWAYS
dispatches it as a background task regardless of LLM input. The parent
peeks at live progress (up to ``PEEK_MESSAGE_MAX`` trailing messages) via
``check_background_progress`` — that calls into the progress provider this
tool registers on the ``BackgroundTaskManager``.

The subagent's run is persisted to ``agent_run_store`` with ``parent_run_id``
+ ``parent_task_id`` set, so the parent can later list its workers, audit
their message history, and retry failed runs.
"""

from __future__ import annotations

import asyncio
import json
import logging
from typing import Any

from agents.run_tracker import AgentRunTracker
from message.messages import (
    ConversationMessage,
    TextBlock,
    ThinkingBlock,
    ToolResultBlock,
    ToolUseBlock,
)
from token_tracker.runtime import persist_run_usage
from tools.core.base import ToolExecutionContext, ToolResult
from tools.core.decorator import tool

logger = logging.getLogger(__name__)


# Hard upper bound on the peek window — even if a caller (e.g. via the
# `last_n` parameter on check_background_progress) requests more, the
# subagent peek clamps to this so the parent's peek response stays bounded.
PEEK_MESSAGE_MAX = 10
# Per-block character cap inside the peek view.
_PEEK_BLOCK_CHAR_CAP = 200
# Total character cap for the peek view.
_PEEK_TOTAL_CHAR_CAP = 2048
_SCOUT_ONLY_CALLERS = frozenset({"team_planner", "atlas_builder", "atlas_refresher"})


def _truncate(s: str) -> str:
    s = s.replace("\n", " ").strip()
    if len(s) > _PEEK_BLOCK_CHAR_CAP:
        return s[: _PEEK_BLOCK_CHAR_CAP - 1] + "…"
    return s


def _compact_args(inp: Any) -> str:
    try:
        s = json.dumps(inp, separators=(",", ":"), default=str)
    except Exception:
        s = str(inp)
    return _truncate(s)


def _normalize_target_paths(value: Any) -> list[str]:
    if isinstance(value, str):
        stripped = value.strip()
        return [stripped] if stripped else []
    if not isinstance(value, list):
        return []
    out: list[str] = []
    for item in value:
        if isinstance(item, str):
            stripped = item.strip()
            if stripped:
                out.append(stripped)
    return out


def _already_covered_scout_targets(context: ToolExecutionContext, target_paths: list[str]) -> list[str]:
    metadata = context.metadata
    if str(metadata.get("agent_name") or "") in {"atlas_builder", "atlas_refresher"}:
        return []
    prior_scouts = set(_normalize_target_paths(metadata.get("_scout_target_paths_this_turn", [])))
    return [path for path in target_paths if path in prior_scouts]


def _render_block(block: Any) -> str:
    """One-line render of a single content block."""
    if isinstance(block, TextBlock):
        return f"[text] {_truncate(block.text)}"
    if isinstance(block, ThinkingBlock):
        return f"[think] {_truncate(block.text)}"
    if isinstance(block, ToolUseBlock):
        return f"[tool] {block.name}({_compact_args(block.input)})"
    if isinstance(block, ToolResultBlock):
        return f"[result] {_truncate(str(block.content))}"
    return ""


def format_last_n_messages(messages: list[ConversationMessage], n: int) -> str:
    """Render the last *n* messages of a subagent for the parent's peek view.

    *n* is hard-clamped to ``PEEK_MESSAGE_MAX`` so a runaway caller cannot
    blow the parent's peek-response budget.
    """
    if not messages:
        return "(no messages yet)"
    n = min(n, PEEK_MESSAGE_MAX)
    tail = messages[-n:]
    rendered: list[str] = []
    for msg in tail:
        prefix = "U:" if msg.role == "user" else "A:"
        for block in msg.content:
            line = _render_block(block)
            if line:
                rendered.append(f"{prefix} {line}")
    if not rendered:
        return "(no renderable content yet)"
    out = "\n".join(rendered)
    if len(out) > _PEEK_TOTAL_CHAR_CAP:
        out = "…" + out[-(_PEEK_TOTAL_CHAR_CAP - 1) :]
    return out


_ENVELOPE_SUMMARY_CAP = 500


def _extract_submitted_output(agent: Any) -> Any | None:
    """Return the agent's accepted submitted output, if any."""
    qc = getattr(agent, "query_context", None)
    if qc is None or qc.tool_metadata is None:
        return None
    for key in ("submitted_summary", "submitted_plan"):
        value = qc.tool_metadata.get(key)
        if value is not None:
            return value
    return None


def _build_subagent_envelope(
    submitted: Any | None,
    sub_run_id: str | None,
    final_text: str,
) -> dict[str, Any]:
    """Project the subagent submission into the typed envelope.

    Per plan §7:
        - ``Plan``               → kind="plan",    payload={items, rationale}
        - ``SubmittedSummary``   → kind="summary", artifact_ref present if
                                   the submitter set one; if its artifact
                                   carries a ``target_paths`` field, kind is
                                   promoted to ``"brief"``.
        - no posthook submission → kind="raw",    payload={"final_text": ...}
    """
    # Lazy imports — avoid pulling team into tools at import time.
    from team.models import Plan
    from tools.posthook import SubmittedSummary

    if isinstance(submitted, Plan):
        return {
            "kind": "plan",
            "summary": (
                f"submitted {len(submitted.items)} work item(s)"
                if submitted.items
                else "empty plan"
            )[:_ENVELOPE_SUMMARY_CAP],
            "artifact_ref": None,
            "payload": {
                "items": [
                    {
                        "agent_name": it.agent_name,
                        "local_id": it.local_id,
                        "kind": it.kind.value,
                        "deps": list(it.deps),
                    }
                    for it in submitted.items
                ],
                "rationale": submitted.rationale,
            },
        }

    if isinstance(submitted, SubmittedSummary):
        artifact = submitted.artifact
        kind = "brief" if (isinstance(artifact, dict) and "target_paths" in artifact) else "summary"
        return {
            "kind": kind,
            "summary": (submitted.summary or "")[:_ENVELOPE_SUMMARY_CAP],
            "artifact_ref": sub_run_id,
            "payload": artifact if isinstance(artifact, dict) else {},
        }

    # No posthook submission — fall back to raw final text.
    return {
        "kind": "raw",
        "summary": (final_text or "")[:_ENVELOPE_SUMMARY_CAP],
        "artifact_ref": None,
        "payload": {"final_text": final_text},
    }


def _extract_final_text(messages: list[ConversationMessage]) -> str:
    """Pull the assistant text out of the subagent's last assistant message."""
    for msg in reversed(messages):
        if msg.role != "assistant":
            continue
        text = msg.text
        if text:
            return text.strip()
    return ""


async def _run_posthook_if_needed(
    *,
    sub_def: Any,
    submitted: Any | None,
    final_text: str,
    parent_cfg: Any,
    sandbox_id: str | None,
    base_metadata: ToolExecutionContext,
) -> Any | None:
    """Run the subagent's serializer posthook when the work phase did not submit."""
    if submitted is not None or getattr(sub_def, "posthook", None) is None:
        return submitted

    from agents import get_definition
    from engine.runtime.agent import spawn_agent

    cfg = sub_def.posthook
    posthook_def = get_definition(cfg.agent_name)
    if posthook_def is None:
        raise RuntimeError(
            f"run_subagent: posthook agent {cfg.agent_name!r} for {sub_def.name!r} is not registered"
        )

    posthook_agent = spawn_agent(
        parent_cfg,
        messages=[],
        agent_def=posthook_def,
        latest_user_prompt=final_text,
        sandbox_id=sandbox_id,
    )
    posthook_meta = getattr(posthook_agent.query_context, "tool_metadata", None)
    if posthook_meta is None or not hasattr(posthook_meta, "update"):
        posthook_meta = base_metadata.metadata.copy()
    else:
        posthook_meta = posthook_meta.copy() if hasattr(posthook_meta, "copy") else posthook_meta
        posthook_meta.update(base_metadata.metadata)
    posthook_agent.query_context.tool_metadata = posthook_meta
    posthook_agent.query_context.tool_metadata.agent_name = posthook_def.name
    posthook_agent.query_context.tool_metadata["posthook_metadata_key"] = cfg.metadata_key

    try:
        async for _event in posthook_agent.run(final_text):
            pass
    except Exception as exc:
        raise RuntimeError(
            f"run_subagent: posthook {posthook_def.name!r} failed: {exc}"
        ) from exc

    submitted = posthook_agent.query_context.tool_metadata.get(cfg.metadata_key)
    if submitted is None:
        raise RuntimeError(
            f"run_subagent: posthook {posthook_def.name!r} ended without writing {cfg.metadata_key!r}"
        )
    return submitted


@tool(
    name="run_subagent",
    description=(
        "Spawn a named subagent (e.g. ``scout``) as a background task. "
        "Returns a task_id immediately. Inspect progress with "
        "check_background_progress(task_id=...), join with "
        "wait_for_background_task(task_id=...), or stop stale work with "
        "cancel_background_task(task_id=...). Pass exactly one of ``prompt`` "
        "(free-form text) or ``input`` (structured payload). In team mode, "
        "planners should use this only for exploration subagents such as "
        "``scout``; never pass ``developer`` or ``validator`` here. Emit "
        "multiple calls in one turn for parallel fan-out."
    ),
    background="always",
    task_type="subagent",
)
async def run_subagent(
    agent_name: str,
    prompt: str | None = None,
    input: dict[str, Any] | None = None,
    *,
    context: ToolExecutionContext,
) -> ToolResult:
    """Spawn a named subagent and rejoin via the background-task lifecycle.

    Args:
        agent_name: Required. Name of a registered ``AgentDefinition``
            whose ``agent_type == "subagent"``. Use ``"scout"`` for
            read-only path exploration, ``"subagent"`` for the generic
            worker, or any user-registered subagent definition. Team-mode
            planners must not pass execution agents like ``developer`` or
            ``validator``; those belong in submitted WorkItems.
        prompt: Free-form task description. Mutually exclusive with ``input``.
        input: Structured payload (e.g. ``{"target_paths": [...]}`` for a
            scout). Mutually exclusive with ``prompt``.

    Returns:
        output (str): JSON-encoded envelope ``{summary, artifact_ref, kind,
            payload}`` where ``kind`` is one of ``"brief" | "plan" |
            "summary" | "raw"``.
    """
    from agents import get_definition
    from engine.runtime.agent import spawn_agent

    parent_cfg = context.metadata.session_config
    sandbox_id = context.metadata.sandbox_id or None
    bg_manager = context.metadata.background_task_manager
    task_id = context.metadata.background_task_id
    parent_run_id = context.metadata.agent_run_id
    parent_task_id = task_id if isinstance(task_id, str) else None
    parent_team_run_id = context.metadata.get("team_run_id")

    if parent_cfg is None:
        return ToolResult(
            output="run_subagent: missing session_config in execution context",
            is_error=True,
        )

    # XOR validation: exactly one of prompt / input must be supplied.
    if (prompt is None) == (input is None):
        return ToolResult(
            output=(
                "run_subagent: must supply exactly one of `prompt` (str) or "
                "`input` (dict). For team planners, prefer "
                "`agent_name=\"scout\"` with `input={\"target_paths\": [...]}`; "
                "do not retry with `prompt=null`."
            ),
            is_error=True,
        )

    caller_agent = str(context.metadata.get("agent_name") or "").strip()
    if caller_agent in _SCOUT_ONLY_CALLERS and agent_name != "scout":
        return ToolResult(
            output=(
                f"run_subagent: caller '{caller_agent}' may dispatch only 'scout', "
                f"got '{agent_name}'. Use "
                "`run_subagent(agent_name=\"scout\", input={\"target_paths\": [...]})` "
                "for bounded exploration only. If you need runtime execution, "
                "coding, or validation, emit `developer` / `validator` WorkItems "
                "in the Plan instead of trying to spawn them here. This is "
                "terminal evidence for planners: the next action must be a "
                "bounded scout or a submitted Plan."
            ),
            is_error=True,
        )

    sub_def = get_definition(agent_name)
    if sub_def is None:
        return ToolResult(
            output=f"run_subagent: agent '{agent_name}' is not registered.",
            is_error=True,
        )
    # Construction-time recursion prevention is enforced by AgentDefinition's
    # post-init (subagent type ⇒ can_spawn_subagents=False). The runtime gate
    # below is the second line of defence: only subagent-typed targets are
    # dispatchable through this tool, period.
    if getattr(sub_def, "agent_type", "agent") != "subagent":
        return ToolResult(
            output=(
                f"run_subagent: agent '{agent_name}' is not a subagent "
                f"(agent_type={getattr(sub_def, 'agent_type', 'agent')!r}); "
                "only subagent-typed agents may be dispatched here. "
                "This is terminal evidence for planners: do not retry or wait "
                "on this background task. If you need coding, validation, or "
                "runtime test evidence, emit `developer` / `validator` "
                "WorkItems in the Plan instead of calling `run_subagent`."
            ),
            is_error=True,
        )
    if agent_name == "scout":
        if prompt is not None:
            return ToolResult(
                output=(
                    "run_subagent: scout requires structured "
                    "`input={\"target_paths\": [...]}`; prompt-mode scout "
                    "calls are rejected. If you need to run tests, shell "
                    "commands, or other execution work, emit `developer` / "
                    "`validator` WorkItems instead."
                ),
                is_error=True,
            )
        target_paths = input.get("target_paths") if isinstance(input, dict) else None
        valid_paths = _normalize_target_paths(target_paths)
        if not valid_paths:
            return ToolResult(
                output=(
                    "run_subagent: scout requires non-empty "
                    "`input={\"target_paths\": [...]}`. Scout is for path-bounded "
                    "read-only exploration only; do not use it as a proxy for "
                    "test execution, shell commands, or validation."
                ),
                is_error=True,
            )
        covered_paths = _already_covered_scout_targets(context, valid_paths)
        if covered_paths and len(covered_paths) == len(valid_paths):
            return ToolResult(
                output=(
                    "run_subagent: scout target_paths are already covered in this turn "
                    f"({', '.join(covered_paths)}). Reuse the file reads or prior scout "
                    "you already have and submit the plan instead of re-exploring the same area."
                ),
                is_error=True,
            )

    # Build the subagent's initial user message: shared_briefings preamble
    # (run-scoped, inherited symmetrically with the DAG executor path) + the
    # caller-supplied prompt or serialized input. Parent ``wi.briefings`` are
    # NOT forwarded — only ``shared_briefings`` cross the subagent boundary.
    body = prompt if prompt is not None else json.dumps(input, separators=(",", ":"), default=str)
    # Local import — avoids dragging team.runtime into tools.subagent at module
    # load time and keeps the dependency direction tools→team explicit.
    from team.runtime.context_builder import prepend_shared_briefings_for_subagent

    final_prompt = prepend_shared_briefings_for_subagent(parent_team_run_id, body)

    # Persist a subagent run record FIRST, before spawn_agent — so spawn
    # failures still leave an audit trail that the parent can list / inspect
    # / retry. Reuses the parent's session_id (FK requirement) but sets
    # parent_run_id so the default `list_runs(session_id)` query filters
    # this row out of the user-facing transcript.
    tracker = AgentRunTracker.create(
        session_id=getattr(parent_cfg, "session_id", None),
        agent_name=sub_def.name,
        input_query=final_prompt,
        parent_run_id=parent_run_id if isinstance(parent_run_id, str) else None,
        parent_task_id=parent_task_id,
    )
    sub_run_id = tracker.run_id

    try:
        from server.app_factory import usage_store
    except Exception:
        usage_store = None

    try:
        agent = spawn_agent(
            parent_cfg,
            messages=[],
            agent_def=sub_def,
            latest_user_prompt=final_prompt,
            sandbox_id=sandbox_id,
        )
    except Exception as exc:
        logger.exception("run_subagent: spawn_agent failed")
        # Mark the run as failed at the spawn stage. No messages to capture
        # because the agent never started.
        tracker.finish(
            status="failed",
            display_messages=[],
            api_messages_snapshot=None,
            error=f"spawn_agent failed: {exc}",
            final_text="",
        )
        return ToolResult(output=f"run_subagent: spawn failed: {exc}", is_error=True)

    # Register the live-peek progress provider — closes over the inner agent's
    # _messages list, so each peek returns a fresh snapshot of the last N
    # messages at the moment of the peek (not a stale historical buffer).
    # Also back-link sub_run_id and tag task_type so the in-memory bg task
    # and the persisted audit row can be cross-resolved by either id.
    if bg_manager is not None and isinstance(task_id, str):
        # The bg manager calls the provider with the user-supplied `last_n`
        # from check_background_progress. format_last_n_messages clamps it
        # to PEEK_MESSAGE_MAX so the response stays bounded.
        bg_manager.set_progress_provider(
            task_id,
            lambda last_n: format_last_n_messages(agent.display_messages, last_n),
        )
        tracked = bg_manager.get_task(task_id) if hasattr(bg_manager, "get_task") else None
        if tracked is None:
            logger.warning(
                "run_subagent: bg_manager.get_task(%s) returned None — "
                "tracked.run_id will stay None",
                task_id,
            )
        else:
            tracked.run_id = sub_run_id
            tracked.task_type = "subagent"
            if sub_run_id is None:
                logger.warning(
                    "run_subagent: sub_run_id is None for task %s — "
                    "BackgroundTaskCompleted.run_id will be null",
                    task_id,
                )

    run_error: str | None = None
    cancelled = False
    try:
        async for _event in agent.run(final_prompt):
            # Drain the event stream — agent.run drives _messages, which is
            # what the peek provider reads. We don't need per-event handling.
            pass
    except asyncio.CancelledError:
        cancelled = True
        logger.info("run_subagent: subagent cancelled via bg manager")
    except Exception as exc:
        run_error = str(exc)
        logger.exception("run_subagent: subagent run crashed")

    # If cancel() was called on this bg task, prefer cancellation framing over
    # a generic failure — even if the agent.run loop happened to exit normally
    # before the cancel propagated.
    cancel_reason: str | None = None
    if bg_manager is not None and isinstance(task_id, str):
        tracked = bg_manager.get_task(task_id) if hasattr(bg_manager, "get_task") else None
        if tracked is not None and tracked.status == "cancelled":
            cancelled = True
            cancel_reason = tracked.cancel_reason

    final_text = _extract_final_text(agent.display_messages)
    # Tolerate test stubs that don't expose a query_context.
    qc = getattr(agent, "query_context", None)
    api_snapshot = qc.api_messages_snapshot if qc is not None else None
    if cancelled:
        final_status = "cancelled"
    elif run_error:
        final_status = "failed"
    else:
        final_status = "completed"
    tracker.finish(
        status=final_status,
        display_messages=agent.display_messages,
        api_messages_snapshot=api_snapshot,
        error=run_error,
        final_text=final_text,
        cancellation_reason=cancel_reason,
    )
    # Test stubs may not expose ``agent_name``/``model``/``total_usage`` —
    # skip usage persistence in that case instead of crashing the tool.
    agent_name_for_usage = getattr(agent, "agent_name", None)
    agent_model_for_usage = getattr(agent, "model", None)
    agent_usage_for_usage = getattr(agent, "total_usage", None)
    if (
        usage_store is not None
        and agent_name_for_usage is not None
        and agent_model_for_usage is not None
        and agent_usage_for_usage is not None
    ):
        persist_run_usage(
            usage_store=usage_store,
            session_id=getattr(parent_cfg, "session_id", None),
            run_id=sub_run_id,
            agent_name=agent_name_for_usage,
            model_id=agent_model_for_usage,
            usage=agent_usage_for_usage,
        )

    if cancelled:
        msg = f"run_subagent: cancelled ({cancel_reason})" if cancel_reason else "run_subagent: cancelled"
        # Re-raise so the bg manager's done callback observes a cancelled task
        # and the parent's wait/peek paths see consistent cancelled framing.
        raise asyncio.CancelledError(msg)
    if run_error:
        return ToolResult(output=f"run_subagent: subagent crashed: {run_error}", is_error=True)

    try:
        submitted = await _run_posthook_if_needed(
            sub_def=sub_def,
            submitted=_extract_submitted_output(agent),
            final_text=final_text,
            parent_cfg=parent_cfg,
            sandbox_id=sandbox_id,
            base_metadata=context,
        )
    except Exception as exc:
        return ToolResult(output=str(exc), is_error=True)

    envelope = _build_subagent_envelope(submitted, sub_run_id, final_text)
    return ToolResult(
        output=json.dumps(envelope, default=str),
        metadata={"envelope": envelope},
    )
