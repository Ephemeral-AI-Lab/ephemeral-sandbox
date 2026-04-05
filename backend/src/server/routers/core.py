"""Core API routes — health, state, chat, config, sessions."""

from __future__ import annotations

import asyncio
import json
from collections.abc import Callable
from typing import TYPE_CHECKING

from fastapi import APIRouter, HTTPException
from fastapi.responses import JSONResponse, StreamingResponse
from pydantic import BaseModel

from ephemeralos.models.provider import detect_provider, auth_status
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
    agent_name: str | None = None
    sandbox_id: str | None = None


class ConfigRequest(BaseModel):
    model: str | None = None
    base_url: str | None = None
    api_key: str | None = None
    api_format: str | None = None



# ---------------------------------------------------------------------------
# Router factory — receives get_session callable from web_server
# ---------------------------------------------------------------------------


def create_core_router(get_session: Callable[[], "SessionState"]) -> APIRouter:
    """Build the core API router."""
    router = APIRouter(prefix="/api")

    @router.get("/health")
    async def health():
        return {"status": "ok", "service": "ephemeralos"}

    @router.get("/state")
    async def get_state():
        session = get_session()
        if session.config is None:
            raise HTTPException(status_code=503, detail="Session not ready")
        settings = session.current_settings()
        app_state = {
            "model": settings.model,
            "cwd": session.cwd,
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
        if session.config is None:
            raise HTTPException(status_code=503, detail="Session not ready")

        async with session._busy_lock:
            if session.busy:
                return JSONResponse(status_code=409, content={"error": "Session is busy"})
            session.busy = True

        queue: asyncio.Queue[BackendEvent | None] = asyncio.Queue()
        session.set_event_queue(queue)

        async def process() -> None:
            try:
                config = session.config
                if config is None:
                    raise RuntimeError("Session not ready")

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

                # Resolve agent definition if requested
                agent_def = None
                if req.agent_name:
                    from ephemeralos.agents.registry import get_definition
                    agent_def = get_definition(req.agent_name)
                    if agent_def is None:
                        await _print_system(f"Agent '{req.agent_name}' not found — using default")

                # Attach sandbox context if requested
                if req.sandbox_id:
                    sandbox_line = f"[sandbox:{req.sandbox_id}] {req.line}"
                else:
                    sandbox_line = req.line

                await handle_line(
                    config,
                    sandbox_line,
                    print_system=_print_system,
                    render_event=_render_event,
                    clear_output=_clear_output,
                    agent_def=agent_def,
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
        if session.config is None:
            raise HTTPException(status_code=503, detail="Session not ready")

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

        # Update durable config overrides so the next ephemeral agent picks them up
        config = session.config
        if req.model is not None:
            config.model_override = req.model
        if req.base_url is not None:
            config.base_url_override = req.base_url
        if req.api_key is not None:
            config.api_key_override = req.api_key
        if req.api_format is not None:
            config.api_format_override = req.api_format

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
        if session.config is None:
            raise HTTPException(status_code=503, detail="Session not ready")
        from ephemeralos.server.app_factory import session_store
        import time as _time

        if session_store._session_factory is None:
            return JSONResponse(content={"sessions": []})

        snapshots = session_store.list_sessions(cwd=session.cwd, limit=10)
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
