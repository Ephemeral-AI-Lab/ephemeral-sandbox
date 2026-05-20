"""Codex Responses-API plan-mode client (ChatGPT-account OAuth).

Plan §A4 / §A15: streams against ``https://chatgpt.com/backend-api/codex/responses``
using credentials from ``~/.codex/auth.json``. ChatGPT-Account-Id is JWT-
extracted from ``tokens.id_token`` (Auth0-namespaced claim per Phase 0
spike).

Plan §S4: tool envelope is FLAT, ``max_output_tokens`` is omitted, model
defaults to ``gpt-5.5`` (Phase 0 spike empirical).

Plan §A17: error-path log line ``coding_plan_mode_error`` with
``provider="codex"``, ``error_type=<category>``, ``request_id=<response.id>``
(or None). Categoriser is Codex-specific; auth/rate-limit/5xx categories
mirror the Anthropic side; Codex-only categories are ``cf_mitigated_challenge``,
``model_rejected``, ``schema_rejected``.
"""

from __future__ import annotations

import base64
import binascii
import json
import logging
from collections.abc import AsyncIterator
from pathlib import Path
from typing import Any
from uuid import uuid4

import httpx

from message import ConversationMessage
from message.messages import TextBlock, ToolUseBlock
from providers.auth_strategy import LLM_CLIENT_MODE_CODING_PLAN
from providers.errors import (
    AuthenticationFailure,
    EphemeralOSApiError,
    RateLimitFailure,
    RequestFailure,
)
from providers.types import (
    ApiMessageCompleteEvent,
    ApiMessageRequest,
    ApiStreamEvent,
    ApiTextDeltaEvent,
    ApiThinkingDeltaEvent,
    ApiToolUseDeltaEvent,
    UsageSnapshot,
)


log = logging.getLogger(__name__)


CODEX_RESPONSES_URL = "https://chatgpt.com/backend-api/codex/responses"
CODEX_ORIGINATOR = "codex_cli_rs"
CODEX_UA_VERSION = "0.125"
CODEX_USER_AGENT = f"{CODEX_ORIGINATOR}/{CODEX_UA_VERSION}"
CODEX_DEFAULT_MODEL = "gpt-5.5"
_AUTH0_NAMESPACE = "https://api.openai.com/auth"
_SCHEMA_REJECT_TOKENS = ("schema", "parameters", "additionalproperties")


class CodexCredentialIncompleteError(RuntimeError):
    """Raised when ``~/.codex/auth.json`` is missing or shape-malformed (plan §A15)."""


def _b64url_decode(segment: str) -> bytes:
    pad = "=" * (-len(segment) % 4)
    try:
        return base64.urlsafe_b64decode(segment + pad)
    except (binascii.Error, ValueError) as exc:
        raise CodexCredentialIncompleteError(
            f"Invalid base64url segment in id_token: {exc}"
        ) from exc


def jwt_extract_chatgpt_account_id(id_token: str) -> str:
    """Extract ``chatgpt_account_id`` from a Codex id_token payload.

    Auth0-namespaced claim path first; top-level fallback for
    forward-compatibility. Raises on missing claim.
    """
    parts = id_token.split(".")
    if len(parts) != 3:
        raise CodexCredentialIncompleteError(
            f"id_token has {len(parts)} segments, expected 3 (JWT)"
        )
    payload_raw = _b64url_decode(parts[1])
    try:
        payload = json.loads(payload_raw)
    except json.JSONDecodeError as exc:
        raise CodexCredentialIncompleteError(
            f"id_token payload is not valid JSON: {exc}"
        ) from exc

    namespaced = payload.get(_AUTH0_NAMESPACE)
    if isinstance(namespaced, dict):
        account_id = namespaced.get("chatgpt_account_id")
        if account_id:
            return str(account_id)

    account_id = payload.get("chatgpt_account_id")
    if not account_id:
        raise CodexCredentialIncompleteError(
            "id_token payload missing 'chatgpt_account_id' claim "
            f"(checked '{_AUTH0_NAMESPACE}' namespace and top-level)."
        )
    return str(account_id)


def _load_model_from_codex_config(config_path: Path) -> str | None:
    """Parse Codex's TOML config for ``model = "..."``; return None if absent."""
    if not config_path.exists():
        return None
    try:
        text = config_path.read_text(encoding="utf-8")
    except OSError:
        return None
    for line in text.splitlines():
        stripped = line.strip()
        if stripped.startswith("model"):
            _, _, value = stripped.partition("=")
            value = value.strip().strip('"').strip("'")
            if value:
                return value
    return None


def _categorize(exc: EphemeralOSApiError) -> str:
    """Codex error categorisation (plan §S4)."""
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
        if "not supported when using codex with a chatgpt account" in msg:
            return "model_rejected"
        if any(token in msg for token in _SCHEMA_REJECT_TOKENS):
            return "schema_rejected"
        if "cf-mitigated" in msg:
            return "cf_mitigated_challenge"
    return "unknown"


class CodexResponsesClient:
    """Codex Responses-API streaming client (plan §A4/§A15)."""

    llm_client_mode: str = LLM_CLIENT_MODE_CODING_PLAN

    def __init__(self, *, db_kwargs: dict[str, Any] | None = None) -> None:
        kwargs = db_kwargs or {}
        self._auth_path = Path(
            kwargs.get("auth_path", Path.home() / ".codex" / "auth.json")
        )
        self._config_path = Path(
            kwargs.get("config_path", Path.home() / ".codex" / "config.toml")
        )
        self._access_token, id_token = self._load_codex_auth(self._auth_path)
        self._chatgpt_account_id = jwt_extract_chatgpt_account_id(id_token)
        self._model = (
            kwargs.get("model")
            or _load_model_from_codex_config(self._config_path)
            or CODEX_DEFAULT_MODEL
        )

    # ------------------------------------------------------------------
    # Credential loading
    # ------------------------------------------------------------------

    @staticmethod
    def _load_codex_auth(auth_path: Path) -> tuple[str, str]:
        if not auth_path.exists():
            raise CodexCredentialIncompleteError(
                f"{auth_path} missing — run `codex login` once to populate."
            )
        try:
            payload = json.loads(auth_path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as exc:
            raise CodexCredentialIncompleteError(
                f"{auth_path} unreadable or not JSON: {exc}"
            ) from exc
        tokens = payload.get("tokens") or {}
        access = tokens.get("access_token")
        id_token = tokens.get("id_token")
        if not access:
            raise CodexCredentialIncompleteError(
                f"{auth_path} missing tokens.access_token"
            )
        if not id_token:
            raise CodexCredentialIncompleteError(
                f"{auth_path} missing tokens.id_token"
            )
        return str(access), str(id_token)

    def _refresh_credentials(self) -> bool:
        """Plan §A7 — reload ``auth.json`` for retry-on-401.

        Returns True iff ``_access_token`` or ``_chatgpt_account_id``
        changed (``build_headers`` reads both, so either flip rotates the
        request signature). ``CodexCredentialIncompleteError`` → False.
        """
        try:
            new_access, new_id_token = self._load_codex_auth(self._auth_path)
            new_account_id = jwt_extract_chatgpt_account_id(new_id_token)
        except CodexCredentialIncompleteError:
            return False
        changed = (
            new_access != self._access_token
            or new_account_id != self._chatgpt_account_id
        )
        if not changed:
            return False
        self._access_token = new_access
        self._chatgpt_account_id = new_account_id
        return True

    # ------------------------------------------------------------------
    # Request builders (testable in isolation)
    # ------------------------------------------------------------------

    def build_headers(self) -> dict[str, str]:
        return {
            "Authorization": f"Bearer {self._access_token}",
            "ChatGPT-Account-Id": self._chatgpt_account_id,
            "originator": CODEX_ORIGINATOR,
            "User-Agent": CODEX_USER_AGENT,
            "OpenAI-Beta": "responses=experimental",
            "Content-Type": "application/json",
        }

    def build_body(self, request: ApiMessageRequest) -> dict[str, Any]:
        """Translate an ``ApiMessageRequest`` into a Codex Responses-API body.

        Tool envelope FLAT (plan v9.2); ``max_output_tokens`` omitted (plan v9.2).
        """
        codex_input: list[dict[str, Any]] = []
        for msg in request.messages:
            blocks: list[dict[str, Any]] = []
            for block in getattr(msg, "content", []):
                if isinstance(block, TextBlock):
                    blocks.append({"type": "input_text", "text": block.text})
                # Tool-result and other block types are passthrough-encoded
                # in their Anthropic shape today; the Codex equivalent is
                # the responsibility of a future round trip and not in S4
                # scope.
            if blocks:
                codex_input.append({"role": msg.role, "content": blocks})

        flat_tools: list[dict[str, Any]] = []
        for tool in request.tools or []:
            flat_tools.append(
                {
                    "type": "function",
                    "name": tool["name"],
                    "description": tool.get("description", ""),
                    "parameters": tool.get(
                        "input_schema", tool.get("parameters", {})
                    ),
                }
            )

        body: dict[str, Any] = {
            "model": request.model or self._model,
            "instructions": request.system_prompt or "",
            "input": codex_input,
            "stream": True,
            "store": False,
            "parallel_tool_calls": True,
        }
        if flat_tools:
            body["tools"] = flat_tools
        return body

    # ------------------------------------------------------------------
    # Streaming
    # ------------------------------------------------------------------

    async def stream_message(
        self, request: ApiMessageRequest
    ) -> AsyncIterator[ApiStreamEvent]:
        body = self.build_body(request)
        response_id: str | None = None
        text_acc = ""
        tool_buffers: dict[str, dict[str, Any]] = {}
        collected_tools: list[ToolUseBlock] = []
        usage_in = 0
        usage_out = 0
        stop_reason: str | None = None

        attempted_refresh = False
        for _attempt in range(2):
            response_id = None
            text_acc = ""
            tool_buffers = {}
            collected_tools = []
            usage_in = 0
            usage_out = 0
            stop_reason = None
            headers = self.build_headers()

            try:
                async with httpx.AsyncClient(timeout=120.0) as http:
                    async with http.stream(
                        "POST", CODEX_RESPONSES_URL, headers=headers, json=body
                    ) as response:
                        if response.status_code != 200:
                            if (
                                response.status_code == 401
                                and not attempted_refresh
                            ):
                                attempted_refresh = True
                                if self._refresh_credentials():
                                    continue
                            body_text = (await response.aread()).decode(
                                "utf-8", errors="replace"
                            )
                            cf_mit = response.headers.get("cf-mitigated", "")
                            message = (
                                f"Codex {response.status_code}: {body_text[:512]}"
                            )
                            if cf_mit:
                                message = f"{message} (cf-mitigated={cf_mit})"
                            raise _CodexHttpError(
                                status_code=response.status_code,
                                message=message,
                                request_id=response.headers.get("x-request-id"),
                            )

                        async for line in response.aiter_lines():
                            if not line or not line.startswith("data: "):
                                continue
                            payload_text = line[6:].strip()
                            if not payload_text or payload_text == "[DONE]":
                                continue
                            try:
                                payload = json.loads(payload_text)
                            except json.JSONDecodeError:
                                continue
                            event_type = payload.get("type")

                            if event_type == "response.created":
                                response_id = (
                                    payload.get("response", {}).get("id") or response_id
                                )
                                continue

                            if event_type == "response.in_progress":
                                continue

                            if event_type == "response.output_text.delta":
                                delta = payload.get("delta", "")
                                text_acc += delta
                                yield ApiTextDeltaEvent(text=delta)
                                continue

                            if event_type == "response.reasoning_summary_text.delta":
                                yield ApiThinkingDeltaEvent(text=payload.get("delta", ""))
                                continue

                            if event_type == "response.output_item.added":
                                item = payload.get("item") or {}
                                if item.get("type") == "function_call":
                                    item_id = item.get("id") or item.get("call_id") or ""
                                    tool_buffers[item_id] = {
                                        "call_id": item.get("call_id") or item.get("id") or f"toolu_{uuid4().hex}",
                                        "name": item.get("name", ""),
                                        "args_buf": "",
                                    }
                                continue

                            if event_type == "response.function_call_arguments.delta":
                                item_id = payload.get("item_id", "")
                                buf = tool_buffers.get(item_id)
                                if buf is not None:
                                    buf["args_buf"] += payload.get("delta", "")
                                continue

                            if event_type == "response.function_call_arguments.done":
                                item_id = payload.get("item_id", "")
                                buf = tool_buffers.get(item_id)
                                if buf is None:
                                    continue
                                raw_args = buf["args_buf"]
                                try:
                                    args = json.loads(raw_args) if raw_args else {}
                                except (json.JSONDecodeError, TypeError):
                                    args = {}
                                collected_tools.append(
                                    ToolUseBlock(
                                        id=buf["call_id"],
                                        name=buf["name"],
                                        input=args,
                                    )
                                )
                                yield ApiToolUseDeltaEvent(
                                    id=buf["call_id"], name=buf["name"], input=args
                                )
                                continue

                            if event_type == "response.completed":
                                resp = payload.get("response") or {}
                                usage = resp.get("usage") or {}
                                usage_in = int(usage.get("input_tokens", 0) or 0)
                                usage_out = int(usage.get("output_tokens", 0) or 0)
                                stop_reason = resp.get("stop_reason", "end_turn")
                                response_id = resp.get("id") or response_id
                                continue
                break

            except _CodexHttpError as exc:
                translated = self._translate_http_error(exc)
                self._emit_coding_plan_mode_error(translated, response_id)
                raise translated from exc
            except EphemeralOSApiError as exc:
                self._emit_coding_plan_mode_error(exc, response_id)
                raise
            except httpx.RequestError as exc:
                translated = RequestFailure(str(exc))
                self._emit_coding_plan_mode_error(translated, response_id)
                raise translated from exc

        content: list[Any] = []
        if text_acc:
            content.append(TextBlock(text=text_acc))
        content.extend(collected_tools)
        message = ConversationMessage(role="assistant", content=content)
        yield ApiMessageCompleteEvent(
            message=message,
            usage=UsageSnapshot(input_tokens=usage_in, output_tokens=usage_out),
            stop_reason=stop_reason,
        )

    # ------------------------------------------------------------------
    # Error helpers
    # ------------------------------------------------------------------

    @staticmethod
    def _translate_http_error(exc: "_CodexHttpError") -> EphemeralOSApiError:
        status = exc.status_code
        msg = exc.message
        request_id = exc.request_id
        if status in {401, 403}:
            return AuthenticationFailure(msg, status_code=status, request_id=request_id)
        if status == 429:
            return RateLimitFailure(msg, status_code=status, request_id=request_id)
        return RequestFailure(msg, status_code=status, request_id=request_id)

    def _emit_coding_plan_mode_error(
        self, exc: EphemeralOSApiError, response_id: str | None
    ) -> None:
        if self.llm_client_mode != LLM_CLIENT_MODE_CODING_PLAN:
            return
        log.error(
            "coding_plan_mode_error",
            extra={
                "provider": "codex",
                "error_type": _categorize(exc),
                "request_id": exc.request_id or response_id,
            },
        )


class _CodexHttpError(Exception):
    """Internal carrier for HTTP-status failures from the Codex stream."""

    def __init__(
        self,
        *,
        status_code: int,
        message: str,
        request_id: str | None,
    ) -> None:
        super().__init__(message)
        self.status_code = status_code
        self.message = message
        self.request_id = request_id


__all__ = [
    "CodexResponsesClient",
    "CodexCredentialIncompleteError",
    "jwt_extract_chatgpt_account_id",
]
