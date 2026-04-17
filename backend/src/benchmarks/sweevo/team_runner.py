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

import json
import logging
from pathlib import Path
from typing import Any

from agents.registry import get_definition
from config.paths import get_project_config_dir
from message.event_printer import MultiAgentEventPrinter
from code_intelligence.routing.service import get_code_intelligence
from team.builtins import (
    DEVELOPER,
    SCOUT,
    TEAM_PLANNER,
    TEAM_REPLANNER,
    VALIDATOR,
    register_all as _register_team_builtins,
)
from team.models import BudgetConfig, TeamDefinition
from team.persistence.store import TeamDefinitionStore
from team.persistence.events import make_checkpoint_repo_state
from team.persistence.run_store import build_default_store
from team.runtime.context_builder import TeamAgentContext
from team.runtime.executor import Executor
from team.runtime.runner import AgentRunState, TeamAgentRunner
from team.runtime.team_run import TeamRun
from team.runtime.telemetry import (
    BenchmarkTelemetry,
    append_event,
    checkpoint_records_from_store as _checkpoint_records_from_store,
    checkpoint_repo_patch_from_store as _checkpoint_repo_patch_from_store,
    default_team_metrics,
    emit_dispatcher_dag as _emit_dispatcher_dag,
    emit_planning_budget_banner as _emit_team_runtime_banner,
    finalize_team_run,
    make_external_hook_emitter as _make_external_hook_emitter,
    tool_names_from_messages as _tool_names_from_messages,
)

from benchmarks.sweevo.dataset import summarize_sweevo_instance
from benchmarks.sweevo.models import SWEEvoInstance, _REPO_DIR
from benchmarks.sweevo.sandbox import (
    apply_sweevo_repo_patch,
    capture_sweevo_repo_patch,
    ensure_sweevo_test_patch,
    setup_sweevo_sandbox,
)

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
    path = _PROJECT_ROOT / ".ephemeralos" / "prompt-reports" / f"team-run-{team_run_id}-messages.jsonl"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.touch(exist_ok=True)
    return path


def _load_or_create_team_definition(session_factory: object) -> TeamDefinition:
    """Load the sweevo team definition from the DB, seeding from the file
    registry on first run. Raises if neither source provides the definition."""
    from team.registry import get_team_definition

    store = TeamDefinitionStore()
    store.initialize(session_factory)  # type: ignore[arg-type]
    file_defn = get_team_definition(_SWEEVO_TEAM_NAME)
    if file_defn is not None:
        return store.seed_builtin(file_defn)  # dual-write, idempotent
    existing = store.get_by_name(_SWEEVO_TEAM_NAME)
    if existing is not None:
        return existing
    raise RuntimeError(
        f"Team definition {_SWEEVO_TEAM_NAME!r} not found — "
        "ensure backend/config/teams/sweevo_benchmark.md exists "
        "or seed the database via the CRUD API."
    )


def _benchmark_team_run_dir() -> Path:
    """Return the benchmark-owned TeamRun event log directory."""
    return get_project_config_dir(_PROJECT_ROOT) / "team-runs"


def _build_benchmark_event_store() -> Any:
    """Project-local TeamRun event log used for benchmark observability."""
    return build_default_store(base_dir=_benchmark_team_run_dir())


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
    """Minimal instance-specific prompt — agent skills/system prompts (from DB)
    carry the detailed workflow policy."""
    return (
        f"You are leading a coding team on a SWE-EVO benchmark instance.\n"
        f"Repository: {instance.repo}\n"
        f"Working directory inside the sandbox: {repo_dir}\n"
        f"Base commit (already checked out): {instance.base_commit}\n\n"
        f"## Objective\n"
        f"Make the grading command pass by fixing the repository so the fail-to-pass "
        f"tests turn green without regressing the pass-to-pass coverage.\n\n"
        f"## Fail-To-Pass Targets\n{json.dumps(instance.fail_to_pass, indent=2)}\n\n"
        f"## Pass-To-Pass count: {len(instance.pass_to_pass)}\n\n"
        f"## Grading command\n```\n{instance.test_cmds}\n```\n\n"
        f"Stay inside {repo_dir}."
    )


def _enforce_validation_evidence(state: AgentRunState) -> None:
    """BenchmarkTelemetry success hook — validator must run daytona_codeact."""
    if state.defn.name != VALIDATOR:
        return
    if "daytona_codeact" in _tool_names_from_messages(list(state.agent.display_messages)):
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
    *,
    repo_dir: str = _REPO_DIR,
):
    """Wire :class:`TeamAgentRunner` with :class:`BenchmarkTelemetry` hooks and
    append a sweevo-specific durable repo checkpoint after each run."""
    telemetry = BenchmarkTelemetry(
        printer=printer,
        team_metrics=team_metrics,
        session_config=session_config,
        banner_agent=TEAM_PLANNER,
        success_hook=_enforce_validation_evidence,
    )
    core_runner = TeamAgentRunner(
        session_config=session_config,
        sandbox_id=sandbox_id,
        agent_overrides=agent_overrides,
        on_spawned=telemetry.on_spawned,
        on_event=telemetry.on_event,
        on_complete=telemetry.on_complete,
        on_checkpoint_event=telemetry.on_checkpoint_event,
    )

    async def _run(defn, ctx: TeamAgentContext):
        result = await core_runner(defn, ctx)
        result["checkpoint_id"] = await _capture_post_run_repo_checkpoint(
            agent_name=result["agent"],
            ctx=ctx,
            tracker_run_id=result.get("agent_run_id"),
            sandbox_id=sandbox_id,
            repo_dir=repo_dir,
            printer=printer,
            team_metrics=team_metrics,
        )
        return result

    return _run


async def _capture_post_run_repo_checkpoint(
    *,
    agent_name: str,
    ctx: TeamAgentContext,
    tracker_run_id: str | None,
    sandbox_id: str,
    repo_dir: str,
    printer: MultiAgentEventPrinter | None,
    team_metrics: dict[str, Any] | None,
) -> str | None:
    """Capture a sweevo repo patch after planner/developer/validator runs so
    resume_sweevo_team can rehydrate the working tree."""
    if agent_name not in {TEAM_PLANNER, "developer", "validator"}:
        return None
    try:
        from team.runtime.registry import get as get_team_run

        team_run_id = ctx.tool_metadata.get("team_run_id")
        team_run = get_team_run(team_run_id) if team_run_id else None
        if team_run is None:
            return None
        work_item_id = ctx.tool_metadata.get("work_item_id")
        label = f"{agent_name}:{work_item_id or tracker_run_id or 'run'}"
        checkpoint_id = await team_run.checkpoint(label=label)
        replans = int(getattr(team_run.budget_state, "replans_used", 0) or 0)
        try:
            repo_patch = await capture_sweevo_repo_patch(
                team_run.sandbox_id or sandbox_id, repo_dir=repo_dir,
            )
            team_run.event_store.append(make_checkpoint_repo_state(
                team_run.id, checkpoint_id=checkpoint_id, repo_patch=repo_patch,
            ))
        except Exception:
            logger.debug("Failed repo patch for checkpoint %s", checkpoint_id, exc_info=True)
        record = {
            "id": checkpoint_id, "label": label, "parent_run": team_run.id,
            "replans_used": replans,
        }
        if team_metrics is not None:
            team_metrics.setdefault("checkpoint_ids", []).append(checkpoint_id)
            team_metrics.setdefault("checkpoints", []).append(record)
        append_event(team_metrics, {
            "event": "checkpoint", "team_run_id": team_run.id,
            "checkpoint_id": checkpoint_id, "agent": agent_name,
            "work_item_id": work_item_id, "agent_run_id": tracker_run_id,
            **record,
        })
        if printer is not None:
            printer.raw_line(
                agent_name,
                f"[checkpoint] id={checkpoint_id} label={label} "
                f"parent_run={team_run.id} replans={replans}",
            )
        return checkpoint_id
    except Exception:
        logger.debug("Failed to checkpoint after %s", agent_name, exc_info=True)
        return None



def _make_context_builders(
    sandbox_id: str,
    repo_dir: str = _REPO_DIR,
):
    """Wrap the default :func:`team.runtime.context_builder.build_query_context`
    with benchmark coordination flags and a code-intelligence warm-up for the
    SWE-EVO sandbox.

    Agent role, terminal tools, and user prompt templates are supplied by the
    default builder; the sweevo team definition loaded from the DB carries
    everything else.
    """
    from team.runtime.context_builder import build_query_context as _default_ctx

    async def build_query_ctx(defn, team_run, wi):
        ctx = await _default_ctx(defn, team_run, wi)
        effective_sandbox = team_run.sandbox_id or sandbox_id
        ctx.tool_metadata.update({
            "sandbox_id": effective_sandbox,
            "repo_root": repo_dir,
            "exec_cwd": repo_dir,
            "ci_workspace_root": repo_dir,
            "team_mode_enabled": True,
            "require_declared_shell_outputs": True,
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
    """Attach sweevo skills and runtime limits to each builtin agent."""
    exec_limits = _derive_execution_runtime_limits(instance)
    # (agent_name, extra_skills, limits, include_toolkits)
    spec: list[tuple[str, tuple[str, ...], dict[str, int], bool]] = [
        (TEAM_PLANNER,     ("sweevo-project-context",),                       _derive_planner_runtime_limits(instance), True),
        (DEVELOPER,        ("sweevo-project-context",),                       exec_limits, False),
        (SCOUT,            ("sweevo-project-context",),                       exec_limits, False),
        (VALIDATOR,        ("sweevo-project-context", "verification-replan"), exec_limits, False),
        (TEAM_REPLANNER,   ("sweevo-project-context",),                       exec_limits, False),
    ]
    overrides: dict[str, dict[str, Any]] = {}
    for name, extra_skills, limits, include_toolkits in spec:
        defn = get_definition(name)
        if defn is None:
            continue
        merged_skills = list(defn.skills)
        for s in extra_skills:
            if s and s not in merged_skills:
                merged_skills.append(s)
        entry: dict[str, Any] = {"skills": merged_skills, **limits}
        if include_toolkits:
            entry["toolkits"] = list(defn.toolkits or [])
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


_finalize_team_result = finalize_team_run  # re-exported for callers / tests


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

    Does not raise on team failure — the caller grades the result via the
    sweevo test command.
    """
    _ensure_team_builtins()
    session_config, session_factory = _prepare_benchmark_session(repo_dir=repo_dir)
    event_store = _build_benchmark_event_store()
    team_def = _load_or_create_team_definition(session_factory)
    root_prompt = _build_root_prompt(instance, repo_dir)
    budgets = _derive_sweevo_budgets(instance)
    team_metrics = _build_team_metrics()
    team_metrics["structured_log_path"] = structured_log_path
    _emit_team_runtime_banner(printer, budgets=budgets)

    tr = TeamRun(
        session_id=getattr(session_config, "session_id", "sweevo"),
        user_request=root_prompt, budgets=budgets,
        sandbox_id=sandbox_id, repo_root=repo_dir, event_store=event_store,
    )
    prompt_messages_path = _prompt_report_messages_path(tr.id)
    tr.coordination_metadata = {
        "team_mode_enabled": True,
        "require_declared_shell_outputs": True,
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
        printer.raw_line("team", f"[prompt_report] messages={prompt_messages_path}")
    append_event(team_metrics, {
        "event": "team_start", "team_run_id": tr.id,
        "session_id": getattr(session_config, "session_id", "sweevo"),
        "sandbox_id": sandbox_id, "instance_id": instance.instance_id,
        "repo": instance.repo, "repo_dir": repo_dir,
        "prompt_report_messages_path": str(prompt_messages_path),
        "budgets": {
            "max_tasks": budgets.max_tasks,
            "max_depth": budgets.max_depth,
            "max_plan_size": budgets.max_plan_size,
        },
    })

    await tr.start_with_team_definition(
        team_def,
        payload={
            "objective": "Produce the initial root plan for this SWE-EVO benchmark instance.",
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
        checkpoint_records=_checkpoint_records_from_store(event_store, tr.id),
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

    ensure_runtime_stores_ready()
    event_store = _build_benchmark_event_store()
    initial_records = _checkpoint_records_from_store(event_store, team_run_id)
    available_ids = [r["id"] for r in initial_records]
    resume_id = checkpoint_id or (available_ids[-1] if use_latest_checkpoint and available_ids else None)
    tr = (
        TeamRun.resume_from(event_store, team_run_id, checkpoint_id=resume_id)
        if resume_id else TeamRun.resume_from(event_store, team_run_id)
    )
    if not tr.sandbox_id:
        raise ValueError(
            f"team run {team_run_id!r} cannot be resumed: missing sandbox_id in persisted header"
        )
    if resume_id:
        await setup_sweevo_sandbox(instance, tr.sandbox_id, repo_dir)
        await ensure_sweevo_test_patch(instance, tr.sandbox_id, repo_dir)
        repo_patch = _checkpoint_repo_patch_from_store(event_store, team_run_id, resume_id)
        if repo_patch:
            await apply_sweevo_repo_patch(tr.sandbox_id, repo_patch, repo_dir)
        if printer is not None:
            patch_info = (
                f"repo_patch_bytes={len(repo_patch.encode('utf-8'))}"
                if repo_patch else "repo_patch=<missing>"
            )
            printer.raw_line(
                "team",
                f"[resume_restore] checkpoint={resume_id} {patch_info} benchmark_patch=reapplied",
            )

    session_config, _ = _prepare_benchmark_session(repo_dir=repo_dir, session_id=tr.session_id or None)
    budgets = tr.budgets
    team_metrics = _build_team_metrics()
    team_metrics["structured_log_path"] = structured_log_path
    _emit_team_runtime_banner(printer, budgets=budgets)
    prompt_messages_path = _prompt_report_messages_path(tr.id)
    tr.coordination_metadata = {
        **getattr(tr, "coordination_metadata", {}),
        "prompt_report_messages_path": str(prompt_messages_path),
    }

    replans = int(getattr(getattr(tr, "budget_state", None), "replans_used", 0) or 0)
    resume_tag = resume_id or "<latest-state>"
    if printer is not None:
        label = next(
            (str(r.get("label") or "") for r in initial_records if r.get("id") == resume_id), "",
        )
        printer.raw_line(
            "team",
            f"[resume] team_run_id={team_run_id} sandbox_id={tr.sandbox_id} "
            f"durable_checkpoints={len(available_ids)} checkpoint={resume_tag} "
            f"resumed_from={team_run_id} resumed_from_checkpoint={resume_tag} "
            f"replans={replans}{f' label={label}' if label else ''}",
        )
        printer.raw_line(
            "team",
            f"[run_ids] team_run_id={tr.id} "
            f"session_id={getattr(session_config, 'session_id', 'sweevo')} "
            f"sandbox_id={tr.sandbox_id}",
        )
        printer.raw_line("team", f"[prompt_report] messages={prompt_messages_path}")
    append_event(team_metrics, {
        "event": "resume", "team_run_id": team_run_id,
        "sandbox_id": tr.sandbox_id, "instance_id": instance.instance_id,
        "checkpoint_id": resume_id,
        "durable_checkpoint_count": len(available_ids),
        "replans_used": replans,
        "prompt_report_messages_path": str(prompt_messages_path),
    })

    await tr.resume(
        executor_factory=_make_executor_factory(
            session_config, tr.sandbox_id, printer, repo_dir=repo_dir,
            team_metrics=team_metrics, agent_overrides=_build_agent_overrides(instance),
        ),
        num_executors=num_executors,
        resumed_from=team_run_id,
        resumed_from_checkpoint=resume_id,
    )
    await tr.wait()
    return _finalize_team_result(
        tr=tr, session_config=session_config, team_metrics=team_metrics,
        budgets=budgets, printer=printer,
        checkpoint_records=_checkpoint_records_from_store(event_store, tr.id),
        resumed_from=team_run_id, resumed_from_checkpoint=resume_id,
    )
