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
from dataclasses import asdict, dataclass, is_dataclass
from typing import Any
from uuid import uuid4

from pydantic import BaseModel, Field
from pydantic.json_schema import GenerateJsonSchema

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
from team._path_utils import scope_paths_from_payload

logger = logging.getLogger(__name__)


# Hard upper bound on the peek window — even if a caller (e.g. via the
# `last_n` parameter on check_background_progress) requests more, the
# subagent peek clamps to this so the parent's peek response stays bounded.
PEEK_MESSAGE_MAX = 10
# Per-block character cap inside the peek view.
_PEEK_BLOCK_CHAR_CAP = 200
# Total character cap for the peek view.
_PEEK_TOTAL_CHAR_CAP = 2048


@dataclass
class _ValidatedRunSubagentRequest:
    sub_def: Any
    subagent_scope_paths: list[str]


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


class RunSubagentInput(BaseModel):
    """Runtime input model for run_subagent.

    Runtime parsing remains permissive so existing internal callers can omit
    one side of the prompt/input pair and let validation return a tool error.
    The published JSON schema is stricter because models tend to emit explicit
    JSON nulls for optional fields, which violates the tool's XOR contract.
    """

    agent_name: str = Field(
        ...,
        description=(
            "Name of a registered dispatchable subagent. In team mode this is "
            "normally `scout`."
        ),
    )
    prompt: str | None = Field(
        default=None,
        description="Free-form subagent task prompt. Mutually exclusive with input.",
    )
    input: dict[str, Any] | None = Field(
        default=None,
        description=(
            "Structured subagent payload. For scout, use "
            "{\"target_paths\": [...]} with live production owner paths only; "
            "keep benchmark tests and missing test-derived paths in task prose "
            "or task_note unless tests are explicitly the owned surface. "
            "Mutually exclusive with prompt."
        ),
    )

    @classmethod
    def model_json_schema(
        cls,
        by_alias: bool = True,
        ref_template: str = "#/$defs/{model}",
        schema_generator: type[GenerateJsonSchema] = GenerateJsonSchema,
        mode: str = "validation",
    ) -> dict[str, Any]:
        schema = super().model_json_schema(
            by_alias=by_alias,
            ref_template=ref_template,
            schema_generator=schema_generator,
            mode=mode,
        )
        props = schema.get("properties", {})

        def _strip_null_variant(name: str, expected_type: str) -> None:
            prop = props.get(name)
            if not isinstance(prop, dict):
                return
            cleaned: dict[str, Any] | None = None
            for variant in prop.get("anyOf", []):
                if isinstance(variant, dict) and variant.get("type") == expected_type:
                    cleaned = dict(variant)
                    break
            if cleaned is None:
                return
            if "title" in prop:
                cleaned["title"] = prop["title"]
            if "description" in prop:
                cleaned["description"] = prop["description"]
            cleaned.pop("default", None)
            if expected_type == "string":
                cleaned["minLength"] = max(int(cleaned.get("minLength", 1) or 1), 1)
            props[name] = cleaned

        _strip_null_variant("prompt", "string")
        _strip_null_variant("input", "object")
        schema["oneOf"] = [
            {"required": ["prompt"]},
            {"required": ["input"]},
        ]
        return schema


class RunSubagentOutput(BaseModel):
    kind: str = Field(
        ...,
        description="Envelope kind: brief, plan, summary, or raw.",
    )
    run_id: str | None = Field(
        default=None,
        description="Persisted subagent run id when available.",
    )
    summary: str = Field(
        default="",
        description="Runtime-derived compact envelope text.",
    )
    artifact_ref: str | None = Field(
        default=None,
        description="Reference to a durable artifact when one was stored.",
    )
    payload: dict[str, Any] = Field(
        default_factory=dict,
        description="JSON-safe subagent output payload.",
    )
    completion_mode: str | None = Field(
        default=None,
        description="Optional completion mode such as early_stopped.",
    )
    cancel_reason: str | None = Field(
        default=None,
        description="Cancellation reason when the subagent was stopped early.",
    )


def _validate_run_subagent_request(
    *,
    agent_name: str,
    prompt: str | None,
    input: dict[str, Any] | None,
    context: ToolExecutionContext,
) -> ToolResult | _ValidatedRunSubagentRequest:
    from agents import get_definition

    parent_cfg = context.metadata.session_config
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

    sub_def = get_definition(agent_name)
    if sub_def is None:
        return ToolResult(
            output=f"run_subagent: agent '{agent_name}' is not registered.",
            is_error=True,
        )
    if sub_def.agent_type != "subagent":
        return ToolResult(
            output=(
                f"run_subagent: agent '{agent_name}' is not a subagent "
                f"(agent_type={sub_def.agent_type!r}); "
                "only subagent-typed agents may be dispatched here. "
                "This is terminal evidence for planners: do not retry or wait "
                "on this background task. If you need coding, validation, or "
                "runtime test evidence, emit `developer` / `validator` "
                "tasks in the plan instead of calling `run_subagent`."
            ),
            is_error=True,
        )
    if not bool(getattr(sub_def, "dispatchable_via_run_subagent", False)):
        return ToolResult(
            output=(
                f"run_subagent: agent '{agent_name}' is an internal subagent and "
                "may not be dispatched via `run_subagent`. Use a dispatchable "
                "worker subagent such as `scout`, or emit `developer` / "
                "`validator` tasks in the plan."
            ),
            is_error=True,
        )

    subagent_scope_paths: list[str] = []
    if agent_name == "scout" and isinstance(input, dict):
        target_paths = input.get("target_paths")
        subagent_scope_paths = _normalize_target_paths(target_paths)
    elif isinstance(input, dict):
        subagent_scope_paths = scope_paths_from_payload(input)

    return _ValidatedRunSubagentRequest(
        sub_def=sub_def,
        subagent_scope_paths=subagent_scope_paths,
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
        - no submission          → kind="raw",    payload={"final_text": ...}
    """
    # Lazy imports — avoid pulling team into tools at import time.
    from team.models import Plan, SubmittedSummary

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

        return envelope

    if submitted is not None:
        envelope = {
            "kind": "summary",
            "run_id": sub_run_id,
            "summary": _derive_submission_summary(submitted, final_text),
            "artifact_ref": artifact_ref,
            "payload": _coerce_payload_object(submitted),
        }

        return envelope

    # No submission — fall back to raw final text.
    envelope = {
        "kind": "raw",
        "run_id": sub_run_id,
        "summary": (final_text or "")[:_ENVELOPE_SUMMARY_CAP],
        "artifact_ref": None,
        "payload": {"final_text": final_text},
    }
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


def _clear_current_task_cancellation() -> None:
    """Clear pending cancellation so salvage logic can await cleanup."""
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


def _snapshot_messages(messages: list[Any] | None) -> list[dict[str, Any]]:
    if not messages:
        return []
    out: list[dict[str, Any]] = []
    for msg in messages:
        if isinstance(msg, dict):
            out.append(msg)
            continue
        dump = getattr(msg, "model_dump", None)
        if callable(dump):
            dumped = dump(mode="json")
            if isinstance(dumped, dict):
                out.append(dumped)
    return out



@tool(
    name="run_subagent",
    description=(
        "Spawn a named subagent (e.g. ``scout``) as a background task. "
        "Returns a task_id immediately. Continue foreground work when useful; "
        "join with wait_for_background_task(task_id=...) when blocked on the "
        "result, optionally use check_background_progress(task_id=...) when "
        "live status changes the next action, or stop stale work with "
        "cancel_background_task(task_id=...). Pass exactly one of ``prompt`` "
        "(free-form text) or ``input`` (structured payload). In team mode, "
        "planners should use this only for exploration subagents such as "
        "``scout``; never pass ``developer`` or ``validator`` here. Emit "
        "multiple disjoint calls in one turn only when live scope status "
        "still admits parallel fan-out. For ``scout`` structured input, "
        "scrub ``target_paths`` to live production owner files/directories; "
        "benchmark tests, ``*/tests/*``, ``test_*.py``, and missing "
        "test-derived paths belong in task prose or task_note unless tests "
        "are explicitly the owned surface."
    ),
    short_description="Spawn a subagent in the background.",
    input_model=RunSubagentInput,
    output_model=RunSubagentOutput,
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
    """Spawn a named subagent and rejoin via the background-task lifecycle."""
    from engine.runtime.agent import spawn_agent

    parent_cfg = context.metadata.session_config
    sandbox_id = context.metadata.sandbox_id or None
    bg_manager = context.metadata.background_task_manager
    task_id = context.metadata.background_task_id
    parent_run_id = context.metadata.agent_run_id
    parent_task_id = task_id if isinstance(task_id, str) else None
    parent_team_run_id = context.metadata.get("team_run_id")

    validation = _validate_run_subagent_request(
        agent_name=agent_name,
        prompt=prompt,
        input=input,
        context=context,
    )
    if isinstance(validation, ToolResult):
        return validation
    sub_def = validation.sub_def
    subagent_scope_paths = validation.subagent_scope_paths

    body = prompt if prompt is not None else json.dumps(input, separators=(",", ":"), default=str)
    final_prompt = body
    if agent_name == "scout" and subagent_scope_paths:
        strict_scope_lines = [
            "## Scout scope contract",
            "- Explore only the assigned `target_paths`.",
            "- If a target path is a file, do not widen to sibling files or the whole package unless the target itself is a directory.",
            "- If a target path does not exist, report zero coverage for that missing path instead of correcting it to a nearby file.",
            "- Do not suggest an 'intended' or 'correct' nearby path when the assigned target is missing.",
            "- Do not inspect already-named benchmark test files or guessed owner files unless they are inside `target_paths`.",
            "- Start source-code scouting with `ci_query_symbol(...)`.",
            "- The durable handoff must be exactly one `submit_task_note(...)` call with non-empty `content`; never put findings only in final text.",
            "- If `ci_query_symbol(...)` already returned definitions for an exact file target, stay read-free and post `submit_task_note(...)` from CI evidence.",
            "- On coordinated benchmark lanes, exact-file and short fixed-file scouts stay read-free; if CI stays cold, report the gap instead.",
            "- If the note tool returns and the loop asks for final text, say only `Posted.`",
        ]
        strict_scope_block = "\n".join(strict_scope_lines)
        final_prompt = f"{strict_scope_block}\n\n{final_prompt}"

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
    if subagent_scope_paths:
        subagent_ctx.metadata["write_scope"] = list(subagent_scope_paths)
    if getattr(sub_def, "role", None):
        subagent_ctx.metadata["role"] = sub_def.role

    qc = getattr(agent, "query_context", None)
    if qc is not None:
        merged = qc.tool_metadata.copy() if qc.tool_metadata is not None else subagent_ctx.metadata.copy()
        merged.update(subagent_ctx.metadata)
        merged.agent_name = sub_def.name
        qc.tool_metadata = merged

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
    # Tolerate test stubs that don't expose a query_context.
    api_snapshot = qc.api_messages_snapshot if qc is not None else None
    run_meta = qc.tool_metadata if qc is not None and qc.tool_metadata is not None else subagent_ctx.metadata
    if final_text:
        run_meta["work_result"] = final_text
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


    envelope = _build_subagent_envelope(
        None,
        sub_run_id,
        final_text,
        artifact_ref=None,
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


def _run_subagent_background_preflight(arguments: Any, context: ToolExecutionContext) -> ToolResult | None:
    validation = _validate_run_subagent_request(
        agent_name=str(getattr(arguments, "agent_name", "")),
        prompt=getattr(arguments, "prompt", None),
        input=getattr(arguments, "input", None),
        context=context,
    )
    if isinstance(validation, ToolResult):
        return validation
    return None


run_subagent._background_preflight = _run_subagent_background_preflight
