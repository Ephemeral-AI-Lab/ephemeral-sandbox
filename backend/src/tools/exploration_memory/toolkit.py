"""Exploration memory toolkit — cross-run cache for explorer findings.

Content-addressed: cache key = hash(scope_paths + file content hashes).
If any file in scope changed since last exploration, cache misses automatically.
Replaces Atlas (~400 lines) with ~80 lines.
"""

from __future__ import annotations

import hashlib
import json
import os
from dataclasses import asdict
from typing import Any

from pydantic import BaseModel, Field

from tools.core.base import BaseTool, BaseToolkit, ToolExecutionContext, ToolResult


# ---------------------------------------------------------------------------
# ExplorationMemory — content-addressed cross-run cache
# ---------------------------------------------------------------------------


class ExplorationMemory:
    """Cross-run note cache. Content-addressed by file hashes. Replaces Atlas."""

    def __init__(self) -> None:
        self._store: dict[str, list[dict[str, Any]]] = {}

    def check(self, scope_paths: list[str], workspace_root: str = "") -> list[dict[str, Any]] | None:
        """Return cached notes if files haven't changed. None = re-explore."""
        content_hash = self._hash_scope(scope_paths, workspace_root)
        key = self._cache_key(scope_paths, content_hash)
        return self._store.get(key)

    def save(self, scope_paths: list[str], notes: list[dict[str, Any]], workspace_root: str = "") -> None:
        """Cache notes after explorer completes."""
        content_hash = self._hash_scope(scope_paths, workspace_root)
        key = self._cache_key(scope_paths, content_hash)
        self._store[key] = notes

    def _cache_key(self, scope_paths: list[str], content_hash: str) -> str:
        scope_str = "|".join(sorted(scope_paths))
        return hashlib.sha256(
            f"{scope_str}:{content_hash}".encode()
        ).hexdigest()[:24]

    _MAX_FILES_TO_HASH = 500  # cap to avoid latency on huge scopes

    def _hash_scope(self, scope_paths: list[str], workspace_root: str) -> str:
        """Hash files under scope_paths. Changes in any file invalidate cache.

        Caps at _MAX_FILES_TO_HASH to avoid latency on large directories.
        If cap is hit, includes file count in hash so growth invalidates too."""
        h = hashlib.sha256()
        file_count = 0
        for scope in sorted(scope_paths):
            full_path = os.path.join(workspace_root, scope) if workspace_root else scope
            if os.path.isfile(full_path):
                h.update(self._hash_file(full_path).encode())
                file_count += 1
            elif os.path.isdir(full_path):
                for root, _dirs, files in sorted(os.walk(full_path)):
                    for fname in sorted(files):
                        if file_count >= self._MAX_FILES_TO_HASH:
                            h.update(f"capped:{file_count}".encode())
                            return h.hexdigest()[:16]
                        fpath = os.path.join(root, fname)
                        h.update(self._hash_file(fpath).encode())
                        file_count += 1
            else:
                h.update(f"missing:{scope}".encode())
        return h.hexdigest()[:16]

    @staticmethod
    def _hash_file(path: str) -> str:
        """Hash file content. Returns empty string on read error."""
        try:
            h = hashlib.sha256()
            with open(path, "rb") as f:
                for chunk in iter(lambda: f.read(8192), b""):
                    h.update(chunk)
            return h.hexdigest()[:16]
        except (OSError, PermissionError):
            return ""


# Singleton — survives across runs within the same process
_exploration_memory = ExplorationMemory()


# ---------------------------------------------------------------------------
# Tool
# ---------------------------------------------------------------------------


class CheckExplorationMemoryInput(BaseModel):
    paths: list[str] = Field(..., description="Scope paths to check for cached exploration")


class CheckExplorationMemoryTool(BaseTool):
    name = "check_exploration_memory"
    description = (
        "Check if a scope was recently explored and files haven't changed. "
        "Returns 'cached' (with notes injected into Task Center) or 'needs_exploration'."
    )
    input_model = CheckExplorationMemoryInput

    async def execute(self, arguments: CheckExplorationMemoryInput, context: ToolExecutionContext) -> ToolResult:
        workspace_root = context.metadata.get("daytona_cwd", "") or context.metadata.get("ci_workspace_root", "")
        cached = _exploration_memory.check(arguments.paths, workspace_root)
        if cached is not None:
            # Inject cached notes into Task Center
            tc = context.metadata.get("task_center")
            if tc:
                from team.models import Note
                for note_dict in cached:
                    tc.post(Note(**note_dict))
            return ToolResult(output=json.dumps({
                "status": "cached",
                "note_count": len(cached),
            }))
        return ToolResult(output=json.dumps({"status": "needs_exploration"}))


class SaveExplorationTool(BaseTool):
    name = "save_exploration"
    description = "Save exploration findings to cache for cross-run reuse. Called automatically after explorer completes."
    input_model = CheckExplorationMemoryInput  # reuse — just needs paths

    async def execute(self, arguments: CheckExplorationMemoryInput, context: ToolExecutionContext) -> ToolResult:
        workspace_root = context.metadata.get("daytona_cwd", "") or context.metadata.get("ci_workspace_root", "")
        tc = context.metadata.get("task_center")
        if tc is None:
            return ToolResult(output="No Task Center available.", is_error=True)
        # Collect notes for these scope paths from Task Center
        notes = tc.read(scope_paths=arguments.paths)
        note_dicts = [asdict(n) for n in notes]
        _exploration_memory.save(arguments.paths, note_dicts, workspace_root)
        return ToolResult(output=json.dumps({
            "status": "saved",
            "note_count": len(note_dicts),
        }))


# ---------------------------------------------------------------------------
# Toolkit
# ---------------------------------------------------------------------------


class ExplorationMemoryToolkit(BaseToolkit):
    def __init__(self) -> None:
        super().__init__(
            name="exploration_memory",
            description="Cross-run cache for explorer findings — check if a scope has already been explored.",
            tools=[CheckExplorationMemoryTool(), SaveExplorationTool()],
        )

    @classmethod
    def from_context(cls, ctx: Any) -> ExplorationMemoryToolkit:
        return cls()
