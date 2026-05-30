"""Provider-history compaction for background task snapshots."""

from __future__ import annotations

import copy
from dataclasses import dataclass
from typing import Any

from message import (
    ContentBlock,
    Message,
    SystemNotificationBlock,
    TextBlock,
    ThinkingBlock,
    ToolResultBlock,
    ToolUseBlock,
)
from tools import (
    build_background_snapshot_metadata,
    render_background_snapshot,
)

_BACKGROUND_SNAPSHOT_TOOLS: frozenset[str] = frozenset({"wait_background_tasks"})
_REDUCIBLE_RUNNING_STATUSES: frozenset[str] = frozenset({"running"})
_REDUCIBLE_TERMINAL_STATUSES: frozenset[str] = frozenset(
    {"completed", "failed", "cancelled", "delivered", "finished"}
)
_REDUCIBLE_STATUSES: frozenset[str] = (
    _REDUCIBLE_RUNNING_STATUSES | _REDUCIBLE_TERMINAL_STATUSES
)


@dataclass(frozen=True)
class _BackgroundSnapshot:
    kind: str
    scope: str
    statuses: list[dict[str, Any]]
    elapsed_seconds: int | float | None


def reduce_background_task_history(
    messages: list[Message],
) -> list[Message]:
    """Keep only the latest provider-visible state for each background task."""
    tool_use_map: dict[str, tuple[int, int, str]] = {}
    snapshot_tool_use_ids: set[str] = set()
    _WinnerKey = tuple[bool, int, int, int, str, int]
    winners: dict[str, _WinnerKey] = {}

    for msg_idx, msg in enumerate(messages):
        if msg.role != "assistant":
            continue
        for block_idx, block in enumerate(msg.content):
            if isinstance(block, ToolUseBlock):
                tool_use_map[block.tool_use_id] = (msg_idx, block_idx, block.name)

    for msg_idx, msg in enumerate(messages):
        for block_idx, block in enumerate(msg.content):
            if not isinstance(block, ToolResultBlock):
                continue
            snapshot = _background_snapshot_info(block, tool_use_map)
            if snapshot is None:
                continue
            snapshot_tool_use_ids.add(block.tool_use_id)
            for status_idx, entry in enumerate(snapshot.statuses):
                task_id = entry.get("task_id")
                status = entry.get("status")
                if not isinstance(task_id, str) or status not in _REDUCIBLE_STATUSES:
                    continue
                is_terminal = status in _REDUCIBLE_TERMINAL_STATUSES
                key = (
                    is_terminal,
                    msg_idx,
                    block_idx,
                    status_idx,
                    block.tool_use_id,
                    status_idx,
                )
                current = winners.get(task_id)
                if current is None or key[:4] > current[:4]:
                    winners[task_id] = key

    keep_snapshot_statuses: dict[str, set[int]] = {}
    for winner in winners.values():
        keep_snapshot_statuses.setdefault(winner[4], set()).add(winner[5])

    drop_tool_use_ids = snapshot_tool_use_ids - keep_snapshot_statuses.keys()

    reduced: list[Message] = []
    for msg in messages:
        new_content: list[ContentBlock] = []
        for block in msg.content:
            if isinstance(block, ToolUseBlock) and block.tool_use_id in drop_tool_use_ids:
                continue

            if isinstance(block, ToolResultBlock):
                snapshot = _background_snapshot_info(block, tool_use_map)
                if snapshot is None:
                    new_content.append(_provider_block_copy(block))
                    continue
                keep_indexes = keep_snapshot_statuses.get(block.tool_use_id)
                if not keep_indexes:
                    continue
                filtered = [
                    copy.deepcopy(status)
                    for idx, status in enumerate(snapshot.statuses)
                    if idx in keep_indexes
                ]
                rebuilt = ToolResultBlock(
                    tool_use_id=block.tool_use_id,
                    content=render_background_snapshot(
                        snapshot.kind,
                        filtered,
                        elapsed_seconds=snapshot.elapsed_seconds,
                    ),
                    is_error=block.is_error,
                    metadata=build_background_snapshot_metadata(
                        snapshot.kind,
                        snapshot.scope,
                        filtered,
                        elapsed_seconds=snapshot.elapsed_seconds,
                    ),
                    is_terminal=block.is_terminal,
                )
                new_content.append(rebuilt)
                continue

            new_content.append(_provider_block_copy(block))

        if new_content:
            reduced.append(Message(role=msg.role, content=new_content))
    return reduced


def _provider_block_copy(block: ContentBlock) -> ContentBlock:
    if isinstance(block, ToolResultBlock):
        return ToolResultBlock(
            tool_use_id=block.tool_use_id,
            content=block.content,
            is_error=block.is_error,
            metadata=_json_metadata_copy(block.metadata),
            is_terminal=block.is_terminal,
        )
    if isinstance(block, ToolUseBlock):
        return ToolUseBlock(
            tool_use_id=block.tool_use_id,
            name=block.name,
            input=copy.deepcopy(block.input),
        )
    if isinstance(block, TextBlock):
        return TextBlock(text=block.text)
    if isinstance(block, ThinkingBlock):
        return ThinkingBlock(text=block.text)
    if isinstance(block, SystemNotificationBlock):
        return SystemNotificationBlock(text=block.text)
    raise TypeError(f"unknown content block type: {type(block).__name__}")


def _json_metadata_copy(metadata: dict[str, Any]) -> dict[str, Any]:
    copied = _jsonish_copy(metadata)
    return copied if isinstance(copied, dict) else {}


def _jsonish_copy(value: Any) -> Any:
    if value is None or isinstance(value, (str, int, float, bool)):
        return value
    if isinstance(value, dict):
        result: dict[str, Any] = {}
        for key, item in value.items():
            copied = _jsonish_copy(item)
            if copied is not _DROP:
                result[str(key)] = copied
        return result
    if isinstance(value, (list, tuple)):
        result = []
        for item in value:
            copied = _jsonish_copy(item)
            if copied is not _DROP:
                result.append(copied)
        return result
    return _DROP


_DROP = object()


def _background_snapshot_info(
    block: ToolResultBlock,
    tool_use_map: dict[str, tuple[int, int, str]],
) -> _BackgroundSnapshot | None:
    if not block.metadata:
        return None
    snapshot = block.metadata.get("background_snapshot")
    if not isinstance(snapshot, dict):
        return None
    tool_use = tool_use_map.get(block.tool_use_id)
    if tool_use is None or tool_use[2] not in _BACKGROUND_SNAPSHOT_TOOLS:
        return None
    statuses = snapshot.get("statuses")
    kind = snapshot.get("kind")
    scope = snapshot.get("scope")
    if (
        not isinstance(statuses, list)
        or not all(isinstance(status, dict) for status in statuses)
        or not isinstance(kind, str)
        or not isinstance(scope, str)
    ):
        return None
    elapsed = snapshot.get("elapsed_seconds")
    if not isinstance(elapsed, (int, float)):
        elapsed = None
    return _BackgroundSnapshot(
        kind=kind,
        scope=scope,
        statuses=statuses,
        elapsed_seconds=elapsed,
    )


__all__ = ["reduce_background_task_history"]
