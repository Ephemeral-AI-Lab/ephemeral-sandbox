"""Helpers for assembling agent and team prompt reports."""

from __future__ import annotations

import asyncio
import os
from pathlib import Path
from types import SimpleNamespace

from agents import get_definition
from agents.types import AgentDefinition
from config.settings import load_settings
from engine.runtime.agent import (
    _build_agent_system_prompt,
    _build_agent_tool_registry,
    finalize_tool_registry_and_prompt,
)
from external_trigger.tc_note import (
    TC_NOTE_EDIT_PROMPT,
    TC_NOTE_TURN_PROMPT,
)
from skills.core.loader import load_skill_registry
from team.models import (
    BudgetConfig,
    BudgetState,
    Note,
    Task,
    TaskStatus,
)
from team.note_manager import NoteManager
from team.persistence.events import TeamRunEvent
from team.persistence.run_store import JsonlTeamRunStore
from team.runtime.rehydration import budget_config_from_event, task_from_dict
from team.task_context_builder import TaskContextBuilder
from team.builtins import register_all as register_team_builtins
from team.models import TeamDefinition
from team.registry import get_team_definition, list_team_definitions
from team.runtime.context_builder import build_query_context
from team.runtime.context_builder import DEFAULT_TERMINAL_TOOLS


def register_builtins() -> None:
    """Register builtin agents and teams into the in-memory registries."""
    register_team_builtins()


def current_settings():
    """Load current runtime settings."""
    return load_settings()


def load_agent_definition(name: str, settings) -> AgentDefinition | None:
    """Load an agent definition by name from memory first, then DB."""
    agent_def = get_definition(name)
    if agent_def is not None:
        return agent_def

    try:
        from db.engine import initialize_db  # type: ignore[attr-defined]
        from agents.db.store import AgentDefinitionStore  # type: ignore[attr-defined]

        sf = initialize_db(settings.database)
        if sf is None:
            return None

        store = AgentDefinitionStore()
        store.initialize(sf)
        record = store.get_by_name(name)
        if record is None:
            return None

        return AgentDefinition(
            name=record.name,
            description=record.description,
            system_prompt=record.system_prompt,
            model=record.model,
            effort=record.effort,
            tool_call_limit=record.tool_call_limit,
            toolkits=record.toolkits or [],
            skills=record.skills or [],
            blocked_tools=record.blocked_tools or [],
            allowed_triggers=record.allowed_triggers or [],
            hooks=record.hooks,
            background=record.background,
            initial_prompt=record.initial_prompt,
            role=record.role,
            agent_type=record.agent_type or "agent",
            supported_kinds=record.supported_kinds or ["atomic", "expandable"],
            source=record.source or "user",
            can_spawn_subagents=record.can_spawn_subagents,
            require_fresh_client=record.require_fresh_client,
            include_skills=record.include_skills,
            dispatchable_via_run_subagent=record.dispatchable_via_run_subagent,
        )
    except Exception:
        return None


def build_agent_system_prompt_text(
    agent_def: AgentDefinition,
    *,
    cwd: str,
    settings,
    sandbox_id: str = "",
    include_runtime_sections: bool = True,
    terminal_tools: set[str] | list[str] | None = None,
) -> str:
    """Build the assembled system prompt exactly as spawn_agent would."""
    config = SimpleNamespace(cwd=cwd)
    system_prompt = _build_agent_system_prompt(
        config,
        agent_def,
        settings,
        latest_user_prompt=None,
    )

    if include_runtime_sections:
        tool_registry = _build_agent_tool_registry(
            config,
            agent_def,
            sandbox_id or None,
            agent_def.name,
        )
        system_prompt, _ = finalize_tool_registry_and_prompt(
            tool_registry,
            system_prompt,
            can_spawn_subagents=agent_def.can_spawn_subagents,
            role=agent_def.role,
            blocked_tools=agent_def.blocked_tools,
            terminal_tools=terminal_tools,
        )

    return system_prompt


def resolve_terminal_tools_for_role(team_def: TeamDefinition | None, role: str | None) -> set[str]:
    """Resolve terminal tools for a team role using team overrides or defaults."""
    role_name = str(role or "").strip()
    if not role_name:
        return set()
    td_map = getattr(team_def, "terminal_tools", None) or {}
    terminal_set = td_map.get(role_name) if td_map else None
    if not terminal_set:
        terminal_set = DEFAULT_TERMINAL_TOOLS.get(role_name, set())
    return set(terminal_set)


def _member_roles(roster: dict[str, list[str]], entry_planner: str) -> dict[str, list[str]]:
    """Return unique team members mapped to their roster roles in stable order."""
    members: dict[str, list[str]] = {}
    for role, agent_names in roster.items():
        for agent_name in agent_names:
            roles = members.setdefault(agent_name, [])
            if role not in roles:
                roles.append(role)
    if entry_planner and entry_planner not in members:
        members[entry_planner] = ["planner"]
    return members


def _append_text_block(lines: list[str], text: str) -> None:
    """Append a markdown text block that can contain nested triple fences."""
    lines.extend(["", "````text", text, "````"])


def load_team_definition(identifier: str, settings) -> TeamDefinition | None:
    """Resolve a team by DB id first, then by name from DB or builtin registry."""
    try:
        from db.engine import initialize_db  # type: ignore[attr-defined]
        from team.persistence.store import TeamDefinitionStore  # type: ignore[attr-defined]

        sf = initialize_db(settings.database)
        if sf is not None:
            store = TeamDefinitionStore()
            store.initialize(sf)
            team_def = store.get_by_id(identifier)
            if team_def is not None:
                return team_def
            team_def = store.get_by_name(identifier)
            if team_def is not None:
                return team_def
    except Exception:
        pass

    team_def = get_team_definition(identifier)
    if team_def is not None:
        return team_def

    for candidate in list_team_definitions():
        if candidate.id == identifier:
            return candidate
    return None


def default_team_prompt_report_path(team_def: TeamDefinition, output_dir: str | None = None) -> Path:
    """Return a stable default output path for a team prompt report."""
    safe_name = "".join(ch if ch.isalnum() or ch in {"-", "_"} else "-" for ch in team_def.name).strip("-")
    stem = f"team-system-prompts-{safe_name or 'team'}-{team_def.id[:8]}"
    base_dir = Path(output_dir) if output_dir else Path(os.getcwd())
    return base_dir / f"{stem}.md"


def default_team_user_prompt_report_path(
    team_def: TeamDefinition, output_dir: str | None = None,
) -> Path:
    """Return a stable default output path for a team user-prompt report."""
    safe_name = "".join(
        ch if ch.isalnum() or ch in {"-", "_"} else "-"
        for ch in team_def.name
    ).strip("-")
    stem = f"team-user-prompts-{safe_name or 'team'}-{team_def.id[:8]}"
    base_dir = Path(output_dir) if output_dir else Path(os.getcwd())
    return base_dir / f"{stem}.md"


def default_team_role_prompt_report_path(
    team_def: TeamDefinition,
    roles: list[str],
    output_dir: str | None = None,
) -> Path:
    """Return a stable default output path for a role-scoped prompt report."""
    safe_name = "".join(
        ch if ch.isalnum() or ch in {"-", "_"} else "-"
        for ch in team_def.name
    ).strip("-")
    role_part = "-".join(
        "".join(ch if ch.isalnum() or ch in {"-", "_"} else "-" for ch in role).strip("-")
        for role in roles
        if role.strip()
    )
    stem = f"team-role-prompts-{safe_name or 'team'}-{role_part or 'all'}-{team_def.id[:8]}"
    base_dir = Path(output_dir) if output_dir else Path(os.getcwd())
    return base_dir / f"{stem}.md"


def default_team_run_prompt_report_path(
    team_run_id: str, output_dir: str | None = None,
) -> Path:
    """Return a stable default output path for a team-run user-prompt report."""
    safe_run_id = "".join(
        ch if ch.isalnum() or ch in {"-", "_"} else "-"
        for ch in team_run_id
    ).strip("-")
    base_dir = Path(output_dir) if output_dir else Path(os.getcwd())
    return base_dir / f"team-run-user-prompts-{safe_run_id or 'run'}.md"


def default_team_run_dir(cwd: str) -> Path:
    """Return the default project-local TeamRun event log directory."""
    return Path(cwd).resolve() / ".ephemeralos" / "team-runs"


def _example_task_for_agent(
    *,
    team_run_id: str,
    entry_planner: str,
    agent_name: str,
    roles: list[str],
    user_request: str,
) -> Task:
    task_id = "root" if agent_name == entry_planner else f"sample-{agent_name}"
    if agent_name == entry_planner:
        return Task(
            id="root",
            team_run_id=team_run_id,
            agent_name=agent_name,
            status=TaskStatus.PENDING,
            objective=user_request,
            root_id="root",
            depth=0,
        )
    objective = _structured_example_objective(agent_name=agent_name, roles=roles)
    return Task(
        id=task_id,
        team_run_id=team_run_id,
        agent_name=agent_name,
        status=TaskStatus.PENDING,
        objective=objective,
        description="Synthetic task used only for prompt inspection.",
        scope_paths=["backend/src"],
        parent_id="root",
        root_id="root",
        depth=1,
    )


def _structured_example_objective(*, agent_name: str, roles: list[str]) -> str:
    """Return a realistic structured task spec for synthetic prompt inspection."""
    role_hint = ", ".join(roles) if roles else "team member"
    if "reviewer" in roles:
        action = "Verify the implementation evidence and report pass or fail."
        acceptance = (
            "- Run the narrowest relevant verification command.\n"
            "- Report PASS only when the observed evidence supports it.\n"
            "- Include failing command output when validation fails."
        )
    elif "explorer" in roles:
        action = "Inspect the assigned paths and produce a compact evidence brief without editing files."
        acceptance = (
            "- Do not edit files.\n"
            "- Name the files and functions that matter.\n"
            "- Summarize risks and unknowns for the downstream owner."
        )
    elif "replanner" in roles:
        action = "Analyze the failed sibling task and draft the smallest corrective plan."
        acceptance = (
            "- Add corrective tasks only when the failure evidence requires them.\n"
            "- Cancel stale sibling work only when replacement is necessary.\n"
            "- Submit a valid `submit_replan(new_tasks=[...], cancel_ids=[...])` payload."
        )
    else:
        action = "Implement the bounded code change described by the parent plan."
        acceptance = (
            "- Keep edits inside the assigned scope.\n"
            "- Add or update focused tests when behavior changes.\n"
            "- Run the narrowest verification command and summarize the result."
        )

    return (
        "Goal\n"
        f"{action}\n\n"
        "Environment\n"
        "- Repository root: /workspace/project\n"
        "- Use the existing project tooling and prompt contracts.\n\n"
        "Scope\n"
        "- backend/src\n\n"
        "Context\n"
        f"- This is a synthetic prompt-inspection task for `{agent_name}` ({role_hint}).\n"
        "- In a live run, this section is written by the planner or replanner with task-specific evidence.\n\n"
        "Acceptance Criteria\n"
        f"{acceptance}"
    )


def _make_task_context(
    *,
    team_run_id: str,
    tasks: dict[str, Task],
    notes: list[Note] | None = None,
) -> SimpleNamespace:
    async def _get_task(task_id: str) -> Task | None:
        return tasks.get(task_id)

    note_manager = NoteManager(team_run_id=team_run_id)
    if notes:
        note_manager.restore(notes)
    task_store = SimpleNamespace(graph=tasks)
    task_context = TaskContextBuilder(
        team_run_id=team_run_id,
        notes=note_manager,
        get_task_fn=_get_task,
        task_store=task_store,
    )
    return SimpleNamespace(context=task_context, graph=tasks)


def _role_filter_matches(
    *,
    agent_name: str,
    roster_roles: list[str],
    agent_def: AgentDefinition | None,
    requested_roles: set[str],
) -> bool:
    if not requested_roles:
        return True
    if "all" in requested_roles:
        return True
    if "worker" in requested_roles or "workers" in requested_roles:
        if "planner" not in roster_roles and "replanner" not in roster_roles:
            return True
    values = {agent_name, *roster_roles}
    if agent_def is not None and agent_def.role:
        values.add(str(agent_def.role))
    return bool(values & requested_roles)


def _rendered_skill_content(skill: object) -> str:
    content = str(getattr(skill, "content", "") or "")
    references = getattr(skill, "references", {}) or {}
    if not references:
        return content
    ref_names = list(references.keys())
    footer = (
        "\n\n---\n"
        f"This skill has {len(ref_names)} reference document(s) available: "
        + ", ".join(f"`{name}`" for name in ref_names)
        + "\nUse `load_skill_reference` to load any of them."
    )
    return content + footer


def _append_skill_bundle(
    lines: list[str],
    *,
    agent_def: AgentDefinition,
    cwd: str,
) -> None:
    lines.extend(["", "### Skill Bundle"])
    if not agent_def.include_skills:
        lines.extend(["", "_Skill toolkit disabled for this agent._"])
        return

    registry = load_skill_registry(cwd)
    skill_names = list(agent_def.skills)
    if not skill_names:
        skill_names = [skill.name for skill in registry.list_skills()]
    if not skill_names:
        lines.extend(["", "_No skills attached._"])
        return

    for skill_name in skill_names:
        skill = registry.get(skill_name)
        lines.extend(["", f"#### Skill: {skill_name}"])
        if skill is None:
            lines.append("")
            lines.append("_Skill not found in registry._")
            continue
        rendered = _rendered_skill_content(skill)
        references = getattr(skill, "references", {}) or {}
        path = str(getattr(skill, "path", "") or "")
        source = str(getattr(skill, "source", "") or "")
        lines.extend(
            [
                "",
                f"- Source: `{source or '(unknown)'}`",
                f"- Path: `{path or '(unknown)'}`",
                f"- Rendered content length: `{len(rendered)}` chars / `{len(rendered.encode('utf-8'))}` bytes",
                f"- Reference count: `{len(references)}`",
                "",
                "##### Rendered SKILL.md",
            ]
        )
        _append_text_block(lines, rendered)
        for reference_name, reference_content in references.items():
            lines.extend(
                [
                    "",
                    f"##### Reference: {reference_name}",
                    "",
                    f"- Rendered content length: `{len(reference_content)}` chars / `{len(reference_content.encode('utf-8'))}` bytes",
                ]
            )
            _append_text_block(lines, reference_content)


async def build_team_user_prompt_report_text(
    team_def: TeamDefinition,
    *,
    user_request: str,
    cwd: str,
    settings,
) -> tuple[str, list[str]]:
    """Build representative team user prompts using the production context path.

    Team-task user prompts depend on the current task graph, notes, dependency
    outputs, replanner root cause traces, and scope-change history. This report therefore
    uses a minimal synthetic graph to show the current prompt shape without
    claiming to reproduce a particular live task.
    """
    team_run_id = "prompt-inspection"
    members = _member_roles(team_def.roster, team_def.entry_planner)
    missing: list[str] = []

    tasks: dict[str, Task] = {}
    for agent_name, roles in members.items():
        task = _example_task_for_agent(
            team_run_id=team_run_id,
            entry_planner=team_def.entry_planner,
            agent_name=agent_name,
            roles=roles,
            user_request=user_request,
        )
        tasks[task.id] = task
    task_center = _make_task_context(team_run_id=team_run_id, tasks=tasks)
    team_run = SimpleNamespace(
        id=team_run_id,
        root_task_id="root",
        task_center=task_center,
        roster=dict(team_def.roster),
        team_definition=team_def,
        project_context=SimpleNamespace(repo_root=cwd),
        coordination_metadata={},
        budgets=BudgetConfig(),
        budget_state=BudgetState(),
        sandbox_id="",
    )

    lines = [
        f"# Team User Prompts: {team_def.name}",
        "",
        f"- Team id: `{team_def.id}`",
        f"- Entry planner: `{team_def.entry_planner}`",
        f"- Working directory: `{cwd}`",
        "- Source: representative synthetic task graph rendered through `build_query_context`.",
        "- User prompt templates: `backend/src/prompt/user_prompt/*.md`.",
        "",
        "Team task prompts vary at runtime with task specs, dependency notes, "
        "replan root cause traces, scope paths, and recent edits. This report shows "
        "the current assembly path and prompt shape.",
        "",
        "## Roster",
        "",
    ]
    for role, agent_names in team_def.roster.items():
        joined = ", ".join(f"`{name}`" for name in agent_names) or "(none)"
        lines.append(f"- `{role}`: {joined}")

    for agent_name, roles in members.items():
        agent_def = load_agent_definition(agent_name, settings)
        lines.extend(
            [
                "",
                f"## Agent: {agent_name}",
                "",
                f"- Roles: {', '.join(f'`{role}`' for role in roles)}",
            ]
        )
        if agent_def is None:
            missing.append(agent_name)
            lines.extend(["", "_Agent definition not found in registry or database._"])
            continue

        if getattr(agent_def, "role", None) == "note_taker" or any(
            "note_taker" in role for role in roles
        ):
            lines.extend(
                [
                    "",
                    "_This agent is invoked by the external `tc_note` trigger, not normal task dispatch._",
                    "",
                    "### Edit Trigger",
                ]
            )
            _append_text_block(lines, TC_NOTE_EDIT_PROMPT)
            lines.extend(
                [
                    "### Turn Trigger",
                ]
            )
            _append_text_block(lines, TC_NOTE_TURN_PROMPT)
            continue

        task = tasks["root"] if agent_name == team_def.entry_planner else tasks[f"sample-{agent_name}"]
        ctx = await build_query_context(agent_def, team_run, task)
        lines.extend(
            [
                "",
                f"- Synthetic task id: `{task.id}`",
            ]
        )
        _append_text_block(lines, ctx.user_message)

    return "\n".join(lines).rstrip() + "\n", missing


async def build_team_role_prompt_report_text(
    team_def: TeamDefinition,
    *,
    roles: list[str],
    user_request: str,
    cwd: str,
    settings,
    sandbox_id: str = "",
) -> tuple[str, list[str]]:
    """Build system, user, and skill-bundle prompt artifacts for roles."""
    team_run_id = "prompt-inspection"
    members = _member_roles(team_def.roster, team_def.entry_planner)
    requested_roles = {role.strip() for role in roles if role.strip()}
    missing: list[str] = []

    tasks: dict[str, Task] = {}
    for agent_name, roster_roles in members.items():
        task = _example_task_for_agent(
            team_run_id=team_run_id,
            entry_planner=team_def.entry_planner,
            agent_name=agent_name,
            roles=roster_roles,
            user_request=user_request,
        )
        tasks[task.id] = task
    task_center = _make_task_context(team_run_id=team_run_id, tasks=tasks)
    team_run = SimpleNamespace(
        id=team_run_id,
        root_task_id="root",
        task_center=task_center,
        roster=dict(team_def.roster),
        team_definition=team_def,
        project_context=SimpleNamespace(repo_root=cwd),
        coordination_metadata={},
        budgets=BudgetConfig(),
        budget_state=BudgetState(),
        sandbox_id=sandbox_id,
    )

    lines = [
        f"# Team Role Prompt Report: {team_def.name}",
        "",
        f"- Team id: `{team_def.id}`",
        f"- Entry planner: `{team_def.entry_planner}`",
        f"- Role filter: `{', '.join(sorted(requested_roles)) or 'all'}`",
        f"- Working directory: `{cwd}`",
        f"- Sandbox id: `{sandbox_id or '(none)'}`",
        "- Source: representative synthetic task graph rendered through production prompt assembly.",
        "- Includes benchmark-time SWE-EVO skill overrides when rendering `sweevo_benchmark`.",
        "",
        "## Roster",
        "",
    ]
    for role, agent_names in team_def.roster.items():
        joined = ", ".join(f"`{name}`" for name in agent_names) or "(none)"
        lines.append(f"- `{role}`: {joined}")

    matched = 0
    for agent_name, roster_roles in members.items():
        base_agent_def = load_agent_definition(agent_name, settings)
        if not _role_filter_matches(
            agent_name=agent_name,
            roster_roles=roster_roles,
            agent_def=base_agent_def,
            requested_roles=requested_roles,
        ):
            continue
        matched += 1
        lines.extend(
            [
                "",
                f"## Agent: {agent_name}",
                "",
                f"- Roster roles: {', '.join(f'`{role}`' for role in roster_roles)}",
            ]
        )
        if base_agent_def is None:
            missing.append(agent_name)
            lines.extend(["", "_Agent definition not found in registry or database._"])
            continue

        agent_def = base_agent_def
        terminal_tools = resolve_terminal_tools_for_role(team_def, getattr(agent_def, "role", None))
        lines.extend(
            [
                f"- Agent role: `{getattr(agent_def, 'role', '') or '(none)'}`",
                f"- Attached skills: `{', '.join(agent_def.skills) or '(all registered skills)'}`",
                f"- Terminal tools: `{', '.join(sorted(terminal_tools)) or '(none)'}`",
                "",
                "### System Prompt",
            ]
        )
        system_prompt = build_agent_system_prompt_text(
            agent_def,
            cwd=cwd,
            settings=settings,
            sandbox_id=sandbox_id,
            include_runtime_sections=True,
            terminal_tools=terminal_tools,
        )
        _append_text_block(lines, system_prompt)
        lines.extend(["", "### User Prompt"])
        if getattr(agent_def, "role", None) == "note_taker" or any(
            "note_taker" in role for role in roster_roles
        ):
            lines.extend(["", "#### Edit Trigger"])
            _append_text_block(lines, TC_NOTE_EDIT_PROMPT)
            lines.extend(["", "#### Turn Trigger"])
            _append_text_block(lines, TC_NOTE_TURN_PROMPT)
        else:
            task = tasks["root"] if agent_name == team_def.entry_planner else tasks[f"sample-{agent_name}"]
            ctx = await build_query_context(agent_def, team_run, task)
            _append_text_block(lines, ctx.user_message)
        _append_skill_bundle(lines, agent_def=agent_def, cwd=cwd)

    if matched == 0:
        lines.extend(["", "_No roster agents matched the requested role filter._"])

    return "\n".join(lines).rstrip() + "\n", missing


def _sort_tasks_for_prompt_report(tasks: dict[str, Task]) -> list[Task]:
    return sorted(tasks.values(), key=lambda task: (task.depth, task.created_at, task.id))


def _normalize_task_event_payload(data: dict[str, object]) -> dict[str, object]:
    """Accept legacy TeamRun event logs that stored task text under ``task``."""
    payload = dict(data)
    if not payload.get("objective") and payload.get("task"):
        payload["objective"] = payload["task"]
    return payload


def _replay_team_run_events(
    *,
    team_run_id: str,
    events: list[TeamRunEvent],
) -> tuple[dict[str, Task], list[Note], dict[str, object], str]:
    created = next((event for event in events if event.kind == "team_run_created"), None)
    if created is None:
        raise ValueError(f"event log for {team_run_id!r} missing team_run_created header")

    tasks: dict[str, Task] = {}
    notes: list[Note] = []
    status = ""
    for event in events:
        if event.kind == "task_added":
            task_data = event.data["task"]
            if not isinstance(task_data, dict):
                raise ValueError(f"task_added event {event.seq} contains invalid task payload")
            task = task_from_dict(_normalize_task_event_payload(task_data))
            tasks[task.id] = task
            continue
        if event.kind == "task_status":
            task = tasks.get(str(event.data.get("task_id") or ""))
            if task is None:
                continue
            task.status = TaskStatus.of(event.data.get("status") or task.status, default=task.status)
            if "agent_run_id" in event.data:
                task.agent_run_id = event.data["agent_run_id"]
            if "failure_reason" in event.data:
                task.failure_reason = event.data["failure_reason"]
            if "fired_by_task_id" in event.data:
                task.fired_by_task_id = event.data["fired_by_task_id"]
            continue
        if event.kind == "note_posted":
            content = str(event.data.get("content_preview") or "").strip()
            if not content:
                continue
            notes.append(
                Note(
                    id=f"event-{event.seq}",
                    task_id=str(event.data.get("task_id") or ""),
                    agent_name=str(event.data.get("agent_name") or ""),
                    content=content + "\n\n[preview from persisted event log]",
                    paths=list(event.data.get("scope_paths") or []),
                    tags=[],
                )
            )
            continue
        if event.kind == "team_run_status":
            status = str(event.data.get("status") or status)

    return tasks, notes, dict(created.data), status


async def build_team_run_user_prompt_report_text(
    *,
    team_run_id: str,
    events: list[TeamRunEvent],
    cwd: str,
    settings,
) -> tuple[str, list[str]]:
    """Build task user prompts from a persisted TeamRun event log."""
    tasks, notes, meta, status = _replay_team_run_events(
        team_run_id=team_run_id,
        events=events,
    )
    if not tasks:
        raise ValueError(f"event log for {team_run_id!r} contains no tasks")

    task_center = _make_task_context(team_run_id=team_run_id, tasks=tasks, notes=notes)
    root = next((task for task in tasks.values() if task.depth == 0), None)
    roster = meta.get("roster") if isinstance(meta.get("roster"), dict) else {}
    budgets = budget_config_from_event(meta)
    team_run = SimpleNamespace(
        id=team_run_id,
        session_id=str(meta.get("session_id") or ""),
        user_request=str(meta.get("user_request") or ""),
        root_task_id=root.id if root else None,
        task_center=task_center,
        roster={str(role): list(names) for role, names in dict(roster).items()},
        team_definition=None,
        project_context=SimpleNamespace(repo_root=str(meta.get("repo_root") or cwd)),
        coordination_metadata={},
        budgets=budgets,
        budget_state=BudgetState(tasks_used=len(tasks)),
        sandbox_id=str(meta.get("sandbox_id") or ""),
    )

    lines = [
        f"# Team Run User Prompts: {team_run_id}",
        "",
        f"- Status: `{status or 'unknown'}`",
        f"- Session id: `{team_run.session_id or '(unknown)'}`",
        f"- Working directory: `{cwd}`",
        f"- Repo root: `{team_run.project_context.repo_root or '(unknown)'}`",
        f"- Task count: `{len(tasks)}`",
        f"- Note previews restored: `{len(notes)}`",
        "",
        "This report replays the persisted TeamRun event log and renders each task "
        "through `build_query_context`. Persisted note events contain previews, "
        "so dependency context may be shorter than it was in the live run. "
        "User prompt templates come from `backend/src/prompt/user_prompt/*.md`.",
        "",
    ]
    if team_run.user_request:
        lines.extend(["## User Request", "", "```markdown", team_run.user_request, "```", ""])
    lines.append("## Tasks")

    missing: list[str] = []
    for task in _sort_tasks_for_prompt_report(tasks):
        agent_def = load_agent_definition(task.agent_name, settings)
        lines.extend(
            [
                "",
                f"### Task: {task.id}",
                "",
                f"- Agent: `{task.agent_name}`",
                f"- Status: `{task.status.value}`",
                f"- Depth: `{task.depth}`",
                f"- Parent: `{task.parent_id or '(root)'}`",
            ]
        )
        if task.deps:
            lines.append(f"- Deps: `{', '.join(task.deps)}`")
        if task.scope_paths:
            lines.append(f"- Scope: `{', '.join(task.scope_paths)}`")
        if agent_def is None:
            missing.append(task.agent_name)
            lines.extend(["", "_Agent definition not found in registry or database._"])
            continue
        ctx = await build_query_context(agent_def, team_run, task)
        _append_text_block(lines, ctx.user_message)

    return "\n".join(lines).rstrip() + "\n", sorted(set(missing))


def build_team_user_prompt_report_text_sync(
    team_def: TeamDefinition,
    *,
    user_request: str,
    cwd: str,
    settings,
) -> tuple[str, list[str]]:
    """Synchronous wrapper for CLI entry points."""
    return asyncio.run(
        build_team_user_prompt_report_text(
            team_def,
            user_request=user_request,
            cwd=cwd,
            settings=settings,
        )
    )


def build_team_role_prompt_report_text_sync(
    team_def: TeamDefinition,
    *,
    roles: list[str],
    user_request: str,
    cwd: str,
    settings,
    sandbox_id: str = "",
) -> tuple[str, list[str]]:
    """Synchronous wrapper for role-scoped team prompt reports."""
    return asyncio.run(
        build_team_role_prompt_report_text(
            team_def,
            roles=roles,
            user_request=user_request,
            cwd=cwd,
            settings=settings,
            sandbox_id=sandbox_id,
        )
    )


def build_team_run_user_prompt_report_text_sync(
    *,
    team_run_id: str,
    events: list[TeamRunEvent],
    cwd: str,
    settings,
) -> tuple[str, list[str]]:
    """Synchronous wrapper for persisted TeamRun prompt reports."""
    return asyncio.run(
        build_team_run_user_prompt_report_text(
            team_run_id=team_run_id,
            events=events,
            cwd=cwd,
            settings=settings,
        )
    )


def load_team_run_events(team_run_id: str, *, team_run_dir: str | Path) -> list[TeamRunEvent]:
    """Load persisted TeamRun events from a JSONL TeamRun store."""
    store = JsonlTeamRunStore(team_run_dir)
    return store.load_run(team_run_id)
