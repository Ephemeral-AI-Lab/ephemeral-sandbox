#!/usr/bin/env python3
"""Convenience script for dumping team user-prompt reports.

Defaults to the built-in Sweevo team and writes reports under
``.ephemeralos/prompt-reports``.
"""

from __future__ import annotations

# ruff: noqa: E402

import argparse
import sys
from pathlib import Path


_ROOT = Path(__file__).resolve().parent.parent
_BACKEND_SRC = _ROOT / "backend" / "src"
_SCRIPTS_DIR = _ROOT / "scripts"
for path in (_BACKEND_SRC, _SCRIPTS_DIR):
    if str(path) not in sys.path:
        sys.path.insert(0, str(path))

from prompt_helpers import (
    build_team_role_prompt_report_text_sync,
    build_team_run_user_prompt_report_text_sync,
    build_team_user_prompt_report_text_sync,
    current_settings,
    default_team_role_prompt_report_path,
    default_team_run_dir,
    default_team_run_prompt_report_path,
    default_team_user_prompt_report_path,
    load_team_definition,
    load_team_run_events,
    register_builtins,
)


def _default_output_dir() -> Path:
    return _ROOT / ".ephemeralos" / "prompt-reports"


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Dump a team user-prompt report with project defaults.",
    )
    parser.add_argument(
        "--team",
        default="sweevo_benchmark",
        help="Team name or id to render. Defaults to sweevo_benchmark.",
    )
    parser.add_argument(
        "--team-run-id",
        default="",
        help="Render a persisted TeamRun event log instead of a synthetic team definition.",
    )
    parser.add_argument(
        "--team-run-dir",
        default="",
        help="Directory containing TeamRun event logs. Defaults to .ephemeralos/team-runs.",
    )
    parser.add_argument(
        "--role",
        action="append",
        default=[],
        help=(
            "Roster role, agent role, or agent name to render. May be repeated. "
            "Use 'workers' for all non-planner/replanner roles. When provided, "
            "the report includes system prompt, user prompt, and skill-bundle content."
        ),
    )
    parser.add_argument(
        "--sandbox-id",
        default="",
        help="Synthetic sandbox id used for role-scoped capability rendering.",
    )
    parser.add_argument(
        "--user-request",
        default="Inspect the project and propose the smallest correct implementation plan.",
        help="Synthetic root request used when rendering a team definition.",
    )
    parser.add_argument(
        "--output",
        default="",
        help="Output markdown path. Defaults under .ephemeralos/prompt-reports.",
    )
    args = parser.parse_args()

    register_builtins()
    settings = current_settings()

    output_dir = _default_output_dir()
    output_dir.mkdir(parents=True, exist_ok=True)

    if args.team_run_id:
        team_run_dir = Path(args.team_run_dir) if args.team_run_dir else default_team_run_dir(str(_ROOT))
        events = load_team_run_events(args.team_run_id, team_run_dir=team_run_dir)
        if not events:
            print(
                f"Error: no events found for team_run_id {args.team_run_id!r} in {team_run_dir}",
                file=sys.stderr,
            )
            return 1
        report, missing = build_team_run_user_prompt_report_text_sync(
            team_run_id=args.team_run_id,
            events=events,
            cwd=str(_ROOT),
            settings=settings,
        )
        output_path = (
            Path(args.output)
            if args.output
            else default_team_run_prompt_report_path(args.team_run_id, output_dir=str(output_dir))
        )
    else:
        team_def = load_team_definition(args.team, settings)
        if team_def is None:
            print(f"Error: team {args.team!r} not found by id or name.", file=sys.stderr)
            return 1
        if args.role:
            report, missing = build_team_role_prompt_report_text_sync(
                team_def,
                roles=list(args.role),
                user_request=args.user_request,
                cwd=str(_ROOT),
                settings=settings,
                sandbox_id=args.sandbox_id,
            )
            output_path = (
                Path(args.output)
                if args.output
                else default_team_role_prompt_report_path(
                    team_def,
                    list(args.role),
                    output_dir=str(output_dir),
                )
            )
        else:
            report, missing = build_team_user_prompt_report_text_sync(
                team_def,
                user_request=args.user_request,
                cwd=str(_ROOT),
                settings=settings,
            )
            output_path = (
                Path(args.output)
                if args.output
                else default_team_user_prompt_report_path(team_def, output_dir=str(output_dir))
            )

    output_path.parent.mkdir(parents=True, exist_ok=True)
    output_path.write_text(report, encoding="utf-8")
    print(f"Saved report to {output_path}")
    if missing:
        print(
            "Warning: missing agent definitions for " + ", ".join(sorted(missing)),
            file=sys.stderr,
        )
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
