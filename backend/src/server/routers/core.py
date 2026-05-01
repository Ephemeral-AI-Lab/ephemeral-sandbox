"""Core API routes — health, state, chat, config, and legacy request history."""

from __future__ import annotations

import logging
import asyncio
import json
from collections.abc import Callable
from typing import TYPE_CHECKING, Any
from collections.abc import Awaitable

from fastapi import APIRouter, HTTPException
from fastapi.responses import JSONResponse, StreamingResponse
from pydantic import BaseModel

from agents.types import AgentDefinition
from providers.provider import detect_provider, auth_status
from message.stream_events import (
    AssistantTextDelta,
    AssistantMessageComplete,
    StreamEvent,
    ThinkingDelta,
    ToolExecutionCancelled,
    ToolExecutionCompleted,
    ToolExecutionStarted,
)
from notification.events import SystemNotification
from server.protocol import BackendEvent, TranscriptItem
from tools.core.base import ExecutionMetadata

if TYPE_CHECKING:
    from server.app_factory import RuntimeConfig, RuntimeState

logger = logging.getLogger(__name__)

AgentStreamEmitter = Callable[[StreamEvent], Awaitable[None]]

# ---------------------------------------------------------------------------
# Request models
# ---------------------------------------------------------------------------


class ChatRequest(BaseModel):
    line: str
    sandbox_id: str | None = None


class ConfigRequest(BaseModel):
    model: str | None = None
    base_url: str | None = None
    api_key: str | None = None


# ---------------------------------------------------------------------------
# Ephemeral agent lifecycle — spawn, run, persist, die
# ---------------------------------------------------------------------------


async def execute_ephemeral_agent_run(
    config: RuntimeConfig,
    input_message: str,
    *,
    on_agent_event: AgentStreamEmitter,
    agent_def: AgentDefinition | None = None,
    sandbox_id: str | None = None,
    task_id: str | None = None,
    extra_tool_metadata: ExecutionMetadata | dict[str, Any] | None = None,
) -> bool:
    """Spawn an ephemeral agent, run it, persist its task run, let it die.

    Thin wrapper around :func:`engine.runtime.lifecycle.run_ephemeral_agent`
    that re-raises run errors to preserve the existing chat-route contract.
    """
    from engine.runtime.lifecycle import run_ephemeral_agent

    result = await run_ephemeral_agent(
        config,
        input_message,
        agent_def=agent_def,
        sandbox_id=sandbox_id,
        persist_agent_run=True,
        task_id=task_id,
        on_event=on_agent_event,
        extra_tool_metadata=extra_tool_metadata,
    )
    logger.info(
        "Agent %r finished (events=%d, status=%s)",
        result.agent_name,
        result.event_count,
        result.status,
    )
    if result.status == "failed" and result.error:
        raise RuntimeError(result.error)
    return True


# ---------------------------------------------------------------------------
# Router factory — receives get_runtime callable from app_factory
# ---------------------------------------------------------------------------


def create_core_router(get_runtime: Callable[[], RuntimeState]) -> APIRouter:
    """Build the core API router."""
    router = APIRouter(prefix="/api")

    @router.get("/health")
    async def health():
        return {"status": "ok", "service": "ephemeralos"}

    @router.get("/state")
    async def get_state():
        runtime = get_runtime()
        if runtime.config is None:
            raise HTTPException(status_code=503, detail="Runtime not ready")
        settings = runtime.current_settings()
        from config.model_config import try_get_active_model_kwargs

        active_kwargs = try_get_active_model_kwargs() or {}
        provider_info = detect_provider()
        app_state = {
            "model": active_kwargs.get("model", ""),
            "cwd": runtime.cwd,
            "provider": provider_info.name,
            "auth_status": "authorized",
            "base_url": active_kwargs.get("base_url") or "",
            "theme": settings.theme,
            "vim_enabled": False,
            "voice_enabled": False,
            "voice_available": False,
            "voice_reason": "",
            "fast_mode": settings.fast_mode,
            "effort": settings.effort,
            "passes": settings.passes,
            "bridge_requests": 0,
            "output_style": "verbose" if settings.verbose else "normal",
        }
        ready = BackendEvent.ready(
            tools=runtime._tool_snapshots(),
            state=app_state,
        )
        return JSONResponse(content=json.loads(ready.model_dump_json()))

    @router.post("/chat")
    async def chat(req: ChatRequest):
        runtime = get_runtime()
        if runtime.config is None:
            raise HTTPException(status_code=503, detail="Runtime not ready")

        async with runtime._busy_lock:
            if runtime.busy:
                return JSONResponse(status_code=409, content={"error": "Runtime is busy"})
            runtime.busy = True

        queue: asyncio.Queue[BackendEvent | None] = asyncio.Queue()
        runtime.set_event_queue(queue)

        async def process() -> None:
            try:
                await runtime.emit(
                    BackendEvent(
                        type="transcript_item",
                        item=TranscriptItem(role="user", text=req.line),
                    )
                )

                async def _on_system_notification(message: str) -> None:
                    await runtime.emit(
                        BackendEvent(
                            type="transcript_item",
                            item=TranscriptItem(role="system", text=message),
                        )
                    )

                def _stream_event_to_backend(event: StreamEvent) -> BackendEvent | None:
                    if isinstance(event, ThinkingDelta):
                        return BackendEvent(type="thinking_delta", message=event.text)
                    if isinstance(event, AssistantTextDelta):
                        return BackendEvent(type="assistant_delta", message=event.text)
                    if isinstance(event, AssistantMessageComplete):
                        text = event.message.text.strip()
                        return BackendEvent(
                            type="assistant_complete",
                            message=text,
                            item=TranscriptItem(role="assistant", text=text),
                        )
                    if isinstance(event, ToolExecutionStarted):
                        return BackendEvent(
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
                    if isinstance(event, ToolExecutionCompleted):
                        return BackendEvent(
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
                    if isinstance(event, ToolExecutionCancelled):
                        return BackendEvent(
                            type="tool_cancelled",
                            tool_name=event.tool_name,
                            cancel_reason=event.reason,
                            item=TranscriptItem(
                                role="tool_result",
                                text=f"[CANCELLED] {event.tool_name}: {event.reason}",
                                tool_name=event.tool_name,
                                is_error=True,
                            ),
                        )
                    return None

                async def _on_agent_event(event: StreamEvent) -> None:
                    if isinstance(event, SystemNotification):
                        await _on_system_notification(event.text)
                        return
                    backend_event = _stream_event_to_backend(event)
                    if backend_event is not None:
                        await runtime.emit(backend_event)

                from server.app_factory import (
                    complex_task_request_store,
                    harness_graph_store,
                    task_center_store,
                    task_segment_store,
                )

                if not (
                    task_center_store.is_ready
                    and complex_task_request_store.is_ready
                    and task_segment_store.is_ready
                    and harness_graph_store.is_ready
                ):
                    raise RuntimeError("TaskCenter stores are not ready.")

                from task_center.entry import start_task_center_entry_run

                entry_run = start_task_center_entry_run(
                    config=runtime.config,
                    prompt=req.line,
                    sandbox_id=req.sandbox_id,
                    on_agent_event=_on_agent_event,
                    task_store=task_center_store,
                    request_store=complex_task_request_store,
                    segment_store=task_segment_store,
                    graph_store=harness_graph_store,
                )
                await runtime.emit(
                    BackendEvent(
                        type="transcript_item",
                        item=TranscriptItem(
                            role="system",
                            text=(
                                "TaskCenter run "
                                f"{entry_run.task_center_run_id} started."
                            ),
                        ),
                    )
                )
                await entry_run.launcher.wait_for_idle()
                await runtime.emit(BackendEvent(type="line_complete"))
            except Exception as exc:
                await runtime.emit(BackendEvent(type="error", message=f"Processing error: {exc}"))
            finally:
                await queue.put(None)
                runtime.busy = False
                runtime.set_event_queue(None)

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
        runtime = get_runtime()
        if runtime.config is None:
            raise HTTPException(status_code=503, detail="Runtime not ready")

        from server.app_factory import model_store

        if not model_store.is_available:
            raise HTTPException(status_code=503, detail="Model store not ready")

        active = model_store.get_active(redact=False)
        if active is None:
            raise HTTPException(
                status_code=400,
                detail="No active model registration to update",
            )

        kwargs = dict(active.get("kwargs") or {})
        changed = False
        if req.model is not None:
            kwargs["model"] = req.model
            changed = True
        if req.base_url is not None:
            kwargs["base_url"] = req.base_url
            changed = True
        if req.api_key is not None:
            kwargs["api_key"] = req.api_key
            changed = True

        if not changed:
            return JSONResponse(content={"changed": False})

        model_store.register(
            key=active["key"],
            label=active.get("label") or active["key"],
            class_path=active.get("class_path") or "",
            kwargs=kwargs,
            activate=True,
        )

        provider = detect_provider()
        return JSONResponse(
            content={
                "changed": True,
                "model": kwargs.get("model", ""),
                "provider": provider.name,
                "auth_status": auth_status(),
                "base_url": kwargs.get("base_url") or "",
            }
        )

    @router.get("/task-center-requests")
    async def list_task_center_requests():
        runtime = get_runtime()
        if runtime.config is None:
            raise HTTPException(status_code=503, detail="Runtime not ready")
        from server.app_factory import task_center_store
        import time as _time

        if task_center_store._session_factory is None:
            return JSONResponse(content={"task_center_requests": []})

        snapshots = task_center_store.list_requests(cwd=runtime.cwd, limit=10)
        options = []
        for request in snapshots:
            ts_value = request.get("created_at")
            if isinstance(ts_value, str):
                from datetime import datetime

                ts = datetime.fromisoformat(ts_value).strftime("%m/%d %H:%M")
            else:
                ts = _time.strftime("%m/%d %H:%M", _time.localtime(0))
            summary = request.get("request_prompt", "")[:50] or "(no prompt)"
            options.append(
                {
                    "value": request["id"],
                    "label": f"{ts}  {summary}",
                }
            )
        return JSONResponse(content={"task_center_requests": options})

    return router
