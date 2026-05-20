"""Anthropic-native streaming client for EphemeralOS.

Uses the official ``anthropic`` Python SDK directly. The key advantage over the
OpenAI-compatible client is that tool-use blocks are yielded as
``ApiToolUseDeltaEvent`` on ``content_block_stop`` (mid-stream), so tools can
begin executing while the model is still generating subsequent content blocks.
"""

from __future__ import annotations

import asyncio
import json
import logging
from typing import TYPE_CHECKING, Any
from collections.abc import AsyncIterator

import anthropic

if TYPE_CHECKING:
    from providers.auth_strategy import AuthStrategy

from providers.auth_strategy import LLM_CLIENT_MODE_CODING_PLAN
from providers.types import (
    ApiMessageCompleteEvent,
    ApiMessageRequest,
    ApiStreamEvent,
    ApiTextDeltaEvent,
    ApiThinkingDeltaEvent,
    ApiToolUseDeltaEvent,
    UsageSnapshot,
)
from providers.errors import (
    AuthenticationFailure,
    EphemeralOSApiError,
    RateLimitFailure,
    RequestFailure,
)
from message import assistant_message_from_api

log = logging.getLogger(__name__)


def _categorize(exc: EphemeralOSApiError) -> str:
    """Plan §A17 error categorisation, mirrored on the Codex side (S4).

    Always categorises from the POST-translation typed exception so the
    mapping is single-source. The emitted log line is tagged
    ``coding_plan_mode_error`` (S2 rename).
    """
    if isinstance(exc, AuthenticationFailure):
        if exc.status_code == 401:
            return "auth_401"
        if exc.status_code == 403:
            return "auth_403"
    if isinstance(exc, RateLimitFailure):
        return "rate_limit_429"
    if isinstance(exc, RequestFailure):
        if exc.status_code in {500, 502, 503, 529}:
            return "server_5xx"
        msg = str(exc).lower()
        if "content_filter" in msg or "policy" in msg:
            return "content_filter_rejection"
    return "unknown"

MAX_RETRIES = 3
BASE_DELAY = 1.0
MAX_DELAY = 30.0


class AnthropicClient:
    """Anthropic-native streaming client.

    Implements the ``SupportsStreamingMessages`` protocol using the official
    ``anthropic`` async SDK.  Tool-use content blocks are emitted mid-stream
    on ``content_block_stop`` so the engine can start tool execution early.
    """

    def __init__(
        self,
        api_key: str | None = None,
        *,
        base_url: str | None = None,
        auth_strategy: "AuthStrategy | None" = None,
        system_prefix: str | None = None,
    ) -> None:
        if auth_strategy is not None:
            # Strategy-injected mode (plan §A2). Owns api_key/auth_token and
            # optional default_headers (e.g. OAuth beta + UA headers).
            self._auth_strategy = auth_strategy
        else:
            # Today's behavior preserved. Non-Anthropic endpoints (e.g.
            # MiniMax) expect Authorization: Bearer instead of x-api-key.
            if api_key is None:
                raise ValueError(
                    "AnthropicClient requires either api_key or auth_strategy"
                )
            use_auth_token = bool(base_url) and "anthropic.com" not in base_url
            from providers.auth_strategy import make_api_key_strategy

            self._auth_strategy = make_api_key_strategy(
                api_key, use_auth_token=use_auth_token
            )

        self._base_url = base_url
        self._system_prefix = system_prefix
        self._client = self._build_sdk_client()

    def _build_sdk_client(self) -> anthropic.AsyncAnthropic:
        """Construct the underlying SDK client from the current strategy state.

        Used at ``__init__`` and after a successful ``refresh()`` so the new
        token (plan §A7) is picked up.
        """
        kwargs: dict[str, Any] = {}
        if self._base_url:
            kwargs["base_url"] = self._base_url
        kwargs.update(self._auth_strategy.get_auth_kwargs())
        return anthropic.AsyncAnthropic(**kwargs)

    # ------------------------------------------------------------------
    # Lifecycle
    # ------------------------------------------------------------------

    async def aclose(self) -> None:
        """Gracefully close the underlying HTTP transport."""
        await self._client.close()

    # ------------------------------------------------------------------
    # Public interface (SupportsStreamingMessages)
    # ------------------------------------------------------------------

    async def stream_message(self, request: ApiMessageRequest) -> AsyncIterator[ApiStreamEvent]:
        """Yield streamed events for *request* with retry logic.

        Retries are gated on ``emitted_any`` so we never re-yield events the
        caller has already observed. Once a single stream event has been
        forwarded, any failure on the active attempt fails fast — re-running
        the request would duplicate text deltas and double-dispatch tool_use
        ids on the engine side.
        """
        attempted_refresh = False
        for attempt in range(MAX_RETRIES + 1):
            emitted_any = False
            try:
                async for event in self._stream_once(request):
                    emitted_any = True
                    yield event
                return
            except EphemeralOSApiError as exc:
                self._emit_coding_plan_mode_error(exc)
                raise
            except Exception as exc:
                status = getattr(exc, "status_code", None)
                if status == 401 and not attempted_refresh:
                    # Plan §A7: refresh-on-401 retry once. Overrides the
                    # emitted_any fail-fast; replayed deltas are accepted cost.
                    attempted_refresh = True
                    if self._auth_strategy.refresh():
                        self._client = self._build_sdk_client()
                        continue
                    # refresh returned False — strategy cannot self-heal; raise.
                    translated = self._translate_error(exc)
                    self._emit_coding_plan_mode_error(translated)
                    raise translated from exc

                if emitted_any or attempt >= MAX_RETRIES or not self._is_retryable(exc):
                    translated = self._translate_error(exc)
                    self._emit_coding_plan_mode_error(translated)
                    raise translated from exc

                delay = min(BASE_DELAY * (2**attempt), MAX_DELAY)
                log.warning(
                    "Anthropic API request failed (attempt %d/%d), retrying in %.1fs: %s",
                    attempt + 1,
                    MAX_RETRIES + 1,
                    delay,
                    exc,
                )
                await asyncio.sleep(delay)

    # ------------------------------------------------------------------
    # Internal helpers
    # ------------------------------------------------------------------

    async def _stream_once(self, request: ApiMessageRequest) -> AsyncIterator[ApiStreamEvent]:
        """Single streaming attempt against the Anthropic API."""
        messages = [msg.to_api_param() for msg in request.messages]

        # Strip output_schema — the Anthropic API does not accept it in tool defs.
        tools = (
            [{k: v for k, v in t.items() if k != "output_schema"} for t in request.tools]
            if request.tools
            else []
        )

        system_prompt = request.system_prompt or ""
        if self._system_prefix is not None:
            # Plan §A13: OAuth requires identity block #0 to be the literal
            # "You are Claude Code, …"; caller's system becomes block #1.
            blocks: list[dict[str, str]] = [
                {"type": "text", "text": self._system_prefix}
            ]
            # Drop block #1 when caller's system is empty — empty text blocks
            # are rejected by some Anthropic paths.
            if system_prompt:
                blocks.append({"type": "text", "text": system_prompt})
            # Idempotency: if caller already prepended the identity literal,
            # use their list unchanged rather than double-prepending.
            if (
                isinstance(system_prompt, str)
                and system_prompt.startswith(self._system_prefix)
            ):
                blocks = [{"type": "text", "text": system_prompt}]
            system_field: Any = blocks
        else:
            system_field = system_prompt

        params: dict[str, Any] = {
            "model": request.model,
            "messages": messages,
            "system": system_field,
            "max_tokens": request.max_tokens,
        }
        if tools:
            params["tools"] = tools
        if request.tool_choice:
            params["tool_choice"] = request.tool_choice

        # Track content blocks by index for reassembly.
        collected_content_blocks: dict[int, dict[str, Any]] = {}

        async with self._client.messages.stream(**params) as stream:
            async for event in stream:
                event_type = event.type

                if event_type == "content_block_start":
                    block = event.content_block
                    collected_content_blocks[event.index] = {
                        "type": block.type,
                        "id": getattr(block, "id", ""),
                        "name": getattr(block, "name", ""),
                        "text": "",
                        "input_json": "",
                    }

                elif event_type == "content_block_delta":
                    delta = event.delta
                    idx = event.index
                    block_state = collected_content_blocks[idx]

                    if delta.type == "text_delta":
                        block_state["text"] += delta.text
                        yield ApiTextDeltaEvent(text=delta.text)

                    elif delta.type == "thinking_delta":
                        block_state["text"] += delta.thinking
                        yield ApiThinkingDeltaEvent(text=delta.thinking)

                    elif delta.type == "input_json_delta":
                        block_state["input_json"] += delta.partial_json

                elif event_type == "content_block_stop":
                    idx = event.index
                    block_state = collected_content_blocks[idx]

                    if block_state["type"] == "tool_use":
                        # KEY: yield tool event MID-STREAM with complete args
                        try:
                            args = (
                                json.loads(block_state["input_json"])
                                if block_state["input_json"]
                                else {}
                            )
                        except (json.JSONDecodeError, TypeError):
                            args = {}
                        yield ApiToolUseDeltaEvent(
                            id=block_state["id"],
                            name=block_state["name"],
                            input=args,
                        )

            # After the stream ends, build the final message from the SDK.
            final_msg = await stream.get_final_message()

        message = assistant_message_from_api(final_msg)

        yield ApiMessageCompleteEvent(
            message=message,
            usage=UsageSnapshot(
                input_tokens=final_msg.usage.input_tokens,
                output_tokens=final_msg.usage.output_tokens,
            ),
            stop_reason=final_msg.stop_reason,
        )

    @staticmethod
    def _is_retryable(exc: Exception) -> bool:
        """Return True if the exception is transient and worth retrying."""
        if isinstance(exc, anthropic.APIStatusError):
            return exc.status_code in {429, 500, 502, 503, 529}
        if isinstance(exc, (ConnectionError, TimeoutError, OSError)):
            return True
        return False

    @staticmethod
    def _translate_error(exc: Exception) -> EphemeralOSApiError:
        """Map upstream exceptions to EphemeralOS error hierarchy."""
        status = getattr(exc, "status_code", None)
        request_id = getattr(exc, "request_id", None)
        msg = str(exc)
        if status in {401, 403}:
            return AuthenticationFailure(msg, status_code=status, request_id=request_id)
        if status == 429:
            return RateLimitFailure(msg, status_code=status, request_id=request_id)
        return RequestFailure(msg, status_code=status, request_id=request_id)

    def _emit_coding_plan_mode_error(self, exc: EphemeralOSApiError) -> None:
        """Plan §A17: structured error log line under coding-plan mode."""
        if self._auth_strategy.llm_client_mode != LLM_CLIENT_MODE_CODING_PLAN:
            return
        log.error(
            "coding_plan_mode_error",
            extra={
                "provider": "anthropic",
                "error_type": _categorize(exc),
                "request_id": exc.request_id,
            },
        )
