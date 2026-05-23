# PLAN: Unify `sandbox.api.tool` for normal + isolated_workspace modes

**Status:** Planner revision v11 (after user direction: full symmetry — rename pipelines on both sides, relocate `sandbox/plugin/` under `ephemeral_workspace/` since plugins are ephemeral-only by Principle 9)
**Date:** 2026-05-24
**Scope:** Public Python surface at `backend/src/sandbox/api/` (tool ops + lifecycle ops), agent-level tool wrappers at `backend/src/tools/{sandbox,isolated_workspace}/`, and daemon-side dispatch.
**Delivery shape:** Four sequential PRs. PR-0 ships verb renames. PR-A ships unified daemon-side tool handlers, deletes `ops_handlers.py`, and **adds host-side `sandbox.api.enter_isolated_workspace`/`exit_isolated_workspace` plus the agent-level `tools/isolated_workspace/` wrappers**. PR-B ships host-side `WorkspaceSession`. PR-C migrates the iws test suite to use the new tool surface.

---

## 1. Context

EphemeralOS structures workspace state into **three named concepts**. Once these are clearly named, the relationship between modes is straightforward — the same tool ops run against an *execution context*, and the only thing that changes between calls is which execution context is active for the agent.

| Concept | Role | Mutability | Lifecycle |
|---|---|---|---|
| **`main_workspace`** | The persistent identity (base repo + LayerStack snapshots) for an agent's sandbox. The single source of truth that other workspaces commit into. | **Written to ONLY through OCC.** No direct file writes. | Persists for the lifetime of the sandbox. |
| **`ephemeral_workspace`** | Per-tool-call execution context. Mounts a fresh overlay against `main_workspace`'s snapshot, runs the tool op, captures the upperdir changeset, commits via OCC, tears down. | OCC-gated merge into `main_workspace` immediately after the tool call. | One per tool call. Created/destroyed implicitly by the daemon. |
| **`isolated_workspace`** | Lifecycle-scoped execution context. Mounts an overlay against `main_workspace`'s snapshot inside a `{user,mnt,pid,net}` namespace stack at `enter_isolated_workspace`; tool ops execute within this persistent overlay; upperdir is **never** merged into `main_workspace` and is discarded at `exit_isolated_workspace`. | **No OCC merge.** Upperdir is a hermetic scratch surface. | Per `enter → exit`; explicit at the agent layer. |

### Shared primitives across `ephemeral_workspace` and `isolated_workspace`

Both execution contexts compose the SAME daemon-side primitives — the only divergences are namespace context and the writeback policy:

| Primitive | `ephemeral_workspace` | `isolated_workspace` |
|---|---|---|
| Overlay mount | `mount_overlay` (`sandbox/execution/overlay/kernel_mount.py:49`) — per-call. | **Same `mount_overlay`** invoked via `setns_overlay_mount.py` once at `enter`. |
| Command exec | `os.execvp` via `EphemeralPipeline.execute_command` (daemon's mnt-ns). | **Same `os.execvp`** via `setns_exec.py:85` after joining iws ns. |
| File-level mutation | `_apply_edits` (`edit.py:210-228`), `_atomic_overwrite_no_follow` (`write.py:211-236`). | **Same primitives**, invoked directly against the persistent overlay's mount point. |
| Upperdir change capture | `walk_upperdir` (`sandbox/execution/overlay/capture.py:19`). | **Same `walk_upperdir`** invoked HOST-SIDE against `handle.upperdir` (the iws upperdir is a host-visible filesystem path). |
| OCC `commit_prepared` | ✅ Used — merges changeset into `main_workspace`. | ❌ **Suppressed.** Captured changes are discarded at `exit`. |
| Network configuration | None (standard sandbox network). | `{user,mnt,pid,net}` namespaces + bridge + veth + nftables MASQUERADE for **outbound NAT only** (no inbound port-exposure path); IMDS drop; RFC1918 deny. |
| Plugin access | ✅ Allowed (`api.plugin.*`, `plugin.<name>.<op>`). | ❌ **Blocked** — `forbidden_in_isolated_workspace` error (see Principle 9). |

### Agent-callable tool surface

- **Six tool ops** — `shell, read_file, write_file, edit_file, grep, glob` — operate against the active execution context (`ephemeral_workspace` by default; `isolated_workspace` when an iws session is open for the agent). Same signature, same daemon-side handlers, same primitives.
- **Two lifecycle ops** — `enter_isolated_workspace`, `exit_isolated_workspace` — switch the agent's active execution context between `ephemeral_workspace` and `isolated_workspace`. They are workspace-target mutations, not tool ops (different result base, different audit class).

### Today's normal-mode tool surface

- Agent-level wrappers live at `backend/src/tools/sandbox/{shell,read_file,write_file,edit_file,grep,glob}/` — each is a `@tool`-decorated function with a Pydantic `XxxInput` model that bridges to `sandbox.api.<verb>(sandbox_id, request)`.
- Host-side API at `sandbox/api/tool/{shell,read,write,edit,grep,glob}.py` — typed coroutines returning `XxxResult` dataclasses; wrapped by `audited_operation`; dispatched via `SandboxTransport.call(sandbox_id, "api.v1.<verb>", payload, ...)`.

### Today's isolated-mode surface — the gap that motivates this plan

- **Lifecycle daemon-RPCs** (`sandbox/isolated_workspace/handlers.py`): `enter, exit, status, list_open, test_reset` — registered at `api.isolated_workspace.<op>`. Survive.
- **Tool-op daemon-RPCs** (`sandbox/isolated_workspace/ops_handlers.py`): `shell, read_file, write_file, edit_file, search_content` — registered at `api.isolated_workspace.<op>`. **Deleted by this plan.**
- **Host-side Python API**: NONE. Callers go straight to `call_daemon_api(sandbox_id, "api.isolated_workspace.<op>", args, ...)` (test fixture `_iws_rpc.py`).
- **Agent-level tool wrappers at `backend/src/tools/isolated_workspace/`**: **NONE — original design intended them; never implemented.** This plan adds:
  - `backend/src/tools/isolated_workspace/enter_isolated_workspace/` — `@tool` wrapper.
  - `backend/src/tools/isolated_workspace/exit_isolated_workspace/` — `@tool` wrapper.
- **Test suite expectation** (`tests/mock/sandbox/isolated_workspace/PLAN.md`, Tier 1): `test_enter_then_shell_then_exit.py` — the audit sequence `enter, tool_call, exit` was always intended to be driven by agent-level tool calls. Today it routes through `_iws_rpc.py`'s raw RPC because the tool wrappers were never built.

### Specific problems being closed

1. **No agent-callable iws lifecycle tools.** Original design called for `tools/isolated_workspace/enter_isolated_workspace` / `exit_isolated_workspace`; absent.
2. **No host-side Python API for iws lifecycle.** `sandbox.api` exports tool verbs but not `enter_isolated_workspace`/`exit_isolated_workspace`.
3. `ops_handlers.py:83-86`: `edit_file` aliases to `write_file` (no search/replace).
4. `ops_handlers.py:89-95`: `search_content` shells `/usr/bin/grep -r -n`; ignores `output_mode`, `multiline`, `case_insensitive`, `line_numbers`, `glob_filter`, `head_limit`, `offset`.
5. No iws `glob_files` op at all.
6. Untyped `dict[str, Any]` IO at the iws tool surface.
7. No `audit_sink` injection on iws host side (only daemon-side JSONL mirror).
8. Result divergence — iws ops never populate `changed_paths`, `conflict`, `applied_edits`.

---

## 2. Principles (RALPLAN-DR v7)

1. **Single typed surface for tool ops.** `sandbox.api.{shell, read_file, write_file, edit_file, grep, glob}` are six free coroutines, each taking a typed `XxxRequest` and returning `XxxResult`. Dict-typed IO never crosses the public API boundary. **No `workspace` parameter on the request** — mode is resolved daemon-side by agent-state lookup (see Principle 4).

2. **Lifecycle ops are categorically different from tool ops.** `sandbox.api.enter_isolated_workspace` and `sandbox.api.exit_isolated_workspace` are workspace-target-mutating operations: they change which workspace state the agent is bound to. They are NOT routed through `audited_operation` (the tool-op audit wrapper); they get their own `lifecycle_operation` audit wrapper publishing `workspace_lifecycle_*` events. Their result types extend a new `LifecycleResultBase` (not `SandboxResultBase`) — no `workspace` discriminator, no `changed_paths`, no `conflict` field. They live in a sibling host-side module `sandbox/api/lifecycle/` (parallel to `sandbox/api/tool/`).

3. **One tool-op RPC namespace.** `api.v1.<verb>` is the only tool-op namespace. The 5 iws tool-op RPC ops `api.isolated_workspace.{shell,read_file,write_file,edit_file,search_content}` are deleted. iws lifecycle RPC ops `api.isolated_workspace.{enter,exit,status,list_open,test_reset}` survive on their existing namespace (they have no normal-mode equivalent; collapsing them is out of scope).

4. **Daemon-side execution-context dispatch is implicit by agent_id state — legitimate because BOTH lifecycle is explicit AND every dispatch outcome (success OR transport failure) carries the `workspace` discriminator (with an honest `unknown` fallback).** When `sandbox/daemon/handler/<verb>.py` receives a tool-op call, it queries the iws manager via `IsolatedPipeline.get_handle(caller.agent_id)`. If the agent has an open handle, dispatch the `isolated_workspace` branch; else, dispatch the `ephemeral_workspace` branch.
   - **Success path**: result payload carries `workspace` field reflecting the branch taken (ground truth — what the daemon actually did); projector populates `XxxResult.workspace`.
   - **Failure path**: when a transport error fires between daemon dispatch and result projection, the host-side `audited_operation` wrapper publishes `sandbox_operation_failed` WITH a `workspace` field set to the host's best-effort hint:
     - `workspace="isolated"` when the call originated from a `WorkspaceSession.enter_isolated(...)`-bound session (session knows its own state).
     - `workspace="ephemeral"` when the call originated from `WorkspaceSession.attach(...)` (default session) OR a back-compat free-function call.
     - `workspace="unknown"` when the call originated from an agent-level tool wrapper that has no host-side session context (rare; documented). Downstream audit consumers MUST handle this third value.
   Typing: `SandboxResultBase.workspace` is `Literal["ephemeral", "isolated"]` (success-result discriminator — narrow). The `workspace` field on the **failure-event payload** (a dict published via `publish_operation_failed`, not a typed dataclass) is `Literal["ephemeral", "isolated", "unknown"]`. The `unknown` third value lives only on failure events, never on a successful `XxxResult`.
   The earlier v2/v3 critique that "implicit dispatch is hard to debug" no longer applies: lifecycle is an explicit audit event AND every tool-op outcome carries `workspace` for post-hoc inspection — including failure cases where the host can only provide a best-effort hint.

7. **Tool-level pre-hooks are mode-agnostic; iws does NOT bypass them.** The destructive-shell guards at `tools/sandbox/shell/shell.py:132` (`DestructiveGitShellPreHook`, `DestructiveShellPreHook`) fire on every shell invocation regardless of whether daemon-side dispatch resolves to normal or iws. Rationale: iws's discarded upper-dir makes destructive ops *harmless to the main workspace* but does NOT make them safe in absolute terms (the iws namespace still contains real files; `rm -rf /` inside an iws kills the agent's working state, blowing up the session). The uniform-safety baseline beats a mode-specific carve-out. The daemon-RPC carve-out variant (`api.isolated_workspace.has_open_handle` → conditional pre-hook bypass) was honestly evaluated and rejected because the latency cost of an extra RPC per shell call AND the policy fragmentation it creates ("which destructive ops are blocked in which mode?") outweigh the agent-ergonomics benefit.

8. **`isolated_workspace` and `ephemeral_workspace` share the SAME primitives — no parallel pipeline.** The two execution contexts differ only in (a) where the overlay is mounted (per-call for `ephemeral_workspace`; once-per-session inside the iws namespace for `isolated_workspace`) and (b) whether the OCC `commit_prepared` step runs after change capture. Every other piece is identical:
   - **Overlay mount** — both invoke `sandbox/execution/overlay/kernel_mount.py::mount_overlay`. `ephemeral_workspace` wraps it per-call via `EphemeralPipeline`; `isolated_workspace` wraps it via `setns_overlay_mount` once at `enter_isolated_workspace`.
   - **Command exec** — both invoke `os.execvp(argv)`. `ephemeral_workspace` runs this in the daemon's mnt-ns under `EphemeralPipeline.execute_command`; `isolated_workspace` runs it inside the persistent iws ns via `setns_exec.py`.
   - **File-level mutation** — both invoke `_apply_edits` (from `edit.py:210-228`) and `_atomic_overwrite_no_follow` (from `write.py:211-236`). `ephemeral_workspace` wraps them with `prepare_single_path_changeset → commit_prepared`. `isolated_workspace` calls them directly inside the iws ns; no `commit_prepared`.
   - **Upperdir change-capture** — both invoke `walk_upperdir` from `sandbox/execution/overlay/capture.py:19`. `ephemeral_workspace` feeds the captured changeset into OCC. `isolated_workspace` discards the changeset after reporting `changed_paths` for telemetry.
   The `in_ns_runner.py` is a thin dispatcher composing these shared primitives inside the iws namespace — NOT a re-implementation. Structurally pinned as such by `test_in_ns_runner_only_composes_primitives.py`.

9. **`isolated_workspace` cannot access plugins.** Plugin RPC ops (`api.plugin.ensure`, `api.plugin.status`, AND any dynamically-registered `plugin.<name>.<op>`) MUST return `{"success": false, "error": {"kind": "forbidden_in_isolated_workspace", "message": "...", "details": {"agent_id": ..., "handle_id": ...}}}` when invoked by a `caller.agent_id` that has an open iws handle. Rationale: plugins can register arbitrary handlers (`api.plugin.ensure` flushes plugin-side `register_plugin_op` calls into the daemon dispatcher; verified at `sandbox/plugin/handler.py`). Allowing plugin ops inside an iws session would (a) leak iws's hermetic boundary (plugin handlers may touch `main_workspace` state directly, bypassing the iws's no-merge invariant) and (b) bypass the destructive-shell pre-hooks. The block is enforced at the daemon-side dispatcher entry point, not at the plugin module itself, so newly-registered `plugin.<name>.<op>` handlers inherit the block without per-plugin coding.

5. **R3 fence preserved via per-module top-level import deny-list.** The iws-branch lives at `sandbox/isolated_workspace/_branch.py` (sibling of `manager.py`/`handlers.py` inside the iws package). The ephemeral-branch lives at `sandbox/ephemeral_workspace/_branch.py`. CI asserts the following module-level import deny-list on `sandbox/isolated_workspace/_branch.py`, `sandbox/_shared/tool_primitives/*`, and `sandbox/_shared/tool_primitives/in_ns_runner.py` (the ephemeral-branch is intentionally NOT in the deny-list — it composes OCC, which is the whole point of `ephemeral_workspace`):
   - `sandbox.occ.*`
   - `sandbox.layer_stack.*`
   - `sandbox.daemon.{async_bridge, request_context, occ_backend, service.sandbox_overlay}`
   - **`sandbox.daemon.handler.{edit, write, read, grep, glob}`** (key: prevents sibling-import leak via `from ..write import _helper`).
   The CI mechanism is the existing module-local AST scan; only target modules and deny-list change. No callgraph walking (Python dynamic dispatch makes a true reachability test unsound).

6. **Result-shape unification via `workspace` discriminator on tool-op results only.** Add `workspace: Literal["ephemeral", "isolated"] = "ephemeral"` to `SandboxResultBase` (the tool-op result root). Lifecycle results extend the new `LifecycleResultBase` and do NOT carry the discriminator (lifecycle ops MUTATE which execution context is active; they don't run inside one). Tool-op callers branch on `workspace`; OCC fields (`conflict`/`conflict_reason`) read only when `workspace == "ephemeral"`. Lint enforcement is best-effort grep-based (`tests/static/test_occ_field_guard.py`); mypy-level Union narrowing deferred as follow-up.

7. **Consistent verb naming.** Rename `search_content` → `grep` (with `SearchContentRequest` → `GrepRequest`, `SearchContentResult` → `GrepResult`, `DAEMON_OP_SEARCH_CONTENT` → `DAEMON_OP_GREP="api.v1.grep"`); pre-existing `SearchContentResult.mode` field renamed to `output_mode` to free the discriminator slot. Rename `glob_files` → `glob` (with `DAEMON_OP_FIND_FILES` → `DAEMON_OP_GLOB="api.v1.glob"`).

---

## 3. Decision Drivers (top 3)

1. **Caller ergonomics — both layers.** Agents call `enter_isolated_workspace` / `exit_isolated_workspace` as tool calls just like `shell` or `edit_file`. Host code uses typed `sandbox.api.*` free functions. `_iws_rpc.py` becomes test-only sugar (or disappears).
2. **Implicit daemon-side mode dispatch is now safe.** Because lifecycle is explicit at the agent layer, the daemon-state lookup is observable in the audit log; the "implicit dispatch = unobservable" critique from earlier rounds no longer holds.
3. **Architectural separation of concerns.** Tool ops (mutate file contents) vs. lifecycle ops (mutate workspace binding) are different abstractions. Different bases, different audit classes, different RPC namespaces, different host-side modules.

---

## 4. Viable Options — Where do iws lifecycle tools live?

### Option L1 (RECOMMENDED): `backend/src/tools/isolated_workspace/{enter,exit}_isolated_workspace/`

- **Pros:** sibling to `tools/sandbox/` (the existing tool-verb tree). Name signals "lifecycle, not tool op." Matches the package boundary in `sandbox.api.lifecycle/`. Surfaces the categorical difference at filesystem level.
- **Cons:** new top-level subdirectory under `tools/`.
- **Why chosen:** the user's direction explicitly named this path. Also: matches the audit-class separation (lifecycle vs tool op) and avoids polluting `tools/sandbox/` with non-tool ops.

### Option L2: `backend/src/tools/sandbox/{enter,exit}_isolated_workspace/`

- **Pros:** one less directory; reuses `tools/sandbox/_lib/session.py` helpers without import-path twist.
- **Cons:** hides the lifecycle-vs-tool-op distinction; agents might think they're equivalent.
- **Rejected** in favor of L1.

### Option L3: top-level `backend/src/tools/lifecycle/` with `iws_enter`/`iws_exit`

- **Pros:** future-proofs for non-iws lifecycle tools (e.g., snapshot/restore).
- **Cons:** speculative; no other lifecycle tools exist.
- **Deferred:** if more lifecycle tools land later, move both then.

### Sub-decision: where does `_iws_branch.py` live on the daemon side?

(Carried over from v4 deliberation, unchanged.)

| Option | Status |
|---|---|
| 5a — `sandbox/daemon/handler/_iws_branch.py` | Rejected: sibling-import leak risk from OCC-importing `write.py`/`edit.py`. |
| **5b — `sandbox/isolated_workspace/_branch.py` (sibling module inside the iws package)** | **CHOSEN (v10).** Lives in the iws package alongside `manager.py`/`handlers.py`; the R3 import deny-list is applied at the module level rather than via subpackage isolation. Symmetric counterpart `sandbox/ephemeral_workspace/_branch.py` for the ephemeral execution context. Both inherit their workspace's package-level documentation. |
| 5c — `sandbox/_shared/iws_dispatch.py` | Rejected: widens `_shared/` beyond pure compute helpers. |

### Sub-decision: how does the daemon resolve mode at tool-op dispatch time?

Implicit by `caller.agent_id` lookup in the iws manager (v5 adopted). This was previously rejected as Option 3 in v2 because lifecycle was not yet explicit at the agent layer. With v5 adding `enter_isolated_workspace`/`exit_isolated_workspace` as explicit agent-callable tools, the daemon-state lookup is fully observable via the audit log. The previous criticism no longer applies.

---

## 5. Pre-mortem (5 scenarios)

### Scenario A — `tool_primitives/` extraction silently changes normal-mode output
**Mitigation:** Capture a parity corpus BEFORE extraction (PR-A step 6). Replay it post-extraction; assert byte-for-byte equality of `_to_jsonable(daemon_response)`. CI-gated.

### Scenario B — R3 fence regression via sibling-import leak
**Mitigation:** Subpackage isolation (Option 5b) + module-level deny-list including the sibling-handler ban (`sandbox.daemon.handler.{edit,write,read,grep,glob}`). CI test `test_iws_branch_isolation_invariant.py`.

### Scenario C — `WorkspaceSession` lifecycle leaks (host-side)
**Mitigation:** `async with`-first; NO iws free-function shorthand at the session layer; `__del__` warning on GC-while-open; daemon-side TTL sweep as backstop.

### Scenario D — Atomic deletion of iws tool RPC ops breaks an unaudited caller
**Mitigation:** Grep audit verified (Critic v3 confirmed): only caller is `_iws_rpc.py` (test fixture). PR-A migrates `_iws_rpc.py` in the SAME PR. No transitional shims.

### Scenario E (NEW for v5) — Mode-dispatch race at the daemon handler boundary
Agent A calls `exit_isolated_workspace` and immediately fires a `shell` tool call before the exit completes. The shell handler queries `pipeline.get_handle(agent_id)` and sees `None`, dispatches normal-mode — but the manager's exit is still tearing down the namespace, leaving partial state. Or vice versa: enter is in progress, manager has a handle but it's not yet usable.
**Mitigation:** Safety derives from a precise ordering invariant in the manager's state machine (verified at `manager.py:671,679,775-786`):
- **On `enter`**: `_wire_handle(...)` (overlay mount, veth, cgroup setup, etc., line 671) MUST complete BEFORE the `async with self._map_lock: self._by_agent[agent_id] = handle_id` insert (line 679-681). So `pipeline.get_handle(agent_id)` returns `None` while the handle is still being constructed; once it returns non-`None`, the handle is fully wired.
- **On `exit`**: removal from `_by_agent` (line 775-782) MUST precede `_teardown(...)` (line 785-786). So during in-flight exit, `pipeline.get_handle(agent_id)` returns `None` and concurrent tool calls dispatch normal-mode — which is safe because normal-mode handlers never touch the iws namespace.
- `pipeline.get_handle` itself is a lock-free dict read (`manager.py:367-369`) — GIL-atomic on the dict operation, NOT `_map_lock`-serialized. The race-free dispatch relies entirely on the wire-before-insert and remove-before-teardown ordering, NOT on the lookup itself being locked. This is a load-bearing invariant; the test below pins it.
- Test: `backend/src/task_center_runner/tests/mock/sandbox/isolated_workspace/concurrency/test_get_handle_returns_none_during_wire_and_teardown.py` — runs against the REAL `IsolatedPipeline` with `asyncio.Event` barriers injected at `_wire_handle` entry and `_teardown` exit (via a subclass that overrides the runtime port; NO FakeRuntime tier). Asserts:
  - During in-flight enter (barrier at `_wire_handle` start, blocked until released): `pipeline.get_handle(agent_id)` returns `None` AND the unified daemon handler dispatches normal-mode.
  - During in-flight exit (barrier at `_teardown` start, blocked until released): `pipeline.get_handle(agent_id)` returns `None` AND the unified daemon handler dispatches normal-mode.
  - After `_wire_handle` completes and `_by_agent` insert lands: `pipeline.get_handle(agent_id)` returns the handle AND iws-branch dispatch routes correctly.

---

## 6. Plan — Numbered Steps + Verification

### PR-FOLD — Create three workspace packages + symmetric pipeline renames + plugin relocation (folder reorg; mechanical; ships first)

This PR establishes the symmetric folder structure that the rest of the plan builds on. No behavior change — pure code motion, class renames, and import-path updates. ~220 import-statement updates total, all internal to `sandbox/`.

#### Phase 0a — Create the three workspace packages

0.a.1. **Create `sandbox/main_workspace/__init__.py`** (NEW, doc-anchor only — no code moves):
   - Package docstring: `"main_workspace = base repo + LayerStack snapshots. Persistent identity. Written ONLY through OCC."`
   - Lists implementing modules: `sandbox/layer_stack/**`, `sandbox/occ/**`, `sandbox/daemon/handler/workspace.py` (workspace binding helpers).
   - Rationale: moving `layer_stack/` or `occ/` would force 500+ import-statement updates across the codebase outside `sandbox/` for no behavioral benefit. The `main_workspace/__init__.py` is a discoverability anchor pointing readers at the actual implementations.
   → **Verify:** `from sandbox import main_workspace` succeeds; `main_workspace.__doc__` returns the expected text.

0.a.2. **Create `sandbox/ephemeral_workspace/`** (NEW package). Initially just `__init__.py` with the package docstring; the file moves in Phase 0b populate it.
   - Package docstring: `"ephemeral_workspace = per-tool-call execution context. Created/destroyed implicitly by the daemon. Mounts overlay against main_workspace's snapshot, runs the tool op, captures the upperdir changeset, commits via OCC, tears down."`

0.a.3. **`sandbox/isolated_workspace/`** is unchanged structurally (already exists). The pipeline rename in Phase 0c modifies it in place.

0.a.4. **Update top-level `sandbox/__init__.py`** to mention the three workspace packages in its docstring. Cross-link to `docs/sandbox/api_surface.md` (step 27) for the trichotomy explanation.

#### Phase 0b — Symmetric pipeline renames (ephemeral side)

0.b.1. **Move + rename: `sandbox/daemon/service/sandbox_overlay.py` → `sandbox/ephemeral_workspace/pipeline.py`**. The file IS the per-call pipeline (mount → exec → walk_upperdir → OCC commit → unmount); the new name accurately describes it. Update class name `EphemeralPipeline` → `EphemeralPipeline`. Update `OverlayLayerStackClient` and `OperationOverlayHandle` if they need similar clarity (TBD by implementer; conservative default = keep).
   - Update all imports across the codebase: ~50 callers, mechanical search-and-replace.
   - Update `__all__` exports and any string references in error messages.
   → **Verify:** `make test` green; `grep -rn "SandboxOverlay\b" backend/src` returns zero hits outside CHANGELOG.

0.b.2. **Move: `sandbox/daemon/service/shell_runner.py` → `sandbox/ephemeral_workspace/shell.py`**. Export `execute_shell_api` (function name preserved). Update ~10 imports.
   → **Verify:** dispatcher's `api.v1.shell` registration still resolves.

0.b.3. **Move: `sandbox/daemon/service/shell_job_handler.py` → `sandbox/ephemeral_workspace/shell_job.py`**. Export `shell_launch`, `shell_poll`, `shell_cancel`, `shell_reap`, `shell_metrics`. Also move `sandbox/daemon/service/shell_job.py` → `sandbox/ephemeral_workspace/_shell_job_state.py` (the job-state dataclass is private to the handler module).
   → **Verify:** background-shell tests pass.

0.b.4. **Move: `sandbox/daemon/service/overlay_manager.py` → `sandbox/ephemeral_workspace/overlay_lease.py`**. This file is the overlay-lease manager (acquires the snapshot, hands the leased view to `EphemeralPipeline`); the new name describes its role.
   → **Verify:** overlay-lease tests pass; flush/stop paths green.

0.b.5. **Move: `sandbox/daemon/service/overlay_events.py` → `sandbox/ephemeral_workspace/events.py`**. Event-bus for the pipeline; co-located.

0.b.6. **Move: `sandbox/daemon/service/layer_stack_client.py`** — stays in `daemon/service/` (it's a generic layer-stack client used by both ephemeral pipeline AND `isolated_workspace/manager.py` to acquire snapshots; NOT ephemeral-specific). Add a docstring note.

0.b.7. **`sandbox/daemon/service/` is now empty** (or near-empty if some helpers don't fit either workspace). Delete the directory if empty; otherwise keep with the remaining mode-agnostic helpers.
   → **Verify:** `ls sandbox/daemon/service/` shows only mode-agnostic infrastructure or is gone.

#### Phase 0c — Symmetric pipeline renames (isolated side)

0.c.1. **Rename in place: `sandbox/isolated_workspace/pipeline.py` → `sandbox/isolated_workspace/pipeline.py`**. Update class name `IsolatedPipeline` → `IsolatedPipeline`. Update `IsolatedWorkspaceError` → keep (the error type is package-level, not pipeline-specific). Update `require_pipeline`, `set_manager`, `get_active_pipeline` → `require_pipeline`, `set_pipeline`, `get_active_pipeline`.
   - Update all imports across the codebase: ~30 callers (handlers.py, tests, dispatcher peer-bootstrap, the new `_branch.py` from PR-A step 11).
   → **Verify:** `make test` green; iws tier 1-9 tests pass; `grep -rn "IsolatedPipeline\b" backend/src` returns zero hits.

0.c.2. **Rename references in `sandbox/isolated_workspace/__init__.py` docstring** to reflect the new pipeline-vs-manager terminology.

#### Phase 0d — Relocate plugin subsystem under ephemeral_workspace

Per Principle 9, plugins are blocked in iws mode (the dispatcher rejects `api.plugin.*` and `plugin.<name>.<op>` when the agent has an open iws handle). Today's `sandbox/plugin/` is also implementation-coupled to ephemeral mode (e.g., `overlay_dispatch.py` and `overlay_child.py` integrate plugins with `EphemeralPipeline`'s overlay context). The current top-level placement obscures this coupling. Move the package to make the ephemeral-only nature structural:

0.d.1. **Move: `sandbox/plugin/` → `sandbox/ephemeral_workspace/plugin/`** (entire subtree, including `runtime/` subpackage):
   - `sandbox/plugin/__init__.py` → `sandbox/ephemeral_workspace/plugin/__init__.py`
   - `sandbox/plugin/handler.py` → `sandbox/ephemeral_workspace/plugin/handler.py` (the `api.plugin.ensure`/`api.plugin.status` RPC handlers)
   - `sandbox/plugin/install.py`, `op_context.py`, `op_registry.py`, `overlay_child.py`, `overlay_dispatch.py`, `projection.py`, `session.py` → `sandbox/ephemeral_workspace/plugin/` (same filenames)
   - `sandbox/plugin/runtime/` → `sandbox/ephemeral_workspace/plugin/runtime/`
   - Update package docstring on the new `__init__.py`: `"Plugin subsystem — ephemeral-only by design. Dispatcher Principle 9 rejects plugin RPCs when an iws handle is open for the calling agent."`
   - Update imports across the codebase: ~70 callers (handlers, dispatcher peer-bootstrap, plugin runtime modules importing each other via relative paths).
   → **Verify:** `make test` green; plugin tests pass; `grep -rn "from sandbox.plugin\b\|import sandbox.plugin\b" backend/src` returns zero hits.

0.d.2. **Update dispatcher peer-bootstrap** in `sandbox/daemon/rpc/dispatcher.py` to import `from sandbox.ephemeral_workspace.plugin import handler as plugin_handler`. Same for any other top-level references.

0.d.3. **Update top-level `sandbox.api/__init__.py`** if it re-exports any plugin types.

#### Phase 0e — Sanity sweep

0.e.1. Run the full test suite. Expected outcome: green. If any test fails because of a missed import, fix the import path in that test.

0.e.2. Run `grep -rn "sandbox\.daemon\.service\.\(sandbox_overlay\|shell_runner\|shell_job\|overlay_manager\|overlay_events\)" backend/src` — expected zero hits.

0.e.3. Run `grep -rn "sandbox\.plugin\b\|sandbox\.isolated_workspace\.manager\.\(IsolatedPipeline\|require_pipeline\|set_manager\|get_active_pipeline\)" backend/src` — expected zero hits.

**End of PR-FOLD.** Zero behavior change; ~220 import updates and 2 class renames. Establishes:
- Three workspace sibling packages with symmetric pipeline modules: `ephemeral_workspace/pipeline.py` (class `EphemeralPipeline`) and `isolated_workspace/pipeline.py` (class `IsolatedPipeline`).
- Plugin subsystem physically nested under `ephemeral_workspace/plugin/` — making the ephemeral-only coupling structural rather than just documented.
- `daemon/service/` either empty or holding only mode-agnostic infrastructure.

---

### PR-0 — Verb renames (mechanical, independently shippable)

1. **Rename `search_content` → `grep`** across the codebase:
   - **a.** Audit: `grep -nE "^\s*mode:\s" backend/src/sandbox/_shared/models.py` — confirm `SearchContentResult.mode` is the only pre-existing `XxxResult.mode` attribute.
   - **b.** Rename the field: `SearchContentResult.mode` → `output_mode`. Update projector at `sandbox/api/tool/core/results.py` and daemon handler at `sandbox/daemon/handler/search.py`.
   - **c.** Rename dataclasses: `SearchContentRequest` → `GrepRequest`; `SearchContentResult` → `GrepResult` in `sandbox/_shared/models.py`.
   - **d.** Rename transport constant: `DAEMON_OP_SEARCH_CONTENT` → `DAEMON_OP_GREP = "api.v1.grep"`.
   - **e.** Rename handler module + function: `sandbox/daemon/handler/search.py` → `grep.py`; `async def search_content` → `async def grep`.
   - **f.** Update dispatcher: drop `"api.search_content"` / `"api.v1.search_content"`; add `"api.v1.grep"`.
   - **g.** Update host-side wrapper: `sandbox/api/tool/grep.py` function `search_content` → `grep`.
   - **h.** Update `sandbox/api/__init__.py` exports.
   - **i.** Update production caller `backend/src/tools/sandbox/grep/grep.py:11,148`.
   - **j.** Update all tests.
   → **Verify:** `grep -rn "search_content\|SearchContent" backend/src` returns zero hits (excluding CHANGELOG). `grep -nE "^\s*mode:\s" backend/src/sandbox/_shared/models.py` returns zero hits inside `XxxResult`. `make test` green.

2. **Rename `glob_files` → `glob`**:
   - `DAEMON_OP_FIND_FILES` → `DAEMON_OP_GLOB = "api.v1.glob"`.
   - `sandbox/api/tool/glob.py`: `async def glob_files` → `async def glob`.
   - `sandbox/api/__init__.py`: rename export.
   - Move daemon handler `find_files` to `sandbox/daemon/handler/glob.py` (symmetry with verb file naming).
   - All callers + tests.
   → **Verify:** `grep -rn "find_files\|glob_files" backend/src` returns zero hits.

3. **CHANGELOG entry** for the rename.
   → **Verify:** entry exists.

---

### PR-A — Daemon-side unification + iws tool-op deletion + agent-level lifecycle tools

#### Phase 1 — Result types + failing tests (lock target)

4. **Add `workspace: Literal["ephemeral", "isolated"] = "ephemeral"` to `SandboxResultBase`** in `sandbox/_shared/models.py`. (Per Principle 6 the field type is two-valued on success results; failure-event projections widen to three values per Principle 4 — see step 13.)

5. **Add `LifecycleResultBase`** in `sandbox/_shared/models.py` (NEW):
   ```python
   @dataclass(frozen=True, kw_only=True)
   class LifecycleResultBase:
       success: bool = True
       timings: dict[str, float] = field(default_factory=dict)
       error: LifecycleError | None = None

   @dataclass(frozen=True, kw_only=True)
   class LifecycleError:
       kind: str
       message: str = ""
       details: dict[str, str] = field(default_factory=dict)
   ```
   (Separate from `SandboxResultBase`/`ConflictInfo` because lifecycle errors are categorical mismatches like `already_open`, `quota_exceeded`, `host_ram_pressure`, not OCC conflicts.)

6. **Add `EnterIsolatedWorkspaceRequest`/`Result`** and **`ExitIsolatedWorkspaceRequest`/`Result`** in `sandbox/_shared/models.py`:
   ```python
   @dataclass(frozen=True, kw_only=True)
   class EnterIsolatedWorkspaceRequest(SandboxRequestBase):
       layer_stack_root: str
       # caller.agent_id keys the iws handle

   @dataclass(frozen=True, kw_only=True)
   class EnterIsolatedWorkspaceResult(LifecycleResultBase):
       manifest_version: str = ""
       manifest_root_hash: str = ""

   @dataclass(frozen=True, kw_only=True)
   class ExitIsolatedWorkspaceRequest(SandboxRequestBase):
       grace_s: float = 5.0

   @dataclass(frozen=True, kw_only=True)
   class ExitIsolatedWorkspaceResult(LifecycleResultBase):
       evicted_upperdir_bytes: int = 0
       lifetime_s: float = 0.0
       phases_ms: dict[str, float] = field(default_factory=dict)
   ```
   → **Verify:** mypy clean; best-effort static lint `tests/static/test_occ_field_guard.py` flags `.conflict`/`.conflict_reason` reads outside `workspace == "ephemeral"` branches under `backend/src/sandbox/**` (does not apply to lifecycle results because they have no `conflict` field — orthogonal).

7. **Write failing golden contract tests** at `backend/src/task_center_runner/tests/mock/sandbox/api_parity/test_<verb>.py` — one per tool verb. Each enters an iws via the new `sandbox.api.enter_isolated_workspace`, fires the tool verb (no workspace parameter), exits, and asserts iws-vs-normal output parity + correct `workspace` discriminator + `conflict is None` in iws mode. Initially `xfail(reason="phase-2/3-in-progress")`.

#### Phase 2 — Compute extraction + parity corpus

8. **Capture parity corpus** before extraction. Fixture: `backend/src/task_center_runner/tests/mock/sandbox/_fixtures/tool_primitives_parity_corpus.json`. Generator: `backend/scripts/gen_tool_primitives_parity_corpus.py`. ≥20 cases per verb, AND ≥10 cases each for `_apply_edits` / `_atomic_overwrite_no_follow` / `walk_upperdir` since these are now extracted or re-exported from `edit.py`/`write.py`/`sandbox/execution/overlay/capture.py` and need byte-equivalent parity guarantees too.

9. **Consolidate shared primitives** into `sandbox/_shared/tool_primitives/` (NEW package). Note: several primitives are ALREADY extracted at the right scope today — v8 consolidates them under one import path so the iws branch and normal handlers depend on identical symbols:
   - `compute_grep.py`, `compute_glob.py`, `compute_read.py` — pure compute (regex compile, walker, UTF-8 read). **NEW extractions.**
   - `file_ops.py` — `atomic_overwrite_no_follow` (extracted from `write.py:211-236`), `apply_edits` (extracted from `edit.py:210-228` — anchor-miss raises `ValueError` exactly as today). **NEW extractions**; both are pure functions with zero OCC coupling so extraction is mechanical.
   - `capture.py` — **thin facade re-exporting `walk_upperdir` from `sandbox/execution/overlay/capture.py:19`** (where it already lives today). NOT a new extraction — `EphemeralPipeline` already imports it as a sibling at `sandbox_overlay.py:23`. v8's role: stabilize the import path so both `EphemeralPipeline` (today's caller) and the new `_iws_branch.py` reference `_shared/tool_primitives/capture.py`. The function returns `list[OverlayPathChange]` (whiteout / opaque_dir / regular tuples), NOT `list[str]` — projection to `changed_paths: list[str]` happens at the result-projection layer (see step 11 and §7 acceptance).
   - `mount.py` — thin facade re-exporting `mount_overlay` from `sandbox/execution/overlay/kernel_mount.py:49` (where it already lives). `setns_overlay_mount.py:65-77` already imports + calls it (verified). v8's role: import-path stabilization.
   - Re-export from existing daemon handlers AND from `EphemeralPipeline`. Normal-mode handlers (`write.py`, `edit.py`) now CALL `tool_primitives.file_ops.*` instead of defining the helpers inline — byte-equivalent output guaranteed by the parity corpus (step 8).
   → **Verify:**
   - Parity-corpus replay byte-for-byte equal across all normal-mode handlers.
   - New `tests/static/test_tool_primitives_no_occ_imports.py` enforces deny-list (`sandbox.occ.*`, `sandbox.layer_stack.*`, etc.).
   - New `tests/static/test_normal_mode_handlers_import_primitives.py` asserts `write.py`/`edit.py`/`EphemeralPipeline` import from `_shared.tool_primitives.file_ops` / `.capture` (verifies normal mode actually uses the shared primitives — not just iws).
   - Existing `EphemeralPipeline` tests pass unchanged (the change-capture primitive is the same logic, now in a shared module).

10. **Add in-ns runner** `sandbox/_shared/tool_primitives/in_ns_runner.py` (NEW — thin dispatcher composing the shared primitives; NOT a re-implementation). Invoked as `python in_ns_runner.py --op {read|write|edit|grep|glob} < stdin.json > stdout.json`.
   - Imports only `sandbox._shared.tool_primitives.*` + stdlib (R10-pinned).
   - For `read|grep|glob`: calls the matching `compute_*` and emits the typed JSON.
   - For `write|edit`: calls `file_ops.atomic_overwrite_no_follow` / `file_ops.apply_edits` against the file path **inside the iws overlay's mount point** (no OCC commit; the overlay's COW semantics put the change in the upperdir automatically).
   - **No `shell_capture` op.** Shell-result capture is HOST-SIDE per step 11 — capture is a host filesystem op against `handle.upperdir`, not a sandboxed op. The runner only handles ops that need a process inside the iws namespace.
   → **Verify:**
   - Subprocess tests per `--op`; assert JSON envelope, atomicity.
   - Extended `test_setns_exec_discipline` pins `in_ns_runner.py`'s module-level imports to only `tool_primitives.*` + stdlib.
   - `tests/static/test_in_ns_runner_only_composes_primitives.py` (NEW) — AST scan asserts every op-branch's function body is a single delegating call to a `tool_primitives.*` function (no algorithmic logic embedded in the runner itself). Pins the "thin dispatcher" property structurally.

#### Phase 3 — Unified daemon handlers with implicit mode dispatch

11. **Add iws-branch module at `sandbox/isolated_workspace/_branch.py`** (per the v10 folder reorg — lives inside the `isolated_workspace/` package, NOT under `daemon/handler/iws/`). Per Principle 8 the branch composes shared primitives — for shell it post-processes the existing `setns_exec` path with `capture.walk_upperdir` (HOST-SIDE, against `handle.upperdir`); for typed verbs it dispatches the in-ns runner which itself only composes `tool_primitives.*`. The package's `__init__.py` is unchanged (lifecycle module); `_branch.py` is a new sibling that is R3-bounded (same import deny-list as `ops_handlers.py` was, before its deletion). Symmetric counterpart: `sandbox/ephemeral_workspace/_branch.py` (step 11b below).
    ```python
    # sandbox/isolated_workspace/_branch.py
    import json, sys
    from sandbox.isolated_workspace.pipeline import IsolatedWorkspaceError, require_pipeline
    from sandbox._shared.tool_primitives.in_ns_runner import IN_NS_RUNNER_PATH

    async def run_in_isolated(args: dict, *, op: str) -> dict:
        manager = require_pipeline()
        agent_id = args["caller"]["agent_id"]
        if op == "shell":
            # Step 1: exec via the existing setns_exec → os.execvp path (unchanged).
            res = await pipeline.run_in_handle(
                agent_id, argv=["/bin/sh", "-c", args["command"]],
            )
            # Step 2 (HOST-SIDE): walk the iws overlay's upperdir for changed_paths
            # via the SAME walk_upperdir primitive normal mode uses. The upperdir
            # lives at `handle.upperdir` (a host filesystem path); capture is a
            # host-side fs op, NOT a sandboxed op — so we do NOT route it through
            # run_in_handle/setns_exec. (The architect verified the iws upperdir
            # is host-visible but not naturally addressable from inside the mount
            # namespace; capture-on-host is the correct topology.)
            handle = pipeline.get_handle(agent_id)
            from sandbox._shared.tool_primitives.capture import walk_upperdir
            changes = walk_upperdir(handle.upperdir)  # list[OverlayPathChange]
            return _project_shell_with_capture(res, changes)
        # Typed verbs: dispatch the in_ns_runner which calls tool_primitives.*
        # directly (file_ops.atomic_overwrite_no_follow / file_ops.apply_edits /
        # compute_grep / compute_glob / compute_read). No OCC commit.
        res = await pipeline.run_in_handle(
            agent_id,
            argv=[sys.executable, IN_NS_RUNNER_PATH, "--op", op],
            stdin=json.dumps(args).encode("utf-8"),
        )
        return _project_typed(res, op=op)
    ```
    Signature `pipeline.run_in_handle(agent_id, argv=..., stdin=...)` matches `manager.py:847-854`. `pipeline.get_handle(agent_id)` is the lock-free dict read at `manager.py:367-369` — safe per the Scenario E ordering invariant (`_wire_handle` completes before insert; remove precedes teardown). Projection helpers set `mode="isolated"`, `conflict=None`, project `list[OverlayPathChange]` to `changed_paths: list[str]` while preserving kind information in a sibling `changed_path_kinds: list[str]` field (so whiteouts and opaque_dirs remain visible to callers; see §7 acceptance).
    → **Verify:**
    - Unit tests against faked manager: exercise both shell (exec + capture) and typed-verb dispatch.
    - `tests/static/test_iws_branch_isolation_invariant.py` (renamed from `test_isolated_workspace_ops_import_fence`) enforces Principle 5 deny-list AND Principle 8 (asserts `_iws_branch.py` only imports from `sandbox._shared.tool_primitives.*`, `sandbox.isolated_workspace.pipeline`, and stdlib — no other `sandbox.*` reachable).
    - New test `test_iws_shell_reports_changed_paths.py`: iws shell that mutates `/testbed/x.txt` returns a result with `changed_paths == ["/testbed/x.txt"]` AND the file remains in the iws upperdir (not committed to main workspace).

11b. **Add ephemeral-branch module at `sandbox/ephemeral_workspace/_branch.py`** (symmetric counterpart to step 11). Composes the same primitives in the daemon's namespace via the existing `EphemeralPipeline` per-call assembly:
    ```python
    # sandbox/ephemeral_workspace/_branch.py
    from sandbox.ephemeral_workspace.pipeline import EphemeralPipeline
    from sandbox.ephemeral_workspace.shell import execute_shell_api
    from sandbox._shared.tool_primitives.file_ops import apply_edits, atomic_overwrite_no_follow
    from sandbox._shared.tool_primitives.capture import walk_upperdir
    from sandbox._shared.tool_primitives import compute_grep, compute_glob, compute_read
    # ... OCC integration via existing prepare_single_path_changeset → commit_prepared

    async def run_in_ephemeral(args: dict, *, op: str) -> dict:
        """Per-tool-call execution context: mount → exec → capture → OCC commit → unmount.
        Composes the SAME tool_primitives as isolated_workspace; the only divergence
        is that this branch invokes commit_prepared at the end."""
        if op == "shell":
            return await execute_shell_api(args)
        if op == "edit":
            return await _ephemeral_edit(args)  # apply_edits + OCC commit
        if op == "write":
            return await _ephemeral_write(args)  # atomic_overwrite_no_follow + OCC commit
        # ... etc.
    ```
    The body is largely a re-organization of code that today is scattered across `sandbox/daemon/handler/{shell,write,edit,read,grep,glob}.py`'s normal-mode body. v10's reorg pulls it into one place so the symmetry with `isolated_workspace/_branch.py` is structural, not just conceptual.
    → **Verify:** parity corpus replay (step 8) still passes byte-for-byte after the relocation. `tests/static/test_ephemeral_branch_imports.py` (NEW) asserts `_branch.py` imports from `tool_primitives.*`, `daemon.service.sandbox_overlay`, OCC (allowed for ephemeral), and the existing OCC commit helpers — NOT from `isolated_workspace.*`.

12. **Add implicit workspace-resolution to each `sandbox/daemon/handler/<verb>.py`**:
    ```python
    from sandbox.isolated_workspace._branch import run_in_isolated
    from sandbox.isolated_workspace.pipeline import get_active_pipeline  # nil-safe getter
    from sandbox.isolated_workspace._branch import run_in_isolated
    from sandbox.ephemeral_workspace._branch import run_in_ephemeral

    async def edit_file(args: dict) -> dict:
        # Workspace resolved by daemon: if agent has open iws handle, route iws-branch;
        # else route ephemeral-branch. Both branches return the same projected shape
        # (workspace discriminator stamped accordingly).
        manager = get_active_pipeline()
        if manager is not None and pipeline.get_handle(args.get("caller", {}).get("agent_id", "")) is not None:
            return await run_in_isolated(args, op="edit")
        return await run_in_ephemeral(args, op="edit")
    ```
    Apply to all 6 handlers (`shell.py`, `read.py`, `write.py`, `edit.py`, `grep.py`, `glob.py`). After v10's reorg each handler is ~5 lines — a thin workspace-router. The substantive logic lives in `ephemeral_workspace/_branch.py` and `isolated_workspace/_branch.py`. `get_active_pipeline()` is a NEW nil-safe accessor (returns `None` if the iws manager isn't initialized — e.g., daemons with no iws traffic) added to `sandbox/isolated_workspace/pipeline.py`. Today's `require_pipeline` raises; the handler needs `get_active_pipeline` to avoid forcing manager bootstrap on every tool call.
    → **Verify:**
    - Scenario E test (`test_get_handle_returns_none_during_wire_and_teardown.py`) runs against the REAL `IsolatedPipeline` (not a faked port — the invariant lives in real `_map_lock` ordering) with `asyncio.Event` barriers injected at `_wire_handle` entry and `_teardown` exit; asserts that during both in-flight enter and in-flight exit, `get_handle` returns `None` AND the unified handler dispatches normal-mode.
    - Golden contract tests from step 7 flip green.
    - Normal-mode handler tests pass unchanged when no iws handle exists.

#### Phase 4 — Host-side lifecycle API

13. **Create `sandbox/api/lifecycle/`** subpackage (NEW, parallel to `sandbox/api/tool/`):
    - `__init__.py` re-exports the two coroutines.
    - `enter_isolated.py`:
      ```python
      async def enter_isolated_workspace(
          sandbox_id: str,
          request: EnterIsolatedWorkspaceRequest,
          *,
          audit_sink: AuditSink | None = None,
          transport: SandboxTransport | None = None,
      ) -> EnterIsolatedWorkspaceResult:
          # wraps in lifecycle_operation(...) — NOT audited_operation
          # dispatches to api.isolated_workspace.enter
      ```
    - `exit_isolated.py` — same shape for `api.isolated_workspace.exit`.
    - New audit helper: `sandbox/audit/lifecycle.py` exposing `lifecycle_operation(...)` — analogue of `audited_operation` but publishes `workspace_lifecycle_*` events (not `SandboxOperation` events).
    - **Audit translator updates (concrete file enumeration):**
      - `sandbox/audit/translation.py` (today: `SandboxOperation = Literal[...]` at line 16): ADD `WorkspaceLifecycle = Literal["enter_isolated_workspace", "exit_isolated_workspace"]` typed enum. ADD `publish_lifecycle_started`, `publish_lifecycle_result`, `publish_lifecycle_failed` translator functions parallel to today's `publish_operation_started/result/failed` at lines 31, 46, 76, 106, 129. The new translators MUST NOT shadow the existing `SandboxOperation` translators — explicit non-overlap (mypy `assert_never` over the union catches additions).
      - `sandbox/audit/events.py`: ADD `WORKSPACE_LIFECYCLE_STARTED`, `WORKSPACE_LIFECYCLE_COMPLETED`, `WORKSPACE_LIFECYCLE_FAILED` event-type constants. These are SEPARATE from the existing `SANDBOX_OP_*` constants — downstream `audit.jsonl` consumers (live e2e harness, host-bus subscribers) get a clean discriminator.
      - `sandbox/audit/lifecycle.py` (NEW): wraps the translator calls in an `async def lifecycle_operation(...)` matching the shape of `sandbox/api/tool/core/audit.py::audited_operation` but using `publish_lifecycle_*`.
    - **Failure-event `workspace` field projection** (Critic A1 — see Principle 4): the `audited_operation` wrapper at `sandbox/api/tool/core/audit.py` is updated so its `publish_operation_failed` call stamps a `workspace` field onto the failure payload, sourced from a `workspace_hint: Literal["ephemeral","isolated","unknown"]` argument the caller (`WorkspaceSession`, free-function wrapper, agent-tool wrapper) supplies. The wrapper's default is `"unknown"`.
    → **Verify:**
    - Unit tests against fake transport; assert correct RPC op; assert lifecycle audit events fire (not tool-op events); assert `LifecycleError` projection on `success=False` daemon responses.
    - `grep -n "WorkspaceLifecycle\|workspace_lifecycle" backend/src/sandbox/audit/translation.py` returns ≥4 hits (Literal type def + 3 translator functions).
    - `grep -n "WORKSPACE_LIFECYCLE_" backend/src/sandbox/audit/events.py` returns 3 hits.
    - New failed-event test: `tests/unit_test/test_sandbox/test_audited_operation_failed_workspace_field.py` exercises `audited_operation` with each value of `workspace_hint`; asserts `workspace` field present on the published `sandbox_operation_failed` event.

14. **Re-export from `sandbox.api`**:
    - `sandbox/api/__init__.py` adds:
      ```python
      from sandbox.api.lifecycle.enter_isolated import enter_isolated_workspace
      from sandbox.api.lifecycle.exit_isolated import exit_isolated_workspace
      from sandbox._shared.models import (
          EnterIsolatedWorkspaceRequest, EnterIsolatedWorkspaceResult,
          ExitIsolatedWorkspaceRequest, ExitIsolatedWorkspaceResult,
          LifecycleError, LifecycleResultBase,
      )
      ```
    Update `__all__`.
    → **Verify:** import smoke test (`from sandbox.api import enter_isolated_workspace, exit_isolated_workspace`).

#### Phase 5 — Agent-level tool wrappers

15. **Create `backend/src/tools/isolated_workspace/`** (NEW, parallel to `backend/src/tools/sandbox/`):
    - `__init__.py` — package marker.
    - `enter_isolated_workspace/` subdir:
      - `enter_isolated_workspace.py` — `@tool`-decorated wrapper. Pydantic `EnterIsolatedWorkspaceInput` (just `layer_stack_root: str` — agent_id is from `caller_from_context`). Bridges to `sandbox.api.enter_isolated_workspace(sandbox_id, request)`. Output JSON: `{status, manifest_version, manifest_root_hash, timings, error?}`.
      - `prompt.py` — `get_enter_isolated_workspace_description()` for the LLM-facing tool description.
      - `__init__.py`.
    - `exit_isolated_workspace/` subdir — same pattern. Pydantic `ExitIsolatedWorkspaceInput` (just `grace_s: float = 5.0`). Output JSON: `{status, evicted_upperdir_bytes, lifetime_s, phases_ms, timings, error?}`.
    - Helpers from `tools.sandbox._lib.session` (`audit_kwargs_from_context`, `caller_from_context`, `sandbox_id_or_error`, `sandbox_audit_metadata`) — reused as-is (the helpers are mode-agnostic).
    → **Verify:**
    - Import smoke test.
    - Unit tests against a fake transport exercising the tool's full path (Input → `sandbox.api.*` → Result → ToolResult JSON).
    - Tests assert that calling `enter_isolated_workspace` does NOT emit a tool-op audit event AND DOES emit a `workspace_lifecycle_started`/`workspace_lifecycle_completed` pair.
    - **Principle 7 enforcement test** (`tests/mock/sandbox/isolated_workspace/policy/test_destructive_pre_hook_fires_in_iws_mode.py`): enter iws → call `tools/sandbox/shell/` with `rm -rf /testbed/foo` → assert the destructive pre-hook BLOCKS the call BEFORE it reaches the daemon, regardless of mode. Same test asserts a benign `ls` succeeds in iws mode (positive control). This pins the uniform-safety baseline from Principle 7.

#### Phase 6 — Delete the iws tool-op surface

16. **Delete `sandbox/isolated_workspace/ops_handlers.py`** and `sandbox/isolated_workspace/scripts/in_ns_write.py`.
    → **Verify:** files gone.

17. **Delete the 5 iws tool-op registrations** in `sandbox/daemon/rpc/dispatcher.py`. Atomic deletion — no transitional shims (only internal caller is `_iws_rpc.py`, migrated in PR-C). Remove the `from sandbox.isolated_workspace import ops_handlers as iws_ops_handlers` import. Keep the 5 lifecycle registrations.
    → **Verify:** dispatcher unit test enumerates: 5 lifecycle ops + 6 unified `api.v1.<verb>` ops + 0 iws tool ops.

18. **Block plugin access from `isolated_workspace`** (per Principle 9). Add a dispatcher-entry gate in `sandbox/daemon/rpc/dispatcher.py::dispatch_envelope_async`:
    - Before invoking `handler(args)` for any op whose name starts with `api.plugin.` OR `plugin.`, query `IsolatedPipeline.get_handle(args["caller"]["agent_id"])` via the same nil-safe `get_active_pipeline()` accessor introduced in step 12.
    - If the agent has an open iws handle, return `{"success": false, "warnings": [], "timings": {}, "error": {"kind": "forbidden_in_isolated_workspace", "message": "plugin access is blocked while an isolated workspace is open for this agent", "details": {"agent_id": ..., "handle_id": ..., "op": ...}}}` BEFORE the handler runs.
    - Else, proceed to the handler as today.
    - This is enforced at the dispatcher level — NOT at each plugin handler — so dynamically-registered `plugin.<name>.<op>` handlers inherit the block automatically.
    - **R3-safe**: dispatcher already imports the iws manager via `_load_peer_bootstraps`; no new OCC reachability is introduced. The dispatcher does NOT live under the iws-branch fence (it's the entry point of the daemon).
    → **Verify:**
    - `tests/mock/sandbox/isolated_workspace/policy/test_plugin_blocked_in_isolated_workspace.py` — enter iws → invoke `api.plugin.ensure` → assert `forbidden_in_isolated_workspace` error. Same test with a fictitious `plugin.foo.bar` op asserts the block extends to dynamic plugin handlers.
    - `test_plugin_allowed_when_no_iws_open.py` — without entering iws, plugin ops succeed as today (positive control).
    - Audit-bus: the dispatcher-gate rejection emits a `sandbox_operation_failed` event with `error.kind == "forbidden_in_isolated_workspace"` and `workspace == "isolated"`.

19. **Update R3 fence test** — rename `test_isolated_workspace_ops_import_fence` → `test_iws_branch_isolation_invariant.py`. Targets: `sandbox/isolated_workspace/_branch.py + sandbox/ephemeral_workspace/_branch.py`, `sandbox/_shared/tool_primitives/*`, `sandbox/_shared/tool_primitives/in_ns_runner.py`. Apply Principle 5 deny-list.
    → **Verify:** renamed test exists and passes.

**End of PR-A.** Single RPC tool-op namespace; `ops_handlers.py` deleted; iws-mode dispatch implicit via manager-state lookup; agent-level `enter_isolated_workspace`/`exit_isolated_workspace` tools exist; host-side `sandbox.api.{enter,exit}_isolated_workspace` exist.

---

### PR-B — Host-side `WorkspaceSession` (sugar over the new free functions)

#### Phase 7 — `WorkspaceSession` async-context-manager

20. **Create `sandbox/api/workspace.py`** with `WorkspaceMode` enum and `WorkspaceSession`:
    - `WorkspaceSession.attach(sandbox_id, caller, *, transport=None, audit_sink=None) -> WorkspaceSession` — normal mode; `__aexit__` no-op.
    - `WorkspaceSession.enter_isolated(sandbox_id, caller, *, layer_stack_root, transport=None, audit_sink=None) -> WorkspaceSession` — iws; **internally calls `sandbox.api.enter_isolated_workspace`**; `__aexit__` **internally calls `sandbox.api.exit_isolated_workspace`**. Stores manifest version + root hash.
    - `await session.status()` — iws only; raises `WorkspaceModeError` in normal.
    - Tool methods `await session.{shell, read_file, write_file, edit_file, grep, glob}(request)` — each just calls the corresponding `sandbox.api.<verb>(self._sandbox_id, request, transport=self._transport, audit_sink=self._audit_sink)`. **Does NOT add a `workspace` field to the payload** — the daemon resolves mode by manager state (Principle 4).
    → **Verify:** unit tests against fake transport assert: `__aexit__` invokes `sandbox.api.exit_isolated_workspace` exactly once including on exception; tool methods do not pass a `workspace` field; audit-bus sees tool-op events from `audited_operation` and lifecycle events from `lifecycle_operation`.

21. **Repoint `sandbox/api/tool/{shell,read,write,edit,grep,glob}.py`** to delegate to `WorkspaceSession.attach(...).<verb>(...)`. Free-function signatures unchanged (back-compat).
    → **Verify:** existing api/tool tests pass unchanged.

    **No iws free-function shorthand.** Iws-mode tool calls happen via the agent's `enter_isolated_workspace` tool call (which mutates daemon state) followed by ordinary tool calls (which auto-dispatch iws-branch via Principle 4). Host code wanting explicit iws lifecycle uses `WorkspaceSession.enter_isolated`.

---

### PR-C — Test suite migration

#### Phase 8 — Migrate iws tests off `_iws_rpc.py`

22. **Migrate `_iws_rpc.py`** tool-op helpers to use `sandbox.api.<verb>` (which auto-dispatches mode); lifecycle helpers (`enter`, `exit_`, `status`, `list_open`) to use `sandbox.api.enter_isolated_workspace` / `exit_isolated_workspace` and the existing `api.isolated_workspace.{status,list_open}` raw RPC.
    - `_iws_rpc.py` shrinks to ≤30 lines (only `test_reset` and `status`/`list_open` remain as raw RPC — the latter two have no host-side coroutines because they are introspection-only).
    → **Verify:** `grep -rn "api\.isolated_workspace\.\(shell\|read_file\|write_file\|edit_file\|search_content\)" backend/src` returns zero hits. Tier 1-9 mock tests pass.

23. **Update `tests/mock/sandbox/isolated_workspace/happy_path/test_enter_then_shell_then_exit.py`** (and any analog) to drive the lifecycle through the new agent-level tools when possible, asserting:
    - Audit sequence: `workspace_lifecycle_started, workspace_lifecycle_completed, sandbox_operation_started(shell), sandbox_operation_completed(shell), workspace_lifecycle_started(exit), workspace_lifecycle_completed(exit)` — replacing today's mixed `enter, tool_call, exit` daemon-side JSONL sequence with the cleaner host-side audit-bus sequence.
    - The daemon-side JSONL mirror (`EOS_ISOLATED_WORKSPACE_AUDIT_PATH`) continues to record the existing `sandbox_isolated_workspace_{enter, exit, tool_call, evicted, gc_orphan}` events as a backstop diagnostic — unchanged.
    → **Verify:** test passes; audit sequence assertions match.

24. **Add Tier 1 tests for the new agent-level tools** under `backend/src/task_center_runner/tests/mock/sandbox/isolated_workspace/tool_wrappers/`:
    - `test_enter_isolated_workspace_tool.py` — full path Input → ToolResult; assert manifest fields, lifecycle audit pair.
    - `test_exit_isolated_workspace_tool.py` — full path; assert evicted bytes, lifetime, lifecycle audit pair.
    - `test_tool_dispatch_routes_iws_after_enter.py` — enter, then call `tools/sandbox/edit_file` — assert daemon dispatched the iws branch by reading the result's `workspace == "isolated"` field.
    - `test_tool_dispatch_routes_normal_after_exit.py` — enter, exit, then call `tools/sandbox/edit_file` — assert `workspace == "ephemeral"`.
    → **Verify:** tests pass.

25. **Update `tests/mock/sandbox/isolated_workspace/PLAN.md`** to reflect the new test layout (the existing Tier 1 / Tier 2 / etc. structure is preserved; we add a `tool_wrappers/` tier; the `_iws_rpc.py` deprecation is noted).
    → **Verify:** doc review.

#### Phase 9 — Documentation

26. **Update `docs/isolated_workspace_runtime_source_blast_radius.md`** with the new module set (`sandbox/api/lifecycle/`, `sandbox/api/workspace.py`, `tools/isolated_workspace/`, `_shared/tool_primitives/`, `daemon/handler/iws/`). Document Principle 8 prominently — explain that normal-mode handlers now delegate to `tool_primitives.*` and iws composes the same primitives inside its persistent ns.

27. **Add `docs/sandbox/api_surface.md`** documenting: unified tool-op contract, lifecycle-op contract, mode dispatch (implicit, daemon-side), R3 fence enforcement (subpackage isolation + module-level deny-list), and the agent-level tool wrappers.

28. **CHANGELOG entry.** Note: tool ops on `api.v1.*`; lifecycle ops on `api.isolated_workspace.{enter,exit,status,list_open,test_reset}` (unchanged wire); new host-side `sandbox.api.enter_isolated_workspace`/`exit_isolated_workspace`; new agent-level tools `tools/isolated_workspace/enter_isolated_workspace`, `tools/isolated_workspace/exit_isolated_workspace`.

---

## 7. Acceptance Criteria

### PR-0 (rename)
- `grep -rn "search_content\|SearchContent\|find_files\|glob_files" backend/src` returns zero hits.
- `grep -nE "^\s*mode:\s" backend/src/sandbox/_shared/models.py` returns zero hits inside `XxxResult` classes.
- All tests green; no behavior change.
- One production caller (`backend/src/tools/sandbox/grep/grep.py`) updated.

### PR-A (daemon unification + iws tool-op deletion + lifecycle host API + agent-level tools)
- `sandbox/isolated_workspace/ops_handlers.py` and `sandbox/isolated_workspace/scripts/in_ns_write.py` deleted.
- The 5 `api.isolated_workspace.<tool-op>` RPC registrations gone (atomic).
- `sandbox/_shared/tool_primitives/*` OCC-free (CI-asserted); parity-corpus byte-equal.
- `sandbox/_shared/tool_primitives/in_ns_runner.py` covers all 5 non-shell ops via `--op`; R10-pinned; structurally pinned as a thin dispatcher (`test_in_ns_runner_only_composes_primitives.py`). No `shell_capture` op (capture is host-side; see step 11).
- **Principle 8 — primitives reuse:** normal-mode handlers (`write.py`, `edit.py`, `EphemeralPipeline`) import `file_ops.atomic_overwrite_no_follow` / `file_ops.apply_edits` / `capture.walk_upperdir` from `tool_primitives/`. Asserted by `tests/static/test_normal_mode_handlers_import_primitives.py`. Both modes share the SAME `mount_overlay` (unchanged at `kernel_mount.py`), the SAME `os.execvp` path (unchanged at `setns_exec.py` for iws and `EphemeralPipeline.execute_command` for normal), the SAME `apply_edits`/`atomic_overwrite_no_follow` (now in `tool_primitives/file_ops.py`), and the SAME `walk_upperdir` (re-exported from `tool_primitives/capture.py`; original lives at `sandbox/execution/overlay/capture.py:19`). The iws branch differs only in (a) namespace context (`setns` wrapping) and (b) skipping `commit_prepared`.
- iws shell returns populated `changed_paths: list[str]` AND `changed_path_kinds: list[str]` (preserving whiteout / opaque_dir / regular kind info from `walk_upperdir`'s `OverlayPathChange` tuples). The changeset is NOT committed.
- Capture is HOST-SIDE — `_iws_branch.py` invokes `walk_upperdir(handle.upperdir)` directly on the daemon process, NOT via `pipeline.run_in_handle`. Asserted by `tests/static/test_iws_branch_shell_capture_is_host_side.py`: AST-scans `_iws_branch.py`'s shell op-branch and asserts the second `run_in_handle` call is absent; instead `walk_upperdir(handle.upperdir)` appears in the branch body.
- Verified by `test_iws_shell_reports_changed_paths.py`: an iws shell that runs `touch /testbed/foo` returns `changed_paths == ["/testbed/foo"]` with `changed_path_kinds == ["regular"]`; an iws shell that runs `rm /testbed/existing.txt` returns the path with `changed_path_kinds == ["whiteout"]`; the files never appear in main workspace.
- `sandbox/isolated_workspace/_branch.py` exists as a sibling module inside the iws package; passes Principle 5 deny-list (`test_iws_branch_isolation_invariant.py`). `sandbox/ephemeral_workspace/_branch.py` exists as the symmetric counterpart (allowed to import OCC; not subject to the iws deny-list).
- Iws `edit_file` performs real search/replace. **Falsifiable criteria:**
  - `applied_edits` equals the total number of `old_text` occurrences successfully replaced.
  - Non-anchor bytes byte-identical pre/post edit (memcmp).
  - Any `old_text` not found in source raises `ValueError` → daemon surfaces `{"success": false, "error": {"kind": "edit_anchor_miss", ...}}`.
- Iws `grep` honors every option in `GrepRequest`.
- Iws `glob` exists; routes through the same `api.v1.glob` RPC as normal mode.
- `sandbox.api.enter_isolated_workspace` and `sandbox.api.exit_isolated_workspace` exist as typed coroutines returning `EnterIsolatedWorkspaceResult` / `ExitIsolatedWorkspaceResult` (extending `LifecycleResultBase`, NOT `SandboxResultBase` — no `workspace` discriminator on lifecycle results).
- `backend/src/tools/isolated_workspace/{enter_isolated_workspace,exit_isolated_workspace}/` exist as `@tool`-decorated wrappers; their unit tests pass.
- Lifecycle tool calls emit `workspace_lifecycle_started/completed/failed` audit events (NOT `SandboxOperation` tool-op events). Tool-op calls continue to emit `SandboxOperation` events through `audited_operation`.
- Daemon-side execution-context dispatch is implicit: `sandbox/daemon/handler/<verb>.py` resolves mode by `pipeline.get_handle(caller.agent_id)` lookup; no `workspace` field is read from the request payload.
- Failure-event projection carries a `workspace: Literal["ephemeral","isolated","unknown"]` field (Principle 4); `audited_operation` accepts a `workspace_hint` arg from the calling wrapper. Failed-event workspace-field test (`test_audited_operation_failed_workspace_field.py`) covers all three values.
- `sandbox/audit/translation.py` exports a NEW `WorkspaceLifecycle` Literal enum + 3 translator functions; `sandbox/audit/events.py` exports 3 NEW `WORKSPACE_LIFECYCLE_*` event-type constants; both are non-overlapping with `SandboxOperation` (mypy `assert_never` over the union).
- Principle 7 enforced: iws shell tool calls pass through the destructive-shell pre-hooks (test `test_destructive_pre_hook_fires_in_iws_mode.py` asserts `rm -rf` blocks in both modes; benign commands succeed).
- **Principle 9 enforced**: plugin ops (`api.plugin.*` AND any `plugin.<name>.<op>`) are blocked when the calling agent has an open iws handle. Asserted by `test_plugin_blocked_in_isolated_workspace.py` (negative: returns `forbidden_in_isolated_workspace`) and `test_plugin_allowed_when_no_iws_open.py` (positive: succeeds normally). The dispatcher-entry gate is the enforcement point — dynamically-registered plugin ops inherit the block automatically.
- **Network policy explicit** (per Principle 5 / Block A): iws veth + bridge + nftables MASQUERADE allows outbound NAT only; no inbound port-exposure path exists. `test_iws_network_outbound_works.py` and `test_iws_network_no_inbound.py` pin the directional policy.

### PR-B (host-side `WorkspaceSession`)
- `WorkspaceSession` exposes typed tool methods covering both modes via the host-side free functions.
- `WorkspaceSession.enter_isolated` internally calls `sandbox.api.enter_isolated_workspace`; `__aexit__` calls `sandbox.api.exit_isolated_workspace`.
- Tool methods do NOT pass a `workspace` field to the daemon (workspace resolution is daemon-side).
- Free-function `sandbox.api.tool.{shell, read_file, write_file, edit_file, grep, glob}` callers see no behavior change.
- No iws free-function shorthand at the session layer; host code uses `async with`.

### PR-C (test suite migration)
- `_iws_rpc.py` reduced to ≤30 lines (only test-only ops remain).
- `grep -rn "api\.isolated_workspace\.\(shell\|read_file\|write_file\|edit_file\|search_content\)" backend/src` returns zero hits.
- New tests `test_enter_isolated_workspace_tool.py`, `test_exit_isolated_workspace_tool.py`, `test_tool_dispatch_routes_iws_after_enter.py`, `test_tool_dispatch_routes_normal_after_exit.py` exist and pass.
- All existing Tier 1-9 tests pass; audit sequences updated to match host-side bus emissions.
- Tier 8 soak passes within 10% of `baselines/tier8_phase_timings.json`.

---

## 8. Risks

| Risk | Severity | Mitigation |
|---|---|---|
| `tool_primitives/` extraction silently changes normal-mode output | High | Parity corpus + CI-gated replay; covers `file_ops`, `capture`, and `compute_*` extractions; normal-mode handlers verified byte-equivalent post-refactor. |
| Normal-mode handlers don't actually use the shared primitives (Principle 8 violated silently) | High | `tests/static/test_normal_mode_handlers_import_primitives.py` asserts `write.py`/`edit.py`/`EphemeralPipeline` import from `_shared.tool_primitives.*`; failing this test means a regression to embedded logic. |
| R3 fence regression via sibling-import leak | High | Subpackage isolation + deny-list + sibling-handler ban. |
| Daemon mode-resolution race (Scenario E) | Medium | `pipeline.get_handle` is a GIL-atomic dict read (NOT `_map_lock`-protected); safety derives from `_wire_handle` completing BEFORE `_by_agent` insert (`manager.py:671,679`) AND `_by_agent` removal preceding `_teardown` (`manager.py:775-786`). Pinned by `test_get_handle_returns_none_during_wire_and_teardown.py` running against the REAL manager with `asyncio.Event` barriers. |
| `get_active_pipeline` returns the wrong manager in concurrent daemon-bootstrap | Low | Idempotent setter (`set_manager` checks for existing); manager singleton bootstrapped under `_bootstrap_lock` (existing in `handlers.py:_ensure_manager`). |
| Single in-ns runner slower than today's `cat`/`grep` | Low-Medium | Same fork cost as today's `setns_exec`; benchmark + baseline. |
| Result-shape contract confusion via workspace discriminator | Medium-High | Best-effort grep-based lint; mypy union narrowing deferred. |
| Lifecycle leaks: `WorkspaceSession.enter_isolated` without exit | Medium | `async with`-first; daemon TTL sweep as backstop. |
| Rename collisions (`SearchContentResult.mode`) | High — addressed in PR-0 | Field renamed FIRST. |
| `run_in_handle` signature drift | Medium — caught in v3 review | Step 11 specifies the exact signature. |
| New `tools/isolated_workspace/` mis-categorized as a regular tool | Medium | Audit class separation: lifecycle events vs tool-op events. Documentation in `docs/sandbox/api_surface.md`. PR-A step 15 tests assert lifecycle audit pair (not tool-op events). |
| Test-suite churn across migration | Medium | PR-C migration runs in isolation after PR-A merges; tier 1-9 tests green at each PR boundary. |

---

## 9. Expanded Test Plan (deliberate mode)

### Unit
- Parity corpus replay; byte-for-byte equality.
- Each `compute_*` helper direct tests.
- `in_ns_runner.py` subprocess tests per `--op`; assert JSON envelope + atomicity.
- `run_in_isolated` against a faked `IsolatedPipeline`.
- `sandbox.api.enter_isolated_workspace` / `exit_isolated_workspace` unit tests against a fake transport:
  - Assert RPC op string is `api.isolated_workspace.enter` / `.exit`.
  - Assert audit events are `workspace_lifecycle_started/completed/failed` (NOT `SandboxOperation` triplets).
  - Assert `LifecycleError` projection on daemon `success=False`.
  - Assert `EnterIsolatedWorkspaceResult.manifest_version` populated from daemon response.
- `WorkspaceSession.__aexit__` always invokes `sandbox.api.exit_isolated_workspace` for iws including on exception; never for normal.
- Tool-wrapper unit tests:
  - `enter_isolated_workspace` tool's Input → `sandbox.api.*` → ToolResult path.
  - `exit_isolated_workspace` tool's Input → `sandbox.api.*` → ToolResult path.
- Edit-semantics falsifiability:
  - `applied_edits` equals total `old_text` occurrences replaced.
  - Non-anchor-region byte equality (memcmp).
  - Anchor-miss → `ValueError` → daemon `edit_anchor_miss`.
- Mode-dispatch race:
  - `test_dispatch_during_in_flight_exit_falls_back_to_normal_mode.py` — concurrent exit + shell; assert no partial-state corruption.

### Integration (mock)
- `tests/mock/sandbox/api_parity/test_<verb>.py` — for each tool verb, drive both modes through `WorkspaceSession` against the mocked daemon. Assert workspace discriminator; assert single `api.v1.<verb>` RPC op used (no `workspace` field in payload).
- Failure-mode injection on `in_ns_runner.py`; assert error projection.
- Concurrent iws sessions across agent_ids; assert per-agent routing.
- Agent-tool-level integration:
  - `test_tool_dispatch_routes_iws_after_enter.py` — calls `tools/isolated_workspace/enter_isolated_workspace` tool, then `tools/sandbox/edit_file` tool, asserts result `workspace == "isolated"`.
  - `test_tool_dispatch_routes_normal_after_exit.py` — enter, exit, then edit_file; asserts `workspace == "ephemeral"`.

### E2E (live, Tier 8 soak)
- Refactor existing iws Tier 1-9 live tests to use the host-side API (Tier 1 `test_enter_then_shell_then_exit.py` drives through the agent-level tools when possible).
- Combined Tier 4-9 ≥1 cycle.

### Observability / audit
- Lifecycle tool calls emit `workspace_lifecycle_*` events to host audit bus.
- Tool-op calls emit `SandboxOperation` start/result/failed triplets via `audited_operation`. `workspace` field on result reflects daemon-resolved branch.
- Daemon-side JSONL mirror (`EOS_ISOLATED_WORKSPACE_AUDIT_PATH`) continues to receive `sandbox_isolated_workspace_{enter,exit,tool_call,evicted,gc_orphan}` events as backstop diagnostics — unchanged from today.

### Static / structural
- `test_iws_branch_isolation_invariant.py` — module-local AST scan; deny-list per Principle 5.
- Extended `test_setns_exec_discipline` pins `in_ns_runner.py` imports.
- `tests/static/test_occ_field_guard.py` (best-effort).
- Bundle test confirms `in_ns_runner.py` packaged by `runtime_bundle.py`.

---

## 10. ADR

- **Decision:** Four architectural shifts in one plan:
  0. **Workspace trichotomy.** Frame the system as three named concepts — `main_workspace` (persistent identity, OCC-only writes), `ephemeral_workspace` (per-tool-call execution context, OCC merge into main), `isolated_workspace` (lifecycle-scoped execution context, no merge, hermetic). Tool ops uniformly "execute within the active execution context" — which is daemon-resolved by agent state. Lifecycle tools (`enter/exit_isolated_workspace`) switch the active context. This naming clarifies the surface that previous revisions called "normal mode vs iws mode" — a framing that conflated `main_workspace` (persistent state) with `ephemeral_workspace` (per-call execution context).
  1. **Tool ops collapse to a single RPC namespace `api.v1.<verb>`.** Delete the 5 iws tool-op RPCs and `ops_handlers.py`. Daemon-side dispatch is implicit by `pipeline.get_handle(agent_id)` lookup.
  2. **Lifecycle ops become first-class at both layers.** Add `sandbox.api.enter_isolated_workspace` / `exit_isolated_workspace` (host-side coroutines using a NEW `LifecycleResultBase` and `lifecycle_operation` audit wrapper). Add `tools/isolated_workspace/{enter,exit}_isolated_workspace/` agent-level tool wrappers — fulfilling the original design that was never implemented.
  3. **Verb renames:** `search_content` → `grep`, `glob_files` → `glob`.
- **Drivers:** caller ergonomics at both layers; architectural simplicity via single tool-op namespace; lifecycle/tool-op separation of concerns; feature parity correctness.
- **Alternatives considered:**
  - Keep `mode` parameter in payload (v4) — rejected: lifecycle is explicit at the agent layer, so implicit daemon-side dispatch is now safe AND simpler.
  - Place lifecycle tools under `tools/sandbox/` (Option L2) — rejected: hides the lifecycle-vs-tool-op distinction.
  - Speculative `tools/lifecycle/` parent dir (Option L3) — deferred: no other lifecycle tools exist.
  - `_iws_branch.py` at `handler/_iws_branch.py` (5a) — rejected: sibling-import leak risk.
  - `_iws_branch.py` at `_shared/iws_dispatch.py` (5c) — rejected: widens `_shared/`.
  - Field-zeroing iws results — rejected: incoherent.
  - Per-verb iws Python helpers — rejected: multiplies R10 surface.
  - Free-function iws shorthand at the session layer — rejected: replaced by agent-level tools (the proper abstraction).
  - Explicit `mode` arg on tool free functions (v2 Option 2) — rejected.
  - Static callgraph fence walker — rejected: Python dynamic dispatch makes it unsound.
  - Mypy union narrowing on `XxxResult` types — deferred follow-up.
  - Transitional `removed_op` dispatcher shims — rejected: only internal caller is `_iws_rpc.py`.
- **Why chosen:** The user's direction (2026-05-24 v3, then 2026-05-24 v5) reflects an underlying design that was always intended: agent-callable lifecycle tools driving daemon-side state; tool ops dispatch by that state. With lifecycle made explicit at the agent layer, every prior objection to implicit daemon-side dispatch dissolves. Single tool-op namespace eliminates dispatcher duplication; lifecycle separation prevents the categorical mix-up between "this mutates the workspace target" and "this executes inside a workspace."
- **Consequences:**
  - PR-0: clean rename.
  - PR-A: `ops_handlers.py` deleted; 5 RPC ops deleted atomically; `daemon/handler/iws/` subpackage; lifecycle host-API; agent-level tools. CI fence test renamed and rewritten.
  - PR-B: `WorkspaceSession` sugar over the new free functions.
  - PR-C: test-suite migration; `_iws_rpc.py` shrinks to ≤30 lines.
  - `sandbox/isolated_workspace/scripts/in_ns_write.py` deleted; `setns_exec.py`, `ns_holder.py`, etc. stay (lifecycle infrastructure).
- **Follow-ups:**
  - Mypy-Union-narrowing on `XxxResult` types (replace best-effort lint).
  - Collapse iws lifecycle RPC ops into `api.v1.workspace.*` (separate plan).
  - `IsolatedWorkspaceSession` subclass for iws-only host methods.
  - Cache iws `manifest_version` per-process.

---

## 11. Out of scope (explicit)

- Daemon wire-protocol versioning beyond rename. No `api.v2.*`.
- OCC writeback for iws (intentional design feature).
- Network-policy API for iws (separate plan).
- iws lifecycle RPC ops naming change (`api.isolated_workspace.*` survives; future collapse into `api.v1.workspace.*`).
- Mypy-level union narrowing on `XxxResult`.
- Provider-level changes (Daytona, Docker).
- Speculative `tools/lifecycle/` parent directory.
