"""Translate engine stream events into shared audit events."""

from __future__ import annotations

import hashlib
import json
from collections.abc import Mapping
from typing import Any

from audit.base import AuditEvent, AuditNode
from engine.audit import events
from message.events import ToolExecutionCompletedEvent, ToolExecutionStartedEvent


def audit_events_from_stream_event(
    stream_event: object,
    *,
    metadata: Mapping[str, Any] | None = None,
    task_center_run_id: str | None = None,
) -> tuple[AuditEvent, ...]:
    """Translate supported StreamEvent objects into engine-owned audit events."""
    if isinstance(stream_event, ToolExecutionStartedEvent):
        return (
            AuditEvent(
                source="engine",
                type=events.TOOL_STARTED,
                node=_node_from_stream(
                    stream_event,
                    metadata=metadata,
                    task_center_run_id=task_center_run_id,
                ),
                payload={
                    "tool_name": stream_event.tool_name,
                    "tool_use_id": stream_event.tool_use_id,
                    "status": "ok",
                    "input_shape": _shape(stream_event.tool_input),
                    "input_redacted": _redacted_shape(stream_event.tool_input),
                    "input_digest": _digest(stream_event.tool_input),
                    "input_bytes": _encoded_size(stream_event.tool_input),
                },
            ),
        )
    if isinstance(stream_event, ToolExecutionCompletedEvent):
        status = "error" if stream_event.is_error else "ok"
        return (
            AuditEvent(
                source="engine",
                type=events.TOOL_FAILED if stream_event.is_error else events.TOOL_COMPLETED,
                node=_node_from_stream(
                    stream_event,
                    metadata=metadata,
                    task_center_run_id=task_center_run_id,
                ),
                payload={
                    "tool_name": stream_event.tool_name,
                    "tool_use_id": stream_event.tool_use_id,
                    "status": status,
                    "error_kind": "tool_result_error" if stream_event.is_error else None,
                    "output_shape": _shape(stream_event.output),
                    "output_digest": _digest(stream_event.output),
                    "output_bytes": _encoded_size(stream_event.output),
                    "is_error": stream_event.is_error,
                    "is_terminal": stream_event.is_terminal,
                    "metadata": _audit_metadata_from_stream_metadata(
                        stream_event.metadata
                    ),
                    "timings": {},
                },
            ),
        )
    return ()


def _node_from_stream(
    stream_event: ToolExecutionStartedEvent | ToolExecutionCompletedEvent,
    *,
    metadata: Mapping[str, Any] | None,
    task_center_run_id: str | None,
) -> AuditNode:
    return AuditNode(
        task_center_run_id=_first_text(
            _metadata_get(metadata, "task_center_run_id"),
            task_center_run_id,
        ),
        request_id=_text_or_none(_metadata_get(metadata, "task_center_request_id")),
        workflow_id=_text_or_none(_metadata_get(metadata, "task_center_workflow_id")),
        attempt_id=_text_or_none(_metadata_get(metadata, "task_center_attempt_id")),
        task_center_task_id=_text_or_none(
            _metadata_get(metadata, "task_center_task_id")
        ),
        agent_name=_first_text(stream_event.agent_name, _metadata_get(metadata, "agent_name")),
        agent_run_id=_first_text(stream_event.run_id, _metadata_get(metadata, "agent_run_id")),
        sandbox_id=_text_or_none(_metadata_get(metadata, "sandbox_id")),
        tool_name=_text_or_none(stream_event.tool_name),
        tool_use_id=_first_text(stream_event.tool_use_id, _metadata_get(metadata, "tool_use_id")),
    )


def _audit_metadata_from_stream_metadata(
    metadata: Mapping[str, Any],
) -> dict[str, Any]:
    audit_metadata = dict(metadata)
    timings = audit_metadata.pop("timings", None)
    if isinstance(timings, dict):
        audit_metadata["domain_timings"] = dict(timings)
    return audit_metadata


def _metadata_get(metadata: Mapping[str, Any] | None, key: str) -> Any:
    if metadata is None:
        return None
    return metadata.get(key)


def _shape(value: Any) -> Any:
    if isinstance(value, dict):
        return {str(key): _shape(item) for key, item in value.items()}
    if isinstance(value, (list, tuple)):
        return [_shape(item) for item in value[:5]]
    return type(value).__name__


def _redacted_shape(value: Any) -> Any:
    if isinstance(value, dict):
        return {str(key): "<redacted>" for key in value}
    if isinstance(value, (list, tuple)):
        return ["<redacted>" for _ in value[:5]]
    return "<redacted>"


def _digest(value: Any) -> str:
    return f"sha256:{hashlib.sha256(_json_bytes(value)).hexdigest()}"


def _encoded_size(value: Any) -> int:
    return len(_json_bytes(value))


def _json_bytes(value: Any) -> bytes:
    return json.dumps(
        value,
        default=str,
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
    ).encode("utf-8")


def _first_text(*values: Any) -> str | None:
    for value in values:
        text = _text_or_none(value)
        if text is not None:
            return text
    return None


def _text_or_none(value: Any) -> str | None:
    if value is None:
        return None
    text = str(value).strip()
    return text or None


__all__ = ["audit_events_from_stream_event"]
