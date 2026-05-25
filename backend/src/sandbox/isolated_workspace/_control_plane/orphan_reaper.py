"""Startup orphan-resource recovery for isolated workspaces."""

from __future__ import annotations

import contextlib
from dataclasses import dataclass
import ipaddress
import os
import shutil
import signal
import subprocess
import time
from pathlib import Path
from typing import Any

from sandbox.isolated_workspace._control_plane.pipeline_state import (
    CGROUP_ROOT,
    HANDLE_PREFIX,
    IsolatedWorkspaceAuditEvent,
    logger,
)

_NS_HOLDER_MARKER = "sandbox.isolated_workspace.scripts.ns_holder"


@dataclass(frozen=True)
class _NamespaceHolderProcess:
    pid: int
    ppid: int
    state: str
    comm: str
    cmdline: str


class _OrphanResourceReaperMixin:
    async def reap_startup_orphans(self) -> None:
        """Reap orphan resources after daemon restart; reconcile IP pool.

        After a fresh daemon start the in-memory ``_handles`` is empty: every
        row in persisted ``manager.json`` is by definition a zombie whose
        kernel resources (veth, cgroup, holder process, lease) outlived the
        last daemon. We:

        1. Reserve each persisted handle's IP so a concurrent ``enter`` cannot
           re-allocate one that an in-flight orphan may still be using.
        2. Release each persisted handle's lease so the OCC layer-stack can
           advance again.
        3. For each persisted handle, kill any remaining cgroup PIDs before
           rmdir.
            4. Sweep any remaining ``eos-iws-*`` veth / scratch / cgroup by
           naming convention.
        """
        persisted = self._read_persisted_handles()
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
        self._reap_orphan_holder_processes()

        t0 = self._clock()
        result = subprocess.run(
            ["ip", "-o", "link", "show"],
            capture_output=True,
            text=True,
            check=False,
        )
        veth_discover_ms = (self._clock() - t0) * 1000.0
        veth_orphans: list[str] = []
        for line in result.stdout.splitlines():
            # ``ip -o link show`` formats lines as
            #     "<idx>: <ifname>[@<peer>]: <flags> ..."
            # — the trailing colon sticks to the ifname token. The earlier
            # ``":" not in token`` filter skipped every veth (each one has
            # ``@if<n>:``), so no orphan veth was ever discovered. Strip
            # the trailing colon then drop the ``@<peer>`` suffix so the
            # remaining string is exactly what ``ip link del`` expects.
            for token in line.split():
                cleaned = token.rstrip(":").split("@", 1)[0]
                handle_prefix = _handle_prefix_from_veth_name(cleaned)
                if handle_prefix is not None:
                    if not any(hid.startswith(handle_prefix) for hid in live_set):
                        veth_orphans.append(cleaned)
                    # The ifname is always the second whitespace token on
                    # the line; no need to keep scanning flag tokens.
                    break
        veth_share_ms = veth_discover_ms / len(veth_orphans) if veth_orphans else 0.0
        for name in veth_orphans:
            t_reap = self._clock()
            subprocess.run(
                ["ip", "link", "del", name],
                check=False,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
            reap_ms = (self._clock() - t_reap) * 1000.0
            self._emit(
                IsolatedWorkspaceAuditEvent.GC_ORPHAN,
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
                    IsolatedWorkspaceAuditEvent.GC_ORPHAN,
                    {
                        "kind": "scratch",
                        "identifier": child.name,
                        "total_ms": scratch_share_ms + reap_ms,
                        "phases_ms": {"discover": scratch_share_ms, "reap": reap_ms},
                    },
                )

        # Cgroup naming-convention sweep — anything left after the per-handle
        # release in reap_startup_orphans gets killed and rmdir'd here.
        if CGROUP_ROOT.is_dir():
            t0 = self._clock()
            cgroup_children = [
                c for c in CGROUP_ROOT.iterdir() if c.is_dir() and c.name.startswith(HANDLE_PREFIX)
            ]
            cgroup_discover_ms = (self._clock() - t0) * 1000.0
            cgroup_orphans = [
                c for c in cgroup_children if c.name[len(HANDLE_PREFIX) :] not in live_set
            ]
            cgroup_share_ms = cgroup_discover_ms / len(cgroup_orphans) if cgroup_orphans else 0.0
            for child in cgroup_orphans:
                t_reap = self._clock()
                self._kill_remaining_pids(child)
                with contextlib.suppress(OSError):
                    child.rmdir()
                reap_ms = (self._clock() - t_reap) * 1000.0
                self._emit(
                    IsolatedWorkspaceAuditEvent.GC_ORPHAN,
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
                self._layer_stack.release_lease(
                    lease_id=lease_id,
                )
            )
        reap_ms = (self._clock() - t0) * 1000.0
        self._emit(
            IsolatedWorkspaceAuditEvent.GC_ORPHAN,
            {
                "kind": "lease",
                "identifier": lease_id,
                "released": released,
                "total_ms": reap_ms,
                "phases_ms": {"reap": reap_ms},
            },
        )

    def _reap_orphan_cgroup(self, persisted_row: dict[str, Any]) -> None:
        """Kill remaining PIDs and remove a persisted handle's cgroup directory."""
        cg_path = persisted_row.get("cgroup_path")
        if not cg_path:
            return
        cgroup = Path(cg_path)
        if not cgroup.exists():
            return
        t0 = self._clock()
        self._kill_remaining_pids(cgroup)
        with contextlib.suppress(OSError):
            cgroup.rmdir()
        reap_ms = (self._clock() - t0) * 1000.0
        self._emit(
            IsolatedWorkspaceAuditEvent.GC_ORPHAN,
            {
                "kind": "cgroup",
                "identifier": cgroup.name,
                "total_ms": reap_ms,
                "phases_ms": {"reap": reap_ms},
            },
        )

    def _kill_remaining_pids(self, cgroup: Path) -> None:
        """Kill any PIDs still attached to an orphan cgroup."""
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
                    int(line)
                    for line in procs_file.read_text().splitlines()
                    if line.strip().isdigit()
                ]
                for pid in pids:
                    with contextlib.suppress(ProcessLookupError, PermissionError):
                        os.kill(pid, signal.SIGKILL)

    def _reap_orphan_holder_processes(self) -> None:
        """Kill stale ns_holder process trees that outlived a daemon restart."""
        live_root_pids = {
            int(handle.root_pid)
            for handle in getattr(self, "_handles", {}).values()
            if getattr(handle, "root_pid", 0)
        }
        t0 = self._clock()
        candidates = [
            proc
            for proc in _iter_namespace_holder_processes()
            if proc.pid not in live_root_pids and proc.ppid not in live_root_pids
        ]
        discover_ms = (self._clock() - t0) * 1000.0
        if not candidates:
            return
        discover_share_ms = discover_ms / len(candidates)
        target_pids = {proc.pid for proc in candidates if proc.state != "Z"}
        t_reap = self._clock()
        for pid in target_pids:
            with contextlib.suppress(ProcessLookupError, PermissionError):
                os.kill(pid, signal.SIGCONT)
        if target_pids:
            time.sleep(0.05)
        for proc in _namespace_holder_signal_order(candidates):
            if proc.state == "Z":
                continue
            with contextlib.suppress(ProcessLookupError, PermissionError):
                os.kill(proc.pid, signal.SIGTERM)
        _wait_namespace_holder_processes(target_pids, timeout_s=1.0)
        for pid in _remaining_live_namespace_holder_pids(target_pids):
            with contextlib.suppress(ProcessLookupError, PermissionError):
                os.kill(pid, signal.SIGCONT)
            with contextlib.suppress(ProcessLookupError, PermissionError):
                os.kill(pid, signal.SIGKILL)
        _wait_namespace_holder_processes(target_pids, timeout_s=1.0)
        reap_ms = (self._clock() - t_reap) * 1000.0
        reap_share_ms = reap_ms / len(candidates)
        for proc in candidates:
            self._emit(
                IsolatedWorkspaceAuditEvent.GC_ORPHAN,
                {
                    "kind": "holder",
                    "identifier": str(proc.pid),
                    "state": proc.state,
                    "comm": proc.comm,
                    "total_ms": discover_share_ms + reap_share_ms,
                    "phases_ms": {
                        "discover": discover_share_ms,
                        "reap": reap_share_ms,
                    },
                },
            )


def _iter_namespace_holder_processes(
    proc_root: Path = Path("/proc"),
) -> list[_NamespaceHolderProcess]:
    try:
        entries = list(proc_root.iterdir())
    except OSError:
        return []
    processes: list[_NamespaceHolderProcess] = []
    for entry in entries:
        if not entry.name.isdigit():
            continue
        stat = _read_proc_stat(entry / "stat")
        if stat is None:
            continue
        pid, comm, state, ppid = stat
        cmdline = _read_cmdline(entry / "cmdline")
        if _NS_HOLDER_MARKER not in cmdline:
            continue
        processes.append(
            _NamespaceHolderProcess(
                pid=pid,
                ppid=ppid,
                state=state,
                comm=comm,
                cmdline=cmdline,
            )
        )
    return processes


def _handle_prefix_from_veth_name(name: str) -> str | None:
    if (
        not name.startswith(HANDLE_PREFIX)
        or len(name) != len(HANDLE_PREFIX) + 7
        or name[-1:] not in {"h", "n"}
    ):
        return None
    return name[len(HANDLE_PREFIX) : -1]


def _namespace_holder_signal_order(
    processes: list[_NamespaceHolderProcess],
) -> list[_NamespaceHolderProcess]:
    return sorted(processes, key=lambda proc: (proc.comm == "unshare", proc.pid))


def _wait_namespace_holder_processes(pids: set[int], *, timeout_s: float) -> None:
    deadline = time.monotonic() + timeout_s
    while time.monotonic() < deadline:
        if not _remaining_live_namespace_holder_pids(pids):
            return
        time.sleep(0.02)


def _remaining_live_namespace_holder_pids(pids: set[int]) -> set[int]:
    live: set[int] = set()
    for proc in _iter_namespace_holder_processes():
        if proc.pid in pids and proc.state != "Z":
            live.add(proc.pid)
    return live


def _read_cmdline(path: Path) -> str:
    try:
        raw = path.read_bytes()
    except OSError:
        return ""
    return raw.replace(b"\0", b" ").decode("utf-8", errors="replace")


def _read_proc_stat(path: Path) -> tuple[int, str, str, int] | None:
    try:
        text = path.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return None
    open_paren = text.find("(")
    close_paren = text.rfind(")")
    if open_paren < 0 or close_paren <= open_paren:
        return None
    try:
        pid = int(text[:open_paren].strip())
        rest = text[close_paren + 1 :].split()
        state = rest[0]
        ppid = int(rest[1])
    except (IndexError, ValueError):
        return None
    return pid, text[open_paren + 1 : close_paren], state, ppid
