"""Shared runtime assembly for EphemeralOS UI backends."""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Awaitable, Callable

from ephemeralos.models.clients.anthropic import AnthropicApiClient
from ephemeralos.models.clients.openai_compat import OpenAICompatibleClient
from ephemeralos.models.provider import auth_status, detect_provider
from ephemeralos.models.types import SupportsStreamingMessages
from ephemeralos.config import load_settings
from ephemeralos.engine import QueryEngine
from ephemeralos.engine.messages import ConversationMessage
from ephemeralos.engine.stream_events import StreamEvent
from ephemeralos.hooks import HookEvent, HookExecutionContext, HookExecutor, load_hook_registry
from ephemeralos.prompts import build_runtime_system_prompt
from ephemeralos.services.session_storage import save_session_snapshot
from ephemeralos.tools import ToolRegistry
from ephemeralos.tools import create_default_tool_registry

SystemPrinter = Callable[[str], Awaitable[None]]
StreamRenderer = Callable[[StreamEvent], Awaitable[None]]
ClearHandler = Callable[[], Awaitable[None]]


@dataclass
class RuntimeBundle:
    """Shared runtime objects for one interactive session."""

    api_client: SupportsStreamingMessages
    cwd: str
    tool_registry: ToolRegistry
    hook_executor: HookExecutor
    engine: QueryEngine
    external_api_client: bool
    session_id: str = ""

    def current_settings(self):
        """Return the latest persisted settings."""
        return load_settings()


async def build_runtime(
    *,
    prompt: str | None = None,
    model: str | None = None,
    base_url: str | None = None,
    system_prompt: str | None = None,
    api_key: str | None = None,
    api_format: str | None = None,
    api_client: SupportsStreamingMessages | None = None,
    restore_messages: list[dict] | None = None,
) -> RuntimeBundle:
    """Build the shared runtime for an EphemeralOS session."""
    settings = load_settings().merge_cli_overrides(
        model=model,
        base_url=base_url,
        system_prompt=system_prompt,
        api_key=api_key,
        api_format=api_format,
    )
    cwd = str(Path.cwd())
    if api_client:
        resolved_api_client = api_client
    elif settings.api_format == "openai":
        resolved_api_client = OpenAICompatibleClient(
            api_key=settings.resolve_api_key(),
            base_url=settings.base_url,
        )
    else:
        resolved_api_client = AnthropicApiClient(
            api_key=settings.resolve_api_key(),
            base_url=settings.base_url,
        )
    tool_registry = create_default_tool_registry()
    hook_executor = HookExecutor(
        load_hook_registry(settings, []),
        HookExecutionContext(
            cwd=Path(cwd).resolve(),
            api_client=resolved_api_client,
            default_model=settings.model,
        ),
    )
    engine = QueryEngine(
        api_client=resolved_api_client,
        tool_registry=tool_registry,
        cwd=cwd,
        model=settings.model,
        system_prompt=build_runtime_system_prompt(settings, cwd=cwd, latest_user_prompt=prompt),
        max_tokens=settings.max_tokens,
        hook_executor=hook_executor,
    )
    # Restore messages from a saved session if provided
    if restore_messages:
        restored = [
            ConversationMessage.model_validate(m) for m in restore_messages
        ]
        engine.load_messages(restored)

    from uuid import uuid4

    return RuntimeBundle(
        api_client=resolved_api_client,
        cwd=cwd,
        tool_registry=tool_registry,
        hook_executor=hook_executor,
        engine=engine,
        external_api_client=api_client is not None,
        session_id=uuid4().hex[:12],
    )


async def start_runtime(bundle: RuntimeBundle) -> None:
    """Run session start hooks."""
    await bundle.hook_executor.execute(
        HookEvent.SESSION_START,
        {"cwd": bundle.cwd, "event": HookEvent.SESSION_START.value},
    )


async def close_runtime(bundle: RuntimeBundle) -> None:
    """Close runtime-owned resources."""
    await bundle.hook_executor.execute(
        HookEvent.SESSION_END,
        {"cwd": bundle.cwd, "event": HookEvent.SESSION_END.value},
    )


async def handle_line(
    bundle: RuntimeBundle,
    line: str,
    *,
    print_system: SystemPrinter,
    render_event: StreamRenderer,
    clear_output: ClearHandler,
) -> bool:
    """Handle one submitted line."""
    if not bundle.external_api_client:
        bundle.hook_executor.update_registry(
            load_hook_registry(bundle.current_settings(), [])
        )

    settings = bundle.current_settings()
    bundle.engine.set_system_prompt(
        build_runtime_system_prompt(settings, cwd=bundle.cwd, latest_user_prompt=line)
    )
    async for event in bundle.engine.submit_message(line):
        await render_event(event)
    save_session_snapshot(
        cwd=bundle.cwd,
        model=settings.model,
        system_prompt=build_runtime_system_prompt(settings, cwd=bundle.cwd, latest_user_prompt=line),
        messages=bundle.engine.messages,
        usage=bundle.engine.total_usage,
        session_id=bundle.session_id,
    )
    return True
