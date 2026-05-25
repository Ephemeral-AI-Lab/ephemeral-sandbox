"""Enter, exit, rollback, and teardown logic for isolated workspaces."""

from __future__ import annotations

import contextlib
import os
import shutil
from typing import Any

from sandbox.isolated_workspace._control_plane.linux_runtime import _directory_file_bytes
from sandbox.isolated_workspace._control_plane.pipeline_state import (
    ISOLATED_WORKSPACE_ROOT,
    IsolatedWorkspaceAuditEvent,
    IsolatedWorkspaceError,
    IsolatedWorkspaceHandle,
    _maybe_inject_failure,
    _PhaseTimer,
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
            snapshot = self._layer_stack.prepare_workspace_snapshot(
                request_id=f"isolated-{self._id_factory()}",
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
        async with self._map_lock:
            self._handles[handle_id] = handle
            self._by_agent[agent_id] = handle_id
        self._persist()
        self._emit(
            IsolatedWorkspaceAuditEvent.ENTER,
            {
                "handle_id": handle_id,
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
                handle,
                setup_timeout_s=self._config.setup_timeout_s,
            )
        with t.measure("open_ns_fds"):
            # ``update`` (not assignment) so the runtime can stash auxiliary
            # FDs on the handle before this method runs without losing them.
            handle.ns_fds.update(self._runtime.open_ns_fds(handle.root_pid))
        with t.measure("install_veth"):
            _maybe_inject_failure("install_veth")
            try:
                handle.veth = self._network.install_veth(
                    handle_id=handle.handle_id,
                    root_pid=handle.root_pid,
                )
            except RuntimeError as exc:
                # When the ns_holder dies between spawn_ns_holder and
                # install_veth (e.g., HOLDER_CRASH inject, real-world race),
                # ``ip link set ... netns <root_pid>`` fails with
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
        if handle.root_pid:
            with contextlib.suppress(Exception):
                self._runtime.kill_holder(handle.root_pid, grace_s=1.0)
        _close_handle_fds(handle)
        with contextlib.suppress(Exception):
            shutil.rmtree(handle.scratch_dir, ignore_errors=True)

    async def exit(
        self,
        agent_id: str,
        *,
        grace_s: float | None = None,
    ) -> dict[str, Any]:
        async with self._map_lock:
            handle_id = self._by_agent.get(agent_id)
            if handle_id is None:
                raise IsolatedWorkspaceError(
                    "not_open",
                    "agent has no open isolated workspace",
                    agent_id=agent_id,
                )
            handle = self._handles[handle_id]
            del self._by_agent[agent_id]
            del self._handles[handle_id]
        upperdir_bytes = _directory_file_bytes(handle.upperdir)
        timer = _PhaseTimer(self._clock)
        effective_grace_s = self._config.exit_grace_s if grace_s is None else max(0.0, grace_s)
        await self._teardown(handle, grace_s=effective_grace_s, timer=timer)
        self._persist()
        lifetime_s = self._clock() - handle.created_at
        total_ms = timer.total_ms()
        phases_ms = timer.phases_ms
        self._emit(
            IsolatedWorkspaceAuditEvent.EXIT,
            {
                "handle_id": handle.handle_id,
                "reason": "explicit",
                "lifetime_s": lifetime_s,
                "upperdir_bytes_discarded": upperdir_bytes,
                "total_ms": total_ms,
                "phases_ms": phases_ms,
            },
        )
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
