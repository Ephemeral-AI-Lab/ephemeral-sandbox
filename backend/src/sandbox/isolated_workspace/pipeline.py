"""Isolated workspace pipeline and singleton accessors."""

from __future__ import annotations

import asyncio
import contextlib
import json
import os
import time
import uuid
from collections.abc import Callable
from pathlib import Path
from typing import Any

from sandbox._shared.models import Intent, ToolCallRequest, ToolCallResult
from sandbox.isolated_workspace._gc import _IsolatedGcMixin
from sandbox.isolated_workspace._lifecycle import _IsolatedLifecycleMixin
from sandbox.isolated_workspace._quota import _IsolatedQuotaMixin
from sandbox.isolated_workspace._runtime import _LinuxRuntime, _read_memavailable_kb
from sandbox.isolated_workspace._ttl import _IsolatedTtlMixin
from sandbox.isolated_workspace._types import (
    AuditSink,
    IsolatedWorkspaceError,
    IsolatedWorkspaceHandle,
    LayerStackPort,
    SCHEMA_VERSION,
    _ManagerConfig,
    _Runtime,
    _PhaseTimer,
    logger,
)
from sandbox.isolated_workspace.network import IsolatedNetwork, IsolatedNetworkUnavailable
from sandbox.overlay import lifecycle as overlay_lifecycle
from sandbox.overlay.handle import OverlayHandle
from sandbox.overlay.namespace_runner import run_in_namespace


class IsolatedPipeline(
    _IsolatedLifecycleMixin,
    _IsolatedGcMixin,
    _IsolatedTtlMixin,
    _IsolatedQuotaMixin,
):
    """Owns isolated workspace lifecycle, runtime, quota, TTL, and GC state.

    Audit/event divergence vs ``EphemeralPipeline``:
        ``IsolatedPipeline`` uses ``_JsonlAuditSink`` (in ``_manager.py``)
        writing ``sandbox_isolated_workspace_*`` events to a JSONL file.
        This is AUDIT (consumed by 20+ tier-3 tests parsing exact
        event-type strings), not runtime control flow. For runtime events,
        see ``EphemeralPipeline``'s ``event_bus`` pattern.

    Body-length divergence vs ``EphemeralPipeline.run_tool_call``:
        ``IsolatedPipeline.run_tool_call`` is intentionally short (~15
        lines) because the isolated handle is persistent — there is no
        per-call ``overlay_lifecycle.create`` / ``destroy`` pair to wrap.
        Honest divergence; do not force-fit the ephemeral 5-step shape
        (Phase 2.6 P1).
    """

    def __init__(
        self,
        *,
        scratch_root: Path,
        layer_stack_root: str,
        layer_stack: LayerStackPort,
        audit: AuditSink | None = None,
        config: _ManagerConfig | None = None,
        network: IsolatedNetwork | None = None,
        runtime: _Runtime | None = None,
        clock: Callable[[], float] = time.monotonic,
        id_factory: Callable[[], str] = lambda: uuid.uuid4().hex[:16],
        meminfo_reader: Callable[[], int] | None = None,
    ) -> None:
        self._scratch_root = Path(scratch_root)
        self._layer_stack_root = layer_stack_root
        self._layer_stack = layer_stack
        self._audit = audit
        self._config = config or _ManagerConfig.from_env()
        self._network = network or IsolatedNetwork(rfc1918_egress=self._config.rfc1918_egress)
        self._runtime: _Runtime = runtime or _LinuxRuntime()
        self._clock = clock
        self._id_factory = id_factory
        self._meminfo_reader = meminfo_reader or _read_memavailable_kb
        self._handles: dict[str, IsolatedWorkspaceHandle] = {}
        self._by_agent: dict[str, str] = {}
        self._map_lock = asyncio.Lock()
        # Default-set: a freshly constructed manager (without ``initialize``)
        # is usable. ``initialize`` clears the event around ``startup_gc`` so
        # concurrent ``enter`` calls block until IP-pool reconciliation
        # completes (plan §5 step 0).
        self._init_complete = asyncio.Event()
        self._init_complete.set()
        self._ttl_task: asyncio.Task[None] | None = None

    @property
    def enabled(self) -> bool:
        return self._config.enabled

    @property
    def scratch_root(self) -> Path:
        return self._scratch_root / "runtime" / "isolated-workspace"

    @property
    def manager_json_path(self) -> Path:
        return self.scratch_root / "manager.json"

    def active_count(self) -> int:
        return len(self._handles)

    def get_handle(self, agent_id: str) -> IsolatedWorkspaceHandle | None:
        handle_id = self._by_agent.get(agent_id)
        return self._handles.get(handle_id) if handle_id else None

    async def run_tool_call(self, req: ToolCallRequest) -> ToolCallResult:
        """Run one foreground tool call inside an already-open isolated workspace.

        iws handle is persistent — no per-call create/destroy.
        ``Intent.WRITE_ALLOWED`` captures ``changed_paths`` for audit ONLY;
        no OCC commit; writes drop at ``exit_isolated_workspace`` via
        ``shutil.rmtree(scratch_dir)``.
        """
        handle = self.get_handle(req.agent_id)
        if handle is None:
            raise IsolatedWorkspaceError(
                "no_isolated_workspace",
                "no open isolated workspace for agent",
            )
        overlay_handle = self._overlay_handle(handle)

        async def _runner(
            argv: list[str],
            stdin: bytes | None,
            timeout_s: float | None,
        ) -> dict[str, Any]:
            return await self.run_in_handle(
                req.agent_id,
                argv=argv,
                stdin=stdin,
                timeout_s=timeout_s,
            )

        result = await run_in_namespace(
            overlay_handle,
            req,
            isolated_runner=_runner,
        )
        result["workspace"] = "isolated"
        if req.intent == Intent.WRITE_ALLOWED:
            changes = await overlay_lifecycle.capture_changes(overlay_handle)
            result["changed_paths"] = [change.path for change in changes]
        return result

    def _overlay_handle(self, handle: IsolatedWorkspaceHandle) -> OverlayHandle:
        return OverlayHandle(
            workspace_root=handle.workspace_root,
            layer_paths=(),
            upperdir=handle.upperdir,
            workdir=handle.workdir,
            snapshot_version=handle.manifest_version,
            lease_id=handle.lease_id,
            namespace_pid=handle.root_pid or None,
            snapshot_manifest=None,
            _release=None,
        )

    # ------------------------------------------------------------------
    # Initialization + GC
    # ------------------------------------------------------------------

    async def initialize(self) -> None:
        """One-shot setup: ensure scratch root, install network, run GC pass.

        Clears the init-complete event so concurrent ``enter`` calls block
        until startup_gc finishes the IP-pool reconciliation (plan §5 step 0).
        """
        self._init_complete.clear()
        try:
            self.scratch_root.mkdir(parents=True, exist_ok=True)
            try:
                self._network.initialize()
            except IsolatedNetworkUnavailable as exc:
                logger.warning("isolated_network unavailable: %s", exc)
            if self._network.initialized:
                for subnet in self._network.reachable_rfc1918_subnets():
                    logger.warning(
                        "isolated_workspace_rfc1918_reachable subnet=%s", subnet
                    )
            await self.startup_gc()
        finally:
            self._init_complete.set()
        if self._ttl_task is None and self._config.ttl_s > 0:
            self._ttl_task = asyncio.create_task(self._ttl_loop())


    async def run_in_handle(
        self,
        agent_id: str,
        *,
        argv: list[str],
        stdin: bytes | None = None,
        timeout_s: float | None = None,
    ) -> dict[str, Any]:
        handle = self.get_handle(agent_id)
        if handle is None:
            raise IsolatedWorkspaceError(
                "no_isolated_workspace", "no open isolated workspace for agent",
            )
        timer = _PhaseTimer(self._clock)
        start = self._clock()
        with timer.measure("exec"):
            # ``_runtime.run_in_handle`` shells out to setns_exec via
            # synchronous ``subprocess.run``. Run it in the default thread pool
            # so concurrent isolated-workspace tool calls can overlap.
            loop = asyncio.get_running_loop()
            exit_code, out, err = await loop.run_in_executor(
                None,
                lambda: self._runtime.run_in_handle(
                    handle, argv=argv, stdin=stdin, timeout_s=timeout_s,
                ),
            )
        duration = self._clock() - start
        handle.last_activity = self._clock()
        self._emit("sandbox_isolated_workspace_tool_call", {
            "handle_id": handle.handle_id,
            "argv0": argv[0] if argv else "",
            "exit_code": exit_code,
            "duration_s": duration,
            "total_ms": timer.total_ms(),
            "phases_ms": timer.phases_ms,
        })
        return {
            "success": exit_code == 0,
            "exit_code": exit_code,
            "stdout": out.decode("utf-8", errors="replace"),
            "stderr": err.decode("utf-8", errors="replace"),
            "duration_s": duration,
        }


    async def shutdown(self) -> None:
        """Tear down every active handle on daemon stop."""
        agent_ids = list(self._by_agent.keys())
        for agent_id in agent_ids:
            with contextlib.suppress(Exception):
                await self.exit(agent_id, grace_s=1.0)
        if self._ttl_task is not None:
            self._ttl_task.cancel()
            with contextlib.suppress(Exception):
                await self._ttl_task

    def list_open_agents(self) -> list[str]:
        """Return every agent ID with an open handle (janitor surface)."""
        return list(self._by_agent.keys())

    async def test_reset(self) -> dict[str, Any]:
        """Janitor: exit every open handle + sweep leftover orphans.

        Test-only — the handler gate (``EOS_ISOLATED_WORKSPACE_TEST_HARNESS``)
        keeps this off the production surface. The fixture loop used to call
        ``exit`` for hardcoded ``agent-A..E``, which missed every test that
        used a non-canonical agent ID (e.g. ``agent-latency-baseline``,
        ``agent-restart-bootstrap``) — those handles, plus their
        ``unshare --fork`` ns_holders, accumulated as zombies until the
        daemon's PID/socket pressure broke later tests.
        """
        agent_ids = list(self._by_agent.keys())
        for agent_id in agent_ids:
            with contextlib.suppress(Exception):
                await self.exit(agent_id, grace_s=1.0)
        # Reap any zombies inherited from earlier daemon instances that died
        # before they could waitpid their own children. Non-blocking sweep —
        # we don't care which PIDs we collect, just that we drain them.
        with contextlib.suppress(ChildProcessError, OSError):
            while True:
                pid, _status = os.waitpid(-1, os.WNOHANG)
                if pid == 0:
                    break
        # Catch veth/scratch/cgroup left over by aborted enters that never
        # made it into ``_handles`` (their _rollback_partial may have raised).
        with contextlib.suppress(Exception):
            self._reap_orphans(live_set=set())
        return {"exited_agents": agent_ids}

    # ------------------------------------------------------------------
    # Internals
    # ------------------------------------------------------------------


    def _persist(self) -> None:
        self.scratch_root.mkdir(parents=True, exist_ok=True)
        path = self.manager_json_path
        tmp = path.with_suffix(".json.tmp")
        payload = {
            "schema_version": SCHEMA_VERSION,
            "handles": [h.to_persisted() for h in self._handles.values()],
        }
        tmp.write_text(json.dumps(payload, indent=2), encoding="utf-8")
        os.replace(tmp, path)

    def _emit(self, event_type: str, payload: dict[str, Any]) -> None:
        if self._audit is None:
            return
        with contextlib.suppress(Exception):
            self._audit.emit(event_type, payload)



# Singleton accessors (``set_pipeline`` / ``get_active_pipeline`` /
# ``require_pipeline`` / ``require_arg``) live in :mod:`._manager` post-C3.
# Re-export here so callers that still import them from ``pipeline`` keep
# working through the rollout window; new code should import from
# ``sandbox.isolated_workspace._manager`` directly.
from sandbox.isolated_workspace._manager import (  # noqa: E402
    get_active_pipeline,
    require_arg,
    require_pipeline,
    set_pipeline,
)


__all__ = [
    "IsolatedPipeline",
    "get_active_pipeline",
    "require_arg",
    "require_pipeline",
    "set_pipeline",
]
