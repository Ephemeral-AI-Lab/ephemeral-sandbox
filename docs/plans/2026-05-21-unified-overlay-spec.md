# Unified Overlay for command_exec and Plugins — Implementation Spec

**Date:** 2026-05-21
**Status:** Implemented; live provider performance sweep still recommended
**Scope:** Sandbox execution surface (`/testbed`), layer stack lifecycle, OCC integration, plugin runtime
**Audience:** Anyone implementing or reviewing the unified-overlay refactor

---

## 1. Overview

Today the sandbox has two parallel mechanisms for projecting the layer stack to consumers:

- `command_exec` mounts an overlayfs at `workspace_root` per command, captures upperdir at command end, publishes via OCC, then unmounts.
- Plugins (Pyright LSP) build a userspace materialized projection via `MergedView.materialize(share_inodes=True)` into `<stack>/runtime/_transient_lowerdir/...`, maintain a stable symlink (`_stable_root`), translate paths via `PathMapper`, and refresh on manifest change.

This spec replaces both with a **single per-sandbox R/W overlay mount at `/testbed`**, owned by the daemon, persistent for the sandbox lifetime, shared by every consumer in the sandbox. `command_exec` and plugin write tools both invoke one shared `publish_cycle` primitive. The plugin runtime drops its projection machinery entirely; plugins like Pyright run with `cwd=/testbed` and use plain POSIX.

This is a structural simplification, not a feature addition. The OCC and capture machinery already exists; the layer-stack storage model is unchanged. What changes is **mount lifetime** (per-command → per-sandbox), **plugin path semantics** (synthetic projection → live overlay), and **notification mechanism** (Pyright-specific refresh dance → generic event bus).

### 1.1 Current implementation status

Landed:

- `SandboxOverlay` is now the daemon-owned freshness/publish facade. It owns `start()`, `stop()`, `ensure_current(...)`, `publish_cycle(...)`, persistent-upperdir publishing, `flush_to_workspace()`, and workspace-change event emission.
- The daemon owns a per-layer-stack/per-workspace overlay cache. When the new mount API is available, the overlay is mounted lazily and kept for the sandbox lifetime; command execution then runs directly against `/testbed` and publishes the shared upperdir.
- `command_exec` enters OCC publication through `SandboxOverlay.publish_cycle(...)` or persistent `publish_pending_changes(...)`; command callers do not coordinate OCC directly.
- The LSP runtime no longer uses `PathMapper`, `_stable_root`, or materialized projection paths. Pyright sessions are rooted directly at the bound workspace root, normally `/testbed`.
- Every LSP tool call runs through `SandboxOverlay.ensure_current(...)` before talking to Pyright.
- LSP write-capable operations now exist for `lsp.apply_workspace_edit`, `lsp.rename`, `lsp.format`, and `lsp.apply_code_action`; they publish through the daemon overlay facade.
- WorkspaceEdit application supports text edits plus LSP create/delete/rename file operations.
- Workspace change events are available through a bounded daemon-local event bus, emitted for command/plugin publishes and flushes, and consumed by LSP sessions through a daemon-local subscription pump.
- A background foreign-publish watcher refreshes a mounted overlay when another daemon path advances the manifest.
- `flush_to_workspace` collapses the current active manifest back into the workspace and rebuilds a fresh base layer.

Remaining follow-up:

- Run the full live Daytona/provider performance sweep. Unit and targeted daemon tests cover the persistent path and disk/resource timing fields; live numbers still need to be collected from a privileged sandbox.
- Optimize `flush_to_workspace` from the functional full-materialize rebuild to the planned delta-apply/hardlink/incremental-hash path.

### 1.2 Round-3 confirmation pass

This is the confirmed design shape, with ownership boundaries made explicit.
These are design contracts; the only remaining evidence gap is the full live
Daytona/provider performance sweep called out in §1.1.

1. **Unified layer-stack / overlay / OCC path: confirmed.** The new design unifies `layer_stack`, overlay, and OCC for both `command_exec` and plugin operations. The shared primitive is `SandboxOverlay.publish_cycle(...)`: capture upperdir, validate/publish through OCC, rotate the active manifest lease, remount `/testbed`, and emit a workspace-change event. `command_exec` and plugin write tools enter the same daemon-owned publish path.
2. **Plugins see a normal filesystem at `/testbed`: confirmed, with daemon-owned leasing.** Plugins such as Pyright execute with `cwd=/testbed` and behave as if they are in a regular filesystem. Every exposed Pyright operation is still a host-level tool call (`lsp.hover`, `lsp.diagnostics`, `lsp.rename`, etc.). At tool-call entry, the host/daemon runs a freshness gate (`SandboxOverlay.ensure_current(...)`): if the manifest has advanced, the daemon rotates the active manifest lease and remounts `/testbed` with the newest lowerdirs plus the active upperdir. The plugin runtime itself does **not** acquire leases or compute `lowerdir` / `upperdir`; the backend/daemon does that before the plugin observes the filesystem. Plugin writes copy up into that upperdir and become durable through the same `publish_cycle` used by `command_exec`.
3. **Performance is protected by design; live sweep remains the final proof.** `command_exec` no longer pays per-command mount/umount overhead, and plugins no longer materialize a synthetic projection. Publish still pays capture + OCC + remount, but that replaces the old projection refresh path. The design is intended not to sabotage either plugin or `command_exec` latency, and Phase 0 records the required before/after p50/p95 timings to prove that under live provider conditions.
4. **Overlay projection disk overhead is O(1): confirmed for the mount, not for all storage.** The overlay mount itself is O(1) disk because `mount(2)` / the new mount API compose lowerdirs and upperdir in the kernel instead of copying a merged tree. This applies equally to plugin and `command_exec` operations because both use the same mounted `/testbed`. Total storage is not O(1): upperdir is O(writes-since-last-publish), and layer storage is O(committed history) until squash/flush compacts it.
5. **The command/plugin process view is a full Linux filesystem with `/testbed` replaced: confirmed.** The overlay does not replace `/`. The process namespace remains a complete container filesystem (`/bin`, `/usr`, `/tmp`, `/home`, `/proc`, etc.), with exactly `/testbed` mounted as `lowerdir+upperdir`. Shell commands and LSP servers can read files outside the workspace through the normal container filesystem, while workspace paths under `/testbed` are guarded by layer-stack + OCC.

## 2. Goals

1. Eliminate userspace materialize for plugins (~400 ms → ~3 ms remount).
2. One mental model: any consumer of `/testbed` sees the merged layer-stack view.
3. Enable plugin write capability (LSP rename, code actions, format) without new mechanism.
4. Decouple change notifications from any specific plugin protocol.
5. Preserve existing OCC semantics (GATED routing, `ManifestConflictError`, atomic manifest swap).
6. Keep mount disk overhead at O(1); bound layer storage via existing squash + new flush API.

## 3. Non-goals

- Cross-platform support (Linux Docker with CAP_SYS_ADMIN only).
- Per-actor isolation inside one sandbox (command_exec and plugins share upperdir intentionally).
- Tracking sandbox state outside `/testbed` (everything else is container FS).
- Live overlay-mount visibility for non-`/testbed` paths.
- A new flush trigger policy (the API exists; trigger is operator-decided).

## 4. Architecture

### 4.1 Filesystem layout

```
SANDBOX (Docker container, CAP_SYS_ADMIN)

  /                       container's root, normal FS
  ├── bin/, usr/, opt/    container image (read-only typically)
  ├── etc/                container image
  ├── proc/, sys/, dev/   kernel virtual filesystems
  ├── tmp/                ephemeral writable, container FS
  ├── home/               persistent writable, container FS
  ├── var/run/eos/        daemon-managed (hidden by convention)
  │   ├── overlay-upper/   ← upperdir (writable scratch for /testbed)
  │   └── overlay-work/    ← workdir (kernel bookkeeping for overlay)
  ├── var/lib/eos/stack/  layer storage (bind mount / volume)
  │   ├── manifest.json    versioned newest-first layer list
  │   ├── layers/B000001-base/   immutable base snapshot
  │   ├── layers/L000002-.../    upper layers
  │   └── ...
  └── testbed/            ← THE overlay mount, only special path

The overlay merges:
   upperdir = /var/run/eos/overlay-upper
   workdir  = /var/run/eos/overlay-work
   lowerdir+ = (newest-first) layers/L000NNN-.../, ..., layers/B000001-base/
```

The agent and every plugin see a complete Linux filesystem. The overlay covers exactly one path-prefix. Reads and writes outside `/testbed` go to the container FS via the normal mount table; the overlay does not intercept them.

### 4.2 Storage substrate (unchanged)

Existing layer-stack contracts remain authoritative:

- Layers are immutable directories under `<stack>/layers/`.
- Each layer encodes regular files, symlinks, whiteouts (`.wh.<name>`), and opaque-dir markers (`.wh..wh..opq`).
- Manifest is newest-first; version increments per publish.
- `LayerStack._lock` (RLock) serializes manifest mutations.
- `LeaseRegistry` refcounts layers pinned by readers; squash respects the pinned set.
- `MergedView` reads the merged view newest-first using per-layer indexes.

This spec does not modify any of the above. It changes who mounts and how plugins consume.

### 4.3 Mount mechanism

The daemon uses the new Linux mount API via direct syscalls (`fsopen` / `fsconfig(SET_STRING, "lowerdir+", path)` / `fsmount` / `move_mount`) as implemented in `backend/src/sandbox/execution/overlay/kernel_mount.py:mount_overlay()`. This API supports many layers without the `mount(8)` 16-layer cap. The mount itself contributes O(1) disk overhead — overlayfs composes the merged view in the kernel without writing to disk.

### 4.4 Lifecycle

```
sandbox start:
  1. daemon ensures /testbed contents are the base-repo source state
  2. build_workspace_base(workspace_root=/testbed, layer_stack_root=<stack>)
       → publishes B000001-base; manifest v=1
  3. SandboxOverlay.start():
       prepare upperdir, workdir
       mount_overlay(/testbed, layer_paths=v1.layer_paths, upperdir, workdir)
       lease = leases.acquire(v1, "sandbox:<id>")
       state = MOUNTED

sandbox running:
  - all reads/writes at /testbed go through overlayfs
  - command_exec runs commands at /testbed; triggers publish_cycle at command end
  - plugins (Pyright) read/write /testbed; LSP write tools trigger publish_cycle
  - daemon emits WorkspaceChangeEvent on each publish_cycle / foreign refresh

sandbox stop:
  1. optional final publish_cycle (catch any in-flight upper edits)
  2. SandboxOverlay.stop():
       umount2(/testbed, MNT_DETACH)
       release lease
       GC upperdir, workdir
       state = STOPPED
```

## 5. Core primitives

### 5.1 `publish_cycle`

Single primitive for any write boundary. Invoked by `command_exec` at command end, by plugin write tools at apply end, by explicit agent commit, and (in a variant) by foreign-publish refresh.

```
publish_cycle(reason: str) -> PublishResult:
  1. CAPTURE            (no daemon lock; reads upperdir only)
       walk upperdir → list[LayerChange]
       apply .gitignore filter
       hash modified files via ContentHasher
       build PreparedChangeset

  2. OCC PUBLISH        (holds stack._lock briefly)
       with stack.commit_transaction() as txn:
           validate against txn.snapshot() (GATED route by default)
           if conflict: raise ManifestConflictError
           txn.publish_layer(changes, source_root=staging_dir)
       result.new_manifest_version = txn.snapshot().version

  3. ROTATE LEASE       (under daemon lock)
       new_lease = leases.acquire(new_manifest, sandbox_id)
       leases.release(old_lease.lease_id)
       # this rotation unpins layers that fell out of the new manifest,
       # allowing squash GC to reclaim them

  4. REMOUNT            (userspace; ~ms)
       umount2(/testbed, MNT_DETACH)
       clear upperdir
       clear workdir
       mount_overlay(/testbed, layer_paths=new_manifest.layer_paths, upperdir, workdir)

  5. EMIT EVENT         (non-blocking; through event bus)
       event = WorkspaceChangeEvent(
                  reason=reason, from_version=N, to_version=N+1,
                  changes=derive_path_changes(captured_changes, old_manifest))
       overlay.event_bus.emit(event)

  return PublishResult(success=True, new_manifest_version=N+1, captured_changes=...)
```

**Ordering invariants:**

- STEP 4 (REMOUNT) must complete before STEP 5 (EMIT EVENT). Subscribers reading `/testbed` after receiving the event must see the new state.
- STEP 3 (ROTATE LEASE) sits between OCC PUBLISH and REMOUNT to keep the pinned set in sync with the active manifest. A failed STEP 4 must not advance the lease; rollback releases the new lease and restores the old.

**Foreign-publish variant:** when the daemon detects an externally-published manifest advance, it runs steps 3 → 4 → 5 with no capture and no OCC. The upperdir is preserved (the sandbox may have in-flight edits); the next local publish_cycle's OCC GATED route catches any conflicts.

### 5.2 `flush_to_workspace`

API that replays accumulated upper layers onto the base-repo files underneath the mount and rebuilds a fresh `B000001-base`. Operator-triggered; trigger policy held outside this spec.

```
LayerStack.flush_to_workspace(timings: dict | None = None) -> FlushResult:
  with daemon lock:
    if leases.active_count() > 1:
        raise FlushBlockedByLeasesError  # other consumers active
    set _flushing flag (persisted to <stack>/runtime/flushing.mark)

  # phase 2 (no daemon lock; long-running)
  final publish_cycle(reason="flush")     # catch any in-flight upper edits

  umount2(/testbed, MNT_DETACH)

  # O(delta) replay onto /testbed (the base-repo files reappear under the mount)
  for layer in reversed(manifest.layers[:-1]):    # oldest upper → newest upper
      MergedView._apply_layer(layer_dir, /testbed, share_inodes=False)

  # rebuild base layer fast
  build_workspace_base_fast(
      workspace_root=/testbed,
      layer_stack_root=<stack>,
      reset=True,
      hardlink_same_fs=True,          # same-FS optimization
      precomputed_hashes=True,        # use layer.content_hash from upper layers
  )
  # → new B000001-base; manifest v=1; new base_root_hash

  with daemon lock:
    rewrite WorkspaceBinding (new base_root_hash)
    clear _flushing flag

  background: rm old layer dirs (now unpinned)

  mount_overlay(/testbed, layer_paths=(new_base,), fresh upperdir, fresh workdir)
  new_lease = leases.acquire(v1, sandbox_id)
  return FlushResult(...)
```

**Why delta-apply, not whole-tree materialize:** the workspace files at `/testbed` (underneath the overlay) are already the byte-equal `B000001-base` from sandbox start. Only the upper layers' delta needs replaying. This is O(delta) writes vs. O(workspace_size).

**Why hardlink-rebuild:** on same-FS storage (Docker overlay2 + ext4 typical), `os.link()` from `/testbed` into the new `B000001-base/` is one inode bump per file vs. `shutil.copy2`'s full byte copy. Same correctness story as `_link_or_copy` in `view.py`.

**Why incremental hash:** the new `base_root_hash` can be computed by walking the layer delta and replacing entries by `(path, size, content_hash)` from layer indexes. Skips a multi-GB sha256 sweep. Phase 2 of implementation; phase 1 may use the simple full rehash.

## 6. command_exec integration

### 6.1 What changes

| Today | Unified design |
|---|---|
| Host/tool code may coordinate execution details | Host/tool code sends one `command_exec` request to the sandbox daemon |
| Mounts overlay per command | No-op for `command_exec` — daemon mount already in place |
| Holds a per-command lease | No per-command lease — daemon owns the active sandbox lease |
| Capture + OCC + publish at command end | Daemon calls `SandboxOverlay.publish_cycle(...)`; command runner does not call OCC directly |
| Unmount after command | No per-command unmount — `publish_cycle` remounts `/testbed` only when needed |

### 6.2 Files affected

- Host-facing `command_exec` tool wrapper — becomes a thin daemon RPC/API call.
- Daemon command service — runs the subprocess at `/testbed`, then calls the `SandboxOverlay` facade.
- `backend/src/sandbox/execution/*` — remove per-command mount/umount and direct OCC wiring; runner just spawns the subprocess at `/testbed`.
- Existing `overlay_capture.capture()` is reused as-is inside `publish_cycle`.
- Existing `sandbox/occ/commit_transaction.py:CommitTransaction.revalidate_and_publish` is reused as-is.

### 6.3 Command boundary contract

```
host command_exec tool:
  return daemon.command_exec(argv, cwd="/testbed", env=...)

daemon command_exec service:
  result = run_subprocess(argv, cwd="/testbed", env=...)
  publish = overlay.publish_cycle(reason=f"cmd:{command_id}")
  return CommandExecResult(result, publish)
```

The agent receives a strictly synchronous contract: when the tool returns, the command's effects are durable and the manifest has advanced.

`command_exec` must not import or call OCC, layer-stack, or low-level overlay internals. Those are daemon-owned implementation details behind `SandboxOverlay`.

## 7. Plugin integration

### 7.1 Plugin contract (universal)

Plugins inside the sandbox use plain POSIX at `/testbed`. They do not:

- Call `mount_overlay` or any layer-stack API.
- Acquire leases per operation.
- Maintain projection caches keyed by manifest version.
- Translate paths between any synthetic projection and the agent's view.

They may:

- Subscribe to the daemon's event bus for change notifications (optional; only stateful tools need this).
- Use any directory outside `/testbed` for their own scratch / cache (e.g., `PYRIGHT_CACHE_DIR=/var/run/eos/pyright-cache`).

Plugin processes are spawned by the plugin host daemon with `cwd=/testbed` and run as long-lived subprocesses for the sandbox lifetime.

### 7.1.1 Tool-call boundary

Every user-visible Pyright operation is a normal tool call even though the Pyright server process is long-lived. The host wrapper owns the tool-call boundary:

```
lsp tool call:
  1. SandboxOverlay.ensure_current(reason=f"lsp:{tool_name}:enter")
       # daemon-owned: check manifest, rotate lease/remount if needed
  2. invoke the long-lived Pyright session rooted at file:///testbed
  3. for read-only tools: return result
  4. for write tools: apply WorkspaceEdit to /testbed, then publish_cycle(...)
```

This preserves the product contract that each LSP operation sees the latest `/testbed` snapshot, while avoiding per-operation materialization or plugin-owned layer-stack logic.

### 7.2 Plugin read operations

Read-only plugin tools (`lsp.hover`, `lsp.find_definitions`, `lsp.find_references`, `lsp.diagnostics`, `lsp.query_symbols`) require zero new design. They:

1. Enter through the host tool wrapper.
2. Run `SandboxOverlay.ensure_current(...)`.
3. Read files at `/testbed/...` via plain POSIX.
4. Answer the query.
5. Return without publishing.

Pyright's in-memory symbol table is kept fresh by:

- Per-document sync in `pyright_session._sync_open_document` on every query touching an open doc (existing code).
- Workspace-wide invalidation via the daemon's change-event subscription (see §8).

### 7.3 Plugin write operations

Write-capable tools (`lsp.rename`, `lsp.apply_code_action`, `lsp.format`, `lsp.apply_workspace_edit`) follow this shape:

```
plugin write tool:
  1. SandboxOverlay.ensure_current(reason=f"lsp:{tool_name}:enter")
  2. ask Pyright for WorkspaceEdit via LSP request
  3. apply edits as POSIX writes to /testbed (atomic per-file: tmp + os.replace)
  4. publish_result = SandboxOverlay.publish_cycle(reason=f"lsp:{tool_name}")
  5. return apply_result
```

Writes land in upperdir via overlayfs copy-up. The same `publish_cycle` that command_exec invokes captures them, runs OCC GATED revalidation, and remounts.

**Two API surfaces:**

- `lsp.compute_<op>` — returns the proposed `WorkspaceEdit` without mutation (preview).
- `lsp.apply_workspace_edit(edit)` — writes + publishes.
- Convenience wrappers (`lsp.rename(symbol, new_name)`) bundle compute + apply for common cases.

**Atomicity policy:** per-file atomic via tmp + rename. Cross-file group atomicity is best-effort (a crash mid-apply may leave partial state; next publish_cycle captures whatever is present). For high-stakes WorkspaceEdits (project-wide rename), an opt-in `staging-then-swap` mode applies the full edit into a tmp directory first and atomically moves files into `/testbed` only after the full edit succeeds. Phase 2.

**Conflict handling:**

- On `ManifestConflictError` from publish_cycle, default policy is **strict atomic** — clear upperdir, remount with current latest manifest, return `ApplyResult(status="conflict")`. The agent decides retry/abandon.
- Opt-in **partial publish** policy is available where the per-path OCC routing returns per-path status; rejected paths are surfaced, accepted paths land in the new layer.

### 7.4 Plugin runtime changes

Files affected in `backend/src/plugins/catalog/lsp/runtime/`:

| File | Change |
|---|---|
| `paths.py` (`PathMapper`) | **Delete entire file.** Plugin paths are `/testbed/...` directly; no translation needed. |
| `session_manager.py` | Collapse manifest-keyed cache to a single Pyright session per sandbox. Drop `_stable_root` digest, `acquire`/`release` per-query lease acquisition. |
| `pyright_session.py` | Drop `lowerdir`, `_stable_root`, `_retarget_workspace_root`, `manifest_key` plumbing. `rootUri = file:///testbed`. Keep `_notify_workspace_refreshed` and `_sync_open_document` essentially unchanged. |
| `server.py` | Register new write tool handlers (`lsp.rename`, `lsp.apply_workspace_edit`, etc.). |
| `apply.py` (**new**) | `apply_workspace_edit(edit) -> ApplyResult`. Walks edit, writes files at `/testbed/...`, invokes `publish_cycle`. |
| `event_adapter.py` (**new**) | Subscribes to daemon event bus; translates `WorkspaceChangeEvent` → LSP `didChangeWatchedFiles`. |
| `tools/rename.py`, `tools/apply_workspace_edit.py`, `tools/format.py` (**new**) | Tool wrappers. |

Files affected in `backend/src/plugins/catalog/lsp/`:

- `plugin.md` — drop "Read-only in v1" constraint; add write tools to the catalog.

### 7.5 Plugin cache placement

Plugins must configure their internal caches outside `/testbed` so they do not pollute the layer stack:

```python
proc = await asyncio.create_subprocess_exec(
    "pyright-langserver", "--stdio",
    cwd="/testbed",
    env={**os.environ, "PYRIGHT_CACHE_DIR": "/var/run/eos/pyright-cache"},
    ...
)
```

This is per-plugin configuration; no daemon change.

## 8. Event bus (decoupled change notifications)

### 8.1 Why generic

The change notification primitive is not LSP-specific. The daemon emits a structured event; per-plugin runtimes translate to their own invalidation protocol. This keeps the daemon plugin-agnostic and lets future tools (Rust analyzer, observability sinks, agent caches) subscribe without daemon changes.

### 8.2 Event types

```python
# backend/src/sandbox/overlay/events.py (new module)

from typing import Literal
from dataclasses import dataclass

@dataclass(frozen=True)
class PathChange:
    path: str
    kind: Literal["write", "delete", "symlink", "opaque_dir"]
    existed_before: bool

@dataclass(frozen=True)
class WorkspaceChangeEvent:
    reason: Literal["publish", "foreign_publish", "flush", "remount", "full_resync"]
    from_version: int
    to_version: int
    changes: tuple[PathChange, ...]
```

### 8.3 Event bus surface

```python
class SandboxOverlayEventBus:
    def subscribe(self, subscriber_id: str, *, max_queue: int = 256) -> asyncio.Queue[WorkspaceChangeEvent]: ...
    def unsubscribe(self, subscriber_id: str) -> None: ...
    def emit(self, event: WorkspaceChangeEvent) -> None: ...   # non-blocking
```

The bus is owned by `SandboxOverlay`. Three methods. No knowledge of any plugin protocol.

### 8.4 Emit semantics

- **Non-blocking.** Slow subscribers do not block `publish_cycle`.
- **Bounded queue per subscriber.** When full, the oldest events are dropped and the subscriber is marked for `full_resync`; on next dequeue it receives a single `WorkspaceChangeEvent(reason="full_resync", changes=())` and must invalidate all of its state.
- **Coalesce on bursts.** Subscriber-side debounce (default 50 ms) merges adjacent events: union the change sets, prefer the latest `existed_before` per path. Implementation is in the subscriber adapter, not the bus.

### 8.5 LSP adapter (Pyright)

```python
class LspChangeEventAdapter:
    """Translates WorkspaceChangeEvent → workspace/didChangeWatchedFiles + textDocument/didChange."""

    def __init__(self, bus, pyright_client, sync_documents_fn):
        self._queue = bus.subscribe("lsp")
        self._client = pyright_client
        self._sync_documents = sync_documents_fn
        self._task = asyncio.create_task(self._loop())

    async def _loop(self):
        while True:
            event = await self._coalesce_burst(self._queue, max_wait_ms=50)
            if event.reason == "full_resync":
                await self._send_full_resync()
                continue
            await self._client.notify("workspace/didChangeWatchedFiles", {
                "changes": [
                    {"uri": f"file:///testbed/{c.path}",
                     "type": 1 if not c.existed_before else (3 if c.kind == "delete" else 2)}
                    for c in event.changes
                    if c.kind != "opaque_dir"  # opaque expanded separately
                ]
            })
            await self._sync_documents()  # textDocument/didChange for each open doc
```

This is the only place that knows about LSP. Replacing Pyright with another LSP server reuses the same adapter. Replacing LSP entirely (e.g., a Rust analyzer with a different protocol) means writing a different adapter, not touching the daemon.

### 8.6 Performance characteristics

Measured / estimated cost per event:

| Step | Cost |
|---|---|
| Daemon: build `WorkspaceChangeEvent` from `captured_changes` | ~10 µs for 1000 changes |
| Daemon: enqueue per subscriber | ~1 µs per subscriber |
| Subscriber: dequeue + coalesce | < 1 µs + bounded by debounce window |
| Subscriber: build LSP notification | ~5 µs per change |
| Subscriber: JSON encode + stdio write | ~10 µs per change |
| Pyright incremental re-index | Pyright-dominated; not the bus's concern |

For typical publish_cycle (1–50 changed paths), end-to-end notification overhead is sub-millisecond. Pathological mass-rewrite (10k paths) is ~15 ms — well below the publish_cycle's overall ms-scale budget, and outside the daemon's critical path because emit is non-blocking.

## 9. Filesystem scope

The agent and plugins see a complete Linux filesystem. Only `/testbed` is layer-stacked.

| Path | Mechanism | Layer-stack tracked? |
|---|---|---|
| `/testbed/...` | overlayfs (upper + lowers) | **Yes** |
| `/bin`, `/usr`, `/opt`, `/lib`, `/etc` | container image | No |
| `/proc`, `/sys`, `/dev` | kernel virtual FS | No |
| `/tmp`, `/home`, `/var/log`, `/var/cache` | container writable layer | No |
| `/var/run/eos/...` | daemon-managed (upperdir, workdir, plugin caches) | No |
| `/var/lib/eos/stack/...` | layer storage (mounted in) | No (this *is* the storage) |

### 9.1 Cross-boundary operations

- **Reads/writes anywhere outside `/testbed`:** go to the container FS via the normal mount table. The overlay does not intercept.
- **Cross-boundary copies:** `cp /tmp/x /testbed/y` works — `/tmp/x` read from container FS, write to `/testbed/y` goes through overlay to upperdir.
- **Symlinks from `/testbed/external → /opt/lib`:** the symlink inode is tracked in the overlay; following it resolves to `/opt/lib` (container FS) via standard Linux symlink resolution.
- **Hard links across the boundary:** fail with `EXDEV` (overlay and container FS are different filesystems). Use `cp` instead.
- **Bind mounts onto `/testbed/...`:** disallowed. `command_exec` policy should reject `mount` syscalls inside the sandbox unless daemon-authorized.

### 9.2 Off-workspace state caveats

- Building in `/tmp`, installing packages to `/home/user/.local`, populating `~/.cache` — all allowed, none reflected in the layer stack. Behavior may differ between two sandboxes from the same manifest if off-workspace state diverges. This is by design; reproducibility from the manifest applies to `/testbed` only.
- Plugin caches must live outside `/testbed` (per §7.5) to avoid polluting layer history.

### 9.3 Gitignored content under `/testbed`

Open question. Two policies:

- **Preserve in upperdir across publishes.** Gitignored content (`node_modules`, `.venv`, `__pycache__`) lives in upperdir, is filtered at capture (never becomes a layer), but survives remounts (upperdir cleared selectively, preserving filtered paths). Pro: transparent. Con: complicates upperdir clearing logic.
- **Relocate outside `/testbed`.** Symlink `/testbed/node_modules → /var/cache/eos/node_modules`. Container image / sandbox bootstrap installs the symlink. Pro: clean. Con: requires per-project knowledge of what to relocate.

Held decision. Phase 1 implementation may default to "preserve in upperdir" with the relocation pattern available as an escape hatch.

## 10. Performance

### 10.1 Per-operation costs

| Operation | Today | Unified design | Delta |
|---|---|---|---|
| Per-command mount + umount | ~4 ms | 0 (amortized) | saves ~4 ms/call |
| Plugin refresh (manifest change) | ~400 ms materialize | ~3 ms remount | **~130× faster** |
| Per-publish_cycle remount | n/a | ~3 ms | new cost, replaces materialize |
| Capture + OCC validate + publish layer | unchanged | unchanged | 0 |
| Notification emit (event bus) | n/a | < 1 ms typical | new cost, sub-ms |
| Plugin per-LSP-call path translation | `PathMapper` 3-way | none | small constant savings |

### 10.2 Steady-state costs

- VFS dentry/inode cache: cold after each remount. First post-remount walk of `/testbed` does fresh `stat()`s — transient 100–300 ms cost for workspace-wide operations. Re-warms under load.
- `MNT_DETACH` accumulation: detached mounts pin kernel memory until plugin fds close. Bounded by sandbox lifetime; reaped on session exit. Monitor `awk '/overlay/ {n++} END {print n}' /proc/self/mountinfo` if concerned.
- Overlay lookup depth: O(layer_count) per negative `stat()`. Mitigated by squash with low `max_depth` (recommend 10).

### 10.3 Disk overhead

- **Overlay mount itself: O(1) disk.** Kernel composes layers in memory; no disk written for the merged view.
- **Upperdir:** O(writes-since-last-publish). Cleared on every `publish_cycle`. Bounded.
- **Layer storage:** O(committed history). Bounded by squash (caps depth) and flush (resets to v=1).
- **`base-repo` files at `/testbed` underneath the overlay:** present, shadowed by the mount. With same-FS hardlinks during `build_workspace_base`, no byte duplication vs. `B000001-base`.

## 11. Risks and mitigations

### 11.1 Lease-related

| Risk | Mitigation |
|---|---|
| Persistent sandbox lease prevents squash from collapsing layers | **Rotating lease pattern:** every `publish_cycle` acquires a new lease against the new manifest, releases the old. Pinned set tracks the live manifest. Squash can collapse anything older that fell out. |
| Flush blocked by sandbox lease | Flush is sandbox-boundary or requires deliberate sandbox teardown. Document. |
| Cross-sandbox flush race | Flush rejects if any global lease is active, not just the local one. Implement via `LeaseRegistry.active_count()` covering all owners. |

### 11.2 Mount-related

| Risk | Mitigation |
|---|---|
| `umount` returns `EBUSY` because Pyright holds open fds | Use `umount2(MNT_DETACH)`. Old mount survives until fds close; new mount serves new opens. |
| Brief ENOENT window during umount→mount | Tools retry. Acceptable because publish_cycle is serialized with command_exec — no in-flight command races with its own publish. |
| inotify watches invalidated on remount | Acceptable — we synthesize change events from the daemon, not from inotify. |
| `MNT_DETACH` leaks if processes never close fds | Bounded by sandbox lifetime; container exit reaps kernel state. |
| Mount syscall fails mid-cycle (`ENOMEM`, etc.) | publish_cycle catches the failure, leaves `_flushing` flag set if applicable, marks sandbox unhealthy, surfaces error. Sandbox restart recovers. |

### 11.3 OCC / capture-related

| Risk | Mitigation |
|---|---|
| Plugin writes against stale base | OCC GATED route revalidates per-path; raises `ManifestConflictError`. Plugin daemon surfaces conflict to agent. |
| command_exec and plugin writes interleave in shared upperdir | No actor isolation by design. Agent serializes tool calls (existing convention). |
| Gitignored writes leak into layers | `sandbox/occ/gitignore.py` filter at capture time. |
| Cross-file group atomicity on plugin WorkspaceEdit | Per-file atomic (tmp + rename); group atomicity best-effort. Opt-in staging-then-swap for high-stakes operations (phase 2). |

### 11.4 Notification-related

| Risk | Mitigation |
|---|---|
| Slow subscriber blocks publish_cycle | Non-blocking emit, bounded queue, full-resync fallback. |
| Burst of publishes overwhelms LSP server | Subscriber-side coalescing with 50 ms debounce. |
| Subscriber sees stale state between events | Per-document sync via `_sync_open_document` on every LSP query guarantees per-query freshness regardless of event timing. |
| Foreign-publish detection lag | Tunable (inotify on manifest.json, or polling at 500–1000 ms). |

### 11.5 Flush-related

| Risk | Mitigation |
|---|---|
| Crash mid-flush | `_flushing` flag persisted; daemon restart sees it and either completes flush (idempotent under delta-apply) or rolls back. |
| Workspace half-flushed if crash between delta-apply and base rebuild | Recovery script: re-run flush — the delta-apply is idempotent (`_apply_layer` overwrites targets cleanly). |
| Hardlink rebuild corrupts base if same-FS but base is editable | Build_workspace_base only runs under the daemon lock; no concurrent writers can corrupt the base during rebuild. After rebuild, the base is immutable per layer-stack contract. |

## 12. Implementation phases

### Phase 0 — Round-2 contract probes

Goal: prove the intended contract before moving code across subsystem boundaries.

1. Add a narrow live/local probe that starts one sandbox overlay and runs:
   - `command_exec` from `cwd=/testbed`
   - a long-lived Pyright process from `cwd=/testbed`
   - a shell command that reads `/bin`, `/usr`, `/tmp`, and `/testbed`
2. Verify namespace shape:
   - `/testbed` is the only layer-stacked path
   - writes under `/testbed` copy up to overlay upperdir and publish through OCC
   - writes outside `/testbed` remain normal container state and do not enter layer-stack history
3. Capture baseline timings before and after the migration:
   - command execution p50/p95 wall time
   - `publish_cycle` p50/p95 broken down by capture, OCC publish, lease rotation, remount, event emit
   - LSP first-call cold path and warm `hover` / `diagnostics` / `find_references`
   - plugin refresh/remount cost after an external workspace change
4. Add a disk-growth probe:
   - confirm mount creation does not materialize a merged tree
   - confirm upperdir grows only with writes since last publish
   - confirm layer storage growth is committed-history growth, not overlay mount growth
5. Add import/path guards:
   - plugin runtime does not import layer-stack or overlay internals
   - plugin code does not reference `_transient_lowerdir`, `_stable_root`, or `PathMapper`
   - `command_exec` no longer performs per-command mount setup
   - each exposed Pyright operation enters through the host tool wrapper and calls `SandboxOverlay.ensure_current(...)`

**Acceptance:**

- A single command can read `/usr/bin`, write `/tmp/eos-probe`, and mutate `/testbed/probe.txt`; only the `/testbed` mutation appears in the captured layer changes.
- A Pyright session rooted at `file:///testbed` sees edits made by `command_exec` after the daemon event is emitted.
- A read-only Pyright tool call refreshes to the newest manifest without publishing a new layer.
- A write-capable Pyright tool call refreshes at entry, writes through `/testbed`, then publishes exactly once at tool-call exit.
- Overlay mount disk use remains constant for a no-op remount; upperdir and layer growth match the written bytes / committed changes.
- Baseline performance data exists before Phase 1 starts, so later phases can prove they did not sabotage `command_exec` or plugin latency.

### Phase 1 — Core unification

Goal: get `/testbed` to be a single persistent overlay shared by `command_exec` and plugins.

1. Add `SandboxOverlay` state machine + `start`/`stop`/`publish_cycle`/`foreign_refresh` methods.
2. Extend `kernel_mount.py` with `umount2(MNT_DETACH)` helper.
3. Adopt rotating-lease pattern in `publish_cycle`.
4. Move overlay mount from per-command (in `sandbox/execution/`) to sandbox start (in daemon bootstrap).
5. Reroute command_exec to invoke `publish_cycle` at command end (replacing inline capture+publish+umount).
6. Add `SandboxOverlayEventBus` + `WorkspaceChangeEvent` types.
7. Emit `WorkspaceChangeEvent` from `publish_cycle`.

**Acceptance:**

- Existing `command_exec` tests pass with the new mount lifetime.
- Mount overhead drops from ~4 ms/command to ~0.
- `LeaseRegistry.active_count()` shows exactly 1 lease per active sandbox, rotating on publishes.
- Squash can still collapse layers that fall out of the active manifest.

### Phase 2 — Plugin runtime migration

Goal: collapse the LSP projection machinery to a thin event-adapter pattern.

1. Add `LspChangeEventAdapter`; subscribe at plugin session start.
2. Migrate `pyright_session.py` to `rootUri = file:///testbed`. Remove `_stable_root`, `_retarget_workspace_root`, `manifest_key`, `lowerdir` tracking.
3. Delete `plugins/catalog/lsp/runtime/paths.py` (`PathMapper`).
4. Collapse `session_manager.py` to single-session-per-sandbox.
5. Configure `PYRIGHT_CACHE_DIR=/var/run/eos/pyright-cache` in subprocess env.
6. Update `plugin.md`.

**Acceptance:**

- Plugin tests pass with the new runtime.
- Per-LSP-query latency unchanged or improved.
- Plugin refresh cost drops from ~400 ms to ~3 ms.
- No code remaining references the synthetic `<stack>/runtime/_transient_lowerdir/` path.

### Phase 3 — Plugin write capability

Goal: enable LSP write tools through the unified publish primitive.

1. Add `lsp.apply_workspace_edit` LSP tool.
2. Add `apply.py` runtime helper.
3. Add convenience wrappers: `lsp.rename`, `lsp.apply_code_action`, `lsp.format`.
4. Wire OCC GATED route as the default capture route for plugin writes.
5. Implement conflict policy (strict atomic default).
6. Update `plugin.md` (drop read-only constraint).

**Acceptance:**

- `lsp.rename` round-trips: agent calls it, files updated at `/testbed`, manifest advances by 1, no upperdir residue.
- Concurrent foreign-publish triggers `ManifestConflictError`; agent receives `ApplyResult(status="conflict")`.
- Pyright sees the rename's results after the publish_cycle (verified via subsequent `find_references`).

### Phase 4 — Flush API

Goal: deliver `LayerStack.flush_to_workspace` as a library call.

1. Implement `FlushService` with delta-apply via `MergedView._apply_layer` onto `/testbed`.
2. Implement `build_workspace_base_fast` with same-FS hardlink path (phase 1 simple full rehash; phase 2 incremental rehash).
3. Persist `_flushing` flag to `<stack>/runtime/flushing.mark`.
4. Implement crash-recovery on daemon restart (check flag, complete or rollback).
5. Wire flush into the daemon as an explicit API; trigger policy held outside.

**Acceptance:**

- Flush with 0 active leases succeeds; post-state has `manifest.version == 1`, one `B000001-*` layer, fresh base_root_hash.
- Flush with > 1 active lease raises `FlushBlockedByLeasesError`.
- Crash-injection mid-delta-apply is recoverable on next flush.
- Performance: flush a workspace with 50k files and 20 layers in < 5 s (delta-apply + hardlink rebuild dominated).

### Phase 5 — Optional optimizations

- Per-file content hashes in `LayerIndex` for incremental rehash in flush.
- Notification debounce tuning based on real workload measurement.
- `staging-then-swap` mode for high-stakes WorkspaceEdits.
- Whiteout aggregation into opaque-dir markers when a directory accumulates many deletions.

## 13. Held decisions

1. **Flush trigger policy.** API is operator-callable; when to invoke is outside this spec.
2. **Gitignored content lifecycle in upperdir.** Preserve across remounts vs. relocate outside `/testbed`.
3. **Per-file hash chain in `LayerIndex`.** Phase 1 (simple rehash) vs. invest immediately.
4. **Plugin atomicity policy.** File-by-file atomic only vs. opt-in staging-then-swap.
5. **Foreign-publish detection mechanism.** Polling interval vs. inotify on `manifest.json` vs. external coordinator push.
6. **Multi-sandbox lease coordination.** Single-sandbox-per-stack (simple) vs. global lease registry (complex but flexible).

## 14. ADR summary

| Field | |
|---|---|
| **Decision** | Single per-sandbox R/W overlay mounted at `/testbed`, daemon-owned, persistent for sandbox lifetime, with a rotating lease that re-acquires on each publish. `command_exec` and plugin write tools both invoke one shared `publish_cycle` primitive. Plugin runtime collapses to a thin event-adapter pattern over a generic `SandboxOverlayEventBus`. Mount uses the new mount API (`fsopen` / `fsconfig(lowerdir+)` / `fsmount` / `move_mount`). |
| **Drivers** | Eliminate ~400 ms materialize per plugin refresh; one mental model for all sandbox consumers; reuse proven OCC and capture code; enable plugin write capability with no new mechanism; decouple notification from protocol. |
| **Alternatives considered** | (1) Per-command overlay (today; can't share with plugins). (2) Materialize-based projection for plugins (today; ~100× too slow; complex). (3) Per-plugin overlay via `acquire_plugin_workspace` (rejected — plugins shouldn't call leasing APIs). (4) Read-only overlay (rejected — need write capability). (5) Separate plugin mountpoint outside `/testbed` (rejected — reintroduces `PathMapper`). (6) Daemon emits LSP notifications directly (rejected — protocol coupling). |
| **Why chosen** | Plugins use POSIX at `/testbed`; daemon's existing overlay machinery handles everything; no new API surface for plugins; LSP write capability falls out of the unified `publish_cycle` without new mechanism; generic event bus keeps the daemon plugin-agnostic. |
| **Consequences** | (a) No actor isolation between command_exec and plugins inside one sandbox (intentional). (b) `MNT_DETACH` accumulates kernel mounts across publishes; bounded by sandbox lifetime. (c) Persistent sandbox lease blocks flush during a live sandbox (sandbox-boundary operation). (d) Cold VFS cache after every remount (~100–300 ms first-walk spike). (e) Plugin caches must be configured outside `/testbed`. (f) Plugin runtime drops `PathMapper`, `_stable_root`, manifest-keyed cache; pyright_session simplifies dramatically. |
| **Follow-ups** | See §13 Held decisions. |

## 15. Glossary

- **`/testbed`** — the agent-facing workspace path; the only overlay mount in the sandbox.
- **publish_cycle** — capture upperdir → OCC validate + publish → rotate lease → remount lowers → emit event.
- **rotating lease** — per-sandbox lease that re-acquires against the new manifest on each publish; releases the old lease so layers that fell out of the manifest become unpinned.
- **upperdir / workdir** — overlayfs scratch areas managed by the daemon at `/var/run/eos/overlay-upper/` and `/var/run/eos/overlay-work/`.
- **lowerdir+** — overlayfs's per-layer lower argument; supports many layers via the new mount API.
- **MNT_DETACH** — `umount2` flag that detaches a mount lazily; existing fds remain valid against the detached instance until they close.
- **B000001-base** — canonical name for the workspace base layer; produced by `build_workspace_base` and rebuilt by flush.
- **GATED route** — OCC validation route that re-reads the current merged content per path before publishing; catches stale-base writes.
- **WorkspaceChangeEvent** — generic event the daemon emits at publish boundaries; subscribers (LSP adapter, future tools) translate to their own protocols.
- **SandboxOverlay** — the daemon component that owns the `/testbed` mount lifecycle, the event bus, and the publish_cycle primitive.

## 16. References

- `backend/src/sandbox/layer_stack/` — layer storage substrate
- `backend/src/sandbox/execution/overlay/kernel_mount.py` — `mount_overlay` syscall sequence
- `backend/src/sandbox/execution/overlay/capture.py` — upperdir → `LayerChange[]`
- `backend/src/sandbox/occ/commit_transaction.py` — OCC validation + publish
- `backend/src/sandbox/layer_stack/view.py` — `MergedView`, `_apply_layer` (reused by flush)
- `backend/src/sandbox/layer_stack/workspace_base.py` — `build_workspace_base` (extended for fast rebuild)
- `backend/src/sandbox/layer_stack/lease.py` — `LeaseRegistry` (rotating lease pattern)
- `backend/src/plugins/catalog/lsp/runtime/` — LSP plugin runtime (simplified by phase 2)
