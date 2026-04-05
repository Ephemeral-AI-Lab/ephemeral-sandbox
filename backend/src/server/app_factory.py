"""FastAPI-based web server for the EphemeralOS web frontend.

Thin app factory that assembles routers and manages the session lifecycle.
Route implementations live in ``ephemeralos.server.routers.*``.
"""

from __future__ import annotations

import asyncio
import logging
import mimetypes
from contextlib import asynccontextmanager
from pathlib import Path
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from ephemeralos.agents.builder.service import AgentBuilderService

from dotenv import load_dotenv
from fastapi import FastAPI
from fastapi.responses import FileResponse, JSONResponse

load_dotenv()

from ephemeralos.config import load_settings
from ephemeralos.db.engine import initialize_db
from ephemeralos.db.stores import AgentDefinitionStore, AgentRunStore, ModelStore, SessionStore, UsageStore
from ephemeralos.skills.db.store import SkillDefinitionStore
from ephemeralos.server.protocol import BackendEvent, BackendHostConfig, ToolkitSnapshot
from ephemeralos.server.runtime import (
    SessionConfig,
    build_session_config,
)
from ephemeralos.tools import ToolRegistry
from ephemeralos.models.api import create_models_router
from ephemeralos.agents.api.router import create_agents_router
from ephemeralos.server.routers.core import create_core_router
from ephemeralos.server.routers.persistence import create_persistence_router
from ephemeralos.server.routers.sandboxes import create_sandbox_router
from ephemeralos.server.routers.code_intelligence import router as ci_router
from ephemeralos.skills.api.router import create_skills_router

logger = logging.getLogger(__name__)

_STATIC_DIR = Path(__file__).resolve().parent.parent.parent.parent / "frontend" / "web" / "dist"


# ---------------------------------------------------------------------------
# Session state — single active session per server
# ---------------------------------------------------------------------------


class SessionState:
    """Manages the single active session and its event routing.

    The session holds only durable config — no engine or API client lives
    between requests.  Each request spawns an ephemeral agent via
    ``handle_line``.
    """

    def __init__(self) -> None:
        self.config: SessionConfig | None = None
        self.busy = False
        self._busy_lock = asyncio.Lock()
        self._event_queue: asyncio.Queue[BackendEvent | None] | None = None
        self._tool_registry: ToolRegistry | None = None

    async def initialize(self, host_config: BackendHostConfig) -> None:
        self.config = build_session_config(
            model=host_config.model,
            base_url=host_config.base_url,
            system_prompt=host_config.system_prompt,
            api_key=host_config.api_key,
            api_format=host_config.api_format,
            api_client=host_config.api_client,
            restore_messages=host_config.restore_messages,
        )
        # Keep a tool registry for config-time queries (agent builder, toolkit snapshots)
        from ephemeralos.tools import create_default_tool_registry
        self._tool_registry = create_default_tool_registry()

    @property
    def session_id(self) -> str:
        if self.config is None:
            raise RuntimeError("SessionState not initialised")
        return self.config.session_id

    @property
    def cwd(self) -> str:
        if self.config is None:
            raise RuntimeError("SessionState not initialised")
        return self.config.cwd

    @property
    def tool_registry(self) -> ToolRegistry:
        if self._tool_registry is None:
            raise RuntimeError("Tool registry not initialised")
        return self._tool_registry

    def current_settings(self) -> "Settings":
        if self.config is None:
            raise RuntimeError("SessionState not initialised")
        return self.config.resolve_settings()

    async def emit(self, event: BackendEvent) -> None:
        """Push an event to the current SSE stream."""
        if self._event_queue is not None:
            await self._event_queue.put(event)

    def set_event_queue(self, queue: asyncio.Queue[BackendEvent | None] | None) -> None:
        self._event_queue = queue

    def _toolkit_snapshots(self) -> list[ToolkitSnapshot]:
        if self._tool_registry is None:
            raise RuntimeError("Tool registry not initialised")
        # Registered toolkits (already instantiated in the default registry)
        registered_names: set[str] = set()
        snapshots: list[ToolkitSnapshot] = []
        for tk in self._tool_registry.list_toolkits():
            registered_names.add(tk.name)
            snapshots.append(
                ToolkitSnapshot(
                    name=tk.name,
                    description=tk.description,
                    tools=tk.tool_names(),
                )
            )
        # Factory-registered toolkits (e.g. daytona, ci) — instantiate with
        # a bare context just to read their tool names for the snapshot.
        from ephemeralos.tools.factory import ToolkitContext, list_factories, create_toolkit

        seen_toolkit_names = set(registered_names)
        for factory_name in list_factories():
            if factory_name in registered_names:
                continue
            try:
                tk = create_toolkit(factory_name, ToolkitContext())
                if tk.name in seen_toolkit_names:
                    continue  # skip aliases that produce duplicate toolkit names
                seen_toolkit_names.add(tk.name)
                snapshots.append(
                    ToolkitSnapshot(
                        name=tk.name,
                        description=tk.description,
                        tools=tk.tool_names(),
                    )
                )
            except Exception:
                logger.debug("Toolkit factory %r skipped (requires runtime context)", factory_name, exc_info=True)
        return snapshots


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
skill_definition_store = SkillDefinitionStore()


def create_app(config: BackendHostConfig) -> FastAPI:
    """Create the FastAPI application with session lifecycle."""

    @asynccontextmanager
    async def lifespan(app: FastAPI):
        global _session, _builder_service
        _session = SessionState()
        await _session.initialize(config)

        # Register built-in agent definitions into the runtime registry
        from ephemeralos.agents import initialize_builtin_definitions

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
            skill_definition_store.initialize(sf)
            # Seed models from registry.json on first boot
            registry_path = Path(__file__).resolve().parent.parent.parent.parent / "models" / "registry.json"
            model_store.seed_from_json(str(registry_path))

            # Bootstrap agent builder service and load DB agents
            from ephemeralos.agents.builder import (
                AgentBuilderService,
                AgentDefinitionValidator,
            )

            tool_reg = _session._tool_registry if _session else None
            validator = AgentDefinitionValidator(tool_reg)
            _builder_service = AgentBuilderService(agent_definition_store, validator)

            # Seed SuperCocoa specialists into DB on first boot (idempotent)
            from ephemeralos.agents.seed import seed_specialists_from_supercocoa

            specialist_dir = (
                Path(__file__).resolve().parent.parent.parent.parent.parent
                / "synthetic-os"
                / ".super-cocoa-agents"
                / "specialist"
            )
            if specialist_dir.exists():
                seed_created, seed_skipped = seed_specialists_from_supercocoa(
                    agent_definition_store, specialist_dir
                )
                if seed_created:
                    logger.info(
                        "Seeded %d SuperCocoa specialists (%d already existed)",
                        seed_created,
                        seed_skipped,
                    )
            else:
                logger.debug("SuperCocoa specialist dir not found: %s", specialist_dir)

            db_agents = _builder_service.load_all_from_db()
            logger.info("Loaded %d user agents from DB", len(db_agents))
            logger.info("Database stores initialised")
        else:
            logger.info("Running without database — file-based persistence only")

        yield
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
    app.include_router(ci_router)
    app.include_router(create_models_router(model_store))
    app.include_router(
        create_agents_router(
            get_builder_service=lambda: _builder_service,
            get_tool_registry=lambda: _session._tool_registry if _session else None,
        )
    )

    # Skills API — lazy-load the registry on first request
    from ephemeralos.skills.loader import load_skill_registry as _load_skills

    _skill_registry_cache: list = []  # mutable container for closure

    def _get_skill_registry():
        if not _skill_registry_cache:
            _skill_registry_cache.append(_load_skills())
        return _skill_registry_cache[0]

    app.include_router(create_skills_router(_get_skill_registry))

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
    if _session is None:
        raise RuntimeError("Session not initialized")
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

    async def start(self, *, reload: bool = False) -> None:
        """Start the FastAPI server and run until interrupted."""
        import uvicorn

        if reload:
            # uvicorn reload requires an import string, not an app instance
            config = uvicorn.Config(
                "ephemeralos.server.app_factory:create_default_app",
                host=self.host,
                port=self.port,
                log_level="info",
                reload=True,
                reload_dirs=["backend/src"],
            )
        else:
            app = create_app(self._config)
            config = uvicorn.Config(
                app,
                host=self.host,
                port=self.port,
                log_level="info",
            )
        server = uvicorn.Server(config)
        await server.serve()


def create_default_app() -> FastAPI:
    """Factory callable for uvicorn reload mode (import string target)."""
    return create_app(BackendHostConfig())


__all__ = ["WebServer", "create_app", "create_default_app", "SessionState"]
