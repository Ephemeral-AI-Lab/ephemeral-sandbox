"""IsolatedWorkspaceManager: pinned per-agent {net, pid, mnt, user} sandbox.

Distinct handle class + distinct exit path are the structural separation from
OCC: ``IsolatedWorkspaceHandle`` is NOT a subclass of
``OperationOverlayHandle`` and has no ``publish_*`` callable. ``exit`` only
discards the tmpfs upperdir + releases the lease; OCC is unreachable from this
module's import graph (verified by ``test_isolated_workspace_ops_import_fence``).

The manager itself does not import ``sandbox.occ.*`` or
``sandbox.daemon.service.sandbox_overlay``; lease/snapshot calls go through
``sandbox.daemon.workspace_server`` (which is layer-stack-only). The actual
overlay mount mechanics are reused from
:func:`sandbox.execution.overlay.kernel_mount.mount_overlay` via the setns
helper subprocess, so this feature shares one source of truth for mount(2)
sequencing with the rest of the sandbox.

Runtime
-------
The daemon only runs inside the sweevo Docker container (Linux). Kernel-
touching operations (ns holder spawn, overlay mount, veth wiring, cgroup
freezer) are delegated to ``_runtime`` hooks that ship a Linux-specific
implementation; tests substitute fakes through the same hook seam.
"""

from __future__ import annotations

import asyncio
import contextlib
import ipaddress
import json
import logging
import os
import shutil
import signal
import subprocess
import sys
import time
import uuid
from collections.abc import Callable
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Literal, Protocol

from sandbox.isolated_workspace.network import (
    IsolatedNetwork,
    IsolatedNetworkUnavailable,
    VethPair,
)

logger = logging.getLogger("sandbox.isolated_workspace.manager")

SCHEMA_VERSION = 1
HANDLE_PREFIX = "eos-iws-"
CGROUP_ROOT = Path("/sys/fs/cgroup")
DEFAULT_WORKSPACE_ROOT = "/testbed"

# PLAN §14: SUBSET-COVER invariant floor.
# sum(phases_ms.values()) <= total_ms + max(_PHASE_TIMER_OVERHEAD_BUDGET_MS,
# 0.05 * total_ms). Exposed for tests; do NOT raise without reviewing
# `assert_subset_cover` callers.
_PHASE_TIMER_OVERHEAD_BUDGET_MS = 2.0


HandleStatus = Literal["active", "exiting", "stopped", "reaping"]


# Test-only failure-injection knobs (PLAN §9.3). The env vars are read at the
# phase boundary every time so tests can change them between enter() calls
# without restarting the daemon. Production keeps these unset; the branches
# are dead code at runtime.
_TEST_HANG_AT_ENV = "EOS_ISOLATED_WORKSPACE_TEST_HANG_AT"
_TEST_FAIL_AT_ENV = "EOS_ISOLATED_WORKSPACE_TEST_FAIL_AT"
_TEST_HOLDER_CRASH_ENV = "EOS_ISOLATED_WORKSPACE_TEST_HOLDER_CRASH"
_TEST_PHASE_DELAY_ENV = "EOS_ISOLATED_WORKSPACE_TEST_PHASE_DELAY"


def _maybe_inject_failure(phase: str) -> None:
    """Raise the configured test failure for ``phase`` if any knob points here.

    Two knobs are supported:

    * ``EOS_ISOLATED_WORKSPACE_TEST_HANG_AT=<phase>`` — surface as a
      ``setup_timeout`` error so the rollback path runs (matches what a real
      hung kernel call would look like once the timeout fired).
    * ``EOS_ISOLATED_WORKSPACE_TEST_FAIL_AT=<phase>`` — surface as a
      generic ``setup_failed`` error.

    Both branches share the same rollback contract (lease released, partial
    state torn down) so a single helper keeps the call sites a one-liner.
    """
    hang_at = os.environ.get(_TEST_HANG_AT_ENV, "").strip()
    if hang_at == phase:
        raise IsolatedWorkspaceError(
            "setup_timeout",
            f"test-only setup_timeout injected at {phase}",
            failed_step=phase,
        )
    fail_at = os.environ.get(_TEST_FAIL_AT_ENV, "").strip()
    if fail_at == phase:
        raise IsolatedWorkspaceError(
            "setup_failed",
            f"test-only setup_failed injected at {phase}",
            failed_step=phase,
        )
    # Tier 9 latency-regression knob: ``<phase>:<ms>`` (comma-separated to
    # delay multiple phases in one run). Sleeps synchronously inside the
    # ``with timer.measure(phase)`` block so the injected ms is reflected in
    # the audit ``phases_ms[<phase>]`` value.
    delays = os.environ.get(_TEST_PHASE_DELAY_ENV, "").strip()
    if delays:
        for spec in delays.split(","):
            entry = spec.strip()
            if not entry or ":" not in entry:
                continue
            target_phase, _, ms_text = entry.partition(":")
            if target_phase.strip() != phase:
                continue
            try:
                delay_ms = float(ms_text.rstrip("ms").strip())
            except ValueError:
                continue
            time.sleep(max(0.0, delay_ms) / 1000.0)


class IsolatedWorkspaceError(Exception):
    """Base class for isolated-workspace lifecycle errors.

    ``kind`` becomes the wire error kind on the daemon RPC response.
    """

    def __init__(self, kind: str, message: str, **details: Any) -> None:
        super().__init__(message)
        self.kind = kind
        self.details = details


class LayerSnapshotLike(Protocol):
    lease_id: str
    manifest_version: int
    root_hash: str
    layer_paths: tuple[str, ...] | None


class LayerStackPort(Protocol):
    """The subset of ``workspace_server`` calls the manager needs."""

    def prepare_workspace_snapshot(
        self, layer_stack_root: str, *, owner_request_id: str, materialize: bool
    ) -> LayerSnapshotLike: ...

    def release_workspace_snapshot(
        self, layer_stack_root: str, *, lease_id: str
    ) -> bool: ...


class AuditSink(Protocol):
    def emit(self, event_type: str, payload: dict[str, Any]) -> None: ...


@dataclass
class IsolatedWorkspaceHandle:
    """Per-workspace state. Not a subclass of ``OperationOverlayHandle`` (C1)."""

    handle_id: str
    agent_id: str
    lease_id: str
    manifest_version: int
    manifest_root_hash: str
    workspace_root: str
    scratch_dir: Path
    upperdir: Path
    workdir: Path
    ns_fds: dict[str, int] = field(default_factory=dict)
    root_pid: int = 0
    # FDs into the ns_holder's readiness/control pipes. ``-1`` means "not yet
    # opened". Closed on teardown / rollback.
    readiness_fd: int = -1
    control_fd: int = -1
    veth: VethPair | None = None
    cgroup_path: Path | None = None
    # Set to True when R11's SIGSTOP fallback fires because cgroup.freeze
    # didn't settle inside the 2 s deadline. Surfaced in audit + status.
    # PR 0 wires the path but R11's fallback itself is deferred — the field
    # stays False on healthy hosts and is forward-compatible with the
    # eventual implementation.
    freezer_degraded: bool = False
    created_at: float = 0.0
    last_activity: float = 0.0
    status: HandleStatus = "active"
    lock: asyncio.Lock = field(default_factory=asyncio.Lock)

    def to_persisted(self) -> dict[str, Any]:
        return {
            "handle_id": self.handle_id,
            "agent_id": self.agent_id,
            "lease_id": self.lease_id,
            "manifest_version": self.manifest_version,
            "manifest_root_hash": self.manifest_root_hash,
            "veth_host_name": self.veth.host_name if self.veth else None,
            "ns_ip": str(self.veth.ns_ip) if self.veth else None,
            "cgroup_path": self.cgroup_path.as_posix() if self.cgroup_path else None,
            "scratch_dir_path": self.scratch_dir.as_posix(),
            "root_pid": self.root_pid,
            "freezer_degraded": self.freezer_degraded,
            "created_at": self.created_at,
        }


@dataclass(frozen=True)
class _ManagerConfig:
    enabled: bool
    ttl_s: float
    per_agent_quota: int
    total_cap: int
    upperdir_bytes: int
    memavail_fraction: float
    setup_timeout_s: float
    rfc1918_egress: Literal["allow", "deny"]
    fallback_dns: str

    @classmethod
    def from_env(cls, env: dict[str, str] | None = None) -> _ManagerConfig:
        env = env or dict(os.environ)
        return cls(
            enabled=env.get("EOS_ISOLATED_WORKSPACE_ENABLED", "false").lower() == "true",
            ttl_s=float(env.get("EOS_ISOLATED_WORKSPACE_TTL_S", "1800")),
            per_agent_quota=int(env.get("EOS_ISOLATED_WORKSPACE_PER_AGENT", "1")),
            total_cap=int(env.get("EOS_ISOLATED_WORKSPACE_TOTAL_CAP", "5")),
            upperdir_bytes=int(env.get("EOS_ISOLATED_WORKSPACE_UPPERDIR_BYTES",
                                      str(1024 * 1024 * 1024))),
            memavail_fraction=float(env.get("EOS_ISOLATED_WORKSPACE_MEMAVAIL_FRACTION", "0.5")),
            setup_timeout_s=float(env.get("EOS_ISOLATED_WORKSPACE_SETUP_TIMEOUT_S", "30")),
            rfc1918_egress="deny" if env.get(
                "EOS_ISOLATED_WORKSPACE_RFC1918_EGRESS", "allow"
            ).lower() == "deny" else "allow",
            fallback_dns=env.get("EOS_ISOLATED_WORKSPACE_FALLBACK_DNS", "1.1.1.1"),
        )


class _PhaseTimer:
    """Per-operation phase-timing helper (PLAN §14).

    ``measure(name)`` is a context manager that records the elapsed
    millisecond cost of a phase ONLY when its body exits normally — phases
    that raised mid-flight are intentionally absent from ``phases_ms`` so
    that "absent" stays distinct from "ran in zero time" (P5).
    """

    def __init__(self, clock: Callable[[], float] = time.monotonic) -> None:
        self._clock = clock
        self._start = clock()
        self._phases: dict[str, float] = {}

    @contextlib.contextmanager
    def measure(self, name: str):
        t0 = self._clock()
        success = False
        try:
            yield
            success = True
        finally:
            if success:
                self._phases[name] = (self._clock() - t0) * 1000.0

    def total_ms(self) -> float:
        return (self._clock() - self._start) * 1000.0

    @property
    def phases_ms(self) -> dict[str, float]:
        return dict(self._phases)


class _Runtime(Protocol):
    """Kernel-touching operations the manager delegates to.

    The only concrete implementation is :class:`_LinuxRuntime`; the Protocol
    exists so individual tests can swap in lightweight doubles without
    importing the kernel-touching module. ``mount_overlay`` and
    ``configure_dns`` are ``async def`` so concurrent enters (Tier 6 / Tier 8)
    do not serialize on subprocess wait — both helpers spawn long-running
    (up to 30 s) setns subprocesses that would otherwise block the event
    loop under N=5 fan-out.
    """

    def spawn_ns_holder(self, handle: IsolatedWorkspaceHandle, *, setup_timeout_s: float) -> int: ...
    def open_ns_fds(self, root_pid: int) -> dict[str, int]: ...
    async def mount_overlay(
        self, handle: IsolatedWorkspaceHandle, *, layer_paths: tuple[str, ...]
    ) -> None: ...
    async def configure_dns(
        self, handle: IsolatedWorkspaceHandle, *, fallback_dns: str
    ) -> bool: ...
    def signal_net_ready(
        self, handle: IsolatedWorkspaceHandle, *, setup_timeout_s: float
    ) -> None: ...
    def create_cgroup(self, handle: IsolatedWorkspaceHandle) -> Path: ...
    def freeze(self, handle: IsolatedWorkspaceHandle, *, freeze: bool) -> None: ...
    def kill_holder(self, root_pid: int, *, grace_s: float) -> None: ...
    def run_in_handle(
        self,
        handle: IsolatedWorkspaceHandle,
        *,
        argv: list[str],
        stdin: bytes | None = None,
        timeout_s: float | None = None,
    ) -> tuple[int, bytes, bytes]: ...


class IsolatedWorkspaceManager:
    """Owns the in-memory handle registry, lifecycle, GC, and quota.

    ``layer_stack_root`` is the daemon-wide layer-stack path (the same string
    every other handler validates with ``require_layer_stack_root``).
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

    async def startup_gc(self) -> None:
        """Reap orphan resources after daemon restart; reconcile IP pool.

        After a fresh daemon start the in-memory ``_handles`` is empty: every
        row in persisted ``manager.json`` is by definition a zombie whose
        kernel resources (veth, cgroup, holder process, lease) outlived the
        last daemon. We:

        1. Reserve each persisted handle's IP so a concurrent ``enter`` cannot
           re-allocate one that an in-flight orphan may still be using.
        2. Release each persisted handle's lease so the OCC layer-stack can
           advance again.
        3. For each persisted handle, unfreeze the cgroup BEFORE rmdir so any
           lingering PID can be killed (R5 ordering — pinned by
           ``test_daemon_restart_gc_order_unfreeze_before_kill``).
        4. Sweep any remaining ``eos-iws-*`` veth / scratch / cgroup by
           naming convention.
        """
        persisted = self._read_manager_json()
        persisted_handles = list(persisted.get("handles", []))
        for row in persisted_handles:
            ns_ip = row.get("ns_ip")
            if ns_ip:
                with contextlib.suppress(ValueError):
                    self._network.pool.reserve(ipaddress.IPv4Address(ns_ip))
        for row in persisted_handles:
            self._release_orphan_lease(row)
            self._reap_orphan_cgroup(row)
        # in-memory is empty on a fresh daemon — every named iws resource is
        # an orphan candidate.
        self._reap_orphans(live_set=set())

    def _reap_orphans(self, live_set: set[str]) -> None:
        # Per-orphan gc_orphan timing (PLAN §15.3): each event carries its own
        # ``total_ms`` plus ``phases_ms.{discover, reap}``. The discover cost
        # is amortized across the orphans found in that pass.
        t0 = self._clock()
        result = subprocess.run(
            ["ip", "-o", "link", "show"], capture_output=True, text=True, check=False,
        )
        veth_discover_ms = (self._clock() - t0) * 1000.0
        veth_orphans: list[str] = []
        for line in result.stdout.splitlines():
            for token in line.split():
                if token.startswith(HANDLE_PREFIX) and ":" not in token:
                    name = token.rstrip("@:")
                    short = name[len(HANDLE_PREFIX):].rstrip("hn")
                    if not any(hid.startswith(short) for hid in live_set):
                        veth_orphans.append(name)
        veth_share_ms = (
            veth_discover_ms / len(veth_orphans) if veth_orphans else 0.0
        )
        for name in veth_orphans:
            t_reap = self._clock()
            subprocess.run(
                ["ip", "link", "del", name],
                check=False, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            )
            reap_ms = (self._clock() - t_reap) * 1000.0
            self._emit(
                "sandbox_isolated_workspace_gc_orphan",
                {
                    "kind": "veth",
                    "identifier": name,
                    "total_ms": veth_share_ms + reap_ms,
                    "phases_ms": {"discover": veth_share_ms, "reap": reap_ms},
                },
            )

        scratch = self.scratch_root
        if scratch.is_dir():
            t0 = self._clock()
            scratch_children = [c for c in scratch.iterdir() if c.name != "manager.json"]
            scratch_discover_ms = (self._clock() - t0) * 1000.0
            scratch_orphans = [c for c in scratch_children if c.name not in live_set]
            scratch_share_ms = (
                scratch_discover_ms / len(scratch_orphans) if scratch_orphans else 0.0
            )
            for child in scratch_orphans:
                t_reap = self._clock()
                shutil.rmtree(child, ignore_errors=True)
                reap_ms = (self._clock() - t_reap) * 1000.0
                self._emit(
                    "sandbox_isolated_workspace_gc_orphan",
                    {
                        "kind": "scratch",
                        "identifier": child.name,
                        "total_ms": scratch_share_ms + reap_ms,
                        "phases_ms": {"discover": scratch_share_ms, "reap": reap_ms},
                    },
                )

        # Cgroup naming-convention sweep — anything left after the per-handle
        # release in startup_gc (e.g. created by a different daemon version
        # that crashed before persisting manager.json) gets unfrozen + rmdir'd
        # here.
        if CGROUP_ROOT.is_dir():
            t0 = self._clock()
            cgroup_children = [
                c for c in CGROUP_ROOT.iterdir()
                if c.is_dir() and c.name.startswith(HANDLE_PREFIX)
            ]
            cgroup_discover_ms = (self._clock() - t0) * 1000.0
            cgroup_orphans = [
                c for c in cgroup_children
                if c.name[len(HANDLE_PREFIX):] not in live_set
            ]
            cgroup_share_ms = (
                cgroup_discover_ms / len(cgroup_orphans) if cgroup_orphans else 0.0
            )
            for child in cgroup_orphans:
                t_reap = self._clock()
                self._unfreeze_and_kill(child)
                with contextlib.suppress(OSError):
                    child.rmdir()
                reap_ms = (self._clock() - t_reap) * 1000.0
                self._emit(
                    "sandbox_isolated_workspace_gc_orphan",
                    {
                        "kind": "cgroup",
                        "identifier": child.name,
                        "total_ms": cgroup_share_ms + reap_ms,
                        "phases_ms": {"discover": cgroup_share_ms, "reap": reap_ms},
                    },
                )

    def _release_orphan_lease(self, persisted_row: dict[str, Any]) -> None:
        """Release a lease that survived the daemon process."""
        lease_id = persisted_row.get("lease_id")
        if not lease_id:
            return
        t0 = self._clock()
        released = False
        with contextlib.suppress(Exception):
            released = bool(
                self._layer_stack.release_workspace_snapshot(
                    self._layer_stack_root, lease_id=lease_id,
                )
            )
        reap_ms = (self._clock() - t0) * 1000.0
        self._emit(
            "sandbox_isolated_workspace_gc_orphan",
            {
                "kind": "lease",
                "identifier": lease_id,
                "released": released,
                "total_ms": reap_ms,
                "phases_ms": {"reap": reap_ms},
            },
        )

    def _reap_orphan_cgroup(self, persisted_row: dict[str, Any]) -> None:
        """Unfreeze (R5) and remove a persisted handle's cgroup directory."""
        cg_path = persisted_row.get("cgroup_path")
        if not cg_path:
            return
        cgroup = Path(cg_path)
        if not cgroup.exists():
            return
        t0 = self._clock()
        self._unfreeze_and_kill(cgroup)
        with contextlib.suppress(OSError):
            cgroup.rmdir()
        reap_ms = (self._clock() - t0) * 1000.0
        self._emit(
            "sandbox_isolated_workspace_gc_orphan",
            {
                "kind": "cgroup",
                "identifier": cgroup.name,
                "total_ms": reap_ms,
                "phases_ms": {"reap": reap_ms},
            },
        )

    def _unfreeze_and_kill(self, cgroup: Path) -> None:
        """Unfreeze a cgroup THEN kill its remaining PIDs (R5 ordering).

        Logs both steps so the order survives test inspection
        (``test_daemon_restart_gc_order_unfreeze_before_kill``).
        """
        freeze_file = cgroup / "cgroup.freeze"
        if freeze_file.exists():
            logger.info("isolated_workspace_gc_unfreeze cgroup=%s", cgroup.name)
            with contextlib.suppress(OSError):
                freeze_file.write_text("0\n")
        kill_file = cgroup / "cgroup.kill"
        if kill_file.exists():
            logger.info("isolated_workspace_gc_kill cgroup=%s", cgroup.name)
            with contextlib.suppress(OSError):
                kill_file.write_text("1\n")
            return
        procs_file = cgroup / "cgroup.procs"
        if procs_file.exists():
            logger.info("isolated_workspace_gc_kill cgroup=%s", cgroup.name)
            with contextlib.suppress(OSError):
                pids = [
                    int(line) for line in procs_file.read_text().splitlines()
                    if line.strip().isdigit()
                ]
                for pid in pids:
                    with contextlib.suppress(ProcessLookupError, PermissionError):
                        os.kill(pid, signal.SIGKILL)

    # ------------------------------------------------------------------
    # Lifecycle: enter / exit / run_in_handle
    # ------------------------------------------------------------------

    async def enter(self, agent_id: str) -> IsolatedWorkspaceHandle:
        if not self._config.enabled:
            raise IsolatedWorkspaceError("feature_disabled",
                                         "isolated workspaces are disabled")
        if not agent_id:
            raise IsolatedWorkspaceError("invalid_argument", "agent_id is required")
        # Block until startup_gc has reconciled the IP pool — otherwise a
        # concurrent enter could double-allocate an IP that GC will then free
        # back into the pool. ``initialize`` sets the event after GC step 8.
        if not self._init_complete.is_set():
            await self._init_complete.wait()
        async with self._map_lock:
            if agent_id in self._by_agent:
                existing = self._handles[self._by_agent[agent_id]]
                raise IsolatedWorkspaceError(
                    "isolated_workspace_already_open",
                    "agent already has an open isolated workspace",
                    created_at=existing.created_at,
                    last_activity=existing.last_activity,
                )
            if len(self._handles) >= self._config.total_cap:
                raise IsolatedWorkspaceError(
                    "quota_exceeded", "global isolated workspace cap reached",
                    total_cap=self._config.total_cap,
                )
            self._check_host_capacity()
        timer = _PhaseTimer(self._clock)
        with timer.measure("prepare_snapshot"):
            snapshot = self._layer_stack.prepare_workspace_snapshot(
                self._layer_stack_root,
                owner_request_id=f"isolated-{self._id_factory()}",
                materialize=False,
            )
        handle_id = self._id_factory()
        scratch = self.scratch_root / handle_id
        upper = scratch / "upper"
        work = scratch / "work"
        upper.mkdir(parents=True, exist_ok=True)
        work.mkdir(parents=True, exist_ok=True)
        now = self._clock()
        layer_paths = tuple(snapshot.layer_paths or ())
        handle = IsolatedWorkspaceHandle(
            handle_id=handle_id,
            agent_id=agent_id,
            lease_id=snapshot.lease_id,
            manifest_version=snapshot.manifest_version,
            manifest_root_hash=snapshot.root_hash,
            workspace_root=DEFAULT_WORKSPACE_ROOT,
            scratch_dir=scratch,
            upperdir=upper,
            workdir=work,
            created_at=now,
            last_activity=now,
        )
        try:
            await self._wire_handle(handle, layer_paths, timer=timer)
        except Exception:
            self._rollback_partial(handle)
            with contextlib.suppress(Exception):
                self._layer_stack.release_workspace_snapshot(
                    self._layer_stack_root, lease_id=snapshot.lease_id,
                )
            raise
        async with self._map_lock:
            self._handles[handle_id] = handle
            self._by_agent[agent_id] = handle_id
        self._persist()
        self._emit("sandbox_isolated_workspace_enter", {
            "handle_id": handle_id,
            "agent_id": agent_id,
            "manifest_version": handle.manifest_version,
            "manifest_root_hash": handle.manifest_root_hash,
            "ns_ip": str(handle.veth.ns_ip) if handle.veth else None,
            "rfc1918_egress_mode": self._config.rfc1918_egress,
            "lowerdir_layer_count": len(layer_paths),
            "materialize": False,
            "total_ms": timer.total_ms(),
            "phases_ms": timer.phases_ms,
        })
        return handle

    async def _wire_handle(
        self,
        handle: IsolatedWorkspaceHandle,
        layer_paths: tuple[str, ...],
        *,
        timer: _PhaseTimer | None = None,
    ) -> None:
        # Caller-supplied timer is used for enter()'s audit event; missing
        # phase keys (e.g. mount_overlay when stubbed) intentionally stay
        # absent in phases_ms (P5: absence != zero).
        t = timer or _PhaseTimer(self._clock)
        with t.measure("spawn_ns_holder"):
            _maybe_inject_failure("ns_holder_ready")
            handle.root_pid = self._runtime.spawn_ns_holder(
                handle, setup_timeout_s=self._config.setup_timeout_s,
            )
        with t.measure("open_ns_fds"):
            # ``update`` (not assignment) so the runtime can stash auxiliary
            # FDs on the handle before this method runs without losing them.
            handle.ns_fds.update(self._runtime.open_ns_fds(handle.root_pid))
        with t.measure("install_veth"):
            _maybe_inject_failure("install_veth")
            handle.veth = self._network.install_veth(
                handle_id=handle.handle_id, root_pid=handle.root_pid,
            )
        with t.measure("mount_overlay"):
            _maybe_inject_failure("overlay_mount")
            await self._runtime.mount_overlay(handle, layer_paths=layer_paths)
        with t.measure("configure_dns"):
            _maybe_inject_failure("configure_dns")
            await self._runtime.configure_dns(
                handle, fallback_dns=self._config.fallback_dns,
            )
        # Signal ns_holder that the network + overlay are wired; ns_holder
        # brings ``lo`` up and acks via the readiness pipe. Wrapped in a
        # suppress so a degraded handshake degrades the freezer state rather
        # than failing enter() outright — the workspace is still functional.
        self._runtime.signal_net_ready(
            handle, setup_timeout_s=self._config.setup_timeout_s,
        )
        with t.measure("create_cgroup"):
            handle.cgroup_path = self._runtime.create_cgroup(handle)

    def _rollback_partial(self, handle: IsolatedWorkspaceHandle) -> None:
        if handle.veth is not None:
            with contextlib.suppress(Exception):
                self._network.teardown_veth(handle.veth)
        if handle.root_pid:
            with contextlib.suppress(Exception):
                self._runtime.kill_holder(handle.root_pid, grace_s=1.0)
        for fd in handle.ns_fds.values():
            with contextlib.suppress(OSError):
                os.close(fd)
        for fd in (handle.readiness_fd, handle.control_fd):
            if fd >= 0:
                with contextlib.suppress(OSError):
                    os.close(fd)
        handle.readiness_fd = -1
        handle.control_fd = -1
        with contextlib.suppress(Exception):
            shutil.rmtree(handle.scratch_dir, ignore_errors=True)

    async def exit(self, agent_id: str, *, grace_s: float = 5.0) -> dict[str, Any]:
        async with self._map_lock:
            handle_id = self._by_agent.get(agent_id)
            if handle_id is None:
                return {"success": True, "evicted_upperdir_bytes": 0}
            handle = self._handles[handle_id]
            handle.status = "exiting"
            del self._by_agent[agent_id]
            del self._handles[handle_id]
        upperdir_bytes = _du_bytes(handle.upperdir)
        timer = _PhaseTimer(self._clock)
        async with handle.lock:
            await self._teardown(handle, grace_s=grace_s, timer=timer)
        handle.status = "stopped"
        self._persist()
        lifetime_s = self._clock() - handle.created_at
        total_ms = timer.total_ms()
        phases_ms = timer.phases_ms
        self._emit("sandbox_isolated_workspace_exit", {
            "handle_id": handle.handle_id,
            "reason": "explicit",
            "lifetime_s": lifetime_s,
            "upperdir_bytes_discarded": upperdir_bytes,
            "total_ms": total_ms,
            "phases_ms": phases_ms,
        })
        return {
            "success": True,
            "evicted_upperdir_bytes": upperdir_bytes,
            "lifetime_s": lifetime_s,
            "total_ms": total_ms,
            "phases_ms": phases_ms,
        }

    async def _teardown(
        self,
        handle: IsolatedWorkspaceHandle,
        *,
        grace_s: float,
        timer: _PhaseTimer | None = None,
    ) -> None:
        t = timer or _PhaseTimer(self._clock)
        if handle.root_pid:
            with contextlib.suppress(Exception):
                with t.measure("kill_holder"):
                    self._runtime.kill_holder(handle.root_pid, grace_s=grace_s)
        if handle.veth is not None:
            with contextlib.suppress(Exception):
                with t.measure("teardown_veth"):
                    self._network.teardown_veth(handle.veth)
        for fd in handle.ns_fds.values():
            with contextlib.suppress(OSError):
                os.close(fd)
        handle.ns_fds = {}
        for fd in (handle.readiness_fd, handle.control_fd):
            if fd >= 0:
                with contextlib.suppress(OSError):
                    os.close(fd)
        handle.readiness_fd = -1
        handle.control_fd = -1
        with contextlib.suppress(Exception):
            with t.measure("release_snapshot"):
                self._layer_stack.release_workspace_snapshot(
                    self._layer_stack_root, lease_id=handle.lease_id,
                )
        if handle.cgroup_path and handle.cgroup_path.exists():
            with contextlib.suppress(OSError):
                with t.measure("cgroup_rmdir"):
                    handle.cgroup_path.rmdir()
        with contextlib.suppress(Exception):
            with t.measure("rmtree_scratch"):
                shutil.rmtree(handle.scratch_dir, ignore_errors=True)

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
        async with handle.lock:
            with timer.measure("unfreeze"):
                self._runtime.freeze(handle, freeze=False)
            try:
                start = self._clock()
                with timer.measure("exec"):
                    exit_code, out, err = self._runtime.run_in_handle(
                        handle, argv=argv, stdin=stdin, timeout_s=timeout_s,
                    )
                duration = self._clock() - start
            finally:
                with contextlib.suppress(Exception):
                    with timer.measure("freeze"):
                        self._runtime.freeze(handle, freeze=True)
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

    async def _ttl_loop(self) -> None:
        """Background task started by ``initialize`` that runs periodic sweeps.

        Tick interval = ``max(0.5 s, min(ttl_s / 2, 30 s))`` so short TTLs
        (Tier 5's ``test_ttl_evict_and_audit`` sets ``TTL_S=1``) still see a
        sweep inside the test budget while the default 1800 s TTL stays at a
        modest 30 s heartbeat.
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

    async def ttl_sweep(self) -> int:
        now = self._clock()
        evicted = 0
        async with self._map_lock:
            stale = [
                h for h in self._handles.values()
                if now - h.last_activity > self._config.ttl_s
            ]
        for handle in stale:
            try:
                stats = await self.exit(handle.agent_id)
                self._emit(
                    "sandbox_isolated_workspace_evicted",
                    {
                        "handle_id": handle.handle_id,
                        "reason": "ttl",
                        "lifetime_s": stats.get("lifetime_s", 0.0),
                        "upperdir_bytes_discarded": stats.get(
                            "evicted_upperdir_bytes", 0
                        ),
                        "total_ms": stats.get("total_ms", 0.0),
                        "phases_ms": stats.get("phases_ms", {}),
                    },
                )
                evicted += 1
            except Exception:  # pragma: no cover - logging only
                logger.exception("ttl_sweep failed for %s", handle.handle_id)
        return evicted

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

    # ------------------------------------------------------------------
    # Internals
    # ------------------------------------------------------------------

    def _check_host_capacity(self) -> None:
        budget = self._compute_host_budget()
        required = (len(self._handles) + 1) * self._config.upperdir_bytes
        if required > budget:
            raise IsolatedWorkspaceError(
                "host_capacity_exceeded",
                "host RAM gate refuses new isolated workspace",
                required_bytes=required, budget_bytes=budget,
            )

    def _compute_host_budget(self) -> int:
        try:
            memavail_kb = self._meminfo_reader()
        except Exception:
            return 2**62
        return int(memavail_kb * 1024 * self._config.memavail_fraction)

    def _read_manager_json(self) -> dict[str, Any]:
        path = self.manager_json_path
        if not path.exists():
            return {"schema_version": SCHEMA_VERSION, "handles": []}
        try:
            data = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            logger.warning("manager_json_unreadable path=%s", path)
            return {"schema_version": SCHEMA_VERSION, "handles": []}
        if data.get("schema_version") != SCHEMA_VERSION:
            logger.warning("manager_json_schema_mismatch expected=%s found=%s",
                           SCHEMA_VERSION, data.get("schema_version"))
            return {"schema_version": SCHEMA_VERSION, "handles": []}
        return data

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


# ----------------------------------------------------------------------
# Linux runtime — kernel-touching helpers. The daemon only ever runs in
# the sweevo Docker container (Linux), so these methods make Linux-only
# syscalls / shell-outs without defensive platform branches. Tests
# substitute a fake by passing ``runtime=`` to the manager.
# ----------------------------------------------------------------------


class _LinuxRuntime:
    """Default runtime — calls real Linux syscalls / utilities."""

    def spawn_ns_holder(self, handle: IsolatedWorkspaceHandle, *, setup_timeout_s: float) -> int:
        # os.pipe() returns (read_fd, write_fd). The holder process WRITES
        # "ns-up" and reads "net-ready"; the parent READS "ns-up" and writes
        # "net-ready". Variable naming below is by usage (whose fd it is),
        # so r_holder is the write end of the readiness pipe and r_parent
        # is the read end.
        r_parent, r_holder = os.pipe()
        c_holder, c_parent = os.pipe()
        proc = subprocess.Popen(
            [
                # ``--map-root-user`` (``-r``): without it the unshared user
                # namespace maps the caller to nobody, and ``--mount-proc``
                # then fails with EPERM on Docker Desktop and any cgroupv2
                # host where the kernel rejects unprivileged /proc mounts.
                # The matching pattern in ``execution/strategies/namespace.py``
                # uses ``unshare -Urm`` (``-r`` is part of the working set).
                "unshare", "--user", "--map-root-user",
                "--net", "--pid", "--mount",
                "--fork", "--mount-proc", "--propagation", "private",
                sys.executable, "-m", "sandbox.isolated_workspace.scripts.ns_holder",
                str(r_holder), str(c_holder),
            ],
            pass_fds=(r_holder, c_holder),
        )
        os.close(r_holder)
        os.close(c_holder)
        try:
            _expect_line(r_parent, b"ns-up", timeout_s=setup_timeout_s)
        except BaseException:
            os.close(r_parent)
            os.close(c_parent)
            raise
        # Keep r_parent open until ``signal_net_ready`` reads the ``ready``
        # ack; closing it eagerly causes the ns_holder's later write to fail
        # with EPIPE and the namespace tears down.
        handle.readiness_fd = r_parent
        handle.control_fd = c_parent
        return proc.pid

    def open_ns_fds(self, root_pid: int) -> dict[str, int]:
        # root_pid is the ``unshare --fork`` process, which stays in the
        # OUTER pid ns. Only ``pid_for_children`` points at the NEW pid ns
        # (per pid_namespaces(7): CLONE_NEWPID does not move the caller,
        # only its future descendants). user/mnt/net are correct via the
        # plain ``ns/<n>`` symlinks since those flags do move the caller.
        ns_paths = {
            "user": f"/proc/{root_pid}/ns/user",
            "mnt": f"/proc/{root_pid}/ns/mnt",
            "pid": f"/proc/{root_pid}/ns/pid_for_children",
            "net": f"/proc/{root_pid}/ns/net",
        }
        return {
            name: os.open(path, os.O_RDONLY | os.O_CLOEXEC)
            for name, path in ns_paths.items()
        }

    async def mount_overlay(
        self, handle: IsolatedWorkspaceHandle, *, layer_paths: tuple[str, ...]
    ) -> None:
        user_fd = handle.ns_fds.get("user")
        mnt_fd = handle.ns_fds.get("mnt")
        if user_fd is None or mnt_fd is None:
            raise IsolatedWorkspaceError(
                "setup_failed",
                "mount_overlay requires user+mnt ns FDs",
                failed_step="overlay_mount",
            )
        # Overlay requires at least one lowerdir. When the manifest is empty
        # (no committed layers), fall back to the workspace_root itself so
        # the mount still succeeds and the upperdir becomes the writable
        # layer. Production workloads should never hit this branch.
        lowerdirs = list(layer_paths) if layer_paths else [handle.workspace_root]
        payload = json.dumps(
            {
                "ns_fds": {"user": user_fd, "mnt": mnt_fd},
                "target": handle.workspace_root,
                "lowerdirs": lowerdirs,
                "upperdir": handle.upperdir.as_posix(),
                "workdir": handle.workdir.as_posix(),
            }
        ).encode("utf-8")
        returncode, _stdout, stderr_bytes = await _run_helper_subprocess(
            argv=[
                sys.executable,
                "-m",
                "sandbox.isolated_workspace.scripts.setns_overlay_mount",
            ],
            stdin_bytes=payload,
            timeout_s=30.0,
            pass_fds=(user_fd, mnt_fd),
        )
        if returncode != 0:
            raise IsolatedWorkspaceError(
                "setup_failed",
                "mount_overlay helper failed",
                failed_step="overlay_mount",
                helper_stderr=stderr_bytes.decode("utf-8", errors="replace"),
                return_code=returncode,
            )

    async def configure_dns(
        self, handle: IsolatedWorkspaceHandle, *, fallback_dns: str
    ) -> bool:
        user_fd = handle.ns_fds.get("user")
        mnt_fd = handle.ns_fds.get("mnt")
        if user_fd is None or mnt_fd is None:
            return False
        payload = json.dumps(
            {
                "ns_fds": {"user": user_fd, "mnt": mnt_fd},
                "fallback_dns": fallback_dns,
            }
        ).encode("utf-8")
        returncode, stdout_bytes, stderr_bytes = await _run_helper_subprocess(
            argv=[
                sys.executable,
                "-m",
                "sandbox.isolated_workspace.scripts.configure_dns_in_ns",
            ],
            stdin_bytes=payload,
            timeout_s=10.0,
            pass_fds=(user_fd, mnt_fd),
        )
        if returncode != 0:
            logger.warning(
                "configure_dns helper failed rc=%d stderr=%s",
                returncode,
                stderr_bytes.decode("utf-8", errors="replace"),
            )
            return False
        try:
            result = json.loads(stdout_bytes.decode("utf-8", errors="replace") or "{}")
        except json.JSONDecodeError:
            return False
        return bool(result.get("applied_fallback", False))

    def signal_net_ready(
        self, handle: IsolatedWorkspaceHandle, *, setup_timeout_s: float
    ) -> None:
        if handle.control_fd < 0:
            return
        # Best-effort handshake. If the ns_holder has already died (e.g.,
        # because the network wiring above failed), the write hits EPIPE; we
        # surface that as a setup_failed error rather than continuing into
        # create_cgroup with a dead PID 1.
        try:
            os.write(handle.control_fd, b"net-ready\n")
        except BrokenPipeError as exc:
            raise IsolatedWorkspaceError(
                "setup_failed",
                "ns_holder closed control pipe before net-ready",
                failed_step="signal_net_ready",
            ) from exc
        if handle.readiness_fd < 0:
            return
        _expect_line(handle.readiness_fd, b"ready", timeout_s=setup_timeout_s)

    def create_cgroup(self, handle: IsolatedWorkspaceHandle) -> Path:
        path = CGROUP_ROOT / f"{HANDLE_PREFIX}{handle.handle_id}"
        path.mkdir(parents=True, exist_ok=True)
        return path

    def freeze(self, handle: IsolatedWorkspaceHandle, *, freeze: bool) -> None:
        """Freeze/thaw via cgroup.freeze with R11 SIGSTOP fallback.

        If the cgroup.freeze write fails (EPERM/EACCES from a missing or
        permissions-stripped controller), walk ``cgroup.procs`` and send
        SIGSTOP/SIGCONT to each PID. Sets ``handle.freezer_degraded=True``
        so the audit + status fields surface the fallback.
        """
        if handle.cgroup_path is None:
            return
        freeze_file = handle.cgroup_path / "cgroup.freeze"
        if freeze_file.exists():
            try:
                freeze_file.write_text("1\n" if freeze else "0\n")
                return
            except OSError:
                handle.freezer_degraded = True
        else:
            handle.freezer_degraded = True
        procs_file = handle.cgroup_path / "cgroup.procs"
        if not procs_file.exists():
            return
        sig = signal.SIGSTOP if freeze else signal.SIGCONT
        try:
            pids = [
                int(line)
                for line in procs_file.read_text().splitlines()
                if line.strip().isdigit()
            ]
        except OSError:
            pids = []
        for pid in pids:
            with contextlib.suppress(ProcessLookupError, PermissionError):
                os.kill(pid, sig)

    def kill_holder(self, root_pid: int, *, grace_s: float) -> None:
        with contextlib.suppress(ProcessLookupError, PermissionError):
            os.kill(root_pid, signal.SIGTERM)
        deadline = time.monotonic() + grace_s
        while time.monotonic() < deadline:
            try:
                os.kill(root_pid, 0)
            except ProcessLookupError:
                return
            time.sleep(0.05)
        with contextlib.suppress(ProcessLookupError, PermissionError):
            os.kill(root_pid, signal.SIGKILL)

    def run_in_handle(
        self,
        handle: IsolatedWorkspaceHandle,
        *,
        argv: list[str],
        stdin: bytes | None = None,
        timeout_s: float | None = None,
    ) -> tuple[int, bytes, bytes]:
        ns_fds = {k: handle.ns_fds[k] for k in ("user", "mnt", "pid", "net") if k in handle.ns_fds}
        payload = json.dumps({"ns_fds": ns_fds, "argv": argv}).encode("utf-8")
        proc = subprocess.run(
            [sys.executable, "-m", "sandbox.isolated_workspace.scripts.setns_exec"],
            input=payload + b"\n" + (stdin or b""),
            capture_output=True,
            timeout=timeout_s,
            pass_fds=tuple(ns_fds.values()),
        )
        return proc.returncode, proc.stdout, proc.stderr


async def _run_helper_subprocess(
    *,
    argv: list[str],
    stdin_bytes: bytes,
    timeout_s: float,
    pass_fds: tuple[int, ...],
) -> tuple[int, bytes, bytes]:
    """Run a setns helper without blocking the asyncio event loop.

    The setns helpers under ``scripts/`` consume their JSON payload on stdin
    and return success via process exit code. Tier 6/8 fan-out N=5 concurrent
    ``enter()`` calls so the long-tail helpers (``setns_overlay_mount``,
    ``configure_dns_in_ns``) MUST not block the loop — otherwise five enters
    serialize behind the same subprocess.run wait.
    """
    proc = await asyncio.create_subprocess_exec(
        *argv,
        stdin=asyncio.subprocess.PIPE,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.PIPE,
        pass_fds=pass_fds,
    )
    try:
        stdout, stderr = await asyncio.wait_for(
            proc.communicate(input=stdin_bytes), timeout=timeout_s,
        )
    except asyncio.TimeoutError:
        with contextlib.suppress(ProcessLookupError):
            proc.kill()
        with contextlib.suppress(Exception):
            await proc.wait()
        raise IsolatedWorkspaceError(
            "setup_timeout",
            f"helper {argv[-1]} exceeded {timeout_s}s",
            failed_step=argv[-1].rsplit(".", 1)[-1],
        )
    return proc.returncode or 0, stdout or b"", stderr or b""


def _read_memavailable_kb() -> int:
    try:
        with open("/proc/meminfo", "r", encoding="utf-8") as fh:
            for line in fh:
                if line.startswith("MemAvailable:"):
                    return int(line.split()[1])
    except OSError:
        pass
    # /proc unreachable (e.g., custom rootfs without procfs) — assume 16 GB
    # free so the host-RAM gate fails open rather than spuriously refusing.
    return 16 * 1024 * 1024


def _du_bytes(path: Path) -> int:
    if not path.exists():
        return 0
    total = 0
    for root, _dirs, files in os.walk(path):
        for f in files:
            with contextlib.suppress(OSError):
                total += os.stat(os.path.join(root, f)).st_size
    return total


def _expect_line(fd: int, prefix: bytes, *, timeout_s: float) -> None:
    deadline = time.monotonic() + timeout_s
    buf = b""
    while b"\n" not in buf:
        if time.monotonic() > deadline:
            raise IsolatedWorkspaceError(
                "setup_timeout", f"ns_holder did not signal {prefix!r}",
                failed_step="ns_holder_ready",
            )
        chunk = os.read(fd, 64)
        if not chunk:
            raise IsolatedWorkspaceError(
                "setup_failed", "ns_holder closed pipe before signaling",
            )
        buf += chunk
    if not buf.startswith(prefix):
        raise IsolatedWorkspaceError(
            "setup_failed", f"unexpected ns_holder signal: {buf!r}",
        )


# ----------------------------------------------------------------------
# Daemon-singleton accessors. The handler modules call these instead of
# importing the daemon's request_context (which transitively pulls in
# ``sandbox.occ.*`` and would break R3 import discipline on
# ``isolated_workspace_ops``).
# ----------------------------------------------------------------------


_manager_singleton: IsolatedWorkspaceManager | None = None


def set_manager(manager: IsolatedWorkspaceManager | None) -> None:
    global _manager_singleton
    _manager_singleton = manager


def require_manager() -> IsolatedWorkspaceManager:
    if _manager_singleton is None:
        raise IsolatedWorkspaceError(
            "feature_disabled", "isolated workspace manager is not initialized",
        )
    return _manager_singleton


def require_arg(args: dict[str, Any], key: str) -> str:
    """Local copy of the arg validator — avoids importing request_context."""
    value = str(args.get(key) or "").strip()
    if not value:
        raise IsolatedWorkspaceError("invalid_argument", f"{key} is required", key=key)
    return value


__all__ = [
    "AuditSink",
    "IsolatedWorkspaceError",
    "IsolatedWorkspaceHandle",
    "IsolatedWorkspaceManager",
    "LayerSnapshotLike",
    "LayerStackPort",
    "_LinuxRuntime",
    "_ManagerConfig",
    "_PHASE_TIMER_OVERHEAD_BUDGET_MS",
    "_PhaseTimer",
    "require_arg",
    "require_manager",
    "set_manager",
]
