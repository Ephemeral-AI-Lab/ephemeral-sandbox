"""Core API routes — health, state, chat, config, sessions."""

from __future__ import annotations

import logging
import asyncio
import json
from collections.abc import Callable
from typing import TYPE_CHECKING
from collections.abc import Awaitable

from fastapi import APIRouter, HTTPException
from fastapi.responses import JSONResponse, StreamingResponse
from pydantic import BaseModel

from agents.types import AgentDefinition
from providers.provider import detect_provider, auth_status
from engine import spawn_agent
from message.stream_events import (
    AssistantTextDelta,
    AssistantTurnComplete,
    StreamEvent,
    SystemNotification,
    ThinkingDelta,
    ToolExecutionCancelled,
    ToolExecutionCompleted,
    ToolExecutionStarted,
)
from server.protocol import BackendEvent, TranscriptItem
from token_tracker.runtime import persist_run_usage

if TYPE_CHECKING:
    from server.app_factory import SessionConfig, SessionState

logger = logging.getLogger(__name__)

AgentStreamEmitter = Callable[[StreamEvent], Awaitable[None]]

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


# ---------------------------------------------------------------------------
# Ephemeral agent lifecycle — spawn, run, persist, die
# ---------------------------------------------------------------------------


async def execute_ephemeral_agent_run(
    config: SessionConfig,
    input_message: str,
    *,
    on_agent_event: AgentStreamEmitter,
    agent_def: AgentDefinition | None = None,
    sandbox_id: str | None = None,
) -> bool:
    """Spawn an ephemeral agent, run it, let it die.

    1. Load conversation history from persistence
    2. Spawn a fresh agent (optionally configured by *agent_def*)
    3. Execute the user's request (full tool-call loop)
    4. Record the agent run + token usage to DB
    5. Save updated history back to DB
    6. Agent goes out of scope — dies
    """
    from agents.run_tracker import AgentRunTracker
    from server.app_factory import agent_run_store, session_store, usage_store

    db_available = agent_run_store.is_ready

    # 1. Load history + session context + full audit history from DB
    messages, session_state, full_history = session_store.load_session_state(config)

    # 2. Spawn ephemeral agent (inherits session state)
    agent = spawn_agent(
        config,
        messages,
        agent_def=agent_def,
        latest_user_prompt=input_message,
        session_state=session_state,
        sandbox_id=sandbox_id,
    )
    logger.info(
        "Spawned agent %r (model=%s, session=%s)", agent.agent_name, agent.model, config.session_id
    )

    # 3. Ensure session record exists (agent_runs FK requires it)
    if db_available:
        try:
            session_store.upsert(
                session_id=config.session_id,
                cwd=config.cwd,
                model=agent.model,
                message_count=0,
            )
        except Exception:
            logger.debug("Failed to ensure session record", exc_info=True)

    # 4. Create agent run record via the shared tracker.
    tracker = AgentRunTracker.create(
        session_id=config.session_id,
        agent_name=agent.agent_name,
        input_query=input_message,
    )
    run_id = tracker.run_id

    # Plumb the parent run id into tool_metadata so subagent dispatches
    # (and any other tool that wants attribution) can persist themselves
    # under this run as their parent.
    if run_id is not None:
        from tools.core.base import ExecutionMetadata

        if agent.query_context.tool_metadata is None:
            agent.query_context.tool_metadata = ExecutionMetadata()
        agent.query_context.tool_metadata.agent_run_id = run_id

    # 5. Run the agent
    event_count = 0
    run_error: str | None = None
    reasoning_parts: list[str] = []

    try:
        async for event in agent.run(input_message):
            event_count += 1
            if isinstance(event, ThinkingDelta):
                reasoning_parts.append(event.text)
            await on_agent_event(event)
    except Exception as exc:
        run_error = str(exc)
        raise
    finally:
        # Finish the agent run row. The tracker short-circuits when
        # persistence is unavailable, so we don't need the db_available
        # guard here.
        run_response = [
            m.model_dump(mode="json")
            for m in agent._display_messages[len(messages):]
        ]
        tracker.finish(
            status="failed" if run_error else "completed",
            response=run_response,
            display_messages=list(agent._display_messages),
            api_messages_snapshot=agent.query_context.api_messages_snapshot,
            reasoning="".join(reasoning_parts) if reasoning_parts else None,
            error=run_error,
            event_count=event_count,
        )

        if db_available:
            persist_run_usage(
                usage_store=usage_store,
                session_id=config.session_id,
                run_id=run_id,
                agent_name=agent.agent_name,
                model_id=agent.model,
                usage=agent.total_usage,
            )

    # 6. Extract new messages for the full (uncompacted) audit log
    new_messages: list[dict] = []
    engine_msgs = agent._display_messages
    for i in range(len(engine_msgs) - 1, -1, -1):
        msg = engine_msgs[i]
        if msg.role == "user" and msg.text.strip() == input_message.strip():
            new_messages = [m.model_dump(mode="json") for m in engine_msgs[i:]]
            break
    if new_messages:
        full_history.extend(new_messages)

    # 7. Save updated history to DB
    if db_available:
        try:
            session_store.upsert(
                session_id=config.session_id,
                cwd=config.cwd,
                model=agent.model,
                system_prompt=agent.query_context.system_prompt,
                messages=[m.model_dump(mode="json") for m in agent._display_messages],
                full_messages=full_history,
                usage=agent.total_usage.model_dump() if agent.total_usage else {},
                session_state=agent.query_context.session_state.to_dict()
                if agent.query_context.session_state
                else None,
                summary=input_message.strip()[:80],
                message_count=len(agent._display_messages),
            )
        except Exception:
            logger.debug("Failed to save session to DB", exc_info=True)

    logger.info(
        "Agent %r finished (events=%d, status=%s)",
        agent.agent_name,
        event_count,
        "failed" if run_error else "completed",
    )

    # 8. Agent goes out of scope — ephemeral lifecycle complete
    return True


# ---------------------------------------------------------------------------
# Router factory — receives get_session callable from web_server
# ---------------------------------------------------------------------------


def create_core_router(get_session: Callable[[], SessionState]) -> APIRouter:
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
        from config.model_config import try_get_active_model_kwargs

        active_kwargs = try_get_active_model_kwargs() or {}
        provider_info = detect_provider()
        app_state = {
            "model": active_kwargs.get("model", ""),
            "cwd": session.cwd,
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
            "bridge_sessions": 0,
            "output_style": "verbose" if settings.verbose else "normal",
        }
        ready = BackendEvent.ready(
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

                async def _on_system_notification(message: str) -> None:
                    await session.emit(
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
                    if isinstance(event, AssistantTurnComplete):
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
                        await session.emit(backend_event)

                # Resolve agent definition if requested
                agent_def = None
                if req.agent_name:
                    from agents.registry import get_definition

                    agent_def = get_definition(req.agent_name)
                    if agent_def is None:
                        await _on_system_notification(
                            f"Agent '{req.agent_name}' not found — using default"
                        )

                await execute_ephemeral_agent_run(
                    config,
                    req.line,
                    on_agent_event=_on_agent_event,
                    agent_def=agent_def,
                    sandbox_id=req.sandbox_id,
                )
                await session.emit(BackendEvent(type="line_complete"))
            except Exception as exc:
                await session.emit(BackendEvent(type="error", message=f"Processing error: {exc}"))
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

    @router.get("/sessions")
    async def list_sessions():
        session = get_session()
        if session.config is None:
            raise HTTPException(status_code=503, detail="Session not ready")
        from server.app_factory import session_store
        import time as _time

        if session_store._session_factory is None:
            return JSONResponse(content={"sessions": []})

        snapshots = session_store.list_sessions(cwd=session.cwd, limit=10)
        options = []
        for s in snapshots:
            ts = _time.strftime("%m/%d %H:%M", _time.localtime(s["created_at"]))
            summary = s.get("summary", "")[:50] or "(no summary)"
            options.append(
                {
                    "value": s["session_id"],
                    "label": f"{ts}  {s['message_count']}msg  {summary}",
                }
            )
        return JSONResponse(content={"sessions": options})

    return router
