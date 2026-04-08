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

import json
import logging
from typing import Any

from agents.run_tracker import AgentRunTracker
from agents.registry import get_definition
from engine.runtime.agent import spawn_agent
from message.event_printer import MultiAgentEventPrinter
from token_tracker.runtime import persist_run_usage
from team.builtins import TEAM_PLANNER, register_all as _register_team_builtins
from team.models import BudgetConfig, TeamRunStatus, WorkItemKind
from team.runtime.context_builder import (
    TeamAgentContext,
    build_initial_user_message,
    build_work_item_metadata,
)
from team.runtime.executor import Executor
from team.runtime.team_run import TeamRun

from benchmarks.sweevo.models import SWEEvoInstance, _REPO_DIR

logger = logging.getLogger(__name__)


import sys as _sys

# No budget tracking — the sweevo benchmark runs the team until it
# finishes, never until a counter trips. All caps are set to their
# maximum possible values so the dispatcher's budget checks are no-ops.
_UNLIMITED_BUDGETS = BudgetConfig(
    max_work_items=_sys.maxsize,
    max_depth=_sys.maxsize,
    max_plan_size=_sys.maxsize,
    max_artifact_bytes=_sys.maxsize,
    max_total_artifact_bytes=_sys.maxsize,
    default_work_item_timeout=10**9,
    max_briefing_bytes=_sys.maxsize,
    max_shared_briefings=_sys.maxsize,
)

# Default pool size for the team's Executor workers. Not a cap — callers
# can still override.
_DEFAULT_NUM_EXECUTORS = 8


# ---------------------------------------------------------------------------
# Prompt construction
# ---------------------------------------------------------------------------


def _build_root_prompt(instance: SWEEvoInstance, repo_dir: str) -> str:
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
        f"{json.dumps(instance.pass_to_pass, indent=2)}\n\n"
        f"## Context (problem statement / release notes)\n"
        f"This section is background context only. It may mention release notes or changelog entries; "
        f"do not treat it as the implementation checklist. The grading command and test targets above define success.\n"
        f"{instance.problem_statement}\n\n"
        f"## Grading command\n"
        f"After your team finishes, this exact command will be executed in the sandbox "
        f"to grade the work:\n```\n{instance.test_cmds}\n```\n\n"
        f"## Instructions\n"
        f"- Decompose the work into concrete developer and validator WorkItems.\n"
        f"- Developers edit the repo in the sandbox via sandbox_operations tools.\n"
        f"- Stay inside {repo_dir}.\n"
        f"- Do NOT modify test files unless the task explicitly asks for it.\n"
        f"- Start from the failing tests or failing behavior, not from the changelog prose.\n"
        f"- Validators should run the grading command (or a tighter subset) and "
        f"report PASS/FAIL with evidence."
    )


def _work_item_base_prompt(payload: Any) -> str:
    if isinstance(payload, dict):
        for key in ("prompt", "task", "description", "instructions", "final_text"):
            val = payload.get(key)
            if isinstance(val, str) and val.strip():
                return val
        return "Execute the following WorkItem payload:\n" + json.dumps(
            payload, indent=2, default=str
        )
    if isinstance(payload, str):
        return payload
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


# ---------------------------------------------------------------------------
# Runner + executor factory
# ---------------------------------------------------------------------------


def _make_runner(
    session_config: Any,
    sandbox_id: str,
    printer: MultiAgentEventPrinter | None,
):
    async def _run(defn, ctx: TeamAgentContext):
        prompt = ctx.user_message or _work_item_base_prompt(None)
        tracker = AgentRunTracker.create(
            session_id=getattr(session_config, "session_id", None),
            run_id=getattr(ctx.tool_metadata, "agent_run_id", None),
            agent_name=defn.name,
            input_query=prompt,
        )
        if tracker.run_id is not None:
            ctx.tool_metadata.agent_run_id = tracker.run_id

        agent = spawn_agent(
            session_config,
            messages=[],
            agent_def=defn,
            latest_user_prompt=prompt,
            sandbox_id=sandbox_id,
        )

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
        agent.query_context.tool_metadata = ctx.tool_metadata

        event_count = 0
        run_error: str | None = None
        try:
            async for event in agent.run(prompt):
                event_count += 1
                if printer is None:
                    continue
                try:
                    object.__setattr__(event, "agent_name", defn.name)
                except Exception:
                    pass
                try:
                    printer.emit(event)
                except Exception:
                    logger.debug("printer.emit failed", exc_info=True)
        except Exception as exc:
            run_error = str(exc)
            logger.exception("sweevo team runner: agent %s crashed", defn.name)
            raise
        finally:
            qc = getattr(agent, "query_context", None)
            tracker.finish(
                status="failed" if run_error else "completed",
                display_messages=list(agent.display_messages),
                api_messages_snapshot=getattr(qc, "api_messages_snapshot", None),
                error=run_error,
                final_text=_extract_final_text(agent.display_messages),
                event_count=event_count,
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
                    agent_name=defn.name,
                    model_id=agent.model,
                    usage=agent.total_usage,
                )
            if printer is not None and agent.total_usage is not None:
                total = agent.total_usage.input_tokens + agent.total_usage.output_tokens
                printer.raw_line(
                    defn.name,
                    (
                        f"[usage] prompt={agent.total_usage.input_tokens} "
                        f"completion={agent.total_usage.output_tokens} total={total}"
                    ),
                )

        return {
            "agent": defn.name,
            "final_text": _extract_final_text(agent.display_messages),
        }

    return _run


def _make_executor_factory(
    session_config: Any,
    sandbox_id: str,
    printer: MultiAgentEventPrinter | None,
):
    runner = _make_runner(session_config, sandbox_id, printer)

    def build_query_ctx(defn, team_run, wi):
        base_prompt = _work_item_base_prompt(wi.payload)
        user_message = build_initial_user_message(team_run, wi, base_prompt)
        meta = build_work_item_metadata(team_run, wi)
        meta["sandbox_id"] = team_run.sandbox_id or sandbox_id
        return TeamAgentContext(user_message=user_message, tool_metadata=meta)

    def build_posthook_ctx(posthook_defn, work_result):
        return TeamAgentContext(
            user_message=_work_item_base_prompt(work_result),
            tool_metadata={
                "agent_name": posthook_defn.name,
                "sandbox_id": sandbox_id,
            },
            work_result=work_result,
        )

    def factory(team_run):
        return Executor(
            team_run=team_run,
            runner=runner,
            build_query_context=build_query_ctx,
            build_posthook_context=build_posthook_ctx,
            agent_lookup=get_definition,
        )

    return factory


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
) -> tuple[TeamRunStatus, int]:
    """Run the builtin planner/developer/validator team against the sandbox.

    Returns ``(TeamRunStatus, work_items_executed)``. Does not raise on
    team failure — the caller grades the result via the sweevo test
    command.
    """
    from config.model_config import get_active_model_kwargs
    from server.app_factory import (
        build_session_config,
        ensure_runtime_stores_ready,
        session_store,
    )

    try:
        _register_team_builtins()
    except Exception:
        logger.debug("team builtins already registered", exc_info=True)

    session_config = build_session_config()
    session_config.cwd = repo_dir
    ensure_runtime_stores_ready()
    try:
        session_store.upsert(
            session_id=session_config.session_id,
            cwd=repo_dir,
            model=str(get_active_model_kwargs().get("model") or ""),
            message_count=0,
        )
    except Exception:
        logger.debug("Failed to ensure sweevo team session row", exc_info=True)
    root_prompt = _build_root_prompt(instance, repo_dir)

    tr = TeamRun(
        session_id=getattr(session_config, "session_id", "sweevo"),
        user_request=root_prompt,
        budgets=_UNLIMITED_BUDGETS,
        sandbox_id=sandbox_id,
        repo_root=repo_dir,
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
        executor_factory=_make_executor_factory(session_config, sandbox_id, printer),
        num_executors=num_executors,
        root_kind=WorkItemKind.EXPANDABLE,
    )

    status = await tr.wait()
    work_items = len(tr.dispatcher.graph)
    logger.info(
        "sweevo team run %s finished: status=%s work_items=%d",
        tr.id,
        getattr(status, "value", status),
        work_items,
    )
    return status, work_items
