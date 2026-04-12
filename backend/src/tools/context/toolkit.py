"""Context toolkit — unified Task Center notes + staleness queries.

Tools:
- post_note        — post a note for other agents (write variant only)
- read_notes       — read/search notes with optional keyword filter
- context_changed_since — check if context is stale (other agents' edits)
"""

from __future__ import annotations

import json
import time
import uuid
from typing import Any

from pydantic import BaseModel, Field

from tools.core.base import BaseTool, BaseToolkit, ToolExecutionContext, ToolResult


# ---------------------------------------------------------------------------
# PostNoteTool
# ---------------------------------------------------------------------------


class PostNoteInput(BaseModel):
    content: str = Field(..., description="Note content to post", min_length=1)
    scope_paths: list[str] | None = Field(default=None, description="File/dir scope for filtering")


class PostNoteTool(BaseTool):
    name = "post_note"
    description = "Post a note to the Task Center for other agents to read."
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
        default=None, description="Filter by task IDs that authored the notes"
    )
    scope_paths: list[str] | None = Field(default=None, description="Filter by scope path prefix")
    keyword: str | None = Field(
        default=None, description="Keyword filter (case-insensitive substring match)"
    )
    limit: int | None = Field(default=None, description="Max notes to return")


class ReadNotesTool(BaseTool):
    name = "read_notes"
    description = (
        "Read notes from the Task Center, optionally filtered by author, scope, or keyword."
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
        since = context.metadata.get("work_item_started_at", 0)
        task_id = context.metadata.get("work_item_id", "")
        agent_run_id = context.metadata.get("agent_run_id", "")

        scope_changes = 0
        new_dep_notes = 0
        new_sibling_completions = 0

        arbiter = context.metadata.get("arbiter")
        scope_paths = context.metadata.get("write_scope") or []
        if arbiter is not None and scope_paths:
            changes = arbiter.changes_since(since)
            scope_changes = sum(
                1
                for e in changes
                if e.agent_id != agent_run_id
                and any(e.file_path.startswith(p.rstrip("/")) for p in scope_paths)
            )

        tc = context.metadata.get("task_center")
        dispatcher = context.metadata.get("dispatcher")
        if tc is not None:
            task_deps = set(context.metadata.get("task_deps", []))
            if task_deps:
                dep_notes = await tc.read(authors=list(task_deps), since=since)
                new_dep_notes = len(dep_notes)
        if dispatcher is not None and hasattr(dispatcher, "done_sibling_ids"):
            sibling_ids = await dispatcher.done_sibling_ids(
                task_id=task_id,
                parent_id=context.metadata.get("task_parent_id"),
                since=since,
            )
            new_sibling_completions = len(sibling_ids)

        stale = scope_changes > 0 or new_dep_notes > 0 or new_sibling_completions > 0
        return ToolResult(
            output=json.dumps(
                {
                    "stale": stale,
                    "scope_changes_by_others": scope_changes,
                    "new_dep_notes": new_dep_notes,
                    "new_sibling_completions": new_sibling_completions,
                    "suggestion": "Re-read affected files and check Task Center "
                    "for new context before committing."
                    if stale
                    else None,
                }
            )
        )


# ---------------------------------------------------------------------------
# Toolkits
# ---------------------------------------------------------------------------

_READ_TOOLS = [ReadNotesTool(), ContextChangedSinceTool()]
_WRITE_TOOLS = [PostNoteTool()] + _READ_TOOLS


class ContextReadToolkit(BaseToolkit):
    """Read-only access to Task Center notes and scope change queries."""

    @classmethod
    def from_context(cls, ctx: object) -> ContextReadToolkit:
        return cls(
            name="context_read",
            description="Read notes and check scope changes.",
            tools=list(_READ_TOOLS),
        )


class ContextWriteToolkit(BaseToolkit):
    """Full read/write access to Task Center notes and scope change queries."""

    @classmethod
    def from_context(cls, ctx: object) -> ContextWriteToolkit:
        return cls(
            name="context_write",
            description="Post/read notes and check scope changes.",
            tools=list(_WRITE_TOOLS),
        )
