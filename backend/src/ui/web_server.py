"""FastAPI-based web server for the EphemeralOS web frontend.

Thin app factory that assembles routers and manages the session lifecycle.
Route implementations live in ``ephemeralos.ui.routers.*``.
"""

from __future__ import annotations

import asyncio
import logging
import mimetypes
from contextlib import asynccontextmanager
from pathlib import Path
from typing import TYPE_CHECKING
from uuid import uuid4

if TYPE_CHECKING:
    from ephemeralos.services.agent_builder.builder import AgentBuilderService

from dotenv import load_dotenv
from fastapi import FastAPI
from fastapi.responses import FileResponse, JSONResponse

load_dotenv()

from ephemeralos.bridge import get_bridge_manager
from ephemeralos.config import load_settings
from ephemeralos.db.engine import initialize_db
from ephemeralos.db.stores import AgentDefinitionStore, AgentRunStore, ModelStore, SessionStore, UsageStore
from ephemeralos.ui.protocol import BackendEvent, BackendHostConfig, ToolkitSnapshot
from ephemeralos.ui.runtime import (
    RuntimeBundle,
    build_runtime,
    close_runtime,
    start_runtime,
)
from ephemeralos.ui.routers.agents import create_agents_router
from ephemeralos.ui.routers.core import create_core_router
from ephemeralos.ui.routers.persistence import create_persistence_router
from ephemeralos.ui.routers.sandboxes import create_sandbox_router

logger = logging.getLogger(__name__)

_STATIC_DIR = Path(__file__).resolve().parent.parent.parent.parent / "frontend" / "web" / "dist"


# ---------------------------------------------------------------------------
# Session state — single active session per server
# ---------------------------------------------------------------------------


class SessionState:
    """Manages the single active session and its event routing."""

    def __init__(self) -> None:
        self.bundle: RuntimeBundle | None = None
        self.busy = False
        self._event_queue: asyncio.Queue[BackendEvent | None] | None = None
        self._permission_futures: dict[str, asyncio.Future[bool]] = {}
        self._question_futures: dict[str, asyncio.Future[str]] = {}

    async def initialize(self, config: BackendHostConfig) -> None:
        self.bundle = await build_runtime(
            model=config.model,
            base_url=config.base_url,
            system_prompt=config.system_prompt,
            api_key=config.api_key,
            api_format=config.api_format,
            api_client=config.api_client,
            restore_messages=config.restore_messages,
            permission_prompt=self._ask_permission,
            ask_user_prompt=self._ask_question,
        )
        await start_runtime(self.bundle)

    async def close(self) -> None:
        if self.bundle:
            await close_runtime(self.bundle)

    async def emit(self, event: BackendEvent) -> None:
        """Push an event to the current SSE stream."""
        if self._event_queue is not None:
            await self._event_queue.put(event)

    def set_event_queue(self, queue: asyncio.Queue[BackendEvent | None] | None) -> None:
        self._event_queue = queue

    async def _ask_permission(self, tool_name: str, reason: str) -> bool:
        request_id = uuid4().hex
        future: asyncio.Future[bool] = asyncio.get_running_loop().create_future()
        self._permission_futures[request_id] = future
        await self.emit(
            BackendEvent(
                type="modal_request",
                modal={
                    "kind": "permission",
                    "request_id": request_id,
                    "tool_name": tool_name,
                    "reason": reason,
                },
            )
        )
        try:
            return await future
        finally:
            self._permission_futures.pop(request_id, None)

    async def _ask_question(self, question: str) -> str:
        request_id = uuid4().hex
        future: asyncio.Future[str] = asyncio.get_running_loop().create_future()
        self._question_futures[request_id] = future
        await self.emit(
            BackendEvent(
                type="modal_request",
                modal={
                    "kind": "question",
                    "request_id": request_id,
                    "question": question,
                },
            )
        )
        try:
            return await future
        finally:
            self._question_futures.pop(request_id, None)

    def resolve_permission(self, request_id: str, allowed: bool) -> bool:
        if request_id in self._permission_futures:
            self._permission_futures[request_id].set_result(allowed)
            return True
        return False

    def resolve_question(self, request_id: str, answer: str) -> bool:
        if request_id in self._question_futures:
            self._question_futures[request_id].set_result(answer)
            return True
        return False

    def status_snapshot(self) -> BackendEvent:
        assert self.bundle is not None
        return BackendEvent.status_snapshot(
            state=self.bundle.app_state.get(),
            mcp_servers=self.bundle.mcp_manager.list_statuses(),
            bridge_sessions=get_bridge_manager().list_sessions(),
            toolkits=self._toolkit_snapshots(),
        )

    def _toolkit_snapshots(self) -> list[ToolkitSnapshot]:
        assert self.bundle is not None
        return [
            ToolkitSnapshot(
                name=tk.name,
                description=tk.description,
                tools=tk.tool_names(),
            )
            for tk in self.bundle.tool_registry.list_toolkits()
        ]


# ---------------------------------------------------------------------------
# App factory
# ---------------------------------------------------------------------------

_session: SessionState | None = None
_builder_service: "AgentBuilderService | None" = None

# Database stores — module-level singletons, initialised during lifespan
session_store = SessionStore()
agent_run_store = AgentRunStore()
usage_store = UsageStore()
model_store = ModelStore()
agent_definition_store = AgentDefinitionStore()


def create_app(config: BackendHostConfig) -> FastAPI:
    """Create the FastAPI application with session lifecycle."""

    @asynccontextmanager
    async def lifespan(app: FastAPI):
        global _session, _builder_service
        _session = SessionState()
        await _session.initialize(config)

        # Register built-in agent definitions into the runtime registry
        from ephemeralos.coordinator.agent_definitions import initialize_builtin_definitions

        initialize_builtin_definitions()

        # Initialise PostgreSQL persistence (opt-in via database config)
        settings = load_settings()
        sf = initialize_db(settings.database)
        if sf is not None:
            session_store.initialize(sf)
            agent_run_store.initialize(sf)
            usage_store.initialize(sf)
            model_store.initialize(sf)
            agent_definition_store.initialize(sf)
            # Seed models from registry.json on first boot
            registry_path = Path(__file__).resolve().parent.parent.parent.parent / "models" / "registry.json"
            model_store.seed_from_json(str(registry_path))

            # Bootstrap agent builder service and load DB agents
            from ephemeralos.services.agent_builder import (
                AgentBuilderService,
                AgentDefinitionValidator,
            )

            tool_reg = _session.bundle.tool_registry if _session.bundle else None
            validator = AgentDefinitionValidator(tool_reg)
            _builder_service = AgentBuilderService(agent_definition_store, validator)
            db_agents = _builder_service.load_all_from_db()
            logger.info("Loaded %d user agents from DB", len(db_agents))
            logger.info("Database stores initialised")
        else:
            logger.info("Running without database — file-based persistence only")

        yield
        await _session.close()
        _session = None
        _builder_service = None

    app = FastAPI(title="EphemeralOS", lifespan=lifespan)

    # Register routers
    app.include_router(create_core_router(_get_session))
    app.include_router(
        create_persistence_router(
            _get_session, session_store, agent_run_store, usage_store, model_store
        )
    )
    app.include_router(create_sandbox_router())
    app.include_router(
        create_agents_router(
            get_builder_service=lambda: _builder_service,
            get_tool_registry=lambda: _session.bundle.tool_registry if _session and _session.bundle else None,
        )
    )

    # Static file serving (SPA fallback) — must be last
    @app.get("/{full_path:path}")
    async def serve_static(full_path: str):
        static_dir = _STATIC_DIR
        if not static_dir.exists():
            return JSONResponse(
                status_code=404,
                content={"error": "Frontend not built. Run: cd frontend/web && npm run build"},
            )
        candidate = (static_dir / full_path).resolve()
        try:
            candidate.relative_to(static_dir)
        except ValueError:
            return JSONResponse(status_code=403, content={"error": "Forbidden"})
        if candidate.is_file():
            mime, _ = mimetypes.guess_type(str(candidate))
            return FileResponse(candidate, media_type=mime or "application/octet-stream")
        index = static_dir / "index.html"
        if index.exists():
            return FileResponse(index, media_type="text/html")
        return JSONResponse(status_code=404, content={"error": "Not Found"})

    return app


def _get_session() -> SessionState:
    assert _session is not None, "Session not initialized"
    return _session


# ---------------------------------------------------------------------------
# Server entry point
# ---------------------------------------------------------------------------


class WebServer:
    """HTTP + SSE server for the EphemeralOS web frontend."""

    def __init__(
        self,
        host: str = "127.0.0.1",
        port: int = 8420,
        *,
        model: str | None = None,
        base_url: str | None = None,
        system_prompt: str | None = None,
        api_key: str | None = None,
        api_format: str | None = None,
        restore_messages: list[dict] | None = None,
    ) -> None:
        self.host = host
        self.port = port
        self._config = BackendHostConfig(
            model=model,
            base_url=base_url,
            system_prompt=system_prompt,
            api_key=api_key,
            api_format=api_format,
            restore_messages=restore_messages,
        )

    async def start(self) -> None:
        """Start the FastAPI server and run until interrupted."""
        import uvicorn

        app = create_app(self._config)
        config = uvicorn.Config(
            app,
            host=self.host,
            port=self.port,
            log_level="info",
        )
        server = uvicorn.Server(config)
        await server.serve()


__all__ = ["WebServer", "create_app", "SessionState"]
