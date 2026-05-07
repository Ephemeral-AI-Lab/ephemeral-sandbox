"""Per-response tool trace bookkeeping used by the query loop."""

from __future__ import annotations

from tools import ExecutionMetadata

_TOOL_TRACE_LIMIT = 64


def _normalize_trace_paths(value: object) -> list[str]:
    if isinstance(value, str):
        stripped = value.strip()
        return [stripped] if stripped else []
    if isinstance(value, list):
        out: list[str] = []
        for item in value:
            if isinstance(item, str):
                stripped = item.strip()
                if stripped:
                    out.append(stripped)
        return out
    return []


def _append_trace_values(
    metadata: ExecutionMetadata | None,
    key: str,
    values: list[str],
) -> None:
    if metadata is None or not values:
        return
    existing = _normalize_trace_paths(metadata.get(key, []))
    seen = set(existing)
    for value in values:
        if value not in seen:
            existing.append(value)
            seen.add(value)
    if len(existing) > _TOOL_TRACE_LIMIT:
        existing = existing[-_TOOL_TRACE_LIMIT:]
    metadata[key] = existing


def _increment_trace_counter(metadata: ExecutionMetadata | None, key: str) -> None:
    if metadata is None:
        return
    current = metadata.get(key, 0)
    metadata[key] = int(current) + 1 if isinstance(current, (int, float)) else 1


def record_tool_trace(
    metadata: ExecutionMetadata | None,
    tool_name: str,
    tool_input: dict[str, object],
    *,
    tool_use_id: str | None = None,
) -> None:
    if metadata is None:
        return
    if tool_name == "read_file_note":
        _increment_trace_counter(metadata, "_read_file_note_calls")
        _append_trace_values(
            metadata,
            "_note_read_paths_this_response",
            _normalize_trace_paths(tool_input.get("file_paths")),
        )
        return
    if tool_name == "shell":
        _increment_trace_counter(metadata, "_shell_calls")
        return
    if tool_name == "read_file":
        _increment_trace_counter(metadata, "_read_file_calls")
        _append_trace_values(
            metadata,
            "_read_paths_this_response",
            _normalize_trace_paths(tool_input.get("file_path")),
        )
        return
    return
