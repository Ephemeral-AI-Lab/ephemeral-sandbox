#!/usr/bin/env python3
"""Dump live tool input/output schemas in a human-readable form."""

from __future__ import annotations

# ruff: noqa: E402

import argparse
import sys
from pathlib import Path
from types import SimpleNamespace


_ROOT = Path(__file__).resolve().parent.parent
_BACKEND_SRC = _ROOT / "backend" / "src"
_SCRIPTS_DIR = _ROOT / "scripts"
if str(_BACKEND_SRC) not in sys.path:
    sys.path.insert(0, str(_BACKEND_SRC))
if str(_SCRIPTS_DIR) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS_DIR))

from tools.core.schema_summary import collect_schema_toolkits, format_tool_schema_summary
from engine.runtime.agent import _build_agent_tool_registry, finalize_tool_registry_and_prompt
from prompt_helpers import (
    current_settings,
    effective_agent_definition_for_team_report,
    load_agent_definition,
    load_team_definition,
    register_builtins,
    resolve_terminal_tools_for_role,
)


def _member_roles(roster: dict[str, list[str]], entry_planner: str) -> dict[str, list[str]]:
    members: dict[str, list[str]] = {}
    for role, agent_names in roster.items():
        for agent_name in agent_names:
            roles = members.setdefault(agent_name, [])
            if role not in roles:
                roles.append(role)
    if entry_planner and entry_planner not in members:
        members[entry_planner] = ["planner"]
    return members


def _role_visibility_summary(
    *,
    team_name: str,
    cwd: Path,
    sandbox_id: str,
    include_descriptions: bool,
    include_instructions: bool,
) -> str:
    register_builtins()
    settings = current_settings()
    team_def = load_team_definition(team_name, settings)
    if team_def is None:
        return f"Team Role Tool Visibility\n  team {team_name!r} not found"

    lines: list[str] = [
        "Team Role Tool Visibility",
        f"  team: {team_def.name}",
        f"  team_id: {team_def.id}",
        "",
    ]
    exposure: dict[str, list[str]] = {
        "submit_plan": [],
        "submit_replan": [],
        "submit_task_summary": [],
        "request_replan": [],
    }
    for agent_name, roster_roles in _member_roles(team_def.roster, team_def.entry_planner).items():
        base_def = load_agent_definition(agent_name, settings)
        if base_def is None:
            lines.extend([f"Agent: {agent_name}", "  missing agent definition", ""])
            continue
        agent_def = effective_agent_definition_for_team_report(base_def, team_def)
        terminal_tools = resolve_terminal_tools_for_role(team_def, getattr(agent_def, "role", None))
        config = SimpleNamespace(cwd=str(cwd))
        registry = _build_agent_tool_registry(config, agent_def, sandbox_id, agent_def.name)
        finalize_tool_registry_and_prompt(
            registry,
            "",
            can_spawn_subagents=agent_def.can_spawn_subagents,
            role=agent_def.role,
            blocked_tools=agent_def.blocked_tools,
            terminal_tools=terminal_tools,
        )
        tool_names = sorted(tool.name for tool in registry.list_tools())
        for tool_name in exposure:
            if tool_name in tool_names:
                exposure[tool_name].append(agent_name)
        lines.extend(
            [
                f"Agent: {agent_name}",
                f"  roster_roles: {', '.join(roster_roles)}",
                f"  agent_role: {agent_def.role or ''}",
                f"  terminal_tools: {', '.join(sorted(terminal_tools)) or '(none)'}",
                f"  visible_tools: {', '.join(tool_names) or '(none)'}",
                format_tool_schema_summary(
                    registry.list_toolkits(),
                    include_descriptions=include_descriptions,
                    include_instructions=include_instructions,
                ),
                "",
            ]
        )

    lines.extend(
        [
            "Visibility Checks",
            f"  submit_plan visible to: {', '.join(exposure['submit_plan']) or '(none)'}",
            f"  submit_replan visible to: {', '.join(exposure['submit_replan']) or '(none)'}",
            f"  submit_task_summary visible to: {', '.join(exposure['submit_task_summary']) or '(none)'}",
            f"  request_replan visible to: {', '.join(exposure['request_replan']) or '(none)'}",
            (
                "  request_replan note: internal TaskCenter method reached through "
                "submit_task_summary(type='fail'), not a model-facing tool."
            ),
        ]
    )
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Print schemas from the live EphemeralOS tool objects.",
    )
    parser.add_argument(
        "--cwd",
        default=str(_ROOT),
        help="Workspace used for runtime tool discovery. Defaults to the repo root.",
    )
    parser.add_argument(
        "--sandbox-id",
        default="schema-dump",
        help="Synthetic sandbox id used when constructing context-aware toolkits.",
    )
    parser.add_argument(
        "--caller-agent",
        default="",
        help="Synthetic caller agent used for caller-aware tool schemas.",
    )
    parser.add_argument(
        "--team",
        default="sweevo_benchmark",
        help="Team name/id for role-filtered visibility checks. Defaults to sweevo_benchmark.",
    )
    parser.add_argument(
        "--no-team-role-visibility",
        action="store_true",
        help="Only dump the global toolkit schemas; skip team role visibility sections.",
    )
    parser.add_argument(
        "--no-descriptions",
        action="store_true",
        help="Omit tool and field descriptions.",
    )
    parser.add_argument(
        "--include-instructions",
        action="store_true",
        help="Include toolkit instructions.",
    )
    parser.add_argument(
        "--output",
        default="",
        help="Optional path to write instead of printing to stdout.",
    )
    args = parser.parse_args()

    toolkits = collect_schema_toolkits(
        cwd=Path(args.cwd),
        sandbox_id=args.sandbox_id,
        caller_agent=args.caller_agent,
    )
    summary = format_tool_schema_summary(
        toolkits,
        include_descriptions=not args.no_descriptions,
        include_instructions=args.include_instructions,
    )
    if not args.no_team_role_visibility:
        summary = (
            summary
            + "\n\n"
            + _role_visibility_summary(
                team_name=args.team,
                cwd=Path(args.cwd),
                sandbox_id=args.sandbox_id,
                include_descriptions=not args.no_descriptions,
                include_instructions=args.include_instructions,
            )
        )

    if args.output:
        output_path = Path(args.output)
        output_path.parent.mkdir(parents=True, exist_ok=True)
        output_path.write_text(summary + "\n", encoding="utf-8")
        print(f"Wrote tool schema summary to {output_path}")
        return 0

    print(summary)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
