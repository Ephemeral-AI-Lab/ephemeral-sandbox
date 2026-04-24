"""Wire a real team run over a provisioned SWE-EVO sandbox.

Drives :class:`team.runtime.team_run.TeamRun` with the builtin
``root_planner`` / ``team_planner`` / ``developer`` / ``validator`` agents from
``team.definitions``. Each Task's agent is spawned through
:func:`engine.runtime.agent.spawn_agent` so it runs with its full
production tool surface (``sandbox_operations``, ``code_intelligence``,
``context``, skills) against the Daytona sandbox that was already
prepared by :func:`benchmarks.sweevo.sandbox.create_sweevo_test_sandbox`.
"""

from __future__ import annotations

import json
import logging
from pathlib import Path
from typing import Any

from agents.registry import get_definition
from config.paths import get_project_config_dir
from message.event_printer import MultiAgentEventPrinter
from code_intelligence.routing.service import get_code_intelligence
from team.definitions import (
    DEVELOPER,
    ROOT_PLANNER,
    SCOUT,
    TEAM_PLANNER,
    TEAM_REPLANNER,
    VALIDATOR,
    register_all as _register_team_builtins,
)
from team.core.models import BudgetConfig, TeamDefinition
from team.persistence.run_store import TeamRunStore
from team.runtime.executor import Executor
from team.runtime.runner import AgentRunState, TeamAgentRunner
from team.runtime.team_run import TeamRun
from benchmarks.sweevo.telemetry import (
    BenchmarkTelemetry,
    append_event,
    default_team_metrics,
    emit_dispatcher_dag as _emit_dispatcher_dag,
    emit_planning_budget_banner as _emit_team_runtime_banner,
    finalize_team_run,
    make_external_hook_emitter as _make_external_hook_emitter,
    tool_names_from_messages as _tool_names_from_messages,
)

from benchmarks.sweevo.dataset import summarize_sweevo_instance
from benchmarks.sweevo.models import SWEEvoInstance, _REPO_DIR

logger = logging.getLogger(__name__)


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


def _prompt_report_messages_path(team_run_id: str) -> Path:
    path = _benchmark_team_run_dir() / team_run_id / f"{team_run_id}.messages.jsonl"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.touch(exist_ok=True)
    return path


def _agent_run_log_dir(team_run_id: str) -> Path:
    path = _benchmark_team_run_dir() / team_run_id / "agent-runs"
    path.mkdir(parents=True, exist_ok=True)
    return path


def _load_or_create_team_definition(
    session_factory: object,
    *,
    team_name: str = _SWEEVO_TEAM_NAME,
) -> TeamDefinition:
    """Load the sweevo team definition from checked-in config."""
    del session_factory
    from team.definitions import get_team_definition

    file_defn = get_team_definition(team_name)
    if file_defn is not None:
        return file_defn
    raise RuntimeError(
        f"Team definition {team_name!r} not found — "
        "ensure backend/config/teams/sweevo_benchmark.md exists."
    )


def _benchmark_team_run_dir() -> Path:
    """Return the benchmark-owned TeamRun event log directory."""
    return get_project_config_dir(_PROJECT_ROOT) / "team-runs"


def _build_benchmark_event_store() -> Any:
    """Project-local TeamRun event log used for benchmark observability."""
    return TeamRunStore(_benchmark_team_run_dir())


# ---------------------------------------------------------------------------
# Prompt construction
# ---------------------------------------------------------------------------


def _derive_sweevo_budgets(instance: SWEEvoInstance) -> BudgetConfig:
    """Return size-aware team budgets for SWE-EVO instead of disabling them."""
    summary = summarize_sweevo_instance(instance)
    size = str(summary.get("size") or "medium")
    f2p_targets = max(1, len(instance.fail_to_pass))

    base = {
        "small":  {"max_depth": 6, "max_plan_size": 8,  "max_tasks": 24},
        "medium": {"max_depth": 6, "max_plan_size": 12, "max_tasks": 40},
        "large":  {"max_depth": 6, "max_plan_size": 16, "max_tasks": 64},
    }.get(size, {"max_depth": 6, "max_plan_size": 12, "max_tasks": 40})
    max_depth = int(base["max_depth"])

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
    """Minimal instance-specific prompt — agent skills/system prompts (from DB)
    carry the detailed workflow policy."""
    return (
        f"You are leading a coding team on a SWE-EVO benchmark instance.\n"
        f"Repository: {instance.repo}\n"
        f"Working directory inside the sandbox: {repo_dir}\n"
        f"Base commit (already checked out): {instance.base_commit}\n\n"
        f"Test command: {instance.test_cmds}\n"
        f"Pass-To-Pass count: {len(instance.pass_to_pass)}\n\n"
        f"## Objective\n"
        f"Make the grading command pass by fixing the repository so the fail-to-pass "
        f"tests turn green without regressing the pass-to-pass coverage.\n\n"
        f"## Fail-To-Pass Targets\n{json.dumps(instance.fail_to_pass, indent=2)}\n\n"
        f"Stay inside {repo_dir}."
    )


def _enforce_validation_evidence(state: AgentRunState) -> None:
    """BenchmarkTelemetry success hook — validator must run daytona_shell."""
    if state.defn.name != VALIDATOR:
        return
    if "daytona_shell" in _tool_names_from_messages(list(state.agent.display_messages)):
        return
    raise RuntimeError(
        "validator_missing_tool_evidence: validator must execute at least one "
        "daytona_shell verification command before returning a verdict"
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
    *,
    repo_dir: str = _REPO_DIR,
):
    """Wire :class:`TeamAgentRunner` with :class:`BenchmarkTelemetry` hooks."""
    telemetry = BenchmarkTelemetry(
        printer=printer,
        team_metrics=team_metrics,
        session_config=session_config,
        banner_agent=ROOT_PLANNER,
        success_hook=_enforce_validation_evidence,
    )
    return TeamAgentRunner(
        session_config=session_config,
        sandbox_id=sandbox_id,
        agent_overrides=agent_overrides,
        on_spawned=telemetry.on_spawned,
        on_event=telemetry.on_event,
        on_complete=telemetry.on_complete,
    )


def _make_context_builders(
    sandbox_id: str,
    repo_dir: str = _REPO_DIR,
):
    """Wrap the default :func:`team.runtime.agent_context.build_query_context`
    with benchmark coordination flags and a code-intelligence warm-up for the
    SWE-EVO sandbox.

    Agent role, terminal tools, and user prompt templates are supplied by the
    default builder; the sweevo team definition loaded from the DB carries
    everything else.
    """
    from team.runtime.agent_context import build_query_context as _default_ctx

    async def build_query_ctx(defn, team_run, wi):
        ctx = await _default_ctx(defn, team_run, wi)
        effective_sandbox = team_run.sandbox_id or sandbox_id
        ctx.tool_metadata.update({
            "sandbox_id": effective_sandbox,
            "repo_root": repo_dir,
            "exec_cwd": repo_dir,
            "ci_workspace_root": repo_dir,
            "verification_surface_write_enforcement": "warn",
        })
        try:
            get_code_intelligence(sandbox_id=effective_sandbox, workspace_root=repo_dir)
        except Exception:
            pass
        return ctx

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
        def after_dispatch(wi, update):
            if update.plan is None or wi.agent_name not in {ROOT_PLANNER, TEAM_PLANNER}:
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
    """Attach SWE-EVO runtime limits to each builtin agent."""
    exec_limits = _derive_execution_runtime_limits(instance)
    planner_limits = _derive_planner_runtime_limits(instance)
    # (agent_name, limits, include_tools)
    spec: list[tuple[str, dict[str, int], bool]] = [
        (ROOT_PLANNER, planner_limits, True),
        (TEAM_PLANNER, planner_limits, True),
        (DEVELOPER, exec_limits, False),
        (SCOUT, exec_limits, False),
        (VALIDATOR, exec_limits, False),
        (TEAM_REPLANNER, exec_limits, False),
    ]
    overrides: dict[str, dict[str, Any]] = {}
    for name, limits, include_tools in spec:
        defn = get_definition(name)
        if defn is None:
            continue
        entry: dict[str, Any] = {"skills": list(defn.skills), **limits}
        if include_tools:
            entry["tools"] = list(defn.tools or [])
        overrides[name] = entry
    return overrides


_build_team_metrics = default_team_metrics  # kept for test monkeypatches


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
    # ``SessionConfig.cwd`` is a host-local EphemeralOS workspace path used for
    # prompt-side project metadata, skill discovery, and local config files.
    # The SWE-EVO checkout lives inside the remote Daytona sandbox at
    # ``repo_dir`` and is routed to tools via task metadata instead.
    session_config.cwd = str(_PROJECT_ROOT)
    if session_id:
        session_config.session_id = session_id
    session_factory = ensure_runtime_stores_ready()
    try:
        session_store.upsert(
            session_id=session_config.session_id,
            cwd=str(_PROJECT_ROOT),
            model=str(get_active_model_kwargs().get("model") or ""),
            message_count=0,
        )
    except Exception:
        logger.debug("Failed to ensure sweevo team session row", exc_info=True)
    return session_config, session_factory


_finalize_team_result = finalize_team_run  # re-exported for callers / tests


# ---------------------------------------------------------------------------
# Public entry point
# ---------------------------------------------------------------------------


async def run_sweevo_team(
    instance: SWEEvoInstance,
    sandbox_id: str,
    *,
    team_name: str = _SWEEVO_TEAM_NAME,
    team_run_id: str | None = None,
    repo_dir: str = _REPO_DIR,
    printer: MultiAgentEventPrinter | None = None,
    num_executors: int = _DEFAULT_NUM_EXECUTORS,
    structured_log_path: str | None = None,
) -> dict[str, Any]:
    """Run the builtin planner/developer/validator team against the sandbox.

    Does not raise on team failure — the caller grades the result via the
    sweevo test command.
    """
    _ensure_team_builtins()
    session_config, session_factory = _prepare_benchmark_session(repo_dir=repo_dir)
    event_store = _build_benchmark_event_store()
    team_def = _load_or_create_team_definition(session_factory, team_name=team_name)
    root_prompt = _build_root_prompt(instance, repo_dir)
    budgets = _derive_sweevo_budgets(instance)
    team_metrics = _build_team_metrics()
    team_metrics["structured_log_path"] = structured_log_path
    team_metrics["team_name"] = team_def.name
    _emit_team_runtime_banner(printer, budgets=budgets)

    tr = TeamRun(
        team_run_id=team_run_id,
        session_id=getattr(session_config, "session_id", "sweevo"),
        user_request=root_prompt, budgets=budgets,
        sandbox_id=sandbox_id, repo_root=repo_dir, event_store=event_store,
    )
    prompt_messages_path = _prompt_report_messages_path(tr.id)
    agent_run_log_dir = _agent_run_log_dir(tr.id)
    team_metrics["agent_run_log_dir"] = str(agent_run_log_dir)
    tr.coordination_metadata = {
        "verification_surface_write_enforcement": "warn",
        "prompt_report_messages_path": str(prompt_messages_path),
        "external_hook_emitter": _make_external_hook_emitter(
            printer=printer, team_metrics=team_metrics,
        ),
    }
    if printer is not None:
        printer.raw_line(
            "team",
            f"[run_ids] team_run_id={tr.id} "
            f"session_id={getattr(session_config, 'session_id', 'sweevo')} "
            f"sandbox_id={sandbox_id}",
        )
        printer.raw_line("team", f"[team_definition] name={team_def.name}")
        printer.raw_line("team", f"[prompt_report] messages={prompt_messages_path}")
        printer.raw_line("team", f"[agent_run_logs] dir={agent_run_log_dir}")
    append_event(team_metrics, {
        "event": "team_start", "team_run_id": tr.id,
        "team_name": team_def.name,
        "session_id": getattr(session_config, "session_id", "sweevo"),
        "sandbox_id": sandbox_id, "instance_id": instance.instance_id,
        "repo": instance.repo, "repo_dir": repo_dir,
        "prompt_report_messages_path": str(prompt_messages_path),
        "agent_run_log_dir": str(agent_run_log_dir),
        "budgets": {
            "max_tasks": budgets.max_tasks,
            "max_depth": budgets.max_depth,
            "max_plan_size": budgets.max_plan_size,
        },
    })

    await tr.start_with_team_definition(
        team_def,
        payload={
            "spec": {
                "goal": "Produce the initial root plan for this SWE-EVO benchmark instance.",
                "detail": root_prompt,
                "acceptance_criteria": (
                    "Submit a valid child plan covering the benchmark instance "
                    "repair and verification work."
                ),
            },
            "prompt": root_prompt,
            "instance_id": instance.instance_id, "repo": instance.repo,
            "repo_dir": repo_dir, "test_cmds": instance.test_cmds,
            "fail_to_pass": instance.fail_to_pass,
            "pass_to_pass": instance.pass_to_pass,
        },
        executor_factory=_make_executor_factory(
            session_config, sandbox_id, printer, repo_dir=repo_dir,
            team_metrics=team_metrics, agent_overrides=_build_agent_overrides(instance),
        ),
        num_executors=num_executors,
    )
    await tr.wait()
    return _finalize_team_result(
        tr=tr, session_config=session_config, team_metrics=team_metrics,
        budgets=budgets, printer=printer,
    )
