"""Isolated workspace pipeline."""

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

from sandbox.audit.events import IsolatedWorkspaceAuditEvent
from sandbox._shared.layer_stack_port import LayerStackSnapshotPort
from sandbox._shared.clock import monotonic_now
from sandbox._shared.models import Intent, ToolCallRequest, ToolCallResult
from sandbox._shared.ordered_lock import OrderedLock
from sandbox.audit.schema import (
    IsolatedWorkspaceSection,
    build_isolated_workspace_event,
    safe_emit,
)
from sandbox.isolated_workspace._control_plane.orphan_reaper import (
    _OrphanResourceReaperMixin,
)
from sandbox.isolated_workspace._control_plane.workspace_handle_lifecycle import (
    _WorkspaceHandleLifecycleMixin,
)
from sandbox.isolated_workspace._control_plane.namespace_runtime import (
    _KernelNamespaceRuntime,
    _read_linux_memavailable_kb,
)
from sandbox.isolated_workspace._control_plane.types import (
    IsolatedWorkspaceAuditSink,
    IsolatedWorkspaceError,
    IsolatedWorkspaceHandle,
    NamespaceRuntimePort,
    PERSISTED_HANDLES_SCHEMA_VERSION,
    _PhaseTimer,
    _PipelineConfig,
    logger,
)
from sandbox.isolated_workspace.network import IsolatedNetwork, IsolatedNetworkUnavailable
from sandbox.overlay import lifecycle as overlay_lifecycle
from sandbox.overlay.handle import OverlayHandle
from sandbox.overlay.namespace_runner import run_in_namespace


class IsolatedPipeline(
    _WorkspaceHandleLifecycleMixin,
    _OrphanResourceReaperMixin,
):
    """Owns isolated workspace lifecycle, namespace runtime, capacity, TTL, and GC state.

    Audit/event divergence vs ``EphemeralPipeline``:
        ``IsolatedPipeline`` uses ``_JsonlAuditSink`` (in
        ``_control_plane.pipeline_registry``) to write
        ``sandbox_isolated_workspace_*`` events to a JSONL file.
        This is AUDIT (consumed by 20+ tier-3 tests parsing exact
        event-type strings), not runtime control flow. For runtime events,
        see ``EphemeralPipeline``'s ``event_bus`` pattern.

    Body-length divergence vs ``EphemeralPipeline.run_tool_call``:
        ``IsolatedPipeline.run_tool_call`` is intentionally short (~15
        lines) because the isolated handle is persistent — there is no
        per-call ``overlay_lifecycle.acquire`` / ``destroy`` pair to wrap.
        Honest divergence; do not force-fit the ephemeral 5-step shape
        (Phase 2.6 P1).
    """

    def __init__(
        self,
        *,
        scratch_root: Path,
        layer_stack: LayerStackSnapshotPort,
        audit: IsolatedWorkspaceAuditSink | None = None,
        config: _PipelineConfig | None = None,
        network: IsolatedNetwork | None = None,
        runtime: NamespaceRuntimePort | None = None,
        clock: Callable[[], float] = time.monotonic,
        id_factory: Callable[[], str] = lambda: uuid.uuid4().hex[:16],
        meminfo_reader: Callable[[], int] | None = None,
    ) -> None:
        self._scratch_root = Path(scratch_root)
        self._layer_stack = layer_stack
        self._audit = audit
        self._config = config or _PipelineConfig.from_env()
        self._network = network or IsolatedNetwork(rfc1918_egress=self._config.rfc1918_egress)
        self._runtime: NamespaceRuntimePort = runtime or _KernelNamespaceRuntime()
        self._clock = clock
        self._id_factory = id_factory
        self._meminfo_reader = meminfo_reader or _read_linux_memavailable_kb
        self._handles: dict[str, IsolatedWorkspaceHandle] = {}
        self._by_agent: dict[str, str] = {}
        # Phase 4 §AC9: ``OrderedLock("_map_lock")`` participates in the
        # per-task lock-order assertion under ``EOS_TEST_MODE=true``. The
        # rule is ``entry_lock`` (per-agent, in ``workspace_tool_dispatch``)
        # outer, ``_map_lock`` inner. Outside test mode the wrapper is a
        # near-zero-overhead pass-through.
        self._map_lock = OrderedLock("_map_lock")
        # Default-set: a freshly constructed pipeline (without ``initialize``)
        # is usable. ``initialize`` clears the event around startup orphan
        # recovery so concurrent ``enter`` calls block until IP-pool reconciliation
        # completes (plan §5 step 0).
        self._init_complete = asyncio.Event()
        self._init_complete.set()
        self._ttl_task: asyncio.Task[None] | None = None
        self._sampler_task: asyncio.Task[None] | None = None

    @property
    def scratch_root(self) -> Path:
        return self._scratch_root / "runtime" / "isolated-workspace"

    @property
    def persisted_handles_path(self) -> Path:
        return self.scratch_root / "manager.json"

    def _check_host_capacity(self) -> None:
        budget = self._compute_host_budget()
        required = (len(self._handles) + 1) * self._config.upperdir_bytes
        if required > budget:
            raise IsolatedWorkspaceError(
                "host_ram_pressure",
                "host RAM gate refuses new isolated workspace",
                required_bytes=required,
                budget_bytes=budget,
            )

    def _compute_host_budget(self) -> int:
        try:
            memavail_kb = self._meminfo_reader()
        except Exception:
            return 2**62
        return int(memavail_kb * 1024 * self._config.memavail_fraction)

    def _read_persisted_handles(self) -> dict[str, Any]:
        path = self.persisted_handles_path
        if not path.exists():
            return _empty_persisted_handles()
        try:
            data = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            logger.warning("isolated_workspace_handles_unreadable path=%s", path)
            return _empty_persisted_handles()
        if data.get("schema_version") != PERSISTED_HANDLES_SCHEMA_VERSION:
            logger.warning(
                "isolated_workspace_handles_schema_mismatch expected=%s found=%s",
                PERSISTED_HANDLES_SCHEMA_VERSION,
                data.get("schema_version"),
            )
            return _empty_persisted_handles()
        return data

    def get_handle(self, agent_id: str) -> IsolatedWorkspaceHandle | None:
        workspace_handle_id = self._by_agent.get(agent_id)
        return self._handles.get(workspace_handle_id) if workspace_handle_id else None

    def _require_handle(self, agent_id: str) -> IsolatedWorkspaceHandle:
        handle = self.get_handle(agent_id)
        if handle is None:
            raise IsolatedWorkspaceError(
                "no_isolated_workspace",
                "no open isolated workspace for agent",
            )
        return handle

    async def run_tool_call(self, req: ToolCallRequest) -> ToolCallResult:
        """Run one foreground tool call inside an already-open isolated workspace.

        iws handle is persistent — no per-call create/destroy.
        ``Intent.WRITE_ALLOWED`` captures ``changed_paths`` for audit ONLY;
        no OCC commit; writes drop at ``exit_isolated_workspace`` via
        ``shutil.rmtree(scratch_dir)``.
        """
        handle = self._require_handle(req.agent_id)
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
            lease_id=handle.lease_id,
            holder_pid=handle.holder_pid or None,
            run_dir=handle.upperdir.parent,
            snapshot_manifest=None,
            _release=None,
        )

    # ------------------------------------------------------------------
    # Initialization + GC
    # ------------------------------------------------------------------

    async def initialize(self) -> None:
        """One-shot setup: ensure scratch root, install network, run GC pass.

        Clears the init-complete event so concurrent ``enter`` calls block
        until startup orphan recovery finishes the IP-pool reconciliation
        (plan §5 step 0).
        """
        self._init_complete.clear()
        try:
            self.scratch_root.mkdir(parents=True, exist_ok=True)
            try:
                self._network.initialize()
            except IsolatedNetworkUnavailable as exc:
                logger.warning("isolated_network unavailable: %s", exc)
            if self._network.initialized:
                for subnet in self._network.daemon_private_routes():
                    logger.warning("isolated_workspace_rfc1918_route_visible subnet=%s", subnet)
            await self.reap_startup_orphans()
        finally:
            self._init_complete.set()
        if self._ttl_task is None and self._config.ttl_s > 0:
            self._ttl_task = asyncio.create_task(self._ttl_loop())
        # Closer C (Phase 2.6): dedicated sampler asyncio task — NOT a new
        # thread — for the ``isolated_workspace.sampled`` cadence. Gated on
        # ``enabled`` so disabling the feature creates no task at all (tests
        # rely on this for ``asyncio.all_tasks()`` assertions).
        if (
            self._sampler_task is None
            and self._config.enabled
            and self._config.sample_interval_s > 0
        ):
            self._sampler_task = asyncio.create_task(self._sampler_loop())

    async def _ttl_loop(self) -> None:
        """Background task started by ``initialize`` that runs periodic sweeps.

        Tick interval = ``max(0.5 s, min(ttl_s / 2, 30 s))`` so short TTLs
        (Tier 5's ``test_ttl_evict_and_audit`` sets ``TTL_S=1``) still see a
        sweep inside the test budget while the default 1800 s TTL stays at a
        modest 30 s heartbeat.

        Sample-lane emission has moved to :meth:`_sampler_loop` (Phase 2.6
        Closer C); this loop owns TTL eviction only.
        """
        interval = max(0.5, min(self._config.ttl_s / 2.0, 30.0))
        while True:
            try:
                await asyncio.sleep(interval)
                await self.ttl_sweep()
            except asyncio.CancelledError:
                return
            except Exception:  # pragma: no cover - background task
                logger.exception("ttl_loop tick failed")

    async def _sampler_loop(self) -> None:
        """Periodic ``isolated_workspace.sampled`` emitter task.

        Cadence = ``EOS_ISOLATED_WORKSPACE_SAMPLE_INTERVAL_S`` (default 0.5 s).
        The loop guards on ``_init_complete`` so we never sample a handle
        mid-teardown (V3 plan §Risk notes for Closer C). The task is started
        in :meth:`initialize` and cancelled in :meth:`shutdown`.
        """
        interval = max(0.01, self._config.sample_interval_s)
        while True:
            try:
                await asyncio.sleep(interval)
                if not self._init_complete.is_set():
                    continue
                for handle in list(self._handles.values()):
                    self._emit_isolated_workspace_sample(handle)
            except asyncio.CancelledError:
                return
            except Exception:  # pragma: no cover - background task
                logger.exception("isolated_workspace sampler tick failed")

    async def ttl_sweep(self) -> int:
        now = self._clock()
        evicted = 0
        async with self._map_lock:
            stale = [
                h
                for h in self._handles.values()
                if now - h.last_activity > self._config.ttl_s and h.active_calls == 0
            ]
        for handle in stale:
            try:
                stats = await self.exit(handle.agent_id)
                self._emit(
                    IsolatedWorkspaceAuditEvent.EVICTED,
                    {
                        "workspace_handle_id": handle.workspace_handle_id,
                        "reason": "ttl",
                        "lifetime_s": stats.get("lifetime_s", 0.0),
                        "upperdir_bytes_discarded": stats.get(
                            "evicted_upperdir_bytes",
                            0,
                        ),
                        "total_ms": stats.get("total_ms", 0.0),
                        "phases_ms": stats.get("phases_ms", {}),
                    },
                )
                safe_emit(
                    build_isolated_workspace_event(
                        "isolated_workspace.evicted",
                        IsolatedWorkspaceSection(
                            operation_id=handle.lease_id,
                            workspace_handle_id=handle.workspace_handle_id,
                            agent_id=handle.agent_id,
                            upperdir_bytes=int(stats.get("evicted_upperdir_bytes", 0)),
                            upperdir_cap_bytes=self._config.upperdir_bytes,
                        ),
                    ),
                    lane="critical",
                )
                evicted += 1
            except Exception:  # pragma: no cover - logging only
                logger.exception("ttl_sweep failed for %s", handle.workspace_handle_id)
        return evicted

    def _emit_isolated_workspace_sample(
        self, handle: IsolatedWorkspaceHandle
    ) -> None:
        """Best-effort daemon-ring sample tick (sample lane, no kernel calls)."""
        holder_alive: bool | None = None
        if handle.holder_pid:
            try:
                os.kill(handle.holder_pid, 0)
                holder_alive = True
            except (ProcessLookupError, PermissionError, OSError):
                holder_alive = False
        safe_emit(
            build_isolated_workspace_event(
                "isolated_workspace.sampled",
                IsolatedWorkspaceSection(
                    operation_id=handle.lease_id,
                    workspace_handle_id=handle.workspace_handle_id,
                    agent_id=handle.agent_id,
                    holder_pid=handle.holder_pid or None,
                    holder_pid_alive=holder_alive,
                    upperdir_cap_bytes=self._config.upperdir_bytes,
                    sampled_at_monotonic_s=monotonic_now(),
                ),
            ),
            lane="sample",
        )

    async def run_in_handle(
        self,
        agent_id: str,
        *,
        argv: list[str],
        stdin: bytes | None = None,
        timeout_s: float | None = None,
    ) -> dict[str, Any]:
        handle = self._require_handle(agent_id)
        timer = _PhaseTimer(self._clock)
        start = self._clock()
        handle.active_calls += 1
        handle.last_activity = self._clock()
        try:
            with timer.measure("exec"):
                # ``_runtime.run_in_handle`` shells out to setns_exec via
                # synchronous ``subprocess.run``. Run it in the default thread pool
                # so concurrent isolated-workspace tool calls can overlap.
                loop = asyncio.get_running_loop()
                exit_code, out, err = await loop.run_in_executor(
                    None,
                    lambda: self._runtime.run_in_handle(
                        handle,
                        argv=argv,
                        stdin=stdin,
                        timeout_s=timeout_s,
                    ),
                )
        finally:
            handle.active_calls = max(0, handle.active_calls - 1)
            handle.last_activity = self._clock()
        duration = self._clock() - start
        self._emit(
            IsolatedWorkspaceAuditEvent.TOOL_CALL,
            {
                "workspace_handle_id": handle.workspace_handle_id,
                "argv0": argv[0] if argv else "",
                "exit_code": exit_code,
                "duration_s": duration,
                "total_ms": timer.total_ms(),
                "phases_ms": timer.phases_ms,
            },
        )
        return {
            "success": exit_code == 0,
            "exit_code": exit_code,
            "stdout": out.decode("utf-8", errors="replace"),
            "stderr": err.decode("utf-8", errors="replace"),
            "duration_s": duration,
        }

    async def shutdown(self) -> None:
        """Tear down every active handle on daemon stop."""
        await self._exit_open_agents(grace_s=1.0)
        if self._sampler_task is not None:
            self._sampler_task.cancel()
            with contextlib.suppress(asyncio.CancelledError):
                await self._sampler_task
        if self._ttl_task is not None:
            self._ttl_task.cancel()
            with contextlib.suppress(asyncio.CancelledError):
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
        agent_ids = await self._exit_open_agents(grace_s=1.0)
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
        self._handles.clear()
        self._by_agent.clear()
        self._persist()
        return {"exited_agents": agent_ids}

    # ------------------------------------------------------------------
    # Internals
    # ------------------------------------------------------------------

    def _persist(self) -> None:
        self.scratch_root.mkdir(parents=True, exist_ok=True)
        path = self.persisted_handles_path
        tmp = path.with_suffix(".json.tmp")
        payload = {
            "schema_version": PERSISTED_HANDLES_SCHEMA_VERSION,
            "handles": [h.to_persisted() for h in self._handles.values()],
        }
        tmp.write_text(json.dumps(payload, indent=2), encoding="utf-8")
        os.replace(tmp, path)

    async def _exit_open_agents(self, *, grace_s: float) -> list[str]:
        agent_ids = list(self._by_agent.keys())
        for agent_id in agent_ids:
            with contextlib.suppress(Exception):
                await self.exit(agent_id, grace_s=grace_s)
        return agent_ids

    def _emit(self, event_type: IsolatedWorkspaceAuditEvent, payload: dict[str, Any]) -> None:
        if self._audit is None:
            return
        with contextlib.suppress(Exception):
            self._audit.emit(event_type.value, payload)


__all__ = ["IsolatedPipeline"]


def _empty_persisted_handles() -> dict[str, Any]:
    return {"schema_version": PERSISTED_HANDLES_SCHEMA_VERSION, "handles": []}
