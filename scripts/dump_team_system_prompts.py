#!/usr/bin/env python3
"""Print and save the assembled system prompts for all members of a team."""

from __future__ import annotations

import argparse
import os
import sys
from collections import OrderedDict
from pathlib import Path

from prompt_helpers import (  # type: ignore[attr-defined]
    build_agent_system_prompt_text,
    current_settings,
    default_team_prompt_report_path,
    load_agent_definition,
    load_team_definition,
    register_builtins,
)


def _member_roles(roster: dict[str, list[str]], entry_planner: str) -> OrderedDict[str, list[str]]:
    """Return unique team members mapped to their roster roles in stable order."""
    members: OrderedDict[str, list[str]] = OrderedDict()
    for role, agent_names in roster.items():
        for agent_name in agent_names:
            roles = members.setdefault(agent_name, [])
            if role not in roles:
                roles.append(role)
    if entry_planner and entry_planner not in members:
        members[entry_planner] = ["planner"]
    return members


def _render_report(
    *,
    team_def,
    cwd: str,
    sandbox_id: str,
    include_capabilities: bool,
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
        f"- Include capabilities: `{include_capabilities}`",
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
        lines.extend([
            "",
            f"## Agent: {agent_name}",
            "",
            f"- Roles: {', '.join(f'`{role}`' for role in roles)}",
        ])
        agent_def = load_agent_definition(agent_name, settings)
        if agent_def is None:
            missing.append(agent_name)
            lines.extend([
                "",
                "_Agent definition not found in registry or database._",
            ])
            continue

        prompt = build_agent_system_prompt_text(
            agent_def,
            cwd=cwd,
            settings=settings,
            sandbox_id=sandbox_id,
            include_capabilities=include_capabilities,
        )
        lines.extend([
            "",
            "```text",
            prompt,
            "```",
        ])

    return "\n".join(lines).rstrip() + "\n", missing


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Print and save all assembled roster-member system prompts for a team",
    )
    parser.add_argument(
        "team_id",
        help="Team definition id to resolve from the DB. Falls back to team name if no id match is found.",
    )
    parser.add_argument("--cwd", default=os.getcwd(), help="Working directory used for prompt assembly")
    parser.add_argument("--sandbox-id", default="", help="Sandbox ID passed to toolkit factories")
    parser.add_argument(
        "--output",
        default="",
        help="Output markdown file path. Defaults to ./team-system-prompts-<name>-<id>.md",
    )
    parser.add_argument(
        "--no-capabilities",
        action="store_true",
        help="Skip toolkit/capability awareness sections",
    )
    args = parser.parse_args()

    register_builtins()
    settings = current_settings()

    team_def = load_team_definition(args.team_id, settings)
    if team_def is None:
        print(
            f"Error: team '{args.team_id}' not found by id or name.",
            file=sys.stderr,
        )
        return 1

    report, missing = _render_report(
        team_def=team_def,
        cwd=args.cwd,
        sandbox_id=args.sandbox_id,
        include_capabilities=not args.no_capabilities,
        settings=settings,
    )
    output_path = Path(args.output) if args.output else default_team_prompt_report_path(team_def)
    output_path.parent.mkdir(parents=True, exist_ok=True)
    output_path.write_text(report, encoding="utf-8")

    sys.stdout.write(report)
    print(f"Saved report to {output_path}", file=sys.stderr)
    if missing:
        print(
            "Warning: missing agent definitions for "
            + ", ".join(sorted(missing)),
            file=sys.stderr,
        )
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
