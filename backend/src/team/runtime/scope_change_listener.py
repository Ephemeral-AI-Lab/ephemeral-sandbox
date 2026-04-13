"""ScopeChangeListener — single LISTEN connection with in-process fan-out.

One instance per TeamRun. Executors subscribe/unsubscribe as tasks
start/finish. Notifications from PostgreSQL are routed to per-executor
ScopeChangeBuffers based on scope_paths filtering.

Uses a dedicated async connection outside the pool to avoid tying up a
pooled connection for the lifetime of the run. The connection runs
LISTEN and polls for notifications in a background asyncio task.

See Section 14.7 of the coordination redesign doc.
"""

from __future__ import annotations

import asyncio
import json
import logging
import re
from dataclasses import dataclass
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from sqlalchemy.ext.asyncio import AsyncEngine

from team.runtime.scope_change_buffer import ScopeChangeBuffer

logger = logging.getLogger(__name__)


def _build_channel_name(run_id: str) -> str:
    """Return a PostgreSQL-safe channel name derived from *run_id*."""
    sanitized = re.sub(r"[^a-zA-Z0-9_]", "_", str(run_id or ""))
    return f"scope_change_{sanitized or 'run'}"


@dataclass
class _Subscription:
    scope_paths: list[str]
    buffer: ScopeChangeBuffer
    agent_run_id: str


class ScopeChangeListener:
    """Single shared LISTEN connection with in-process fan-out.

    One instance per TeamRun. Executors register/unregister as tasks
    start/finish. The listener filters notifications by scope and
    routes them to per-executor ScopeChangeBuffers.

    Notifications are buffered in each ScopeChangeBuffer and flushed
    at the top of each query loop turn — no timer-based flush loop.
    """

    def __init__(self, engine: "AsyncEngine", run_id: str) -> None:
        self._engine = engine
        self._run_id = run_id
        self._channel = _build_channel_name(run_id)
        self._subscribers: dict[str, _Subscription] = {}
        self._conn = None
        self._driver_conn = None
        self._listen_task: asyncio.Task | None = None
        self._running = False
        self._db_listen_active = False

    async def start(self) -> None:
        """Attach a LISTEN listener on a dedicated connection when possible."""
        self._running = True
        try:
            self._conn = await self._engine.connect()
            raw = await self._conn.get_raw_connection()
            self._driver_conn = getattr(raw, "driver_connection", None)
            if self._driver_conn is None:
                self._driver_conn = getattr(raw, "dbapi_connection", None)
            if self._driver_conn is None:
                raise RuntimeError("raw connection does not expose a driver connection")

            # Use the async SQLAlchemy connection for LISTEN setup so greenlet
            # handoff stays inside SQLAlchemy's supported async boundary.
            await self._conn.exec_driver_sql(f"LISTEN {self._channel}")
            self._db_listen_active = True
            self._running = True
            self._listen_task = asyncio.create_task(
                self._poll_loop(self._driver_conn),
                name=f"scope_listener_{self._run_id}",
            )
            logger.info("ScopeChangeListener started on channel %s", self._channel)
        except Exception:
            logger.warning(
                "ScopeChangeListener failed to attach PostgreSQL LISTEN; "
                "continuing with in-process fan-out only.",
                exc_info=True,
            )
            self._db_listen_active = False
            if self._conn is not None:
                try:
                    await self._conn.close()
                except Exception:
                    pass
            self._conn = None
            self._driver_conn = None

    async def _poll_loop(self, dbapi_conn) -> None:  # type: ignore[no-untyped-def]
        """Poll for NOTIFY events and route to subscribers.

        Uses psycopg's async notifies() generator when available,
        falls back to a polling loop with short sleep.
        """
        try:
            # psycopg3 async connection exposes .notifies() async generator
            notifies = getattr(dbapi_conn, "notifies", None)
            if callable(notifies):
                async for notify in notifies():
                    if not self._running:
                        break
                    self._route_notification(notify.payload)
            else:
                # Fallback: poll with short sleep (for psycopg2 or other drivers)
                while self._running:
                    await asyncio.sleep(0.5)
                    if hasattr(dbapi_conn, "poll"):
                        dbapi_conn.poll()
                    while dbapi_conn.notifies:
                        notify = dbapi_conn.notifies.pop(0)
                        self._route_notification(notify.payload)
        except asyncio.CancelledError:
            pass
        except Exception:
            if self._running:
                logger.warning("ScopeChangeListener poll loop error", exc_info=True)
        finally:
            self._db_listen_active = False

    def _route_notification(self, payload: str) -> None:
        """Parse a NOTIFY payload and route to matching subscribers."""
        try:
            change = json.loads(payload)
        except (json.JSONDecodeError, TypeError):
            logger.debug("Ignoring malformed NOTIFY payload: %s", payload[:100])
            return

        self._route_change(change)

    def _route_change(self, change: dict[str, object]) -> None:
        """Route a parsed scope-change payload to matching subscribers."""
        file_path = change.get("file_path", "")
        change_agent_run_id = change.get("agent_run_id", "")
        if not isinstance(file_path, str) or not file_path:
            return
        change_agent_run_id = str(change_agent_run_id or "")

        routed_count = 0
        for sub in self._subscribers.values():
            # Don't notify agent about its own edits
            if change_agent_run_id and change_agent_run_id == sub.agent_run_id:
                continue
            # Only notify if file is in agent's scope
            if any(file_path.startswith(p.rstrip("/")) for p in sub.scope_paths):
                sub.buffer.buffer(
                    {
                        "file_path": file_path,
                        "agent_id": str(change.get("agent_id", "") or ""),
                        "agent_run_id": change_agent_run_id,
                        "edit_type": str(change.get("edit_type", "") or "edit"),
                    }
                )
                routed_count += 1
        if routed_count:
            logger.info(
                "[scope_listener] routed file=%s editor=%s to %d subscriber(s)",
                file_path,
                change_agent_run_id[:12] if change_agent_run_id else "unknown",
                routed_count,
            )

    async def _fire_pg_notify(self, payload_json: str) -> None:
        """Fire a PostgreSQL NOTIFY on a short-lived pooled connection.

        Uses a separate connection from the LISTEN connection to avoid
        interfering with the poll loop. The channel name is pre-sanitized
        at construction time.
        """
        try:
            from sqlalchemy import text

            async with self._engine.connect() as conn:
                await conn.execute(
                    text("SELECT pg_notify(:channel, :payload)"),
                    {"channel": self._channel, "payload": payload_json},
                )
                await conn.commit()
        except Exception:
            logger.debug(
                "pg_notify failed for channel %s", self._channel, exc_info=True
            )

    def publish_change(
        self,
        *,
        file_path: str,
        agent_id: str = "",
        agent_run_id: str = "",
        edit_type: str = "edit",
    ) -> None:
        """Fan out a scope change in-process and via PostgreSQL NOTIFY."""
        if not self._running:
            return
        change = {
            "file_path": file_path,
            "agent_id": agent_id,
            "agent_run_id": agent_run_id,
            "edit_type": edit_type,
        }
        # In-process fan-out to subscribed executors.
        self._route_change(change)
        # Cross-process: fire pg_notify so other processes' LISTEN picks it up.
        if self._db_listen_active:
            try:
                loop = asyncio.get_running_loop()
                loop.create_task(
                    self._fire_pg_notify(json.dumps(change)),
                    name=f"pg_notify_{self._channel}",
                )
            except RuntimeError:
                pass  # No running event loop — in-process fan-out is sufficient

    def subscribe(
        self,
        agent_run_id: str,
        scope_paths: list[str],
        buffer: ScopeChangeBuffer,
    ) -> None:
        """Register an executor's buffer for scope notifications."""
        self._subscribers[agent_run_id] = _Subscription(
            scope_paths=scope_paths,
            buffer=buffer,
            agent_run_id=agent_run_id,
        )
        logger.info(
            "[scope_listener] subscribe agent_run=%s scopes=%s total_subs=%d",
            agent_run_id[:12],
            scope_paths,
            len(self._subscribers),
        )

    def unsubscribe(self, agent_run_id: str) -> None:
        """Unregister an executor's buffer."""
        removed = self._subscribers.pop(agent_run_id, None)
        if removed is not None:
            logger.info(
                "[scope_listener] unsubscribe agent_run=%s remaining_subs=%d",
                agent_run_id[:12],
                len(self._subscribers),
            )

    async def stop(self) -> None:
        """Stop listening and close the dedicated connection."""
        self._running = False
        self._db_listen_active = False
        if self._listen_task is not None:
            self._listen_task.cancel()
            try:
                await self._listen_task
            except (asyncio.CancelledError, Exception):
                pass
            self._listen_task = None
        if self._conn is not None:
            try:
                await self._conn.close()
            except Exception:
                pass
            self._conn = None
        self._driver_conn = None
        self._subscribers.clear()
        logger.info("ScopeChangeListener stopped on channel %s", self._channel)

    @property
    def is_running(self) -> bool:
        return self._running
