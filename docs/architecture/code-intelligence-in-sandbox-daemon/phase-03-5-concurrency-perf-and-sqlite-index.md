# Phase 3.5 — Concurrency/perf E2E suite + SQLite-backed index storage

**Estimated effort:** 5 days (3 days engineering + 2 days E2E)
**Risk profile:** MEDIUM-HIGH — discovers daemon stability issues under sustained load BEFORE the highest-volume hot path (`svc.cmd`) ships in Phase 4
**Status:** Not started
**Blocks on:** Phase 3 complete

## Goal

Two deliverables:

1. **Concurrency / sustained-load E2E suite.** Stress the daemon with realistic high-volume workloads, collect p50/p95/p99 timing distributions, and assert resource-usage ceilings (RSS memory, file descriptors, open SQLite handles). Catches leaks, contention pathologies, and degradation under load before they reach production via Phase 4's `svc.cmd` hot path.
2. **SQLite-backed index storage.** Replace Phase 1's pickle `index.snapshot` with `index.sqlite3` so `refresh(file_path)` updates a single row instead of rewriting the entire blob. Enables incremental query optimization later (e.g. `WHERE name LIKE` instead of in-memory scan).

## Why between Phase 3 and 4

Three reasons:

1. **Catch perf regressions before Phase 4 ships them to the hot path.** Phase 4 routes `svc.cmd` through the daemon — the most-used CodeAct primitive. If the daemon has a memory leak, FD leak, or contention pathology under sustained load, Phase 4 puts it in front of every user. Phase 3.5 is the safety net.
2. **The OCC engine is now in the daemon (Phase 3) but only single-call-tested.** The 5-invariant E2E in Phase 3 used at most 2 concurrent ops. Real production load is N concurrent agents each looping `query + edit + cmd`. Phase 3.5 simulates that load.
3. **Pickle `index.snapshot` is now visibly suboptimal.** Phase 1 chose pickle for delivery speed. By Phase 3.5 we have `LedgerStore` proving SQLite works in this codebase; migrating the index to SQLite is small work and unblocks per-file invalidation efficiency.

## What ships

| Artifact | File | Purpose |
|---|---|---|
| `IndexStore` SQLite adapter | `backend/src/sandbox/code_intelligence/daemon/storage.py` (extended) | `index.sqlite3` table with one row per file; `query_by_substring`, `refresh_file`, `delete_file`, `bulk_replace` |
| Daemon index migration | `backend/src/sandbox/code_intelligence/daemon/server.py` (modified) | Writes to SQLite via `IndexStore.bulk_replace` instead of pickle |
| Daemon `query_symbols` swap | `backend/src/sandbox/code_intelligence/daemon/server.py` (modified) | Reads from `IndexStore.query_by_substring` instead of in-memory `_symbols` dict |
| Pickle → SQLite migration | `backend/src/sandbox/code_intelligence/daemon/storage.py` (one-shot helper) | If `index.snapshot` exists at startup, drain into `index.sqlite3` and unlink the pickle |
| `TimingHarness.step_repeat()` | `backend/tests/test_e2e/_timing_harness.py` (extended) | Collects N samples, reports p50/p95/p99 |
| Resource sampler | `backend/tests/test_e2e/_timing_harness.py` (extended) | `harness.sample_rss(label)` and `harness.sample_fds(label)` via `transport.exec` reading `/proc/<pid>/status`, `/proc/<pid>/fd/` |
| Phase 3.5 live E2E | `backend/tests/test_e2e/test_live_ci_phase3_5_concurrent_perf.py` | Sustained mixed workload, p50/p95/p99, RSS/FD ceilings |
| Multi-orchestrator E2E | (same file) | Two `DaemonBackend` instances against the same daemon; verify lock arbitration |
| Index storage unit tests | `backend/tests/test_sandbox/test_code_intelligence/test_storage_index.py` | SQLite schema, migration path, query parity with pickle baseline |

## Detailed task list

### Task 3.5.1 — `IndexStore` SQLite adapter

**File:** `backend/src/sandbox/code_intelligence/daemon/storage.py` (extended)

**Schema:**

```sql
CREATE TABLE IF NOT EXISTS index_files (
    file_path TEXT PRIMARY KEY,
    generation INTEGER NOT NULL DEFAULT 0,
    indexed_at REAL NOT NULL,
    -- One row per file; symbols stored as msgpack blob to keep schema simple.
    -- Migration path to per-symbol rows is open if needed for advanced queries.
    symbols_blob BLOB NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_index_files_generation ON index_files(generation);

-- Computed view for fast substring queries; the symbols_blob is unpacked to fill this.
-- Phase 3.5 keeps it as an in-memory cache; future work could materialize.
```

**API:**

```python
class IndexStore:
    def __init__(self, state_dir: Path) -> None: ...

    def bulk_replace(self, snapshot: dict[str, list[SymbolInfo]]) -> None:
        """Atomic full replacement: BEGIN; DELETE; INSERT…; COMMIT;"""

    def refresh_file(self, file_path: str, symbols: list[SymbolInfo]) -> int:
        """INSERT OR REPLACE single row; returns new generation."""

    def delete_file(self, file_path: str) -> int:
        """DELETE single row; returns new generation."""

    def query_by_substring(self, needle: str) -> list[SymbolInfo]:
        """Naive scan for Phase 3.5; future task: materialize per-symbol rows
        for index-based lookup. Today's in-memory query is also linear, so we
        stay parity-preserving."""

    def file_symbols(self, file_path: str) -> list[SymbolInfo]:
        """Return symbols for one file via PK lookup."""

    def indexed_paths(self) -> list[str]:
        """SELECT file_path FROM index_files ORDER BY file_path."""

    def all_symbols(self) -> Iterator[SymbolInfo]:
        """Generator over every symbol; used by `size` property and full-scan queries."""
```

**Pragmas:** same as ledger (WAL, NORMAL synchronous, 64MB mmap).

**Concurrency:** WAL handles concurrent readers fine. Write lock per-process (asyncio Lock); SQLite serializes anyway.

### Task 3.5.2 — Pickle → SQLite migration helper

**File:** `backend/src/sandbox/code_intelligence/daemon/storage.py`

```python
def migrate_pickle_to_sqlite(state: Path) -> None:
    """One-shot migration: if index.snapshot exists, drain into index.sqlite3 then unlink.
    Idempotent: if neither exists, no-op. If both exist (interrupted migration), pickle wins."""
    pickle_path = state / "index.snapshot"
    sqlite_path = state / "index.sqlite3"
    if not pickle_path.exists():
        return  # Nothing to migrate

    snapshot = read_snapshot(state, "index.snapshot")
    if snapshot is None:
        # Corrupt pickle — already unlinked by read_snapshot
        return

    store = IndexStore(state_dir=state)
    store.bulk_replace(snapshot)
    pickle_path.unlink(missing_ok=True)
    logging.info("storage: migrated %d files from pickle to sqlite", len(snapshot))
```

Called once at daemon startup (in `run_daemon`, before `CodeIntelligenceService` construction).

### Task 3.5.3 — Daemon swaps `query_symbols` to read from `IndexStore`

**File:** `backend/src/sandbox/code_intelligence/daemon/server.py`

**Two implementation choices:**

- **(A) Inject `IndexStore` into `SymbolIndex`.** Modify `sandbox/code_intelligence/indexing/symbol_index.py` to accept an optional persistence backend. Daemon constructs `SymbolIndex(persistence=IndexStore(state))`. Cleaner; benefits the orchestrator too.
- **(B) Daemon-side override.** `handle_query_symbols` reads from `IndexStore` directly, bypassing `_DAEMON_STATE.svc.symbol_index`. Faster to ship, but breaks the package-reuse principle from Phase 3.

**Recommendation: (A).** Adds an optional `persistence` kwarg to `SymbolIndex.__init__`; default `None` keeps today's behavior. Daemon passes `IndexStore`; in-process backend keeps the in-memory cache. ~30 LOC change in `symbol_index.py`.

```python
# symbol_index.py minimal change
class SymbolIndex:
    def __init__(self, ..., persistence: IndexStore | None = None) -> None:
        ...
        self._persistence = persistence

    def refresh(self, file_path, content=None) -> int:
        gen = self._refresh_in_memory(...)
        if self._persistence is not None:
            self._persistence.refresh_file(file_path, self._symbols[file_path].symbols)
        return gen

    def find(self, query, kind=None):
        if self._persistence is not None:
            return self._persistence.query_by_substring(query)
        # ... existing in-memory path ...
```

Drift guard: a parity test (`test_symbol_index_persistence_parity.py`) constructs both backends, applies the same edits, and asserts identical query results.

### Task 3.5.4 — `TimingHarness.step_repeat()` extension

**File:** `backend/tests/test_e2e/_timing_harness.py` (extended from Phase 0)

```python
class TimingHarness:
    def step_repeat(self, name: str, n: int = 100) -> Iterator[Iterator[None]]:
        """Yields n step() context managers under the same `name`. Records the full
        distribution; `report()` shows p50/p95/p99/min/max."""
        # Implementation: maintain self._distributions[name] as a list[float]

    def sample_rss_mb(self, label: str, transport, sandbox_id: str, pid: int) -> float:
        """One sample of RSS memory in MB, recorded under `label`.
        Reads /proc/<pid>/status remotely via transport.exec."""

    def sample_fds(self, label: str, transport, sandbox_id: str, pid: int) -> int:
        """One sample of open FD count via `ls /proc/<pid>/fd | wc -l`."""
```

**Report format extension:**

```
=== Phase 3.5 sustained_load timing breakdown ===
write_file:       p50=0.045s p95=0.061s p99=0.084s   (200 samples)
query_symbols:    p50=0.008s p95=0.012s p99=0.022s   (200 samples)
svc_cmd:          p50=0.612s p95=0.745s p99=0.901s   (50 samples)
--- RESOURCE SAMPLES ---
rss_at_start:     127.4 MB
rss_at_50%:       142.1 MB  (+14.7 MB)
rss_at_100%:      144.8 MB  (+17.4 MB)  ← bounded
fds_at_start:     34
fds_at_50%:       42
fds_at_100%:      40        ← no leak
```

### Task 3.5.5 — Phase 3.5 live E2E

**File:** `backend/tests/test_e2e/test_live_ci_phase3_5_concurrent_perf.py`

#### 3.5.5.A — Sustained mixed workload (200 ops)

```python
async def test_sustained_mixed_workload_distribution(live_sweevo_env):
    """200 mixed ops at moderate concurrency. Catches latency tail growth."""
    h = TimingHarness(phase=3.5, test_name="sustained_mixed_workload")
    env = live_sweevo_env
    svc = env.make_ci_service_flag_on()

    # Daemon PID for resource sampling
    pid = int(env.exec(f"cat $HOME/.cache/eos-ci/{wh()}/v1/daemon.pid")[1].strip())

    h.sample_rss_mb("rss_at_start", env.transport, env.sandbox_id, pid)
    h.sample_fds("fds_at_start", env.transport, env.sandbox_id, pid)

    # 100 writes
    for step in h.step_repeat("write_file", n=100):
        with step:
            i = next_counter()
            svc.write_file([WriteSpec(file_path=f"/testbed/_phase3_5_w{i}.txt",
                                       content=f"v{i}", overwrite=True)])

    h.sample_rss_mb("rss_at_50%", env.transport, env.sandbox_id, pid)
    h.sample_fds("fds_at_50%", env.transport, env.sandbox_id, pid)

    # 100 queries interleaved with 50 reads
    for step in h.step_repeat("query_symbols", n=100):
        with step:
            svc.query_symbols("Bag")

    for step in h.step_repeat("read_via_status", n=50):
        with step:
            svc.status()

    h.sample_rss_mb("rss_at_100%", env.transport, env.sandbox_id, pid)
    h.sample_fds("fds_at_100%", env.transport, env.sandbox_id, pid)

    # Resource ceilings
    rss_growth = h.values["rss_at_100%"] - h.values["rss_at_start"]
    assert rss_growth < 100.0, f"RSS grew {rss_growth:.1f} MB during 250 ops — possible leak"

    fd_growth = h.values["fds_at_100%"] - h.values["fds_at_start"]
    assert fd_growth < 10, f"FD count grew by {fd_growth} during 250 ops — possible leak"

    # Latency tail not pathological
    assert h.distributions["write_file"]["p99"] < 5 * h.distributions["write_file"]["p50"], \
        "p99 > 5x p50 indicates contention pathology"

    print(h.report())
    print(h.compare_to(latest_phase0_baseline()))
    h.dump_json()
```

#### 3.5.5.B — Concurrent agents (8 simulated CodeAct loops)

```python
async def test_concurrent_agents_no_pathologies(live_sweevo_env):
    """8 concurrent 'agents' each looping query + edit + svc.cmd for 30s.
    Catches cross-handler contention and lock starvation."""
    h = TimingHarness(phase=3.5, test_name="concurrent_agents_8x")
    env = live_sweevo_env
    svc = env.make_ci_service_flag_on()

    pid = int(env.exec(f"cat $HOME/.cache/eos-ci/{wh()}/v1/daemon.pid")[1].strip())
    h.sample_rss_mb("rss_at_start", env.transport, env.sandbox_id, pid)

    stop_at = time.time() + 30.0
    op_counts = {"query": 0, "edit": 0, "cmd": 0}
    errors = []

    async def agent(agent_id: int):
        while time.time() < stop_at:
            try:
                # Query
                svc.query_symbols("Bag")
                op_counts["query"] += 1
                # Edit
                target = f"/testbed/_phase3_5_agent{agent_id}_{op_counts['edit']}.txt"
                svc.write_file([WriteSpec(file_path=target, content="x", overwrite=True)])
                op_counts["edit"] += 1
                # cmd (small)
                await svc.cmd(env.raw_sandbox, "true")
                op_counts["cmd"] += 1
            except Exception as exc:
                errors.append((agent_id, str(exc)))

    with h.step("agents_30s_8way"):
        await asyncio.gather(*[agent(i) for i in range(8)])

    h.sample_rss_mb("rss_at_end", env.transport, env.sandbox_id, pid)

    print(f"Op counts: {op_counts}")
    print(f"Errors: {len(errors)}")

    assert len(errors) == 0, f"errors during sustained agents: {errors[:5]}"
    assert all(c > 50 for c in op_counts.values()), "agent throughput too low"

    rss_growth = h.values["rss_at_end"] - h.values["rss_at_start"]
    assert rss_growth < 200.0, f"RSS grew {rss_growth:.1f} MB during 8-agent 30s run"

    print(h.report())
    h.dump_json()
```

#### 3.5.5.C — Multi-orchestrator → single-daemon arbitration

```python
async def test_multi_orchestrator_single_daemon_arbitration(live_sweevo_env):
    """Two DaemonBackend instances simulate two orchestrator processes hitting
    the same daemon. Lock arbitration must be consistent."""
    h = TimingHarness(phase=3.5, test_name="multi_orchestrator")
    env = live_sweevo_env

    daemon_a = DaemonBackend(env.transport, env.sandbox_id, env.repo_dir)
    daemon_b = DaemonBackend(env.transport, env.sandbox_id, env.repo_dir)

    target = "/testbed/_phase3_5_multi.txt"
    env.exec(f"echo 'v0' > {target}")

    # Both clients try to commit to the same file in parallel
    with h.step("two_daemon_backends_concurrent_commit"):
        results = await asyncio.gather(
            daemon_a._call_daemon_command("commit_operation_against_base", {
                "changes": [{"file_path": target, "base_content": "v0\n",
                             "base_hash": content_hash("v0\n"), "final_content": "vA\n",
                             "base_existed": True, "strict_base": True}],
                "edit_type": "write_file", "agent_id": "daemon_a"}),
            daemon_b._call_daemon_command("commit_operation_against_base", {
                "changes": [{"file_path": target, "base_content": "v0\n",
                             "base_hash": content_hash("v0\n"), "final_content": "vB\n",
                             "base_existed": True, "strict_base": True}],
                "edit_type": "write_file", "agent_id": "daemon_b"}),
        )

    # Exactly one must succeed (single-point lock arbitration)
    successes = sum(1 for r in results if r["success"])
    aborts = sum(1 for r in results if r["status"] == "aborted_version")
    assert successes == 1 and aborts == 1, f"expected 1 success + 1 abort, got {results}"

    print(h.report())
    h.dump_json()
```

#### 3.5.5.D — Index SQLite migration parity

```python
async def test_sqlite_index_parity_with_pickle(live_sweevo_env):
    """Force Phase 1 pickle path; capture symbol counts. Then trigger Phase 3.5
    SQLite migration; assert identical results."""
    h = TimingHarness(phase=3.5, test_name="sqlite_index_parity")
    env = live_sweevo_env
    svc = env.make_ci_service_flag_on()
    svc.ensure_initialized(wait=True)

    with h.step("query_via_pickle_or_sqlite"):
        baseline_results = svc.query_symbols("Bag")
    baseline_count = len(baseline_results)

    # Force migration (cleanly: stop daemon, delete sqlite, start with pickle present)
    env.exec(f"kill -TERM $(cat $HOME/.cache/eos-ci/{wh()}/v1/daemon.pid)")
    await asyncio.sleep(0.5)
    env.exec(f"rm -f $HOME/.cache/eos-ci/{wh()}/v1/index.sqlite3")

    # Need a pickle to migrate from — write one synthesized
    env.exec(f"... regenerate index.snapshot via daemon indexer legacy mode ...")

    # Restart daemon — should auto-migrate
    with h.step("daemon_restart_with_migration"):
        svc2 = env.make_ci_service_flag_on()
        svc2.ensure_initialized(wait=True)

    with h.step("query_post_migration"):
        post_results = svc2.query_symbols("Bag")

    assert len(post_results) == baseline_count
    assert sorted(s.name for s in post_results) == sorted(s.name for s in baseline_results)

    # Pickle should be unlinked after migration
    code, _ = env.exec(f"test -f $HOME/.cache/eos-ci/{wh()}/v1/index.snapshot")
    assert code != 0, "pickle not unlinked after migration"

    print(h.report())
    h.dump_json()
```

#### 3.5.5.E — Per-file refresh efficiency

```python
async def test_refresh_file_does_not_rewrite_world(live_sweevo_env):
    """Phase 1 pickle rewrote the entire snapshot on every refresh.
    Phase 3.5 SQLite must touch only one row."""
    h = TimingHarness(phase=3.5, test_name="refresh_efficiency")
    env = live_sweevo_env
    svc = env.make_ci_service_flag_on()
    svc.ensure_initialized(wait=True)

    target = "/testbed/dask/__init__.py"  # large file, lots of symbols

    for step in h.step_repeat("refresh_file", n=20):
        with step:
            svc.symbol_index.refresh(target)

    # p99 should be sub-100ms; pickle baseline was several seconds for a 1k-file repo
    p99 = h.distributions["refresh_file"]["p99"]
    assert p99 < 0.1, f"refresh_file p99 ({p99:.3f}s) — SQLite per-file write should be much faster"

    print(h.report())
    h.dump_json()
```

**Run command:** `uv run pytest backend/tests/test_e2e/test_live_ci_phase3_5_concurrent_perf.py -m live -v -s`

### Task 3.5.6 — Index storage unit tests

**File:** `backend/tests/test_sandbox/test_code_intelligence/test_storage_index.py`

**Cases:**
- `IndexStore.bulk_replace` is atomic — interrupted commit doesn't leave partial rows.
- `refresh_file` updates a single PK; bumps generation correctly.
- `delete_file` removes; subsequent `file_symbols` returns `[]`.
- `query_by_substring` parity with in-memory linear scan on a 100-symbol fixture.
- `migrate_pickle_to_sqlite` is idempotent; only-pickle → migrates, only-sqlite → no-op, both → pickle wins (interrupted-migration recovery).
- Concurrent reads (10 threads) succeed.

### Task 3.5.7 — Regression check

- `.venv/bin/pytest backend/tests/test_sandbox/ backend/tests/test_tools/ -q` — green.
- Re-run Phases 0, 1, 2, 3 live E2Es.
- Run `test_symbol_index_persistence_parity.py` — in-memory and SQLite-backed `SymbolIndex` produce identical results.

## Definition of done

- [ ] `IndexStore` SQLite adapter ships with documented schema and pragmas.
- [ ] `migrate_pickle_to_sqlite` runs at daemon startup; idempotent; correct interrupted-migration recovery.
- [ ] `SymbolIndex` accepts optional `persistence: IndexStore | None`; daemon passes it.
- [ ] Parity test `test_symbol_index_persistence_parity.py` green.
- [ ] `TimingHarness.step_repeat()` and `sample_rss_mb`/`sample_fds` extensions ship.
- [ ] **Phase 3.5 live E2E (5 subtests A-E) passes against `dask__dask_2023.3.2_2023.4.0`.**
- [ ] **Resource ceilings: RSS growth < 100MB during 250 ops; FD growth < 10.**
- [ ] **Latency tail healthy: write_file p99 < 5x p50.**
- [ ] **Multi-orchestrator: exactly 1 success + 1 abort under concurrent commit (3.5.5.C).**
- [ ] **Per-file refresh: p99 < 100ms (3.5.5.E) — proves SQLite per-row vs pickle full-rewrite win.**
- [ ] Sustained 8-agent 30s loop produces zero errors and >50 ops per kind per agent.
- [ ] Regression check: Phases 0, 1, 2, 3 E2Es + full unit suite green.
- [ ] PR description includes: full p50/p95/p99 distribution table, RSS/FD samples, multi-orchestrator log.

## Risk callouts (Phase 3.5 specific)

| Severity | Risk | Mitigation |
|---|---|---|
| **HIGH** | Daemon RSS leak under sustained load → eventual OOM | Explicit ceiling check in 3.5.5.A; RSS sampled at start/50%/100%; if grown by >100MB, fail loud and investigate via daemon log |
| **HIGH** | FD leak (e.g. unclosed sqlite cursors, zombie jedi subprocesses) | FD count sampled; ceiling at +10 vs start; investigate via `ls -la /proc/<pid>/fd/` if exceeded |
| **HIGH** | Latency tail (p99) blows up under contention | Explicit p99 < 5×p50 assertion; if exceeded, indicates a contention pathology (likely lock waits in `Arbiter`) |
| **HIGH** | SQLite migration corrupts the index | Migration is idempotent + integrity-checked; both old pickle and new sqlite kept until migration succeeds; pickle unlinked only after `bulk_replace` returns |
| **MEDIUM** | jedi subprocess pool grows unboundedly | Worker pool capped (Phase 3 risk callout); recycle after N requests; verify in 3.5.5.B 8-agent run |
| **MEDIUM** | SQLite WAL files (`-wal`, `-shm`) grow large under sustained writes | `PRAGMA wal_checkpoint(PASSIVE)` periodically; alternative: `PRAGMA journal_size_limit` |
| **MEDIUM** | Multi-orchestrator test catches a real bug in single-point arbitration | Fix is the entire goal — but expect surprises since today's tests never exercised this |
| **LOW** | `step_repeat` adds overhead that masks real timings | Subtract harness overhead (`time.perf_counter` measurement noise is ~µs) — negligible at the seconds-scale being measured |

## Hand-off to Phase 4

Phase 4 picks up with:
- Daemon stability under sustained load proven.
- Per-file refresh efficient (SQLite-backed).
- Multi-orchestrator arbitration verified.
- Resource ceilings established as production guardrails.
- A perf safety net so Phase 4's `svc.cmd` hot-path migration can be evaluated against known-stable baselines.
- The `_timings/` directory now holds distribution data (`step_repeat`) that Phase 4 and 5 can reference for tail-latency comparisons.
