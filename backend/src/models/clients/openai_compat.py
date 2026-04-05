"""OpenAI-compatible API client for providers like Alibaba DashScope, GitHub Models, etc."""

from __future__ import annotations

import asyncio
import json
import logging
from typing import Any, AsyncIterator

from openai import AsyncOpenAI

from models.types import (
    ApiCancelEvent,
    ApiMessageCompleteEvent,
    ApiMessageRequest,
    ApiStreamEvent,
    ApiTextDeltaEvent,
    ApiThinkingDeltaEvent,
    ApiToolUseDeltaEvent,
    UsageSnapshot,
)
from models.errors import (
    AuthenticationFailure,
    EphemeralOSApiError,
    RateLimitFailure,
    RequestFailure,
)
from engine.messages import (
    ConversationMessage,
    ContentBlock,
    TextBlock,
    ThinkingBlock,
    ToolResultBlock,
    ToolUseBlock,
)

log = logging.getLogger(__name__)

MAX_RETRIES = 3
BASE_DELAY = 1.0
MAX_DELAY = 30.0


def _convert_tools_to_openai(tools: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """Convert Anthropic tool schemas to OpenAI function-calling format.

    Anthropic format:
        {"name": "...", "description": "...", "input_schema": {...}, "output_schema": {...}}
    OpenAI format:
        {"type": "function", "function": {"name": "...", "description": "...", "parameters": {...}}}

    OpenAI does not support output schemas in tool definitions, so when an
    output_schema is present we append a summary of the expected return
    fields to the description so the model is aware of the structured output.
    """
    result = []
    for tool in tools:
        description = tool.get("description", "")
        output_schema = tool.get("output_schema")
        if output_schema and "properties" in output_schema:
            fields = ", ".join(
                f"{k}: {v.get('type', 'any')}" for k, v in output_schema["properties"].items()
            )
            description += f"\n\nReturns JSON with fields: {fields}"

        result.append(
            {
                "type": "function",
                "function": {
                    "name": tool["name"],
                    "description": description,
                    "parameters": tool.get("input_schema", {}),
                },
            }
        )
    return result


def _convert_messages_to_openai(
    messages: list[ConversationMessage],
    system_prompt: str | None,
) -> list[dict[str, Any]]:
    """Convert Anthropic-style messages to OpenAI chat format.

    Key differences:
    - Anthropic: system prompt is a separate parameter
    - OpenAI: system prompt is a message with role="system"
    - Anthropic: tool_use / tool_result are content blocks
    - OpenAI: tool_calls on assistant message, tool results are separate messages
    """
    openai_messages: list[dict[str, Any]] = []

    if system_prompt:
        openai_messages.append({"role": "system", "content": system_prompt})

    for msg in messages:
        if msg.role == "assistant":
            openai_msg = _convert_assistant_message(msg)
            openai_messages.append(openai_msg)
        elif msg.role == "user":
            # User messages may contain text or tool_result blocks
            tool_results = [b for b in msg.content if isinstance(b, ToolResultBlock)]
            text_blocks = [b for b in msg.content if isinstance(b, TextBlock)]

            if tool_results:
                # Each tool result becomes a separate message with role="tool"
                for tr in tool_results:
                    openai_messages.append(
                        {
                            "role": "tool",
                            "tool_call_id": tr.tool_use_id,
                            "content": tr.content,
                        }
                    )
            if text_blocks:
                text = "".join(b.text for b in text_blocks)
                if text.strip():
                    openai_messages.append({"role": "user", "content": text})
            if not tool_results and not text_blocks:
                # Empty user message (shouldn't happen, but handle gracefully)
                openai_messages.append({"role": "user", "content": ""})

    return openai_messages


def _convert_assistant_message(msg: ConversationMessage) -> dict[str, Any]:
    """Convert an assistant ConversationMessage to OpenAI format.

    Providers with thinking models (e.g. Kimi k2.5) require a
    ``reasoning_content`` field on every assistant message that contains
    tool calls.  ThinkingBlock content is replayed as ``reasoning_content``.
    """
    text_parts = [b.text for b in msg.content if isinstance(b, TextBlock)]
    thinking_parts = [b.text for b in msg.content if isinstance(b, ThinkingBlock)]
    tool_uses = [b for b in msg.content if isinstance(b, ToolUseBlock)]

    openai_msg: dict[str, Any] = {"role": "assistant"}

    content = "".join(text_parts)
    openai_msg["content"] = content if content else None

    # Replay reasoning_content for thinking models from ThinkingBlock content
    reasoning = "".join(thinking_parts)
    if reasoning:
        openai_msg["reasoning_content"] = reasoning
    elif tool_uses:
        # Thinking models require this field even if empty
        openai_msg["reasoning_content"] = ""

    if tool_uses:
        openai_msg["tool_calls"] = [
            {
                "id": tu.id,
                "type": "function",
                "function": {
                    "name": tu.name,
                    "arguments": json.dumps(tu.input),
                },
            }
            for tu in tool_uses
        ]

    return openai_msg


class OpenAICompatibleClient:
    """Client for OpenAI-compatible APIs (DashScope, GitHub Models, etc.).

    Implements the SupportsStreamingMessages protocol for the agent loop.
    """

    def __init__(self, api_key: str, *, base_url: str | None = None) -> None:
        kwargs: dict[str, Any] = {"api_key": api_key}
        if base_url:
            kwargs["base_url"] = base_url
        self._client = AsyncOpenAI(**kwargs)

    async def stream_message(self, request: ApiMessageRequest) -> AsyncIterator[ApiStreamEvent]:
        """Yield text deltas and the final message, matching the Anthropic client interface."""
        last_error: Exception | None = None

        for attempt in range(MAX_RETRIES + 1):
            try:
                async for event in self._stream_once(request):
                    yield event
                return
            except EphemeralOSApiError:
                raise
            except Exception as exc:
                last_error = exc
                if attempt >= MAX_RETRIES or not self._is_retryable(exc):
                    raise self._translate_error(exc) from exc

                delay = min(BASE_DELAY * (2**attempt), MAX_DELAY)
                log.warning(
                    "OpenAI API request failed (attempt %d/%d), retrying in %.1fs: %s",
                    attempt + 1,
                    MAX_RETRIES + 1,
                    delay,
                    exc,
                )
                await asyncio.sleep(delay)

        if last_error is not None:
            raise self._translate_error(last_error) from last_error

    async def _stream_once(self, request: ApiMessageRequest) -> AsyncIterator[ApiStreamEvent]:
        """Single attempt: stream an OpenAI chat completion."""
        openai_messages = _convert_messages_to_openai(request.messages, request.system_prompt)
        openai_tools = _convert_tools_to_openai(request.tools) if request.tools else None

        params: dict[str, Any] = {
            "model": request.model,
            "messages": openai_messages,
            "max_tokens": request.max_tokens,
            "stream": True,
            "stream_options": {"include_usage": True},
        }
        if openai_tools:
            params["tools"] = openai_tools
            # Some providers (Kimi) error on empty reasoning_content in
            # tool-call follow-ups.  Omit the entire stream_options key if
            # tools are present -- avoids triggering model-side thinking mode
            # that requires reasoning_content on every assistant message.
            params.pop("stream_options", None)

        # Collect full response while streaming text deltas
        collected_content = ""
        collected_reasoning = ""
        collected_tool_calls: dict[int, dict[str, Any]] = {}
        yielded_tool_calls: set[int] = set()
        finish_reason: str | None = None
        usage_data: dict[str, int] = {}

        stream = await self._client.chat.completions.create(**params)
        async for chunk in stream:
            if not chunk.choices:
                # Usage-only chunk (some providers send this at the end)
                if chunk.usage:
                    usage_data = {
                        "input_tokens": chunk.usage.prompt_tokens or 0,
                        "output_tokens": chunk.usage.completion_tokens or 0,
                    }
                continue

            delta = chunk.choices[0].delta
            chunk_finish = chunk.choices[0].finish_reason

            if chunk_finish:
                finish_reason = chunk_finish

            # Stream reasoning_content from thinking models
            reasoning_piece = getattr(delta, "reasoning_content", None) or ""
            if reasoning_piece:
                collected_reasoning += reasoning_piece
                yield ApiThinkingDeltaEvent(text=reasoning_piece)

            # Stream text content to user
            if delta.content:
                collected_content += delta.content
                yield ApiTextDeltaEvent(text=delta.content)

            # Accumulate tool calls and yield mid-stream for early tool start
            if delta.tool_calls:
                for tc_delta in delta.tool_calls:
                    idx = tc_delta.index
                    if idx not in collected_tool_calls:
                        collected_tool_calls[idx] = {
                            "id": tc_delta.id or "",
                            "name": "",
                            "arguments": "",
                        }
                    entry = collected_tool_calls[idx]
                    if tc_delta.id:
                        entry["id"] = tc_delta.id
                    if tc_delta.function:
                        if tc_delta.function.name:
                            entry["name"] = tc_delta.function.name
                        if tc_delta.function.arguments:
                            entry["arguments"] += tc_delta.function.arguments

                    # Yield mid-stream when we have name and some arguments
                    if idx not in yielded_tool_calls and entry["name"]:
                        yielded_tool_calls.add(idx)
                        try:
                            args = json.loads(entry["arguments"]) if entry["arguments"] else {}
                        except (json.JSONDecodeError, TypeError):
                            args = {}
                        yield ApiToolUseDeltaEvent(
                            id=entry["id"] or f"toolu_{idx}",
                            name=entry["name"],
                            input=args,
                        )

            # Usage in chunk (if provider sends it)
            if chunk.usage:
                usage_data = {
                    "input_tokens": chunk.usage.prompt_tokens or 0,
                    "output_tokens": chunk.usage.completion_tokens or 0,
                }

        # Build the final ConversationMessage
        content: list[ContentBlock] = []
        if collected_reasoning:
            content.append(ThinkingBlock(text=collected_reasoning))
        if collected_content:
            content.append(TextBlock(text=collected_content))

        for _idx in sorted(collected_tool_calls.keys()):
            tc = collected_tool_calls[_idx]
            # Skip phantom/empty tool calls that some providers send
            if not tc["name"]:
                continue
            try:
                args = json.loads(tc["arguments"])
            except (json.JSONDecodeError, TypeError):
                args = {}
            content.append(
                ToolUseBlock(
                    id=tc["id"],
                    name=tc["name"],
                    input=args,
                )
            )

        final_message = ConversationMessage(role="assistant", content=content)

        yield ApiMessageCompleteEvent(
            message=final_message,
            usage=UsageSnapshot(
                input_tokens=usage_data.get("input_tokens", 0),
                output_tokens=usage_data.get("output_tokens", 0),
            ),
            stop_reason=finish_reason,
        )

    @staticmethod
    def _is_retryable(exc: Exception) -> bool:
        status = getattr(exc, "status_code", None)
        if status and status in {429, 500, 502, 503}:
            return True
        if isinstance(exc, (ConnectionError, TimeoutError, OSError)):
            return True
        return False

    @staticmethod
    def _translate_error(exc: Exception) -> EphemeralOSApiError:
        status = getattr(exc, "status_code", None)
        msg = str(exc)
        if status == 401 or status == 403:
            return AuthenticationFailure(msg)
        if status == 429:
            return RateLimitFailure(msg)
        return RequestFailure(msg)
