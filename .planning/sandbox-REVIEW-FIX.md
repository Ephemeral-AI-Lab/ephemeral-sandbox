# `backend/src/sandbox` — Code Review Fix Report (Phase 8 — Naming Renames)

**Date:** 2026-05-16
**Branch:** `codex/fix-dot-path-normalization-tests`
**Range:** commits `ef0c3aa9..f1e908f6` (15 commits)
**Source review:** `.planning/sandbox-REVIEW.md` §5 (cross-cutting rename map)
**Prior phases:** see `.planning/sandbox-REVIEW-DEFERRED-IMPLEMENTATION.md` for Phases 2 through 9.4 (already complete)

This pass covers the deferred Phase 8 naming work — the cross-cutting rename map that prior sessions stopped before because parallel TaskCenter rename activity made it unsafe. Scope was restricted to **sandbox-internal renames only**; no `task_center/`, `task_center_runner/`, or `tools/` files were touched.

---

## 1. Naming changes summary

14 renames across 5 subsystems, plus one consolidation commit picking up the prior session's uncommitted Phase 9.2/9.3/9.4 work.

### `layer_stack/` (6 renames)

| Old | New | Why |
|---|---|---|
| `_storage_lock.py` | `storage_lock.py` | The leading `_` lied about visibility — `manager.py` imported it across the package boundary. |
| `_paths.py` | `paths.py` | Same — 8 sibling modules plus `overlay_capture` import it. |
| `layer_change.py` | `changes.py` | The `layer_` prefix is redundant inside `layer_stack/`. |
| `layer_publisher.py` | `publisher.py` | Same. |
| `maintenance.py` | `squash.py` | The module only exports `SquashService` and squash helpers. "Maintenance" overpromised. |
| `manager.py::LayerStackManager` | `stack.py::LayerStack` | "Manager" was meaningless noise. The stack *is* the object. Headline rename — 69 files updated. |

### `execution/` (4 renames)

| Old | New | Why |
|---|---|---|
| `entrypoints.py` | `namespace_child.py` | 340-LOC private-namespace child-process helper. Plural was misleading. |
| `policy.py` | `env_policy.py` | "Policy" of what was ambiguous; the file owns env + path-character allowlists. |
| `overlay_change.py` | `path_change.py` | Inside `execution/`, `overlay_` collided with 7 sibling `overlay_*.py` modules. `PathChange` matches the type. |
| `workspace_environment.py` | `subprocess_runner.py` | The module is the subprocess wrapper used by all execution strategies. |

### `provider/daytona/` (2 renames)

| Old | New | Why |
|---|---|---|
| `context.py` | `exec_context.py` | Disambiguates from the 4 other `context.py` files in sandbox. Matches the file's tool-exec-context responsibility. |
| `bash.py` | `exec_wrapper.py` | Named after the shell; actual job is wrap-command + exit-marker-parse, provider-agnostic. |

### `occ/` (2 renames)

| Old | New | Why |
|---|---|---|
| `ports.py` | `protocols.py` | Hexagonal-architecture jargon → Python idiom (`typing.Protocol`). |
| `router.py::Router` | `preparer.py::ChangesetPreparer` | The class prepares changesets; it does not route. File name now matches the public verbs (`prepare_*`). |

---

## 2. Deleted / reduced redundant code

This pass was a rename-only pass; no deletions or LOC reductions. Behavior-preserving by design.

Net LOC delta (excluding `git mv` rename metadata): **+0 / -0**. All rewrites were 1:1 import-path or symbol substitutions.

Prior phases already executed the deletion-heavy work:
- C2, S1–S10, 7.5–7.11 deletions tracked in `sandbox-REVIEW-DEFERRED-IMPLEMENTATION.md`
- ~3,000 LOC + 8–12 files already removed across the cleanup arc

---

## 3. Public compatibility paths preserved

| Surface | Status |
|---|---|
| `sandbox.layer_stack.LayerStack` (was `LayerStackManager`) | New name. No alias kept — class is sandbox-internal, used only by daemon, OCC, plugin/projection, and tests, all of which were updated. |
| `sandbox.occ.ChangesetPreparer` (was `Router`) | New name re-exported through `occ/__init__.py`. No alias. All internal callers updated. |
| `sandbox.plugin.runtime` (separate, prior phase) | **DeprecationWarning shim retained** from Phase 5 (S6). Plugin authors importing `sandbox.plugin.runtime.{context,registry}` still work; the LSP plugin keeps building. |
| Bundle layout assertions (`test_bundle_upload.py`, `test_overlay_dependency_boundaries.py`, `test_occ_dependency_boundaries.py`) | Updated to require the new filenames. The bundle scans directories wholesale, so no runtime code in `runtime_bundle.py` needed changing. |

No new compatibility shims were introduced. The rename map in REVIEW.md §5 explicitly asked for clean renames inside sandbox; none of the renamed symbols were part of a documented public surface contract.

---

## 4. Exact tests and checks run

After every individual rename commit:
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox -q` — narrowest relevant suite

Final verification (after all 14 renames committed):
- `.venv/bin/pytest backend/tests/unit_test/test_sandbox -q` — **547 passed, 1 skipped, 1 expected deprecation warning** (the `sandbox.plugin.runtime` shim from Phase 5)
- `.venv/bin/ruff check backend/src/sandbox backend/tests/unit_test/test_sandbox` — clean except for **2 pre-existing F401 unused-import warnings** in `test_manifest.py` (lines 10 and 12 — `DeleteLayerChange`, `SymlinkLayerChange`). These predate this pass; left alone per project CLAUDE.md ("If you notice unrelated dead code, mention it - don't delete it").

Targeted grep checks after each rename to confirm no stale references survived. Two non-import patterns were caught and fixed after initial sweeps:
- `from sandbox.provider.daytona import bash as bash_mod` (submodule-style import) → updated to `import exec_wrapper as bash_mod` in `test_daytona_bash_exit_code.py`.
- `occ_root / "router.py"` (`Path` construction in `test_occ_dependency_boundaries.py`) → updated to `"preparer.py"`.

---

## 5. Commits in this pass

```
f1e908f6 sandbox: rename occ/router.py::Router to occ/preparer.py::ChangesetPreparer
304294ca sandbox: rename occ/ports.py to protocols.py
4c55d602 sandbox: rename provider/daytona/bash.py to exec_wrapper.py
e7d54e5b sandbox: rename provider/daytona/context.py to exec_context.py
59690fde sandbox: rename execution/workspace_environment.py to subprocess_runner.py
0d7b059e sandbox: rename execution/overlay_change.py to path_change.py
958ef81c sandbox: rename execution/policy.py to env_policy.py
d68e3a83 sandbox: rename execution/entrypoints.py to namespace_child.py
fd253d26 sandbox: rename LayerStackManager to LayerStack, manager.py to stack.py
20217a52 sandbox: rename layer_stack/maintenance.py to squash.py
bf69b50e sandbox: rename layer_stack/layer_publisher.py to publisher.py
9a6c012d sandbox: rename layer_stack/layer_change.py to changes.py
d7020b4c sandbox: rename layer_stack/_paths.py to paths.py
7ca8c0ab sandbox: rename layer_stack/_storage_lock.py to storage_lock.py
ef0c3aa9 sandbox: consolidate plugin state, dedupe daytona config, unify rpc error envelope
```

15 commits, each atomic, each with green sandbox tests at HEAD.

---

## 6. Remaining work intentionally not done

The full REVIEW.md §5 rename map includes additional renames that this pass left alone:

| Item | Why deferred |
|---|---|
| `api/_impl/{_audit,_classifiers,_payload,_results}.py` underscore removal + responsibility renames | Touches the API import-boundary contract (Phase 7.1 noted `sandbox.api.__init__` cannot import from sibling packages). Needs an API boundary decision first. |
| `api/_control.py` → `_lifecycle.py` (or fold into `__init__.py`) | Same boundary concern; the file already received content fixes in Phase 7.1. |
| `daemon/handler/request_context.py` → split into 4 files | Already partially addressed by Phase 6 (Option B: promoted to `daemon/_toolbox.py`). The further 4-way split is a structural change, not a rename. |
| `daemon/async_bridge.py` → `sandbox/io_loop.py` (move) | Cross-package move, not a rename — requires evaluating the io-loop boundary. |
| `host/bootstrap.py::setup_after_*` → `bootstrap_sandbox` (function unification) | Behavior unification, not a rename. Deferred in Phase 7.16. |
| `host/daemon_client.py::_DaemonDispatchError`, `_DaemonReadinessError` → `DaemonError` (one class) | Class-merge, not a rename. Deferred in Phase 7.16. |
| `plugin/handler.py` → `plugin/daemon_handler.py` (or move under `daemon/handler/plugin.py`) | Move + import-chain restructure; needs the plugin-authoring contract to confirm the public surface. |
| `plugin/session.py` → `plugin/host_call.py` | Sandbox-internal but touches the plugin-author docs; review the plugins-refactor doc first. |
| `plugin/projection.py::WorkspaceProjection` → `PluginWorkspaceView` | Same — sandbox-internal but referenced in plugins docs. |
| Pre-existing `F401` unused imports in `test_manifest.py:10,12` | Pre-existing dead code per CLAUDE.md "mention, don't delete." Pointed out here for a future cleanup pass. |

---

**Status:** Phase 8 sandbox-internal naming pass complete. 547 sandbox tests green at HEAD `f1e908f6`. No `task_center/` files touched. No compatibility shims introduced beyond the pre-existing `sandbox.plugin.runtime` deprecation re-export from Phase 5.
