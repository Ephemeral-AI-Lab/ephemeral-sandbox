# Phase 1 — Foundation

**Type:** Mechanical refactor + class-rename shims + `manager.py` mechanical decomposition. No daemon behavior change; class internals rewritten in Phase 2.
**Scope:** Materialize the three workspace packages (with `main_workspace/` as a thin re-export facade); extract overlay subsystem (FLAT, namespace-only); define `OverlayHandle` + lifecycle primitives; extract `tool_primitives` package; relocate non-overlay `execution/*` files; mechanically extract `manager.py` (1624 lines) into 7 focused modules; capture parity corpus scoped to ephemeral-mode only.
**Cost:** ~300 atomic import updates across `backend/`; PR review checklist must explicitly verify 753-line file moves used `git mv` to preserve blame.
**Depends on:** nothing (grep/glob verb renames already shipped in main).
**Blocks:** Phase 2.
**Atomic commit plan:** ≤10 logical commits. Suggested split: (1) create the 3 workspace packages + thin `main_workspace/` facade; (2) Phase 1 §2 ephemeral relocation (`sandbox_overlay.py` → `ephemeral_workspace/pipeline.py`, etc.); (3) Phase 1 §3 + §3.A `manager.py` extraction (one commit per extracted module pair; ≥3 commits — `_runtime.py`/`_lifecycle.py`+`_gc.py`/`_ttl.py`+`_quota.py`+`_types.py`); (4) overlay subsystem extraction; (5) `tool_primitives` package; (6) non-overlay `execution/*` relocations + final `sandbox/execution/` deletion; (7) parity corpus + grep audit. Each commit runs `make test` on parent SHA; rollback via `git revert <sha>`.

See [`unify_sandbox_workspace.md`](unify_sandbox_workspace.md) for the overview and ADR.

---

## Goals

After Phase 1 lands, the sandbox tree has three sibling workspace packages, a dedicated `sandbox/overlay/` filesystem substrate (flat, no `strategies/` subfolder), and a `sandbox/_shared/tool_primitives/` package containing verb compute implementations. `sandbox/execution/` is deleted entirely. The daemon/service/ directory contains only mode-agnostic helpers (`layer_stack_client.py`). No tool-call behavior has changed — every existing test passes byte-equivalently against the parity corpus.

---

## Step 1 — Create the workspace packages

**1.1.** Create `sandbox/main_workspace/__init__.py` (NEW, **thin re-export facade** — Planner F.14 / Critic must-fix #12 / not just a doc anchor):

```python
"""main_workspace = base repo + LayerStack snapshots. Persistent identity.
Written ONLY through OCC.

This package re-exports the public surface from sandbox.layer_stack and
sandbox.occ for trichotomy symmetry. Implementing modules continue to live
under sandbox/layer_stack/ and sandbox/occ/ (preserved import paths to avoid
500+ external import updates).

NOT re-exported: OperationOverlayHandle (lives in ephemeral_workspace —
that handle is ephemeral-coupled, not main_workspace-coupled per Architect F.14).
"""

from sandbox.layer_stack import LayerStack, prepare_workspace_snapshot
from sandbox.occ import CommitQueue, Change, WriteChange, DeleteChange

__all__ = [
    "LayerStack",
    "prepare_workspace_snapshot",
    "CommitQueue",
    "Change",
    "WriteChange",
    "DeleteChange",
]
```

Rationale: 5 lines costs nothing and makes the trichotomy real for any new code that opens `sandbox/main_workspace/__init__.py`. Existing imports unchanged.

**1.2.** Create `sandbox/main_workspace/README.md` (short — cross-links to `layer_stack/` and `occ/`).

**1.3.** Create `sandbox/ephemeral_workspace/__init__.py` — package docstring describing per-tool-call execution context. Populated by Step 2.

**1.4.** Update top-level `sandbox/__init__.py` docstring to name the three workspace packages and cross-link to `docs/sandbox/api_surface.md` (created in Phase 3).

→ **Verify:** `from sandbox import main_workspace, ephemeral_workspace, isolated_workspace` succeeds.

---

## Step 2 — Relocate ephemeral-mode machinery + pipeline rename

**Planner F.3 / Critic must-fix #11 — Scope acknowledgment.** `sandbox/daemon/service/sandbox_overlay.py` is 27KB / 753 lines. Phase 1 §2 is `git mv` + class-rename + minimal shim creation. The class internals (`acquire_operation_overlay`, `release_operation_overlay`, `publish_cycle`, `publish_pending_changes`, `flush_to_workspace`, `_apply_workspace_capture`, `_mount_active`, `_remount_active`, `_detach_active_mount`, etc.) are STRUCTURALLY COLLAPSED into 30 lines + `_commit_and_attach` in Phase 2 §3.1 — that rewrite is NOT part of Phase 1. Phase 1's "no behavior change" framing holds; the work IS mechanical (rename + shim), but the file is 753 lines and import-fix blast radius is ~300 sites. PR review checklist must verify `git mv` was used to preserve blame history.

**2.1.** Move `sandbox/daemon/service/sandbox_overlay.py` → `sandbox/ephemeral_workspace/pipeline.py` (via `git mv` — blame preservation REQUIRED). Rename class `SandboxOverlay` → `EphemeralPipeline`. Preserve all public methods unchanged (Phase 2 will collapse them into the new `run_tool_call` entry point — that's the substantive rewrite).

**2.2.** Move `sandbox/daemon/service/shell_runner.py` content into `sandbox/ephemeral_workspace/pipeline.py` (absorbed — shell becomes one of six verbs in Phase 2).

**2.3.** Historical Phase 1 draft moved the old shell-specific background files mechanically, but Phase 2.5 superseded that path. The shipped design deletes the old shell job files and uses engine-owned background tasks plus generic request lifecycle RPCs (`api.v1.cancel`, `api.v1.heartbeat`, `api.v1.inflight_count`).

**2.4.** Move `sandbox/daemon/service/overlay_manager.py` content into `sandbox/ephemeral_workspace/pipeline.py` (overlay lifecycle is pipeline-owned).

**2.5.** Move `sandbox/daemon/service/overlay_events.py` → `sandbox/ephemeral_workspace/events.py`.

**2.6.** Move `sandbox/plugin/` → `sandbox/ephemeral_workspace/plugin/` (entire subtree including `runtime/`). Update package docstring to reference Principle 10 (iws blocks plugin access; dispatcher gate added in Phase 2).

**2.7.** Update dispatcher peer-bootstrap in `sandbox/daemon/rpc/dispatcher.py`: `from sandbox.ephemeral_workspace.plugin import handler as plugin_handler`.

**2.8.** If `sandbox/daemon/service/` contains only `layer_stack_client.py` after the moves, keep that file. Otherwise delete the directory.

→ **Verify:** `make test` green; class rename + plugin relocation complete.

---

## Step 3 — Mechanically decompose `manager.py` (1624 lines) + rename iws pipeline

**Critic must-fix #3 / Planner F.2 / Architect F.2 — verified ground truth:** `wc -l sandbox/isolated_workspace/manager.py` = 1624 (NOT 1016 as Planner's review stated; NOT 970 as the original plan implied). The file contains `IsolatedWorkspaceManager`, `_LinuxRuntime` (~600 lines), `_PhaseTimer`, `_ManagerConfig`, `IsolatedWorkspaceError`, `IsolatedWorkspaceHandle`, and ~15 helper methods (`_wire_handle`, `_teardown`, `_rollback_partial`, `_check_host_capacity`, `_ttl_loop`, `startup_gc`, `_reap_orphans`, `_release_orphan_lease`, `_reap_orphan_cgroup`, `_unfreeze_and_kill`, `_compute_host_budget`, `_read_manager_json`). Phase 1 lands the mechanical extraction (no behavior change); Phase 2 §4.1 rewrites the now-isolated `pipeline.py` surface.

**3.1.** Mechanically extract `sandbox/isolated_workspace/manager.py` (1624 lines) into 7 focused modules. **No behavior change** — every extracted function preserves its current body and call sites are updated to the new import path. Target: no file >400 lines.

| New file | Contents extracted | Approx lines |
|---|---|---|
| `sandbox/isolated_workspace/pipeline.py` | `IsolatedWorkspaceManager` class (renamed to `IsolatedPipeline` per §3.2). Public surface only: `enter`, `exit_`, `run_in_handle`, `status`, `list_open`, `get_handle`, `require_pipeline`, `set_pipeline`, `get_active_pipeline`. Phase 2 §4.1 rewrites this body. | ~250 |
| `sandbox/isolated_workspace/_types.py` | `IsolatedWorkspaceError`, `IsolatedWorkspaceHandle` dataclass, `_ManagerConfig`, `_PhaseTimer`. Used by every other extracted file. | ~150 |
| `sandbox/isolated_workspace/_lifecycle.py` | `_wire_handle`, `_teardown`, `_rollback_partial`. The 671/679/775-786 ordering invariants live here (Phase 3 §6.2 test pins them). | ~200 |
| `sandbox/isolated_workspace/_gc.py` | `startup_gc`, `_reap_orphans`, `_release_orphan_lease`, `_reap_orphan_cgroup`, `_unfreeze_and_kill`. | ~250 |
| `sandbox/isolated_workspace/_ttl.py` | `_ttl_loop`, `ttl_sweep`. | ~150 |
| `sandbox/isolated_workspace/_quota.py` | `_check_host_capacity`, `_compute_host_budget`, `_read_manager_json`. | ~150 |
| `sandbox/isolated_workspace/_runtime.py` | `_LinuxRuntime` class (the largest chunk — `spawn_ns_holder`, `mount_overlay`, `configure_dns`, `signal_net_ready`, `create_cgroup`, `freeze`, `kill_holder`, `run_in_handle` + 3 module-level helpers). | ~600 |

`pipeline.py` imports from each extracted module. `__init__.py` re-exports the public surface so callers don't need to know the internal layout.

**3.2.** As part of the `pipeline.py` extraction: rename class `IsolatedWorkspaceManager` → `IsolatedPipeline`. Rename `require_manager` → `require_pipeline`, `set_manager` → `set_pipeline`. Add nil-safe accessor `get_active_pipeline()` (returns `None` if not bootstrapped — used by daemon handlers in Phase 2 to avoid forcing pipeline bootstrap on every tool call).

**3.3.** Update `sandbox/isolated_workspace/__init__.py` docstring + re-exports.

→ **Verify:** `make test` green; iws tier 1–9 tests pass; `find sandbox/isolated_workspace -name "*.py" -exec wc -l {} \;` shows no file >400 lines.

---

## Step 4 — Extract overlay subsystem (FLAT, namespace-only)

`sandbox/execution/overlay/` and the namespace-strategy portions of `sandbox/execution/strategies/` move to a new top-level `sandbox/overlay/` package. The new package is OCC-unaware. **`copy_backed` strategy and related infrastructure are DELETED entirely.** No `strategies/` subfolder — flat layout.

**4.1.** Move `sandbox/execution/overlay/kernel_mount.py` → `sandbox/overlay/kernel_mount.py`. Keep all function signatures (`mount_overlay(workspace_root, layer_paths, upperdir, workdir, ...)` — `workspace_root` parameter name preserved).

**4.2.** Move `sandbox/execution/overlay/mount_syscalls.py` → `sandbox/overlay/mount_syscalls.py`.

**4.3.** Move `sandbox/execution/overlay/capture.py` → `sandbox/overlay/capture.py`. `walk_upperdir` lives here.

**4.4.** Delete the old overlay layout abstraction. Mount-input validation now lives at the kernel boundary in `sandbox/overlay/kernel_mount.py`; `MaterializeLayout`, `LayerPathsLayout`, the `OverlayLayout` alias, and the `AnyOverlayLayout` union are gone.

**4.5.** Move `sandbox/execution/overlay/capability.py` → `sandbox/overlay/capability.py`. Update `mount_syscalls_supported()` from a runtime gate to a **hard startup precondition**: sandbox refuses to boot if mount syscalls unavailable. Also delete the `EOS_OVERLAY_FORCE_MATERIALIZE=1` kill switch (no longer meaningful).

**4.5.1.** Create `scripts/verify_overlay_preconditions.py` (Planner F.10 / Critic must-fix #8 / Scenario D.3). The script:
- Probes for `fsopen`/`fsconfig`/`fsmount` (mount syscalls).
- Probes for private user namespace support.
- Exits non-zero with a diagnostic if either is missing.
Phase 3 §6C wires this into CI as a deployment guard.

**4.5.2.** Pre-rollout audit — enumerate ALL known deployment targets (CI runners, staging, production Docker images) and verify kernel versions support the mount syscalls BEFORE Phase 1 lands. Document audit results in `docs/sandbox/deployment_targets.md`. No surprises post-merge.

**4.5.3.** No mount-precondition runtime bypass remains. If a deployment target lacks the mount syscalls or private namespaces, startup fails closed and rollback is a normal `git revert`.

**4.6.** Keep the raw `OverlayPathChange` logic and delete copy-backed change synthesis. **Edit the existing `sandbox/occ/overlay_change_conversion.py`** (Planner F.7 — the file already exists, do NOT "move into"; this is an in-place edit) to add a `source: str = "overlay_capture"` keyword-only parameter so callers can override it (used by Phase 2 §6.1 EphemeralPipeline for typed-write coalescing). Default preserves today's behavior.

**Important — Phase 2 §6.1 enumerates the 4-helper chain.** Threading `source` through `overlay_path_changes_to_occ_changes` requires synchronous edits at 4 sites (see Phase 2 §6.1–§6.4): the function itself, plus `build_overlay_write_change` and `build_overlay_delete_change` in `occ/changeset.py`, plus inline `SymlinkChange`/`OpaqueDirChange` constructors. Phase 1 only stages the parameter on the top-level function with the default; the helper-site edits land in Phase 2 as one atomic commit.

**4.7.** Move `sandbox/execution/strategies/namespace_runner.py` → `sandbox/overlay/namespace_runner.py` (host-side fork+unshare+wait coordinator).

**4.8.** Move `sandbox/execution/strategies/namespace_entrypoint.py` → `sandbox/overlay/namespace_entrypoint.py` (child entry point — Phase 2 extends this with the two-tier verb dispatch).

**4.9.** **DELETE entirely:**
- `sandbox/execution/strategies/base.py` (`ExecutionStrategy` ABC)
- `sandbox/execution/strategies/copy_backed.py`
- `sandbox/execution/strategies/_workspace_rewrite.py`
- `sandbox/execution/contract.py::MountMode` enum (single value isn't an enum)
- `sandbox/execution/runner.py::_strategies_for_mount_mode`, `_build_strategy`, `should_fall_back` — replace `run_workspace_replaced_command` with a direct call into `sandbox/overlay/namespace_runner.py`
- `materialize: bool` parameter on `prepare_workspace_snapshot()` (always False now)

**4.10.** Create `sandbox/overlay/handle.py` (NEW):
```python
@dataclass
class OverlayHandle:
    """State-bearing handle for a mounted overlay. Not an immutable value object.
    (Planner F.9 / Critic must-fix #10 / Architect §D Principle 3).

    Mutability is intentional: `_destroyed: bool` is flipped by
    `sandbox.overlay.lifecycle.destroy(handle)` to guarantee idempotency.
    Concurrent destroy from multiple asyncio tasks is safe because the
    owning pipeline holds a per-handle asyncio.Lock keyed by lease_id
    (see EphemeralPipeline._destroy_with_lease_guard, Phase 2 §3.1).

    Field `_destroyed` is a single-bit write under the per-handle lock — no
    torn-read concerns. The dataclass is NOT @dataclass(frozen=True);
    `_destroyed` mutation under lock is the documented contract.

    `namespace_pid` lifecycle (Critic D.4 / Architect F.2):
    - For iws handles (long-lived ns_holder spawned at `enter`):
      populated by `_lifecycle._wire_handle` after `_runtime.spawn_ns_holder`.
      The pid stays valid for the session; teardown invokes
      `_runtime.kill_holder(namespace_pid)`.
    - For ephemeral handles (per-call fork-and-exit):
      `namespace_pid` is None. EphemeralPipeline's `overlay.run_in_namespace`
      forks per call; the child exits before `run_tool_call` returns.
      The field is preserved on the dataclass for shape uniformity
      between modes (we do NOT split into Ephemeral/Isolated subclasses
      because the protocol surface is identical and the field is cheap).
    """
    workspace_root: str
    layer_paths: tuple[str, ...]
    upperdir: Path
    workdir: Path
    snapshot_version: int
    lease_id: str
    namespace_pid: int | None  # iws: populated; ephemeral: always None
    _destroyed: bool = False  # field-level idempotency guard; written under per-handle lock
```

**4.11.** Create `sandbox/overlay/lifecycle.py` (NEW) — the user-facing overlay API both pipelines consume:
```python
async def create(
    layer_stack: WorkspaceLeaseClient,
    *,
    agent_id: str,
    workspace_root: str = "/testbed",
    network: NetworkConfig | None = None,
) -> OverlayHandle: ...

async def capture_changes(handle: OverlayHandle) -> Sequence[OverlayPathChange]: ...

async def destroy(handle: OverlayHandle) -> None:
    """Idempotent. Safe to call concurrently from multiple threads."""
    if handle._destroyed:
        return
    handle._destroyed = True
    # umount + release lease + cleanup run_dir
    ...
```

**4.12.** Create `sandbox/overlay/__init__.py` package docstring describing the OCC-unaware filesystem substrate, namespace-only execution model.

**4.13.** Update all importers across `backend/`:
- `from sandbox.execution.overlay.kernel_mount import ...` → `from sandbox.overlay.kernel_mount import ...`
- `from sandbox.execution.overlay.capture import walk_upperdir` → `from sandbox.overlay.capture import walk_upperdir`
- `from sandbox.execution.strategies.namespace import detect_private_mount_namespace` → `from sandbox.overlay.namespace_runner import detect_private_mount_namespace`
- (etc. across `sandbox/plugin/`, `backend/src/plugins/catalog/lsp/runtime/`, tests)

→ **Verify:** `make test` green; `grep -rn "sandbox\.execution\." backend/` returns zero hits; `grep -rn "copy_backed\|MaterializeLayout\|MountMode" backend/` returns zero hits.

---

## Step 5 — Relocate non-overlay `execution/*` files

The remaining `sandbox/execution/` files are shell-pipeline/runtime machinery that doesn't belong in the overlay substrate. They relocate to `sandbox/ephemeral_workspace/` or `sandbox/_shared/`.

**5.1.** Move `sandbox/execution/contract.py` → split:
- `CommandExecRequest`, `ShellProcessResult`, `OverlayShellRequest`, `OverlayCapture` → `sandbox/_shared/shell_contract.py` (used only by shell)
- `WorkspaceCapture`, `WorkspaceCapturePublisher`, `WorkspaceCapturePublishResult`, `CommandExecResult` → `sandbox/ephemeral_workspace/pipeline.py` (absorbed; these are pipeline result types)
- `WorkspaceLeaseClient`, `WorkspaceSnapshotLease`, `OCCMutationClient`, `SnapshotManifest`, `ChangesetResultLike`, `EmptyChangesetResult` → `sandbox/_shared/ports.py` (NEW — mode-agnostic protocol types)

**5.2.** Move `sandbox/execution/service.py::execute_command` → absorbed into `sandbox/ephemeral_workspace/pipeline.py::EphemeralPipeline.run_tool_call` in Phase 2. For Phase 1, keep as a free function under `sandbox/ephemeral_workspace/_execute_command.py` (temporary; deleted in Phase 2).

**5.3.** Move `sandbox/execution/runner.py::run_workspace_replaced_command` → simplified inline call to `sandbox/overlay/namespace_runner.py::run_in_namespace`. The file deletes (single-strategy makes the runner trivial).

**5.4.** Move `sandbox/execution/env_policy.py` → `sandbox/_shared/env_policy.py` (mode-agnostic; covers both shell and tool primitives).

**5.5.** Move `sandbox/execution/resource_audit.py` → `sandbox/_shared/resource_audit.py` (mode-agnostic; covers timing/audit).

**5.6.** Move `sandbox/execution/subprocess_runner.py` → `sandbox/overlay/subprocess_runner.py` (used by `namespace_runner.py` to spawn child).

**5.7.** Move `sandbox/execution/writable_dirs.py` → `sandbox/overlay/writable_dirs.py` (`overlay_writable_root` used by overlay lifecycle).

**5.8.** Move `sandbox/execution/path_change.py` → `sandbox/overlay/path_change.py` (`OverlayPathChange` dataclass is overlay-shaped).

**5.9.** Delete `sandbox/execution/` directory entirely.

→ **Verify:** `make test` green; `grep -rn "sandbox\.execution" backend/` returns zero hits; `ls sandbox/execution/` returns "No such file or directory".

---

## Step 6 — Create `sandbox/_shared/tool_primitives/` package

Pure-compute verb implementations that run inside the namespace child. Two-tier shape: uniform `compute(args) → ToolCallResult` for read/write/edit/grep/glob; shell-specific `run(args, cancel_event, stdout_ref, stderr_ref) → ShellResult` for shell.

**6.1.** Create `sandbox/_shared/tool_primitives/__init__.py` with package docstring.

**6.2.** Create `sandbox/_shared/tool_primitives/read.py` — extract from `daemon/handler/read.py::_read_in_workspace` body. Use `os.open(path, O_RDONLY | O_NOFOLLOW)` unconditionally. Returns `ReadResult` (preserves today's `success`/`exists`/`content`/`encoding`/`timings` shape).

**6.3.** Create `sandbox/_shared/tool_primitives/write.py` — extract from `daemon/handler/write.py::_write_in_workspace` body. Use `O_NOFOLLOW` on all opens. Atomic-overwrite-via-temp-file semantics preserved. Returns `WriteResult`.

**6.4.** Create `sandbox/_shared/tool_primitives/edit.py` — extract from `daemon/handler/edit.py::_edit_in_workspace` body. `O_NOFOLLOW` mandatory. `_apply_edits` helper for search/replace logic. Anchor-miss raises loud `ValueError`. Returns `EditResult`.

**6.5.** Create `sandbox/_shared/tool_primitives/grep.py` — extract from `daemon/handler/grep.py::_grep_sync` body. Walks paths via `os.walk` inside the namespace (overlay-mounted FS). Returns `GrepResult`.

**6.6.** Create `sandbox/_shared/tool_primitives/glob.py` — extract from `daemon/handler/glob.py::_glob_sync` body. Returns `GlobResult`.

**6.7.** Create `sandbox/_shared/tool_primitives/shell.py` — extract from `ephemeral_workspace/shell_job.py::run_command_to_refs` plus the in-namespace exec wrapper. Different shape: `run(args, *, cancel_event, stdout_ref, stderr_ref, pid_recorder) → ShellResult`. Carries cancellation hooks + stdout/stderr ref paths.

**6.8.** Create `sandbox/_shared/tool_primitives/file_ops.py` — shared no-follow helpers.

**Critic must-fix #15 / Architect F.6 / Principle 8 — preservation of per-component walk semantics.** Today's `sandbox/daemon/request_context._open_no_follow` (lines 155-179) does a per-component walk: open root with `O_DIRECTORY` → open each intermediate segment with `O_DIRECTORY|O_NOFOLLOW|dir_fd=parent` → final open with `flags|O_NOFOLLOW`. This defends against intermediate-component symlinks (e.g., `/testbed/dir → /etc`, then `read("/testbed/dir/passwd")` is refused because the per-component walk hits the symlink at the intermediate step).

A naive single-call `os.open(path, flags|O_NOFOLLOW)` would silently weaken this guarantee — `O_NOFOLLOW` only refuses the LAST component.

**file_ops.open_no_follow MUST preserve the per-component walk OR use `openat2(RESOLVE_NO_SYMLINKS)` if the kernel supports it.** Pseudocode:

```python
def open_no_follow(path: str, flags: int) -> int:
    """Open `path` refusing to traverse ANY symlink (intermediate or trailing).

    Preserves the per-component walk semantics from
    daemon/request_context._open_no_follow. A naive `os.open(path, flags|O_NOFOLLOW)`
    only refuses the trailing symlink and leaks intermediate-symlink attacks.
    """
    if HAVE_OPENAT2:  # kernel >= 5.6
        return _openat2_resolve_no_symlinks(path, flags)
    # Fallback: per-component walk
    parts = _split_absolute_path(path)
    dir_fd = os.open("/", os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        for segment in parts[:-1]:
            new_fd = os.open(segment, os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=dir_fd)
            os.close(dir_fd); dir_fd = new_fd
        return os.open(parts[-1], flags | os.O_NOFOLLOW, dir_fd=dir_fd)
    finally:
        os.close(dir_fd)


def walk_dirs_no_follow(root: str): ...
```

Also add: **before editing**, audit current `_xxx_in_workspace` extracted bodies (Phase 1 §6.2–§6.6) for `os.open` call sites. Confirm each uses the per-component walk via `read_bytes_no_follow`/`write_text_no_follow` (which today call `_open_no_follow`). If a body uses bare `os.open(path, ...)` without going through the walker, parity is silently broken — flag in the parity-corpus pre-merge review. Used by read/write/edit/grep/glob via `open_no_follow`.

**6.9.** Create `sandbox/_shared/tool_primitives/capture.py` — facade re-exporting `walk_upperdir` from `sandbox/overlay/capture.py`. Single import path for in-namespace callers.

→ **Verify:** `make test` green; tool_primitives imports succeed; O_NOFOLLOW present in every `os.open` call in read/write/edit (grep via lint check).

---

## Step 7 — Capture parity corpus (EPHEMERAL MODE ONLY)

**Critic must-fix #2 / Architect F.1 (DISCRIMINATING FINDING) — corpus is scoped to ephemeral mode only.** `sandbox/isolated_workspace/ops_handlers.py` is 98 lines of shell-out wrappers (`/bin/cat`, `/usr/bin/grep`, `in_ns_write.py`) returning `subprocess.run` shape. The iws verbs do NOT honor most options (grep's `mode`/`case_insensitive`/`include_pattern`/`multiline`; edit_file's `search/replace` semantics; read_file's 16MB cap; write_file's OCC conflict tracking). There is NO byte-equivalent "before" output for iws — it returns a different shape with different semantics. iws verb migration is a **functional upgrade** validated by Phase 3's `behavior_upgrade/` tier, NOT by parity replay.

The corpus is therefore the regression safety net for **ephemeral-mode verbs only** (today's `daemon/handler/{read,write,edit,grep,glob,shell}.py` bodies). For iws verbs, Phase 3's `behavior_upgrade/` tests assert the new typed-shape behavior against fixtures (not against today's iws output).

**7.1.** Add `tests/mock/sandbox/_fixtures/tool_primitives_parity_corpus.json` with ≥40 cases covering **ephemeral mode + daemon-handler bodies, pre-unification**:
- `read_file` in-workspace + out-of-workspace (today's two branches; Phase 2 unifies via overlay).
- `write_file` create-only + overwrite + non-utf8.
- `edit_file` anchor-match + anchor-miss + count-mismatch.
- `grep` content + files_with_matches + count modes.
- `glob` pattern matches.
- `shell` simple cmd + env + cwd + timeout (foreground). Background-shell parity is NOT in the corpus. Phase 2.5 owns the canonical background behavior: the engine wraps the same `pipeline.run_tool_call(req)` coroutine, cancellation uses generic request-level RPCs, and the daemon request registry handles TTL orphan cleanup.
- Non-workspace paths: `/etc/hosts`, `/tmp/scratch_test` (Planner F.23 — read/write today goes through the `_out_of_workspace` branch; Phase 2's overlay pass-through must replicate the same behavior modulo the new host-path denylist in Phase 2 §7.5).
- Edge cases: empty path, `..` escape, symlink-to-host, large file (>16 MiB).

**7.2.** Add `tests/mock/sandbox/_fixtures/test_tool_primitives_parity_corpus.py` — replays each case against today's daemon handlers (`daemon/handler/{read,write,edit,grep,glob,shell}.py`) and asserts byte-equivalent results. NOT replayed against today's `isolated_workspace/ops_handlers.py` — those would diverge by design.

**7.3.** Phase 2's acceptance criterion: running this corpus against the new EphemeralPipeline asserts byte-equivalence (modulo the documented OCC commit semantics change for typed writes — single-path coalescing preserved via `source="api_write"`; also modulo Phase 2 §7.5's host-path denylist which rejects writes to `/etc/*`, `/var/*`, `/proc/*`, `/sys/*`, `/boot/*`). iws-mode behavior is validated by Phase 3's `behavior_upgrade/` tier.

→ **Verify:** corpus replay passes on the Phase 1 codebase (no behavior change).

---

## Step 8 — Update top-level imports + cleanup

**8.1.** Update `sandbox/__init__.py` to re-export the three workspace packages.

**8.2.** Run `make lint` + `make test` end-to-end. Fix import paths surfaced by lint.

**8.3.** Run the targeted grep audit (Planner F.13 — includes `EOS_OVERLAY_FORCE_MATERIALIZE`):
```bash
grep -rn "sandbox\.execution\b\|SandboxOverlay\b\|IsolatedWorkspaceManager\b\|copy_backed\|MaterializeLayout\|MountMode\b\|EOS_OVERLAY_FORCE_MATERIALIZE\|sandbox\.daemon\.service\.\(sandbox_overlay\|shell_runner\|shell_job\|overlay_manager\|overlay_events\)\|from sandbox.plugin\b\|import sandbox.plugin\b\|sandbox\.execution\.overlay\|sandbox\.execution\.strategies" backend/
```
Must return zero hits.

Also audit the docs:
```bash
python - <<'PY'
from pathlib import Path
tokens = [
    "_iws" + "_rpc",
    "sandbox.api." + "lifecycle",
    "sandbox.api." + "workspace.",
    "value " + "type",
]
hits = []
for path in Path("docs/plans").glob("unify_sandbox_*.md"):
    text = path.read_text(encoding="utf-8")
    hits.extend(f"{path}:{token}" for token in tokens if token in text)
if hits:
    raise SystemExit("\n".join(hits))
PY
```
Must return zero hits (those have been corrected per Critic must-fix #1, #6, #10).

→ **Verify:** all lint + all tests green; grep audits clean.

---

## Acceptance criteria

- ✅ `sandbox/main_workspace/` exists as a **thin re-export facade** (5-line `__init__.py` re-exporting `LayerStack`, `prepare_workspace_snapshot`, `CommitQueue`, `Change`, `WriteChange`, `DeleteChange`).
- ✅ `sandbox/ephemeral_workspace/` contains `pipeline.py` (class `EphemeralPipeline`), `shell_job.py`, `shell_contract.py`, `events.py`, `plugin/` subtree, `_execute_command.py` (temporary — deleted in Phase 2 §3.2).
- ✅ `sandbox/isolated_workspace/` decomposed into 7 modules: `pipeline.py` (class `IsolatedPipeline`), `_types.py`, `_lifecycle.py`, `_gc.py`, `_ttl.py`, `_quota.py`, `_runtime.py`. PLUS original `network.py`, `handlers.py`, `ops_handlers.py` (still present — deleted in Phase 2 §14.3), `scripts/`. **No file >400 lines** post-decomposition (`wc -l` check).
- ✅ `sandbox/overlay/` exists as a FLAT top-level package containing `handle.py`, `lifecycle.py`, `namespace_runner.py`, `namespace_entrypoint.py`, `kernel_mount.py`, `mount_syscalls.py`, `capability.py`, `capture.py`, `subprocess_runner.py`, `writable_dirs.py`, `path_change.py`. No `strategies/` subfolder.
- ✅ `sandbox/overlay/handle.py::OverlayHandle` exists with `_destroyed: bool = False` field; docstring documents it as a **mutable state-bearing handle** with idempotent destroy under per-handle lock; documents `namespace_pid` lifecycle (iws-populated, ephemeral-None).
- ✅ `sandbox/overlay/lifecycle.py` exposes `create`, `destroy` (idempotent), `capture_changes`.
- ✅ `sandbox/_shared/tool_primitives/` contains read/write/edit/grep/glob/shell/file_ops/capture.
- ✅ `tool_primitives.file_ops.open_no_follow` preserves per-component walk semantics (not naive `os.open(path, flags|O_NOFOLLOW)`); uses `openat2(RESOLVE_NO_SYMLINKS)` when available.
- ✅ `sandbox/execution/` directory does not exist.
- ✅ `sandbox/plugin/` directory does not exist (moved under `ephemeral_workspace/plugin/`).
- ✅ `copy_backed`, `_workspace_rewrite`, `ExecutionStrategy`, `MountMode`, `MaterializeLayout`, `should_fall_back`, `EOS_OVERLAY_FORCE_MATERIALIZE` all deleted from the codebase.
- ✅ `mount_syscalls_supported()` is a hard startup precondition — sandbox refuses to boot without it.
- ✅ `scripts/verify_overlay_preconditions.py` exists and exits non-zero on degraded kernels (Phase 3 §6C wires into CI).
- ✅ Pre-rollout audit `docs/sandbox/deployment_targets.md` exists with kernel-version verification for every deployment target.
- ✅ The old mount-precondition tombstone flag has been deleted; rollback is a code revert.
- ✅ Parity corpus committed at `tests/mock/sandbox/_fixtures/tool_primitives_parity_corpus.json` with ≥40 cases **scoped to ephemeral mode + daemon-handler bodies, pre-unification**. iws-mode verbs are NOT in the corpus (functional upgrade, validated in Phase 3 `behavior_upgrade/`).
- ✅ All existing tests pass byte-equivalently.
- ✅ Grep audit returns zero hits for the legacy module names AND for the corrected planning-doc references checked by the token script in Step 8.
- ✅ PR review checklist explicitly verifies `git mv` was used for the 753-line `sandbox_overlay.py` move and the 1624-line `manager.py` extractions (blame preservation).
