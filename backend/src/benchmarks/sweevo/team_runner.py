"""Wire a real team run over a provisioned SWE-EVO sandbox.

Drives :class:`team.runtime.team_run.TeamRun` with the builtin
``team_planner`` / ``developer`` / ``validator`` agents from
``team.builtins``. Each WorkItem's agent is spawned through
:func:`engine.runtime.agent.spawn_agent` so it runs with its full
production tool surface (``sandbox_operations``, ``code_intelligence``,
skills, posthook tools) against the Daytona sandbox that was already
prepared by :func:`benchmarks.sweevo.sandbox.create_sweevo_test_sandbox`.
"""

from __future__ import annotations

from collections import Counter
import json
import logging
from pathlib import Path
from typing import Any

from agents.run_tracker import AgentRunTracker
from agents.registry import get_definition
from config.paths import get_project_config_dir
from engine.runtime.agent import spawn_agent
from message.event_printer import MultiAgentEventPrinter
from message.messages import ConversationMessage, ToolUseBlock
from message.stream_events import ToolExecutionCompleted
from token_tracker.runtime import persist_run_usage
from team.builtins import DEVELOPER, TEAM_PLANNER, VALIDATOR, register_all as _register_team_builtins
from team.atlas.scheduler import AtlasMaintenanceScheduler
from team.models import BudgetConfig, TeamRunStatus, WorkItemKind
from team.persistence.run_store import build_default_store
from team.runtime.context_builder import (
    TeamAgentContext,
    build_initial_user_message,
    build_work_item_metadata,
    render_work_item_payload,
)
from team.runtime.executor import Executor
from team.runtime.team_run import TeamRun

from benchmarks.sweevo.dataset import summarize_sweevo_instance
from benchmarks.sweevo.models import SWEEvoInstance, _REPO_DIR

logger = logging.getLogger(__name__)

# Default pool size for the team's Executor workers. Not a cap — callers
# can still override.
_DEFAULT_NUM_EXECUTORS = 8
_PROJECT_ROOT = Path(__file__).resolve().parents[4]


def _benchmark_team_run_dir() -> Path:
    """Return the benchmark-owned TeamRun event log directory."""
    return get_project_config_dir(_PROJECT_ROOT) / "team-runs"


def _build_benchmark_event_store(*, session_factory: object | None) -> Any:
    """Prefer DB-backed durability, else fall back to a stable project-local JSONL log."""
    if session_factory is not None:
        return build_default_store(session_factory=session_factory)
    return build_default_store(base_dir=_benchmark_team_run_dir())


def _checkpoint_ids_from_store(store: Any, team_run_id: str) -> list[str]:
    checkpoint_ids: list[str] = []
    for event in store.load_run(team_run_id):
        if event.kind != "checkpoint_taken":
            continue
        checkpoint_id = str(event.data.get("checkpoint_id") or "").strip()
        if checkpoint_id and checkpoint_id not in checkpoint_ids:
            checkpoint_ids.append(checkpoint_id)
    return checkpoint_ids


# ---------------------------------------------------------------------------
# Prompt construction
# ---------------------------------------------------------------------------


def _recommended_frontier_cap(instance: SWEEvoInstance) -> int:
    size = str(summarize_sweevo_instance(instance).get("size") or "medium")
    size_cap = 3 if size == "large" else 2
    return max(1, min(size_cap, len(instance.fail_to_pass) or 1))


def _derive_sweevo_budgets(instance: SWEEvoInstance) -> BudgetConfig:
    """Return size-aware team budgets for SWE-EVO instead of disabling them."""
    summary = summarize_sweevo_instance(instance)
    size = str(summary.get("size") or "medium")
    f2p_targets = max(1, len(instance.fail_to_pass))

    base = {
        "small": {
            "max_depth": 4,
            "max_plan_size": 8,
            "max_work_items": 24,
            "max_shared_briefings": 8,
            "max_briefing_bytes": 24_000,
        },
        "medium": {
            "max_depth": 5,
            "max_plan_size": 12,
            "max_work_items": 40,
            "max_shared_briefings": 12,
            "max_briefing_bytes": 48_000,
        },
        "large": {
            "max_depth": 6,
            "max_plan_size": 16,
            "max_work_items": 64,
            "max_shared_briefings": 16,
            "max_briefing_bytes": 64_000,
        },
    }.get(size, {
        "max_depth": 5,
        "max_plan_size": 12,
        "max_work_items": 40,
        "max_shared_briefings": 12,
        "max_briefing_bytes": 48_000,
    })

    plan_size = min(24, int(base["max_plan_size"]) + max(0, min(4, f2p_targets - 1)))
    work_items = max(int(base["max_work_items"]), plan_size * int(base["max_depth"]))
    return BudgetConfig(
        max_work_items=work_items,
        max_depth=int(base["max_depth"]),
        max_plan_size=plan_size,
        max_artifact_bytes=1_000_000,
        max_total_artifact_bytes=50_000_000,
        default_work_item_timeout=None,
        max_briefing_bytes=int(base["max_briefing_bytes"]),
        max_shared_briefings=int(base["max_shared_briefings"]),
    )


def _derive_atlas_parallelism(instance: SWEEvoInstance, *, num_executors: int) -> int:
    del instance, num_executors
    # SWE-EVO runs are dominated by benchmark-critical planner/developer/validator
    # work. Atlas maintenance currently adds substantial background churn and token
    # burn without helping the grading path reliably enough to justify it.
    return 0


def _derive_planner_runtime_limits(instance: SWEEvoInstance) -> dict[str, int]:
    """Return benchmark-specific planner limits."""
    del instance
    tool_call_limit = 100
    max_turns = max(48, tool_call_limit * 4)
    return {
        "tool_call_limit": tool_call_limit,
        "max_turns": max_turns,
    }


def _build_root_prompt(instance: SWEEvoInstance, repo_dir: str) -> str:
    summary = summarize_sweevo_instance(instance)
    size = str(summary.get("size") or "medium")
    frontier_cap = _recommended_frontier_cap(instance)
    file_counts: Counter[str] = Counter()
    for test_id in instance.fail_to_pass:
        file_counts[test_id.split("::", 1)[0]] += 1
    rendered_clusters = "\n".join(
        f"- {path}: {count} fail-to-pass target(s)"
        for path, count in sorted(file_counts.items())
    )
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
        f"## Fail-To-Pass Summary\n"
        f"{rendered_clusters}\n\n"
        f"## Fail-To-Pass Targets\n"
        f"{json.dumps(instance.fail_to_pass, indent=2)}\n\n"
        f"## Pass-To-Pass Guardrail\n"
        f"{json.dumps(instance.pass_to_pass, indent=2)}\n\n"
        f"## Grading command\n"
        f"After your team finishes, this exact command will be executed in the sandbox "
        f"to grade the work:\n```\n{instance.test_cmds}\n```\n\n"
        f"## Runtime Notes\n"
        f"- Instance size: {size} ({summary.get('bullet_count', 0)} changelog bullets, "
        f"{len(instance.fail_to_pass)} fail-to-pass target(s)).\n"
        f"- Recommended first-ready frontier cap: {frontier_cap} benchmark-critical "
        f"implementation lane(s).\n"
        f"- Stable SWE-EVO workflow policy lives in the declared skills for this run; "
        f"use the test targets and grading command above as the source of truth.\n"
        f"- release notes are intentionally omitted from the root planner prompt; "
        f"do not treat changelog prose as an implementation checklist.\n"
        f"- When debugging runtime, coordination, retry, or checkpoint behavior, "
        f"refer to the benchmark run log file under `.ephemeralos/benchmark-logs/` "
        f"as an extra source for debugging context. Use it as supporting evidence, "
        f"not as a replacement for the live workspace and current test output.\n"
        f"- the root planner must not inspect dependency/version metadata once the "
        f"live owner surface is clear.\n"
        f"\n- Fix the repository checkout itself. Do not rely on ad hoc sandbox-only "
        f"package upgrades or ambient environment mutations as the benchmark fix.\n"
        f"- Stay inside {repo_dir} and treat the named tests as reproduction signals, not as a "
        f"reason to restate the changelog or inspect unrelated manifests from the root planner."
    )


def _work_item_base_prompt(payload: Any) -> str:
    rendered = render_work_item_payload(payload)
    if rendered is not None:
        return rendered
    return f"Payload: {payload!r}"


def _extract_final_text(messages: list[Any]) -> str:
    """Return the last assistant text emitted by an agent run."""
    for msg in reversed(messages):
        if getattr(msg, "role", None) != "assistant":
            continue
        text = getattr(msg, "text", "")
        if text:
            return str(text).strip()
    return ""


def _extract_last_json_object(text: str) -> dict[str, Any] | None:
    stripped = text.strip()
    if not stripped:
        return None

    decoder = json.JSONDecoder()
    candidates = [stripped]
    first_brace = stripped.find("{")
    last_brace = stripped.rfind("}")
    if 0 <= first_brace < last_brace:
        inner = stripped[first_brace : last_brace + 1]
        if inner != stripped:
            candidates.append(inner)

    for candidate in candidates:
        try:
            payload, end = decoder.raw_decode(candidate)
        except ValueError:
            continue
        if isinstance(payload, dict) and candidate[end:].strip() == "":
            return payload
    return None


def _matches_posthook_payload(payload: dict[str, Any], metadata_key: str) -> bool:
    if metadata_key == "submitted_plan":
        return isinstance(payload.get("items"), list)
    if metadata_key == "submitted_summary":
        return isinstance(payload.get("summary"), str)
    if metadata_key == "submitted_atlas":
        return isinstance(payload.get("chunks"), list)
    return False


def _extract_posthook_input_text(
    messages: list[ConversationMessage] | list[dict[str, Any]],
    metadata_key: str | None,
) -> str | None:
    """Return the latest assistant JSON payload accepted by the posthook."""
    if not metadata_key:
        return None

    def _message_role(msg: ConversationMessage | dict[str, Any]) -> str | None:
        if isinstance(msg, dict):
            role = msg.get("role")
            return role if isinstance(role, str) else None
        return getattr(msg, "role", None)

    def _message_blocks(msg: ConversationMessage | dict[str, Any]) -> list[Any]:
        if isinstance(msg, dict):
            content = msg.get("content")
            return content if isinstance(content, list) else []
        return list(getattr(msg, "content", []))

    def _block_text(block: Any) -> str | None:
        if isinstance(block, dict):
            text = block.get("text")
            return text if isinstance(text, str) else None
        text = getattr(block, "text", None)
        return text if isinstance(text, str) else None

    for msg in reversed(messages):
        if _message_role(msg) != "assistant":
            continue
        for block in reversed(_message_blocks(msg)):
            text = _block_text(block)
            if text is None:
                continue
            payload = _extract_last_json_object(text)
            if payload is None or not _matches_posthook_payload(payload, metadata_key):
                continue
            return json.dumps(payload, ensure_ascii=False)
    return None


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


def _enforce_validation_evidence(
    agent_name: str,
    display_messages: list[ConversationMessage],
) -> None:
    if agent_name != VALIDATOR:
        return
    tool_names = _tool_names_from_messages(display_messages)
    if "daytona_bash" in tool_names:
        return
    raise RuntimeError(
        "validator_missing_tool_evidence: validator must execute at least one "
        "daytona_bash command before returning a verdict"
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
        prompt = ctx.user_message or _work_item_base_prompt(None)
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
        # submit_plan / submit_summary tools write into the slot that
        # execute_with_posthook reads back. Preserve session_config and
        # sandbox_id that spawn_agent installed for subagent dispatch.
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
                    f"tool_call_limit={agent.query_context.tool_call_limit} "
                    f"max_turns={agent.query_context.max_turns}"
                ),
            )

        event_count = 0
        run_error: str | None = None
        final_text = ""
        posthook_input_text = None
        try:
            async for event in agent.run(prompt):
                event_count += 1
                if printer is None:
                    continue
                try:
                    object.__setattr__(event, "agent_name", defn.name)
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
            posthook_key = getattr(getattr(effective_defn, "posthook", None), "metadata_key", None)
            posthook_input_text = _extract_posthook_input_text(
                list(agent.display_messages),
                posthook_key,
            )
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
                "posthook_input_text": posthook_input_text,
                "tool_calls_used": int(getattr(qc, "tool_calls_used", 0) or 0),
                "tool_call_limit": getattr(qc, "tool_call_limit", None),
                "final_context_tokens": final_context_tokens,
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
                if compacted_before is not None:
                    usage_line += (
                        f" compactions={'+1' if new_compactions > 0 else '0'}"
                        f"(total={compacted_total})"
                    )
                printer.raw_line(
                    effective_defn.name,
                    usage_line,
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
                    if team_metrics is not None:
                        team_metrics.setdefault("checkpoint_ids", []).append(checkpoint_id)
                    if printer is not None:
                        printer.raw_line(
                            effective_defn.name,
                            f"[checkpoint] id={checkpoint_id} label={checkpoint_label}",
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
            "posthook_input_text": posthook_input_text,
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
            by_id.get(dep_id).local_id or dep_id[:8]
            if by_id.get(dep_id) is not None
            else dep_id[:8]
            for dep_id in wi.deps
        ]
        label = wi.local_id or wi.id[:8]
        printer.raw_line(
            "team",
            (
                "[dag] "
                f"{label} agent={wi.agent_name} kind={wi.kind.value} status={wi.status.value} "
                f"depth={wi.depth} deps={deps or []}"
            ),
        )


def _make_context_builders(
    sandbox_id: str,
    repo_dir: str = _REPO_DIR,
):
    def build_query_ctx(defn, team_run, wi):
        if wi.depth == 0 and wi.agent_name == TEAM_PLANNER:
            # The root planner already receives the benchmark prompt via
            # ``team_run.user_request``. Re-rendering the full payload here
            # duplicates the prompt and the full FAIL_TO_PASS list.
            base_prompt = team_run.user_request
        else:
            base_prompt = _work_item_base_prompt(wi.payload)
        user_message = build_initial_user_message(team_run, wi, base_prompt)
        meta = build_work_item_metadata(team_run, wi)
        meta["sandbox_id"] = team_run.sandbox_id or sandbox_id
        meta["daytona_cwd"] = repo_dir
        meta["ci_workspace_root"] = repo_dir
        return TeamAgentContext(user_message=user_message, tool_metadata=meta)

    def build_posthook_ctx(posthook_defn, work_result):
        meta = {
            "agent_name": posthook_defn.name,
            "sandbox_id": sandbox_id,
            "daytona_cwd": repo_dir,
            "ci_workspace_root": repo_dir,
        }
        user_message = _work_item_base_prompt(work_result)
        if isinstance(work_result, dict):
            for key in ("team_run_id", "work_item_id"):
                value = work_result.get(key)
                if value:
                    meta[key] = value
            posthook_input_text = work_result.get("posthook_input_text")
            if isinstance(posthook_input_text, str) and posthook_input_text.strip():
                user_message = posthook_input_text
            else:
                final_text = work_result.get("final_text")
                if isinstance(final_text, str) and final_text.strip():
                    user_message = final_text
        return TeamAgentContext(
            user_message=user_message,
            tool_metadata=meta,
            work_result=work_result,
        )

    return build_query_ctx, build_posthook_ctx


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
    build_query_ctx, build_posthook_ctx = _make_context_builders(
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
            build_posthook_context=build_posthook_ctx,
            agent_lookup=get_definition,
            after_dispatch=after_dispatch,
        )

    return factory


def _make_atlas_scheduler_factory(
    session_config: Any,
    sandbox_id: str,
    printer: MultiAgentEventPrinter | None,
    *,
    repo_dir: str = _REPO_DIR,
    team_metrics: dict[str, Any] | None = None,
    max_concurrent_jobs: int = 1,
):
    runner = _make_runner(
        session_config,
        sandbox_id,
        printer,
        team_metrics=team_metrics,
    )
    build_query_ctx, build_posthook_ctx = _make_context_builders(
        sandbox_id,
        repo_dir,
    )

    def factory(team_run):
        return AtlasMaintenanceScheduler(
            team_run=team_run,
            runner=runner,
            build_query_context=build_query_ctx,
            build_posthook_context=build_posthook_ctx,
            agent_lookup=get_definition,
            max_concurrent_jobs=max_concurrent_jobs,
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
        agent_overrides[TEAM_PLANNER] = {
            "skills": _with_extra_skills(planner_def.skills, "sweevo-project-context"),
            **planner_limits,
        }
    developer_def = get_definition(DEVELOPER)
    if developer_def is not None:
        agent_overrides[DEVELOPER] = {
            "skills": _with_extra_skills(developer_def.skills, "sweevo-project-context"),
        }
    validator_def = get_definition(VALIDATOR)
    if validator_def is not None:
        agent_overrides[VALIDATOR] = {
            "skills": _with_extra_skills(
                validator_def.skills,
                "sweevo-project-context",
                "verification-replan",
            ),
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
            f"max_work_items={budgets.max_work_items} "
            f"max_shared_briefings={budgets.max_shared_briefings}"
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
    atlas_parallelism: int,
    printer: MultiAgentEventPrinter | None,
    checkpoint_ids: list[str] | None = None,
    resumed_from: str | None = None,
    resumed_from_checkpoint: str | None = None,
) -> dict[str, Any]:
    status = tr.status
    work_items = len(tr.dispatcher.graph)
    logger.info(
        "sweevo team run %s finished: status=%s work_items=%d",
        tr.id,
        getattr(status, "value", status),
        work_items,
    )
    if status != TeamRunStatus.SUCCEEDED:
        failures = [
            wi for wi in tr.dispatcher.graph.values() if wi.status.value == "failed"
        ]
        for wi in failures:
            logger.warning(
                "sweevo failed work item: id=%s agent=%s local_id=%s kind=%s reason=%s",
                wi.id,
                wi.agent_name,
                wi.local_id,
                wi.kind.value,
                wi.failure_reason,
            )
            if printer is not None:
                printer.raw_line(
                    "team",
                    (
                        "[failed_work_item] "
                        f"agent={wi.agent_name} local_id={wi.local_id or '-'} "
                        f"kind={wi.kind.value} reason={wi.failure_reason or 'unknown'}"
                    ),
                )

    resolved_checkpoint_ids = checkpoint_ids or [cp.id for cp in tr.dispatcher.list_checkpoints()]
    max_depth_reached = max((wi.depth for wi in tr.dispatcher.graph.values()), default=0)
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
                f"work_items={work_items} max_depth={max_depth_reached} "
                f"agent_runs={team_metrics['agent_runs']} "
                f"checkpoints={len(resolved_checkpoint_ids)} "
                f"atlas_parallelism={atlas_parallelism}"
            ),
        )

    return {
        "status": status,
        "work_items": work_items,
        "team_run_id": tr.id,
        "sandbox_id": tr.sandbox_id,
        "session_id": session_config.session_id,
        "usage": usage_summary,
        "usage_by_model": usage_by_model,
        "checkpoint_ids": resolved_checkpoint_ids,
        "latest_checkpoint_id": resolved_checkpoint_ids[-1] if resolved_checkpoint_ids else None,
        "max_depth_reached": max_depth_reached,
        "agent_runs": int(team_metrics["agent_runs"]),
        "agent_counts": dict(team_metrics["agent_counts"]),
        "budgets": {
            "max_work_items": budgets.max_work_items,
            "max_depth": budgets.max_depth,
            "max_plan_size": budgets.max_plan_size,
            "max_shared_briefings": budgets.max_shared_briefings,
            "max_briefing_bytes": budgets.max_briefing_bytes,
        },
        "atlas_parallelism": atlas_parallelism,
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
    work_item_timeout: float | None = None,  # noqa: ARG001 — kept for CLI compat; no-op
) -> dict[str, Any]:
    """Run the builtin planner/developer/validator team against the sandbox.

    Returns a metrics dict including ``status`` and ``work_items``.
    Does not raise on team failure — the caller grades the result via
    the sweevo test command.
    """
    try:
        _register_team_builtins()
    except Exception:
        logger.debug("team builtins already registered", exc_info=True)

    session_config, session_factory = _prepare_benchmark_session(repo_dir=repo_dir)
    event_store = _build_benchmark_event_store(session_factory=session_factory)
    root_prompt = _build_root_prompt(instance, repo_dir)
    budgets = _derive_sweevo_budgets(instance)
    agent_overrides = _build_agent_overrides(instance)
    atlas_parallelism = _derive_atlas_parallelism(instance, num_executors=num_executors)
    team_metrics = _build_team_metrics()
    _emit_team_runtime_banner(printer, budgets=budgets)

    tr = TeamRun(
        session_id=getattr(session_config, "session_id", "sweevo"),
        user_request=root_prompt,
        budgets=budgets,
        sandbox_id=sandbox_id,
        repo_root=repo_dir,
        event_store=event_store,
    )
    _emit_team_identity_banner(
        printer,
        team_run_id=tr.id,
        session_id=getattr(session_config, "session_id", "sweevo"),
        sandbox_id=sandbox_id,
    )

    atlas_factory = (
        _make_atlas_scheduler_factory(
            session_config,
            sandbox_id,
            printer,
            repo_dir=repo_dir,
            team_metrics=team_metrics,
            max_concurrent_jobs=atlas_parallelism,
        )
        if atlas_parallelism > 0
        else None
    )

    await tr.start(
        agent_name=TEAM_PLANNER,
        payload={
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
        atlas_scheduler_factory=atlas_factory,
        num_executors=num_executors,
        root_kind=WorkItemKind.EXPANDABLE,
    )

    await tr.wait()
    return _finalize_team_result(
        tr=tr,
        session_config=session_config,
        team_metrics=team_metrics,
        budgets=budgets,
        atlas_parallelism=atlas_parallelism,
        printer=printer,
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
) -> dict[str, Any]:
    """Resume a persisted SWE-EVO TeamRun in a fresh process."""
    try:
        _register_team_builtins()
    except Exception:
        logger.debug("team builtins already registered", exc_info=True)

    from server.app_factory import ensure_runtime_stores_ready

    session_factory = ensure_runtime_stores_ready()
    event_store = _build_benchmark_event_store(session_factory=session_factory)
    checkpoint_ids = _checkpoint_ids_from_store(event_store, team_run_id)
    resolved_checkpoint_id = checkpoint_id
    if resolved_checkpoint_id is None and use_latest_checkpoint and checkpoint_ids:
        resolved_checkpoint_id = checkpoint_ids[-1]
    tr = TeamRun.resume_from(
        event_store,
        team_run_id,
        checkpoint_id=resolved_checkpoint_id,
    )
    if not tr.sandbox_id:
        raise ValueError(
            f"team run {team_run_id!r} cannot be resumed: missing sandbox_id in persisted header"
        )

    session_config, _ = _prepare_benchmark_session(
        repo_dir=repo_dir,
        session_id=tr.session_id or None,
    )
    budgets = tr.budgets
    agent_overrides = _build_agent_overrides(instance)
    atlas_parallelism = _derive_atlas_parallelism(instance, num_executors=num_executors)
    team_metrics = _build_team_metrics()
    _emit_team_runtime_banner(printer, budgets=budgets)
    if printer is not None:
        printer.raw_line(
            "team",
            (
                "[resume] "
                f"team_run_id={team_run_id} sandbox_id={tr.sandbox_id} "
                f"durable_checkpoints={len(checkpoint_ids)} "
                f"checkpoint={resolved_checkpoint_id or '<latest-state>'}"
            ),
        )
    _emit_team_identity_banner(
        printer,
        team_run_id=tr.id,
        session_id=getattr(session_config, "session_id", "sweevo"),
        sandbox_id=tr.sandbox_id,
    )

    atlas_factory = (
        _make_atlas_scheduler_factory(
            session_config,
            tr.sandbox_id,
            printer,
            repo_dir=repo_dir,
            team_metrics=team_metrics,
            max_concurrent_jobs=atlas_parallelism,
        )
        if atlas_parallelism > 0
        else None
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
        atlas_scheduler_factory=atlas_factory,
        num_executors=num_executors,
    )
    await tr.wait()
    return _finalize_team_result(
        tr=tr,
        session_config=session_config,
        team_metrics=team_metrics,
        budgets=budgets,
        atlas_parallelism=atlas_parallelism,
        printer=printer,
        checkpoint_ids=checkpoint_ids,
        resumed_from=team_run_id,
        resumed_from_checkpoint=resolved_checkpoint_id,
    )
