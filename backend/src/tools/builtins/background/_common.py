"""Shared helpers for background task tools."""

from __future__ import annotations

import copy
import json
from typing import Any

from pydantic import Field

TASK_ID_FIELD_DESCRIPTION = (
    "REQUIRED. Either the exact `task_id` string (e.g. \"bg_1\") shown "
    "in the `[BACKGROUND LAUNCHED]` message / `check_background_progress` "
    "output, OR the literal string \"all\" to target every pending "
    "background task. Never pass null/None and never omit this field."
)

TASK_ID_FIELD = Field(..., min_length=1, description=TASK_ID_FIELD_DESCRIPTION)


# Total char budget for `output` fields in a single tool response,
# summed across every status entry. ~1k tokens, chosen to keep batched
# `task_id="all"` responses within the agent's context budget.
MAX_TOTAL_OUTPUT_CHARS = 4000
# Floor on per-entry budget so a many-task response still leaves each
# entry with enough tail to be useful.
MIN_PER_ENTRY_CHARS = 200

POSTED_SUBAGENT_RESULT_GUIDANCE = (
    "For `run_subagent` results whose summary is `Posted.`, the useful content "
    "is in file notes or the referenced artifact, not in another "
    "background status snapshot. Do not call `wait_for_background_task` or "
    "`check_background_progress` again for this delivered subagent result. "
    "When file notes are referenced, read them with "
    "`read_file_note(file_paths=[...])`. Subagents are not Task Center tasks; "
    "do not use `read_task_graph()` or `read_task_details(...)` to retrieve "
    "subagent results, and never pass `bg_*` "
    "background ids as task ids."
)


def apply_last_n_lines(status: list[dict[str, Any]], last_n_lines: int) -> None:
    """Trim 'output' field in each status entry, in-place.

    Two-stage trim:
      1. Keep only the *last* ``last_n_lines`` lines per entry.
      2. Split ``MAX_TOTAL_OUTPUT_CHARS`` evenly across all entries that
         still have output, char-capping each entry to its share (with
         a floor of ``MIN_PER_ENTRY_CHARS``).

    The per-entry char-cap keeps the tail and prepends a
    ``... (head truncated)`` marker so the reader sees the marker before
    the kept content. After char-capping, any leading partial line is
    dropped so the first visible line is complete.

    Caller must own the list — this mutates entries in place.
    """
    # Stage 1: line trim.
    for entry in status:
        if "output" in entry and isinstance(entry["output"], str):
            lines = entry["output"].splitlines()
            if len(lines) > last_n_lines:
                entry["output"] = "\n".join(lines[-last_n_lines:])

    # Stage 2: total char budget, split per entry.
    entries_with_output = [
        e for e in status
        if "output" in e and isinstance(e["output"], str) and e["output"]
    ]
    if not entries_with_output:
        return
    per_entry_budget = max(
        MIN_PER_ENTRY_CHARS,
        MAX_TOTAL_OUTPUT_CHARS // len(entries_with_output),
    )
    for entry in entries_with_output:
        text = entry["output"]
        if len(text) <= per_entry_budget:
            continue
        tail = text[-per_entry_budget:]
        # Drop the leading partial line so the first visible line is whole.
        nl = tail.find("\n")
        if nl != -1:
            tail = tail[nl + 1:]
        entry["output"] = "... (head truncated)\n" + tail


def _output_summary_is_posted(output: Any) -> bool:
    if not isinstance(output, str):
        return False
    try:
        payload = json.loads(output)
    except json.JSONDecodeError:
        return '"summary": "Posted."' in output or '"summary":"Posted."' in output

    if not isinstance(payload, dict):
        return False
    if payload.get("summary") == "Posted.":
        return True
    nested_payload = payload.get("payload")
    return isinstance(nested_payload, dict) and nested_payload.get("final_text") == "Posted."


def _has_posted_subagent_result(statuses: list[dict[str, Any]]) -> bool:
    return any(
        (
            entry.get("tool_name") == "run_subagent"
            or entry.get("task_type") == "subagent"
        )
        and _output_summary_is_posted(entry.get("output"))
        for entry in statuses
    )


def _posted_subagent_guidance_suffix(statuses: list[dict[str, Any]]) -> str:
    if not _has_posted_subagent_result(statuses):
        return ""
    return f"\n{POSTED_SUBAGENT_RESULT_GUIDANCE}"


def render_background_snapshot(
    kind: str,
    statuses: list[dict[str, Any]],
    *,
    elapsed_seconds: float | None = None,
) -> str:
    """Render a background status snapshot exactly as the tools return it."""
    if kind == "progress":
        return json.dumps(statuses, indent=2)

    if kind == "wait_completed":
        return (
            f"[COMPLETED]\n{json.dumps(statuses, indent=2)}"
            f"{_posted_subagent_guidance_suffix(statuses)}"
        )

    if kind == "wait_timed_out":
        elapsed = elapsed_seconds or 0.0
        hint = (
            "Call wait_for_background_task again to continue waiting, "
            "or cancel_background_task to stop."
        )
        return (
            f"[TIMED_OUT after {elapsed:.1f}s]\n"
            f"{json.dumps(statuses, indent=2)}\n"
            f"{hint}"
        )

    if kind == "wait_no_tasks":
        if statuses:
            guidance = _posted_subagent_guidance_suffix(statuses)
            return (
                "[NO TASKS RUNNING] 0 background tasks are pending. "
                "All previously launched tasks have already finished; "
                "their results were (or will be) delivered as "
                "[BACKGROUND <task_id> COMPLETED] messages. Do not poll "
                "or wait on those task ids again; act on the delivered "
                "results instead."
                f"{guidance}\n"
                f"{json.dumps(statuses, indent=2)}"
            )
        return (
            "[NO TASKS RUNNING] 0 background tasks are pending and "
            "none have ever been launched in this session. Do not poll "
            "or wait unless you launch new background work."
        )

    raise ValueError(f"Unknown background snapshot kind: {kind}")


def build_background_snapshot_metadata(
    kind: str,
    scope: str,
    statuses: list[dict[str, Any]],
    *,
    elapsed_seconds: float | None = None,
) -> dict[str, Any]:
    """Build internal metadata used by API-view reduction."""
    snapshot: dict[str, Any] = {
        "kind": kind,
        "scope": scope,
        "statuses": copy.deepcopy(statuses),
    }
    if elapsed_seconds is not None:
        snapshot["elapsed_seconds"] = round(elapsed_seconds, 1)
    return {"background_snapshot": snapshot}
