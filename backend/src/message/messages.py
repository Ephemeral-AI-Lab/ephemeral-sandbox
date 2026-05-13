"""Conversation message models used by the query engine."""

from __future__ import annotations

from typing import Any, Annotated, Literal
from uuid import uuid4

from pydantic import BaseModel, Field


class TextBlock(BaseModel):
    """Plain text content."""

    type: Literal["text"] = "text"
    text: str


class ToolUseBlock(BaseModel):
    """A request from the model to execute a named tool."""

    type: Literal["tool_use"] = "tool_use"
    id: str = Field(default_factory=lambda: f"toolu_{uuid4().hex}")
    name: str
    input: dict[str, Any] = Field(default_factory=dict)


class ThinkingBlock(BaseModel):
    """Model reasoning / chain-of-thought content."""

    type: Literal["thinking"] = "thinking"
    text: str


class ToolResultBlock(BaseModel):
    """Tool result content sent back to the model."""

    type: Literal["tool_result"] = "tool_result"
    tool_use_id: str
    content: str
    is_error: bool = False
    metadata: dict[str, Any] = Field(default_factory=dict)
    # Engine-level marker stamped when a successful terminal tool returned.
    # Consumed by the query loop to exit with TOOL_STOP. Wire-irrelevant —
    # never serialized to the provider.
    does_terminate: bool = False


class SystemNotificationBlock(BaseModel):
    """Engine-generated reminder for the model wrapped in tags.

    This is a first-class content block — distinct from a user-authored
    TextBlock — so the engine and UI can treat it
    specially:

    - The display layer can render it differently (greyed-out, icon,
      collapsible) instead of mixing it with real user text.
    - Provider-history preparation can filter or dedupe stale notifications before
      they reach the model.
    - Audit / persistence can count notifications separately from real user
      messages.

    On the wire (Anthropic and other providers that only accept ``text``
    blocks mid-conversation), this block is serialized as a ``text`` block
    whose body is wrapped in ``<system-reminder>...</system-reminder>``
    tags. See :func:`serialize_content_block`. The role of the parent
    :class:`ConversationMessage` should be ``"user"`` because Anthropic's
    API does not accept arbitrary roles in the messages array.
    """

    type: Literal["system_notification"] = "system_notification"
    text: str


ContentBlock = Annotated[
    TextBlock
    | ThinkingBlock
    | ToolUseBlock
    | ToolResultBlock
    | SystemNotificationBlock,
    Field(discriminator="type"),
]


class ConversationMessage(BaseModel):
    """A single assistant or user message."""

    role: Literal["user", "assistant"]
    content: list[ContentBlock] = Field(default_factory=list)

    @classmethod
    def from_user_text(cls, text: str) -> ConversationMessage:
        """Construct a user message from raw text."""
        return cls(role="user", content=[TextBlock(text=text)])

    @property
    def text(self) -> str:
        """Return concatenated text blocks (excludes thinking and notifications)."""
        return "".join(
            block.text for block in self.content if isinstance(block, TextBlock)
        )

    @property
    def system_notifications(self) -> list[SystemNotificationBlock]:
        """Return all system-notification blocks contained in this message."""
        return [b for b in self.content if isinstance(b, SystemNotificationBlock)]

    @property
    def system_notification_text(self) -> str:
        """Concatenated text of all system-notification blocks (no tags)."""
        return "\n".join(b.text for b in self.system_notifications)

    @property
    def thinking(self) -> str:
        """Return concatenated thinking blocks."""
        return "".join(
            block.text for block in self.content if isinstance(block, ThinkingBlock)
        )

    @property
    def tool_uses(self) -> list[ToolUseBlock]:
        """Return all tool calls contained in the message."""
        return [block for block in self.content if isinstance(block, ToolUseBlock)]

    def to_api_param(self) -> dict[str, Any]:
        """Convert the message into Anthropic SDK message params.

        Thinking blocks are excluded — Anthropic manages thinking
        internally and does not accept them back in the messages array.
        """
        return {
            "role": self.role,
            "content": [
                serialize_content_block(block)
                for block in self.content
                if not isinstance(block, ThinkingBlock)
            ],
        }


def serialize_content_block(block: ContentBlock) -> dict[str, Any]:
    """Convert a local content block into the provider wire format."""
    if isinstance(block, TextBlock):
        return {"type": "text", "text": block.text}

    if isinstance(block, ThinkingBlock):
        return {"type": "thinking", "text": block.text}

    if isinstance(block, SystemNotificationBlock):
        # Anthropic and most providers do not accept arbitrary block types
        # mid-conversation. Flatten to a text block whose body is wrapped
        # in <system-reminder> tags so the model recognises it as engine-
        # generated guidance rather than user input.
        return {
            "type": "text",
            "text": f"<system-reminder>\n{block.text}\n</system-reminder>",
        }

    if isinstance(block, ToolUseBlock):
        return {
            "type": "tool_use",
            "id": block.id,
            "name": block.name,
            "input": block.input,
        }

    return {
        "type": "tool_result",
        "tool_use_id": block.tool_use_id,
        "content": block.content,
        "is_error": block.is_error,
    }


def assistant_message_from_api(raw_message: Any) -> ConversationMessage:
    """Convert an Anthropic SDK message object into a conversation message."""
    content: list[ContentBlock] = []

    for raw_block in getattr(raw_message, "content", []):
        block_type = getattr(raw_block, "type", None)
        if block_type == "thinking":
            content.append(ThinkingBlock(text=getattr(raw_block, "thinking", "") or getattr(raw_block, "text", "")))
        elif block_type == "text":
            content.append(TextBlock(text=getattr(raw_block, "text", "")))
        elif block_type == "tool_use":
            content.append(
                ToolUseBlock(
                    id=getattr(raw_block, "id", f"toolu_{uuid4().hex}"),
                    name=getattr(raw_block, "name", ""),
                    input=dict(getattr(raw_block, "input", {}) or {}),
                )
            )

    return ConversationMessage(role="assistant", content=content)
