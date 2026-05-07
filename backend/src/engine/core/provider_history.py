"""Provider-facing conversation history preparation."""

from __future__ import annotations

import copy
from typing import Any

from message import (
    BackgroundTaskStateBlock,
    ContentBlock,
    ConversationMessage,
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
    {"completed", "failed", "cancelled", "delivered"}
)
_REDUCIBLE_STATUSES: frozenset[str] = (
    _REDUCIBLE_RUNNING_STATUSES | _REDUCIBLE_TERMINAL_STATUSES
)


def prepare_provider_messages(
    messages: list[ConversationMessage],
) -> list[ConversationMessage]:
    """Return a provider-safe copy of the durable message history.

    The query loop keeps ``messages`` as the append-only transcript.
    Providers receive a separate deep-copied view so stale background task
    snapshots and malformed historical tool pairs cannot leak into the next
    request. This function never mutates ``messages``.
    """
    return sanitize_tool_sequence(reduce_background_task_history(messages))


def reduce_background_task_history(
    messages: list[ConversationMessage],
) -> list[ConversationMessage]:
    """Keep only the latest provider-visible state for each background task."""
    tool_use_map: dict[str, tuple[int, int, str]] = {}
    snapshot_tool_use_ids: set[str] = set()
    _WinnerKey = tuple[bool, int, int, int, tuple[int, int] | None, str | None, int | None]
    winners: dict[str, _WinnerKey] = {}

    for msg_idx, msg in enumerate(messages):
        if msg.role != "assistant":
            continue
        for block_idx, block in enumerate(msg.content):
            if isinstance(block, ToolUseBlock):
                tool_use_map[block.id] = (msg_idx, block_idx, block.name)

    for msg_idx, msg in enumerate(messages):
        for block_idx, block in enumerate(msg.content):
            if isinstance(block, BackgroundTaskStateBlock) and block.status in _REDUCIBLE_STATUSES:
                is_terminal = block.status in _REDUCIBLE_TERMINAL_STATUSES
                key = (
                    is_terminal,
                    msg_idx,
                    block_idx,
                    -1,
                    (msg_idx, block_idx),
                    None,
                    None,
                )
                current = winners.get(block.task_id)
                if current is None or key[:4] > current[:4]:
                    winners[block.task_id] = key
                continue

            if not isinstance(block, ToolResultBlock):
                continue
            snapshot = _background_snapshot_info(block, tool_use_map)
            if snapshot is None:
                continue
            snapshot_tool_use_ids.add(block.tool_use_id)
            for status_idx, entry in enumerate(snapshot["statuses"]):
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
                    None,
                    block.tool_use_id,
                    status_idx,
                )
                current = winners.get(task_id)
                if current is None or key[:4] > current[:4]:
                    winners[task_id] = key

    keep_state_blocks: set[tuple[int, int]] = set()
    keep_snapshot_statuses: dict[str, set[int]] = {}
    for winner in winners.values():
        if winner[4] is not None:
            keep_state_blocks.add(winner[4])
        if winner[5] is not None and winner[6] is not None:
            keep_snapshot_statuses.setdefault(winner[5], set()).add(winner[6])

    drop_tool_use_ids = snapshot_tool_use_ids - keep_snapshot_statuses.keys()

    reduced: list[ConversationMessage] = []
    for msg_idx, msg in enumerate(messages):
        new_content: list[ContentBlock] = []
        for block_idx, block in enumerate(msg.content):
            if isinstance(block, BackgroundTaskStateBlock):
                if (msg_idx, block_idx) in keep_state_blocks:
                    new_content.append(block.model_copy(deep=True))
                continue

            if isinstance(block, ToolUseBlock) and block.id in drop_tool_use_ids:
                continue

            if isinstance(block, ToolResultBlock):
                snapshot = _background_snapshot_info(block, tool_use_map)
                if snapshot is None:
                    new_content.append(block.model_copy(deep=True))
                    continue
                keep_indexes = keep_snapshot_statuses.get(block.tool_use_id)
                if not keep_indexes:
                    continue
                filtered = [
                    copy.deepcopy(status)
                    for idx, status in enumerate(snapshot["statuses"])
                    if idx in keep_indexes
                ]
                rebuilt = block.model_copy(deep=True)
                rebuilt.content = render_background_snapshot(
                    snapshot["kind"],
                    filtered,
                    elapsed_seconds=snapshot["elapsed_seconds"],
                )
                rebuilt.metadata = build_background_snapshot_metadata(
                    snapshot["kind"],
                    snapshot["scope"],
                    filtered,
                    elapsed_seconds=snapshot["elapsed_seconds"],
                )
                new_content.append(rebuilt)
                continue

            new_content.append(block.model_copy(deep=True))

        if new_content:
            reduced.append(ConversationMessage(role=msg.role, content=new_content))
    return reduced


def sanitize_tool_sequence(
    messages: list[ConversationMessage],
) -> list[ConversationMessage]:
    """Drop malformed stale tool-use/result blocks from the provider view."""
    sanitized = copy.deepcopy(messages)
    _walk_tool_sequence(sanitized)
    return [msg for msg in sanitized if msg.content]


def _background_snapshot_info(
    block: ToolResultBlock,
    tool_use_map: dict[str, tuple[int, int, str]],
) -> dict[str, Any] | None:
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
    if not isinstance(statuses, list) or not isinstance(kind, str) or not isinstance(scope, str):
        return None
    elapsed = snapshot.get("elapsed_seconds")
    if not isinstance(elapsed, (int, float)):
        elapsed = None
    return {
        "kind": kind,
        "scope": scope,
        "statuses": statuses,
        "elapsed_seconds": elapsed,
    }


def _message_tool_use_ids(message: ConversationMessage) -> set[str]:
    return {block.id for block in message.content if isinstance(block, ToolUseBlock)}


def _message_tool_result_ids(message: ConversationMessage) -> set[str]:
    return {
        block.tool_use_id
        for block in message.content
        if isinstance(block, ToolResultBlock)
    }


def _walk_tool_sequence(messages: list[ConversationMessage]) -> None:
    pending_ids: set[str] = set()
    pending_msg_idx: int | None = None

    def _strip_tool_uses(idx: int | None, ids: set[str]) -> None:
        if idx is None or not ids:
            return
        message = messages[idx]
        message.content = [
            block
            for block in message.content
            if not (isinstance(block, ToolUseBlock) and block.id in ids)
        ]

    for msg_idx, message in enumerate(messages):
        tool_use_ids = _message_tool_use_ids(message)
        tool_result_ids = _message_tool_result_ids(message)
        satisfied_pending = False

        if pending_ids:
            if message.role != "user" or not pending_ids.issubset(tool_result_ids):
                _strip_tool_uses(pending_msg_idx, pending_ids)
                pending_ids = set()
                pending_msg_idx = None
                tool_result_ids = _message_tool_result_ids(message)
            else:
                extra = tool_result_ids - pending_ids
                if extra:
                    message.content = [
                        block
                        for block in message.content
                        if not (
                            isinstance(block, ToolResultBlock)
                            and block.tool_use_id in extra
                        )
                    ]
                pending_ids = set()
                pending_msg_idx = None
                tool_result_ids = _message_tool_result_ids(message)
                satisfied_pending = True

        if tool_result_ids and not tool_use_ids and not satisfied_pending:
            message.content = [
                block
                for block in message.content
                if not isinstance(block, ToolResultBlock)
            ]

        tool_use_ids = _message_tool_use_ids(message)
        if tool_use_ids:
            pending_ids = set(tool_use_ids)
            pending_msg_idx = msg_idx

    if pending_ids:
        _strip_tool_uses(pending_msg_idx, pending_ids)


__all__ = [
    "prepare_provider_messages",
    "reduce_background_task_history",
    "sanitize_tool_sequence",
]
