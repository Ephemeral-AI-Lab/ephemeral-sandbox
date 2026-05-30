"""Provider-facing conversation history preparation."""

from __future__ import annotations

import copy
from typing import Any

from engine.background.history import reduce_background_task_history
from message import (
    ContentBlock,
    Message,
    SystemNotificationBlock,
    TextBlock,
    ThinkingBlock,
    ToolResultBlock,
    ToolUseBlock,
)


def build_provider_messages(
    messages: list[Message],
) -> list[Message]:
    """Return a provider-safe copy of the durable message history.

    The query loop keeps ``messages`` as the append-only transcript.
    Providers receive a separate deep-copied view so stale background task
    snapshots and malformed historical tool pairs cannot leak into the next
    request. This function never mutates ``messages``.
    """
    return sanitize_tool_sequence(reduce_background_task_history(messages))


def sanitize_tool_sequence(
    messages: list[Message],
) -> list[Message]:
    """Drop malformed stale tool-use/result blocks from the provider view."""
    sanitized = [_provider_message_copy(message) for message in messages]
    _drop_unmatched_tool_blocks_in_place(sanitized)
    return [msg for msg in sanitized if msg.content]


def _provider_message_copy(message: Message) -> Message:
    return Message(
        role=message.role,
        content=[_provider_block_copy(block) for block in message.content],
    )


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


def _message_tool_use_ids(message: Message) -> set[str]:
    return {block.tool_use_id for block in message.content if isinstance(block, ToolUseBlock)}


def _message_tool_result_ids(message: Message) -> set[str]:
    return {
        block.tool_use_id
        for block in message.content
        if isinstance(block, ToolResultBlock)
    }


def _drop_unmatched_tool_blocks_in_place(messages: list[Message]) -> None:
    pending_tool_use_ids: set[str] = set()
    pending_message_index: int | None = None

    def _drop_tool_uses_from_message(
        message_index: int | None,
        tool_use_ids: set[str],
    ) -> None:
        if message_index is None or not tool_use_ids:
            return
        message = messages[message_index]
        message.content = [
            block
            for block in message.content
            if not (isinstance(block, ToolUseBlock) and block.tool_use_id in tool_use_ids)
        ]

    for message_index, message in enumerate(messages):
        tool_use_ids = _message_tool_use_ids(message)
        tool_result_ids = _message_tool_result_ids(message)
        matched_pending_tool_uses = False

        if pending_tool_use_ids:
            if message.role != "user" or not pending_tool_use_ids.issubset(
                tool_result_ids
            ):
                _drop_tool_uses_from_message(
                    pending_message_index,
                    pending_tool_use_ids,
                )
                pending_tool_use_ids = set()
                pending_message_index = None
                tool_result_ids = _message_tool_result_ids(message)
            else:
                unmatched_result_ids = tool_result_ids - pending_tool_use_ids
                if unmatched_result_ids:
                    message.content = [
                        block
                        for block in message.content
                        if not (
                            isinstance(block, ToolResultBlock)
                            and block.tool_use_id in unmatched_result_ids
                        )
                    ]
                pending_tool_use_ids = set()
                pending_message_index = None
                tool_result_ids = _message_tool_result_ids(message)
                matched_pending_tool_uses = True

        if tool_result_ids and not tool_use_ids and not matched_pending_tool_uses:
            message.content = [
                block
                for block in message.content
                if not isinstance(block, ToolResultBlock)
            ]

        tool_use_ids = _message_tool_use_ids(message)
        if tool_use_ids:
            pending_tool_use_ids = set(tool_use_ids)
            pending_message_index = message_index

    if pending_tool_use_ids:
        _drop_tool_uses_from_message(pending_message_index, pending_tool_use_ids)


__all__ = [
    "build_provider_messages",
    "sanitize_tool_sequence",
]
