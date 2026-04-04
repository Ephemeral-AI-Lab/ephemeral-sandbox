"""CI integration helpers for the Daytona toolkit.

Provides gateway acquisition, tree cache priming after writes,
and shell mutation detection. All CI features are optional —
tools degrade gracefully if no CI service is configured.
"""

from __future__ import annotations

import logging
from typing import Any

from ephemeralos.tools.base import ToolExecutionContext

logger = logging.getLogger(__name__)


def get_ci_gateway(context: ToolExecutionContext) -> Any | None:
    """Get the CodeIntelligenceGateway from context, or None if unavailable."""
    return context.metadata.get("ci_gateway")


def get_ci_service(context: ToolExecutionContext) -> Any | None:
    """Get the CodeIntelligenceService from context, or None."""
    gw = get_ci_gateway(context)
    if gw is None:
        return None
    return getattr(gw, "_resolve_service", lambda: None)()


def prime_cache_after_write(context: ToolExecutionContext, file_path: str, content: str) -> None:
    """Prime the tree cache and refresh the symbol index after a write."""
    gw = get_ci_gateway(context)
    if gw is None:
        return
    try:
        tc = gw.tree_cache
        if tc:
            tc.put_content(file_path, content)
        si = gw.symbol_index
        if si:
            si.refresh(file_path, content)
        # Invalidate LSP cache
        gw.invalidate(file_path)
    except Exception:
        logger.debug("CI prime_cache_after_write failed for %s", file_path)


def record_edit_in_ledger(
    context: ToolExecutionContext,
    file_path: str,
    agent_id: str = "",
    edit_type: str = "edit",
    old_hash: str = "",
    new_hash: str = "",
    description: str = "",
) -> None:
    """Record an edit in the CI ledger if available."""
    gw = get_ci_gateway(context)
    if gw is None:
        return
    try:
        ledger = gw.ledger
        if ledger:
            ledger.record(
                file_path=file_path,
                agent_id=agent_id,
                edit_type=edit_type,
                old_hash=old_hash,
                new_hash=new_hash,
                description=description,
            )
    except Exception:
        logger.debug("CI record_edit_in_ledger failed for %s", file_path)


def classify_command_mutation(command: str) -> str:
    """Classify a shell command's mutation potential.

    Returns one of: 'read_only', 'test_like', 'mutating'.
    """
    cmd = command.strip().split()[0] if command.strip() else ""

    read_only_commands = {
        "ls", "cat", "head", "tail", "grep", "rg", "find", "wc",
        "file", "stat", "which", "type", "echo", "pwd", "env",
        "printenv", "whoami", "id", "uname", "date", "df", "du",
        "free", "top", "ps", "git log", "git status", "git diff",
        "git show", "git branch",
    }

    test_commands = {
        "pytest", "python -m pytest", "npm test", "npx jest",
        "cargo test", "go test", "make test", "yarn test",
        "python -m unittest", "nosetests", "tox",
    }

    # Check read-only
    if cmd in read_only_commands:
        return "read_only"
    for ro in read_only_commands:
        if command.strip().startswith(ro):
            return "read_only"

    # Check test-like
    for tc in test_commands:
        if command.strip().startswith(tc):
            return "test_like"

    return "mutating"


def maybe_invalidate_after_shell(
    context: ToolExecutionContext,
    command: str,
) -> None:
    """Invalidate CI caches after a shell command if it was mutating."""
    classification = classify_command_mutation(command)
    if classification == "read_only":
        return

    gw = get_ci_gateway(context)
    if gw is None:
        return

    try:
        if classification == "mutating":
            # Full invalidation for mutating commands
            gw.invalidate_all()
        elif classification == "test_like":
            # Lightweight: no invalidation for test commands
            pass
    except Exception:
        logger.debug("CI invalidation after shell failed")
