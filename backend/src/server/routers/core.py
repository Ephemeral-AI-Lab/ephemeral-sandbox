"""Core API routes — health, state, chat, config, sessions."""

from __future__ import annotations

import asyncio
import json
from typing import TYPE_CHECKING

from fastapi import APIRouter
from fastapi.responses import JSONResponse, StreamingResponse
from pydantic import BaseModel

from ephemeralos.models.clients.anthropic import AnthropicApiClient
from ephemeralos.models.clients.openai_compat import OpenAICompatibleClient
from ephemeralos.models.provider import auth_status, detect_provider
from ephemeralos.config import load_settings, save_settings
from ephemeralos.engine.stream_events import (
    AssistantTextDelta,
    AssistantTurnComplete,
    StreamEvent,
    ToolExecutionCompleted,
    ToolExecutionStarted,
)
from ephemeralos.tasks import get_task_manager
from ephemeralos.server.protocol import BackendEvent, TranscriptItem
from ephemeralos.server.runtime import handle_line

if TYPE_CHECKING:
    from ephemeralos.server.app_factory import SessionState

# ---------------------------------------------------------------------------
# Request models
# ---------------------------------------------------------------------------


class ChatRequest(BaseModel):
    line: str


class ConfigRequest(BaseModel):
    model: str | None = None
    base_url: str | None = None
    api_key: str | None = None
    api_format: str | None = None



# ---------------------------------------------------------------------------
# Router factory — receives get_session callable from web_server
# ---------------------------------------------------------------------------


def create_core_router(get_session: callable) -> APIRouter:
    """Build the core API router."""
    router = APIRouter(prefix="/api")

    @router.get("/health")
    async def health():
        return {"status": "ok", "service": "ephemeralos"}

    @router.get("/state")
    async def get_state():
        session = get_session()
        assert session.bundle is not None
        settings = session.bundle.current_settings()
        app_state = {
            "model": settings.model,
            "cwd": session.bundle.cwd,
            "provider": settings.api_format,
            "auth_status": "authorized",
            "base_url": settings.base_url or "",
            "theme": settings.theme,
            "vim_enabled": False,
            "voice_enabled": False,
            "voice_available": False,
            "voice_reason": "",
            "fast_mode": settings.fast_mode,
            "effort": settings.effort,
            "passes": settings.passes,
            "bridge_sessions": 0,
            "output_style": "verbose" if settings.verbose else "normal",
            "keybindings": {},
        }
        ready = BackendEvent.ready(
            get_task_manager().list_tasks(),
            toolkits=session._toolkit_snapshots(),
            state=app_state,
        )
        return JSONResponse(content=json.loads(ready.model_dump_json()))

    @router.post("/chat")
    async def chat(req: ChatRequest):
        session = get_session()
        assert session.bundle is not None

        if session.busy:
            return JSONResponse(status_code=409, content={"error": "Session is busy"})

        queue: asyncio.Queue[BackendEvent | None] = asyncio.Queue()
        session.set_event_queue(queue)
        session.busy = True

        async def process() -> None:
            try:
                bundle = session.bundle
                assert bundle is not None

                await session.emit(
                    BackendEvent(
                        type="transcript_item",
                        item=TranscriptItem(role="user", text=req.line),
                    )
                )

                async def _print_system(message: str) -> None:
                    await session.emit(
                        BackendEvent(
                            type="transcript_item",
                            item=TranscriptItem(role="system", text=message),
                        )
                    )

                async def _render_event(event: StreamEvent) -> None:
                    if isinstance(event, AssistantTextDelta):
                        await session.emit(BackendEvent(type="assistant_delta", message=event.text))
                    elif isinstance(event, AssistantTurnComplete):
                        await session.emit(
                            BackendEvent(
                                type="assistant_complete",
                                message=event.message.text.strip(),
                                item=TranscriptItem(role="assistant", text=event.message.text.strip()),
                            )
                        )
                        await session.emit(BackendEvent.tasks_snapshot(get_task_manager().list_tasks()))
                    elif isinstance(event, ToolExecutionStarted):
                        await session.emit(
                            BackendEvent(
                                type="tool_started",
                                tool_name=event.tool_name,
                                tool_input=event.tool_input,
                                item=TranscriptItem(
                                    role="tool",
                                    text=f"{event.tool_name} {json.dumps(event.tool_input, ensure_ascii=True)}",
                                    tool_name=event.tool_name,
                                    tool_input=event.tool_input,
                                ),
                            )
                        )
                    elif isinstance(event, ToolExecutionCompleted):
                        await session.emit(
                            BackendEvent(
                                type="tool_completed",
                                tool_name=event.tool_name,
                                output=event.output,
                                is_error=event.is_error,
                                item=TranscriptItem(
                                    role="tool_result",
                                    text=event.output,
                                    tool_name=event.tool_name,
                                    is_error=event.is_error,
                                ),
                            )
                        )
                        await session.emit(BackendEvent.tasks_snapshot(get_task_manager().list_tasks()))

                async def _clear_output() -> None:
                    await session.emit(BackendEvent(type="clear_transcript"))

                await handle_line(
                    bundle,
                    req.line,
                    print_system=_print_system,
                    render_event=_render_event,
                    clear_output=_clear_output,
                )
                await session.emit(BackendEvent.tasks_snapshot(get_task_manager().list_tasks()))
                await session.emit(BackendEvent(type="line_complete"))
            except Exception as exc:
                await session.emit(
                    BackendEvent(type="error", message=f"Processing error: {exc}")
                )
            finally:
                await queue.put(None)
                session.busy = False
                session.set_event_queue(None)

        task = asyncio.create_task(process())

        async def event_generator():
            try:
                while True:
                    event = await queue.get()
                    if event is None:
                        break
                    yield f"data: {event.model_dump_json()}\n\n"
                yield "data: [DONE]\n\n"
            except asyncio.CancelledError:
                task.cancel()

        return StreamingResponse(
            event_generator(),
            media_type="text/event-stream",
            headers={
                "Cache-Control": "no-cache",
                "Connection": "keep-alive",
                "X-Accel-Buffering": "no",
            },
        )

    @router.post("/config")
    async def update_config(req: ConfigRequest):
        session = get_session()
        assert session.bundle is not None

        settings = load_settings()
        changed = False
        for key in ("model", "base_url", "api_key", "api_format"):
            value = getattr(req, key, None)
            if value is not None:
                setattr(settings, key, value)
                changed = True

        if not changed:
            return JSONResponse(content={"changed": False})

        save_settings(settings)

        if settings.api_format == "openai":
            new_client = OpenAICompatibleClient(
                api_key=settings.resolve_api_key(),
                base_url=settings.base_url,
            )
        else:
            new_client = AnthropicApiClient(
                api_key=settings.resolve_api_key(),
                base_url=settings.base_url,
            )

        session.bundle.api_client = new_client
        session.bundle.engine._api_client = new_client
        session.bundle.engine.set_model(settings.model)

        provider = detect_provider(settings)
        return JSONResponse(content={
            "changed": True,
            "model": settings.model,
            "provider": provider.name,
            "auth_status": auth_status(settings),
            "base_url": settings.base_url or "",
        })

    @router.get("/sessions")
    async def list_sessions():
        session = get_session()
        assert session.bundle is not None
        from ephemeralos.services.session_storage import list_session_snapshots
        import time as _time

        snapshots = list_session_snapshots(session.bundle.cwd, limit=10)
        options = []
        for s in snapshots:
            ts = _time.strftime("%m/%d %H:%M", _time.localtime(s["created_at"]))
            summary = s.get("summary", "")[:50] or "(no summary)"
            options.append({
                "value": s["session_id"],
                "label": f"{ts}  {s['message_count']}msg  {summary}",
            })
        return JSONResponse(content={"sessions": options})

    return router
