# 06 — Crate Map, Acyclic Severings, and Reproduce-Exactly Constants

**Status:** FROZEN CONTRACT (source-of-truth pass). Every claim below was verified against the live Python sandbox runtime at `backend/src/sandbox/` on the `main` branch (commit `f0c70b165`), 2026-05-31. Cited as `path:line` relative to `backend/src/sandbox/`. Where a plan anchor was imprecise the corrected anchor is given and noted in §D (risks).

Scope: plan §1 (External project layout) + §11 (Resulting structure & loose-coupling contract), cross-checked against §0 (PND/PPC/MF-1) and §12 (verified env).

A Rust author who never reads the Python should be able to lay out the current
10-runtime-crate workspace plus the `xtask` packaging helper and reproduce every
constant byte-for-byte from this file alone.

---

## CRITICAL METHODOLOGY NOTE — crate edges are NOT package edges

Two runtime crates are **carved out of Python directories that map to other crates**; they have no 1:1 directory. Naive `grep "from sandbox.occ"` by current directory gives *package* edges, which over- or under-attribute dependencies. The edges in §A are computed at **crate-source granularity**, not directory granularity. The carve-outs:

- **`eos-runner`** sources span two directories: `overlay/namespace_runner.py` + `overlay/namespace_entrypoint.py` (fresh-ns path) AND `isolated_workspace/scripts/setns_exec.py` + `setns_overlay_mount.py` + `_setns_libc.py` + `configure_dns_in_ns.py` (setns mode, PND). These live *inside* `overlay/` and `isolated_workspace/` today.
- **`eos-ns-holder`** = the single file `isolated_workspace/scripts/ns_holder.py`, today *inside* `isolated_workspace/`.

Therefore `eos-isolated`'s real deps are computed **excluding `scripts/ns_holder.py`, `scripts/setns_*`, `scripts/_setns_libc.py`, `scripts/configure_dns_in_ns.py`** (those become eos-runner/eos-ns-holder), to avoid over-attributing their deps to eos-isolated.

---

## A) The Runtime Crate Dependency Edge Table

The table below is the current Rust direct Cargo graph as of 2026-06-02 for the
10 runtime crates; the workspace also contains `xtask` for packaging. The
source-derived carve-out rationale remains the evidence for ownership, but the
implementation deliberately avoids some direct edges by injecting narrow ports
from `eos-daemon` and by spawning `eosd ns-holder` / `eosd ns-runner` children
instead of linking those runtime children into `eos-isolated`.

| # | Crate | Internal-crate deps (one-way, leaf→root) | Key external deps | Threading model |
|---|-------|------------------------------------------|-------------------|-----------------|
| 1 | **eos-protocol** | **(none — must depend on nothing internal)** | `serde`, `serde_json` | N/A (pure types) |
| 2 | **eos-ns-holder** | **(none internal)** | `libc`, `thiserror`; Linux target `rustix`; dev tests `nix` | **single-threaded, syscall-only, NO tokio** (kernel requirement: `unshare(CLONE_NEWUSER\|NS\|PID\|NET)` must run single-threaded) |
| 3 | **eos-runner** | `eos-overlay` (uses `kernel_mount`), `eos-protocol` (ToolCallRequest/Result, Intent) | `serde`, `serde_json`, `thiserror`; Linux target `rustix`, `libc`, `regex` | **single-threaded, syscall-only, NO tokio** for namespace entry; no direct `nix` edge |
| 4 | **eos-layerstack** | `eos-protocol` | `serde`, `serde_json`, `rustix`, `sha2`, `thiserror` | single-threaded core + per-root `RLock`-equivalent (reentrant) mutex; no tokio |
| 5 | **eos-overlay** | `eos-protocol` | `sha2`, `thiserror`; Linux target `rustix`, `libc` (raw `fsopen`/`fsconfig`/`fsmount`/`move_mount`) | syscall path single-threaded; no tokio required |
| 6 | **eos-occ** | `eos-overlay` (one-way only), `eos-protocol` | `serde`, `thiserror` | dedicated single **writer thread** per `layer_stack_root` (`occ-commit-queue`); daemon injects layer-stack-backed maintenance/transaction ports |
| 7 | **eos-isolated** | `eos-overlay` — **NOT `eos-occ`** (build-time no-publish guarantee) | `serde_json`, `thiserror`, `nix`; Linux target `futures-util`, `libc`, `netlink-sys`, `rtnetlink`, `tokio` | daemon-owned orchestration; ns syscalls delegated to spawned holder/runner children, and Linux netlink helpers are target-gated |
| 8 | **eos-plugin** | `eos-protocol` — **NOT `eos-occ`**, `eos-overlay`, `eos-layerstack`, `nix`, or `tokio` | `serde`, `serde_json`, `thiserror` | pure plugin manifest/service/refresh/PPC contracts |
| 9 | **eos-plugin-ops** | `eos-plugin`, `eos-config`, `eos-ephemeral-workspace`, `eos-isolated-workspace`, `eos-layerstack`, `eos-namespace` | `serde`, `serde_json`, `sha2`, `thiserror`; Linux target `nix` | plugin package publishing, service processes, PPC transport, dispatch, snapshot refresh, OCC callbacks, and oneshot overlays |
| 10 | **eos-daemon** | `eos-protocol`, `eos-layerstack`, `eos-overlay`, `eos-occ`, `eos-isolated`, `eos-runner`, `eos-plugin`, `eos-plugin-ops` — implements + injects the port traits lower crates define, and spawns holder/runner children by binary subcommand | `serde`, `serde_json`, `sha2`, **`tokio`**, `tokio-util`, `thiserror`; Linux target `nix` | **tokio control plane** for AF_UNIX + TCP async multiplexing and PTY/session supervision |
| 11 | **eosd** | `eos-daemon`, `eos-runner`, `eos-ns-holder`, `eos-overlay`, `eos-protocol` (subcommand dispatch only) | `anyhow`, `serde_json`, `tokio` | binary entry; `eosd daemon` / `eosd ns-runner` / `eosd ns-holder` |

### Enforced-edge checklist (source evidence plus current Rust edge)

| Required edge | Status | Source evidence / current Rust edge |
|---|---|---|
| `eos-protocol` depends on nothing internal | ✅ | by construction — SoT crate; plan §1 line 87 |
| `eos-layerstack ← eos-protocol` | ✅ | `layer_stack/` has ZERO imports of `sandbox.occ` / `sandbox.overlay` (base layer) |
| `eos-overlay ← eos-layerstack` | ✅ source, intentionally severed in Rust | `overlay/` imports `sandbox.layer_stack.*`; Rust `eos-overlay` now consumes concrete lowerdir inputs and protocol changes without a direct `eos-layerstack` Cargo edge. `overlay/` imports of `sandbox.occ` = **NONE** (no back-edge). |
| `eos-occ ← eos-layerstack (+ eos-overlay, one-way only)` | ✅ source, intentionally port-inverted in Rust | occ's ONLY overlay import: `occ/overlay_change_conversion.py:16` → `sandbox.overlay.path_change.OverlayPathChange`. Rust `eos-occ` keeps the `eos-overlay`/`eos-protocol` edge and accepts daemon-injected layer-stack transaction/maintenance ports instead of directly linking `eos-layerstack`. |
| Python `ephemeral_workspace/` source edge | ✅ source, intentionally not mirrored as a Rust crate | `ephemeral_workspace/` (excl. plugin): `overlay`, `occ`, AND direct `layer_stack` imports at `ephemeral_workspace/pipeline.py` + `ephemeral_workspace/pipeline_registry.py`. The old Rust `eos-ephemeral` crate was removed; daemon/runner/OCC crates own the concrete overlay, publish, and namespace paths. |
| `eos-isolated ← overlay + runner + ns-holder + layerstack, NOT eos-occ` | ✅ current Rust no-publish guarantee | isolated control-plane (excl. scripts/) touches occ ONLY via `_control_plane/pipeline_registry.py:22` → `sandbox.occ.layer_stack_adapter` (the HINGE). Current Rust preserves **no `eos-occ` edge** and narrows direct deps to `eos-overlay`; daemon injects the layer-stack snapshot/lease port and spawns holder/runner children. |
| Historical `eos-plugin ← ?` edge | ✅ RESOLVED | plugin's ONLY occ import is `ephemeral_workspace/plugin/projection.py:10` → `sandbox.occ.layer_stack_adapter` (the SAME HINGE adapter, used for snapshot/lease/projection — never publish). Current Rust `eos-plugin` is narrowed to `eos-protocol` plus serde/JSON/error support; it links neither `eos-occ`, `eos-overlay`, nor `eos-layerstack`. `PluginServiceKey`, `ServiceMode`, `RefreshStrategy`, manifest validation, refresh messages, public op names, and PPC frames are pure contracts. WRITE_ALLOWED publish now lives in `eos-plugin-ops`, with daemon adapters only shaping wire responses. |
| `eosd ← all` | ✅ adjusted | binary subcommand dispatch depends on `eos-daemon`, `eos-runner`, `eos-ns-holder`, `eos-overlay`, and `eos-protocol`. The daemon links the runtime service crates it actually calls and spawns holder/runner subcommands instead of depending on `eos-ns-holder` directly. |

### tokio vs no-tokio justification (task requirement)

- **tokio control-plane crates = `eos-daemon` and `eosd`; `eos-isolated` has Linux-target tokio only for netlink helpers.** The daemon justification comes from source: `daemon/rpc/server.py` uses `asyncio.start_unix_server` (`:183`) AND `asyncio.start_server` for loopback TCP (`:193`) plus `loop.add_signal_handler` (`:219`) — concurrent AF_UNIX + TCP connection multiplexing with signal-driven shutdown. The current Rust isolated netlink implementation also pulls target-gated `tokio`/`rtnetlink`, but namespace syscalls still run in dedicated single-threaded holder/runner children.
- **no-tokio, single-threaded, syscall-only = `eos-runner` + `eos-ns-holder`.** This is a **kernel requirement, not a style choice** (plan §0 line 43, §1 line 87): `unshare(CLONE_NEWUSER)` (ns-holder create) and `setns()` into a user namespace (runner setns mode) both require the calling process to be single-threaded. Neither can run inline in the multithreaded tokio daemon; both live in dedicated single-threaded children. (NB: the *current Python* `overlay/namespace_runner.py` uses `asyncio` to orchestrate the subprocess wait — but the actual `unshare`/`setns` syscalls execute in the spawned single-threaded child `namespace_entrypoint.py` / `setns_exec.py`, not in the async parent. The Rust target correctly places those crates as no-tokio.)

---

## B) The HINGE + the 4 Acyclic Severings

### B.1 The HINGE — snapshot/lease access must stay outside `eos-occ`

**File:** `backend/src/sandbox/occ/layer_stack_adapter.py` — class `LayerStackPortAdapter` (`:17`).

Verified facts (the reason the `eos-isolated ⊥ eos-occ` and `eos-plugin ⊥ eos-occ` guarantees hold):

1. **Imports only `sandbox.layer_stack.*`** — `commit_staging` (`:8`), `manifest` (`:9`), `stack` (`:10-13`). It is semantically a layer-stack forwarder.
2. **Its ONLY `occ` reference is a type annotation.** `from sandbox.occ.ports import LayerCommitTransaction` (`:14`), used solely as the return-type annotation of `begin_transaction()` (`:48`: `AbstractContextManager[LayerCommitTransaction]`). No occ behavior is invoked.
3. **It is the SINGLE site each of `isolated_workspace` and the `plugin` layer touch `occ/`:**
   - `isolated_workspace/_control_plane/pipeline_registry.py:22` → `from sandbox.occ.layer_stack_adapter import LayerStackPortAdapter` (the ONLY `sandbox.occ` import in all of `isolated_workspace/`).
   - `ephemeral_workspace/plugin/projection.py:10` → `from sandbox.occ.layer_stack_adapter import LayerStackPortAdapter` (the ONLY `sandbox.occ` import in all of `ephemeral_workspace/plugin/`).
   - Both use it for **snapshot/lease (+ projection), never publish.**

**Rust placement rule:** Keep snapshot/lease access outside `eos-occ`, split
from publish transactions. Current Rust uses two narrow forms of that rule:
`eos-isolated` defines a small `LayerStackSnapshotPort` and the daemon injects
the layer-stack-backed implementation. `eos-plugin` no longer consumes any
snapshot/lease surface directly; service-process refresh, projection, and
publish callbacks are daemon-owned. The publish-transaction half —
`begin_transaction` → `LayerCommitTransaction`, `allocate_commit_staging`,
`drop_commit_staging` (`layer_stack_adapter.py:48-55`) — remains owned by
eos-occ/daemon publish paths. The snapshot/lease half — `acquire_snapshot`
(`:57`), `release_lease` (`:66`), `read_active_manifest` (`:31`),
`read_bytes`/`read_text` (`:34-46`), plus `can_squash`/`squash` (`:69-73`) —
is what isolated mode and daemon-owned plugin refresh need. **If this surface
moves back into `eos-occ`, eos-isolated would be forced to link `eos-occ` and
the build-time no-publish guarantee would silently break.**

### B.2 The 4 acyclic severings (sever current upward Python edges → leaf→root crate graph)

| # | Current upward edge | Severing | Source evidence |
|---|---|---|---|
| 1 | audit event-type **schema** referenced upward | Move the **pure** schema (dataclasses + `build_*` constructors) into **`eos-protocol`** | `daemon/audit_schema.py` — `DaemonSection`/`LayerStackSection`/`OverlayWorkspaceSection`/`IsolatedWorkspaceSection`/`OccSection`/`PluginSection`/`BackgroundToolSection`/`ToolCallSection`/`OsResourceSection` (`:28-291`) are pure `@dataclass` + `_drop_none` + `build_*` (typing only). **PARTIAL — see precise split below.** |
| 2 | `occ_runtime_services` accessor imported upward by ephemeral/isolated | Invert into a **port trait** the lower crate defines; `eos-daemon` implements + injects | `daemon/occ_runtime_services.py:48` `get_occ_runtime_services(layer_stack_root)`; imported by `ephemeral_workspace/pipeline_registry.py`, `daemon/workspace_tool/dispatch.py`, etc. |
| 3 | `layer_stack_runtime` accessor imported upward | Invert into a **port trait**; `eos-daemon` implements + injects | `daemon/layer_stack_runtime.py`; imported by `isolated_workspace/__init__.py`, `isolated_workspace/_control_plane/pipeline_registry.py:20`, `ephemeral_workspace/plugin/runtime_api.py`, `daemon/occ_runtime_services.py` |
| 4 | `changeset_projection` / dispatch **drain-gate** | Invert into a **port trait**; `eos-daemon` implements + injects | located in `daemon/workspace_tool/dispatch.py` + `daemon/workspace_tool/payloads.py` |

**Confirmed one-way (no back-edge to undo):** `occ → overlay` only — `occ/overlay_change_conversion.py:16` → `overlay.path_change`; `overlay/` has **zero** `occ` imports. So the occ↔overlay axis is already acyclic; the severings above are about daemon-side accessors and the audit schema, not the occ/overlay axis.

### B.3 PRECISE audit-schema split (severing #1 is NOT a whole-module move)

`daemon/audit_schema.py` is **mostly** pure but contains **two impure functions that MUST stay in `eos-daemon`**:

- `safe_emit(event, lane)` (`:294`) — lazy-imports `sandbox.daemon.audit_buffer.get_audit_buffer` (`:303`). Impure, daemon-side.
- `safe_record_phase(phase, duration_ms)` (`:310`) — lazy-imports `engine.tool_call.phase_buffer.record_phase` (`:323`). Impure, reaches into the (out-of-scope) `engine` package.

→ **Move to `eos-protocol`:** the 9 `@dataclass` sections + `_drop_none` + the 9 `build_*_event` constructors + the `Lane` type alias (`:14`). **Keep in `eos-daemon`:** `safe_emit` + `safe_record_phase`. The task's "confirm pure dataclass/typing (movable into eos-protocol)" holds for the **schema types + builders only**, not the whole module.

---

## C) Reproduce-Exactly Constants Table

Every value verified against source. Rust authors: these are the byte-for-byte constants the port must reproduce.

### C.1 OCC commit queue (`occ/commit_queue.py`)

| Constant | Value | Source | Notes |
|---|---|---|---|
| Commit-queue writer thread name | `"occ-commit-queue"` | `occ/commit_queue.py:90` (`name="occ-commit-queue"`) | **single** daemon-thread writer; `daemon=True` (`:91`) |
| `max_batch_size` (default) | `64` | `occ/commit_queue.py:66` | clamped `max(1, int(...))` (`:73`) |
| `batch_window_s` (default) | `0.002` (2 ms) | `occ/commit_queue.py:67` | clamped `max(0.0, float(...))` (`:74`); window only paid when drain emptied queue AND headroom remains (`:143`) |
| `MAX_OCC_CAS_RETRIES` | `3` | `occ/commit_queue.py:27` (`MAX_OCC_CAS_RETRIES: int = 3`) | also the default `max_cas_retries` (`:68`); validated `>= 1` (`:70`); on exhaustion → all paths `ABORTED_VERSION` (`:296`) |

### C.2 OCC service + maintenance (`occ/service.py`, `occ/maintenance.py`)

| Constant / fact | Value | Source | Notes |
|---|---|---|---|
| Sync offload primitive | `run_sync_in_executor` | `occ/service.py:32` import; used `:173` (maintenance), `:240` (prepare) | dispatches to dedicated sandbox executor that does NOT copy contextvars (`shared/async_bridge.py:258-292`); re-seeds only `sandbox_io_loop` |
| `AUTO_SQUASH_MAX_DEPTH` | `100` | `occ/service.py:34` | distinct from the layer-stack 16-layer mount ceiling |
| `AutoSquashMaintenancePolicy` | class | `occ/maintenance.py:29` | per-policy `threading.Lock` `_squash_lock` (`:44`) |
| `_LayerSquashPort` Protocol | `can_squash(max_depth)` / `squash(max_depth)` | `occ/maintenance.py:21-26` | |
| `MaintenancePolicy` Protocol | `after_publish_sync(result)` | `occ/maintenance.py:15-18` | |

### C.3 The RLock (reentrant) deadlock trap (`layer_stack/storage_lock.py`)

The "RLock" the task warns about is the **layer-stack storage writer lock**, NOT an OCC-service lock. `occ/service.py` itself holds no RLock — it offloads sync work via `run_sync_in_executor`; the reentrant lock is taken inside the executor-offloaded transaction.

| Constant / fact | Value | Source | Notes |
|---|---|---|---|
| Single-owner advisory lease | `fcntl.flock(fd, fcntl.LOCK_EX \| fcntl.LOCK_NB)` | `layer_stack/storage_lock.py:71` | raises `RuntimeError` if root already owned by another process (`:74-77`); released with `LOCK_UN` (`:55`) at refcount 0 |
| Lock file name | `.storage-writer.lock` | `layer_stack/storage_lock.py:13` | opened `O_RDWR\|O_CREAT, 0o644` (`:69`) |
| **Per-root reentrant mutex** | `threading.RLock` | `layer_stack/storage_lock.py:22` (field), `:78` (created) | **REENTRANT** — a naive Rust port to `std::sync::Mutex` (non-reentrant) DEADLOCKS when re-acquired on the same thread. Use a reentrant guard or restructure so re-entry is impossible. |
| Process-wide refcount registry | `_STORAGE_WRITER_LOCKS: dict[str, _StorageWriterLock]` | `layer_stack/storage_lock.py:14` | guarded by `_STORAGE_WRITER_LOCKS_LOCK` (`threading.Lock`, `:15`); `refcount` increments on re-acquire (`:65`), decrements on release (`:51`) |
| Registry key | `str(storage_root.resolve())` | `layer_stack/storage_lock.py:61` | canonicalized absolute path |
| Mutex consumers | `.exclusive()` | `layer_stack/transaction.py:45`, `layer_stack/stack.py:365` | both layers (fcntl lease + RLock) MUST be reproduced: flock guards cross-process; RLock serializes multiple in-process managers after cache drops/overlay resets (`:33-40`) |

### C.4 Layer-stack squash (`layer_stack/squash.py`)

| Type | Source | Notes |
|---|---|---|
| `SquashPlan` (frozen dataclass) | `layer_stack/squash.py:32` | fields `active_version`, `active_layers`, `entries`; requires ≥1 checkpoint segment (`:43`) |
| `CheckpointSegment` (frozen dataclass) | `layer_stack/squash.py:20` | ≥2 layers per segment (`:24-26`) |
| `LayerCheckpointSquasher` | `layer_stack/squash.py:51` | `plan()` (`:61`), `build_checkpoint()` (`:95`), `relabel_checkpoint()` (`:115`), `discard_checkpoint()` (`:128`) |
| Checkpoint id format | `f"B{next_version:06d}-{uuid4().hex[:8]}"` | `layer_stack/squash.py:179-180` | reproduce exactly for layer-id parity |
| Storage layout (one fs) | `storage_root/{layers,staging}` | `layer_stack/paths.py:107-108` | `staging/` + `layers/` under one `storage_root` (12.1 / CP-1b item ii) |

### C.5 Overlay raw-syscall mount (`overlay/kernel_mount.py`)

| Fact | Source | Notes |
|---|---|---|
| Raw new-mount API (no `mount(8)`) | `overlay/kernel_mount.py:63-70` | `fsopen(b"overlay")` (`:63`) → `fsconfig_string(fsfd, b"lowerdir+", ...)` per layer (`:65`) → `fsconfig_string(b"upperdir", ...)` (`:66`) → `fsconfig_string(b"workdir", ...)` (`:67`) → `fsconfig_create(fsfd)` (`:68`) → `fsmount(fsfd)` (`:69`) → `move_mount(mfd, workspace_root)` (`:70`) |
| Ordering invariant | `overlay/kernel_mount.py:6` | first `fsconfig(SET_STRING, "lowerdir+", path)` per layer in newest-first order |
| `move_mount` dest constraint | `overlay/kernel_mount.py:149` | does NOT accept a `/proc/self/fd` symlink as destination |

### C.6 Namespace runner (`overlay/namespace_runner.py`, `overlay/namespace_entrypoint.py`)

| Fact | Source | Notes |
|---|---|---|
| Fresh-ns path | `overlay/namespace_runner.py:72` `_run_tool_call_in_fresh_namespace` | dispatched from `run_in_namespace` (`:48`) |
| Existing-ns (setns) path | `overlay/namespace_runner.py:138` `_run_tool_call_in_existing_namespace` | |
| Fresh-ns entrypoint exec | `overlay/namespace_runner.py:227-250` `_run_namespace_entrypoint_async` | uses `_unshare_path()` (`:238` = `shutil.which("unshare")`, `:332-333`) |
| **Process-group / new session** | `start_new_session=True` | `overlay/namespace_runner.py:250` | the plan's "start_new_session process-group" anchor; corrected line (plan said ~237-244) |
| Private-mount-ns detect | `overlay/namespace_runner.py:314-330` `detect_private_mount_namespace` | probes `unshare -Urm true` |

### C.7 Plugin op registry intents (`ephemeral_workspace/plugin/op_registry.py`)

| Constant / fact | Value | Source | Notes |
|---|---|---|---|
| `Intent` enum | `READ_ONLY` / `WRITE_ALLOWED` / `LIFECYCLE` | imported from `sandbox.shared.models` (`op_registry.py:34`); semantics documented `:14-23` | `READ_ONLY` → in-process (no overlay, no ns child, no publish_cycle); `WRITE_ALLOWED` → overlay+OCC publish; `LIFECYCLE` → rejected at registration (`:117-120`) |
| `auto_workspace_overlay` default | `True` | `op_registry.py:73` (dataclass field) AND `:86` (param) | `False` = self-managed publish (LSP `apply.py`); branch at `:226` skips the standard wrapper to keep publish path UNCHANGED (plan's `:227` ≈ `:226`) |
| importlib plugin-load path | `importlib.import_module(f"plugins.catalog.{plugin_name}.runtime.server")` | `ephemeral_workspace/plugin/overlay_child.py:129` (import `:7`) | the path a Rust daemon CANNOT reproduce → replaced by the PPC out-of-process protocol |
| Public op name format | `f"plugin.{plugin}.{op}"` | `op_registry.py:206` | |

### C.8 OCC runtime services keying (MF-1 single-writer)

| Constant / fact | Value | Source | Notes |
|---|---|---|---|
| Services factory key | `layer_stack_root` | `daemon/occ_runtime_services.py:48` `get_occ_runtime_services(layer_stack_root)` | the singleton owner that keys the ONE `occ-commit-queue` writer per root (MF-1) |
| Services cache | LRU `OrderedDict`, lock-guarded | `daemon/occ_runtime_services.py:52,84-90` | self-managed plugin callback MUST route through THIS same per-root services instance, never a second writer |
| Cache max | `256` | `daemon/occ_runtime_services.py:43` (`_OCC_RUNTIME_SERVICES_CACHE_MAX = 256`) | |

### C.9 Isolated-workspace constants — `from_env()` defaults (`isolated_workspace/_control_plane/types.py`)

| Constant | Value | Env var | Source |
|---|---|---|---|
| TTL | `1800.0` s | `EOS_ISOLATED_WORKSPACE_TTL_S` | `types.py:166` |
| Total cap | `5` | `EOS_ISOLATED_WORKSPACE_TOTAL_CAP` | `types.py:167` |
| Upperdir bytes | `1073741824` (= `1024*1024*1024` = 1 GiB) | `EOS_ISOLATED_WORKSPACE_UPPERDIR_BYTES` | `types.py:168-170` |
| MemAvail fraction | `0.5` | `EOS_ISOLATED_WORKSPACE_MEMAVAIL_FRACTION` | `types.py:171` |
| Setup timeout | `30.0` s | `EOS_ISOLATED_WORKSPACE_SETUP_TIMEOUT_S` | `types.py:172` |
| Exit grace | `0.25` s (clamped ≥0) | `EOS_ISOLATED_WORKSPACE_EXIT_GRACE_S` | `types.py:173-176` |
| RFC1918 egress | `"allow"` (set `"deny"` to deny) | `EOS_ISOLATED_WORKSPACE_RFC1918_EGRESS` | `types.py:177-179` |
| Fallback DNS | `"1.1.1.1"` | `EOS_ISOLATED_WORKSPACE_FALLBACK_DNS` | `types.py:180` |
| Sample interval | `0.5` s (clamped ≥0.01) | `EOS_ISOLATED_WORKSPACE_SAMPLE_INTERVAL_S` | `types.py:181-184` |
| Enabled | `False` | `EOS_ISOLATED_WORKSPACE_ENABLED` | `types.py:165` |
| `HANDLE_PREFIX` | `"eos-iws-"` | n/a (module const) | `types.py:19` |
| `PERSISTED_HANDLES_SCHEMA_VERSION` | `1` | n/a | `types.py:18` |
| EOS workspace root | `"/testbed"` | `EOS_WORKSPACE_ROOT` | `caps.rs` |
| `CGROUP_ROOT` | `Path("/sys/fs/cgroup")` | n/a | `types.py:20` |

### C.10 Isolated-workspace NETWORK constants — `isolated_workspace/network.py` (NOT types.py)

> The task grouped these under `types.py`; they actually live in `network.py`. See §D risk.

| Constant | Value | Source |
|---|---|---|
| `BRIDGE_NAME` | `"eos-shared0"` | `network.py:27` |
| `BRIDGE_CIDR` | `10.244.0.0/24` | `network.py:28` |
| `GATEWAY` | `10.244.0.1` | `network.py:29` |
| `NFT_NAT_TABLE` | `"eos_iws_nat"` | `network.py:30` |
| `NFT_FILTER_TABLE` | `"eos_iws_filter"` | `network.py:31` |
| `IMDS_ADDR` | `"169.254.169.254"` | `network.py:32` |
| `RFC1918_NETS` | `("10.0.0.0/8","172.16.0.0/12","192.168.0.0/16")` | `network.py:33` |
| `VETH_PREFIX` | `"eos-iws-"` | `network.py:34` — **same literal as `HANDLE_PREFIX` (types.py:19); Rust should share ONE const** |
| Pool range | `10.244.0.2 – 10.244.0.254` (/32 each) | `network.py:48,55-57` (skips .0/.1/.255) |
| Veth names | `eos-iws-{handle_id[:6]}h` (host) / `…n` (peer) | `network.py:10-11` |

### C.11 ns-holder handshake + shell-outs (`isolated_workspace/scripts/ns_holder.py`)

| Fact | Value | Source |
|---|---|---|
| Handshake step 1 | write `b"ns-up\n"` to readiness fd | `ns_holder.py:94` (after rbind /proc `:93`) |
| Handshake step 2 (read) | read until `\n`; require prefix `b"net-ready"` (else exit code 2) | `ns_holder.py:100-107` |
| Handshake step 3 (write) | bring `lo` up, purge IPv6, write `b"ready\n"` | `ns_holder.py:109-111` |
| Terminal state | `signal.pause()` until SIGTERM (`sys.exit(0)`) | `ns_holder.py:113-114` |
| CLI | `ns_holder.py <readiness_fd> <control_fd>` | `ns_holder.py:14,89-91` |
| **`accept_ra=0` sysctl shell-out** | `sysctl -w net.ipv6.conf.{iface}.accept_ra=0` per iface | `ns_holder.py:39` — **lives HERE, not types.py** → replace with `/proc/sys/net/ipv6/conf/{iface}/accept_ra` write |
| **IPv6 route flush shell-out** | `ip -6 route flush default` | `ns_holder.py:45` → replace with rtnetlink |
| `/proc` rbind shell-out | `mount --rbind /proc /proc` (best-effort) | `ns_holder.py:81-86` |
| `lo` up shell-out | `ip link set lo up` | `ns_holder.py:109` → replace with rtnetlink |
| IPv6 conf root | `/proc/sys/net/ipv6/conf` | `ns_holder.py:25` |
| Fallback iface list | `("all","default","lo","eth0")` | `ns_holder.py:26` |
| Holder-crash test knob | `EOS_ISOLATED_WORKSPACE_TEST_HOLDER_CRASH=true` → exit 7 | `ns_holder.py:97-98` |

### C.12 Docker launch + thin-client recovery constants (plan §0/§2/§12 — cross-reference, FIRST-PARTY verified)

| Constant | Value | Source (re-opened + confirmed this pass) |
|---|---|---|
| Thin-client `CONNECT_FAILED` exit | `97` | `host/daemon_client.py:37` (`_THIN_CLIENT_CONNECT_FAILED = 97`) |
| Thin-client `IO_FAILED` exit | `98` | `host/daemon_client.py:38` (`_THIN_CLIENT_IO_FAILED = 98`) |
| `DAEMON_PROTOCOL_VERSION` | `1` | `host/daemon_client.py:46` |
| `DAEMON_PROTOCOL_FIELD` | `"_eos_daemon_protocol_version"` | `host/daemon_client.py:47` |
| Default Docker run flags | `--cap-add=SYS_ADMIN --cap-add=NET_ADMIN --security-opt seccomp=unconfined --security-opt apparmor=unconfined` | `provider/docker/client.py:25-29` (`DEFAULT_RUN_FLAGS`) |
| EphemeralOS runtime tmpfs target | `/eos` (`--tmpfs …:rw,size=2g,mode=1777`) | `provider/docker/client.py:40` (`OVERLAY_WRITABLE_TMPFS_TARGET`) |
| `overlay_writable_root()` | `Path("/eos/mount")` | `overlay/writable_dirs.py:13` (`OVERLAY_WRITABLE_ROOT`) |

> §C.12 values were independently re-opened and confirmed in this pass (not merely plan-asserted).

---

## D) Risks — edges/anchors where the source contradicts or refines the plan

1. **Net constants are NOT in `types.py`.** The task lists `10.244.0.x/24`, `eos-shared0`, `accept_ra=0` as `types.py` "net constants." Source: `eos-shared0`/`10.244.0.0/24`/gateway are `isolated_workspace/network.py:27-29`; `accept_ra=0` is `isolated_workspace/scripts/ns_holder.py:39`. `types.py` holds only `FALLBACK_DNS="1.1.1.1"` (`:180`) and `HANDLE_PREFIX="eos-iws-"` (`:19`) of the named set. Rust must split these across the eos-isolated (network) and eos-ns-holder (sysctl) crates accordingly.

2. **`HANDLE_PREFIX` and `VETH_PREFIX` are duplicate literals** (`"eos-iws-"`) in two files (`types.py:19`, `network.py:34`). Rust should define ONE shared const (likely in eos-protocol or eos-isolated) to avoid drift.

3. **`audit_schema.py` is NOT a pure whole-module move.** The task says "confirm pure dataclass/typing (movable into eos-protocol)." TRUE for the 9 dataclasses + `build_*` + `Lane`, but `safe_emit` (`:294`, imports `daemon.audit_buffer`) and `safe_record_phase` (`:310`, imports `engine.tool_call.phase_buffer`) are impure and STAY in eos-daemon. Severing #1 is a *partial* extraction.

4. **The "RLock in occ/service.py" framing is imprecise.** `occ/service.py` holds NO RLock; it offloads sync work via `run_sync_in_executor` (`:173,:240`). The reentrant `threading.RLock` deadlock trap lives in `layer_stack/storage_lock.py:22,78`, taken via `.exclusive()` inside the executor-offloaded transaction (`layer_stack/transaction.py:45`, `layer_stack/stack.py:365`). The trap is real and load-bearing — just located in eos-layerstack, not eos-occ.

5. **`namespace_runner.py:237-244` (plan) does not contain `start_new_session`.** The `start_new_session=True` process-group call is at `:250` (inside `_run_namespace_entrypoint_async`, `:227-272`); the `_unshare_path()` arg is at `:238`. The plan's range is one statement early.

6. **setns sources are under `scripts/`, not the plan's stated paths.** Plan §0 cites `setns_exec.py`, `setns_overlay_mount.py`, `_setns_libc.py`. Actual: `isolated_workspace/scripts/setns_exec.py`, `…/setns_overlay_mount.py`, `…/_setns_libc.py`. These are eos-runner sources (setns mode), and `setns_*` import `_setns_libc` (intra-crate) + `setns_overlay_mount.py:65` imports `overlay.kernel_mount` → confirms **eos-runner → eos-overlay**.

7. **eos-runner is NOT internal-dep-free.** It depends on eos-overlay (`namespace_entrypoint.py:18` and `setns_overlay_mount.py:65` both import `overlay.kernel_mount`) and eos-protocol (ToolCallRequest/Result/Intent from `shared.models`). The plan's §1 line 87 phrasing ("eos-runner and eos-ns-holder are single-threaded, syscall-only … no tokio") is about the *threading model*, not dep-freedom — only eos-ns-holder is a true near-leaf (no internal deps; tokens inline).

8. **eos-plugin ⊥ eos-occ is a real, derivable guarantee** (task left it open).
plugin's sole occ touch is `projection.py:10` → the HINGE adapter
(snapshot/lease/projection, never publish). Current Rust narrows `eos-plugin`
to pure contracts and PPC framing; it does not link `eos-layerstack`,
`eos-occ` or `eos-overlay`. WRITE_ALLOWED publish is owned by
`eos-plugin-ops` via the concrete OCC/LayerStack path. This is NOT contradicted
by source, but is stronger than the plan spells out — flagged so the Rust author
treats `eos-plugin ⊥ eos-occ` as an enforced build-time edge, parallel to
eos-isolated.

9. **Python `ephemeral_workspace` → layerstack is a DIRECT source edge** (not merely transitive via overlay/occ). `ephemeral_workspace/pipeline.py` and `pipeline_registry.py` import `sandbox.layer_stack.*` directly. The task's old "eos-ephemeral ← overlay+occ+runner" phrasing omitted this. Current Rust intentionally does **not** carry that full runtime graph as its own crate: the old `eos-ephemeral` crate is removed, and the daemon/other crates own the concrete overlay/OCC/runner/plugin paths.
