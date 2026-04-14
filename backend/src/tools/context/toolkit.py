"""Task Center tools — notes + staleness.

Tools:
- post_note                — post a note for other agents
- read_notes               — read/search notes with optional keyword filter
- context_changed_since    — check if context is stale (other agents' edits)

Role-based restrictions are handled via ``blocked_tools`` in agent definitions
rather than separate read/write toolkit variants.
"""

from __future__ import annotations

import json
import time
import uuid

from pydantic import BaseModel, Field

from tools.context.freshness import check_freshness
from tools.core.base import BaseTool, BaseToolkit, ToolExecutionContext, ToolResult


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
    tool_types = frozenset({"external_trigger", "post_run"})

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
    scope: str | None = Field(
        default=None,
        description=(
            "Structural note scope. Use 'siblings' to read sibling-task and descendant notes "
            "for the current task; omit or use 'full' for the whole Task Center."
        ),
    )
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
        if arguments.scope:
            notes = await tc.read_notes(
                task_id=str(context.metadata.get("work_item_id") or ""),
                scope=arguments.scope,
                keyword=arguments.keyword,
                scope_paths=arguments.scope_paths,
                limit=arguments.limit,
            )
        else:
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
        # post_note posthook) only report changes since THIS check,
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
# Toolkit
# ---------------------------------------------------------------------------

_ALL_TOOLS = [
    ReadNotesTool(),
    ContextChangedSinceTool(),
]


class TaskCenterToolkit(BaseToolkit):
    """Task Center notes and scope change queries.

    All tools are registered; role-based restrictions (e.g. blocking
    ``post_note`` for planners) are handled via ``blocked_tools`` in
    agent definitions.
    """

    @classmethod
    def from_context(cls, ctx: object) -> TaskCenterToolkit:
        return cls(
            name="task_center",
            description="Post/read notes and check scope changes.",
            tools=list(_ALL_TOOLS),
        )
