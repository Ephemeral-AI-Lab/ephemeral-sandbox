"""Daemon-native isolated workspace feature.

A self-contained directory for the per-agent ``{user, mnt, pid, net}`` sandbox
that the daemon offers via ``api.isolated_workspace.{enter, exit, status,
shell, read_file, write_file, edit_file, grep}``.

Submodules
----------
- :mod:`.manager` — lifecycle state machine, quota / TTL / host-RAM gate,
  ``manager.json`` persistence, GC pass, ``_PhaseTimer``, ``_LinuxRuntime``.
- :mod:`.network` — bridge + nftables + per-workspace veth + IP pool.
- :mod:`.handlers` — RPC handlers for the lifecycle ops
  (``enter``, ``exit_``, ``status``).
- :mod:`.ops_handlers` — RPC handlers for the per-tool ops
  (``shell``, ``read_file``, ``write_file``, ``edit_file``,
  ``grep``). Subject to the R3 import-graph fence:
  transitive imports MUST NOT include ``sandbox.occ.*`` or
  ``sandbox.daemon.service.sandbox_overlay``.
- :mod:`.scripts` — single-threaded subprocess helpers that perform setns
  syscalls. R10 import discipline applies: their module-level import sets
  are pinned by ``test_setns_exec_discipline``.

Cross-package reuse
-------------------
- ``setns_overlay_mount`` calls
  :func:`sandbox.execution.overlay.kernel_mount.mount_overlay` after setns —
  a deferred import keeps R10 (single-thread for ``setns(CLONE_NEWUSER)``).
- Lease / snapshot calls go through
  ``sandbox.daemon.workspace_server`` (layer-stack-only; OCC is unreachable).
"""
