"""Enter, exit, rollback, and teardown logic for isolated workspaces."""

from __future__ import annotations

import contextlib
import os
import shutil
from typing import Any

from sandbox.audit.events import IsolatedWorkspaceAuditEvent
from sandbox.daemon.audit_schema import (
    IsolatedWorkspaceSection,
    build_isolated_workspace_event,
    safe_emit,
)
from sandbox.isolated_workspace._control_plane.namespace_runtime import _directory_file_bytes
from sandbox.isolated_workspace._control_plane.types import (
    ISOLATED_WORKSPACE_ROOT,
    IsolatedWorkspaceError,
    IsolatedWorkspaceHandle,
    _maybe_inject_failure,
    _PhaseTimer,
)


def _emit_isolated_workspace(
    event_type: str,
    section: IsolatedWorkspaceSection,
    *,
    lane: str = "critical",
) -> None:
    safe_emit(
        build_isolated_workspace_event(event_type, section),
        lane=lane,  # type: ignore[arg-type]
    )


class _WorkspaceHandleLifecycleMixin:
    async def enter(self, agent_id: str) -> IsolatedWorkspaceHandle:
        if not self._config.enabled:
            raise IsolatedWorkspaceError("feature_disabled", "isolated workspaces are disabled")
        if not agent_id:
            raise IsolatedWorkspaceError("invalid_argument", "agent_id is required")
        # Block until startup orphan recovery has reconciled the IP pool — otherwise a
        # concurrent enter could double-allocate an IP that GC will then free
        # back into the pool. ``initialize`` sets the event after GC step 8.
        if not self._init_complete.is_set():
            await self._init_complete.wait()
        async with self._map_lock:
            if agent_id in self._by_agent:
                existing = self._handles[self._by_agent[agent_id]]
                raise IsolatedWorkspaceError(
                    "already_open",
                    "agent already has an open isolated workspace",
                    created_at=existing.created_at,
                    last_activity=existing.last_activity,
                )
            if len(self._handles) >= self._config.total_cap:
                raise IsolatedWorkspaceError(
                    "quota_exceeded",
                    "global isolated workspace cap reached",
                    total_cap=self._config.total_cap,
                )
            self._check_host_capacity()
        timer = _PhaseTimer(self._clock)
        with timer.measure("prepare_snapshot"):
            snapshot = self._layer_stack.acquire_snapshot(
                request_id=f"isolated-{self._id_factory()}",
            )
        workspace_handle_id = self._id_factory()
        scratch = self.scratch_root / workspace_handle_id
        upper = scratch / "upper"
        work = scratch / "work"
        upper.mkdir(parents=True, exist_ok=True)
        work.mkdir(parents=True, exist_ok=True)
        now = self._clock()
        layer_paths = tuple(snapshot.layer_paths or ())
        handle = IsolatedWorkspaceHandle(
            workspace_handle_id=workspace_handle_id,
            agent_id=agent_id,
            lease_id=snapshot.lease_id,
            manifest_version=snapshot.manifest_version,
            manifest_root_hash=snapshot.root_hash,
            workspace_root=ISOLATED_WORKSPACE_ROOT,
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
                self._layer_stack.release_lease(
                    lease_id=snapshot.lease_id,
                )
            raise
        handle.last_activity = self._clock()
        async with self._map_lock:
            self._handles[workspace_handle_id] = handle
            self._by_agent[agent_id] = workspace_handle_id
        self._persist()
        self._emit(
            IsolatedWorkspaceAuditEvent.ENTER,
            {
                "workspace_handle_id": workspace_handle_id,
                "agent_id": agent_id,
                "manifest_version": handle.manifest_version,
                "manifest_root_hash": handle.manifest_root_hash,
                "ns_ip": str(handle.veth.ns_ip) if handle.veth else None,
                "rfc1918_egress_mode": self._config.rfc1918_egress,
                "lowerdir_layer_count": len(layer_paths),
                "tree-copy": False,
                "total_ms": timer.total_ms(),
                "phases_ms": timer.phases_ms,
            },
        )
        _emit_isolated_workspace(
            "isolated_workspace.entered",
            IsolatedWorkspaceSection(
                operation_id=handle.lease_id,
                workspace_handle_id=handle.workspace_handle_id,
                agent_id=agent_id,
                holder_pid=handle.holder_pid or None,
                holder_pid_alive=bool(handle.holder_pid),
                cgroup_id=(
                    handle.cgroup_path.as_posix() if handle.cgroup_path else None
                ),
                upperdir_cap_bytes=self._config.upperdir_bytes,
            ),
        )
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
            handle.holder_pid = self._runtime.spawn_ns_holder(
                handle,
                setup_timeout_s=self._config.setup_timeout_s,
            )
        with t.measure("open_ns_fds"):
            # ``update`` (not assignment) so the runtime can stash auxiliary
            # FDs on the handle before this method runs without losing them.
            handle.ns_fds.update(self._runtime.open_ns_fds(handle.holder_pid))
        with t.measure("install_veth"):
            _maybe_inject_failure("install_veth")
            try:
                handle.veth = self._network.install_veth(
                    workspace_handle_id=handle.workspace_handle_id,
                    holder_pid=handle.holder_pid,
                )
            except RuntimeError as exc:
                # When the ns_holder dies between spawn_ns_holder and
                # install_veth (e.g., HOLDER_CRASH inject, real-world race),
                # ``ip link set ... netns <holder_pid>`` fails with
                # "RTNETLINK answers: No such process". Translate to
                # setup_failed so the dispatcher surfaces a coherent error
                # instead of the dispatcher's catch-all ``internal_error``.
                if "No such process" in str(exc):
                    raise IsolatedWorkspaceError(
                        "setup_failed",
                        f"ns_holder died before install_veth completed: {exc}",
                        failed_step="install_veth",
                    ) from exc
                raise
        with t.measure("mount_overlay"):
            _maybe_inject_failure("overlay_mount")
            await self._runtime.mount_overlay(handle, layer_paths=layer_paths)
        with t.measure("configure_dns"):
            _maybe_inject_failure("configure_dns")
            await self._runtime.configure_dns(
                handle,
                fallback_dns=self._config.fallback_dns,
            )
        # Signal ns_holder that the network + overlay are wired; ns_holder
        # brings ``lo`` up and acks via the readiness pipe.
        self._runtime.signal_net_ready(
            handle,
            setup_timeout_s=self._config.setup_timeout_s,
        )
        with t.measure("create_cgroup"):
            handle.cgroup_path = self._runtime.create_cgroup(handle)

    def _rollback_partial(self, handle: IsolatedWorkspaceHandle) -> None:
        if handle.veth is not None:
            with contextlib.suppress(Exception):
                self._network.teardown_veth(handle.veth)
        if handle.holder_pid:
            with contextlib.suppress(Exception):
                self._runtime.kill_holder(handle.holder_pid, grace_s=1.0)
        _close_handle_fds(handle)
        with contextlib.suppress(Exception):
            shutil.rmtree(handle.scratch_dir, ignore_errors=True)

    async def exit(
        self,
        agent_id: str,
        *,
        grace_s: float | None = None,
    ) -> dict[str, Any]:
        # Lazy import: ``workspace_tool_dispatch`` imports the isolated
        # pipeline registry at module load, so this module cannot import
        # it at top level without a cycle.
        from sandbox.daemon.workspace_tool.dispatch import (
            begin_exit_drain,
            finalize_exit_drain,
            lifecycle_exit_critical_section,
        )

        effective_grace_s = self._config.exit_grace_s if grace_s is None else max(0.0, grace_s)
        # Phase 4 §D2: drain in-flight foreground dispatches before touching
        # routing state. ``exit_pending`` blocks new dispatches the moment
        # ``begin_exit_drain`` returns; the wait covers calls that probed
        # the pipeline before the gate flipped.
        drain_mode, inflight_at_timeout = await begin_exit_drain(
            agent_id, grace_s=effective_grace_s
        )
        if drain_mode == "timeout":
            return _exit_drain_timeout_payload(
                inflight=inflight_at_timeout, grace_s=effective_grace_s
            )
        async with lifecycle_exit_critical_section(agent_id):
            async with self._map_lock:
                workspace_handle_id = self._by_agent.get(agent_id)
                if workspace_handle_id is None:
                    # State exists but no map entry — the agent already exited
                    # via another path. Reset exit_pending so the dispatch
                    # state does not strand future dispatches.
                    await finalize_exit_drain(agent_id)
                    raise IsolatedWorkspaceError(
                        "not_open",
                        "agent has no open isolated workspace",
                        agent_id=agent_id,
                    )
                handle = self._handles[workspace_handle_id]
                del self._by_agent[agent_id]
                del self._handles[workspace_handle_id]
        upperdir_bytes = _directory_file_bytes(handle.upperdir)
        timer = _PhaseTimer(self._clock)
        await self._teardown(handle, grace_s=effective_grace_s, timer=timer)
        await finalize_exit_drain(agent_id)
        self._persist()
        lifetime_s = self._clock() - handle.created_at
        total_ms = timer.total_ms()
        phases_ms = timer.phases_ms
        self._emit(
            IsolatedWorkspaceAuditEvent.EXIT,
            {
                "workspace_handle_id": handle.workspace_handle_id,
                "reason": "explicit",
                "lifetime_s": lifetime_s,
                "upperdir_bytes_discarded": upperdir_bytes,
                "total_ms": total_ms,
                "phases_ms": phases_ms,
            },
        )
        orphan_counts = self._post_exit_orphan_check(handle)
        _emit_isolated_workspace(
            "isolated_workspace.exited",
            IsolatedWorkspaceSection(
                operation_id=handle.lease_id,
                workspace_handle_id=handle.workspace_handle_id,
                agent_id=agent_id,
                holder_pid=handle.holder_pid or None,
                cgroup_id=(
                    handle.cgroup_path.as_posix() if handle.cgroup_path else None
                ),
                cgroup_removed=(
                    handle.cgroup_path is not None and not handle.cgroup_path.exists()
                ),
                scratch_removed=not handle.scratch_dir.exists(),
                upperdir_bytes=upperdir_bytes,
                upperdir_cap_bytes=self._config.upperdir_bytes,
                orphan_holder_count=orphan_counts["holder"],
                orphan_cgroup_count=orphan_counts["cgroup"],
                orphan_scratch_count=orphan_counts["scratch"],
            ),
        )
        _emit_isolated_workspace(
            "isolated_workspace.orphan_check_completed",
            IsolatedWorkspaceSection(
                operation_id=handle.lease_id,
                workspace_handle_id=handle.workspace_handle_id,
                agent_id=agent_id,
                orphan_holder_count=orphan_counts["holder"],
                orphan_cgroup_count=orphan_counts["cgroup"],
                orphan_scratch_count=orphan_counts["scratch"],
            ),
        )
        return {
            "success": True,
            "evicted_upperdir_bytes": upperdir_bytes,
            "lifetime_s": lifetime_s,
            "total_ms": total_ms,
            "phases_ms": phases_ms,
        }

    def _post_exit_orphan_check(
        self, handle: IsolatedWorkspaceHandle
    ) -> dict[str, int]:
        """Best-effort post-exit residue check; no kernel calls beyond stat."""
        counts = {"holder": 0, "cgroup": 0, "scratch": 0}
        if handle.holder_pid:
            try:
                os.kill(handle.holder_pid, 0)
                counts["holder"] = 1
            except (ProcessLookupError, PermissionError, OSError):
                pass
        if handle.cgroup_path is not None and handle.cgroup_path.exists():
            counts["cgroup"] = 1
        if handle.scratch_dir.exists():
            counts["scratch"] = 1
        return counts

    async def _teardown(
        self,
        handle: IsolatedWorkspaceHandle,
        *,
        grace_s: float,
        timer: _PhaseTimer | None = None,
    ) -> None:
        t = timer or _PhaseTimer(self._clock)
        if handle.holder_pid:
            with contextlib.suppress(Exception):
                with t.measure("kill_holder"):
                    self._runtime.kill_holder(handle.holder_pid, grace_s=grace_s)
        if handle.veth is not None:
            with contextlib.suppress(Exception):
                with t.measure("teardown_veth"):
                    self._network.teardown_veth(handle.veth)
        _close_handle_fds(handle)
        with contextlib.suppress(Exception):
            with t.measure("release_snapshot"):
                self._layer_stack.release_lease(
                    lease_id=handle.lease_id,
                )
        if handle.cgroup_path and handle.cgroup_path.exists():
            with contextlib.suppress(OSError):
                with t.measure("cgroup_rmdir"):
                    handle.cgroup_path.rmdir()
        with contextlib.suppress(Exception):
            with t.measure("rmtree_scratch"):
                shutil.rmtree(handle.scratch_dir, ignore_errors=True)


def _close_handle_fds(handle: IsolatedWorkspaceHandle) -> None:
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


def _exit_drain_timeout_payload(
    *,
    inflight: int,
    grace_s: float,
) -> dict[str, Any]:
    """Wire-shape result for Phase 4 §D2 exit_drain_timeout.

    The handle stays live, ``exit_pending`` has already been reset by
    ``begin_exit_drain``, so the agent can retry with a larger
    ``grace_s`` or pre-cancel via the background-task path. Maps and
    ``_teardown`` are intentionally NOT executed.
    """
    return {
        "success": False,
        "evicted_upperdir_bytes": 0,
        "lifetime_s": 0.0,
        "total_ms": 0.0,
        "phases_ms": {},
        "error": {
            "kind": "exit_drain_timeout",
            "message": (
                "exit_isolated_workspace timed out waiting for in-flight "
                "dispatches to drain; retry with larger grace_s or cancel "
                "via background path"
            ),
            "details": {
                "inflight": str(inflight),
                "grace_s": str(grace_s),
            },
        },
    }
