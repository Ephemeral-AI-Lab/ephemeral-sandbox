"""CLI entry points for prompt inspection utilities."""

from __future__ import annotations

import argparse
import os
import sys
from pathlib import Path

from prompt.helpers import (
    build_agent_system_prompt_text,
    build_team_run_user_prompt_report_text_sync,
    build_team_user_prompt_report_text_sync,
    current_settings,
    default_team_run_dir,
    default_team_run_prompt_report_path,
    default_team_prompt_report_path,
    default_team_user_prompt_report_path,
    load_team_run_events,
    load_agent_definition,
    load_team_definition,
    register_builtins,
    resolve_terminal_tools,
)


def build_system_prompt_main() -> int:
    """Build and print the system prompt for a given agent name."""
    parser = argparse.ArgumentParser(description="Build system prompt for a named agent")
    parser.add_argument("agent_name", help="Name of the agent definition to look up")
    parser.add_argument("--cwd", default=os.getcwd(), help="Working directory (default: cwd)")
    parser.add_argument("--sandbox-id", default="", help="Sandbox ID passed to tool setup")
    parser.add_argument(
        "--no-runtime-sections",
        action="store_true",
        help="Skip runtime-added sections such as termination conditions",
    )
    args = parser.parse_args()

    register_builtins()
    settings = current_settings()

    agent_def = load_agent_definition(args.agent_name, settings)
    if agent_def is None:
        print(f"Error: agent '{args.agent_name}' not found.", file=sys.stderr)
        return 1

    system_prompt = build_agent_system_prompt_text(
        agent_def,
        cwd=args.cwd,
        settings=settings,
        sandbox_id=args.sandbox_id,
        include_runtime_sections=not args.no_runtime_sections,
    )

    print(system_prompt)
    return 0


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


def _render_team_prompt_report(
    *,
    team_def,
    cwd: str,
    sandbox_id: str,
    include_runtime_sections: bool,
    settings,
) -> tuple[str, list[str]]:
    """Build the markdown report and return any unresolved agent names."""
    lines = [
        f"# Team System Prompts: {team_def.name}",
        "",
        f"- Team id: `{team_def.id}`",
        f"- Entry planner: `{team_def.entry_planner}`",
        f"- Working directory: `{cwd}`",
        f"- Sandbox id: `{sandbox_id or '(none)'}`",
        f"- Include runtime sections: `{include_runtime_sections}`",
        "",
        "## Roster",
        "",
    ]
    for role, agent_names in team_def.roster.items():
        joined = ", ".join(f"`{name}`" for name in agent_names) or "(none)"
        lines.append(f"- `{role}`: {joined}")

    missing: list[str] = []
    members = _member_roles(team_def.roster, team_def.entry_planner)

    for agent_name, roles in members.items():
        lines.extend(
            [
                "",
                f"## Agent: {agent_name}",
                "",
                f"- Roles: {', '.join(f'`{role}`' for role in roles)}",
            ]
        )
        agent_def = load_agent_definition(agent_name, settings)
        if agent_def is None:
            missing.append(agent_name)
            lines.extend(
                [
                    "",
                    "_Agent definition not found in backend/config registry._",
                ]
            )
            continue

        prompt = build_agent_system_prompt_text(
            agent_def,
            cwd=cwd,
            settings=settings,
            sandbox_id=sandbox_id,
            include_runtime_sections=include_runtime_sections,
            terminal_tools=resolve_terminal_tools(agent_def),
        )
        lines.extend(
            [
                "",
                "```text",
                prompt,
                "```",
            ]
        )

    return "\n".join(lines).rstrip() + "\n", missing


def dump_team_system_prompts_main() -> int:
    """Print and save the assembled system prompts for all members of a team."""
    parser = argparse.ArgumentParser(
        description="Print and save all assembled roster-member system prompts for a team",
    )
    parser.add_argument(
        "team_id",
        help="Team definition id to resolve from the DB. Falls back to team name if no id match is found.",
    )
    parser.add_argument("--cwd", default=os.getcwd(), help="Working directory used for prompt assembly")
    parser.add_argument("--sandbox-id", default="", help="Sandbox ID passed to tool setup")
    parser.add_argument(
        "--output",
        default="",
        help="Output markdown file path. Defaults to ./team-system-prompts-<name>-<id>.md",
    )
    parser.add_argument(
        "--no-runtime-sections",
        action="store_true",
        help="Skip runtime-added sections such as termination conditions",
    )
    args = parser.parse_args()

    register_builtins()
    settings = current_settings()

    team_def = load_team_definition(args.team_id, settings)
    if team_def is None:
        print(f"Error: team '{args.team_id}' not found by id or name.", file=sys.stderr)
        return 1

    report, missing = _render_team_prompt_report(
        team_def=team_def,
        cwd=args.cwd,
        sandbox_id=args.sandbox_id,
        include_runtime_sections=not args.no_runtime_sections,
        settings=settings,
    )
    output_path = Path(args.output) if args.output else default_team_prompt_report_path(team_def)
    output_path.parent.mkdir(parents=True, exist_ok=True)
    output_path.write_text(report, encoding="utf-8")

    sys.stdout.write(report)
    print(f"Saved report to {output_path}", file=sys.stderr)
    if missing:
        print(
            "Warning: missing agent definitions for " + ", ".join(sorted(missing)),
            file=sys.stderr,
        )
        return 2
    return 0


def dump_team_user_prompts_main() -> int:
    """Print and save representative user prompts for all members of a team."""
    parser = argparse.ArgumentParser(
        description="Print and save representative roster-member user prompts for a team",
    )
    parser.add_argument(
        "team_id",
        nargs="?",
        help="Team definition id to resolve from the DB. Falls back to team name if no id match is found.",
    )
    parser.add_argument(
        "--team-run-id",
        default="",
        help="Render prompts from a persisted TeamRun event log instead of a synthetic team definition.",
    )
    parser.add_argument(
        "--team-run-dir",
        default="",
        help="Directory containing TeamRun event logs. Defaults to ./.ephemeralos/team-runs.",
    )
    parser.add_argument("--cwd", default=os.getcwd(), help="Working directory used for prompt assembly")
    parser.add_argument(
        "--user-request",
        default="Inspect the project and propose the smallest correct implementation plan.",
        help="Synthetic root request used for the entry planner prompt",
    )
    parser.add_argument(
        "--output",
        default="",
        help="Output markdown file path. Defaults to ./team-user-prompts-<name>-<id>.md",
    )
    args = parser.parse_args()

    register_builtins()
    settings = current_settings()

    if args.team_run_id:
        team_run_dir = Path(args.team_run_dir) if args.team_run_dir else default_team_run_dir(args.cwd)
        events = load_team_run_events(args.team_run_id, team_run_dir=team_run_dir)
        if not events:
            print(
                f"Error: no events found for team_run_id '{args.team_run_id}' in {team_run_dir}",
                file=sys.stderr,
            )
            return 1
        report, missing = build_team_run_user_prompt_report_text_sync(
            team_run_id=args.team_run_id,
            events=events,
            cwd=args.cwd,
            settings=settings,
        )
        output_path = (
            Path(args.output)
            if args.output
            else default_team_run_prompt_report_path(args.team_run_id)
        )
        output_path.parent.mkdir(parents=True, exist_ok=True)
        output_path.write_text(report, encoding="utf-8")
        sys.stdout.write(report)
        print(f"Saved report to {output_path}", file=sys.stderr)
        if missing:
            print(
                "Warning: missing agent definitions for " + ", ".join(sorted(missing)),
                file=sys.stderr,
            )
            return 2
        return 0

    if not args.team_id:
        print("Error: team_id is required unless --team-run-id is provided.", file=sys.stderr)
        return 1

    team_def = load_team_definition(args.team_id, settings)
    if team_def is None:
        print(f"Error: team '{args.team_id}' not found by id or name.", file=sys.stderr)
        return 1

    report, missing = build_team_user_prompt_report_text_sync(
        team_def,
        user_request=args.user_request,
        cwd=args.cwd,
        settings=settings,
    )
    output_path = (
        Path(args.output)
        if args.output
        else default_team_user_prompt_report_path(team_def)
    )
    output_path.parent.mkdir(parents=True, exist_ok=True)
    output_path.write_text(report, encoding="utf-8")

    sys.stdout.write(report)
    print(f"Saved report to {output_path}", file=sys.stderr)
    if missing:
        print(
            "Warning: missing agent definitions for " + ", ".join(sorted(missing)),
            file=sys.stderr,
        )
        return 2
    return 0
