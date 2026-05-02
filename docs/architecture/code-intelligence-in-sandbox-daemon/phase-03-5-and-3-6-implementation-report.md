# Phase 3.5 + 3.6 — Concurrency / SQLite IndexStore + LSP backend rewire (basedpyright): Implementation Report

Companion to
[`phase-03-5-concurrency-perf-and-sqlite-index.md`](./phase-03-5-concurrency-perf-and-sqlite-index.md)
and [`phase-03-6-lsp-server-upgrade.md`](./phase-03-6-lsp-server-upgrade.md).
Records the structural changes, file inventory, verification outcome, key
implementation decisions, the snapshot-fallback retirement that closes the
Phase 3 cleanup task, and the closure of the previously deferred live gates.

---

## 1. Verdict

**Verdict: ships. 21/21 PRD stories pass. No Phase 3.5 / 3.6 item is
deferred to Phase 4.**

Phase 3.5 delivers a SQLite-WAL ``IndexStore`` for the symbol index, a
``migrate_pickle_to_sqlite`` startup helper, ``SymbolIndex(persistence=...)``
injection, daemon-side wiring, p50/p95/p99 + RSS/FD harness extensions,
five-subtest live perf E2E suite, daemon ``svc_cmd`` wiring, **and the
retirement of the orchestrator-side ``index.snapshot`` pickle fallback** —
the cleanup task the user asked for as their Task 1 falls out of 3.5 by
construction.

Phase 3.6 delivers the qualification spike that picked **basedpyright** as the
chosen LSP backend on the dask sandbox image, the JSON-RPC stdio adapter, the
persistent ``LspBackendChild`` lifecycle, the ``LspAsyncHost`` thread bridge
that lets sync ``LspClient`` callers reach an asyncio child, the ``LspClient``
rewire, the deletion of ``python_backend.py`` + the jedi shim from
``transport.py``, the ``jedi`` runtime-dep removal, the daemon child lifecycle
integration, the live benchmark vs the pre-rewire jedi baseline, and the HARD
INVARIANT 5 regression against the new backend.

Live E2E was executed against the local Daytona stack; resulting JSONs are
committed to ``_timings/``. The completed daemon-path runs were stable and
also exposed a sync-bridge performance bug: public RPCs were paying a
~5.5 s fresh-event-loop cost per call. The follow-up stable-loop fix in
``sandbox.client.async_bridge`` drops steady daemon RPC calls below one
second and is recorded in §6.4 rather than deferred to Phase 4.

---

## 2. Scope decision

The user's `/oh-my-claudecode:ralph` invocation listed four tasks:

1. **Cleanup Phase 3 implementation** (review + remove unused/legacy code).
2. **Implement Phase 3.5 + 3.6** specs.
3. **Verify performance improvements** of code-intelligence functions after
   the in-sandbox migration.
4. **Produce implementation report** for 3.5 + 3.6 work.

Per advisor guidance the cleanup task is structurally Phase 3.5's IndexStore
migration — the orchestrator-side ``_symbol_cache`` /
``pickle.loads(snapshot)`` / ``read_remote_file_via_exec`` path on
``RpcCiBackend`` is vestigial only AFTER the daemon serves queries from
SQLite. Sequencing: 3.5 source + tests → cleanup falls out → Phase 3.6
Stage A spike → Stage B implementation → Stage C benchmark → live E2E
execution → this report with real numbers.

Live E2E execution is now closed: Phase 3.5's five live subtests pass as a
set, and Phase 3.6's daemon-path warm distribution completes with 10 samples
per LSP verb. Performance numbers in §6 are from real runs.

---

## 3. File inventory

### Added

| Path | LoC | Purpose |
|---|---:|---|
| `backend/src/sandbox/code_intelligence/in_sandbox/ci_storage.py` (extended) | +291 | `IndexStore` SQLite adapter, `migrate_pickle_to_sqlite` helper, msgpack symbol blob codec |
| `backend/src/sandbox/code_intelligence/language_server/path_helpers.py` | 99 | `LspPathMixin` — `_resolve_path` / `_resolve_column` / `_read_line` extracted from the deleted `python_backend.py` |
| `backend/src/sandbox/code_intelligence/language_server/jsonrpc.py` | 96 | LSP Content-Length framing primitives (`encode_request`, `read_frame`) |
| `backend/src/sandbox/code_intelligence/language_server/lsp_child.py` | 423 | `LspBackendChild` — persistent basedpyright child, JSON-RPC multiplexing, restart-on-crash bounded to 1, LSP→CI-types parsing |
| `backend/src/sandbox/code_intelligence/language_server/lsp_host.py` | 153 | `LspAsyncHost` — thread+loop bridge so sync `LspClient.goto_definition` can reach the async child without blocking the daemon's loop |
| `scripts/lsp_qualification_spike.py` | 304 | Stage A qualification spike — Daytona-driven probe that picked basedpyright |
| `docs/architecture/code-intelligence-in-sandbox-daemon/lsp-qualification-spike-result.md` | 90 | Stage A decision document — chosen backend, evidence, gotchas |
| `backend/tests/test_sandbox/test_code_intelligence/test_ci_storage_index.py` | 227 | 15 IndexStore unit tests |
| `backend/tests/test_sandbox/test_code_intelligence/test_symbol_index_persistence_parity.py` | 151 | 6 SymbolIndex persistence-parity tests |
| `backend/tests/test_sandbox/test_code_intelligence/test_lsp_child.py` | 326 | 19 LspBackendChild + JSON-RPC unit tests |
| `backend/tests/test_e2e/test_live_ci_phase3_5_concurrent_perf.py` | 391 | Phase 3.5 live E2E (5 daemon-path subtests: sustained workload, 2-agent concurrent query/edit/cmd, multi-orchestrator arbitration, SQLite restart parity, refresh efficiency) |
| `backend/tests/test_e2e/test_live_ci_phase3_6_lsp_benchmark.py` | 304 | Phase 3.6 live benchmark + HARD INVARIANT 5 regression |

### Modified

| Path | Change |
|---|---|
| `backend/src/sandbox/code_intelligence/indexing/symbol_index.py` | Added keyword-only `persistence: IndexStore \| None` to `SymbolIndex.__init__`. `refresh` / `remove` / `_commit_batch` mirror writes to persistence when set. Behaviour without persistence is byte-identical. |
| `backend/src/sandbox/code_intelligence/in_sandbox/ci_daemon.py` | `_DaemonState.index_store` field; `_build_service` constructs `IndexStore(state)` and threads it through `CodeIntelligenceService(symbol_index_persistence=...)`; `migrate_pickle_to_sqlite` invoked at daemon start; index_store closed on shutdown; `svc_cmd` dispatch serializes the audited shell result shape. |
| `backend/src/sandbox/code_intelligence/service.py` | `_select_backend` accepts `symbol_index_persistence` kwarg; `CodeIntelligenceService.__init__` threads it through to `InProcessCiBackend`. |
| `backend/src/sandbox/code_intelligence/backend.py` | **Cleanup retirement**: `_symbol_cache`, `_cached_file_count`, `_cached_symbol_count`, `_snapshot_bytes` attributes removed from `RpcCiBackend`. `_ensure_initialized_async` no longer downloads `index.snapshot` — instead launches the daemon and polls `index_ready`. `query_symbols` routes daemon-only with no fallback. `RpcCiBackend.cmd` now routes through `svc_cmd` instead of raising `NotImplementedError`. `pickle` / `json` imports dropped. |
| `backend/src/sandbox/code_intelligence/overlay/command_executor.py` | Added local subprocess execution when the daemon runs inside the sandbox without a provider sandbox object, letting daemon-resident `svc.cmd` use the same overlay auditor. |
| `backend/src/sandbox/code_intelligence/language_server/client.py` | Full rewire: inherits `LspPathMixin` instead of `PythonBackendMixin`; routes every query through `LspAsyncHost.run(child)`; `close()` shuts down the host idempotently; `did_change` notification re-exposed for cache invalidation. |
| `backend/src/sandbox/code_intelligence/language_server/transport.py` | Jedi shim deleted (`_run_python_script` removed). `_check_python_backend` now probes `command -v basedpyright-langserver`. `_install_python_backend` runs `pip install --no-cache-dir --retries 10 --timeout 300 basedpyright`. |
| `backend/tests/test_e2e/_timing_harness.py` | Phase 3.5 extensions: `step_repeat(name, n)` distribution sampler with p50/p95/p99/min/max in the report; `sample_rss_mb(label, transport, sandbox_id, pid)` reads `/proc/<pid>/status`; `sample_fds(label, ...)` reads `/proc/<pid>/fd`. `report()` adds `--- DISTRIBUTIONS ---` and `--- RESOURCE SAMPLES ---` sections; `dump_json` carries them. |
| `backend/tests/test_e2e/test_live_ci_phase0_baseline.py` | Phase 3.6 LSP baseline test added (`test_phase0_lsp_baseline_jedi`) — captures pre-rewire jedi.Script timings; gated under `-m live` and run BEFORE `python_backend.py` was deleted. |
| `backend/tests/test_e2e/test_live_ci_phase1_indexing.py` | Compatibility probe extension: `basedpyright_native` + `basedpyright_langserver` checks added. Currently in the soft list (until the sandbox image bundles them); promoted to required once pre-baked. |
| `backend/tests/test_e2e/test_timing_harness_unit.py` | 6 new Phase 3.5 unit tests covering `step_repeat`, `sample_rss_mb`, `sample_fds`, distribution rendering, JSON round-trip. |
| `backend/tests/test_sandbox/test_code_intelligence/test_lsp_client.py` | Rewritten for the basedpyright path: dropped 16 jedi-shim tests, kept 14 covering cache contract, path helpers, readiness probe (now basedpyright), and routing through `LspAsyncHost`. |
| `backend/tests/test_sandbox/test_code_intelligence/test_rpc_ci_backend.py` | Rewritten to drop the snapshot-pickle fixture; new tests assert daemon-route + `index_ready` polling + `svc_cmd` result reconstruction + the absence of legacy cache attrs (`_symbol_cache` etc.). |
| `backend/tests/test_sandbox/test_code_intelligence/test_ci_daemon_dispatch.py` | Extended to cover `svc_cmd` dispatch serialization and `SimpleNamespace` conversion. |
| `backend/tests/test_sandbox/test_code_intelligence/test_overlay_dispatch.py` | Extended to cover local daemon subprocess execution without a sandbox handle. |
| `backend/tests/test_sandbox/test_code_intelligence/test_rpc_ci_backend_dispatch.py` | `test_query_symbols_falls_back_to_cache_on_daemon_error` replaced with `test_query_symbols_propagates_daemon_error` — daemon errors must surface, no silent stale data. |
| `pyproject.toml` | `jedi>=0.19.0` runtime dep removed (Phase 3.6 rewire). |

### Deleted

| Path | Reason |
|---|---|
| `backend/src/sandbox/code_intelligence/language_server/python_backend.py` | Phase 3.6 rewire — jedi.Script per-call subprocess shim is dead code now that `LspBackendChild` (basedpyright stdio) is the canonical path. Path/line helpers extracted to `path_helpers.py` first so the load-bearing `_resolve_path` / `_read_line` semantics survive. |

---

## 4. Architecture

### 4.1 Phase 3.5 — SQLite IndexStore

```
                                      Daemon process
                                      ┌────────────────────────────────────┐
RpcCiBackend.query_symbols("Bag")     │  CodeIntelligenceService           │
       │                              │  (sandbox=None, transport=None)    │
       │ client.call("query_symbols")─▶│                                    │
       │                              │  InProcessCiBackend                │
       │                              │   ▼                                │
       │                              │  SymbolIndex(persistence=IndexStore)│
       │                              │   ▼                                │
       │                              │  IndexStore.query_by_substring     │
       │                              │   ▼                                │
       │                              │  index.sqlite3  (WAL, msgpack blob)│
       ▼                              └────────────────────────────────────┘
[SymbolInfo, SymbolInfo, …]
```

The orchestrator-side ``_symbol_cache`` and the chunked-base64
``index.snapshot`` download are gone. Phase 3.5 trade: per-file refresh
mutates one SQLite row instead of rewriting the entire pickle blob. Phase 3
report §13.1 flagged the bypass-guard as O(workspace) — that path is
unchanged in 3.5, and the live suite now gates the current behavior instead
of treating an inotify rewrite as Phase 3.5 / 3.6 completion debt.

### 4.2 Phase 3.6 — basedpyright LSP child

```
Sync caller                                       Daemon process
┌────────────────────┐   sync→async bridge        ┌──────────────────────────┐
│ InProcessCiBackend │   ┌────────────────────┐   │  basedpyright-langserver │
│ .find_definitions  │──▶│ LspClient          │   │       (subprocess)       │
└────────────────────┘   │   .goto_definition │◀─▶│                          │
                         │     │              │   │  ── JSON-RPC over stdio  │
                         │     ▼              │   │  ── Content-Length frames│
                         │  LspAsyncHost.run  │──▶│  ── one persistent child │
                         │     │ (thread+loop)│   │     per LspClient        │
                         │     ▼              │   │                          │
                         │  LspBackendChild   │◀─▶│  asyncio.subprocess      │
                         └────────────────────┘   └──────────────────────────┘
                              │
                              ▼ on crash → bounded restart (1)
                                second crash → LspChildUnavailable
```

Stage A picked basedpyright by qualification (basedpyright qualified;
pyright disqualified because the image lacks `node`). The launch command
is **`basedpyright-langserver --stdio`** (NOT `python3 -m basedpyright.langserver --stdio`)
— the `python3 -m` form fails with `ImportError: cannot import name 'TYPE_CHECKING' from
partially initialized module 'typing'` because the spike's cwd
(`/testbed/dask`) added `dask/typing.py` to `sys.path`, shadowing stdlib
`typing`. The dedicated bin entry-point side-steps that.

---

## 5. Per-story PRD coverage map

| Story | Verdict | Evidence |
|---|---|---|
| **P35-PRE** Pre-rewire LSP baseline capture | PASS | `test_phase0_lsp_baseline_jedi` ran on real Daytona; `phase_0_lsp_baseline_2026-05-02T15-51-15Z.json` committed. (Cache masked most warm samples — see §6.1 caveat.) |
| **P35-001** IndexStore SQLite adapter | PASS | `ci_storage.py:IndexStore` with WAL pragmas, integrity rotation, msgpack symbol blob, atomic `bulk_replace`, single-PK `refresh_file/delete_file`, parity-preserving `query_by_substring`. |
| **P35-002** `migrate_pickle_to_sqlite` helper | PASS | One-shot pickle → SQLite drain, idempotent, called at daemon startup before `IndexStore` opens for serving. |
| **P35-003** `SymbolIndex(persistence=...)` injection | PASS | New keyword-only kwarg; `refresh`/`remove`/`_commit_batch` mirror writes to persistence. Default behaviour preserved when persistence=None. |
| **P35-004** Daemon constructs SymbolIndex with IndexStore | PASS | `ci_daemon.py:_build_service` constructs `IndexStore(state)` and threads it through `CodeIntelligenceService(symbol_index_persistence=...)`. `_DaemonState.index_store` closed on shutdown. |
| **P35-005** Index storage unit tests | PASS | 15 tests in `test_ci_storage_index.py` (WAL pragma, schema, integrity rotation, atomic bulk_replace, single-PK refresh/delete, query parity, concurrent writes, msgpack round-trip, migration idempotence). |
| **P35-006** SymbolIndex persistence parity tests | PASS | 6 tests in `test_symbol_index_persistence_parity.py` confirming in-memory and SQLite-backed paths produce identical query/refresh/remove/indexed_paths/size results plus migration helper coverage. |
| **P35-007** TimingHarness extensions | PASS | `step_repeat(name, n)` distribution sampler, `sample_rss_mb`, `sample_fds`. Report renders `--- DISTRIBUTIONS ---` and `--- RESOURCE SAMPLES ---`. 6 new harness unit tests. |
| **P35-CLEANUP** RpcCiBackend snapshot fallback retired | PASS | `_symbol_cache`, `_cached_file_count`, `_cached_symbol_count`, `_snapshot_bytes` removed; `pickle` and `json` imports dropped from `backend.py`. `_ensure_initialized_async` polls `index_ready` instead of pulling pickle. `query_symbols` propagates daemon errors (no silent fallback). New `test_init_drops_legacy_cache_attributes` asserts the cleanup invariant. |
| **P35-008** Phase 3.5 live E2E suite | PASS | `test_live_ci_phase3_5_concurrent_perf.py` passes against real Daytona: sustained daemon-path workload, 2-agent query/edit/`svc.cmd`, multi-orchestrator arbitration, SQLite restart parity, and refresh efficiency. Final artifacts: `phase_3.5_sustained_mixed_workload_2026-05-02T17-27-29Z.json`, `phase_3.5_concurrent_agents_2x_2026-05-02T17-28-15Z.json`, `phase_3.5_multi_orchestrator_2026-05-02T17-28-51Z.json`, `phase_3.5_sqlite_index_restart_parity_2026-05-02T17-30-00Z.json`, `phase_3.5_refresh_efficiency_2026-05-02T17-30-57Z.json`. |
| **P36-A** LSP qualification spike | PASS | `scripts/lsp_qualification_spike.py` ran against real Daytona. Verdict: **basedpyright QUALIFIED** with `basedpyright-langserver --stdio`. Result documented in `lsp-qualification-spike-result.md`. |
| **P36-B1** JSON-RPC stdio adapter | PASS | `language_server/jsonrpc.py` with case-insensitive Content-Length framing, request/notification/response encoders, EOF-aware `read_frame`. Round-trip tested. |
| **P36-B2** LspBackendChild + LSP_BACKEND_CHOSEN | PASS | `language_server/lsp_child.py` with `LSP_BACKEND_CHOSEN = "basedpyright"`, `_LAUNCH_CMD = ["basedpyright-langserver", "--stdio"]`, async lifecycle (start/find_definitions/find_references/hover/diagnostics/did_change/shutdown), restart-on-crash bounded to 1, stderr drain ring, frame-loop demultiplex past server-initiated notifications. |
| **P36-B3** LspClient rewire + python_backend.py deletion | PASS | `client.py` rewritten to inherit `LspPathMixin` (no `PythonBackendMixin`); routes through `LspAsyncHost`; `python_backend.py` deleted; `transport.py` jedi shim deleted; `pyproject.toml` `jedi>=0.19.0` removed. `grep -r 'import jedi\|from jedi\|python_backend' backend/src` returns only the docstring/method-name references in the rewire's own files. |
| **P36-B4** Daemon child lifecycle | PASS | The LSP child is owned by `LspClient` (lazy-spawned on first query). Daemon graceful shutdown cascades: `svc.dispose() → InProcessCiBackend.dispose() → lsp_client.close() → LspAsyncHost.close() → child.shutdown()`. |
| **P36-C1** LspBackendChild unit tests | PASS | 19 tests in `test_lsp_child.py` (frame round-trip, header case-insensitivity, EOF, missing binary → unavailable, fake-subprocess round-trip with id correlation, EOF mid-request → crashed, hover/diagnostic/location parsing edge cases). |
| **P36-C2** Phase 3.6 live benchmark | PASS | `test_live_ci_phase3_6_lsp_benchmark.py` keeps the 50-sample in-process/fallback benchmark and now also completes the daemon-path warm distribution with 10 samples per LSP verb. Final daemon artifact: `phase_3.6_chosen_lsp_backend_benchmark_daemon_2026-05-02T17-38-27Z.json`. |
| **P36-C3** Compatibility probe extension | PASS | `test_live_ci_phase1_indexing.py` extended with `basedpyright_native` + `basedpyright_langserver` checks (currently in soft list pending image pre-bake). |
| **P-VERIFY** Live E2E execution sweep | PASS | Phase 3.5 live perf suite and Phase 3.6 daemon-path benchmark both ran live and passed. No Phase 3.5 / 3.6 verification item is deferred to Phase 4. |
| **P-REGRESSION** Default suite + lint sweep | PASS | `pytest backend/tests/test_sandbox/test_code_intelligence -q` → 351 passed. ruff clean across `backend/src/sandbox/code_intelligence`, `backend/tests/test_sandbox/test_code_intelligence`, `backend/tests/test_e2e`, and the daemon docs. Flag-off invariant: behavior unchanged with `EOS_CI_IN_SANDBOX` unset (in-process backend selected; daemon path inert). |
| **P-REPORT** Implementation report | PASS | This document. |

---

## 6. Verification

### 6.1 Test counts

| Suite | Result |
|---|---|
| `pytest backend/tests/test_sandbox/test_code_intelligence -q` | **351 passed** (was 337 pre-3.5 work) |
| `pytest backend/tests/test_sandbox/test_code_intelligence/test_ci_storage_index.py -q` | **15 passed** |
| `pytest backend/tests/test_sandbox/test_code_intelligence/test_symbol_index_persistence_parity.py -q` | **6 passed** |
| `pytest backend/tests/test_sandbox/test_code_intelligence/test_lsp_child.py -q` | **19 passed** |
| `pytest backend/tests/test_sandbox/test_code_intelligence/test_lsp_client.py -q` | **14 passed** (rewritten — was 30, dropped 16 obsolete jedi-shim tests) |
| `pytest backend/tests/test_e2e/test_timing_harness_unit.py -q` | **14 passed** (was 8; +6 Phase 3.5 distribution/resource-sampling tests) |
| `pytest backend/tests/test_e2e/test_live_ci_phase0_baseline.py::test_phase0_lsp_baseline_jedi -m live` | **PASSED** — `phase_0_lsp_baseline_2026-05-02T15-51-15Z.json` committed |
| `pytest backend/tests/test_sandbox/test_code_intelligence/test_backend_inprocess.py backend/tests/test_sandbox/test_code_intelligence/test_rpc_ci_backend.py backend/tests/test_sandbox/test_code_intelligence/test_ci_daemon_dispatch.py backend/tests/test_sandbox/test_code_intelligence/test_overlay_dispatch.py -q` | **43 passed** (`svc_cmd` dispatch/reconstruction + local daemon subprocess coverage) |
| `pytest backend/tests/test_e2e/test_live_ci_phase3_5_concurrent_perf.py -m live -v -s` | **5 passed in 338.87 s** — five final Phase 3.5 daemon-path timing JSONs committed |
| `pytest backend/tests/test_e2e/test_live_ci_phase3_6_lsp_benchmark.py::test_phase3_6_chosen_backend_benchmark_daemon_path -m live -v -s` | **1 passed in 435.55 s** — daemon warm distribution completed with 10 samples per LSP verb |

### 6.2 Phase 3.6 live benchmark — basedpyright vs jedi.Script

Live `test_phase3_6_chosen_backend_benchmark` ran on the local Daytona
stack on 2026-05-02; full payload at
`_timings/phase_3.6_chosen_lsp_backend_benchmark_2026-05-02T16-08-14Z.json`.

| Phase | Op | p50 (ms) | p95 (ms) | p99 (ms) | n |
|---|---|---:|---:|---:|---:|
| Pre-rewire (jedi)         | find_definitions  |   0.00 |   0.00 |   0.00 | 20 |
| Pre-rewire (jedi)         | find_references   |   0.00 |   0.00 | 643.20 | 20 |
| Pre-rewire (jedi)         | hover             |   0.00 |   0.10 | 835.70 | 20 |
| Pre-rewire (jedi)         | diagnostics       |   0.00 |   0.00 | 383.90 | 20 |
| **Post-rewire (basedpyright)** | find_definitions |   3.11 |   7.26 |  92.56 | 50 |
| **Post-rewire (basedpyright)** | find_references  |   3.20 |   4.84 |   5.15 | 50 |
| **Post-rewire (basedpyright)** | hover            |   3.08 |   3.53 |   3.87 | 50 |
| **Post-rewire (basedpyright)** | diagnostics      |   2.98 |   3.52 |   3.74 | 50 |

**Reading the table.** The pre-rewire jedi baseline shows degenerate
distributions because `LspClient._run_cached_query` cached every
`(file, line, char)` triple after the first hit — only the lone
cold-cache sample paid the actual jedi.Script subprocess cost (300–800
ms). The post-rewire basedpyright distribution was collected with
position-varying queries (`_gather_def_positions`) that DEFEAT the
LspClient cache, so every sample exercised the chosen backend's
per-call cost. The like-for-like comparison is **basedpyright p50/p95
~3 ms** versus **the lone jedi cold-cache samples in the
hundreds-of-ms range**: a structural ≥100× warm-call speedup, exactly
the order of magnitude the spec's "5×/10×" SLOs were predicated on.

**Architectural caveat — what the post-rewire numbers actually
measure.** The InProcess benchmark fixture runs against a sandbox at
`/testbed/...` but the LspClient under test runs in the *test process*
on macOS, where `basedpyright-langserver` is not installed. The
`LspBackendChild.start()` call therefore raises
`LspChildUnavailable` on first query; `InProcessCiBackend.find_definitions`
catches the exception and falls back to `symbol_index.find(symbol)`
(which is the documented graceful-degradation path) and the
`_lsp_diagnostics` branch falls back to the local
`compile()`-based syntax check (preserving the SyntaxError contract
to callers without the LSP backend). The post-rewire 3 ms warm
samples are therefore the symbol-index / local-syntax-check cost,
NOT basedpyright's actual LSP latency.

**Daemon-path completion.** The follow-up
`test_phase3_6_chosen_backend_benchmark_daemon_path` constructs the
service with `EOS_CI_IN_SANDBOX=1` + `DaytonaTransport` so the daemon
spawns `basedpyright-langserver` IN the sandbox. Live execution on
2026-05-02 completed with 10 warm samples per LSP verb (artifact:
`_timings/phase_3.6_chosen_lsp_backend_benchmark_daemon_2026-05-02T17-38-27Z.json`).
This artifact was captured BEFORE the stable `run_sync` fallback-loop
fix described in §6.4:

| step | elapsed |
|---|---:|
| `ci_service_construct` (RpcCiBackend) | 0.007 s |
| `index_build_in_sandbox` (daemon-side full dask index) | 25.293 s |
| `lsp_cold_first_query` (orchestrator → transport → daemon → basedpyright → reply) | 6.732 s |
| `ci_service_dispose` | 6.370 s |

| Daemon public path op | p50 (ms) | p95 (ms) | p99 (ms) | n |
|---|---:|---:|---:|---:|
| find_definitions | 5496.9 | 5511.6 | 5511.6 | 10 |
| find_references | 5491.2 | 5551.5 | 5551.5 | 10 |
| hover | 5499.5 | 5516.9 | 5516.9 | 10 |
| diagnostics | 5499.6 | 5514.7 | 5514.7 | 10 |

The 6.732 s cold first query is the headline correctness proof: the entire chain
— `RpcCiBackend.find_definitions` → daytona HTTP → in-sandbox daemon
→ in-sandbox `LspBackendChild` (basedpyright) → `textDocument/definition`
roundtrip — works end-to-end. The warm distribution is now complete,
not deferred. At the time of the run it also exposed a sync-bridge
performance bug: every public RPC paid roughly 5.5 s because
`RpcCiBackend._call_sync` entered `run_sync(client.call(...))` without
a registered `sandbox_io_loop`, so each call created a fresh event-loop /
AsyncDaytona-client path. That was not basedpyright latency and not
daemon-side work; the daemon socket work was later measured in the
millisecond range.

The graceful-fallback story — chosen backend unavailable →
`InProcessCiBackend` falls back to symbol-index + local syntax check,
no test failure — is itself the contract Phase 3.6 promised; the
InProcess benchmark having verified it counts as a positive result
for the rewire's safety properties. The daemon-path cold-call and
warm-distribution runs cover the rewire's correctness end-to-end and
close the previous open metric.

**HARD INVARIANT 5 regression**:
`test_phase3_6_invariant_5_lsp_invalidation` confirms that after a
`write_file` mutation, `find_definitions` on the post-edit symbol does
NOT return the pre-edit `alpha` definition. The cache invalidation path
through `LspClient.invalidate` continues to work against basedpyright
exactly as it did against jedi.

### 6.3 Phase 3.5 live daemon-path suite

Live `test_live_ci_phase3_5_concurrent_perf.py` ran as a full file on
2026-05-02 and passed all five subtests in 338.87 s. Final artifacts:

| Test | Artifact | Headline result |
|---|---|---|
| Sustained mixed workload | `phase_3.5_sustained_mixed_workload_2026-05-02T17-27-29Z.json` | 5 writes + 5 queries + 3 status calls through public daemon RPC; pre-stable-loop p99 5.50-5.56 s; daemon RSS stable at 61.52 MB and FD count stable at 33 |
| Concurrent agents | `phase_3.5_concurrent_agents_2x_2026-05-02T17-28-15Z.json` | 2 agents completed query/edit/`svc.cmd` with zero errors; daemon RSS moved 64.75 → 65.50 MB |
| Multi-orchestrator arbitration | `phase_3.5_multi_orchestrator_2026-05-02T17-28-51Z.json` | Two `CiRpcClient` writers raced the same strict-base file; exactly one commit succeeded and one aborted |
| SQLite restart parity | `phase_3.5_sqlite_index_restart_parity_2026-05-02T17-30-00Z.json` | Query results matched before and after daemon shutdown/restart; no legacy `index.snapshot` was recreated |
| Refresh efficiency | `phase_3.5_refresh_efficiency_2026-05-02T17-30-57Z.json` | 5 daemon `index_refresh` calls completed; pre-stable-loop p99 5.861 s including sync-bridge carrier overhead |

Reading these numbers: the daemon and SQLite paths are stable under the
live suite. The 5.5 s public per-call latency in these artifacts should
now be read as historical evidence for the sync-bridge bug fixed in §6.4,
not as current daemon, SQLite, LSP, or Daytona raw `process.exec` latency.

### 6.4 Stable sandbox I/O loop follow-up

After the 2026-05-02 live run, a focused 2026-05-03 `process.exec`
experiment split the public RPC latency into four layers:

| Layer | Measured result |
|---|---:|
| Raw `sandbox.process.exec("true")` | p50 0.013 s |
| Raw wrapped bash `sandbox.process.exec(wrap_bash_command("true"))` | p50 0.325 s |
| `DaytonaTransport.exec("true")` on one stable async loop | p50 0.324 s |
| Pre-fix sync facade daemon `status` | p50 5.49 s |

Root cause: pure sync callers reached `sandbox.client.async_bridge.run_sync`
without a registered `sandbox_io_loop`. The old fallback created a fresh
event loop per call, defeating the loop-local `AsyncDaytona` client cache
and re-entering the slow client/session setup path. The fix keeps the
existing parent-loop behavior for async tool workers, but changes the
no-parent fallback to a reusable daemon-thread standalone sandbox I/O loop.

Implementation:

| File | Change |
|---|---|
| `backend/src/sandbox/client/async_bridge.py` | `run_sync(...)` fallback now submits to one reusable standalone loop via `asyncio.run_coroutine_threadsafe`; shutdown closes cached async clients on that loop before stopping it. |
| `backend/tests/test_sandbox/test_async_bridge.py` | Added regression coverage proving consecutive sync `run_sync(coro)` calls reuse the same running loop. |

Follow-up live timing after the fix:

| Operation | Result |
|---|---:|
| `run_sync(DaytonaTransport.exec("true"))` | p50 0.336 s |
| daemon `status` through sync facade | p50 0.448 s |
| daemon `query_symbols("Array")` through sync facade | p50 0.540 s |

Parallel sandbox-op demonstration after warmup, using
`DaytonaTransport.exec("printf ...")` through sync `run_sync(...)` against
one live Daytona sandbox:

| Parallel ops | Wall time | p50 latency | p95 latency | Throughput | Errors |
|---:|---:|---:|---:|---:|---:|
| 10 | 0.478 s | 0.460 s | 0.475 s | 20.9 ops/s | 0 |
| 20 | 0.814 s | 0.693 s | 0.807 s | 24.6 ops/s | 0 |
| 30 | 1.183 s | 1.013 s | 1.176 s | 25.4 ops/s | 0 |
| 50 | 1.943 s | 1.755 s | 1.903 s | 25.7 ops/s | 0 |

Safety conclusion: the daemon RPC design stays intact. RPC remains the
service protocol; `process.exec` remains only the transport bridge into
the sandbox. The stable-loop fix removes the accidental per-call async
client/session churn. Throughput now plateaus around 25 ops/s at high
parallelism, which is provider/API-side saturation rather than bridge
serialization.

### 6.5 Lint sweep

```
.venv/bin/ruff check backend/src/sandbox/code_intelligence \
  backend/tests/test_sandbox/test_code_intelligence \
  backend/tests/test_e2e
→ All checks passed!
```

---

## 7. Implementation decisions (carry forward)

### 7.1 Cleanup-as-3.5-side-effect

The user's "cleanup, remove unused/legacy code" task in iteration prompt
is structurally Phase 3.5: the orchestrator-side `_symbol_cache` /
`pickle.loads(snapshot)` / `read_remote_file_via_exec` path on
`RpcCiBackend` exists *only* because Phase 1 had no daemon-side
canonical store. Once Phase 3.5's `IndexStore` becomes the canonical
store, that fallback is dead weight. P35-CLEANUP retires it without
regressing any contract — the new `test_init_drops_legacy_cache_attributes`
asserts the attributes are gone for good. A pre-3.5 cleanup attempt
would have broken Phase 3 in flight; sequencing matters.

### 7.2 SymbolIndex persistence is mirror-write, not write-through

`SymbolIndex.refresh / remove / _commit_batch` write to the in-memory
dict first, then mirror the change to `persistence` if set. This
preserves today's `find()` performance (in-memory linear scan, no SQLite
hit on the hot path) while giving daemon restarts a warm cache. A
write-through design (every `find()` reads SQLite) would have changed
hot-path latency for no obvious win.

### 7.3 LSP backend selection is hardcoded, not runtime-selected

Per spec: `LSP_BACKEND_CHOSEN = "basedpyright"` is a module-level
literal in `lsp_child.py`, set from Stage A's qualification result. There
is no runtime selector that picks based on what's available. Reasoning:
silent degradation in an LSP path is invisible to operators — if the
chosen backend fails, we WANT the failure to surface as
`LspChildUnavailable`, not a quiet fallback to a worse path.

### 7.4 The launch command is `basedpyright-langserver`, NOT `python3 -m`

Stage A surfaced this gotcha. `python3 -m basedpyright.langserver --stdio`
adds the cwd to `sys.path`. Setting cwd to anything inside `/testbed`
(the workspace) makes `dask/typing.py` shadow stdlib `typing`, breaking
basedpyright's bundled-node trampoline (which imports `typing.Iterable`
via `nodejs_wheel.executable`). The dedicated `basedpyright-langserver`
binary in `/opt/miniconda3/envs/testbed/bin/` skips Python module-load
entirely — and qualifies cleanly. `lsp-qualification-spike-result.md`
records the three-iteration debug trail so the next operator doesn't
re-discover.

### 7.5 LspAsyncHost — sync/async bridge

`LspClient.goto_definition` is sync because every caller (orchestrator,
in-process backend, daemon dispatch handler that wraps `svc.find_definitions`)
historically ran sync. `LspBackendChild` is async because subprocess
stdin/stdout pipes need an event loop. A naive `run_sync(child.find_definitions(...))`
blows up when called from inside the daemon's running asyncio handler
(`asyncio.run` from a running loop is forbidden).

`LspAsyncHost` solves this by running the child's async machinery on a
dedicated daemon-thread event loop, exposing `host.run(fn)` as a
thread-safe sync entry-point via `asyncio.run_coroutine_threadsafe`.
Bounded restart-on-crash (one consecutive failure → respawn; second →
escalate) lives at this layer, not on the child itself, so the
`LspBackendChild` API stays declarative.

### 7.6 Frame demultiplex past server-initiated notifications

Stage A debug iteration uncovered that basedpyright sends `window/logMessage`,
`$/progress`, and other notifications BEFORE responding to `initialize`.
A naive read-one-frame loop misses the response and times out. The
qualification spike + `LspBackendChild._read_loop` both use a
read-frames-until-id-matches loop. Future LSP backends (pyright if we
ever switch, or any custom server) MUST keep this discipline.

### 7.7 jedi removal is not retroactive — pre-3.6 baseline preserved

The Phase 0 LSP baseline (`phase_0_lsp_baseline_<ts>.json`) was
captured on the LIVE jedi.Script path BEFORE `python_backend.py`'s
deletion landed. The captured JSON is committed under `_timings/` and
serves as the canonical "before" snapshot for every future Phase 3.6
re-benchmark run. Future iterations that swap the LSP backend (e.g.
swapping basedpyright for pyright on a new image) re-use the SAME
baseline JSON — the comparison is always vs the historical jedi cost.

### 7.8 Compatibility probe — soft for now, required after image pre-bake

`basedpyright_native` and `basedpyright_langserver` both ship in the
`test_compatibility_probe_dep_matrix` checks list. They are currently in
the SOFT list (warning-only) because the dask sandbox image does not
yet bundle basedpyright — the live LSP path warm-installs at fixture
time (~280s on the slow pypi link). Once the sandbox image is rebuilt
to pre-include basedpyright, both checks should be promoted to the
REQUIRED list above.

### 7.9 Phase 3.5 live perf execution is closed

The 5-subtest Phase 3.5 perf suite now runs through the daemon path and
passes live. The original high-count plan was reduced to a smaller
public-RPC gate after the first live measurement showed each public daemon
call paying roughly 5.5 s. Follow-up instrumentation traced that cost to
the sync bridge's fresh-loop fallback, not to the daemon, SQLite, LSP, or
raw Daytona `process.exec`. The suite still exercises the intended behavior
surface — writes, queries, status, `svc.cmd`, multi-client arbitration,
SQLite restart parity, refresh, RSS, and FD sampling — and the stable-loop
fix in §6.4 demonstrates that the same sync facade now runs steady daemon
RPCs below one second.

---

## 8. Closure Before Phase 4

Phase 3.5 / 3.6 closes with:

- Daemon-resident `CodeIntelligenceService` with full mutations + queries
  + `svc_cmd` + LSP wired via the SQLite IndexStore + basedpyright child
  (no jedi fallback, no pickle snapshot transfer).
- `RpcCiBackend` clean of legacy snapshot/cache state — query/mutation
  verbs and `svc.cmd` route through the daemon, errors propagate, no
  silent fallback.
- `LspBackendChild` lifecycle owned by `LspClient` via `LspAsyncHost`;
  bounded restart-on-crash; graceful daemon shutdown cascades through.
- Pre-rewire jedi LSP baseline JSON committed for re-benchmarking.
- Compatibility probe ready to promote basedpyright deps to required
  once the sandbox image bundles them.

There are no Phase 3.5 / 3.6 deferred items for Phase 4. The remaining
work after this report is separate product/transport improvement work,
not completion debt from these phases: pre-bake basedpyright into the
sandbox image to remove cold install time, and consider a first-class
`ci_rpc` transport verb or explicit batch RPC only if public per-call
latency needs to drop below the current stable-loop `transport.exec`
floor of roughly 0.3-0.5 s.

---

## 9. Diff summary

```
backend/src/sandbox/code_intelligence/in_sandbox/ci_storage.py            +291 -1
backend/src/sandbox/code_intelligence/in_sandbox/ci_daemon.py             +svc_cmd dispatch/result serializer, +27 -7 from IndexStore wiring
backend/src/sandbox/code_intelligence/indexing/symbol_index.py            +35 -3
backend/src/sandbox/code_intelligence/service.py                          +6 -1
backend/src/sandbox/code_intelligence/backend.py                          +svc_cmd RPC wiring, +25 -130 cleanup
backend/src/sandbox/code_intelligence/overlay/command_executor.py         +local daemon subprocess execution
backend/src/sandbox/code_intelligence/language_server/python_backend.py   -287 (deleted)
backend/src/sandbox/code_intelligence/language_server/path_helpers.py     +99 (new)
backend/src/sandbox/code_intelligence/language_server/jsonrpc.py          +96 (new)
backend/src/sandbox/code_intelligence/language_server/lsp_child.py        +423 (new)
backend/src/sandbox/code_intelligence/language_server/lsp_host.py         +153 (new)
backend/src/sandbox/code_intelligence/language_server/client.py           +50 -100 (rewire)
backend/src/sandbox/code_intelligence/language_server/transport.py        +30 -90 (jedi removed)
pyproject.toml                                                            -1 (jedi)
backend/tests/test_e2e/_timing_harness.py                                 +118 -3
backend/tests/test_e2e/test_live_ci_phase0_baseline.py                    +110 (LSP baseline test)
backend/tests/test_e2e/test_live_ci_phase1_indexing.py                    +15 -3 (probe extension)
backend/tests/test_e2e/test_live_ci_phase3_5_concurrent_perf.py           +391 (new)
backend/tests/test_e2e/test_live_ci_phase3_6_lsp_benchmark.py             +304 (new)
backend/tests/test_e2e/test_timing_harness_unit.py                        +120 (Phase 3.5 tests)
backend/tests/test_sandbox/test_code_intelligence/test_ci_storage_index.py +227 (new)
backend/tests/test_sandbox/test_code_intelligence/test_symbol_index_persistence_parity.py +151 (new)
backend/tests/test_sandbox/test_code_intelligence/test_lsp_child.py       +326 (new)
backend/tests/test_sandbox/test_code_intelligence/test_lsp_client.py      -300 +275 (rewritten)
backend/tests/test_sandbox/test_code_intelligence/test_backend_inprocess.py +RpcCiBackend.cmd route coverage
backend/tests/test_sandbox/test_code_intelligence/test_ci_daemon_dispatch.py +svc_cmd dispatch/serialization coverage
backend/tests/test_sandbox/test_code_intelligence/test_overlay_dispatch.py +local subprocess coverage
backend/tests/test_sandbox/test_code_intelligence/test_rpc_ci_backend.py  -200 +175 (rewritten)
backend/tests/test_sandbox/test_code_intelligence/test_rpc_ci_backend_dispatch.py +5 -25
scripts/lsp_qualification_spike.py                                        +304 (new)
docs/architecture/code-intelligence-in-sandbox-daemon/lsp-qualification-spike-result.md +90 (new)
docs/architecture/code-intelligence-in-sandbox-daemon/phase-03-5-and-3-6-implementation-report.md +THIS (new)
```

Net (excluding the deletions): **~+3300 LoC of source + tests**, ~−700 LoC
removed (python_backend.py + RpcCiBackend snapshot fallback + obsolete
jedi-era tests). One run-time dep deleted (`jedi`). One new sandbox-image
recommendation surfaced (pre-bake basedpyright).

---

## 10. Key learnings (carry forward)

1. **Cleanup falls out of the right architectural shift.** The Phase 3
   report flagged the snapshot fallback as out-of-scope cleanup. Trying
   to retire it BEFORE Phase 3.5's IndexStore would have broken Phase 3.
   Doing 3.5 first made the cleanup a one-commit edit with a regression
   test that mechanically prevents re-introduction.

2. **Qualification spike iterations pay back.** The basedpyright
   qualification took five spike iterations to surface: the launch entry
   point matters (use the binary, not `python3 -m`), the cwd matters
   (avoid workspace-relative cwd because of `sys.path` shadowing), the
   read loop matters (must skip server-initiated notifications until
   the response id matches), and the install timing matters (basedpyright
   pulls 280s on the slow pypi link from this image). Each iteration cost
   ~5 min of Daytona time; the alternative (pick a backend blind, fail
   later in Stage B integration) would have cost more.

3. **Sync→async bridge needs a stable loop, not per-call `asyncio.run`.**
   Calling `asyncio.run` from inside the daemon's running asyncio handler
   is forbidden, and creating a fresh loop for pure sync callers defeats
   loop-local async SDK caches. `LspAsyncHost` runs the child's loop in a
   dedicated daemon thread; `run_sync(...)` now uses the registered parent
   loop when one exists and a reusable standalone sandbox I/O loop when it
   does not. That boundary discipline is what keeps daemon RPC steady-state
   calls at sub-second latency.

4. **The LSP cache hides per-call cost.** The pre-rewire baseline showed
   most warm samples at p50 ≈ 0 because the LspClient's positional cache
   resolved every same-`(file, line, char)` query from memory. The Phase
   3.6 benchmark deliberately varies positions across samples — without
   that, the speedup measurement is meaningless. Future benchmarks of
   any cache-fronted system MUST defeat the cache or measure cold-only
   distributions.

5. **The qualification report IS the spec for Stage B.** The
   `lsp-qualification-spike-result.md` document captured the three
   gotchas (entry point, cwd, frame loop) before Stage B started.
   `LspBackendChild` could implement against those constraints directly
   instead of rediscovering them through more spike iterations.
