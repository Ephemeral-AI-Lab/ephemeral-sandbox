"""Task Center tools — notes + staleness.

Tools exposed in the main loop:
- read_task_note                — read/search notes with optional keyword filter
- task_center_changed_since     — check if task-center state is stale

Role-based restrictions are handled via ``blocked_tools`` in agent definitions
rather than separate read/write toolkit variants.
"""

from __future__ import annotations

import json
import re
import time
import uuid
from typing import Literal

from pydantic import BaseModel, Field

from team._path_utils import normalize_scope_paths, scope_paths_overlap
from tools.task_center.freshness import check_freshness
from tools.core.base import (
    BaseTool,
    BaseToolkit,
    TextToolOutput,
    ToolExecutionContext,
    ToolResult,
)

_BACKTICK_PATH_RE = re.compile(r"`([^`\n]+)`")


def _scout_scope_repair_paths(content: str, note_paths: list[str]) -> list[str]:
    if "does not exist" not in content.lower():
        return []
    leaked: list[str] = []
    for token in _BACKTICK_PATH_RE.findall(content):
        candidate = token.strip().replace("\\", "/").rstrip("/")
        if "/" not in candidate or " " in candidate:
            continue
        if any(scope_paths_overlap(candidate, allowed) for allowed in note_paths):
            continue
        leaked.append(candidate)
    return normalize_scope_paths(leaked)


def _sanitize_scout_gap_paths(content: str, note_paths: list[str]) -> str:
    leaked = set(_scout_scope_repair_paths(content, note_paths))
    if not leaked:
        return content

    def _rewrite(match: re.Match[str]) -> str:
        token = match.group(1).strip().replace("\\", "/").rstrip("/")
        return token if token in leaked else match.group(0)

    return _BACKTICK_PATH_RE.sub(_rewrite, content)


# ---------------------------------------------------------------------------
# SubmitTaskNoteTool
# ---------------------------------------------------------------------------


class PostNoteInput(BaseModel):
    content: str = Field(
        ...,
        description=(
            "REQUIRED. Put the entire Task Center note here as a non-empty string. "
            "Always send this field in the tool input object, and never put the "
            "note only in assistant text. The tool input JSON must look like "
            '{"content":"<concise Task Center note>","paths":["<path>"],'
            '"tags":["discovery"]}.'
        ),
        min_length=1,
    )
    paths: list[str] | None = Field(
        default=None,
        description=(
            "File/dir paths this note relates to. Can be existing or planned paths. "
            "If omitted, defaults to the task's write_scope. Other agents can find "
            "this note via read_task_note(paths=[...])."
        ),
    )
    tags: list[str] | None = Field(
        default=None,
        description=(
            "Classify the note with one or more tags: discovery, implementation, "
            "bug_fix, blocker, proposal, verification, architecture, dependency, "
            "warning, refactor. Use 'proposal' for notes about paths not yet created."
        ),
    )
    parent_note_id: str | None = Field(
        default=None,
        description="ID of a prior note this is a follow-up to (threading).",
    )


class TaskNoteOutput(BaseModel):
    note_id: str = Field(..., description="Created Task Center note id.")
    task_id: str = Field(..., description="Runtime-stamped task id that owns the note.")
    agent_name: str = Field(..., description="Runtime-stamped agent name that posted the note.")
    content: str = Field(..., description="Stored note content.")
    timestamp: float = Field(..., description="Unix timestamp when the note was posted.")
    paths: list[str] = Field(default_factory=list, description="Scope paths attached to the note.")
    tags: list[str] = Field(default_factory=list, description="Tags attached to the note.")
    parent_note_id: str | None = Field(
        default=None,
        description="Parent note id when the note is part of a thread.",
    )


class SubmitTaskNoteTool(BaseTool):
    name = "submit_task_note"
    description = (
        "Post a note to the Task Center for other agents to read. "
        "The input object must include non-empty `content`. "
        'Use JSON like {"content":"<concise Task Center note>","paths":["<path>"],'
        '"tags":["discovery"]}; put the note in the `content` field rather than '
        "assistant text. "
        "Use for: blockers that siblings should know about, partial progress "
        "updates on long tasks, discoveries about the codebase that downstream "
        "tasks need, and exploration findings (scouts). Notes are append-only "
        "and immutable — post a new note to update, don't try to edit."
    )
    short_description = "Post a Task Center note."
    input_model = PostNoteInput
    output_model = TaskNoteOutput

    async def execute(self, arguments: BaseModel, context: ToolExecutionContext) -> ToolResult:
        assert isinstance(arguments, PostNoteInput)
        from team.models import Note, NoteTag

        tc = context.metadata.get("task_center")
        if tc is None:
            return ToolResult(output="Error: Task Center not available", is_error=True)

        # Validate tags
        if arguments.tags:
            valid_tags = {t.value for t in NoteTag}
            invalid = [t for t in arguments.tags if t not in valid_tags]
            if invalid:
                return ToolResult(
                    output=f"Invalid tag(s): {invalid}. Valid tags: {sorted(valid_tags)}",
                    is_error=True,
                )

        content = arguments.content
        note_paths = normalize_scope_paths(
            arguments.paths or list(context.metadata.get("write_scope") or [])
        )
        if str(context.metadata.get("agent_name") or "").strip() == "scout" and note_paths:
            if "intended path" not in content.lower() and "correct path" not in content.lower():
                content = _sanitize_scout_gap_paths(content, note_paths)
        note = Note(
            id=str(uuid.uuid4()),
            task_id=context.metadata.get("work_item_id", ""),
            agent_name=context.metadata.get("agent_name", ""),
            content=content,
            timestamp=time.time(),
            paths=note_paths,
            tags=list(arguments.tags or []),
            parent_note_id=arguments.parent_note_id,
        )
        await tc.notes.post(note)
        payload = TaskNoteOutput(
            note_id=note.id,
            task_id=note.task_id,
            agent_name=note.agent_name,
            content=note.content,
            timestamp=note.timestamp,
            paths=note.paths,
            tags=note.tags,
            parent_note_id=note.parent_note_id,
        )
        return ToolResult(output=payload.model_dump_json())


# ---------------------------------------------------------------------------
# TaskCenterChangedSinceTool
# ---------------------------------------------------------------------------


class TaskCenterChangedSinceInput(BaseModel):
    pass  # No arguments needed — uses task start time


class TaskCenterChangedSinceOutput(BaseModel):
    stale: bool = Field(..., description="Whether relevant Task Center state changed.")
    scope_changes_by_others: list[dict[str, object]] = Field(
        default_factory=list,
        description="Changes made by other agents in overlapping scope.",
    )
    new_dep_notes: list[dict[str, object]] = Field(
        default_factory=list,
        description="New notes from dependency tasks since the freshness baseline.",
    )
    new_sibling_completions: list[dict[str, object]] = Field(
        default_factory=list,
        description="Sibling task completions since the freshness baseline.",
    )
    suggestion: str | None = Field(
        default=None,
        description="Suggested next action when the task context is stale.",
    )


class TaskCenterChangedSinceTool(BaseTool):
    name = "task_center_changed_since"
    description = (
        "Check if Task Center state has changed since task start. "
        "Call before committing multi-file changes."
    )
    short_description = "Check whether Task Center state is stale."
    input_model = TaskCenterChangedSinceInput
    output_model = TaskCenterChangedSinceOutput

    async def execute(
        self, arguments: TaskCenterChangedSinceInput, context: ToolExecutionContext
    ) -> ToolResult:
        context.metadata["checked_context_freshness"] = True
        report = await check_freshness(context)
        # Update the freshness baseline so subsequent checks (e.g. in
        # submit_task_note) only report changes since THIS check,
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
                    "for new state before committing."
                    if report.stale
                    else None,
                }
            )
        )


# ---------------------------------------------------------------------------
# ReadTaskNoteTool — unified read with scope parameter
# ---------------------------------------------------------------------------


class ReadTaskNoteInput(BaseModel):
    scope: Literal["own", "sibling"] = Field(
        default="own",
        description=(
            "'own' reads notes from your own task. Background scout/subagent notes "
            "created by run_subagent are own-scope notes. 'sibling' reads from true "
            "sibling team tasks and descendants."
        ),
    )
    task_ids: list[str] | None = Field(
        default=None,
        description="Filter by specific task IDs. Overrides scope — returns notes only from these tasks.",
    )
    paths: list[str] | None = Field(
        default=None,
        description="Filter by path prefix — returns notes whose paths overlap with these prefixes.",
    )
    tags: list[str] | None = Field(
        default=None,
        description=(
            "Filter by tag (OR semantics). Valid tags: discovery, implementation, "
            "bug_fix, blocker, proposal, verification, architecture, dependency, "
            "warning, refactor."
        ),
    )
    keyword: str | None = Field(
        default=None,
        description="Keyword filter (case-insensitive substring match). Use '|' for OR matching.",
    )
    last_n: int | None = Field(default=None, description="Return only the N most recent matching notes.")


class ReadTaskNoteTool(BaseTool):
    name = "read_task_note"
    description = (
        "Read notes from the Task Center. Use scope='own' for your task's notes, "
        "including notes posted by run_subagent scouts; omit scope or keep scope='own' "
        "after a background scout wave. Use scope='sibling' only for true sibling "
        "team tasks. ALWAYS include paths=[<your_scope_paths>] to scope reads. "
        "Also use tags= and keyword= for filtering."
    )
    short_description = "Read Task Center notes."
    input_model = ReadTaskNoteInput
    output_model = TextToolOutput

    async def execute(self, arguments: BaseModel, context: ToolExecutionContext) -> ToolResult:
        assert isinstance(arguments, ReadTaskNoteInput)
        from team.models import NoteTag

        tc = context.metadata.get("task_center")
        if tc is None:
            return ToolResult(output="Error: Task Center not available", is_error=True)

        if arguments.tags:
            valid_tags = {t.value for t in NoteTag}
            invalid = [t for t in arguments.tags if t not in valid_tags]
            if invalid:
                return ToolResult(
                    output=f"Invalid tag(s): {invalid}. Valid tags: {sorted(valid_tags)}",
                    is_error=True,
                )

        if arguments.task_ids:
            # Direct task_id filter — bypasses scope logic
            notes = await tc.notes.read(
                authors=arguments.task_ids,
                paths=arguments.paths,
                tags=arguments.tags,
                keyword=arguments.keyword,
                last_n=arguments.last_n,
            )
        elif arguments.scope == "sibling":
            task_id = str(context.metadata.get("work_item_id") or "")
            if not task_id:
                return ToolResult(output="Error: no task context available", is_error=True)
            notes = await tc.notes.read_sibling_notes(
                task_id=task_id,
                paths=arguments.paths,
                tags=arguments.tags,
                keyword=arguments.keyword,
                last_n=arguments.last_n,
            )
        else:
            if arguments.paths:
                matched = await tc.notes.read(paths=arguments.paths)
                if not matched:
                    known = tc.notes.known_paths()
                    return ToolResult(
                        output=(
                            f"No notes found for paths: {arguments.paths}. "
                            f"Known note paths: {known}"
                        ),
                    )
            notes = await tc.notes.read_notes(
                paths=arguments.paths,
                tags=arguments.tags,
                keyword=arguments.keyword,
                last_n=arguments.last_n,
            )

        if not notes:
            return ToolResult(output="No notes found.")
        lines: list[str] = []
        for n in notes:
            header = f"### {n.agent_name} ({n.task_id})"
            if n.paths:
                header += f" [paths: {', '.join(n.paths)}]"
            if n.tags:
                header += f" [tags: {', '.join(n.tags)}]"
            lines.append(header)
            lines.append(n.content)
            lines.append("")
        return ToolResult(output="\n".join(lines))


# ---------------------------------------------------------------------------
# ReadTaskDetailsTool — full detail view for specific tasks
# ---------------------------------------------------------------------------


class ReadTaskDetailsInput(BaseModel):
    task_ids: list[str] = Field(
        ...,
        min_length=1,
        description="Task IDs to look up. Get IDs from read_task_graph or from your deps.",
    )


class ReadTaskDetailsTool(BaseTool):
    name = "read_task_details"
    description = (
        "Get full details for specific tasks by ID: spec, deps, status, "
        "scope_paths, summary, and recent notes. Use read_task_graph first "
        "to discover task IDs."
    )
    short_description = "Read task details by ID."
    input_model = ReadTaskDetailsInput
    output_model = TextToolOutput

    async def execute(self, arguments: BaseModel, context: ToolExecutionContext) -> ToolResult:
        assert isinstance(arguments, ReadTaskDetailsInput)
        tc = context.metadata.get("task_center")
        if tc is None:
            return ToolResult(output="Error: Task Center not available", is_error=True)

        graph = getattr(tc, "graph", None)
        if not isinstance(graph, dict):
            return ToolResult(output="Error: task graph not available", is_error=True)

        sections: list[str] = []
        for tid in arguments.task_ids:
            task = graph.get(tid)
            if task is None:
                sections.append(f"## {tid}\nNot found in task graph.")
                continue

            header = f"## {task.id} ({task.agent_name}) [{task.status.value}]"
            lines = [header]

            # Title
            if task.description:
                lines.append(f"**Description:** {task.description}")

            # Spec
            lines.append(f"**Objective:** {task.objective}")

            # Deps
            if task.deps:
                lines.append(f"**Deps:** {', '.join(task.deps)}")

            # Scope
            if task.scope_paths:
                lines.append(f"**Scope:** {', '.join(task.scope_paths)}")

            # Failure reason
            if task.failure_reason:
                lines.append(f"**Failure:** {task.failure_reason}")

            # Notes for this task
            try:
                notes = await tc.notes.read_notes(last_n=5)
                task_notes = [n for n in notes if n.task_id == tid]
                if task_notes:
                    lines.append("**Notes:**")
                    for n in task_notes[-3:]:
                        tag_str = f" [{', '.join(n.tags)}]" if n.tags else ""
                        lines.append(f"  - {n.agent_name}{tag_str}: {n.content[:200]}")
            except Exception:
                pass  # notes unavailable

            sections.append("\n".join(lines))

        return ToolResult(output="\n\n".join(sections))


# ---------------------------------------------------------------------------
# ReadTaskGraphTool — DAG structure overview
# ---------------------------------------------------------------------------


class ReadTaskGraphInput(BaseModel):
    scope: Literal["parent", "global"] = Field(
        default="parent",
        description=(
            "'parent' shows tasks under the same parent (your peers). "
            "'global' shows the full task tree."
        ),
    )
    include_status: bool = Field(default=True, description="Include task status in output.")
    include_deps: bool = Field(default=True, description="Include dependency edges in output.")


class ReadTaskGraphTool(BaseTool):
    name = "read_task_graph"
    description = (
        "View the task DAG structure: IDs, agents, status, and dependency edges. "
        "Use scope='parent' to see your peer tasks, scope='global' for the full tree. "
        "Follow up with read_task_details(task_ids=[...]) for full info on specific tasks."
    )
    short_description = "Read the task graph."
    input_model = ReadTaskGraphInput
    output_model = TextToolOutput

    async def execute(self, arguments: BaseModel, context: ToolExecutionContext) -> ToolResult:
        assert isinstance(arguments, ReadTaskGraphInput)
        tc = context.metadata.get("task_center")
        if tc is None:
            return ToolResult(output="Error: Task Center not available", is_error=True)

        graph = getattr(tc, "graph", None)
        if not isinstance(graph, dict):
            return ToolResult(output="Error: task graph not available", is_error=True)

        task_id = str(context.metadata.get("work_item_id") or "")

        if arguments.scope == "parent":
            own_task = graph.get(task_id)
            if own_task is None:
                return ToolResult(output="Error: own task not found in graph", is_error=True)
            parent_id = own_task.parent_id
            tasks = [
                t for t in graph.values()
                if getattr(t, "parent_id", None) == parent_id
            ]
        else:
            tasks = list(graph.values())

        if not tasks:
            return ToolResult(output="No tasks found.")

        lines: list[str] = []
        for t in tasks:
            marker = " **(you)**" if t.id == task_id else ""
            title_str = f" \"{t.description}\"" if t.description else ""
            status_str = f" [{t.status.value}]" if arguments.include_status else ""
            dep_str = f" deps=[{', '.join(t.deps)}]" if arguments.include_deps and t.deps else ""
            scope_str = f" scope=[{', '.join(t.scope_paths[:2])}]" if t.scope_paths else ""
            failure = f" FAIL: {t.failure_reason[:80]}" if t.failure_reason else ""
            lines.append(
                f"- **{t.id}** {t.agent_name}{status_str}{title_str}{marker}"
                f"{dep_str}{scope_str}{failure}"
            )

        return ToolResult(output="\n".join(lines))


# ---------------------------------------------------------------------------
# Toolkit
# ---------------------------------------------------------------------------

_ALL_TOOLS = [
    SubmitTaskNoteTool(),
    ReadTaskNoteTool(),
    ReadTaskDetailsTool(),
    ReadTaskGraphTool(),
    TaskCenterChangedSinceTool(),
]


class TaskCenterToolkit(BaseToolkit):
    """Task Center tools: notes, task graph, task details, and freshness checks.

    All tools are registered; role-based restrictions (e.g. blocking
    ``submit_task_note`` for planners) are handled via ``blocked_tools`` in
    agent definitions.
    """

    @classmethod
    def from_context(cls, ctx: object) -> TaskCenterToolkit:
        return cls(
            name="task_center",
            description="Task Center tools: notes, task graph, details, and freshness checks.",
            tools=list(_ALL_TOOLS),
        )
