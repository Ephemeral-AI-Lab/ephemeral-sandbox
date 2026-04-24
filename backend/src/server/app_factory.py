"""FastAPI-based web server for the EphemeralOS web frontend.

Thin app factory that assembles routers and manages the session lifecycle.
Route implementations live in ``server.routers.*``.
"""

# ruff: noqa: E402

from __future__ import annotations

import asyncio
import logging
import mimetypes
from contextlib import asynccontextmanager
from dataclasses import dataclass, field
from pathlib import Path

from dotenv import load_dotenv
from fastapi import FastAPI
from fastapi.responses import FileResponse, JSONResponse

load_dotenv()

from config import Settings, load_settings
from db.engine import get_session_factory, initialize_db
from db.stores import AgentRunStore, ModelStore, SessionStore, UsageStore
from server.protocol import BackendEvent, BackendHostConfig, ToolSnapshot
from server.logging_config import configure_runtime_logging
from providers.types import SupportsStreamingMessages
from tools import ToolRegistry
from tools.core.catalog import collect_tool_catalog
from providers.api import create_models_router
from agents.api.router import create_agents_router
from server.routers.core import create_core_router
from server.routers.persistence import create_persistence_router
from server.routers.sandboxes import create_sandbox_router
from server.routers.code_intelligence import router as ci_router
from skills.api.router import create_skills_router

logger = logging.getLogger(__name__)

_STATIC_DIR = Path(__file__).resolve().parent.parent.parent.parent / "frontend" / "web" / "dist"


# ---------------------------------------------------------------------------
# SessionConfig — durable configuration that survives across requests
# ---------------------------------------------------------------------------


@dataclass
class SessionConfig:
    """Durable session configuration — persists across ephemeral agents."""

    cwd: str
    session_id: str
    system_prompt_override: str | None = None
    # If an external API client was injected, store it for reuse
    external_api_client: SupportsStreamingMessages | None = None
    # Messages to restore on first spawn (from session restore)
    _initial_messages: list[dict] | None = field(default=None, repr=False)

    def resolve_settings(self) -> Settings:
        """Load settings and apply any CLI overrides."""
        return load_settings().merge_cli_overrides(
            system_prompt=self.system_prompt_override,
        )


def build_session_config(
    *,
    system_prompt: str | None = None,
    api_client: SupportsStreamingMessages | None = None,
    restore_messages: list[dict] | None = None,
) -> SessionConfig:
    """Build durable session config. Called once at server startup."""
    from uuid import uuid4

    return SessionConfig(
        cwd=str(Path.cwd()),
        session_id=uuid4().hex[:12],
        system_prompt_override=system_prompt,
        external_api_client=api_client,
        _initial_messages=restore_messages,
    )


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
            system_prompt=host_config.system_prompt,
            api_client=host_config.api_client,
            restore_messages=host_config.restore_messages,
        )
        # Keep a tool registry for config-time queries.
        from tools import create_default_tool_registry

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

    def current_settings(self) -> Settings:
        if self.config is None:
            raise RuntimeError("SessionState not initialised")
        return self.config.resolve_settings()

    async def emit(self, event: BackendEvent) -> None:
        """Push an event to the current SSE stream."""
        if self._event_queue is not None:
            await self._event_queue.put(event)

    def set_event_queue(self, queue: asyncio.Queue[BackendEvent | None] | None) -> None:
        self._event_queue = queue

    def _tool_snapshots(self) -> list[ToolSnapshot]:
        if self._tool_registry is None:
            raise RuntimeError("Tool registry not initialised")
        return [
            ToolSnapshot(
                name=entry.name,
                description=entry.description,
            )
            for entry in collect_tool_catalog(
                self._tool_registry,
                include_runtime_tools=True,
                cwd=self.cwd,
            )
        ]


# ---------------------------------------------------------------------------
# App factory
# ---------------------------------------------------------------------------

_session: SessionState | None = None

# Database stores — module-level singletons, initialised during lifespan
session_store = SessionStore()
agent_run_store = AgentRunStore()
usage_store = UsageStore()
model_store = ModelStore()


def _model_registry_path() -> Path:
    return Path(__file__).resolve().parent.parent.parent.parent / "models" / "registry.json"


def ensure_runtime_stores_ready(settings: Settings | None = None):
    """Initialise the runtime stores needed by non-server entrypoints.

    Benchmarks, CLI helpers, and other direct runtime paths do not pass
    through the FastAPI lifespan hook, so they need a shared bootstrap
    path for DB-backed model resolution and run persistence.
    """
    settings = settings or load_settings()
    sf = get_session_factory() or initialize_db(settings.database)
    if sf is None:
        logger.info("Running without database — file-based persistence only")
        return None

    if not session_store.is_ready:
        session_store.initialize(sf)
    if not agent_run_store.is_ready:
        agent_run_store.initialize(sf)
    if not usage_store.is_ready:
        usage_store.initialize(sf)
    if not model_store.is_available:
        model_store.initialize(sf)

    model_store.seed_from_json(str(_model_registry_path()))
    return sf


def _initialize_runtime_stores() -> None:
    """Initialize DB-backed runtime stores when a database is configured."""
    settings = load_settings()
    sf = ensure_runtime_stores_ready(settings)
    if sf is None:
        return
    logger.info("Runtime database stores initialised")


def create_app(config: BackendHostConfig) -> FastAPI:
    """Create the FastAPI application with session lifecycle."""

    @asynccontextmanager
    async def lifespan(app: FastAPI):
        global _session
        _session = SessionState()
        await _session.initialize(config)
        configure_runtime_logging(verbose=_session.current_settings().verbose)

        from team.definitions import register_all as register_team_builtins

        register_team_builtins()
        _initialize_runtime_stores()

        yield
        # Close any externally-injected API client to avoid "Event loop is
        # closed" errors from orphaned httpx transports during GC.
        if _session and _session.config and _session.config.external_api_client:
            client = _session.config.external_api_client
            if hasattr(client, "aclose"):
                await client.aclose()
        _session = None

    app = FastAPI(title="EphemeralOS", lifespan=lifespan)

    # Register routers
    app.include_router(create_core_router(_get_session))
    app.include_router(create_persistence_router(session_store, agent_run_store, usage_store))
    app.include_router(create_sandbox_router())
    app.include_router(ci_router)
    app.include_router(create_models_router(model_store))
    app.include_router(
        create_agents_router(
            get_tool_registry=lambda: _session._tool_registry if _session else None,
        )
    )

    app.include_router(create_skills_router())

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
        system_prompt: str | None = None,
        restore_messages: list[dict] | None = None,
    ) -> None:
        self.host = host
        self.port = port
        self._config = BackendHostConfig(
            system_prompt=system_prompt,
            restore_messages=restore_messages,
        )

    async def start(self, *, reload: bool = False) -> None:
        """Start the FastAPI server and run until interrupted."""
        import uvicorn

        if reload:
            # uvicorn reload requires an import string, not an app instance
            # Use absolute path so reload works regardless of CWD
            src_dir = str(Path(__file__).resolve().parents[1])
            config = uvicorn.Config(
                "server.app_factory:create_default_app",
                host=self.host,
                port=self.port,
                log_level="info",
                reload=True,
                reload_dirs=[src_dir],
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


__all__ = ["SessionState", "WebServer", "create_app", "create_default_app"]
