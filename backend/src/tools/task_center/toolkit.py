"""Task Center tools — notes + staleness.

Tools exposed in the main loop:
- submit_file_note              — post a file-scoped note (scouts, file-surface notes)
- submit_task_note              — post a task-scoped note (note_taker, task updates)
- read_task_details             — task spec + recent notes by task id / scope
- read_file_note                — search notes by file path
- task_center_changed_since     — check if task-center state is stale

Role-based restrictions are handled via ``blocked_tools`` in agent definitions
rather than separate read/write toolkit variants.
"""

from __future__ import annotations

import json
import re
import time
import uuid

from pydantic import BaseModel, ConfigDict, Field, field_validator

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
# SubmitFileNoteTool / SubmitTaskNoteTool
# ---------------------------------------------------------------------------


def _non_blank_content(value: str) -> str:
    if not value.strip():
        raise ValueError("content must contain non-whitespace text")
    return value


class SubmitFileNoteInput(BaseModel):
    content: str = Field(
        ...,
        description=(
            "REQUIRED. The note body as a non-empty, non-whitespace string. "
            "Put the entire note here rather than in assistant text."
        ),
        min_length=1,
    )
    paths: list[str] = Field(
        ...,
        min_length=1,
        description=(
            "REQUIRED. File/dir paths this note relates to. Can be existing or "
            "planned paths. Other agents find the note via read_file_note."
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

    @field_validator("content")
    @classmethod
    def _content_must_not_be_blank(cls, value: str) -> str:
        return _non_blank_content(value)


class SubmitTaskNoteInput(BaseModel):
    content: str = Field(
        ...,
        description=(
            "REQUIRED. The note body as a non-empty, non-whitespace string. "
            "Put the entire note here rather than in assistant text."
        ),
        min_length=1,
    )
    task_id: str = Field(
        ...,
        min_length=1,
        description=(
            "REQUIRED. ID of the task this note is about. "
            "Get IDs from read_task_graph or your deps."
        ),
    )
    paths: list[str] = Field(
        ...,
        min_length=1,
        description=(
            "REQUIRED. File/dir paths this note relates to. "
            "Other agents find the note via read_file_note."
        ),
    )
    tags: list[str] | None = Field(
        default=None,
        description=(
            "Classify the note with one or more tags: discovery, implementation, "
            "bug_fix, blocker, proposal, verification, architecture, dependency, "
            "warning, refactor."
        ),
    )
    parent_note_id: str | None = Field(
        default=None,
        description="ID of a prior note this is a follow-up to (threading).",
    )

    @field_validator("content")
    @classmethod
    def _content_must_not_be_blank(cls, value: str) -> str:
        return _non_blank_content(value)


class TaskNoteOutput(BaseModel):
    note_id: str = Field(..., description="Created Task Center note id.")
    task_id: str = Field(..., description="Task id attached to the note (empty for file notes).")
    agent_name: str = Field(..., description="Runtime-stamped agent name that posted the note.")
    content: str = Field(..., description="Stored note content.")
    timestamp: float = Field(..., description="Unix timestamp when the note was posted.")
    paths: list[str] = Field(default_factory=list, description="Scope paths attached to the note.")
    tags: list[str] = Field(default_factory=list, description="Tags attached to the note.")
    parent_note_id: str | None = Field(
        default=None,
        description="Parent note id when the note is part of a thread.",
    )


async def _post_note(
    *,
    content: str,
    paths: list[str],
    tags: list[str] | None,
    parent_note_id: str | None,
    task_id: str,
    context: ToolExecutionContext,
) -> ToolResult:
    from team.models import Note, NoteTag

    tc = context.metadata.get("task_center")
    if tc is None:
        return ToolResult(output="Error: Task Center not available", is_error=True)

    if tags:
        valid_tags = {t.value for t in NoteTag}
        invalid = [t for t in tags if t not in valid_tags]
        if invalid:
            return ToolResult(
                output=f"Invalid tag(s): {invalid}. Valid tags: {sorted(valid_tags)}",
                is_error=True,
            )

    note_paths = normalize_scope_paths(paths)
    if str(context.metadata.get("agent_name") or "").strip() == "scout" and note_paths:
        if "intended path" not in content.lower() and "correct path" not in content.lower():
            content = _sanitize_scout_gap_paths(content, note_paths)

    note = Note(
        id=str(uuid.uuid4()),
        task_id=task_id,
        agent_name=context.metadata.get("agent_name", ""),
        content=content,
        timestamp=time.time(),
        paths=note_paths,
        tags=list(tags or []),
        parent_note_id=parent_note_id,
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


class SubmitFileNoteTool(BaseTool):
    name = "submit_file_note"
    description = (
        "Post a file-scoped note to the Task Center. Use for scout discoveries "
        "and any note about file surfaces that is not tied to a specific task. "
        "Requires non-empty `content` and at least one file/dir path in `paths`. "
        "The note is stored without a task_id so it surfaces on file-based "
        "lookups via read_file_note. Notes are append-only and immutable."
    )
    short_description = "Post a file-scoped note."
    input_model = SubmitFileNoteInput
    output_model = TaskNoteOutput

    async def execute(self, arguments: BaseModel, context: ToolExecutionContext) -> ToolResult:
        assert isinstance(arguments, SubmitFileNoteInput)
        return await _post_note(
            content=arguments.content,
            paths=arguments.paths,
            tags=arguments.tags,
            parent_note_id=arguments.parent_note_id,
            task_id="",
            context=context,
        )


class SubmitTaskNoteTool(BaseTool):
    name = "submit_task_note"
    description = (
        "Post a task-scoped note to the Task Center. Use for note_taker lanes "
        "and any update tied to a specific task. Requires non-empty `content`, "
        "a `task_id` (get it from read_task_graph or your deps), and at least "
        "one file/dir path in `paths`. Notes are append-only and immutable."
    )
    short_description = "Post a task-scoped note."
    input_model = SubmitTaskNoteInput
    output_model = TaskNoteOutput

    async def execute(self, arguments: BaseModel, context: ToolExecutionContext) -> ToolResult:
        assert isinstance(arguments, SubmitTaskNoteInput)
        return await _post_note(
            content=arguments.content,
            paths=arguments.paths,
            tags=arguments.tags,
            parent_note_id=arguments.parent_note_id,
            task_id=arguments.task_id,
            context=context,
        )


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
# ReadFileNoteTool — path search across all notes
# ---------------------------------------------------------------------------


class ReadFileNoteInput(BaseModel):
    model_config = ConfigDict(extra="forbid")

    file_path: str = Field(
        ...,
        min_length=1,
        description=(
            "REQUIRED. Path to a file or directory in the sandbox. Returns "
            "notes whose attached paths overlap with this prefix. Put the "
            "actual path here; `task_note` is only the reason for the call and "
            "is not searched."
        ),
    )
    tags: list[str] | None = Field(
        default=None,
        description=(
            "Filter by tag (OR semantics). Valid tags: discovery, implementation, "
            "bug_fix, blocker, proposal, verification, architecture, dependency, "
            "warning, refactor."
        ),
    )
    last_n: int | None = Field(
        default=None, description="Return only the N most recent matching notes."
    )


class ReadFileNoteTool(BaseTool):
    name = "read_file_note"
    description = (
        "Search Task Center notes by file path. Developers and validators must "
        "call this before reading or editing files that may have notes. "
        "Entry/root planners should not use it during initial setup; read file "
        "notes after scouts post findings or when the prompt names a known note "
        "path. Pass file_path=\"<path>\"; never put the searched path only in "
        "task_note."
    )
    short_description = "Search notes by file path."
    input_model = ReadFileNoteInput
    output_model = TextToolOutput

    async def execute(self, arguments: BaseModel, context: ToolExecutionContext) -> ToolResult:
        assert isinstance(arguments, ReadFileNoteInput)
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

        paths = [arguments.file_path]

        matched = await tc.notes.read(paths=paths)
        if not matched:
            known = tc.notes.known_paths()
            return ToolResult(
                output=(
                    f"No notes found for file_path: {arguments.file_path}. "
                    f"Known note paths: {known}"
                ),
            )

        notes = await tc.notes.read(
            paths=paths,
            tags=arguments.tags,
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
# ReadTaskDetailsTool — full detail view for one task
# ---------------------------------------------------------------------------


class ReadTaskDetailsInput(BaseModel):
    model_config = ConfigDict(extra="forbid")

    task_id: str = Field(
        ...,
        min_length=1,
        description=(
            "Single task ID to look up. Use the exact id from the prompt header, "
            "a dependency id, or read_task_graph sibling discovery. Never pass "
            "display slugs or shortened ids."
        ),
    )


class ReadTaskDetailsTool(BaseTool):
    name = "read_task_details"
    description = (
        "Read full details for one known task id: spec, deps, status, "
        "scope_paths, failure reason, completion summary, and recent notes. "
        "Non-root developers, validators, child planners, and replanners use "
        "this for the ids exposed in their prompt headers and dependencies. "
        "Do not use this for scout/subagent results: they are background work, "
        "not Task Center tasks, and their handoff is retrieved with "
        "read_file_note. Child planners and replanners may use read_task_graph "
        "first to enumerate siblings."
    )
    short_description = "Read one task's details + recent notes by ID."
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

        tid = arguments.task_id
        task = graph.get(tid)
        if task is None:
            return ToolResult(output=f"## {tid}\nNot found in task graph.")

        header = f"## {task.id} ({task.agent_name}) [{task.status.value}]"
        lines = [header]

        if task.description:
            lines.append(f"**Description:** {task.description}")
        lines.append(f"**Objective:** {task.objective}")
        if task.deps:
            lines.append(f"**Deps:** {', '.join(task.deps)}")
        if task.scope_paths:
            lines.append(f"**Scope:** {', '.join(task.scope_paths)}")
        if task.failure_reason:
            lines.append(f"**Failure:** {task.failure_reason}")

        # Notes for this task — full content, last 3, plus the latest
        # completion summary if present (posted as an `implementation` note
        # by the runtime when a task reports success).
        try:
            task_notes = await tc.notes.read(authors=[tid])
            if task_notes:
                summary_note = next(
                    (
                        n
                        for n in reversed(task_notes)
                        if "implementation" in (n.tags or [])
                    ),
                    None,
                )
                initial_plan_note = next(
                    (
                        n
                        for n in reversed(task_notes)
                        if "initial_planned_tasks" in (n.tags or [])
                    ),
                    None,
                )
                if initial_plan_note is not None:
                    lines.append("**Initial Plan:**")
                    lines.append("```json")
                    lines.append(initial_plan_note.content)
                    lines.append("```")

                initial_replan_note = next(
                    (
                        n
                        for n in reversed(task_notes)
                        if "initial_replanned_tasks" in (n.tags or [])
                    ),
                    None,
                )
                if initial_replan_note is not None:
                    lines.append("**Initial Replan:**")
                    lines.append("```json")
                    lines.append(initial_replan_note.content)
                    lines.append("```")

                if summary_note is not None:
                    lines.append("**Summary:**")
                    lines.append(summary_note.content)

                structured_plan_tags = {
                    "initial_planned_tasks",
                    "initial_replanned_tasks",
                }
                recent_candidates = [
                    n
                    for n in task_notes
                    if not structured_plan_tags.intersection(n.tags or [])
                ]
                recent = recent_candidates[-3:]
                if recent:
                    lines.append("**Recent notes:**")
                    for n in recent:
                        tag_str = f" [{', '.join(n.tags)}]" if n.tags else ""
                        path_str = (
                            f" [paths: {', '.join(n.paths)}]" if n.paths else ""
                        )
                        lines.append(f"### {n.agent_name}{tag_str}{path_str}")
                        lines.append(n.content)
        except Exception:
            pass  # notes unavailable

        return ToolResult(output="\n".join(lines))


# ---------------------------------------------------------------------------
# ReadTaskGraphTool — DAG structure overview
# ---------------------------------------------------------------------------


class ReadTaskGraphInput(BaseModel):
    global_scope: bool = Field(
        default=False,
        description=(
            "If true, return the full task tree. If false (default), return peer "
            "tasks under the same parent (your siblings) with their children "
            "nested recursively."
        ),
    )


class ReadTaskGraphTool(BaseTool):
    name = "read_task_graph"
    description = (
        "View the task DAG as a JSON tree for sibling/dependent enumeration. "
        "Use this for child planners and replanners that need same-parent peer "
        "context. Entry/root planners have no parent, deps, or siblings and "
        "should not call this as initial setup. Nodes include id, agent, status, "
        "description, deps, scope_paths, failure_reason, is_you, and children. "
        "Default returns peers under your parent; set global_scope=true only "
        "when local peer context is insufficient."
    )
    short_description = "Read the task graph as JSON."
    input_model = ReadTaskGraphInput
    output_model = TextToolOutput

    @staticmethod
    def _node(t: object, self_id: str, children: list[dict]) -> dict:
        return {
            "id": t.id,
            "agent": t.agent_name,
            "status": t.status.value,
            "description": t.description or "",
            "deps": list(t.deps),
            "scope_paths": list(t.scope_paths),
            "failure_reason": t.failure_reason,
            "is_you": t.id == self_id,
            "children": children,
        }

    async def execute(self, arguments: BaseModel, context: ToolExecutionContext) -> ToolResult:
        assert isinstance(arguments, ReadTaskGraphInput)
        tc = context.metadata.get("task_center")
        if tc is None:
            return ToolResult(output="Error: Task Center not available", is_error=True)

        graph = getattr(tc, "graph", None)
        if not isinstance(graph, dict):
            return ToolResult(output="Error: task graph not available", is_error=True)

        self_id = str(context.metadata.get("work_item_id") or "")

        # Build child adjacency over the full graph.
        children_by_parent: dict[str | None, list[object]] = {}
        for t in graph.values():
            children_by_parent.setdefault(getattr(t, "parent_id", None), []).append(t)

        def build_subtree(task: object) -> dict:
            kids = [build_subtree(c) for c in children_by_parent.get(task.id, [])]
            return self._node(task, self_id, kids)

        if arguments.global_scope:
            included_ids = set(graph.keys())
            roots = children_by_parent.get(None, [])
            tasks_json = [build_subtree(r) for r in roots]
            detached = [
                build_subtree(t)
                for t in graph.values()
                if getattr(t, "parent_id", None) is not None
                and t.parent_id not in included_ids
            ]
            payload = {"tasks": tasks_json, "detached": detached}
        else:
            own_task = graph.get(self_id)
            if own_task is None:
                return ToolResult(output="Error: own task not found in graph", is_error=True)
            parent_id = own_task.parent_id
            parent_task = graph.get(parent_id) if parent_id else None
            parent_json = (
                {
                    "id": parent_task.id,
                    "agent": parent_task.agent_name,
                    "status": parent_task.status.value,
                    "description": parent_task.description or "",
                }
                if parent_task is not None
                else None
            )
            peers = children_by_parent.get(parent_id, [])
            tasks_json = [build_subtree(p) for p in peers]
            payload = {"parent": parent_json, "tasks": tasks_json}

        return ToolResult(output=json.dumps(payload, indent=2))


# ---------------------------------------------------------------------------
# Toolkit
# ---------------------------------------------------------------------------

_ALL_TOOLS = [
    SubmitFileNoteTool(),
    SubmitTaskNoteTool(),
    ReadFileNoteTool(),
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
