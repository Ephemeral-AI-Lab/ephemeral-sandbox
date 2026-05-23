# PROGRESS: Unify `sandbox.api.tool` for normal + isolated_workspace modes

**Companion to:** `docs/plans/unify_sandbox_tool_api_PLAN.md` (planner v9, 2026-05-24).
**Date:** 2026-05-24.

This file records what has shipped vs. what remains, and lists the implementation plan for the deferred PRs. The numbered steps in the canonical PLAN are authoritative — this file only records status and surfaces stop-the-world items found during PR-0.

---

## Status at a glance

| PR | Status | Scope |
|---|---|---|
| **PR-0 — Verb renames** | **DONE** (this session) | `search_content` → `grep`, `glob_files` → `glob`; `SearchContent{Request,Result}` → `Grep{Request,Result}`; constants + wire ops + handler module split. |
| PR-A — Daemon unification + iws tool-op deletion + lifecycle host API + agent-level tools | **NOT STARTED** | PLAN §6 PR-A (Phases 1–6). 17 numbered steps. |
| PR-B — Host-side `WorkspaceSession` | **NOT STARTED** | PLAN §6 PR-B (Phase 7). 2 numbered steps. |
| PR-C — Test suite migration | **NOT STARTED** | PLAN §6 PR-C (Phases 8–9). 7 numbered steps. |

---

## PR-0: What shipped

### Source code (renamed in `backend/src/`)

- `sandbox/_shared/models.py` — `SearchContentRequest`/`SearchContentResult` → `GrepRequest`/`GrepResult`. Field `mode` → `output_mode` on the result (frees the slot for the workspace discriminator in PR-A step 4).
- `sandbox/api/transport.py` — `DAEMON_OP_FIND_FILES`/`DAEMON_OP_SEARCH_CONTENT` → `DAEMON_OP_GLOB="api.v1.glob"` / `DAEMON_OP_GREP="api.v1.grep"`.
- `sandbox/api/timeouts.py` — `FIND_FILES_TIMEOUT_S`/`SEARCH_CONTENT_TIMEOUT_S` → `GLOB_TIMEOUT_S`/`GREP_TIMEOUT_S`.
- `sandbox/api/tool/grep.py` — function renamed `search_content` → `grep`.
- `sandbox/api/tool/glob.py` — function renamed `glob_files` → `glob`.
- `sandbox/api/tool/core/results.py` — `search_content_result_from_daemon_response` → `grep_result_from_daemon_response`; reads `raw["output_mode"]` instead of `raw["mode"]`.
- `sandbox/api/__init__.py` — re-exports `grep`, `glob`, `GrepRequest`, `GrepResult`. Old names removed.
- `sandbox/daemon/handler/search.py` — **deleted**. Split into:
  - `sandbox/daemon/handler/grep.py` — `_grep_sync`, `grep`, shared dir-walk helpers (`is_vcs_excluded`, `layer_subpath`, `under`), `DEFAULT_GREP_HEAD_LIMIT`, `MAX_GREP_CONTENT_BYTES`, `MAX_GREP_FILE_BYTES`, `VCS_EXCLUDED`. The response dict key `"mode"` → `"output_mode"`; timing keys `api.search_content.*` → `api.grep.*`.
  - `sandbox/daemon/handler/glob.py` — `_glob_sync`, `glob`, `DEFAULT_GLOB_LIMIT`. Imports the shared helpers from `grep.py`. Timing keys `api.find_files.*` → `api.glob.*`.
- `sandbox/daemon/rpc/dispatcher.py` — bootstrap import updated; old wire ops `api.find_files`, `api.v1.find_files`, `api.search_content`, `api.v1.search_content` removed; new `api.glob`, `api.v1.glob`, `api.grep`, `api.v1.grep` registered. The iws-side `api.isolated_workspace.search_content` is renamed to `api.isolated_workspace.grep` (the full iws tool-op surface is deleted in PR-A step 17).
- `sandbox/isolated_workspace/ops_handlers.py` — function `search_content` → `grep` (the iws wrapper that shells out to `/usr/bin/grep`). The whole module is slated for deletion in PR-A step 16; this rename keeps the grep deny-list clean in the meantime.
- `sandbox/isolated_workspace/__init__.py` — docstring updated to reflect the renamed op.
- `sandbox/audit/translation.py` — `SandboxOperation` Literal: `"search_content"` → `"grep"`, `"glob_files"` → `"glob"`.
- `tools/sandbox/grep/grep.py` — import + call site updated.
- `tools/sandbox/glob/glob.py` — function renamed `glob_files` → `glob`; tool-decorator name was already `"glob"`.
- `tools/sandbox/_lib/registry.py` — import + tool-list updated.
- `task_center_runner/agent/mock/complex_project_build_grep_glob_probe.py` — `glob_files as glob_tool` → `glob as glob_tool`.
- `task_center_runner/tests/mock/sandbox/isolated_workspace/_iws_rpc.py` — helper function `search_content` → `grep` and wire op string updated.
- `task_center_runner/tests/mock/sandbox/isolated_workspace/{PLAN,NEXT-AGENT-GUIDE}.md` — doc references updated.

### Tests rewritten

- `backend/tests/unit_test/test_sandbox/test_api/test_grep_glob.py` — full rewrite against new names; 7 cases pass.
- `backend/tests/unit_test/test_sandbox/test_daemon/test_search_handler.py` — full rewrite (kept the original filename to minimize churn in test-ID logs; can be renamed to `test_grep_glob_handlers.py` in a follow-up). Imports `_glob_sync` from `sandbox.daemon.handler.glob` and `_grep_sync` from `sandbox.daemon.handler.grep`; OCC-immunity guard now checks both modules; 24 cases pass.
- `backend/tests/unit_test/test_sandbox/test_daemon/test_routing_invariants.py` — handler imports + expected OP_TABLE updated; 2 cases pass.
- `backend/tests/unit_test/test_tools_sandbox/test_grep_glob.py` — full rewrite against renamed Pydantic input models and monkeypatch targets; 9 cases pass.

### Verification evidence

- `grep -rn "search_content\|SearchContent\|find_files\|glob_files" backend/src backend/tests` → **zero hits**.
- `grep -rn "DAEMON_OP_(SEARCH_CONTENT\|FIND_FILES)\|(SEARCH_CONTENT\|FIND_FILES)_TIMEOUT_S" backend` → **zero hits**.
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox/test_api/test_grep_glob.py backend/tests/unit_test/test_sandbox/test_daemon/test_search_handler.py backend/tests/unit_test/test_sandbox/test_daemon/test_routing_invariants.py backend/tests/unit_test/test_tools_sandbox/test_grep_glob.py` → **42 passed**.
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox/ backend/tests/unit_test/test_tools_sandbox/` → **786 passed, 9 failed, 3 skipped**. All 9 failures are pre-existing (confirmed by `git stash && pytest`; failure set and identities are identical with and without this PR-0 work). They are unrelated to the rename surface:
  - `test_shell_atomic_by_path_count.py` ×3: `_StubOccClient` missing `run_maintenance_after_publish`.
  - `test_runtime_invoker_cleanup.py` ×1, `test_snapshot_overlay_runner.py` ×1: same `_StubOccClient` gap.
  - `test_docker_adapter.py` ×2: pre-existing Docker provider drift.
  - `test_live_harness_provider_resolution.py` ×2: `_FakeSandboxSettings` lacks `daytona` attr.
- IWS mock tests collect cleanly: 95 tests under `backend/src/task_center_runner/tests/mock/sandbox/isolated_workspace/`. (Live-only — not executed in this session.)

### Notes / micro-deviations from the planner text

- **`SearchContentResult.mode` → `output_mode`** is implemented (PLAN step 1b). The plan also requires `grep -nE "^\s*mode:\s" backend/src/sandbox/_shared/models.py` returning zero hits inside `XxxResult` classes — verified.
- **Daemon handler split** was implemented by putting shared directory-walk helpers (`is_vcs_excluded`, `layer_subpath`, `under`) in `grep.py` and having `glob.py` import them from `grep.py`. PLAN doesn't constrain this layout. PR-A step 9 extracts both into `_shared/tool_primitives/{compute_grep,compute_glob}.py`, so the temporary cross-handler import will dissolve naturally.
- **Test filename `test_search_handler.py`** was kept rather than renamed to `test_grep_glob_handlers.py`. The plan doesn't mandate test-file naming. A follow-up `git mv` is a one-liner; deferred to avoid review churn.

---

## Deferred-items implementation plan

The canonical PLAN sequences PR-A → PR-B → PR-C; this section indexes the next-session work without re-deriving content already in the PLAN.

### PR-A: Daemon unification + iws tool-op deletion + lifecycle host API + agent-level tools

**Read first:** PLAN §6 PR-A (Phases 1–6, steps 4–19). PLAN §2 (Principles 1–9). PLAN §5 (Pre-mortem scenarios A–E).

**Phase 1 — Result types + failing tests (PLAN steps 4–7):**
1. Add `workspace: Literal["ephemeral", "isolated"] = "ephemeral"` to `SandboxResultBase` in `sandbox/_shared/models.py`.
2. Add `LifecycleResultBase` and `LifecycleError` dataclasses to `sandbox/_shared/models.py`.
3. Add `EnterIsolatedWorkspaceRequest`/`Result` and `ExitIsolatedWorkspaceRequest`/`Result` dataclasses.
4. Write per-verb golden contract tests at `backend/src/task_center_runner/tests/mock/sandbox/api_parity/test_<verb>.py` (initially `xfail`).

**Phase 2 — Compute extraction + parity corpus (PLAN steps 8–10):**
5. Capture parity-corpus fixture at `backend/src/task_center_runner/tests/mock/sandbox/_fixtures/tool_primitives_parity_corpus.json` via `backend/scripts/gen_tool_primitives_parity_corpus.py`. ≥20 cases per verb; ≥10 per `_apply_edits`/`_atomic_overwrite_no_follow`/`walk_upperdir`.
6. Create `sandbox/_shared/tool_primitives/` package: `compute_grep.py`, `compute_glob.py`, `compute_read.py`, `file_ops.py` (`atomic_overwrite_no_follow`, `apply_edits`), `capture.py` (re-export `walk_upperdir`), `mount.py` (re-export `mount_overlay`). Repoint `write.py`/`edit.py`/`SandboxOverlay` to import from this package.
7. Add `in_ns_runner.py` — thin dispatcher for `--op {read|write|edit|grep|glob}` composing only `tool_primitives.*`. No `shell_capture` op.

**Phase 3 — Unified daemon handlers with implicit mode dispatch (PLAN steps 11–12):**
8. Create `sandbox/daemon/handler/iws/` subpackage with `_branch.py` containing `run_in_isolated(args, op=...)`. Host-side `walk_upperdir(handle.upperdir)` capture for shell ops. Empty `__init__.py` with the OCC-free constraint docstring.
9. Add `get_active_manager()` (nil-safe accessor) to `sandbox/isolated_workspace/manager.py`. Patch each of the 6 daemon handlers (`shell`, `read`, `write`, `edit`, `grep`, `glob`) to dispatch to `_branch.run_in_isolated` when the caller has an open iws handle.
10. Write `test_get_handle_returns_none_during_wire_and_teardown.py` (Scenario E pin) against the REAL `IsolatedWorkspaceManager`.

**Phase 4 — Host-side lifecycle API (PLAN steps 13–14):**
11. Create `sandbox/api/lifecycle/{enter_isolated,exit_isolated}.py` parallel to `sandbox/api/tool/`.
12. Add `sandbox/audit/lifecycle.py` with `lifecycle_operation(...)` wrapper. Add `WorkspaceLifecycle` Literal + 3 translator fns to `sandbox/audit/translation.py`. Add `WORKSPACE_LIFECYCLE_{STARTED,COMPLETED,FAILED}` constants to `sandbox/audit/events.py`.
13. Update `audited_operation` at `sandbox/api/tool/core/audit.py` to accept `workspace_hint: Literal["ephemeral","isolated","unknown"]` and stamp the `workspace` field on `sandbox_operation_failed` payloads.
14. Re-export `enter_isolated_workspace`, `exit_isolated_workspace`, `EnterIsolatedWorkspaceRequest`, `EnterIsolatedWorkspaceResult`, `ExitIsolatedWorkspaceRequest`, `ExitIsolatedWorkspaceResult`, `LifecycleError`, `LifecycleResultBase` from `sandbox/api/__init__.py`.

**Phase 5 — Agent-level tool wrappers (PLAN step 15):**
15. Create `backend/src/tools/isolated_workspace/{enter,exit}_isolated_workspace/` with `@tool`-decorated wrappers. Reuse helpers from `tools.sandbox._lib.session`. Write unit tests + the destructive-shell pre-hook policy test (`test_destructive_pre_hook_fires_in_iws_mode.py`).

**Phase 6 — Delete the iws tool-op surface (PLAN steps 16–19):**
16. Delete `sandbox/isolated_workspace/ops_handlers.py` and `sandbox/isolated_workspace/scripts/in_ns_write.py`.
17. Remove the 5 `api.isolated_workspace.{shell,read_file,write_file,edit_file,grep}` registrations from `dispatcher.py` (atomic; PR-C migrates the only remaining caller `_iws_rpc.py`).
18. Add the dispatcher-entry plugin block for iws-active callers (Principle 9). Write `test_plugin_blocked_in_isolated_workspace.py` and `test_plugin_allowed_when_no_iws_open.py`.
19. Rename `test_isolated_workspace_ops_import_fence` → `test_iws_branch_isolation_invariant.py` with the new deny-list targets.

### PR-B: Host-side `WorkspaceSession` (PLAN steps 20–21)

20. Create `sandbox/api/workspace.py` with `WorkspaceMode` enum and `WorkspaceSession` async-CM. `attach` / `enter_isolated` factory; tool methods delegate to the renamed free functions; `__aexit__` calls `sandbox.api.exit_isolated_workspace` for iws sessions.
21. Repoint the six `sandbox/api/tool/*.py` free functions to delegate to `WorkspaceSession.attach(...).<verb>(...)` while preserving their existing signatures (back-compat for current callers).

### PR-C: Test suite migration (PLAN steps 22–25)

22. Migrate `_iws_rpc.py` callers off the deleted tool-op RPC ops. Shrink the helper to ≤30 lines (only `test_reset`, `status`, `list_open` remain as raw RPC).
23. Update `tests/mock/sandbox/isolated_workspace/happy_path/test_enter_then_shell_then_exit.py` to drive the lifecycle through the new agent-level tools; assert the new host-side audit sequence.
24. Add Tier 1 tests under `tests/mock/sandbox/isolated_workspace/tool_wrappers/`: `test_enter_isolated_workspace_tool.py`, `test_exit_isolated_workspace_tool.py`, `test_tool_dispatch_routes_iws_after_enter.py`, `test_tool_dispatch_routes_normal_after_exit.py`.
25. Update `tests/mock/sandbox/isolated_workspace/PLAN.md` to reflect the new `tool_wrappers/` tier and `_iws_rpc.py` deprecation.

### Documentation (PLAN steps 26–28; can be amortized into PR-A and PR-C)

- Update `docs/isolated_workspace_runtime_source_blast_radius.md` with the new module set.
- Add `docs/sandbox/api_surface.md`.
- CHANGELOG entry per PR.

---

## Pre-existing test failures discovered during verification (NOT in scope for this PR-0)

Surfaced here because they will block PR-A's CI gate if not addressed first. All confirmed pre-existing by running `git stash && pytest` — failure set and identities are identical with and without this PR-0 work.

Full `.venv/bin/pytest backend/tests/unit_test/`: **1911 passed, 19 failed, 4 skipped** (same before and after PR-0).

Sandbox-side (9 failures):
1. `test_sandbox/test_api/test_shell_atomic_by_path_count.py` (3): `_StubOccClient` test double missing `run_maintenance_after_publish` method.
2. `test_sandbox/test_overlay/test_runtime_invoker_cleanup.py` (1) and `test_sandbox/test_overlay/test_snapshot_overlay_runner.py` (1): same `_StubOccClient` gap.
3. `test_sandbox/test_provider/test_docker_adapter.py` (2): Docker provider mock drift.
4. `test_sandbox/test_provider/test_live_harness_provider_resolution.py` (2): `_FakeSandboxSettings` missing `daytona` attribute.

Live-e2e-tooling-side (10 failures):
5. `test_live_e2e_tools/test_run_tiered.py::test_load_real_tiers_toml_parses` (1): unrelated TOML-parsing regression.
6. `test_live_e2e_tools/test_tier0_health.py` (9): Tier 0 health-check assertions; `Tier0Result(passed=False, ..., notes="...image_inspect=missing_live_image_default")` — missing local Docker image for the live harness.

Recommendation: triage each before PR-A's parity-corpus capture (PLAN step 8) lands, since the parity-corpus generator depends on a green sandbox-test baseline to be meaningful. The live-e2e-tools failures look orthogonal to the unify-API work — they may be triable independently.

---

## Bonus: stale doc cleanup (touched in this session)

- `backend/docs/sandbox-architecture.html` — three references updated: tool-verb list (`glob_files`/`search_content` → `glob`/`grep`), RPC op rows (`api.v1.find_files`/`api.v1.search_content` → `api.v1.glob`/`api.v1.grep`), and file-ref pointers (`daemon/handler/search.py` → `daemon/handler/{grep,glob}.py`). Not test-gated; flagged by the advisor pass.

---

## Out-of-scope items confirmed unchanged from PLAN §11

- Daemon wire-protocol versioning beyond rename.
- OCC writeback for iws (intentional design feature).
- Network-policy API for iws.
- iws lifecycle RPC ops naming change (`api.isolated_workspace.{enter,exit,status,list_open,test_reset}` survives).
- Mypy-level union narrowing on `XxxResult`.
- Provider-level changes.
- Speculative `tools/lifecycle/` parent directory.
