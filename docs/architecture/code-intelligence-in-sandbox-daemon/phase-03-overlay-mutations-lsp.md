# Phase 3 — Overlay + mutations + LSP into the daemon (via package reuse) + SQLite ledger

**Estimated effort:** 5-6 days (3 days engineering + 2-3 days E2E with 5 invariants live)
**Risk profile:** HIGH — preserves all five HARD INVARIANTS; the OCC pipeline is the consistency-critical core
**Status:** Not started
**Blocks on:** Phase 2 complete

## Goal

Make the daemon serve mutations, queries, and overlay commands by **reusing the existing `CodeIntelligenceService`** (instantiated with `sandbox=None, transport=None` so local-FS branches activate) and exposing each public method as an daemon command dispatch entry. The edit history ledger becomes a SQLite WAL database at `$HOME/.cache/eos-ci/<wh>/v1/ledger.sqlite3`, replacing the in-memory `EditHistoryLedger`.

This is the highest-risk phase. All five HARD INVARIANTS must continue to hold, and the live E2E reproduces each one against real edits on a real `dask` swe-evo sandbox. **Drift risk is eliminated by construction** — daemon and orchestrator's in-process backend run literally the same Python code.

## Why fourth

Three reasons:

1. **The daemon scaffolding is now proven.** Phase 2 shipped the wire protocol, retry semantics, and lifecycle. Phase 3 adds methods to the dispatch table; it doesn't have to also debug socket plumbing.
2. **OCC must be daemon-side BEFORE `svc.cmd` (Phase 4) can route through the daemon.** `svc.cmd`'s overlay-commit half goes through `OverlayCommandCommitter` which calls `WriteCoordinator`. If OCC is still orchestrator-side, every `svc.cmd` would have to bounce back through the transport per file read. Phase 3 collapses that.
3. **Single-point lock arbitration is strictly stronger than today's per-orchestrator drift detection.** Today each orchestrator process has its own `Arbiter` locks; OCC base-hash check catches cross-process drift. With the daemon, the daemon is the single arbitration point — locks across orchestrators are now consistent in-memory, AND the OCC base-hash check still defends against sandbox-external mutations.

## Design choice — package reuse, not reimplementation

**Rejected approach:** create new daemon-local overlay, mutation, and LSP files that copy or rewrite the OCC/overlay/LSP logic. Adds drift surface, requires drift-guard tests, doubles the maintenance for every change to `WriteCoordinator` etc.

**Chosen approach (consistent with Phase 1):** the daemon constructs the existing `CodeIntelligenceService` and routes each daemon command op to the corresponding method. The bundle from Phase 1 already ships the entire `sandbox.code_intelligence` package; Phase 3 just wires more of its methods into the dispatch table.

```python
# In server.py, at startup:
from sandbox.code_intelligence.service import CodeIntelligenceService
_svc = CodeIntelligenceService(
    sandbox_id="local",
    workspace_root=args.workspace_root,
    sandbox=None,
    transport=None,
)
_svc.ensure_initialized(wait=True)

# Each handler is one line:
async def handle_write_file(args: dict) -> dict:
    specs = [_writespec_from_dict(s) for s in args["specs"]]
    result = _svc.write_file(specs, agent_id=args.get("agent_id", ""),
                              description=args.get("description", ""))
    return _operation_result_to_dict(result)
```

**Why this works:**
- `CodeIntelligenceService` already has working local-FS code paths (`_read_local`, `_write_local`, `_apply_local_batch_checked`, `collect_local_files`). They activate whenever `sandbox=None, transport=None`.
- All five HARD INVARIANTS are enforced by the existing `WriteCoordinator + Arbiter + TimeMachine + Patcher + ContentManager` cluster. Same code, same locks, same semantics.
- Drift risk = zero by construction.
- Phase 5 cleanup (~600 LOC) only deletes the dead REMOTE branches; the LOCAL branches the daemon depends on stay.

**Trade-off:** the daemon process carries the dead remote branches (`_apply_remote_*`, `_read_remote*`, etc.) in memory until Phase 5 cleanup. Harmless — they're never invoked because `sandbox=None`.

## What ships

| Artifact | File | Purpose |
|---|---|---|
| Daemon `CodeIntelligenceService` instance | `backend/src/sandbox/code_intelligence/daemon/server.py` (extended) | Constructed at startup with `sandbox=None`; all dispatch handlers route to its methods |
| Mutation/query dispatch entries | `server.py` (extended) | `apply_edit`, `commit_operation_against_base`, `commit_specs_many`, `write_file`, `edit_file`, `delete_file`, `move_file`, `undo_last_edit`, `find_definitions`, `find_references`, `hover`, `diagnostics`, `query_symbols`, `index_refresh`, `lsp_invalidate`, `list_folder_files`, `status`, `get_telemetry` |
| SQLite WAL ledger adapter | `backend/src/sandbox/code_intelligence/daemon/storage.py` (extended) | `LedgerStore` class implementing the existing `EditHistoryLedger` interface, persisting to `ledger.sqlite3` |
| Ledger injection | `backend/src/sandbox/code_intelligence/mutations/arbiter.py` (modified) | Accepts an injected `edit_history` ledger; daemon passes the SQLite-backed `LedgerStore`; orchestrator-side keeps the in-memory default |
| **Workspace-write bypass guard** | `backend/src/sandbox/code_intelligence/daemon/server.py` (new check) | Wrapper around dispatch that asserts no handler writes under `workspace_root` except via the `_svc` instance |
| Orchestrator passthrough | `backend/src/sandbox/code_intelligence/backends/` (extended) | Each `DaemonBackend` method becomes one `_call_daemon_command(op, args)` |
| Phase 3 live E2E | `backend/tests/test_e2e/test_live_ci_phase3_invariants.py` | Five HARD INVARIANT subtests + ledger replay + bypass guard |
| Mutation parity tests | `backend/tests/test_sandbox/test_code_intelligence/test_ci_mutations_parity.py` | Daemon vs in-process backend produce identical results on fixture workspaces |
| Ledger unit tests | `backend/tests/test_sandbox/test_code_intelligence/test_storage_ledger.py` | WAL config, schema, integrity check, replay, interface conformance |

**Notably NOT shipped (vs original draft):**
- ~~daemon-local mutation copy~~ — package reuse instead
- ~~daemon-local overlay copy~~ — package reuse instead
- ~~daemon-local LSP copy~~ — package reuse instead
- ~~daemon drift-guard test for copied files~~ — no copies, no drift to guard

## Detailed task list

### Task 3.1 — SQLite WAL ledger

**File:** `backend/src/sandbox/code_intelligence/daemon/storage.py` (extended from Phase 1)

**Schema:**

```sql
CREATE TABLE IF NOT EXISTS edits (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    ts REAL NOT NULL,                  -- time.time() at record
    run_id TEXT NOT NULL DEFAULT '',
    agent_run_id TEXT NOT NULL DEFAULT '',
    task_id TEXT NOT NULL DEFAULT '',
    agent_id TEXT NOT NULL DEFAULT '',
    file_path TEXT NOT NULL,
    edit_type TEXT NOT NULL,           -- 'write_file', 'edit_file', 'delete_file', 'move_file', 'apply_edit', 'shell_overlay'
    old_hash TEXT NOT NULL DEFAULT '',
    new_hash TEXT NOT NULL DEFAULT '',
    description TEXT NOT NULL DEFAULT ''
);

CREATE INDEX IF NOT EXISTS idx_edits_file ON edits(file_path);
CREATE INDEX IF NOT EXISTS idx_edits_ts ON edits(ts);
CREATE INDEX IF NOT EXISTS idx_edits_run ON edits(run_id);
CREATE INDEX IF NOT EXISTS idx_edits_agent_run ON edits(run_id, agent_run_id);
```

**PRAGMAs at connection open:**

```python
conn.execute("PRAGMA journal_mode = WAL;")
conn.execute("PRAGMA synchronous = NORMAL;")
conn.execute("PRAGMA temp_store = MEMORY;")
conn.execute("PRAGMA mmap_size = 67108864;")  # 64 MB
```

**Startup integrity check:**

```python
def _open_ledger(state_dir: Path) -> sqlite3.Connection:
    path = state_dir / "ledger.sqlite3"
    conn = sqlite3.connect(path, isolation_level=None, check_same_thread=False)
    # Apply pragmas
    ...
    # Integrity check
    result = conn.execute("PRAGMA integrity_check").fetchone()
    if result[0] != "ok":
        logging.warning("storage: ledger corrupt (%s); rotating", result[0])
        conn.close()
        path.rename(path.with_suffix(f".corrupt.{int(time.time())}.sqlite3"))
        conn = sqlite3.connect(path, isolation_level=None, check_same_thread=False)
        # Re-apply pragmas
    conn.executescript(_SCHEMA_SQL)
    return conn
```

**Interface conformance:** `LedgerStore` exposes the same methods as today's `EditHistoryLedger` (`record`, `changes_in_scope`, `external_changes_in_scope`, `changes_since`, `recent_edits`, `hotspots`, `who_changed`, `changes_by_agent_run`, `contention_hotspots`, `initialized` property). The `Arbiter` constructor accepts an `edit_history` parameter today (per `arbiter.py:64`); the daemon passes a `LedgerStore`, the orchestrator's in-process backend keeps the default `EditHistoryLedger()`.

**Concurrency:** the daemon is single-process asyncio. The DB connection opens with `check_same_thread=False` because LSP worker threads may also write. Wrap writes in `threading.Lock` (SQLite serializes WAL writes anyway, the lock prevents Python-side races on the cursor object).

### Task 3.2 — `Arbiter` ledger injection (minimal change)

**File:** `backend/src/sandbox/code_intelligence/mutations/arbiter.py`

The `Arbiter.__init__` already accepts `edit_history: EditHistoryLedger | None = None`. **No code change needed** — the daemon just constructs:

```python
ledger = LedgerStore(state_dir=state_dir(args.workspace_root))
arbiter = Arbiter(workspace_root=args.workspace_root, edit_history=ledger)
```

But the daemon doesn't construct `Arbiter` directly — it constructs `CodeIntelligenceService`, which constructs `Arbiter` internally. So we need a **constructor injection point** on `CodeIntelligenceService`:

**Minimal change to `service.py`:** add an optional `edit_history` kwarg threaded into the `Arbiter` construction:

```python
def __init__(
    self,
    sandbox_id: str,
    workspace_root: str = "/workspace",
    sandbox: Any = None,
    *,
    transport: SandboxTransport | None = None,
    edit_history: Any = None,    # NEW
) -> None:
    ...
    self.arbiter = Arbiter(workspace_root=workspace_root, edit_history=edit_history)
```

**Verify:** `git diff backend/src/sandbox/code_intelligence/service.py` shows a one-arg addition + one line passing it to `Arbiter`. No other behavior change.

### Task 3.3 — Daemon constructs `CodeIntelligenceService` (SOCKET-FIRST startup)

**File:** `backend/src/sandbox/code_intelligence/daemon/server.py` (extends Phase 2)

**Critical change vs original draft:** the daemon must bind the socket BEFORE starting the symbol-index build. Otherwise `create_sandbox`'s eager bootstrap blocks for the full index-build duration (multi-seconds for a 1k-file repo), defeating the eager-bootstrap SLO of <3s cold. The fix: kick the index build into the existing `SymbolIndex._background_build` thread (it already exists per `symbol_index.py:201-211`) and bind the socket immediately.

**At daemon startup, in `run_daemon`:**

```python
from sandbox.code_intelligence.service import CodeIntelligenceService
from sandbox.code_intelligence.daemon.storage import state_dir, LedgerStore, IndexStore

async def run_daemon(workspace_root: str) -> None:
    state = state_dir(workspace_root)

    # Migrate pickle index → sqlite if Phase 1 leftover present (Phase 3.5 helper)
    migrate_pickle_to_sqlite(state)

    # Daemon-resident CI service. sandbox=None, transport=None → local-FS branches.
    # SQLite-backed ledger from storage.
    ledger = LedgerStore(state_dir=state)
    _DAEMON_STATE.svc = CodeIntelligenceService(
        sandbox_id="local",
        workspace_root=workspace_root,
        sandbox=None,
        transport=None,
        edit_history=ledger,
    )

    # SOCKET-FIRST: kick index build to background, bind socket immediately.
    # SymbolIndex.ensure_built(wait=False) starts the existing background thread.
    _DAEMON_STATE.svc.symbol_index.ensure_built(wait=False)

    # Now bind socket — ping returns immediately; query_symbols returns
    # empty (or partial) until the background build finishes.
    server = await asyncio.start_unix_server(handle_client, path=str(state / "daemon.sock"))
    os.chmod(state / "daemon.sock", 0o600)

    # Write PID file AFTER socket bound (so launcher's readiness poll succeeds)
    (state / "daemon.pid").write_text(str(os.getpid()))

    _DAEMON_STATE.workspace_root = workspace_root
    _DAEMON_STATE.guard_enabled = True
    _DAEMON_STATE.started_at = time.time()

    # Run forever
    async with server:
        await server.serve_forever()
```

**`_DAEMON_STATE.svc` is a process-level singleton; every dispatch handler reads it.**

**Implication for callers:** `query_symbols` may return empty or partial results until the background build completes. Callers that need full results call `svc.warmup()` (waits up to 60s) or check `svc.is_initialized`. The Phase 1 eager-bootstrap test (`test_eager_bootstrap_timing` Task 1.5.F) waits for index readiness explicitly via `svc.query_symbols("Bag")` returning >0 results.

**Add `index_ready` op to dispatch:**

```python
async def handle_index_ready(args: dict) -> dict:
    """Returns whether the background index build has completed.
    Used by the orchestrator to poll readiness without blocking."""
    return {"ready": _DAEMON_STATE.svc.symbol_index.is_built}
```

### Task 3.4 — Add daemon dispatch entries

**File:** `backend/src/sandbox/code_intelligence/daemon/server.py` (extends Phase 2)

**New ops:**

```python
DISPATCH.update({
    # Mutations
    "apply_edit":                       handle_apply_edit,
    "commit_operation_against_base":    handle_commit_operation_against_base,
    "commit_specs_many":                handle_commit_specs_many,
    "write_file":                       handle_write_file,
    "edit_file":                        handle_edit_file,
    "delete_file":                      handle_delete_file,
    "move_file":                        handle_move_file,
    "undo_last_edit":                   handle_undo_last_edit,

    # Queries
    "query_symbols":                    handle_query_symbols,
    "find_definitions":                 handle_find_definitions,
    "find_references":                  handle_find_references,
    "hover":                            handle_hover,
    "diagnostics":                      handle_diagnostics,

    # Internal
    "index_refresh":                    handle_index_refresh,
    "lsp_invalidate":                   handle_lsp_invalidate,
    "list_folder_files":                handle_list_folder_files,
    "status":                           handle_status,
    "get_telemetry":                    handle_get_telemetry,
})
```

**Each handler is a 3-5 line wrapper:**

```python
async def handle_write_file(args: dict) -> dict:
    specs = [WriteSpec(**s) for s in args["specs"]]
    result = _DAEMON_STATE.svc.write_file(
        specs, agent_id=args.get("agent_id", ""), description=args.get("description", ""),
    )
    return _operation_result_to_dict(result)

async def handle_query_symbols(args: dict) -> dict:
    return [_symbol_info_to_dict(s) for s in _DAEMON_STATE.svc.query_symbols(args["query"])]

# etc. for every op
```

**Serialization:** `_operation_result_to_dict`, `_symbol_info_to_dict`, etc. use `dataclasses.asdict` for outbound; reconstruct via `cls(**args)` on the orchestrator side.

### Task 3.5 — Workspace-write bypass guard (NEW, audit response)

**Goal:** Make it impossible for an daemon command handler to write directly under `workspace_root` without going through `_DAEMON_STATE.svc` (i.e. without going through `WriteCoordinator`). This enforces the storage-boundary invariant from the overview.

**Implementation:** wrap `handle_client` in `server.py` so dispatch is bracketed by an inotify-style mtime sample. After each handler returns, scan `workspace_root` for files mtime'd within the request window that don't appear in the ledger. Any such file is a bypass.

```python
async def handle_client(reader, writer):
    try:
        while not reader.at_eof():
            req = await read_frame(reader)
            handler = DISPATCH.get(req["op"])
            if handler is None:
                resp = {"v": 1, "id": req["id"], "ok": False,
                        "error": {"kind": "UnsupportedOp", ...}}
            else:
                pre_seq = _DAEMON_STATE.svc.arbiter.metrics.total_edits
                window_start = time.time()
                try:
                    result = await handler(req["args"])
                    resp = {"v": 1, "id": req["id"], "ok": True, "result": result}
                except Exception as exc:
                    resp = {"v": 1, "id": req["id"], "ok": False, "error": {...}}

                # BYPASS GUARD: any workspace_root file modified during the handler
                # window without a corresponding ledger entry is a bug.
                if _DAEMON_STATE.guard_enabled:  # default True; opt-out for query ops
                    new_seq = _DAEMON_STATE.svc.arbiter.metrics.total_edits
                    bypassed = _scan_unledgered_changes(
                        workspace_root=_DAEMON_STATE.workspace_root,
                        window_start=window_start,
                        ledger_delta_count=new_seq - pre_seq,
                    )
                    if bypassed:
                        logging.error(
                            "WORKSPACE WRITE BYPASS: handler=%s bypassed paths=%s — this is a bug",
                            req["op"], bypassed,
                        )
                        # Optionally: surface as an error envelope so tests catch it
                        if _DAEMON_STATE.guard_strict:  # set True in tests
                            resp = {"v": 1, "id": req["id"], "ok": False,
                                    "error": {"kind": "WorkspaceBypass",
                                              "message": f"unledgered writes: {bypassed}"}}

            writer.write(encode_frame(resp))
            await writer.drain()
    finally:
        ...
```

**Test (3.7.G below):** craft a malicious dispatch handler that writes `workspace_root/__bypass__.txt` directly via `pathlib.Path.write_text` (NOT via `_DAEMON_STATE.svc.write_file`). With `guard_strict=True`, the daemon must respond `WorkspaceBypass` and the file must be visible (the guard doesn't prevent the write — it surfaces it).

### Task 3.6 — Wire `DaemonBackend` methods

**File:** `backend/src/sandbox/code_intelligence/backends/` (extends Phase 1)

Each method becomes a daemon-command call:

```python
class DaemonBackend:
    async def write_file_async(self, specs, *, agent_id="", description=""):
        args = {"specs": [_writespec_to_dict(s) for s in _normalize(specs)],
                "agent_id": agent_id, "description": description}
        raw = await self._call_daemon_command("write_file", args)
        return _operation_result_from_dict(raw)

    def write_file(self, specs, *, agent_id="", description=""):
        return run_sync(self.write_file_async(specs, agent_id=agent_id, description=description))
```

Use `sandbox.async_bridge.run_sync` (already promoted in the predecessor migration).

### Task 3.7 — Phase 3 live E2E (the BIG one)

**File:** `backend/tests/test_e2e/test_live_ci_phase3_invariants.py`

One subtest per HARD INVARIANT, plus persistence-replay and bypass-guard subtests. Each uses `TimingHarness`.

#### 3.7.A — INVARIANT 1: Sorted-path locks (no deadlock)

```python
async def test_invariant_sorted_path_locks(live_sweevo_env):
    h = TimingHarness(phase=3, test_name="invariant_sorted_locks")
    env = live_sweevo_env
    svc = env.make_ci_service_flag_on()

    files = ["/testbed/_phase3_a.txt", "/testbed/_phase3_b.txt"]
    env.exec(f"echo 'A' > {files[0]} && echo 'B' > {files[1]}")

    async def edit_in_order(order):
        return svc.commit_operation_against_base(
            [OperationChange(file_path=files[order[0]], base_content="A\n",
                             base_hash=content_hash("A\n"), final_content="A1\n", base_existed=True),
             OperationChange(file_path=files[order[1]], base_content="B\n",
                             base_hash=content_hash("B\n"), final_content="B1\n", base_existed=True)],
            edit_type="edit_file", agent_id="op_a")

    with h.step("concurrent_opposite_orders"):
        results = await asyncio.gather(
            asyncio.to_thread(edit_in_order, [0, 1]),
            asyncio.to_thread(edit_in_order, [1, 0]),
        )

    successes = sum(1 for r in results if r.success)
    assert successes >= 1
    assert all(r.status in {"committed", "aborted_version", "aborted_lock"} for r in results)

    print(h.report())
    h.dump_json()
```

#### 3.7.B — INVARIANT 2: Strict-base OCC + `aborted_version` on drift

(Same as the previous draft — no change.)

#### 3.7.C — INVARIANT 3: Non-overlap merge fallback

(Same as the previous draft — no change.)

#### 3.7.D — INVARIANT 4: TimeMachine rollback

(Same as the previous draft — no change.)

#### 3.7.E — INVARIANT 5: Symbol/LSP invalidation on commit

(Same as the previous draft — no change.)

#### 3.7.F — Ledger persistence across daemon kill -9

```python
async def test_ledger_persistence_across_daemon_restart(live_sweevo_env):
    h = TimingHarness(phase=3, test_name="ledger_persistence")
    env = live_sweevo_env
    svc = env.make_ci_service_flag_on()

    targets = [f"/testbed/_phase3_ledger_{i}.txt" for i in range(5)]

    with h.step("five_edits"):
        for t in targets:
            svc.write_file([WriteSpec(file_path=t, content=f"content_{t}", overwrite=True)])

    pre_count = svc.status()["edit_buffer"]["entries"]

    with h.step("kill_daemon"):
        env.exec("kill -9 $(cat $HOME/.cache/eos-ci/<wh>/v1/daemon.pid)")
        await asyncio.sleep(0.5)

    with h.step("respawn_via_call"):
        post_count = svc.status()["edit_buffer"]["entries"]  # triggers respawn + ledger replay

    assert post_count == pre_count, f"ledger replay failed: pre={pre_count} post={post_count}"

    print(h.report())
    h.dump_json()
```

#### 3.7.G — Workspace-write bypass guard (NEW, audit response)

```python
async def test_workspace_write_bypass_guard_surfaces_violation(live_sweevo_env):
    """Inject a malicious handler that bypasses WriteCoordinator. Daemon must
    surface WorkspaceBypass in strict mode."""
    h = TimingHarness(phase=3, test_name="bypass_guard")
    env = live_sweevo_env
    svc = env.make_ci_service_flag_on()

    # Enable strict mode on the daemon
    await svc._impl.__call_daemon_command("_set_guard_mode", {"strict": True})

    # Inject a test-only op that intentionally writes the workspace bypass
    # (this op is registered only when an env var is set on the daemon).
    env.exec("touch $HOME/.cache/eos-ci/<wh>/v1/.allow_test_bypass_op")
    await svc._impl.__call_daemon_command("ping")  # daemon picks up the env on next call

    with pytest.raises(DaemonCommandError) as exc_info:
        await svc._impl.__call_daemon_command("_test_bypass_handler",
                                      {"path": "/testbed/__bypass_target__.txt",
                                       "content": "this should be flagged"})

    assert exc_info.value.kind == "WorkspaceBypass"
    assert "__bypass_target__.txt" in str(exc_info.value)

    # The file IS written (guard is detection, not prevention) — but the violation
    # was surfaced.
    code, content = env.exec("cat /testbed/__bypass_target__.txt")
    assert code == 0

    # Cleanup
    env.exec("rm -f /testbed/__bypass_target__.txt")
    env.exec("rm -f $HOME/.cache/eos-ci/<wh>/v1/.allow_test_bypass_op")

    print(h.report())
    h.dump_json()
```

**Implementation note for the `_test_bypass_handler` op:** registered conditionally in `server.py` only when the marker file `.allow_test_bypass_op` exists. This keeps it out of production daemons.

**Run command:** `uv run pytest backend/tests/test_e2e/test_live_ci_phase3_invariants.py -m live -v -s`

### Task 3.8 — Mutation parity tests

**File:** `backend/tests/test_sandbox/test_code_intelligence/test_ci_mutations_parity.py`

Parametrize every existing `test_write_coordinator_*.py`, `test_mutation_service_*.py`, `test_arbiter_*.py` over `["inprocess", "daemon"]` backends. Use a fixture that constructs a real daemon in a tmpdir-rooted fake "sandbox" (just `subprocess.Popen` of `python -m sandbox.code_intelligence.daemon --workspace-root <tmpdir>`). Both backends must produce identical results.

This is a unit-test-style harness; it runs in CI without Daytona.

### Task 3.9 — Ledger unit tests

**File:** `backend/tests/test_sandbox/test_code_intelligence/test_storage_ledger.py`

**Cases:**
- WAL pragma applied (`PRAGMA journal_mode` returns `wal`).
- Schema created on first open.
- Integrity check fails → file rotated to `.corrupt.<ts>.sqlite3`.
- Interface parity: every method on `EditHistoryLedger` exists on `LedgerStore` with matching signature.
- Round-trip: `record()` then query via `recent_edits`, `who_changed`, `hotspots`.
- Concurrency: 10 threads writing `record()` concurrently produce 10 distinct rows in the right order.

### Task 3.10 — Regression check

- `.venv/bin/pytest backend/tests/test_sandbox/ backend/tests/test_tools/ -q` — green with flag off.
- `.venv/bin/pytest backend/tests/test_sandbox/test_code_intelligence/test_ci_mutations_parity.py -q` — daemon parity green.
- Re-run Phases 0, 1, 2 live E2Es — all green.

## Definition of done

- [ ] SQLite WAL ledger at `$HOME/.cache/eos-ci/<wh>/v1/ledger.sqlite3` with documented schema and pragmas; `LedgerStore` interface-compatible with `EditHistoryLedger`.
- [ ] Startup integrity check rotates corrupt ledger files; daemon recovers cleanly.
- [ ] `service.py:CodeIntelligenceService.__init__` accepts optional `edit_history` kwarg; passed through to `Arbiter`. No other behavior change.
- [ ] Daemon constructs `CodeIntelligenceService(sandbox=None, transport=None, edit_history=LedgerStore(...))` at startup.
- [ ] Daemon dispatch table includes all mutation/query/internal ops listed in Task 3.4.
- [ ] **Workspace-write bypass guard surfaces unledgered writes (3.7.G).**
- [ ] `DaemonBackend` methods all wired through `_call_daemon_command(...)`.
- [ ] **Phase 3 live E2E: all FIVE INVARIANT subtests + ledger replay + bypass guard pass against `dask__dask_2023.3.2_2023.4.0`.**
- [ ] Mutation parity tests green (daemon vs in-process).
- [ ] `apply_edits` and `query_symbols` warm-path latency NOT >50ms slower than Phase 0 baseline.
- [ ] Ledger fully replayed after `kill -9` (pre/post edit counts equal).
- [ ] Regression check: Phases 0, 1, 2 E2Es + full unit suite green.
- [ ] PR description includes: all five invariant E2E reports + `compare_to(phase_0_baseline)` deltas + ledger persistence proof + bypass guard proof.

## Risk callouts (Phase 3 specific)

| Severity | Risk | Mitigation |
|---|---|---|
| **HIGH** | Any of the 5 HARD INVARIANTS regresses silently — but with package-reuse approach, the daemon runs literally the same code | Drift risk eliminated by construction (no copies); 5-invariant E2E + parity tests catch any subtle integration bug |
| **HIGH** | Daemon crash mid-write → in-flight commit lost; ledger inconsistent with FS | TimeMachine rollback on partial-apply (existing); SQLite WAL atomic; on respawn, daemon scans for files newer than last `committed` ledger entry and surfaces drift |
| **HIGH** | Lock state in daemon RAM lost on crash → next request sees no held lock when it should | Locks today are advisory in-process; OCC base-hash check is the actual safety net. Document that daemon-RAM locks are PERFORMANCE optimization; correctness rests on OCC. 3.7.A and 3.7.B together verify this |
| **HIGH** | daemon command handler bypasses `WriteCoordinator` and writes `workspace_root` directly | Bypass guard (3.5) + 3.7.G test |
| **MEDIUM** | LSP cache corruption in daemon RAM serves stale results | Invariant 5 (E2E 3.7.E) catches this; `lsp_invalidate` op for forced eviction |
| **MEDIUM** | SQLite WAL files (`ledger.sqlite3-wal`, `-shm`) not cleaned on dispose | Document; harmless if state dir is wiped by `dispose_sandbox` |
| **MEDIUM** | jedi subprocess leak in daemon → slow growth | Worker pool sized to `min(4, cpu_count)`; recycle workers after N requests |
| **MEDIUM** | Daemon serialization of complex dataclasses fails (e.g. `EditResult.timings` is `dict[str, float]`) | `_register_dataclass` table; round-trip test for every type |
| **LOW** | Telemetry counters (`overlay_counters`) divergent — daemon-side counters not visible to orchestrator-side `record_overlay_op` | `status()` op returns counter snapshot from daemon; orchestrator sums for cross-sandbox aggregation |
| **LOW** | Daemon's bundled `code_intelligence` includes dead remote branches in memory | Harmless until Phase 5 cleanup deletes them; daemon `sandbox=None` ensures they're never invoked |

## Hand-off to Phase 3.5

Phase 3.5 picks up with:
- All mutations and the overlay-commit half running inside the daemon via package reuse.
- 5-invariant correctness proven; bypass guard live.
- A baseline for `apply_edits`, `query_symbols` latency under single-call workloads.
- Per-file `refresh()` rewriting the entire pickle index — Phase 3.5 fixes by SQLite migration.
- No sustained-load testing yet — Phase 3.5 adds it before the high-volume `svc.cmd` lands in Phase 4.
