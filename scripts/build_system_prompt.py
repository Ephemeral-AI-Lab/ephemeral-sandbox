#!/usr/bin/env python3
"""Build and print the system prompt for a given agent name.

Usage:
    python scripts/build_system_prompt.py <agent_name> [--cwd <dir>]

Examples:
    python scripts/build_system_prompt.py coder
    python scripts/build_system_prompt.py planner --cwd /tmp/project
"""

from __future__ import annotations

import argparse
import os
import sys

from prompt_helpers import (  # type: ignore[attr-defined]
    build_agent_system_prompt_text,
    current_settings,
    load_agent_definition,
    register_builtins,
)


def main() -> None:
    parser = argparse.ArgumentParser(description="Build system prompt for a named agent")
    parser.add_argument("agent_name", help="Name of the agent definition to look up")
    parser.add_argument("--cwd", default=os.getcwd(), help="Working directory (default: cwd)")
    parser.add_argument("--sandbox-id", default="", help="Sandbox ID (passed to toolkit factories)")
    parser.add_argument(
        "--no-capabilities",
        action="store_true",
        help="Skip toolkit/capability awareness section",
    )
    args = parser.parse_args()

    register_builtins()
    settings = current_settings()

    # Try file-based lookup first, then fall back to DB
    agent_def = load_agent_definition(args.agent_name, settings)
    if agent_def is None:
        print(f"Error: agent '{args.agent_name}' not found.", file=sys.stderr)
        sys.exit(1)

    system_prompt = build_agent_system_prompt_text(
        agent_def,
        cwd=args.cwd,
        settings=settings,
        sandbox_id=args.sandbox_id,
        include_capabilities=not args.no_capabilities,
    )

    print(system_prompt)


if __name__ == "__main__":
    main()
