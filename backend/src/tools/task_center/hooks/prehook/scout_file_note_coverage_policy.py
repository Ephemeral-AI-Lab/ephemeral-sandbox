"""Enforce exact scout note coverage for batched file-note submissions."""

from __future__ import annotations

from pydantic import BaseModel

from team.core.scope import normalize_scope_paths
from tools.core.base import ToolExecutionContext
from tools.core.hooks import PreHookOutcome, ToolHookRegistry, default_registry


def _normalize_metadata_paths(value: object) -> list[str]:
    if isinstance(value, str):
        return normalize_scope_paths([value])
    if isinstance(value, (list, tuple)):
        raw_paths = [item for item in value if isinstance(item, str)]
        return normalize_scope_paths(raw_paths)
    return []


def _submitted_paths(args: BaseModel) -> list[str]:
    raw_notes = getattr(args, "notes", None)
    if not isinstance(raw_notes, list):
        return []
    submitted: list[str] = []
    for note in raw_notes:
        path = getattr(note, "path", None)
        if isinstance(path, str) and path.strip():
            submitted.append(path)
    return normalize_scope_paths(submitted)


async def hook(
    tool_name: str,
    args: BaseModel,
    context: ToolExecutionContext,
) -> PreHookOutcome:
    del tool_name
    if str(context.metadata.get("agent_name") or "").strip() != "scout":
        return PreHookOutcome()

    assigned = _normalize_metadata_paths(context.metadata.get("write_scope"))
    if not assigned:
        return PreHookOutcome()

    submitted = _submitted_paths(args)
    missing = [path for path in assigned if path not in submitted]
    extra = [path for path in submitted if path not in assigned]
    if not missing and not extra:
        return PreHookOutcome()

    parts: list[str] = [
        "scout submit_file_notes must cover exactly the assigned target_paths."
    ]
    if missing:
        parts.append(f"Missing: {', '.join(missing)}.")
    if extra:
        parts.append(f"Unexpected: {', '.join(extra)}.")
    return PreHookOutcome(
        has_error=True,
        error_message=" ".join(parts),
    )


def register(registry: ToolHookRegistry | None = None) -> None:
    reg = registry or default_registry()
    reg.register(
        "submit_file_notes",
        "pre",
        10,
        hook,
        name="submit_file_notes:scout_exact_scope_coverage",
    )
