"""Context tools — unified Task Center notes + staleness + exploration memory.

Tools:
- post_note                — post a note for other agents
- read_notes               — read/search notes with optional keyword filter
- context_changed_since    — check if context is stale (other agents' edits)
- check_exploration_memory — check if a scope was recently explored

Role-based restrictions are handled via ``blocked_tools`` in agent definitions
rather than separate read/write toolkit variants.
"""

from __future__ import annotations

import hashlib
import json
import os
import time
import uuid
from typing import Any

from pydantic import BaseModel, Field

from tools.context.freshness import check_freshness
from tools.core.base import BaseTool, BaseToolkit, ToolExecutionContext, ToolResult


# ---------------------------------------------------------------------------
# ExplorationMemory — cross-run note cache (moved from tools.memory.cache)
# ---------------------------------------------------------------------------


class ExplorationMemory:
    """Cross-run note cache. Content-addressed by file hashes.

    Uses an in-memory LRU dict. No PostgreSQL dependency.
    """

    _MAX_FILES_TO_HASH = 500
    _MAX_CACHE_ENTRIES = 256

    def __init__(self) -> None:
        self._cache: dict[str, list[dict[str, Any]]] = {}

    def attach_store(self, store: Any) -> None:
        """No-op — kept for backwards compatibility."""

    def attach_pg(self, pg_store: Any) -> None:
        """No-op — kept for backwards compatibility."""

    async def check_async(
        self,
        scope_paths: list[str],
        workspace_root: str = "",
    ) -> list[dict[str, Any]] | None:
        """Check the in-memory cache for cached notes."""
        content_hash = self._hash_scope(scope_paths, workspace_root)
        key = self._cache_key(scope_paths, content_hash)
        return self._cache.get(key)

    async def save_async(
        self,
        scope_paths: list[str],
        notes: list[dict[str, Any]],
        workspace_root: str = "",
    ) -> None:
        """Write notes to the in-memory cache."""
        content_hash = self._hash_scope(scope_paths, workspace_root)
        key = self._cache_key(scope_paths, content_hash)
        # Simple LRU: evict oldest when full
        if len(self._cache) >= self._MAX_CACHE_ENTRIES and key not in self._cache:
            oldest = next(iter(self._cache))
            del self._cache[oldest]
        self._cache[key] = notes

    def _cache_key(self, scope_paths: list[str], content_hash: str) -> str:
        scope_str = "|".join(sorted(scope_paths))
        return hashlib.sha256(f"{scope_str}:{content_hash}".encode()).hexdigest()[:24]

    def _hash_scope(self, scope_paths: list[str], workspace_root: str) -> str:
        """Hash files under scope_paths to invalidate stale cache entries."""
        digest = hashlib.sha256()
        file_count = 0
        for scope in sorted(scope_paths):
            full_path = os.path.join(workspace_root, scope) if workspace_root else scope
            if os.path.isfile(full_path):
                digest.update(self._hash_file(full_path).encode())
                file_count += 1
            elif os.path.isdir(full_path):
                for root, _dirs, files in sorted(os.walk(full_path)):
                    for fname in sorted(files):
                        if file_count >= self._MAX_FILES_TO_HASH:
                            digest.update(f"capped:{file_count}".encode())
                            return digest.hexdigest()[:16]
                        file_path = os.path.join(root, fname)
                        digest.update(self._hash_file(file_path).encode())
                        file_count += 1
            else:
                digest.update(f"missing:{scope}".encode())
        return digest.hexdigest()[:16]

    @staticmethod
    def _hash_file(path: str) -> str:
        try:
            digest = hashlib.sha256()
            with open(path, "rb") as handle:
                for chunk in iter(lambda: handle.read(8192), b""):
                    digest.update(chunk)
            return digest.hexdigest()[:16]
        except (OSError, PermissionError):
            return ""


_exploration_memory = ExplorationMemory()


def get_exploration_memory() -> ExplorationMemory:
    """Return the process-wide exploration cache singleton."""
    return _exploration_memory


# ---------------------------------------------------------------------------
# PostNoteTool
# ---------------------------------------------------------------------------


class PostNoteInput(BaseModel):
    content: str = Field(..., description="Note content to post", min_length=1)
    scope_paths: list[str] | None = Field(
        default=None,
        description=(
            "File/dir scope for filtering. If omitted, defaults to the task's "
            "write_scope. Other agents can find this note via read_notes(scope_paths=[...])."
        ),
    )


class PostNoteTool(BaseTool):
    name = "post_note"
    description = (
        "Post a note to the Task Center for other agents to read. "
        "Use for: blockers that siblings should know about, partial progress "
        "updates on long tasks, discoveries about the codebase that downstream "
        "tasks need, and exploration findings (scouts). Notes are append-only "
        "and immutable — post a new note to update, don't try to edit."
    )
    input_model = PostNoteInput

    async def execute(self, arguments: BaseModel, context: ToolExecutionContext) -> ToolResult:
        assert isinstance(arguments, PostNoteInput)
        from team.models import Note

        tc = context.metadata.get("task_center")
        if tc is None:
            return ToolResult(output="Error: Task Center not available", is_error=True)
        scope = arguments.scope_paths or list(context.metadata.get("write_scope") or [])
        note = Note(
            id=str(uuid.uuid4()),
            task_id=context.metadata.get("work_item_id", ""),
            agent_name=context.metadata.get("agent_name", ""),
            content=arguments.content,
            timestamp=time.time(),
            scope_paths=scope,
        )
        await tc.post(note)
        return ToolResult(output=f"Note posted ({len(arguments.content)} chars).")


# ---------------------------------------------------------------------------
# ReadNotesTool — absorbs former search_context via optional keyword param
# ---------------------------------------------------------------------------


class ReadNotesInput(BaseModel):
    authors: list[str] | None = Field(
        default=None,
        description=(
            "Filter by task IDs that authored the notes. Task IDs appear in "
            "your context under 'Context from dependencies' headers as (task_id)."
        ),
    )
    scope_paths: list[str] | None = Field(
        default=None,
        description=(
            "Filter by scope path prefix — returns notes whose scope_paths "
            "overlap with these prefixes (e.g. 'src/auth/' matches 'src/auth/session.py')."
        ),
    )
    keyword: str | None = Field(
        default=None, description="Keyword filter (case-insensitive substring match on note content)"
    )
    limit: int | None = Field(default=None, description="Max notes to return (most recent first)")


class ReadNotesTool(BaseTool):
    name = "read_notes"
    description = (
        "Read notes from the Task Center, optionally filtered by scope, author, "
        "or keyword. Use scope_paths to find notes about specific files/dirs. "
        "Use after explorer waves to read findings, before widening into shared "
        "scopes to check sibling activity, and before retrying to see what changed."
    )
    input_model = ReadNotesInput

    async def execute(self, arguments: BaseModel, context: ToolExecutionContext) -> ToolResult:
        assert isinstance(arguments, ReadNotesInput)

        tc = context.metadata.get("task_center")
        if tc is None:
            return ToolResult(output="Error: Task Center not available", is_error=True)
        notes = await tc.read(
            authors=arguments.authors,
            scope_paths=arguments.scope_paths,
            limit=arguments.limit,
        )
        if arguments.keyword:
            kw = arguments.keyword.lower()
            notes = [n for n in notes if kw in n.content.lower()]
        if not notes:
            return ToolResult(output="No notes found.")
        lines: list[str] = []
        for n in notes:
            header = f"### {n.agent_name} ({n.task_id})"
            if n.scope_paths:
                header += f" [scope: {', '.join(n.scope_paths)}]"
            lines.append(header)
            lines.append(n.content)
            lines.append("")
        return ToolResult(output="\n".join(lines))


# ---------------------------------------------------------------------------
# ContextChangedSinceTool
# ---------------------------------------------------------------------------


class ContextChangedSinceInput(BaseModel):
    pass  # No arguments needed — uses task start time


class ContextChangedSinceTool(BaseTool):
    name = "context_changed_since"
    description = "Check if your context has changed since task started. Call before committing multi-file changes."
    input_model = ContextChangedSinceInput

    async def execute(
        self, arguments: ContextChangedSinceInput, context: ToolExecutionContext
    ) -> ToolResult:
        context.metadata["checked_context_freshness"] = True
        report = await check_freshness(context)
        # Update the freshness baseline so subsequent checks (e.g. in
        # submit_summary posthook) only report changes since THIS check,
        # not since work_item_started_at.  Fixes the monotonic-count bug
        # where sibling completions accumulate across the entire run.
        import time as _time
        context.metadata["freshness_checked_at"] = _time.time()
        return ToolResult(
            output=json.dumps(
                {
                    "stale": report.stale,
                    "scope_changes_by_others": report.scope_changes_by_others,
                    "new_dep_notes": report.new_dep_notes,
                    "new_sibling_completions": report.new_sibling_completions,
                    "suggestion": "Re-read affected files and check Task Center "
                    "for new context before committing."
                    if report.stale
                    else None,
                }
            )
        )


# ---------------------------------------------------------------------------
# CheckExplorationMemoryTool (absorbed from tools.memory)
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

    async def execute(
        self, arguments: CheckExplorationMemoryInput, context: ToolExecutionContext
    ) -> ToolResult:
        mem = get_exploration_memory()
        workspace_root = context.metadata.get("daytona_cwd", "") or context.metadata.get(
            "ci_workspace_root", ""
        )
        cached = await mem.check_async(arguments.paths, workspace_root)
        if cached is not None:
            tc = context.metadata.get("task_center")
            if tc:
                from team.models import Note

                for note_dict in cached:
                    await tc.post(Note(**note_dict))
            return ToolResult(
                output=json.dumps(
                    {
                        "status": "cached",
                        "note_count": len(cached),
                    }
                )
            )
        return ToolResult(output=json.dumps({"status": "needs_exploration"}))


# ---------------------------------------------------------------------------
# Toolkit
# ---------------------------------------------------------------------------

_ALL_TOOLS = [
    PostNoteTool(),
    ReadNotesTool(),
    ContextChangedSinceTool(),
    CheckExplorationMemoryTool(),
]


class ContextToolkit(BaseToolkit):
    """Task Center notes, scope change queries, and exploration cache.

    All tools are registered; role-based restrictions (e.g. blocking
    ``post_note`` for planners) are handled via ``blocked_tools`` in
    agent definitions.
    """

    @classmethod
    def from_context(cls, ctx: object) -> ContextToolkit:
        return cls(
            name="context",
            description="Post/read notes, check scope changes, and query exploration cache.",
            tools=list(_ALL_TOOLS),
        )
