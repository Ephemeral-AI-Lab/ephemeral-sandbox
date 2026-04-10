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
import time
from dataclasses import asdict, is_dataclass
from typing import Any
from uuid import uuid4

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
from tools.daytona_toolkit.coordination import (
    build_scope_packet_for_context,
    render_scope_packet,
    scope_paths_from_payload,
    scopes_overlap,
)

logger = logging.getLogger(__name__)


# Hard upper bound on the peek window — even if a caller (e.g. via the
# `last_n` parameter on check_background_progress) requests more, the
# subagent peek clamps to this so the parent's peek response stays bounded.
PEEK_MESSAGE_MAX = 10
# Per-block character cap inside the peek view.
_PEEK_BLOCK_CHAR_CAP = 200
# Total character cap for the peek view.
_PEEK_TOTAL_CHAR_CAP = 2048
_SCOUT_ONLY_CALLERS = frozenset({"team_planner"})
_MAX_SCOUT_LAUNCHES_PER_TURN = 8


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
    prior_scouts = _normalize_target_paths(metadata.get("_scout_target_paths_this_turn", []))
    overlaps: set[str] = set()
    for target in target_paths:
        for prior in prior_scouts:
            if scopes_overlap(target, prior):
                overlaps.add(prior)
    return sorted(overlaps)


def _scout_fanout_admission_error(
    context: ToolExecutionContext,
    target_paths: list[str],
) -> tuple[dict[str, Any] | None, str | None]:
    if str(context.metadata.get("coordination_mode") or "") != "ultra":
        return None, None
    prior_launches_raw = context.metadata.get("_scout_launches_this_turn", 0)
    prior_launches = int(prior_launches_raw) if isinstance(prior_launches_raw, (int, float)) else 0
    if prior_launches >= _MAX_SCOUT_LAUNCHES_PER_TURN:
        return None, (
            "run_subagent: scout fanout cap reached for this turn "
            f"({_MAX_SCOUT_LAUNCHES_PER_TURN}). Reuse the current evidence, inspect progress on "
            "existing scouts, or emit the plan instead of opening another scout lane."
        )
    prior_scouts = _normalize_target_paths(context.metadata.get("_scout_target_paths_this_turn", []))
    if not prior_scouts:
        return None, None
    packet = build_scope_packet_for_context(
        context,
        scope_paths=target_paths,
        baseline_packet=None,
    )
    admission = packet.get("admission") if isinstance(packet, dict) else {}
    if not isinstance(admission, dict) or admission.get("allow_parallel_fanout", True):
        return packet if isinstance(packet, dict) else None, None
    reasons = "; ".join(str(item) for item in (admission.get("reasons") or []) if str(item).strip())
    return packet if isinstance(packet, dict) else None, (
        "run_subagent: live scope status requires serialized scout expansion for this "
        + ("scope" if not reasons else f"scope ({reasons})")
        + ". Reuse the current evidence, inspect progress on existing scouts, or emit "
        "the plan instead of opening another overlapping or hot scout lane."
    )


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
_DIRECT_SUBMISSION_METADATA_KEY = "submitted_output"


def _extract_submitted_output(agent: Any) -> Any | None:
    """Return the agent's accepted submitted output from the active slot."""
    qc = getattr(agent, "query_context", None)
    if qc is None or qc.tool_metadata is None:
        return None
    key = qc.tool_metadata.get("posthook_metadata_key", _DIRECT_SUBMISSION_METADATA_KEY)
    if not isinstance(key, str) or not key.strip():
        key = _DIRECT_SUBMISSION_METADATA_KEY
    return qc.tool_metadata.get(key)


def _coerce_payload_object(value: Any) -> dict[str, Any]:
    """Best-effort conversion of arbitrary structured output into a JSON object."""
    if value is None:
        return {}
    if isinstance(value, dict):
        return value
    if is_dataclass(value):
        dumped = asdict(value)
        return dumped if isinstance(dumped, dict) else {"value": dumped}
    model_dump = getattr(value, "model_dump", None)
    if callable(model_dump):
        dumped = model_dump(mode="json")
        return dumped if isinstance(dumped, dict) else {"value": dumped}
    try:
        dumped = json.loads(json.dumps(value, default=str))
    except Exception:
        return {"value": str(value)}
    return dumped if isinstance(dumped, dict) else {"value": dumped}


def _derive_submission_summary(submitted: Any, final_text: str) -> str:
    summary: str | None = None
    if isinstance(submitted, dict):
        candidate = submitted.get("summary")
        if isinstance(candidate, str):
            summary = candidate.strip()
    else:
        candidate = getattr(submitted, "summary", None)
        if isinstance(candidate, str):
            summary = candidate.strip()
    if summary:
        return summary[:_ENVELOPE_SUMMARY_CAP]
    if final_text:
        return final_text[:_ENVELOPE_SUMMARY_CAP]
    return str(submitted)[:_ENVELOPE_SUMMARY_CAP]


def _build_subagent_envelope(
    submitted: Any | None,
    sub_run_id: str | None,
    final_text: str,
    *,
    artifact_ref: str | None = None,
    atlas: dict[str, Any] | None = None,
) -> dict[str, Any]:
    """Project the subagent submission into the typed envelope.

    Per plan §7:
        - ``Plan``               → kind="plan",    payload={items, rationale}
        - ``SubmittedSummary``   → kind="summary", artifact_ref present if
                                   the runtime stored a real team artifact; if
                                   its artifact carries a ``target_paths`` field,
                                   kind is promoted to ``"brief"``.
        - other structured data  → kind="summary", payload=best-effort JSON
                                   object, summary derived from the submission
                                   or final_text.
        - no posthook submission → kind="raw",    payload={"final_text": ...}
    """
    # Lazy imports — avoid pulling team into tools at import time.
    from team.models import Plan
    from tools.posthook import SubmittedSummary

    if isinstance(submitted, Plan):
        envelope = {
            "kind": "plan",
            "run_id": sub_run_id,
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
        if atlas:
            envelope["atlas"] = dict(atlas)
        return envelope

    if isinstance(submitted, SubmittedSummary):
        artifact = submitted.artifact
        kind = "brief" if (isinstance(artifact, dict) and "target_paths" in artifact) else "summary"
        envelope = {
            "kind": kind,
            "run_id": sub_run_id,
            "summary": (submitted.summary or "")[:_ENVELOPE_SUMMARY_CAP],
            "artifact_ref": artifact_ref,
            "payload": artifact if isinstance(artifact, dict) else {},
        }
        if atlas:
            envelope["atlas"] = dict(atlas)
        return envelope

    if submitted is not None:
        envelope = {
            "kind": "summary",
            "run_id": sub_run_id,
            "summary": _derive_submission_summary(submitted, final_text),
            "artifact_ref": artifact_ref,
            "payload": _coerce_payload_object(submitted),
        }
        if atlas:
            envelope["atlas"] = dict(atlas)
        return envelope

    # No posthook submission — fall back to raw final text.
    envelope = {
        "kind": "raw",
        "run_id": sub_run_id,
        "summary": (final_text or "")[:_ENVELOPE_SUMMARY_CAP],
        "artifact_ref": None,
        "payload": {"final_text": final_text},
    }
    if atlas:
        envelope["atlas"] = dict(atlas)
    return envelope


def _fallback_run_id() -> str:
    """Return a local run id when audit persistence is unavailable."""
    return f"ephemeral-{uuid4().hex[:16]}"


def _extract_final_text(messages: list[ConversationMessage]) -> str:
    """Pull the assistant text out of the subagent's last assistant message."""
    for msg in reversed(messages):
        if msg.role != "assistant":
            continue
        text = msg.text
        if text:
            return text.strip()
    return ""


def _build_posthook_input(final_text: str, messages: list[ConversationMessage]) -> str:
    """Return the best available serializer input for a partial subagent run."""
    if final_text.strip():
        return final_text.strip()

    assistant_text: list[str] = []
    for msg in messages:
        if msg.role != "assistant":
            continue
        text = msg.text.strip()
        if text:
            assistant_text.append(text)

    return "\n\n".join(assistant_text).strip()


def _clear_current_task_cancellation() -> None:
    """Clear pending cancellation so salvage logic can await the posthook."""
    task = asyncio.current_task()
    if task is None:
        return
    uncancel = getattr(task, "uncancel", None)
    cancelling = getattr(task, "cancelling", None)
    if not callable(uncancel):
        return
    remaining = int(cancelling() or 0) if callable(cancelling) else 1
    for _ in range(max(1, remaining)):
        uncancel()


async def _run_posthook_if_needed(
    *,
    posthook_cfg: Any | None,
    posthook_def: Any | None,
    submitted: Any | None,
    final_text: str,
    parent_cfg: Any,
    sandbox_id: str | None,
    base_metadata: ToolExecutionContext,
) -> Any | None:
    """Run the subagent's serializer posthook when the work phase did not submit."""
    if submitted is not None or posthook_cfg is None:
        return submitted

    from engine.runtime.agent import spawn_agent
    from hooks.agent_posthook import read_posthook_output, stamp_posthook_metadata_key

    if posthook_def is None:
        raise RuntimeError(
            f"run_subagent: posthook agent {posthook_cfg.agent_name!r} is not registered"
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
    stamp_posthook_metadata_key(posthook_agent.query_context, posthook_cfg.metadata_key)

    try:
        async for _event in posthook_agent.run(final_text):
            pass
    except Exception as exc:
        raise RuntimeError(
            f"run_subagent: posthook {posthook_def.name!r} failed: {exc}"
        ) from exc

    submitted = read_posthook_output(posthook_agent.query_context, posthook_cfg.metadata_key)
    if submitted is None:
        raise RuntimeError(
            f"run_subagent: posthook {posthook_def.name!r} ended without writing {posthook_cfg.metadata_key!r}"
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
        "multiple disjoint calls in one turn only when live scope status "
        "still admits parallel fan-out."
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
        output (str): JSON-encoded envelope ``{summary, run_id,
            artifact_ref, kind, payload}`` where ``run_id`` is the
            subagent audit run id and ``artifact_ref`` is a real team
            artifact ref when the runtime stored one. ``kind`` is one of
            ``"brief" | "plan" | "summary" | "raw"``.
    """
    from agents import get_definition
    from engine.runtime.agent import spawn_agent
    from hooks.agent_posthook import (
        PosthookMisconfigured,
        resolve_posthook_definition,
        stamp_posthook_metadata_key,
    )

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
    subagent_scope_packet: dict[str, Any] | None = None
    subagent_scope_paths: list[str] = []
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
        if covered_paths:
            return ToolResult(
                output=(
                    "run_subagent: scout target_paths overlap a scope already covered in this turn "
                    f"({', '.join(covered_paths)}). Reuse the file reads or prior scout "
                    "you already have and submit the plan instead of re-exploring the same area."
                ),
                is_error=True,
            )
        subagent_scope_paths = valid_paths
        subagent_scope_packet, fanout_err = _scout_fanout_admission_error(context, valid_paths)
        if fanout_err is not None:
            return ToolResult(
                output=fanout_err,
                is_error=True,
                metadata={"scope_packet": subagent_scope_packet or {}, "conflict": True},
            )
    elif isinstance(input, dict):
        subagent_scope_paths = scope_paths_from_payload(input)
    else:
        baseline_packet = context.metadata.get("scope_packet")
        if isinstance(baseline_packet, dict):
            subagent_scope_paths = [str(item) for item in (baseline_packet.get("scope_paths") or []) if isinstance(item, str)]

    try:
        posthook_cfg, posthook_def = resolve_posthook_definition(
            sub_def,
            agent_lookup=get_definition,
        )
    except PosthookMisconfigured as exc:
        return ToolResult(output=str(exc), is_error=True)

    # Build the subagent's initial user message: shared_briefings preamble
    # (run-scoped, inherited symmetrically with the DAG executor path) + the
    # caller-supplied prompt or serialized input. Parent ``wi.briefings`` are
    # NOT forwarded — only ``shared_briefings`` cross the subagent boundary.
    body = prompt if prompt is not None else json.dumps(input, separators=(",", ":"), default=str)
    # Local import — avoids dragging team.runtime into tools.subagent at module
    # load time and keeps the dependency direction tools→team explicit.
    from team.runtime.context_builder import prepend_shared_briefings_for_subagent

    final_prompt = prepend_shared_briefings_for_subagent(parent_team_run_id, body)
    if subagent_scope_packet is None and subagent_scope_paths:
        maybe_packet = build_scope_packet_for_context(
            context,
            scope_paths=subagent_scope_paths,
            baseline_packet=None,
        )
        if isinstance(maybe_packet, dict):
            subagent_scope_packet = maybe_packet
    rendered_scope_packet = render_scope_packet(subagent_scope_packet)
    if rendered_scope_packet:
        final_prompt = f"{rendered_scope_packet}\n\n{final_prompt}"

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
    persisted_run_id = tracker.run_id
    sub_run_id = persisted_run_id or _fallback_run_id()

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

    subagent_ctx = ToolExecutionContext(cwd=context.cwd, metadata=context.metadata.copy())
    subagent_ctx.metadata["work_item_started_at"] = time.time()
    if parent_team_run_id:
        subagent_ctx.metadata.team_run_id = parent_team_run_id
    if isinstance(subagent_scope_packet, dict):
        subagent_ctx.metadata["scope_packet"] = subagent_scope_packet
        coherence_token = str(subagent_scope_packet.get("coherence_token") or "")
        if coherence_token:
            subagent_ctx.metadata["coherence_token"] = coherence_token

    qc = getattr(agent, "query_context", None)
    if qc is not None:
        merged = qc.tool_metadata.copy() if qc.tool_metadata is not None else subagent_ctx.metadata.copy()
        merged.update(subagent_ctx.metadata)
        merged.agent_name = sub_def.name
        qc.tool_metadata = merged
        stamp_posthook_metadata_key(
            qc,
            posthook_cfg.metadata_key if posthook_cfg is not None else _DIRECT_SUBMISSION_METADATA_KEY,
        )

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
            if persisted_run_id is None:
                logger.warning(
                    "run_subagent: agent_run persistence unavailable for task %s; "
                    "using ephemeral run_id %s",
                    task_id,
                    sub_run_id,
                )

    run_error: str | None = None
    cancelled = False
    early_stopped = False
    try:
        async for _event in agent.run(final_prompt):
            # Drain the event stream — agent.run drives _messages, which is
            # what the peek provider reads. We don't need per-event handling.
            pass
    except asyncio.CancelledError:
        tracked = bg_manager.get_task(task_id) if (
            bg_manager is not None and isinstance(task_id, str) and hasattr(bg_manager, "get_task")
        ) else None
        if tracked is not None and getattr(tracked, "stop_mode", None) == "early_stop":
            early_stopped = True
            _clear_current_task_cancellation()
            logger.info("run_subagent: subagent interrupted for early-stop salvage")
        else:
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
        if tracked is not None and getattr(tracked, "stop_mode", None) == "early_stop":
            early_stopped = True
            cancel_reason = tracked.cancel_reason
        elif tracked is not None and tracked.status == "cancelled":
            cancelled = True
            cancel_reason = tracked.cancel_reason

    final_text = _extract_final_text(agent.display_messages)
    posthook_input = _build_posthook_input(final_text, agent.display_messages)
    # Tolerate test stubs that don't expose a query_context.
    api_snapshot = qc.api_messages_snapshot if qc is not None else None
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
            run_id=persisted_run_id,
            agent_name=agent_name_for_usage,
            model_id=agent_model_for_usage,
            usage=agent_usage_for_usage,
        )

    if cancelled:
        tracker.finish(
            status="cancelled",
            display_messages=agent.display_messages,
            api_messages_snapshot=api_snapshot,
            error=None,
            final_text=final_text,
            cancellation_reason=cancel_reason,
        )
        msg = f"run_subagent: cancelled ({cancel_reason})" if cancel_reason else "run_subagent: cancelled"
        # Re-raise so the bg manager's done callback observes a cancelled task
        # and the parent's wait/peek paths see consistent cancelled framing.
        raise asyncio.CancelledError(msg)
    if run_error:
        tracker.finish(
            status="failed",
            display_messages=agent.display_messages,
            api_messages_snapshot=api_snapshot,
            error=run_error,
            final_text=final_text,
            cancellation_reason=cancel_reason,
        )
        return ToolResult(output=f"run_subagent: subagent crashed: {run_error}", is_error=True)

    submitted = _extract_submitted_output(agent)
    try:
        if submitted is None and (not early_stopped or posthook_input):
            submitted = await _run_posthook_if_needed(
                posthook_cfg=posthook_cfg,
                posthook_def=posthook_def,
                submitted=submitted,
                final_text=posthook_input,
                parent_cfg=parent_cfg,
                sandbox_id=sandbox_id,
                base_metadata=subagent_ctx,
            )
    except Exception as exc:
        tracker.finish(
            status="failed",
            display_messages=agent.display_messages,
            api_messages_snapshot=api_snapshot,
            error=str(exc),
            final_text=final_text,
            cancellation_reason=cancel_reason,
        )
        return ToolResult(output=str(exc), is_error=True)

    stored_artifact_ref: str | None = None
    atlas_info: dict[str, Any] | None = None
    if (
        agent_name == "scout"
        and parent_team_run_id
    ):
        try:
            from team.context.scout_briefings import (
                auto_promote_scout_briefing,
                store_stable_scout_artifact,
            )
            from team.context.canonicalize import scope_of_artifact
            from team.runtime.registry import get as _get_team_run
            from tools.posthook import SubmittedSummary

            team_run = _get_team_run(parent_team_run_id)
            if team_run is not None and isinstance(submitted, SubmittedSummary):
                artifact = submitted.artifact
                if isinstance(artifact, dict):
                    scope = scope_of_artifact(artifact) or ""
                    stored_artifact_ref = store_stable_scout_artifact(
                        team_run,
                        artifact,
                        run_id=persisted_run_id,
                    )
                    if stored_artifact_ref is not None:
                        promoted = auto_promote_scout_briefing(team_run, stored_artifact_ref)
                        persisted = team_run.note_direct_scout_brief(
                            artifact,
                            ci_service=context.metadata.get("ci_service"),
                            reason="run_subagent:scout-complete",
                        )
                        atlas_info = {
                            "subsystem": scope,
                            "persisted": bool(persisted),
                            "promoted": bool(promoted),
                            "artifact_ref": stored_artifact_ref,
                            "reason": "run_subagent:scout-complete",
                        }
        except Exception:
            logger.debug("run_subagent: scout artifact promotion failed", exc_info=True)

    envelope = _build_subagent_envelope(
        submitted,
        sub_run_id,
        final_text,
        artifact_ref=stored_artifact_ref,
        atlas=atlas_info,
    )
    if early_stopped:
        envelope["completion_mode"] = "early_stopped"
        if cancel_reason:
            envelope["cancel_reason"] = cancel_reason
    tracker.finish(
        status="completed",
        display_messages=agent.display_messages,
        api_messages_snapshot=api_snapshot,
        error=None,
        final_text=final_text,
        cancellation_reason=cancel_reason,
    )
    return ToolResult(
        output=json.dumps(envelope, default=str),
        metadata={"envelope": envelope},
    )
