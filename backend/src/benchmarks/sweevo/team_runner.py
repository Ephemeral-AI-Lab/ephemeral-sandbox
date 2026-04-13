"""Wire a real team run over a provisioned SWE-EVO sandbox.

Drives :class:`team.runtime.team_run.TeamRun` with the builtin
``team_planner`` / ``developer`` / ``validator`` agents from
``team.builtins``. Each Task's agent is spawned through
:func:`engine.runtime.agent.spawn_agent` so it runs with its full
production tool surface (``sandbox_operations``, ``code_intelligence``,
``context``, skills) against the Daytona sandbox that was already
prepared by :func:`benchmarks.sweevo.sandbox.create_sweevo_test_sandbox`.
"""

from __future__ import annotations

from collections import Counter
from datetime import datetime, timezone
import json
import logging
from pathlib import Path
from typing import Any, Callable

from agents.run_tracker import AgentRunTracker
from agents.registry import get_definition
from config.paths import get_project_config_dir
from engine.runtime.agent import spawn_agent
from message.event_printer import MultiAgentEventPrinter
from message.messages import ConversationMessage, ToolUseBlock
from message.stream_events import ToolExecutionCompleted
from token_tracker.runtime import persist_run_usage
from code_intelligence.routing.service import get_code_intelligence
from team.builtins import (
    DEVELOPER,
    SCOUT,
    TEAM_PLANNER,
    TEAM_REPLANNER,
    VALIDATOR,
    register_all as _register_team_builtins,
)
from team.models import BudgetConfig, TeamDefinition, TeamRunStatus
from team.persistence.store import TeamDefinitionStore
from team.persistence.events import make_checkpoint_repo_state
from team.persistence.run_store import build_default_store
from team.runtime.context_builder import (
    TeamAgentContext,
    build_initial_user_message,
    build_work_item_metadata,
)
from team.runtime.executor import Executor
from team.runtime.team_run import TeamRun

from benchmarks.sweevo.dataset import summarize_sweevo_instance
from benchmarks.sweevo.models import SWEEvoInstance, _REPO_DIR
from benchmarks.sweevo.sandbox import (
    apply_sweevo_repo_patch,
    capture_sweevo_repo_patch,
    ensure_sweevo_test_patch,
    setup_sweevo_sandbox,
)

logger = logging.getLogger(__name__)


def _utc_iso_now() -> str:
    return datetime.now(timezone.utc).isoformat()


def _append_benchmark_event(
    team_metrics: dict[str, Any] | None,
    event: dict[str, Any],
) -> None:
    if not team_metrics:
        return
    path_value = team_metrics.get("structured_log_path")
    if not path_value:
        return
    path = Path(str(path_value))
    path.parent.mkdir(parents=True, exist_ok=True)
    payload = {"ts": _utc_iso_now(), **event}
    with path.open("a", encoding="utf-8") as handle:
        handle.write(json.dumps(payload, default=str) + "\n")


def _ensure_team_builtins() -> None:
    try:
        _register_team_builtins()
    except Exception:
        logger.debug("team builtins already registered", exc_info=True)


# Default pool size for the team's Executor workers. Not a cap — callers
# can still override.
_DEFAULT_NUM_EXECUTORS = 8
_PROJECT_ROOT = Path(__file__).resolve().parents[4]

_SWEEVO_TEAM_NAME = "sweevo_benchmark"


def _load_or_create_team_definition(
    session_factory: object | None,
) -> TeamDefinition:
    """Load the sweevo team definition from file registry or DB.

    Priority:
    1. File-based builtin (already in registry via ``register_all``).
    2. DB dual-write via ``seed_builtin`` when file found + DB available.
    3. DB fallback when file is missing but DB has an existing record.

    Raises ``RuntimeError`` if the team definition is not found in either
    the file registry or the database.
    """
    from team.registry import get_team_definition

    defn = get_team_definition(_SWEEVO_TEAM_NAME)

    if session_factory is not None:
        store = TeamDefinitionStore()
        store.initialize(session_factory)  # type: ignore[arg-type]
        if defn is not None:
            # File found — dual-write to DB (idempotent).
            return store.seed_builtin(defn)
        # File missing — try DB fallback.
        existing = store.get_by_name(_SWEEVO_TEAM_NAME)
        if existing is not None:
            return existing
        raise RuntimeError(
            f"Team definition {_SWEEVO_TEAM_NAME!r} not found — "
            "ensure backend/config/teams/sweevo_benchmark.md exists "
            "or seed the database via the CRUD API."
        )

    # No DB — return file-based definition.
    if defn is not None:
        return defn
    raise RuntimeError(
        f"Team definition {_SWEEVO_TEAM_NAME!r} not found — "
        "ensure backend/config/teams/sweevo_benchmark.md exists."
    )


def _benchmark_team_run_dir() -> Path:
    """Return the benchmark-owned TeamRun event log directory."""
    return get_project_config_dir(_PROJECT_ROOT) / "team-runs"


def _build_benchmark_event_store(*, session_factory: object | None) -> Any:
    """Prefer DB-backed durability, else fall back to a stable project-local JSONL log."""
    if session_factory is not None:
        return build_default_store(session_factory=session_factory)
    return build_default_store(base_dir=_benchmark_team_run_dir())


def _checkpoint_records_from_store(store: Any, team_run_id: str) -> list[dict[str, Any]]:
    checkpoints: list[dict[str, Any]] = []
    seen_ids: set[str] = set()
    load_run = getattr(store, "load_run", None)
    if not callable(load_run):
        return checkpoints
    for event in load_run(team_run_id):
        if event.kind != "checkpoint_taken":
            continue
        checkpoint_id = str(event.data.get("checkpoint_id") or "").strip()
        if not checkpoint_id or checkpoint_id in seen_ids:
            continue
        seen_ids.add(checkpoint_id)
        checkpoints.append(
            {
                "id": checkpoint_id,
                "label": event.data.get("label"),
                "sequence": int(event.data.get("sequence") or 0),
            }
        )
    return checkpoints


def _checkpoint_ids_from_store(store: Any, team_run_id: str) -> list[str]:
    return [record["id"] for record in _checkpoint_records_from_store(store, team_run_id)]


def _checkpoint_repo_patch_from_store(store: Any, team_run_id: str, checkpoint_id: str) -> str:
    load_run = getattr(store, "load_run", None)
    if not callable(load_run):
        return ""
    repo_patch = ""
    for event in load_run(team_run_id):
        if event.kind != "checkpoint_repo_state":
            continue
        if str(event.data.get("checkpoint_id") or "").strip() != checkpoint_id:
            continue
        repo_patch = str(event.data.get("repo_patch") or "")
    return repo_patch


# ---------------------------------------------------------------------------
# Prompt construction
# ---------------------------------------------------------------------------


def _derive_sweevo_budgets(instance: SWEEvoInstance) -> BudgetConfig:
    """Return size-aware team budgets for SWE-EVO instead of disabling them."""
    summary = summarize_sweevo_instance(instance)
    size = str(summary.get("size") or "medium")
    f2p_targets = max(1, len(instance.fail_to_pass))

    base = {
        "small":  {"max_depth": 4, "max_plan_size": 8,  "max_tasks": 24},
        "medium": {"max_depth": 5, "max_plan_size": 12, "max_tasks": 40},
        "large":  {"max_depth": 6, "max_plan_size": 16, "max_tasks": 64},
    }.get(size, {"max_depth": 5, "max_plan_size": 12, "max_tasks": 40})
    max_depth = min(int(base["max_depth"]), 4)

    # Keep each planner level inside the benchmark-size ceiling. When the
    # natural task set is wider than that, compress adjacent work into
    # expandable child-planner lanes rather than flattening more siblings.
    plan_size = int(base["max_plan_size"])
    max_tasks = max(
        int(base["max_tasks"]),
        max(4, min(plan_size, f2p_targets)) * max_depth,
    )
    return BudgetConfig(
        max_tasks=max_tasks,
        max_depth=max_depth,
        max_plan_size=plan_size,
    )


def _derive_planner_runtime_limits(instance: SWEEvoInstance) -> dict[str, int]:
    """Return benchmark-specific planner limits.

    Keep the planner on the default coordination budget so it can finish
    decomposition before execution lanes inherit tighter limits.
    """
    del instance
    tool_call_limit = 100
    return {
        "tool_call_limit": tool_call_limit,
    }


def _derive_execution_runtime_limits(instance: SWEEvoInstance) -> dict[str, int]:
    """Return tighter runtime limits for execution lanes on SWE-EVO."""
    del instance
    tool_call_limit = 50
    return {
        "tool_call_limit": tool_call_limit,
    }


def _build_root_prompt(instance: SWEEvoInstance, repo_dir: str) -> str:
    summary = summarize_sweevo_instance(instance)
    size = str(summary.get("size") or "medium")
    max_plan_size = _derive_sweevo_budgets(instance).max_plan_size
    pass_to_pass_summary = _summarize_guardrail_tests(instance.pass_to_pass)
    return (
        f"You are leading a coding team on a SWE-EVO benchmark instance.\n"
        f"Repository: {instance.repo}\n"
        f"Working directory inside the sandbox: {repo_dir}\n"
        f"Base commit (already checked out): {instance.base_commit}\n\n"
        f"## Objective\n"
        f"Make the grading command pass by fixing the repository so the fail-to-pass tests turn green "
        f"without regressing the pass-to-pass coverage.\n\n"
        f"The SWE-EVO test patch has already been applied inside the sandbox, so any newly added "
        f"or modified fail-to-pass tests are present in the working tree.\n\n"
        f"## Fail-To-Pass Targets\n"
        f"{json.dumps(instance.fail_to_pass, indent=2)}\n\n"
        f"## Pass-To-Pass Guardrail\n"
        f"{json.dumps(pass_to_pass_summary, indent=2)}\n\n"
        f"## Grading command\n"
        f"After your team finishes, this exact command will be executed in the sandbox "
        f"to grade the work:\n```\n{instance.test_cmds}\n```\n\n"
        f"## Run Focus\n"
        f"- Instance size: {size} ({summary.get('bullet_count', 0)} changelog bullets, "
        f"{len(instance.fail_to_pass)} fail-to-pass target(s)).\n"
        f"- This run is primarily evaluating the coordination behavior described in "
        f"`docs/architecture/plan-a-team-coordination-redesign.md`.\n"
        f"- Keep the prompt light and let the declared skills own the detailed workflow policy.\n"
        f"- Prioritize evidence that the Task Center, scout waves, scoped-path freshness, "
        f"and recovery/replanning loop behave as designed under live repository change.\n"
        f"- Root planning should submit early, split direct work from expandable work, "
        f"and treat the per-layer cap of {max_plan_size} tasks as a budgeting guardrail.\n"
        f"- Use `.ephemeralos/benchmark-logs/` only as supporting evidence when debugging "
        f"runtime, coordination, retry, or checkpoint behavior.\n"
        f"- Fix the repository checkout itself. Do not rely on ad hoc sandbox-only "
        f"package upgrades or ambient environment mutations as the benchmark fix.\n"
        f"- Stay inside {repo_dir}."
    )


def _summarize_guardrail_tests(test_ids: list[str]) -> dict[str, Any]:
    file_counts = Counter(
        item.split("::", 1)[0]
        for item in test_ids
        if isinstance(item, str) and item.strip()
    )
    return {
        "total_tests": len(test_ids),
        "unique_files": len(file_counts),
        "top_files_by_test_count": [
            {"file": file_path, "tests": count}
            for file_path, count in file_counts.most_common(10)
        ],
        "sample_test_ids": list(test_ids[:20]),
    }


def _task_base_prompt(task_text: Any) -> str:
    if isinstance(task_text, dict) and task_text:
        rendered = json.dumps(task_text, indent=2, default=str)
        primary: list[str] = []
        for key in ("task", "prompt", "description", "instructions"):
            value = task_text.get(key)
            if isinstance(value, str) and value.strip():
                primary.append(value.strip())
        if primary:
            return "\n\n".join(primary) + "\n\nTask context:\n" + rendered
        return "Task context:\n" + rendered
    if isinstance(task_text, str):
        return task_text
    return f"Task: {task_text!r}"


def _extract_final_text(messages: list[Any]) -> str:
    """Return the last assistant text emitted by an agent run."""
    for msg in reversed(messages):
        if getattr(msg, "role", None) != "assistant":
            continue
        text = getattr(msg, "text", "")
        if text:
            return str(text).strip()
    return ""


def _extract_json_object(
    text: str,
    *,
    matcher: Callable[[dict[str, Any]], bool] | None = None,
) -> dict[str, Any] | None:
    if not text.strip():
        return None

    decoder = json.JSONDecoder()
    best_payload: dict[str, Any] | None = None
    best_start: int | None = None
    best_end = -1

    for start, char in enumerate(text):
        if char != "{":
            continue
        try:
            payload, end = decoder.raw_decode(text, idx=start)
        except ValueError:
            continue
        if not isinstance(payload, dict) or (matcher is not None and not matcher(payload)):
            continue
        if end > best_end or (end == best_end and (best_start is None or start < best_start)):
            best_payload = payload
            best_start = start
            best_end = end
    return best_payload



def _find_matching_delimiter(text: str, start: int, open_char: str, close_char: str) -> int:
    depth = 0
    in_string = False
    escape = False
    for idx in range(start, len(text)):
        char = text[idx]
        if in_string:
            if escape:
                escape = False
            elif char == "\\":
                escape = True
            elif char == '"':
                in_string = False
            continue
        if char == '"':
            in_string = True
            continue
        if char == open_char:
            depth += 1
        elif char == close_char:
            depth -= 1
            if depth == 0:
                return idx
    return -1


_PLAN_ITEM_PRIMARY_KEYS = frozenset({"id", "agent_name"})


def _skip_json_whitespace(text: str, start: int) -> int:
    idx = start
    while idx < len(text) and text[idx].isspace():
        idx += 1
    return idx


def _parse_json_object_field(
    text: str,
    start: int,
    decoder: json.JSONDecoder,
) -> tuple[str, Any, int] | None:
    idx = _skip_json_whitespace(text, start)
    try:
        key, key_end = decoder.raw_decode(text, idx)
    except ValueError:
        return None
    if not isinstance(key, str):
        return None
    colon = _skip_json_whitespace(text, key_end)
    if colon >= len(text) or text[colon] != ":":
        return None
    value_start = _skip_json_whitespace(text, colon + 1)
    try:
        value, value_end = decoder.raw_decode(text, value_start)
    except ValueError:
        return None
    return key, value, value_end


def _peek_plan_item_start_key(
    text: str,
    start: int,
    decoder: json.JSONDecoder,
) -> tuple[str | None, bool]:
    idx = _skip_json_whitespace(text, start)
    saw_open_brace = False
    if idx < len(text) and text[idx] == "{":
        saw_open_brace = True
        idx = _skip_json_whitespace(text, idx + 1)
    parsed = _parse_json_object_field(text, idx, decoder)
    if parsed is None:
        return None, saw_open_brace
    key, _, _ = parsed
    if key not in _PLAN_ITEM_PRIMARY_KEYS:
        return None, saw_open_brace
    return key, saw_open_brace


def _has_duplicate_top_level_primary_keys(
    text: str,
    decoder: json.JSONDecoder,
) -> bool:
    idx = _skip_json_whitespace(text, 0)
    if idx >= len(text) or text[idx] != "{":
        return False
    idx = _skip_json_whitespace(text, idx + 1)

    seen_primary: set[str] = set()
    while idx < len(text):
        if text[idx] == "}":
            return False
        parsed = _parse_json_object_field(text, idx, decoder)
        if parsed is None:
            return False
        key, _, value_end = parsed
        if key in _PLAN_ITEM_PRIMARY_KEYS:
            if key in seen_primary:
                return True
            seen_primary.add(key)

        idx = _skip_json_whitespace(text, value_end)
        if idx >= len(text):
            return False
        if text[idx] == "}":
            return False
        if text[idx] != ",":
            return False
        idx = _skip_json_whitespace(text, idx + 1)
    return False


def _parse_repaired_plan_item(
    text: str,
    start: int,
    decoder: json.JSONDecoder,
) -> tuple[dict[str, Any], int] | None:
    idx = _skip_json_whitespace(text, start)
    if idx >= len(text):
        return None

    try:
        payload, end = decoder.raw_decode(text, idx)
    except ValueError:
        payload = None
    if isinstance(payload, dict):
        raw_object = text[idx:end]
        if not _has_duplicate_top_level_primary_keys(raw_object, decoder):
            return payload, end

    saw_open_brace = False
    if text[idx] == "{":
        saw_open_brace = True
        idx = _skip_json_whitespace(text, idx + 1)

    item: dict[str, Any] = {}
    saw_non_primary_key = False
    while idx < len(text):
        while idx < len(text) and text[idx] == "}":
            idx += 1
            idx = _skip_json_whitespace(text, idx)
        parsed = _parse_json_object_field(text, idx, decoder)
        if parsed is None:
            break
        key, value, value_end = parsed
        item[key] = value
        if key not in _PLAN_ITEM_PRIMARY_KEYS:
            saw_non_primary_key = True

        idx = _skip_json_whitespace(text, value_end)
        while idx < len(text) and text[idx] == "}":
            idx += 1
            idx = _skip_json_whitespace(text, idx)
            if saw_open_brace:
                return item, idx

        if idx >= len(text) or text[idx] != ",":
            break

        next_key, next_has_open_brace = _peek_plan_item_start_key(text, idx + 1, decoder)
        should_split = (
            next_key in _PLAN_ITEM_PRIMARY_KEYS
            and bool(item)
            and (next_has_open_brace or saw_non_primary_key or item.keys() >= _PLAN_ITEM_PRIMARY_KEYS)
        )
        idx += 1
        if should_split:
            break
        idx = _skip_json_whitespace(text, idx)

    if not item:
        return None
    return item, idx


def _repair_submitted_plan_payload(text: str) -> dict[str, Any] | None:
    items_key = text.find('"items"')
    if items_key < 0:
        return None
    array_start = text.find("[", items_key)
    if array_start < 0:
        return None
    array_end = _find_matching_delimiter(text, array_start, "[", "]")
    if array_end < 0:
        return None

    decoder = json.JSONDecoder()
    raw_items = text[array_start + 1 : array_end]

    items: list[dict[str, Any]] = []
    idx = 0
    while True:
        idx = _skip_json_whitespace(raw_items, idx)
        while idx < len(raw_items) and raw_items[idx] in ",}":
            idx += 1
            idx = _skip_json_whitespace(raw_items, idx)
        if idx >= len(raw_items):
            break
        parsed_item = _parse_repaired_plan_item(raw_items, idx, decoder)
        if parsed_item is None:
            return None
        item, idx = parsed_item
        items.append(item)
    if not items:
        return None

    repaired: dict[str, Any] = {"items": items}
    rationale_key = text.find('"rationale"', array_end)
    if rationale_key >= 0:
        colon = text.find(":", rationale_key)
        if colon >= 0:
            raw_tail = text[colon + 1 :].lstrip()
            try:
                rationale, _ = decoder.raw_decode(raw_tail)
            except ValueError:
                rationale = None
            if isinstance(rationale, str):
                repaired["rationale"] = rationale
    return repaired


def _estimate_final_context(messages: list[ConversationMessage] | None) -> int:
    """Best-effort token estimate for the final compacted provider context."""
    if not messages:
        return 0
    try:
        from compaction import estimate_message_tokens

        return estimate_message_tokens(messages)
    except Exception:
        logger.debug("Failed to estimate final compacted context", exc_info=True)
        return 0


def _persist_benchmark_session(
    *,
    session_config: Any,
    agent: Any,
    summary_text: str,
) -> None:
    """Persist the latest benchmark agent history into the shared session row."""
    try:
        from server.app_factory import session_store
    except Exception:
        session_store = None
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
        logger.debug("Failed to persist benchmark session snapshot", exc_info=True)


def _tool_names_from_messages(messages: list[ConversationMessage]) -> list[str]:
    names: list[str] = []
    for msg in messages:
        for block in getattr(msg, "content", []):
            if isinstance(block, ToolUseBlock):
                names.append(block.name)
    return names


def _background_tool_names_from_messages(
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


def _enforce_validation_evidence(
    agent_name: str,
    display_messages: list[ConversationMessage],
) -> None:
    if agent_name != VALIDATOR:
        return
    tool_names = _tool_names_from_messages(display_messages)
    if "daytona_codeact" in tool_names:
        return
    raise RuntimeError(
        "validator_missing_tool_evidence: validator must execute at least one "
        "daytona_codeact verification command before returning a verdict"
    )


# ---------------------------------------------------------------------------
# Runner + executor factory
# ---------------------------------------------------------------------------


def _make_runner(
    session_config: Any,
    sandbox_id: str,
    printer: MultiAgentEventPrinter | None,
    team_metrics: dict[str, Any] | None = None,
    agent_overrides: dict[str, dict[str, Any]] | None = None,
):
    async def _run(defn, ctx: TeamAgentContext):
        effective_defn = defn
        if agent_overrides:
            overrides = agent_overrides.get(defn.name)
            if overrides:
                effective_defn = defn.model_copy(update=overrides)
        prompt = ctx.user_message or _task_base_prompt(None)
        tracker = AgentRunTracker.create(
            session_id=getattr(session_config, "session_id", None),
            run_id=getattr(ctx.tool_metadata, "agent_run_id", None),
            agent_name=effective_defn.name,
            input_query=prompt,
        )
        if tracker.run_id is not None:
            ctx.tool_metadata.agent_run_id = tracker.run_id

        agent = spawn_agent(
            session_config,
            messages=[],
            agent_def=effective_defn,
            latest_user_prompt=prompt,
            sandbox_id=sandbox_id,
        )
        compacted_before = None
        if getattr(agent.query_context, "session_state", None) is not None:
            compacted_before = int(agent.query_context.session_state.compacted)

        # Redirect the spawned agent's tool_metadata to the team ctx so
        # submit_plan / submit_summary tools write into the correct slot.
        # Preserve session_config and sandbox_id that spawn_agent installed.
        spawned_meta = agent.query_context.tool_metadata
        if getattr(spawned_meta, "session_config", None) is not None:
            ctx.tool_metadata.session_config = spawned_meta.session_config
        sb = getattr(spawned_meta, "sandbox_id", None) or ""
        if sb:
            ctx.tool_metadata["sandbox_id"] = sb
        ctx.tool_metadata.agent_name = effective_defn.name
        agent.query_context.tool_metadata = ctx.tool_metadata
        agent.query_context.run_id = tracker.run_id or ""
        if printer is not None and effective_defn.name == TEAM_PLANNER:
            printer.raw_line(
                effective_defn.name,
                (
                    "[runtime_limits] "
                    f"tool_call_limit={agent.query_context.tool_call_limit}"
                ),
            )

        event_count = 0
        run_error: str | None = None
        final_text = ""
        try:
            async for event in agent.run(prompt):
                event_count += 1
                if printer is None:
                    continue
                try:
                    object.__setattr__(event, "agent_name", effective_defn.name)
                except Exception:
                    pass
                try:
                    printer.emit(event)
                except Exception:
                    logger.debug("printer.emit failed", exc_info=True)
                if effective_defn.name == TEAM_PLANNER and isinstance(event, ToolExecutionCompleted):
                    printer.raw_line(
                        effective_defn.name,
                        (
                            "[runtime_budget] "
                            f"used={agent.query_context.tool_calls_used} "
                            f"limit={agent.query_context.tool_call_limit}"
                        ),
                    )
        except Exception as exc:
            run_error = str(exc)
            logger.exception("sweevo team runner: agent %s crashed", defn.name)
            raise
        finally:
            qc = getattr(agent, "query_context", None)
            final_text = _extract_final_text(agent.display_messages)
            session_state = getattr(qc, "session_state", None)
            compacted_total = int(getattr(session_state, "compacted", 0) or 0)
            new_compactions = 0
            if session_state is not None and compacted_before is not None:
                new_compactions = compacted_total - compacted_before
            final_context_tokens = _estimate_final_context(
                getattr(qc, "api_messages_snapshot", None),
            )
            response_payload = {
                "final_text": final_text,
                "tool_calls_used": int(getattr(qc, "tool_calls_used", 0) or 0),
                "tool_call_limit": getattr(qc, "tool_call_limit", None),
                "final_context_tokens": final_context_tokens,
                "compactions_added": new_compactions,
                "compacted": compacted_total,
            }
            tracker.finish(
                status="failed" if run_error else "completed",
                display_messages=list(agent.display_messages),
                api_messages_snapshot=getattr(qc, "api_messages_snapshot", None),
                response=response_payload,
                error=run_error,
                final_text=final_text,
                event_count=event_count,
            )
            _persist_benchmark_session(
                session_config=session_config,
                agent=agent,
                summary_text=final_text or prompt,
            )
            try:
                from server.app_factory import usage_store
            except Exception:
                usage_store = None
            if usage_store is not None:
                persist_run_usage(
                    usage_store=usage_store,
                    session_id=getattr(session_config, "session_id", None),
                    run_id=tracker.run_id,
                    agent_name=effective_defn.name,
                    model_id=agent.model,
                    usage=agent.total_usage,
                )
            if printer is not None:
                prompt_tokens = int(getattr(agent.total_usage, "input_tokens", 0) or 0)
                completion_tokens = int(getattr(agent.total_usage, "output_tokens", 0) or 0)
                total = prompt_tokens + completion_tokens
                tool_calls_used = int(getattr(qc, "tool_calls_used", 0) or 0)
                tool_call_limit = getattr(qc, "tool_call_limit", None)
                usage_line = (
                    f"[usage] prompt={prompt_tokens} "
                    f"completion={completion_tokens} total={total} "
                    f"tool_calls={tool_calls_used}"
                )
                if tool_call_limit is not None:
                    usage_line += f"/{tool_call_limit}"
                usage_line += f" final_context={final_context_tokens}"
                background_tool_names = _background_tool_names_from_messages(
                    list(agent.display_messages)
                )
                if background_tool_names:
                    bg_counts = Counter(background_tool_names)
                    bg_summary = ", ".join(
                        f"{name}={count}" for name, count in sorted(bg_counts.items())
                    )
                    usage_line += f" background_tools={bg_summary}"
                if compacted_before is not None:
                    compactions_delta = f"+{new_compactions}" if new_compactions > 0 else str(new_compactions)
                    usage_line += (
                        f" compactions={compactions_delta}"
                        f"(total={compacted_total})"
                    )
                printer.raw_line(
                    effective_defn.name,
                    usage_line,
                )
            tool_names = _tool_names_from_messages(list(agent.display_messages))
            background_tool_names = _background_tool_names_from_messages(
                list(agent.display_messages)
            )
            _append_benchmark_event(
                team_metrics,
                {
                    "event": "agent_complete",
                    "team_run_id": ctx.tool_metadata.get("team_run_id"),
                    "work_item_id": ctx.tool_metadata.get("work_item_id"),
                    "agent_run_id": tracker.run_id,
                    "agent": effective_defn.name,
                    "status": "failed" if run_error else "completed",
                    "prompt_tokens": int(getattr(agent.total_usage, "input_tokens", 0) or 0),
                    "completion_tokens": int(getattr(agent.total_usage, "output_tokens", 0) or 0),
                    "total_tokens": int(getattr(agent.total_usage, "input_tokens", 0) or 0)
                    + int(getattr(agent.total_usage, "output_tokens", 0) or 0),
                    "tool_calls_used": int(getattr(qc, "tool_calls_used", 0) or 0),
                    "tool_call_limit": getattr(qc, "tool_call_limit", None),
                    "tool_names": tool_names,
                    "tool_counts": dict(Counter(tool_names)),
                    "background_tool_names": background_tool_names,
                    "background_tool_counts": dict(Counter(background_tool_names)),
                    "final_context_tokens": final_context_tokens,
                    "compactions_added": new_compactions,
                    "compacted": compacted_total,
                },
            )

        if run_error is None:
            _enforce_validation_evidence(
                effective_defn.name,
                list(agent.display_messages),
            )

        if team_metrics is not None:
            team_metrics["agent_runs"] = int(team_metrics.get("agent_runs", 0)) + 1
            counts = team_metrics.setdefault("agent_counts", Counter())
            counts[defn.name] += 1

        checkpoint_id = None
        if run_error is None:
            try:
                from team.runtime.registry import get as get_team_run

                team_run_id = ctx.tool_metadata.get("team_run_id")
                team_run = get_team_run(team_run_id) if team_run_id else None
                if team_run is not None and effective_defn.name in {TEAM_PLANNER, "developer", "validator"}:
                    checkpoint_label = (
                        f"{effective_defn.name}:{ctx.tool_metadata.get('work_item_id') or tracker.run_id or 'run'}"
                    )
                    checkpoint_id = await team_run.checkpoint(label=checkpoint_label)
                    retry_count_total = sum(
                        int(getattr(wi, "retry_count", 0) or 0)
                        for wi in team_run.dispatcher.graph.values()
                    )
                    replans_used = int(getattr(team_run.budget_state, "replans_used", 0) or 0)
                    try:
                        repo_patch = await capture_sweevo_repo_patch(
                            team_run.sandbox_id or sandbox_id,
                            repo_dir=repo_dir,
                        )
                        team_run.event_store.append(
                            make_checkpoint_repo_state(
                                team_run.id,
                                checkpoint_id=checkpoint_id,
                                repo_patch=repo_patch,
                            )
                        )
                    except Exception:
                        logger.debug(
                            "Failed to capture repo patch for checkpoint %s",
                            checkpoint_id,
                            exc_info=True,
                        )
                    if team_metrics is not None:
                        team_metrics.setdefault("checkpoint_ids", []).append(checkpoint_id)
                        team_metrics.setdefault("checkpoints", []).append(
                            {
                                "id": checkpoint_id,
                                "label": checkpoint_label,
                                "parent_run": team_run.id,
                                "retry_count_total": retry_count_total,
                                "replans_used": replans_used,
                            }
                        )
                    _append_benchmark_event(
                        team_metrics,
                        {
                            "event": "checkpoint",
                            "team_run_id": team_run.id,
                            "checkpoint_id": checkpoint_id,
                            "label": checkpoint_label,
                            "parent_run": team_run.id,
                            "agent": effective_defn.name,
                            "work_item_id": ctx.tool_metadata.get("work_item_id"),
                            "agent_run_id": tracker.run_id,
                            "retry_count_total": retry_count_total,
                            "replans_used": replans_used,
                        },
                    )
                    if printer is not None:
                        printer.raw_line(
                            effective_defn.name,
                            (
                                "[checkpoint] "
                                f"id={checkpoint_id} label={checkpoint_label} "
                                f"parent_run={team_run.id} "
                                f"retries={retry_count_total} replans={replans_used}"
                            ),
                        )
            except Exception:
                logger.debug("Failed to checkpoint after %s", effective_defn.name, exc_info=True)

        return {
            "agent": effective_defn.name,
            "final_text": final_text,
            "team_run_id": ctx.tool_metadata.get("team_run_id"),
            "work_item_id": ctx.tool_metadata.get("work_item_id"),
            "agent_run_id": ctx.tool_metadata.get("agent_run_id"),
            "checkpoint_id": checkpoint_id,
        }

    return _run


def _emit_dispatcher_dag(
    printer: MultiAgentEventPrinter | None,
    team_run: TeamRun,
    *,
    trigger_agent: str,
) -> None:
    if printer is None:
        return
    graph = team_run.dispatcher.graph
    by_id = graph
    printer.raw_line(
        "team",
        f"[dag] after={trigger_agent} nodes={len(graph)}",
    )
    ordered = sorted(
        graph.values(),
        key=lambda wi: (wi.depth, wi.created_at, wi.id),
    )
    for wi in ordered:
        deps = [
            dep_id[:8]
            for dep_id in wi.deps
        ]
        label = wi.id[:8]
        printer.raw_line(
            "team",
            (
                "[dag] "
                f"{label} agent={wi.agent_name} status={wi.status.value} "
                f"depth={wi.depth} deps={deps or []}"
            ),
        )


def _build_runtime_metadata(
    *,
    sandbox_id: str,
    repo_dir: str,
    base: dict[str, Any] | None = None,
) -> dict[str, Any]:
    meta = dict(base or {})
    meta["sandbox_id"] = sandbox_id
    meta["daytona_cwd"] = repo_dir
    meta["ci_workspace_root"] = repo_dir
    meta["team_mode_enabled"] = True
    meta["require_declared_shell_outputs"] = True
    meta["verification_surface_write_enforcement"] = "warn"
    return meta


def _make_context_builders(
    sandbox_id: str,
    repo_dir: str = _REPO_DIR,
):
    sandbox_note = (
        "## Sandbox Working Directory\n"
        f"- Repo root inside the sandbox: {repo_dir}\n"
        "- `daytona_codeact`, `daytona_read_file`, `daytona_edit_file`, and related "
        "tools already execute relative to that repo root when you use relative paths.\n"
        "- Do not prepend guessed roots such as `/workspace`, `/home/user`, or "
        "`/home/user/repos/...` unless the payload names a real child directory.\n\n"
    )

    async def build_query_ctx(defn, team_run, wi):
        if wi.depth == 0 and wi.agent_name == TEAM_PLANNER:
            # The root planner already receives the benchmark prompt via
            # ``team_run.user_request``. Re-rendering the full payload here
            # duplicates the prompt and the full FAIL_TO_PASS list.
            base_prompt = team_run.user_request
        else:
            base_prompt = sandbox_note + _task_base_prompt(wi.task)
        user_message = await build_initial_user_message(team_run, wi, base_prompt)
        meta = _build_runtime_metadata(
            sandbox_id=team_run.sandbox_id or sandbox_id,
            repo_dir=repo_dir,
            base=build_work_item_metadata(team_run, wi),
        )
        try:
            get_code_intelligence(
                sandbox_id=team_run.sandbox_id or sandbox_id,
                workspace_root=repo_dir,
            )
        except Exception:
            pass
        return TeamAgentContext(user_message=user_message, tool_metadata=meta)

    return build_query_ctx


def _make_executor_factory(
    session_config: Any,
    sandbox_id: str,
    printer: MultiAgentEventPrinter | None,
    *,
    repo_dir: str = _REPO_DIR,
    team_metrics: dict[str, Any] | None = None,
    agent_overrides: dict[str, dict[str, Any]] | None = None,
):
    runner = _make_runner(
        session_config,
        sandbox_id,
        printer,
        team_metrics=team_metrics,
        agent_overrides=agent_overrides,
    )
    build_query_ctx = _make_context_builders(
        sandbox_id,
        repo_dir,
    )

    def factory(team_run):
        def after_dispatch(wi, result, _new_items):
            if result.submitted_plan is None or wi.agent_name != TEAM_PLANNER:
                return
            _emit_dispatcher_dag(printer, team_run, trigger_agent=wi.agent_name)

        return Executor(
            team_run=team_run,
            runner=runner,
            build_query_context=build_query_ctx,
            agent_lookup=get_definition,
            after_dispatch=after_dispatch,
        )

    return factory


def _build_agent_overrides(instance: SWEEvoInstance) -> dict[str, dict[str, Any]]:
    def _with_extra_skills(base: list[str], *extra: str) -> list[str]:
        merged = list(base)
        for skill_name in extra:
            if skill_name and skill_name not in merged:
                merged.append(skill_name)
        return merged

    planner_def = get_definition(TEAM_PLANNER)
    agent_overrides: dict[str, dict[str, Any]] = {}
    if planner_def is not None:
        planner_limits = _derive_planner_runtime_limits(instance)
        planner_toolkits = list(planner_def.toolkits or [])
        agent_overrides[TEAM_PLANNER] = {
            "skills": _with_extra_skills(planner_def.skills, "sweevo-project-context"),
            "toolkits": planner_toolkits,
            **planner_limits,
        }
    developer_def = get_definition(DEVELOPER)
    if developer_def is not None:
        agent_overrides[DEVELOPER] = {
            "skills": _with_extra_skills(developer_def.skills, "sweevo-project-context"),
            **_derive_execution_runtime_limits(instance),
        }
    scout_def = get_definition(SCOUT)
    if scout_def is not None:
        agent_overrides[SCOUT] = {
            "skills": _with_extra_skills(scout_def.skills, "sweevo-project-context"),
            **_derive_execution_runtime_limits(instance),
        }
    validator_def = get_definition(VALIDATOR)
    if validator_def is not None:
        agent_overrides[VALIDATOR] = {
            "skills": _with_extra_skills(
                validator_def.skills,
                "sweevo-project-context",
                "verification-replan",
            ),
            **_derive_execution_runtime_limits(instance),
        }
    replanner_def = get_definition(TEAM_REPLANNER)
    if replanner_def is not None:
        agent_overrides[TEAM_REPLANNER] = {
            "skills": _with_extra_skills(
                replanner_def.skills,
                "sweevo-project-context",
            ),
            **_derive_execution_runtime_limits(instance),
        }
    return agent_overrides


def _emit_team_runtime_banner(
    printer: MultiAgentEventPrinter | None,
    *,
    budgets: BudgetConfig,
) -> None:
    if printer is None:
        return
    printer.raw_line(
        "team",
        (
            "[planning_budget] "
            f"max_plan_size={budgets.max_plan_size} max_depth={budgets.max_depth} "
            f"max_tasks={budgets.max_tasks}"
        ),
    )


def _emit_team_identity_banner(
    printer: MultiAgentEventPrinter | None,
    *,
    team_run_id: str,
    session_id: str,
    sandbox_id: str,
) -> None:
    if printer is None:
        return
    printer.raw_line(
        "team",
        (
            "[run_ids] "
            f"team_run_id={team_run_id} "
            f"session_id={session_id} "
            f"sandbox_id={sandbox_id}"
        ),
    )


def _build_team_metrics() -> dict[str, Any]:
    return {
        "agent_runs": 0,
        "agent_counts": Counter(),
        "checkpoint_ids": [],
        "checkpoints": [],
        "structured_log_path": None,
    }


def _prepare_benchmark_session(
    *,
    repo_dir: str,
    session_id: str | None = None,
) -> tuple[Any, object | None]:
    from config.model_config import get_active_model_kwargs
    from server.app_factory import (
        build_session_config,
        ensure_runtime_stores_ready,
        session_store,
    )

    session_config = build_session_config()
    session_config.cwd = repo_dir
    if session_id:
        session_config.session_id = session_id
    session_factory = ensure_runtime_stores_ready()
    try:
        session_store.upsert(
            session_id=session_config.session_id,
            cwd=repo_dir,
            model=str(get_active_model_kwargs().get("model") or ""),
            message_count=0,
        )
    except Exception:
        logger.debug("Failed to ensure sweevo team session row", exc_info=True)
    return session_config, session_factory


def _finalize_team_result(
    *,
    tr: TeamRun,
    session_config: Any,
    team_metrics: dict[str, Any],
    budgets: BudgetConfig,
    printer: MultiAgentEventPrinter | None,
    checkpoint_records: list[dict[str, Any]] | None = None,
    resumed_from: str | None = None,
    resumed_from_checkpoint: str | None = None,
) -> dict[str, Any]:
    status = tr.status
    task_count = len(tr.dispatcher.graph)
    logger.info(
        "sweevo team run %s finished: status=%s tasks=%d",
        tr.id,
        getattr(status, "value", status),
        task_count,
    )
    if status != TeamRunStatus.SUCCEEDED:
        failures = [
            wi for wi in tr.dispatcher.graph.values() if wi.status.value == "failed"
        ]
        for wi in failures:
            logger.warning(
                "sweevo failed task: id=%s agent=%s reason=%s",
                wi.id,
                wi.agent_name,
                wi.failure_reason,
            )
            if printer is not None:
                printer.raw_line(
                    "team",
                    (
                        "[failed_task] "
                        f"agent={wi.agent_name} id={wi.id[:8]} "
                        f"reason={wi.failure_reason or 'unknown'}"
                    ),
                )

    resolved_checkpoint_records = checkpoint_records or [
        {
            "id": cp.id,
            "label": cp.label,
            "sequence": cp.sequence,
        }
        for cp in tr.dispatcher.list_checkpoints()
    ]
    resolved_checkpoint_ids = [
        str(record.get("id") or "").strip()
        for record in resolved_checkpoint_records
        if str(record.get("id") or "").strip()
    ]
    max_depth_reached = max((wi.depth for wi in tr.dispatcher.graph.values()), default=0)
    retry_count_total = sum(int(getattr(wi, "retry_count", 0) or 0) for wi in tr.dispatcher.graph.values())
    replans_used = int(getattr(tr.budget_state, "replans_used", 0) or 0)
    usage_summary = None
    usage_by_model: list[dict[str, Any]] = []
    try:
        from server.app_factory import usage_store

        if usage_store is not None and getattr(usage_store, "is_ready", False):
            usage_summary = usage_store.get_session_usage(session_config.session_id)
            usage_by_model = usage_store.get_usage_by_model(session_config.session_id)
    except Exception:
        logger.debug("Failed to load sweevo token usage summary", exc_info=True)

    if printer is not None and usage_summary is not None:
        printer.raw_line(
            "team",
            (
                "[team_usage] "
                f"prompt={usage_summary['prompt_tokens']} "
                f"completion={usage_summary['completion_tokens']} "
                f"total={usage_summary['total_tokens']} "
                f"run_rows={usage_summary.get('run_count', usage_summary.get('call_count', 0))}"
            ),
        )
        printer.raw_line(
            "team",
            (
                "[team_stats] "
                f"tasks={task_count} max_depth={max_depth_reached} "
                f"agent_runs={team_metrics['agent_runs']} "
                f"checkpoints={len(resolved_checkpoint_ids)} "
                f"retries={retry_count_total} replans={replans_used}"
            ),
        )
    _append_benchmark_event(
        team_metrics,
        {
            "event": "team_result",
            "team_run_id": tr.id,
            "sandbox_id": tr.sandbox_id,
            "session_id": session_config.session_id,
            "status": getattr(status, "value", status),
            "work_items": task_count,
            "max_depth_reached": max_depth_reached,
            "agent_runs": int(team_metrics["agent_runs"]),
            "agent_counts": dict(team_metrics["agent_counts"]),
            "checkpoint_ids": resolved_checkpoint_ids,
            "latest_checkpoint_id": resolved_checkpoint_ids[-1] if resolved_checkpoint_ids else None,
            "retry_count_total": retry_count_total,
            "replans_used": replans_used,
            "usage": usage_summary,
            "usage_by_model": usage_by_model,
            "resumed_from": resumed_from,
            "resumed_from_checkpoint": resumed_from_checkpoint,
        },
    )

    return {
        "status": status,
        "work_items": task_count,
        "team_run_id": tr.id,
        "sandbox_id": tr.sandbox_id,
        "session_id": session_config.session_id,
        "structured_log_path": team_metrics.get("structured_log_path"),
        "usage": usage_summary,
        "usage_by_model": usage_by_model,
        "checkpoints": resolved_checkpoint_records,
        "checkpoint_ids": resolved_checkpoint_ids,
        "latest_checkpoint_id": resolved_checkpoint_ids[-1] if resolved_checkpoint_ids else None,
        "latest_checkpoint_label": (
            resolved_checkpoint_records[-1].get("label")
            if resolved_checkpoint_records
            else None
        ),
        "max_depth_reached": max_depth_reached,
        "agent_runs": int(team_metrics["agent_runs"]),
        "agent_counts": dict(team_metrics["agent_counts"]),
        "retry_count_total": retry_count_total,
        "replans_used": replans_used,
        "budgets": {
            "max_tasks": budgets.max_tasks,
            "max_depth": budgets.max_depth,
            "max_plan_size": budgets.max_plan_size,
        },
        "resumed_from": resumed_from,
        "resumed_from_checkpoint": resumed_from_checkpoint,
    }


# ---------------------------------------------------------------------------
# Public entry point
# ---------------------------------------------------------------------------


async def run_sweevo_team(
    instance: SWEEvoInstance,
    sandbox_id: str,
    *,
    repo_dir: str = _REPO_DIR,
    printer: MultiAgentEventPrinter | None = None,
    num_executors: int = _DEFAULT_NUM_EXECUTORS,
    structured_log_path: str | None = None,
) -> dict[str, Any]:
    """Run the builtin planner/developer/validator team against the sandbox.

    Returns a metrics dict including ``status`` and ``work_items`` (task count).
    Does not raise on team failure — the caller grades the result via
    the sweevo test command.
    """
    _ensure_team_builtins()

    session_config, session_factory = _prepare_benchmark_session(repo_dir=repo_dir)
    event_store = _build_benchmark_event_store(session_factory=session_factory)
    team_def = _load_or_create_team_definition(session_factory)
    root_prompt = _build_root_prompt(instance, repo_dir)
    budgets = _derive_sweevo_budgets(instance)
    agent_overrides = _build_agent_overrides(instance)
    team_metrics = _build_team_metrics()
    team_metrics["structured_log_path"] = structured_log_path
    _emit_team_runtime_banner(printer, budgets=budgets)

    tr = TeamRun(
        session_id=getattr(session_config, "session_id", "sweevo"),
        user_request=root_prompt,
        budgets=budgets,
        sandbox_id=sandbox_id,
        repo_root=repo_dir,
        event_store=event_store,
    )
    tr.coordination_metadata = {
        "team_mode_enabled": True,
        "require_declared_shell_outputs": True,
        "verification_surface_write_enforcement": "warn",
    }
    _emit_team_identity_banner(
        printer,
        team_run_id=tr.id,
        session_id=getattr(session_config, "session_id", "sweevo"),
        sandbox_id=sandbox_id,
    )
    _append_benchmark_event(
        team_metrics,
        {
            "event": "team_start",
            "team_run_id": tr.id,
            "session_id": getattr(session_config, "session_id", "sweevo"),
            "sandbox_id": sandbox_id,
            "instance_id": instance.instance_id,
            "repo": instance.repo,
            "repo_dir": repo_dir,
            "budgets": {
                "max_tasks": budgets.max_tasks,
                "max_depth": budgets.max_depth,
                "max_plan_size": budgets.max_plan_size,
            },
        },
    )

    await tr.start_with_team_definition(
        team_def,
        payload={
            "task": "Produce the initial root plan for this SWE-EVO benchmark instance.",
            "prompt": root_prompt,
            "instance_id": instance.instance_id,
            "repo": instance.repo,
            "repo_dir": repo_dir,
            "test_cmds": instance.test_cmds,
            "fail_to_pass": instance.fail_to_pass,
            "pass_to_pass": instance.pass_to_pass,
        },
        executor_factory=_make_executor_factory(
            session_config,
            sandbox_id,
            printer,
            repo_dir=repo_dir,
            team_metrics=team_metrics,
            agent_overrides=agent_overrides,
        ),
        num_executors=num_executors,
    )

    await tr.wait()
    checkpoint_records = _checkpoint_records_from_store(event_store, tr.id)
    return _finalize_team_result(
        tr=tr,
        session_config=session_config,
        team_metrics=team_metrics,
        budgets=budgets,
        printer=printer,
        checkpoint_records=checkpoint_records,
    )


async def resume_sweevo_team(
    instance: SWEEvoInstance,
    team_run_id: str,
    *,
    repo_dir: str = _REPO_DIR,
    printer: MultiAgentEventPrinter | None = None,
    num_executors: int = _DEFAULT_NUM_EXECUTORS,
    checkpoint_id: str | None = None,
    use_latest_checkpoint: bool = False,
    structured_log_path: str | None = None,
) -> dict[str, Any]:
    """Resume a persisted SWE-EVO TeamRun in a fresh process."""
    _ensure_team_builtins()

    from server.app_factory import ensure_runtime_stores_ready

    session_factory = ensure_runtime_stores_ready()
    event_store = _build_benchmark_event_store(session_factory=session_factory)
    initial_checkpoint_records = _checkpoint_records_from_store(event_store, team_run_id)
    checkpoint_ids = [record["id"] for record in initial_checkpoint_records]
    resolved_checkpoint_id = checkpoint_id
    if resolved_checkpoint_id is None and use_latest_checkpoint and checkpoint_ids:
        resolved_checkpoint_id = checkpoint_ids[-1]
    if resolved_checkpoint_id is None:
        tr = TeamRun.resume_from(event_store, team_run_id)
    else:
        tr = TeamRun.resume_from(
            event_store,
            team_run_id,
            checkpoint_id=resolved_checkpoint_id,
        )
    if not tr.sandbox_id:
        raise ValueError(
            f"team run {team_run_id!r} cannot be resumed: missing sandbox_id in persisted header"
        )
    if resolved_checkpoint_id:
        await setup_sweevo_sandbox(instance, tr.sandbox_id, repo_dir)
        await ensure_sweevo_test_patch(instance, tr.sandbox_id, repo_dir)
        repo_patch = _checkpoint_repo_patch_from_store(
            event_store,
            team_run_id,
            resolved_checkpoint_id,
        )
        if repo_patch:
            await apply_sweevo_repo_patch(tr.sandbox_id, repo_patch, repo_dir)
        if printer is not None:
            patch_info = (
                f"repo_patch_bytes={len(repo_patch.encode('utf-8'))}"
                if repo_patch
                else "repo_patch=<missing>"
            )
            printer.raw_line(
                "team",
                (
                    "[resume_restore] "
                    f"checkpoint={resolved_checkpoint_id} {patch_info} "
                    "benchmark_patch=reapplied"
                ),
            )

    session_config, _ = _prepare_benchmark_session(
        repo_dir=repo_dir,
        session_id=tr.session_id or None,
    )
    budgets = tr.budgets
    agent_overrides = _build_agent_overrides(instance)
    team_metrics = _build_team_metrics()
    team_metrics["structured_log_path"] = structured_log_path
    _emit_team_runtime_banner(printer, budgets=budgets)
    checkpoint_label = ""
    retry_count_total = sum(
        int(getattr(wi, "retry_count", 0) or 0)
        for wi in tr.dispatcher.graph.values()
    )
    budget_state = getattr(tr, "budget_state", None)
    replans_used = int(getattr(budget_state, "replans_used", 0) or 0)
    if printer is not None:
        checkpoint_label = next(
            (
                str(record.get("label") or "")
                for record in initial_checkpoint_records
                if record.get("id") == resolved_checkpoint_id
            ),
            "",
        )
        printer.raw_line(
            "team",
            (
                "[resume] "
                f"team_run_id={team_run_id} sandbox_id={tr.sandbox_id} "
                f"durable_checkpoints={len(checkpoint_ids)} "
                f"checkpoint={resolved_checkpoint_id or '<latest-state>'} "
                f"resumed_from={team_run_id} "
                f"resumed_from_checkpoint={resolved_checkpoint_id or '<latest-state>'} "
                f"retries={retry_count_total} replans={replans_used}"
                f"{f' label={checkpoint_label}' if checkpoint_label else ''}"
            ),
        )
    _append_benchmark_event(
        team_metrics,
        {
            "event": "resume",
            "team_run_id": team_run_id,
            "sandbox_id": tr.sandbox_id,
            "instance_id": instance.instance_id,
            "checkpoint_id": resolved_checkpoint_id,
            "durable_checkpoint_count": len(checkpoint_ids),
            "retry_count_total": retry_count_total,
            "replans_used": replans_used,
        },
    )
    _emit_team_identity_banner(
        printer,
        team_run_id=tr.id,
        session_id=getattr(session_config, "session_id", "sweevo"),
        sandbox_id=tr.sandbox_id,
    )

    await tr.resume(
        executor_factory=_make_executor_factory(
            session_config,
            tr.sandbox_id,
            printer,
            repo_dir=repo_dir,
            team_metrics=team_metrics,
            agent_overrides=agent_overrides,
        ),
        num_executors=num_executors,
        resumed_from=team_run_id,
        resumed_from_checkpoint=resolved_checkpoint_id,
    )
    await tr.wait()
    checkpoint_records = _checkpoint_records_from_store(event_store, tr.id)
    return _finalize_team_result(
        tr=tr,
        session_config=session_config,
        team_metrics=team_metrics,
        budgets=budgets,
        printer=printer,
        checkpoint_records=checkpoint_records,
        resumed_from=team_run_id,
        resumed_from_checkpoint=resolved_checkpoint_id,
    )
