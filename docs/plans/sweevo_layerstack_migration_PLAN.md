# sweevo benchmark migration + layer_stack refactor

**Status:** Approved (ralplan consensus iter 3 — Architect APPROVE, Critic APPROVE on v8; user accepted v10 with `rebuild_base` dropped and POST stage removed)
**Owner:** TBD
**Scope:** `backend/src/benchmarks/sweevo/`, `backend/src/task_center_runner/benchmarks/`, `backend/src/sandbox/layer_stack/stack.py`, `backend/src/sandbox/daemon/{builtin_operations.py,rpc/dispatcher.py,layer_stack_runtime.py}`, `backend/src/task_center_runner/environments/sweevo_image/fixtures.py`, tests at `backend/tests/unit_test/test_benchmarks/`, `backend/tests/integration_test/test_benchmarks/`.

---

## Goal

Migrate `backend/src/benchmarks/` to `backend/src/task_center_runner/benchmarks/sweevo/`. Refactor sweevo to consume `sandbox.layer_stack` primitives through a new `api.commit_to_workspace` daemon RPC instead of its own 170-line embedded materializer. Drop Daytona code paths from the sweevo benchmark (the provider system itself stays multi-provider). Reshape the package as **3 stage files + 1 pipeline file + 4 helper modules** with the workflow visible top-to-bottom in `pipeline.py`. Make sandboxes persistent per `instance_id` (no teardown).

Net change: ~600 LoC removed (from ~1800 to ~1200 in the sweevo package).

---

## ADR

### Decision
Sequenced migration + refactor (Option B). Reuse `LayerStack.commit_to_workspace` via a new daemon RPC. Drop Daytona-specific code paths from sweevo (provider system unchanged). Reshape into 3 stage files + pipeline file + helper modules. Sandboxes persist per `instance_id` with deterministic naming.

### Drivers
1. **Reuse over re-invent.** `LayerStack.commit_to_workspace` (`stack.py:299`) already does what `apply_layerstack_to_repo` open-codes via a 115-line embedded Python script.
2. **Daytona-removal in sweevo benchmark fixes a latent docker bug** — sweevo predicates check `state` key against Daytona vocabulary while `DockerProviderAdapter._serialize_container` (`adapter.py:50`) emits `status` key with Docker vocabulary. In docker mode, prune never matches and reuse over-includes (`""` is in the allow-set).
3. **Workflow visible in one file** — `pipeline.py::run_benchmark_sweevo` is the canonical workflow reference.

### Alternatives considered
- **Big-bang single PR** — rejected; large diff loses merge games against parallel-agent worktree activity.
- **Migration only, defer refactor** — rejected; user explicitly asked to consume `sandbox/layer_stack` primitives.
- **Provider-level Daytona deletion** — rejected; the provider system is shared infra used by `live_e2e_test` and other consumers; out of authorized scope.
- **Stage-per-file with no helpers** (v4 strict, `pre.py` at ~850 LoC) — rejected; user's "split pre.py" feedback.
- **Stage-per-file with 5 helpers including `_data.py`** (v6/v7) — rejected on principle (CLAUDE.md §2 "no abstractions for single-use code"); `_data.py` has only `setup.py` as caller, so loaders fold into `setup.py`.

### Why chosen
v10 honors three user constraints in one design:
1. Consume `sandbox/layer_stack` (the new daemon RPC).
2. Migrate to `task_center_runner/benchmarks/`.
3. Sandbox setup is docker-only; sandbox is unique per `instance_id` and persists across runs.

Each phase ships independently, compiles, and passes tests in isolation. Linear import graph (no cycles). Provider system unchanged.

### Consequences
- Adapter dict-shape divergence (`state` vs `status` key + different vocabularies) becomes visible debt; captured in `FOLLOWUP_provider_state_canonical.md`.
- `_enforce_global_sandbox_quota` is deleted (one container per instance is naturally bounded; disk hygiene is the user's responsibility via `docker rm`).
- sweevo package shrinks from ~1800 to ~1200 LoC.
- `fixtures.py` needs a one-line update: drop `reuse_existing_auto=_reuse_existing_auto_enabled()` (always reuse-by-name) and remove the unused `_reuse_existing_auto_enabled` helper.

### Follow-ups (out of scope)
1. Normalize provider adapter dict shape (`state`/`status` key + vocabulary) at the protocol boundary. Separate ADR.
2. Promote `_exec` / `_wait_for_sandbox_exec_ready` to `sandbox.host` if a second consumer appears.
3. Disk-cleanup CLI flag (`--gc` subcommand) if power users complain about manual `docker rm`.

---

## Verified facts (primary-source)

| Fact | Source |
|---|---|
| Provider system supports both docker and daytona | `sandbox/provider/bootstrap.py:21` `_VALID_PROVIDERS = frozenset({"docker", "daytona"})` |
| Docker adapter emits key `"status"`, not `"state"` | `sandbox/provider/docker/adapter.py:50` |
| Daytona adapter emits key `"state"` | `sandbox/provider/daytona/adapter.py:80,175` |
| `_replace_workspace_contents` uses bare `os.replace` (no EXDEV handling) | `sandbox/layer_stack/stack.py:395-401` |
| `LayerStack.commit_to_workspace` exists and projects+swaps+rebuilds | `sandbox/layer_stack/stack.py:299-361` |
| sweevo prune predicate at `sandbox.py:237-239` checks `state` key, Daytona vocab | direct read |
| sweevo reuse predicate at `sandbox.py:269-271` includes `""` in allow-set → over-reuses in docker | direct read |
| `_register_sweevo_snapshot_daytona` at lines 540-568 | direct read |
| `verify_sweevo_snapshot_exists` enum normalization at lines 641-645 | direct read |
| `fixtures.py:114` calls `reset_sweevo_workspace` (live, not dead) | grep |
| `fixtures.py:95-96` passes `register_snapshot=True, reuse_existing_auto=...` (live) | grep |
| `provision_sweevo_sandbox` at lines 475-502 has no callers | grep |
| `run_sweevo_required_test` (1311-1419) + `prepare_sweevo_test_run` (1422-1468) have no external callers | grep |

---

## Final file structure

```
backend/src/task_center_runner/benchmarks/sweevo/
|
├── __init__.py          # ~15 LoC — public re-exports + disk-cleanup doc note
├── __main__.py          # ~20 LoC — argparse + asyncio.run
├── pipeline.py          # ~15 LoC — wires 3 stages (no try/finally)
|
├── setup.py             # STAGE 1 — preflight + provision_sandbox      ~280 LoC
├── run.py               # STAGE 2 — RunConfig + SweevoProvisioner       ~80 LoC
├── eval.py              # STAGE 3 — Lifecycle + commit + score + verdict ~240 LoC
|
├── models.py            # SWEEvoInstance, SWEEvoResult, PreContext, constants  ~250 LoC
├── _snapshot.py         # docker-only register + verify                  ~80 LoC
├── _provision.py        # docker-only create + resume + setup           ~120 LoC
├── _exec.py             # _exec, _wait_for_exec_ready                    ~80 LoC
|
└── FOLLOWUP_provider_state_canonical.md
```

**10 files. ~1180 LoC total** (down from ~1800).

### Files dissolved into the new layout

| Today | Fate |
|---|---|
| `benchmarks/sweevo/models.py` | folded into `models.py` (renamed in place during Phase 4) |
| `benchmarks/sweevo/dataset.py` | folded into `setup.py` |
| `benchmarks/sweevo/prompt.py` | folded into `setup.py` |
| `benchmarks/sweevo/sandbox.py` | justified subset split across `_snapshot.py` / `_provision.py` / `_exec.py`; unjustified parts DELETED |
| `benchmarks/sweevo/evaluation.py` | folded into `eval.py` |
| `benchmarks/sweevo/__main__.py` | becomes `__main__.py` + `pipeline.py` + parts of `setup.py` |
| `task_center_runner/benchmarks/sweevo/lifecycle.py` | folded into `eval.py` |
| `task_center_runner/benchmarks/sweevo/provisioner.py` | folded into `run.py` |
| `task_center_runner/benchmarks/sweevo/agent_runner.py` | collapsed (triple-factory → one function) into `run.py` |

### `pipeline.py` (the entire workflow)

```python
"""Wire the 3 sweevo workflow stages. Pure orchestration."""

from .setup import preflight, provision_sandbox
from .run import build_run_config
from .eval import format_verdict
from task_center_runner.core.engine import run_pipeline


async def run_benchmark_sweevo(args) -> int:
    ctx = await preflight(args)
    sandbox_id = await provision_sandbox(ctx)
    config = build_run_config(ctx, sandbox_id)
    report = await run_pipeline(config)
    line, rc = format_verdict(report)
    print(line)
    return rc
```

No `try`/`finally`. No teardown. The sandbox container persists; the user reclaims disk with `docker container prune` or:

```
docker ps -a --filter 'name=sweevo-' --format '{{.Names}}' | xargs -r docker rm -f
```

### Import graph (linear, acyclic)

```
__main__.py  ──► pipeline.py
                   │
                   ├──► setup.py    ──► _snapshot.py ──► models.py
                   │                ──► _provision.py ──► _exec.py
                   │                                  ──► _snapshot.py
                   │                                  ──► models.py
                   │                ──► models.py
                   │
                   ├──► run.py      ──► setup.py (PreContext type)
                   │                ──► _provision.py (verify-only provisioner)
                   │
                   └──► eval.py     ──► models.py
                                    ──► _exec.py
                                    ──► sandbox.api (api.commit_to_workspace RPC)
```

---

## Phase sequence

```
0   Preflight grep audit
1a  EXDEV fallback in _replace_workspace_contents (stack.py:395-401)
1b  api.commit_to_workspace daemon RPC (no rebuild_base kwarg)
3a  Daytona removal in sweevo (delete-list of 11 items below)
    + docker state-vocabulary fix
    + FOLLOWUP_provider_state_canonical.md
2   Materializer → RPC wrapper (~10 LoC); delete embedded script + b64 helpers
    + delete confirmed-dead provision_sweevo_sandbox + run_sweevo_required_test
      + prepare_sweevo_test_run
3b  Move materialize call → SweevoLifecycle.after_run with .git assert
4a  git mv source + tests (single commit)
4b  Rewrite imports + literals + logger + entry point + doc paths
5a  Apply persistent-sandbox redesign:
       - Replace _default_sweevo_sandbox_name (random) with deterministic
         f"sweevo-{instance_id}"
       - DELETE _prune_auto_sweevo_sandboxes_for_fresh_run,
         _find_reusable_auto_sweevo_sandbox, _safe_list_sandboxes,
         _enforce_global_sandbox_quota, _global_sandbox_quota,
         _cleanup_failed_sandbox
       - Inline _log_sandbox_creation_failure → 1-line logger.warning
       - DELETE reuse_existing_auto parameter; always reuse-by-name
       - DELETE _kill_other_sweevo_processes (docker name uniqueness suffices)
       - Update fixtures.py: drop reuse_existing_auto + _reuse_existing_auto_enabled
5b  Refactor into final file structure (10 files; setup.py / run.py / eval.py
    + pipeline.py + helpers)
5c  Delete old evaluation.py / lifecycle.py / provisioner.py / agent_runner.py
    / sandbox.py
```

**Why this order:**
- Phase 1a/1b add new code in `sandbox.layer_stack` and the daemon RPC surface; reversible by removing the new dispatcher entry.
- Phase 3a runs BEFORE Phase 2 so Phase 2's file split doesn't route soon-dead Daytona code into new files.
- Phase 2 swaps the materializer to the new RPC.
- Phase 3b relocates the materialize call from `evaluation.py` to `lifecycle.py`.
- Phase 4 is the wide-blast-radius `git mv`; ships in two commits (file move, import rewrite) to keep bisection sharp.
- Phase 5 reorganizes into the final file layout and applies the persistent-sandbox redesign last so prior phases didn't need to know about the new naming.

---

## Phase 0 — Preflight grep audit

Run all of the following. Each one informs a later phase.

```bash
# 1. Import statements
grep -rn "from benchmarks\|import benchmarks" backend/ | grep -v __pycache__

# 2. String literals (catches __main__.py:87, prompt.py:27, logger names)
grep -rEn '"benchmarks\.|'\''benchmarks\.|\bbenchmarks\.sweevo\b' backend/

# 3. Mock targets that vanish in Phase 2
grep -rEn 'mock\.patch.*benchmarks|patch\(.*benchmarks' backend/tests/

# 4. Confirm orphan helpers (should match nothing after Phase 2)
grep -rn "_materialize_layerstack_command\|_upload_file_with_fallback\|_write_file_via_chunked_base64_exec" backend/

# 5. Daytona literals slated for removal (Phase 3a)
grep -rin "daytona" backend/src/benchmarks/ backend/src/task_center_runner/benchmarks/ | grep -v __pycache__

# 6. mock.patch.*daytona to confirm tests aren't pinning to deleted symbols
grep -rEn 'mock\.patch.*daytona|patch\(.*daytona' backend/tests/
```

Output of (1), (2), (3) feeds Phase 4b's rewrite. Output of (5) is the work list for Phase 3a's comment refresh.

---

## Phase 1a — EXDEV fallback in `_replace_workspace_contents`

**File:** `backend/src/sandbox/layer_stack/stack.py:395-401`

**Current:**
```python
def _replace_workspace_contents(destination: Path, source: Path) -> None:
    """Atomically swap *destination*'s children for *source*'s children."""
    destination.mkdir(parents=True, exist_ok=True)
    for child in destination.iterdir():
        remove_path(child)
    for child in source.iterdir():
        os.replace(child, destination / child.name)
```

**Patch:** wrap `os.replace` with EXDEV fallback to `shutil.move`, mirroring the existing handler in `benchmarks/sweevo/sandbox.py:933-953`:

```python
def _replace_workspace_contents(destination: Path, source: Path) -> None:
    """Atomically swap *destination*'s children for *source*'s children.

    Falls back to shutil.move on EXDEV; docker bind-mounts /testbed as a
    separate volume so a kernel rename across the device boundary fails.
    """
    import errno, shutil
    destination.mkdir(parents=True, exist_ok=True)
    for child in destination.iterdir():
        remove_path(child)
    for child in source.iterdir():
        target = destination / child.name
        try:
            os.replace(child, target)
        except OSError as exc:
            if exc.errno != errno.EXDEV:
                raise
            shutil.move(str(child), str(target))
```

**Test:** `backend/tests/unit_test/test_sandbox/test_layer_stack/test_replace_workspace_contents_exdev.py` — mocks `os.replace` to raise `OSError(errno.EXDEV, "Invalid cross-device link")` and asserts `shutil.move` ran for every child.

---

## Phase 1b — `api.commit_to_workspace` daemon RPC

**Files:**
- `backend/src/sandbox/daemon/layer_stack_runtime.py` — add passthrough method
- `backend/src/sandbox/daemon/builtin_operations.py` — add `commit_to_workspace(args)` op
- `backend/src/sandbox/daemon/rpc/dispatcher.py:427-429` — add `"api.commit_to_workspace": builtin_operations.commit_to_workspace`

**Signature:**
```python
async def commit_to_workspace(args: dict[str, object]) -> dict[str, object]:
    """Project the active overlay onto the bound workspace_root.

    Privileged tear-down sync op (no permission model exists — see
    api.acquire_snapshot precedent). Refuses to run while any snapshot
    lease is active.
    """
    workspace_root = require_nonempty_string_arg(args, "workspace_root")
    timings: dict[str, float] = {}
    new_manifest = layer_stack_runtime.commit_to_workspace(
        workspace_root=workspace_root,
        timings=timings,
    )
    return {
        "success": True,
        "manifest_version": new_manifest.version,
        "timings": timings,
    }
```

**No `rebuild_base` kwarg** — accept the default rebuild behavior. The cost (~one extra repo re-scan per run cycle in the persistent-sandbox model) is traded for code simplicity.

**Test:** `backend/tests/unit_test/test_sandbox/test_daemon/test_commit_to_workspace_op.py` — asserts dispatcher routes the RPC and `layer_stack_runtime.commit_to_workspace` is called with `workspace_root` from args.

---

## Phase 3a — Daytona removal in sweevo (BEFORE Phase 2)

**File:** `backend/src/benchmarks/sweevo/sandbox.py` (current path; Phase 4 moves it later)

### Symbol-by-symbol delete/rewrite list

| # | Symbol | Lines | Action |
|---|---|---|---|
| 1 | `_register_sweevo_snapshot_daytona` | 540-568 | DELETE |
| 2 | Daytona branch in `register_sweevo_snapshot` | 528-531 | DELETE, inline docker case |
| 3 | Daytona SDK enum normalization in `verify_sweevo_snapshot_exists` | 641-645 | DELETE; simplify to plain `match.get("state")` check or drop entirely (docker images don't have an active/inactive state) |
| 4 | `EOS_SANDBOX_PROVIDER == "docker"` gate around `/eos-mount-scratch` check in `setup_sweevo_sandbox` | 679 | DELETE gate, always check |
| 5 | Retry loop body in `_safe_list_sandboxes` | 122-139 | SIMPLIFY to direct `service.list_sandboxes()` (docker SDK is local; no transient resets) |
| 6 | `_prune_auto_sweevo_sandboxes_for_fresh_run` predicate at line 237-238 | (rewritten) | `status = sandbox.get("status") or ""; if status.lower() in {"exited", "dead", "created"}: ...` |
| 7 | `_find_reusable_auto_sweevo_sandbox` predicates at lines 269-270, 292 | (rewritten) | `status in {"running", "exited"}`; sort key `status != "running"` |
| 8 | `_configure_reusable_sweevo_sandbox` state check at line 111 | (rewritten) | `if (existing.get("status") or "").lower() == "running": return service.get_sandbox(sandbox_id)` |
| 9 | `create_sweevo_test_sandbox` state checks at lines 1074, 1076, 1092, 1116, 1134, 1160 | (rewritten) | migrate `state` → `status`, docker vocabulary throughout |
| 10 | `_cleanup_failed_sandbox` predicate at lines 337-338 | (rewritten) | `status in {"exited", "dead"}` |
| 11 | 12 stale "Daytona ..." comments / docstrings (per Phase 0 grep #5) | scattered | UPDATE wording |
| 12 | File `FOLLOWUP_provider_state_canonical.md` | n/a | CREATE alongside `__init__.py` |

### `FOLLOWUP_provider_state_canonical.md` contents

```
# Follow-up: provider adapter dict-shape divergence

The ProviderAdapter protocol claims a "canonical dict" shape but lets
implementations diverge on the state key and vocabulary:

| adapter | key      | vocabulary                                        |
|---------|----------|---------------------------------------------------|
| Daytona | "state"  | started / stopped / pending_build / build_failed / error |
| Docker  | "status" | created / running / paused / restarting / removing / exited / dead |

Sweevo benchmark patches around this locally as of <commit-hash>. The
correct long-term fix is normalizing both adapters at the protocol
boundary — pick one canonical key (recommend `state`) and one vocabulary,
then translate inside each adapter's _serialize_container helper.

Out of scope for this refactor: shared infra with multiple consumers
(live_e2e_test, internal scenarios). File a separate ADR.
```

### Final guard (acceptance criterion 7)
```bash
grep -rin "daytona" backend/src/benchmarks/sweevo/ | grep -v __pycache__
# expect 0
```

---

## Phase 2 — Materializer → RPC wrapper; dead-code deletion

**File:** `backend/src/benchmarks/sweevo/sandbox.py`

### Rewrite `apply_layerstack_to_repo` (currently lines 800-846)
```python
async def apply_layerstack_to_repo(
    sandbox_id: str,
    repo_dir: str = _REPO_DIR,
) -> None:
    """Project the active overlay onto repo_dir via daemon RPC."""
    from sandbox.host.daemon_client import call_daemon_api
    await call_daemon_api(
        sandbox_id,
        "api.commit_to_workspace",
        {"workspace_root": repo_dir},
        timeout=_DEFAULT_SANDBOX_SETUP_TIMEOUT,
    )
```

### Delete

- `_materialize_layerstack_command` and its embedded Python script (lines 848-963) — 115 LoC
- `_upload_file_with_fallback` (lines 355-361) — 7 LoC
- `_write_file_via_chunked_base64_exec` (lines 409-429) — 21 LoC
- `_stop_public_workspace_overlay` (lines 755-773) — 19 LoC (already a no-op wrapper)
- `provision_sweevo_sandbox` (lines 475-502) — 28 LoC (confirmed unused via grep)
- `run_sweevo_required_test` (lines 1311-1419) — 109 LoC (confirmed no external callers)
- `prepare_sweevo_test_run` (lines 1422-1468) — 47 LoC (confirmed no external callers)
- `_progress` callback parameter throughout `setup_sweevo_sandbox` and `create_sweevo_test_sandbox` — ~25 LoC of plumbing (inconsistent with `_step` in `__main__.py`; pick one)

**Rewrite `ensure_sweevo_test_patch`** (currently at lines 999-1054) to write the patch via `git apply -` piped through `raw_exec` stdin instead of the deleted `_upload_file_with_fallback`:

```python
async def ensure_sweevo_test_patch(
    instance: SWEEvoInstance,
    sandbox_id: str,
    repo_dir: str = _REPO_DIR,
) -> None:
    test_patch = instance.test_patch
    if not test_patch:
        return
    # Pipe the patch through stdin instead of uploading a temp file
    response = await sandbox_api.raw_exec(
        sandbox_id,
        f"cd {repo_dir} && git apply - 2>&1",
        cwd="/",
        stdin=test_patch,
        timeout=_DEFAULT_SANDBOX_COMMAND_TIMEOUT,
    )
    if response.exit_code != 0:
        # Idempotent? Try reverse-check to decide
        check = await sandbox_api.raw_exec(
            sandbox_id,
            f"cd {repo_dir} && git apply -R --check - 2>&1",
            stdin=test_patch,
            timeout=_DEFAULT_SANDBOX_COMMAND_TIMEOUT,
        )
        if check.exit_code == 0:
            logger.info("Test patch for %s already applied", instance.instance_id)
        else:
            logger.warning("Test patch for %s failed: %s", instance.instance_id, response.stdout[:300])
```

Confirm `sandbox_api.raw_exec` accepts `stdin=`; if not, fall back to `echo ... | git apply -` via shell quoting, but the daemon-level stdin approach is preferred.

---

## Phase 3b — Move materialize call into lifecycle

**File:** `backend/src/task_center_runner/benchmarks/sweevo/lifecycle.py:51-81`

Currently `evaluate_sweevo_result` (in `evaluation.py:43`) calls `apply_layerstack_to_repo`. After this phase, the lifecycle hook calls it BEFORE evaluating, with a post-commit assertion.

Modified `after_run` body (insert between L64 `if completed_cleanly:` and L65 `result = await evaluate_sweevo_result(...)`):

```python
if completed_cleanly:
    await apply_layerstack_to_repo(report.sandbox_id, self._repo_dir)
    assert (Path(self._repo_dir) / ".git").is_dir(), \
        "post-commit .git missing — overlay opaque-dir shadowed the repo"
    result = await evaluate_sweevo_result(
        self._instance, result, report.sandbox_id, self._repo_dir
    )
```

`evaluation.py:43` (the call to `apply_layerstack_to_repo` and step 1 of the function) is deleted; `evaluate_sweevo_result` keeps its signature unchanged.

If the active-lease `RuntimeError("commit_to_workspace blocked by active leases")` ever fires in practice, it indicates an agent-loop bug (the engine should have drained terminal tools before `after_run`). Let it surface; do not paper over.

---

## Phase 4 — Migration

### 4a — single commit: `git mv` source + tests together

```bash
mkdir -p backend/src/task_center_runner/benchmarks/sweevo
git mv backend/src/benchmarks/sweevo/dataset.py    backend/src/task_center_runner/benchmarks/sweevo/dataset.py
git mv backend/src/benchmarks/sweevo/evaluation.py backend/src/task_center_runner/benchmarks/sweevo/evaluation.py
git mv backend/src/benchmarks/sweevo/models.py     backend/src/task_center_runner/benchmarks/sweevo/models.py
git mv backend/src/benchmarks/sweevo/prompt.py     backend/src/task_center_runner/benchmarks/sweevo/prompt.py
git mv backend/src/benchmarks/sweevo/sandbox.py    backend/src/task_center_runner/benchmarks/sweevo/sandbox.py
git mv backend/src/benchmarks/sweevo/__main__.py   backend/src/task_center_runner/benchmarks/sweevo/__main__.py
git mv backend/src/benchmarks/sweevo/__init__.py   backend/src/task_center_runner/benchmarks/sweevo/_old_init.py  # merge in 4b
rmdir backend/src/benchmarks/sweevo
rmdir backend/src/benchmarks

# Tests
git mv backend/tests/unit_test/test_benchmarks/* backend/tests/unit_test/test_benchmarks/   # noop or new dir
# (no relocation needed if test path is already correct; verify after 4b)
```

Commit: `refactor(sweevo): move benchmarks/ → task_center_runner/benchmarks/ (no logic changes)`.

### 4b — rewrite all references in a separate commit

Driven by Phase 0 grep outputs (#1, #2). Substitutions:

```
from benchmarks.sweevo.        → from task_center_runner.benchmarks.sweevo.
import benchmarks.sweevo.      → import task_center_runner.benchmarks.sweevo.
"benchmarks.sweevo"            → "task_center_runner.benchmarks.sweevo"   (logger names)
python -m benchmarks.sweevo    → python -m task_center_runner.benchmarks.sweevo
```

Additional one-offs:
- `backend/src/task_center_runner/read.md:123` — update doc path reference.
- `backend/scripts/smoke_docker_provider.sh` — update if it invokes `python -m benchmarks.sweevo`.
- `backend/src/task_center_runner/__init__.py` — update any `benchmarks` import.
- `backend/src/task_center_runner/environments/sweevo_image/{health.py,fixtures.py}` — update imports.
- `backend/tests/unit_test/test_task_center_runner/test_no_core_imports.py` — extend test to forbid `from benchmarks.` (acceptance criterion 12).

Commit: `refactor(sweevo): rewrite all references to new import path`.

### Rollback recipe

```bash
git revert <commit-4b>   # undo references
git revert <commit-4a>   # undo file move
```

Phase 1 RPC stays in place (independent). Both reverts are pure-mechanical commits; no data loss.

---

## Phase 5 — Persistent-sandbox redesign + final file layout

### 5a — Persistent sandbox model

**Deterministic naming.** Replace `_default_sweevo_sandbox_name` (random suffix `f"sweevo-test-{instance_id}-{uuid4().hex[:8]}"`) with:

```python
def _sweevo_sandbox_name(instance: SWEEvoInstance) -> str:
    return _truncate_dns_label(f"sweevo-{instance.instance_id}")
```

**`provision_sandbox` flow** (replaces today's `create_sweevo_test_sandbox` ~250 LoC):

```python
async def provision_sandbox(ctx: PreContext) -> str:
    name = _sweevo_sandbox_name(ctx.instance)
    existing = _find_existing_sandbox_by_name(_service(), name)

    if existing is None:
        sandbox_id = await _create_sandbox(ctx.instance, name)
    else:
        sandbox_id = await _resume_sandbox(existing, name, ctx.instance)

    await setup_sweevo_sandbox(
        ctx.instance, sandbox_id, ctx.repo_dir, install_lsp=True,
    )
    return sandbox_id


async def _resume_sandbox(
    existing: dict, name: str, instance: SWEEvoInstance,
) -> str:
    status = (existing.get("status") or "").lower()
    sandbox_id = str(existing["id"])
    if status == "running":
        return sandbox_id
    if status in ("exited", "created", "paused"):
        sandbox_api.start_sandbox(sandbox_id)
        return sandbox_id
    # "dead", "removing", "restarting" — recreate
    logger.warning("Sandbox %s in unrecoverable status=%s; recreating", name, status)
    sandbox_api.delete_sandbox(sandbox_id)
    return await _create_sandbox(instance, name)
```

**Deletions (continuation of Phase 3a's cuts):**
- `_prune_auto_sweevo_sandboxes_for_fresh_run` (~25 LoC)
- `_find_reusable_auto_sweevo_sandbox` (~40 LoC)
- `_safe_list_sandboxes` (~25 LoC) — no longer needed; `_find_existing_sandbox_by_name` is the only lookup
- `_enforce_global_sandbox_quota` + `_global_sandbox_quota` (~70 LoC)
- `_cleanup_failed_sandbox` (~15 LoC) — failed creates surface directly
- `_log_sandbox_creation_failure` (~30 LoC) — inline one-line `logger.warning(...)`
- `reuse_existing_auto` parameter throughout
- `_kill_other_sweevo_processes` (~75 LoC in `__main__.py:54-129`) — docker name uniqueness handles concurrent runs; second invocation gets "container already in use" and fails fast
- `runner.sandbox_reuse_mode` central config dependency — no longer consulted

**`fixtures.py` updates** (`task_center_runner/environments/sweevo_image/fixtures.py:84-126`):
- Drop `reuse_existing_auto=_reuse_existing_auto_enabled()` argument
- Delete `_reuse_existing_auto_enabled` helper function

### 5b — File reorganization

Carve up the migrated `sandbox.py` into:

| Destination | Symbols |
|---|---|
| `setup.py` | `preflight`, `provision_sandbox`, `_resume_sandbox`, `_create_sandbox`, `bootstrap_sandbox_provider` call site, plus CSV/JSONL loaders (`load_pr_description`, `load_sweevo_instance`, `select_sweevo_instance`, `summarize_sweevo_instance`, `load_pr_description_overrides`) folded from `dataset.py`/`prompt.py` |
| `run.py` | `SweevoProvisioner` (verify-only), `build_agent_delegate` (collapsed from `agent_runner.py`'s triple-factory), `build_run_config` |
| `eval.py` | `SweevoLifecycle` (from `lifecycle.py`), `evaluate_sweevo_result`, `ensure_sweevo_test_patch`, `_run_test_set`, `_build_test_set_command`, `_parse_pytest_passed_count`, `format_verdict` |
| `models.py` | `SWEEvoInstance`, `SWEEvoResult`, `PreContext` (new dataclass), constants (`_REPO_DIR`, `_CONDA_ACTIVATE`, `_DEFAULT_SWEEVO_TEST_TIMEOUT`, ...), pure helpers (`_normalize_sweevo_image_ref`, `_truncate_dns_label`, `_has_explicit_sweevo_image_version`, `_strip_exit_code_marker`) |
| `_snapshot.py` | `register_sweevo_snapshot` (docker-only), `verify_sweevo_snapshot_exists`, `SnapshotNotRegisteredError`, `resolve_sweevo_snapshot`, `default_sweevo_snapshot_name` |
| `_provision.py` | `_create_sandbox`, `_resume_sandbox`, `setup_sweevo_sandbox`, `reset_sweevo_workspace` (kept — fixtures.py needs it), `_sweevo_sandbox_name`, `_sweevo_sandbox_labels`, `_merge_sandbox_labels`, `_configure_reusable_sweevo_sandbox`, `_find_existing_sandbox_by_name`, `_rebuild_sweevo_workspace_base` |
| `_exec.py` | `_exec`, `_wait_for_sandbox_exec_ready`, `_is_transient_sandbox_exec_error` |

### 5c — Delete now-empty files

```bash
git rm backend/src/task_center_runner/benchmarks/sweevo/evaluation.py
git rm backend/src/task_center_runner/benchmarks/sweevo/lifecycle.py
git rm backend/src/task_center_runner/benchmarks/sweevo/provisioner.py
git rm backend/src/task_center_runner/benchmarks/sweevo/agent_runner.py
git rm backend/src/task_center_runner/benchmarks/sweevo/sandbox.py
git rm backend/src/task_center_runner/benchmarks/sweevo/dataset.py
git rm backend/src/task_center_runner/benchmarks/sweevo/prompt.py
git rm backend/src/task_center_runner/benchmarks/sweevo/_old_init.py
```

`__init__.py` rewritten with public re-exports + disk-cleanup doc note.

---

## Pre-mortem

| # | Scenario | Mitigation |
|---|---|---|
| 1 | `.git` shadowed by an overlay opaque dir | post-commit `assert (repo_dir / ".git").is_dir()` in `SweevoLifecycle.after_run` |
| 2 | Active lease at commit time | existing `RuntimeError("commit_to_workspace blocked by active leases")` surfaces; signals an agent-loop bug, fix at source |
| 3 | EXDEV cross-device rename under docker bind-mount | Phase 1a fix; unit-tested with mocked `OSError(errno.EXDEV)` |
| 4 | Mock targets pinned to deleted symbols | Phase 0 grep #3 + #6 audit before Phase 2 ships |
| 5 | Stale daytona comment escapes Phase 3a | Phase 0 grep #5 + acceptance criterion 7 |
| 6 | Dynamic / string-based imports of `benchmarks.*` missed by Phase 4b | Phase 0 grep #2 covers literal patterns; acceptance criterion 8 |
| 7 | Two concurrent CLI invocations for the same instance_id | Docker name uniqueness rejects the second create; second invocation gets a clear error from docker. Recommend fail-fast (no waiting). |
| 8 | Container in `dead` or `removing` state | `_resume_sandbox` auto-recreates |
| 9 | Disk fills up with persistent containers | Documented in `__init__.py` docstring; `docker container prune` and the one-liner are the cleanup path |
| 10 | `commit_to_workspace` rebuilds base every time, then next setup rebuilds again from `base_commit` | Accepted cost (~one extra repo re-scan per cycle); traded for `rebuild_base` kwarg-free RPC |

---

## Acceptance criteria

1. `_replace_workspace_contents` handles EXDEV — unit test in `backend/tests/unit_test/test_sandbox/test_layer_stack/`.
2. *(removed; no rebuild_base kwarg)*
3. `grep -rEn "WHITEOUT_PREFIX|OPAQUE_MARKER" backend/src/` returns hits only inside `sandbox/layer_stack/`.
4. `apply_layerstack_to_repo` body ≤ 10 LoC.
5. `eval.py` does not import from `_provision.py` (only from `_exec.py`, `models.py`, and `sandbox.api`).
6. `backend/src/benchmarks/` does not exist; `python -m task_center_runner.benchmarks.sweevo --instance-id=<id>` is the canonical entry.
7. `grep -rin "daytona" backend/src/task_center_runner/benchmarks/sweevo/` returns 0.
8. `grep -rEn '"benchmarks\.|'\''benchmarks\.|\bbenchmarks\.sweevo\b' backend/` returns 0.
9. `ls backend/src/task_center_runner/benchmarks/sweevo/*.py` lists exactly: `__init__.py`, `__main__.py`, `pipeline.py`, `setup.py`, `run.py`, `eval.py`, `models.py`, `_snapshot.py`, `_provision.py`, `_exec.py` — 10 files.
10. `pipeline.py::run_benchmark_sweevo` body ≤ 12 lines containing 3 named stage calls in order; NO `try`/`finally`.
11. Field-level parity (`fix_rate` + `resolved`) on `dask__dask_2023.3.2_2023.4.0`:
    ```
    uv run python -m task_center_runner.benchmarks.sweevo --instance-id=dask__dask_2023.3.2_2023.4.0
    jq '{fix_rate, resolved}' .sweevo_runs/<run_dir>/sweevo_result.json
    ```
    matches `backend/tests/integration_test/test_benchmarks/fixtures/sweevo_baseline_dask__dask_2023.3.2_2023.4.0.json`.
12. `test_no_core_imports` extended to forbid `from benchmarks.` anywhere in `backend/`.
13. (end-state, after Phase 4) `grep -n '\.get("state"' backend/src/task_center_runner/benchmarks/sweevo/` returns 0.
14. `backend/src/task_center_runner/benchmarks/sweevo/FOLLOWUP_provider_state_canonical.md` exists.
15. State-vocab regression test asserts: (a) `_resume_sandbox` returns `sandbox_id` for `status="exited"` after `start_sandbox` is called; (b) returns `sandbox_id` directly for `status="running"`; (c) recreates for `status="dead"`.
16. Persistent-sandbox regression test: running `python -m … --instance-id=X` twice in succession produces the same `fix_rate`; the second invocation uses the existing container (verified via `docker inspect`).
17. Idempotent setup: after a previous run that produced agent edits, the next setup leaves `git status` clean (no untracked / modified files).

---

## Verification per phase

```bash
# After 1a
uv run pytest backend/tests/unit_test/test_sandbox/test_layer_stack/test_replace_workspace_contents_exdev.py -xvs

# After 1b
uv run pytest backend/tests/unit_test/test_sandbox/test_daemon/test_commit_to_workspace_op.py -xvs

# After 3a
grep -rin "daytona" backend/src/benchmarks/sweevo/ | grep -v __pycache__   # expect 0

# After 2
grep -rn "_materialize_layerstack_command\|_upload_file_with_fallback\|_write_file_via_chunked_base64_exec" backend/   # expect 0
wc -l backend/src/benchmarks/sweevo/sandbox.py   # expect ~700 (down from ~1469)

# After 3b
grep -n "apply_layerstack_to_repo" backend/src/benchmarks/sweevo/evaluation.py   # expect 0

# After 4
test ! -d backend/src/benchmarks
uv run python -m task_center_runner.benchmarks.sweevo --help
grep -rEn '"benchmarks\.|'\''benchmarks\.|\bbenchmarks\.sweevo\b' backend/   # expect 0

# After 5
ls backend/src/task_center_runner/benchmarks/sweevo/*.py | wc -l   # expect 10
grep -cE "^(async )?def " backend/src/task_center_runner/benchmarks/sweevo/pipeline.py   # expect 1
wc -l backend/src/task_center_runner/benchmarks/sweevo/pipeline.py   # expect ≤ 30

# E2E
uv run python -m task_center_runner.benchmarks.sweevo --instance-id=dask__dask_2023.3.2_2023.4.0
jq '{fix_rate, resolved}' .sweevo_runs/$(ls -t .sweevo_runs/ | head -1)/sweevo_result.json
```

---

## Out of scope

- Provider system structural changes (`backend/src/sandbox/provider/*` stays as-is).
- Normalizing the `state`/`status` adapter divergence at the protocol boundary — captured as a follow-up ADR.
- Restructuring `__main__.py` of the wider `task_center_runner` package.
- Promoting `_exec` / `_wait_for_sandbox_exec_ready` to `sandbox.host` (defer until a second consumer appears).
- Adding a `--gc` subcommand for persistent-sandbox cleanup (the documented one-liner is sufficient).

---

## References

- `backend/src/sandbox/layer_stack/stack.py:299-361` — `LayerStack.commit_to_workspace`
- `backend/src/sandbox/layer_stack/stack.py:395-401` — `_replace_workspace_contents` (EXDEV fix site)
- `backend/src/sandbox/provider/docker/adapter.py:50` — Docker adapter emits `status` key
- `backend/src/sandbox/provider/daytona/adapter.py:80,175` — Daytona adapter emits `state` key
- `backend/src/sandbox/daemon/rpc/dispatcher.py:427-429` — RPC dispatch table
- `backend/src/sandbox/daemon/builtin_operations.py` — existing ops (`build_workspace_base`, `acquire_snapshot`, `release_lease`)
- `backend/src/benchmarks/sweevo/sandbox.py:800-963` — current materializer (to be replaced)
- `backend/src/benchmarks/sweevo/sandbox.py:933-953` — existing EXDEV fallback (reference implementation for Phase 1a)
- `backend/src/task_center_runner/benchmarks/sweevo/lifecycle.py:51-81` — `SweevoLifecycle.after_run` insertion site
- `backend/src/task_center_runner/environments/sweevo_image/fixtures.py:84-126` — fixture callers requiring Phase 5a updates
- Project memory anchors: `daytona_pending_build_root_cause.md`, `feedback_parallel_user_commits.md`, `checked_batch_apply_argv_limit.md`
