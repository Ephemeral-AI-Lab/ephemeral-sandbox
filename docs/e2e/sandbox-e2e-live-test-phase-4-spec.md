# `sandbox-e2e-live-test` — Phase 4 Spec, Stage 1 (Observability Snapshot Monitoring)

Implementation-ready spec for **Phase 4, Stage 1 only** of
`crates/sandbox-e2e-live-test`. This is the **author** spec in an
author→verifier discipline: build to green from this document without
re-deriving any design decision. It is **spec, not code** — do **not** implement
the crate from this file. **Live code is the source of truth**; every `file:line`
it relies on is in the *Anchor Ledger* (§8) with a `confirmed`/`corrected`
verdict, re-derived from the working tree (not from the parent spec's prose).

Parent design: `docs/e2e/sandbox-e2e-live-test-spec.md`
(`## Observability and Performance Monitoring`, `## Implementation Phases` →
*Phase 4*, `### Two-stage delivery during the runtime migration`). Phase map:
`docs/e2e/sandbox-e2e-live-test-phases-note.md` (`## Phase 4 — Observability
monitoring`, *Stage map per phase*). Prior phase: `sandbox-e2e-live-test-phase-3-spec.md`.

---

## 1. Phase boundary + Stage 1 statement

Phase 3 is **live and complete** in the working tree: `eos-e2e`
(`src/bin/eos-e2e.rs`) runs preflight → manifest → attach → `cargo test` →
aggregate → cleanup, and `report.rs` already carries
`write_exchange`/`write_result`/`write_summary`/`write_run_manifest` plus the
full `Summary`/`Timing`/`Counts`/`TestEntry` DTO set. Phase 4 Stage 1 adds
**read-only observability snapshot capture** on top of that pipeline, through the
**public manager operation only**.

Stage 1 adds exactly:

1. A new `report.rs` writer + DTOs for a per-sandbox
   `{run_root}/reports/{sandbox_id}/observability.json` artifact: the latest
   public observability-tree **node** for that sandbox, bounded recent-trace
   summaries, bounded resource samples, P1 cgroup detection, and source-call
   metadata + warnings.
2. A bounded **observability poller** in `eos-e2e` that runs **concurrently with
   the `cargo test` child** on a `std::thread` side-thread, calling
   `sandbox-cli manager get_observability_tree` on a fixed interval, keying nodes
   by the returned `sandbox_id`, and joining **before** cleanup removes
   `run_root`.
3. A non-gating `summary.observability` diagnostic sub-object on the existing
   `Summary` DTO (additive; Phase 3 fields untouched).

**Stage 1 statement.** Stage 1 drives **zero runtime CLI operations**. The
orchestrator's default `cargo test` target stays pinned to the manager binary
behind the existing `STAGE1_DEFAULT_TARGET` const (`src/bin/eos-e2e.rs:17`,
applied `:304`); Phase 4 Stage 1 **does not touch that constant**. The poller
issues only `manager get_observability_tree` (and nothing else). The sole skip
path remains `EOS_E2E_RUN_ROOT` unset (`tests/support/mod.rs:7-9`); Phase 4 adds
**no** runtime-readiness skip guard.

**Pass/fail is unchanged.** The run gate stays the Phase 3 gate: the `cargo test`
child exit code (captured at `src/bin/eos-e2e.rs:140`) **and** every
`result.json` status `== "passed"` (`src/bin/eos-e2e.rs:145-156`). Observability
capture is **diagnostic only**: every observability failure mode is a recorded
warning, never an exit-code change. Missing/absent/unavailable **P1** fields
**lower resolution, never fail** an otherwise-green manager run.

**Out of scope (named, not designed here).** Carry this scope fence verbatim.

| Out-of-scope | Where it lives |
|---|---|
| **P2 namespace queue-wait timing** — `enqueued_at_unix_ms`, `running_at_unix_ms`, derived `queue_wait_ms`. These fields **do not exist in live code** today (`namespace_execution.rs` records only `started_at_unix_ms`, `:50,:53`). | Stage 2 |
| **Runtime command traces** as a required signal (`recent_traces` namespace-execution rows). Pre-migration the tree carries little or no runtime-command activity. | Stage 2 |
| **R1 green proof; R2–R8/N2 runtime leaves; runtime assertion helpers** (`err_detail`/`offsets_monotonic`/`non_decreasing`); flipping `STAGE1_DEFAULT_TARGET` to the full suite. | Stage 2 |
| **Any internal SQLite / `*_for_test` observability reader.** Linking `sandbox-observability` or reading `store.rs` would break the black-box boundary. P1/P2 **unit** assertions live in the daemon crates, never here. | n/a (boundary law) |
| **Manager-side observability sink / second classification axis / forwarding-span trace store.** | permanently out (§5, parent decision 2) |
| **Spawn-mode gateway, Docker run-label orphan reaper, any Phase 3 cleanup redesign.** | Open Items #1/#2; unchanged |
| **`build.rs` / `tests/*` churn beyond the one optional DTO unit test (§8 design Q8).** No runtime test leaf is added. | n/a |

---

## 2. Resulting file/folder structure

`[EDITED]` = changed this phase; `[NEW]` = created this phase. Everything else is
**unchanged** by Phase 4 Stage 1.

```text
crates/sandbox-e2e-live-test/
  Cargo.toml                          unchanged (no new deps; std::thread only)
  build.rs                            unchanged
  src/
    lib.rs                            unchanged (report already pub)
    config.rs                         unchanged
    cli_client.rs                     unchanged (manager() + CallRecord reused as-is)
    fixtures.rs                       unchanged (per-sandbox result.json/exchange ownership intact)
    gateway.rs                        unchanged
    cleanup.rs                        unchanged (RunGuard teardown order intact)
    assertion.rs                      unchanged
    report.rs               [EDITED]  + ObservabilitySnapshot/Resources/RecentTrace/P1 DTOs;
                                        + write_observability(run_root, &ObservabilitySnapshot);
                                        + observability_node_from_tree(...) projection helpers;
                                        + ObservabilitySummary (summary.observability rollup);
                                        + OBSERVABILITY_SCHEMA_VERSION const;
                                        Summary gains one additive field `observability`.
    bin/
      eos-e2e.rs            [EDITED]  + observability poller side-thread (std::thread) spanning
                                        the cargo-test child; join before guard.teardown();
                                        fold ObservabilitySummary into the summary it already writes.
  tests/
    support/mod.rs                    unchanged
    observability_writer.rs  [NEW] (optional, design Q8) narrow unit test for the
                                        report.rs DTO projection over a synthetic tree Value;
                                        no Docker, no gateway, no runtime leaf.
```

`Cargo.toml` is **not edited**: the e2e crate already depends on `anyhow`,
`serde`, `serde_json`, `clap`, `sha2`, `time` (`Cargo.toml:16-21`), and the
poller uses only `std::thread`/`std::sync` + `std::time`. **No async runtime, no
new crate deps** (prefer less).

---

## 3. Orchestrator polling pipeline and lifecycle

### 3.1 Polling owner & lifecycle (design Q1)

**Decision: a single bounded side-thread, spawned by `eos-e2e`, that polls the
whole manager tree concurrently with the `cargo test` child, and is joined before
cleanup.** Rationale: snapshot capture must run **while sandboxes exist**.
Sandboxes live only for the duration of each `#[test]` — the per-test
`Sandbox::drop` issues `destroy_sandbox` (`fixtures.rs:157-159`) as each test
finishes, so a before/after-the-child pair would observe an **empty** tree (no
ready sandboxes) most of the time. The runner already owns thread-level
concurrency exclusively via `cargo test --test-threads` (`eos-e2e.rs:309`); the
poller is a single extra `std::thread`, not a fan-out, so it adds no scheduler.

The poller is owned by `run_pipeline` (`src/bin/eos-e2e.rs:88`) and threaded
through the existing run window:

```text
run_pipeline:
  ... write_run_manifest (eos-e2e.rs:104) ; RunGuard::new (eos-e2e.rs:111) ...
  gateway::await_ready (eos-e2e.rs:128)                         # socket reachable
  -- START POLLER -------------------------------------------------------------
  let stop = Arc<AtomicBool::new(false)>                        # std::sync::atomic
  let handle = std::thread::spawn(move || poll_loop(socket, run_root, stop, ...))
  let cargo_status = run_cargo_test(&config, &filters)          # eos-e2e.rs:140 (BLOCKS)
  -- STOP POLLER --------------------------------------------------------------
  stop.store(true, Ordering::Relaxed)
  let obs_summary = handle.join()...                            # JOIN before aggregate
  let tests = report::build_tests(&config.run_root)             # eos-e2e.rs:143 (unchanged)
  ... write_summary (eos-e2e.rs:197) ...
  guard.teardown() (eos-e2e.rs:200)                             # removes run_root per policy
```

**Pinned lifecycle parameters:**

| Knob | Value | Justification |
|---|---|---|
| Interval | `POLL_INTERVAL = 1000 ms` between poll cycles | recent traces age out of the bounded resource window; a 1 s cadence captures activity during fast manager tests without flooding the gateway. Fixed const, not a CLI knob (prefer less). |
| Per-CLI-call timeout | inherited from `CliClient` (no per-call timeout exists today; `cli_client.rs:64-70` blocks on `Command::output()`). Stage 1 adds **no** new timeout knob. | `CliClient` has no timeout field today; the manager op itself bounds fan-out daemon-side (8 concurrent, 1500 ms/daemon — `get_observability_tree.rs:12-13`). A per-call wall timeout is out of scope (prefer less). |
| Stop condition | `stop: AtomicBool` set `true` immediately after `run_cargo_test` returns (`eos-e2e.rs:140`) | the cargo child blocks `run_pipeline`; when it returns, every test has finished and every per-test sandbox has been destroyed, so one final poll cycle after `stop` captures any still-present node, then the loop exits. |
| Final cycle | the loop checks `stop` at the **top** of each cycle; the spawn-time first cycle runs immediately so even a near-instant cargo run yields ≥1 poll attempt | guarantees at least one snapshot attempt even for trivial runs. |
| Exit-before-cleanup | `handle.join()` **before** `guard.teardown()` (`eos-e2e.rs:200`) and before `report::build_tests` (`eos-e2e.rs:143`) | the poller writes under `{run_root}/reports/{id}/`; joining first guarantees every `observability.json` is flushed before `OnSuccess`/`Always` cleanup runs `remove_dir_all(run_root)` (`cleanup.rs:82`). |

The poller is **synchronous** inside the thread: each cycle calls
`CliClient::manager("get_observability_tree", …)` (`cli_client.rs:38`), parses
the `{ sandboxes: [...] }` response, projects each node, and writes/overwrites
one `observability.json` per observed `sandbox_id`. No `tokio`, no
`CancellationToken` — a plain `Arc<AtomicBool>` flag matches the existing sync
orchestrator (prefer less).

### 3.2 The poll call (confirmed CLI arg names)

The poller issues exactly:

```
sandbox-cli manager get_observability_tree \
  --include-recent-traces 1 \
  --trace-limit 100 \
  --resource-window-ms 60000
```

**No `--sandbox-id`** — Stage 1 polls the **whole** tree and keys by the returned
`sandbox_id` (§3.3). The four flag names are confirmed against the live operation
spec (`get_observability_tree.rs`): `--sandbox-id` (`:33`),
`--include-recent-traces` (`:43`), `--trace-limit` (`:52`), `--resource-window-ms`
(`:62`). These **match the stage-fence names exactly** — no correction needed.
`--trace-limit 100` is the daemon cap (`MAX_TRACE_LIMIT = 100`,
`service.rs:30`, applied `:498`); `--resource-window-ms 60000` is well under the
daemon cap (`MAX_RESOURCE_WINDOW_MS = 600_000`, `service.rs:31`, applied `:501`).
Over-limit args are **clamped, not rejected** (`service.rs:498,501`), so the
poller never errors on the limits.

Built through `CliClient::manager` (`cli_client.rs:38-42`), the poller reuses the
exact black-box call path the tests use; the returned `CallRecord`
(`cli_client.rs:11-20`) carries `response_json`, `exit_code`, `stderr`, and
`latency_ms`, which feed the snapshot's source-call metadata (§4).

### 3.3 Sandbox-id discovery while tests run (design Q2)

**Decision: poll the whole manager tree and key each node by its returned
`sandbox_id` — pure black-box, zero new coupling.** The `get_observability_tree`
response is `{ "sandboxes": [ node, … ] }` (`get_observability_tree.rs:106`), and
every node carries a top-level `sandbox_id` string
(`get_observability_tree.rs:253` inserts it; `unavailable_node` carries it too,
`:290`). With **no** `--sandbox-id`, the manager lists all **ready** sandboxes
with daemon endpoints (`get_observability_tree.rs:134-139`), so the poller
naturally observes whatever sandboxes are live at poll time.

This is preferred over the alternatives because:

- **Phase 3 report dirs are created late.** `reports/{id}/` is created in
  `Sandbox::drop` (via `report::write_exchange`, `report.rs:25-27`, called at
  `fixtures.rs:139`) — i.e. *after* the test finishes. A discovery scheme keyed on
  `reports/*/` (the way `cleanup.rs:112-124` sweeps survivors) would miss
  in-flight sandboxes. The tree's returned `sandbox_id` is available the moment a
  sandbox is `Ready`, which is exactly when it has interesting observability.
- **No early provision marker, no central registry.** Adding a marker file or a
  shared id registry would fight Phase 3's per-sandbox report-dir ownership
  (`fixtures.rs` owns `reports/{id}/`) and add a second source of truth. The tree
  already *is* the registry of live sandboxes.

The poller **creates** `{run_root}/reports/{sandbox_id}/` if it does not yet
exist (the writer calls `fs::create_dir_all`, mirroring `write_exchange`
`report.rs:27` and `write_result` `report.rs:65`). When the per-test
`Sandbox::drop` later writes `exchange.jsonl`/`result.json` into the same dir,
the three files coexist; when the poller writes after `Sandbox::drop`, the dir
already exists. Either order is safe (both call `create_dir_all`).

---

## 4. Artifact schema for observability.json (+ summary addition)

### 4.1 `observability.json` ownership & schema (design Q3)

**Owner: `report.rs`** (it already owns artifact writing and outcome DTOs, per
`README`/parent crate-shape note). The poller in `eos-e2e.rs` projects each tree
node into the DTO and calls `report::write_observability`. One file per sandbox
at `{run_root}/reports/{sandbox_id}/observability.json`.

**Snapshot count: latest-only.** Each poll cycle **overwrites**
`observability.json` with the most recent node for that sandbox, plus a small
bounded *poll-meta* block recording how many cycles observed it. Latest-only
keeps the artifact bounded and matches the parent's "latest tree node + bounded
recent-trace summaries in one file" (`spec.md` Observability section). Bounded
history is **not** kept (prefer less); the `recent_traces` and `resources.history`
arrays inside the latest node already carry the daemon's own bounded history.

Writer signature (mirrors `write_result`, `report.rs:63-67`):

```rust
pub const OBSERVABILITY_SCHEMA_VERSION: u32 = 1;

/// Write {run_root}/reports/{sandbox_id}/observability.json (latest-only),
/// creating the report dir like write_exchange/write_result do. Best-effort:
/// returns io::Result so the poller can swallow failures (a write error is a
/// recorded warning, never a run failure).
pub fn write_observability(run_root: &Path, snapshot: &ObservabilitySnapshot) -> io::Result<()>;
```

**DTO shape** (all `#[derive(Serialize, Deserialize)]`, every artifact carries
`schema_version` per the cross-phase invariant):

```jsonc
// {run_root}/reports/{sandbox_id}/observability.json
{
  "schema_version": 1,
  "sandbox_id": "...",                  // the tree node's sandbox_id (key)
  "captured_at": "YYYYMMDDThhmmssZ",    // config::utc_stamp() at the cycle that wrote this
  "source_call": {                      // metadata of the get_observability_tree call
    "argv": ["--gateway-socket","…","manager","get_observability_tree","--include-recent-traces","1","--trace-limit","100","--resource-window-ms","60000"],
    "exit_code": 0,
    "latency_ms": 12
  },
  "poll_meta": {
    "cycles_observed": 7,               // how many poll cycles saw this sandbox_id
    "last_cycle_index": 41              // 0-based index of the writing cycle
  },
  "node": {                             // the latest public-tree node, projected (bounded)
    "lifecycle_state": "ready",
    "availability": "available" | "partial" | "unavailable",
    "sampled_at_unix_ms": 1_700_000_000_000 | null,
    "errors": ["…"],                    // node-level errors array (bounded count)
    "resources": {
      "latest": { /* projected resource sample, §4.2 */ } | null,
      "history": [ /* ≤ RESOURCE_HISTORY_CAP projected samples */ ]
    },
    "recent_traces": [ /* ≤ RECENT_TRACE_CAP projected trace summaries, §4.3 */ ],
    "workspace_count": 0                // length of node.workspaces (NOT the full workspace bodies)
  },
  "p1": { /* §5 P1 detection block */ },
  "warnings": [ "…" ]                   // §6 warning strings (e.g. P1 unavailable, malformed shape)
}
```

Bounds (constants in `report.rs`, prefer less — small fixed caps so the artifact
never grows unbounded even if the daemon returns large arrays):

- `RECENT_TRACE_CAP = 50` — the poll asks for `--trace-limit 100`; the artifact
  keeps the first 50 projected summaries and records a warning if the node carried
  more.
- `RESOURCE_HISTORY_CAP = 50` — same treatment for `resources.history`.

The writer keeps **summaries, not full node bodies**: it deliberately drops the
full `workspaces[]` bodies (recording only `workspace_count`), and projects only
the trace/resource fields named in §4.2/§4.3. The node's `daemon` block (socket
paths) is **omitted** — paths are reproducible from the manifest and add no
diagnostic value.

**A sandbox id seen by the tree but not yet represented in `reports/{id}` ⇒ the
writer creates the dir** (`fs::create_dir_all`, §3.3). This is the common case
for in-flight sandboxes whose `Sandbox::drop` has not yet run.

### 4.2 Projected resource sample (the P1 carrier)

Each resource sample in the live public tree is shaped (daemon projection,
`service.rs:631-652`):

```jsonc
{
  "sampled_at_unix_ms": 1_700_000_000_000,
  "sample_delta_ms": null,
  "cgroup": {
    "available": false,            // service.rs:635  (NOTE: key is `available`, NOT `cgroup_available`)
    "cpu_usage_usec": null,        // service.rs:636
    "cpu_usage_delta_usec": null,
    "memory_current_bytes": null,  // service.rs:637
    "memory_current_delta_bytes": null,
    "memory_max_bytes": null,      // service.rs:638
    "memory_max_unlimited": null,  // service.rs:639
    "error": "cgroup path unavailable" // service.rs:640
  },
  "disk": { /* upperdir_bytes, upperdir_delta_bytes, file_count, … service.rs:642-650 */ }
}
```

The projected resource sample DTO keeps `sampled_at_unix_ms` and the **whole
`cgroup` object verbatim** (the P1 carrier); `disk` is kept as an opaque
pass-through `serde_json::Value` (informational, bounded by the daemon). The
`resources.latest`/`resources.history` keys come from the daemon's
`resource_bundle_value` (`service.rs:612-629`); the empty case is
`{ "latest": null, "history": [] }` (manager default `empty_resources_value`,
`get_observability_tree.rs:311-316`; daemon `:618` yields `null` latest).

### 4.3 Projected recent-trace summary

Each recent-trace row in the live tree is shaped (daemon
`request_trace_value`/`namespace_trace_value`, `service.rs:666-714`). The
projected summary keeps only:

| Kept field | Source | Notes |
|---|---|---|
| `trace_id` | `service.rs:668,700` | opaque id |
| `kind` | `service.rs:669,701` | `"request"` / `"namespace_execution"` |
| `operation` | `service.rs:670,702` | |
| `status` | `service.rs:671,703` | `"ok"`/`"error"` |
| `duration_ms` | `service.rs:677,709` | the performance signal |
| `error_kind` | `service.rs:678,711` | nullable |

`request_id`, `workspace_id`, `namespace_execution_id`, `exit_code`, timestamps,
and `error_message` are **dropped** from the summary (prefer less; the
diagnostic value is operation + duration + status). Pre-migration these arrays
are mostly empty for runtime activity (Stage 2 surfaces namespace-execution
traces).

### 4.4 Summary integration (design Q5)

**Decision: add ONE additive, non-gating field `observability` to the existing
`Summary` DTO** (`report.rs:191-205`). Rationale: a single roll-up object lets an
operator see Stage 1's observability outcome from `summary.json` alone without
opening every per-sandbox file, and it is the natural home for the aggregate P1
verdict. It is strictly additive — the Phase 3 aggregation contract (every
existing `Summary` field built by `build_tests`/`Counts::tally`, `eos-e2e.rs:143-196`)
is unchanged. `Summary` is `#[derive(Serialize)]` only (`report.rs:190`), so
adding a field cannot break any deserializer (nothing deserializes `Summary`;
`build_tests` deserializes `TestOutcome`, not `Summary`, `report.rs:235`).

```rust
#[derive(Serialize)]
pub struct ObservabilitySummary {
    pub schema_version: u32,            // OBSERVABILITY_SCHEMA_VERSION
    pub poll_cycles: u64,               // total poll cycles run
    pub poll_errors: u64,               // cycles whose get_observability_tree call returned non-zero / unparsable
    pub snapshots_written: usize,       // distinct sandbox_ids with an observability.json
    pub p1_available: bool,             // true iff ANY observed sample had cgroup.available == true
    pub warnings: Vec<String>,          // deduplicated run-level warnings (bounded)
}
```

`Summary` gains exactly:

```rust
pub struct Summary {
    // … all existing Phase 3 fields unchanged (report.rs:192-204) …
    pub observability: ObservabilitySummary,   // [NEW] additive, non-gating
}
```

The orchestrator fills it from the joined poller result (§3.1) and folds it into
the `report::Summary` literal it already builds (`eos-e2e.rs:172-196`) before
`write_summary` (`eos-e2e.rs:197`). Because `eos-e2e.rs:203-209` rewrites the
summary after teardown when the run_root is kept, the observability roll-up is
written in **both** summary writes (it is part of the `Summary` value, so this is
automatic — no extra code path).

**Why not "per-sandbox file only".** Considered and rejected: a top-level P1
verdict and `snapshots_written` count are cheap, additive, and let
acceptance/CI assert the diagnostic at the run level. The per-sandbox files
remain the primary artifact; the summary block is a roll-up, not a second source
of truth.

---

## 5. P1 detection/reporting rules and Stage 2 P2 deferral

### 5.1 What counts as P1 "available" (design Q4)

P1 is consumed **only** from the public tree, under each resource sample's
**nested `cgroup` object** (§4.2). The exact field names are confirmed against
the daemon projection (`service.rs:631-641`), which **corrects the parent/stage
prose**:

- The availability flag is `cgroup.available` (`service.rs:635`), **not**
  `cgroup_available`. (`cgroup_available` is the internal `CgroupSample` /
  `ResourceSampleRow` field name — `cgroup.rs:4`, `service.rs:434` — but the
  **public-tree key is `available`**.)
- The numeric fields are `cgroup.cpu_usage_usec`, `cgroup.memory_current_bytes`,
  `cgroup.memory_max_bytes`, `cgroup.memory_max_unlimited`
  (`service.rs:636-639`) — these names match the stage fence.
- The diagnostic flag is `cgroup.error` (`service.rs:640`), **not**
  `cgroup_error` (the internal name, `cgroup.rs:5`).

**P1 detection per sample:** a sample is **P1-available** iff
`node.resources.latest.cgroup.available == true` **and** at least one numeric
field (`cpu_usage_usec` or `memory_current_bytes`) is a non-null number.

**P1 block** in `observability.json`:

```jsonc
"p1": {
  "available": false,                 // per the rule above, over resources.latest
  "cpu_usage_usec": null,             // mirrored from cgroup.cpu_usage_usec when present
  "memory_current_bytes": null,       // mirrored from cgroup.memory_current_bytes when present
  "memory_max_bytes": null,
  "memory_max_unlimited": null,
  "reason": "cgroup unavailable: cgroup path unavailable"  // null when available==true
}
```

### 5.2 Null / missing / `available == false` representation

The three degraded shapes are represented distinctly and **all are warnings, not
failures** (§6):

| Live shape | P1 block | Warning recorded |
|---|---|---|
| `resources.latest == null` (no sample; the empty default `service.rs:618`, manager `:311-316`) | `available:false`, all fields `null`, `reason:"no resource sample"` | `"P1 unavailable for {id}: no resource sample"` |
| `resources.latest.cgroup.available == false` (the **current live default** — cgroup is always `CgroupSample::unavailable("cgroup path unavailable")`, `service.rs:263,416`) | `available:false`, numeric fields `null`, `reason:"cgroup unavailable: {cgroup.error}"` | `"P1 unavailable for {id}: cgroup unavailable"` |
| `cgroup.available == true` but a numeric field is `null`/absent | `available:false`, fields mirrored where present, `reason:"cgroup available but counters absent"` | `"P1 partial for {id}: counters absent"` |
| `cgroup.available == true` and counters present | `available:true`, fields mirrored, `reason:null` | none |

**Today, every run records the second row** (P1 unavailable) because the daemon
hard-codes `CgroupSample::unavailable(...)` for both the sandbox-root sample
(`service.rs:263`) and workspace samples (`service.rs:416`). This is **expected
and green**: P1 absence only lowers resolution. When the daemon later fills real
cgroup counters (a daemon-side change, parent decision P1 owner = `sandbox-daemon`),
the projection picks them up **with no runner change** because it reads whatever
the additive tree carries.

### 5.3 Stage 2 P2 deferral (named, not designed)

P2 (namespace queue-wait timing — `enqueued_at_unix_ms`, `running_at_unix_ms`,
derived `queue_wait_ms`) is **deferred to Stage 2** and **not represented** in
Stage 1's DTOs. These fields **do not exist in live code**: the daemon's
namespace-execution record carries only `started_at_unix_ms`
(`namespace_execution.rs:50,53`), and the public namespace-execution / trace
projections expose no enqueue/running split (`service.rs:602-714`). Stage 1 must
**not** treat any P2 field as a requirement; the `rg` checks in §9 enforce this.
Runtime command traces are likewise Stage 2 (pre-migration the `recent_traces`
namespace rows are mostly empty).

---

## 6. Failure semantics and cleanup interaction

### 6.1 Failure classification (design Q6) — default = diagnostic only

| Failure mode | Classification | Where recorded | Affects exit? |
|---|---|---|---|
| Poll CLI call returns non-zero exit (`CallRecord.exit_code != 0`, `cli_client.rs:70`) | warning | that cycle's `poll_errors += 1`; run-level `summary.observability.warnings` | **No** |
| Tree response unparsable / not `{ sandboxes: [...] }` (missing/`null` `sandboxes` array) | warning | `poll_errors += 1`; warning `"malformed tree: no sandboxes array"` | **No** |
| A node is `availability == "unavailable"` (`get_observability_tree.rs:290-299` / `service.rs` node) | recorded as-is in the node + a per-snapshot warning | `observability.json.warnings` | **No** |
| A node missing an expected key | warning; the projection defaults the field (the manager already defaults `errors`/`resources`/`workspaces`/`recent_traces`, `get_observability_tree.rs:257-270`) | per-snapshot `warnings` | **No** |
| Missing/absent/`available == false` **P1** fields | warning (§5.2) | per-snapshot `warnings` + `summary.observability.p1_available` | **No** |
| `write_observability` I/O error | warning (best-effort write, `let _ = …`) | run-level warning | **No** |
| Poller **thread panic** | caught at `handle.join()`; recorded as a run-level warning; `ObservabilitySummary` defaults | run-level warning | **No** |

**Every Stage 1 observability failure is non-gating.** No exception. The
pass/fail gate remains the Phase 3 gate: cargo-test exit code (`eos-e2e.rs:140,145`)
AND per-test `result.json` statuses (`eos-e2e.rs:147`, via
`build_tests`/`status`). The poller never sets `guard.set_succeeded`
(`eos-e2e.rs:156` is the sole caller and is unchanged) and never changes the
`ExitCode` branch (`eos-e2e.rs:213-222`).

### 6.2 Cleanup interaction (design Q7)

Phase 3 cleanup is **unchanged**. The default policy is `OnSuccess`
(`config.rs:268`): `remove_dir_all(run_root)` runs iff the run succeeded
(`cleanup.rs:104-110`, `:80-94`). The poller is joined **before**
`guard.teardown()` (`eos-e2e.rs:200`), so every `observability.json` is flushed
before any removal. Path namespacing is intact — the artifact lives under
`{run_root}/reports/{id}/`, inside the tree `remove_dir_all` owns.

**Acceptance proves the artifact without weakening cleanup:** use
`--keep-artifacts` (forces `CleanupPolicy::Never`, `config.rs:265-266`,
`cleanup.rs:108`), which keeps `run_root` for inspection regardless of outcome.
The default cleanup policy is **not** changed; the spec adds no flag and no
policy. The two-write summary path (`eos-e2e.rs:197` then `:203-209`) already
ensures `summary.json` (with the observability roll-up) survives a kept run_root.

---

## 7. Implementation steps mapped to files

Ordered, additive, localized (other agents may be editing concurrently — touch
only these units).

1. **`report.rs` — DTOs + writer (the bulk of Stage 1).**
   - Add `pub const OBSERVABILITY_SCHEMA_VERSION: u32 = 1;` (beside the existing
     `*_SCHEMA_VERSION` consts at `report.rs:12-16`) and
     `RECENT_TRACE_CAP`/`RESOURCE_HISTORY_CAP` consts.
   - Add the `ObservabilitySnapshot`, `ObsSourceCall`, `ObsPollMeta`,
     `ObsNode`, `ObsResources`, `ObsResourceSample`, `ObsRecentTrace`, `P1`,
     and `ObservabilitySummary` DTOs (§4, §5). `Serialize` always;
     `Deserialize` on `ObservabilitySnapshot` if the §8-Q8 unit test round-trips
     it.
   - Add `pub fn write_observability(run_root, &ObservabilitySnapshot) -> io::Result<()>`
     mirroring `write_result` (`report.rs:63-67`): `create_dir_all` the report
     dir, then `write_json_pretty` (`report.rs:266`).
   - Add `pub fn observability_node_from_tree(node: &Value, source: &SourceMeta, …) -> (ObsNode, P1, Vec<String>)`
     pure projection over one `sandboxes[i]` `Value`, applying §4.2/§4.3 bounds
     and §5 P1 rules, returning warnings.
   - Add one additive field to `Summary`: `pub observability: ObservabilitySummary`.
2. **`bin/eos-e2e.rs` — poller side-thread.**
   - Add `const OBS_POLL_INTERVAL_MS: u64 = 1000;`.
   - Before `run_cargo_test` (`eos-e2e.rs:140`): build a `CliClient`
     (`CliClient::new(PathBuf::from(CLI_BIN), config.gateway_socket.clone())`,
     reusing the existing `CLI_BIN`/imports `eos-e2e.rs:9,19`), an
     `Arc<AtomicBool>` stop flag, an `Arc<run_root>`, and
     `std::thread::spawn(poll_loop)`.
   - After `run_cargo_test` returns (`eos-e2e.rs:140-141`): set `stop = true`,
     `handle.join()`, fold the returned `ObservabilitySummary` into the
     `report::Summary` literal (`eos-e2e.rs:172-196`).
   - `poll_loop(socket_cli, run_root, stop, interval) -> ObservabilitySummary`:
     loop { if stop → break-after-one-final-cycle; `cli.manager("get_observability_tree",
     ["--include-recent-traces","1","--trace-limit","100","--resource-window-ms","60000"])`;
     parse `/sandboxes`; per node → `report::observability_node_from_tree` →
     `report::write_observability`; accumulate counts/warnings; `sleep(interval)`. }
   - Do **not** edit `STAGE1_DEFAULT_TARGET` (`eos-e2e.rs:17`), `run_cargo_test`
     (`eos-e2e.rs:301-315`), the gate (`eos-e2e.rs:145-156`), or the exit branch
     (`eos-e2e.rs:213-222`).
3. **`tests/observability_writer.rs` (optional, §8-Q8).** Narrow unit test
   feeding a synthetic tree `Value` (with and without cgroup fields) through
   `report::observability_node_from_tree`, asserting P1 detection and warning
   strings. No Docker, no gateway, no runtime leaf. Lives in `tests/`, not
   `src/` (no test code in `src/`).
4. **No edits** to `Cargo.toml`, `build.rs`, `config.rs`, `cli_client.rs`,
   `fixtures.rs`, `cleanup.rs`, `gateway.rs`, `assertion.rs`, `lib.rs`, or any
   manager test leaf.

---

## 8. Anchor ledger

Every load-bearing `file:line` below was confirmed by reading the working tree
with `nl -ba`/`rg`. Paths are crate-relative under
`crates/sandbox-e2e-live-test/` unless prefixed.

| Anchor (`file:line`) | Claim | Verdict |
|---|---|---|
| `src/bin/eos-e2e.rs:17` | `const STAGE1_DEFAULT_TARGET: &[&str] = &["--test","manager"]` — the sole stage line; Phase 4 leaves it untouched | **confirmed** |
| `src/bin/eos-e2e.rs:20` | `const RUN_ROOT_ENV: &str = "EOS_E2E_RUN_ROOT"` (the export contract) | **confirmed** |
| `src/bin/eos-e2e.rs:88` | `fn run_pipeline(args: &RunArgs) -> ExitCode` — poller is owned here | **confirmed** |
| `src/bin/eos-e2e.rs:104` | `report::write_run_manifest(...)` (run_root exists after this) | **confirmed** |
| `src/bin/eos-e2e.rs:111` | `RunGuard::new(...)` constructs the cleanup guard | **confirmed** |
| `src/bin/eos-e2e.rs:128` | `gateway::await_ready(&config.gateway_socket)` — socket ready before poller spawn | **confirmed** |
| `src/bin/eos-e2e.rs:140` | `let cargo_status = run_cargo_test(&config, &filters)` — blocks the pipeline; poller spans this | **confirmed** |
| `src/bin/eos-e2e.rs:143` | `report::build_tests(&config.run_root)` — join poller before this | **confirmed** |
| `src/bin/eos-e2e.rs:145-156` | gate: `cargo_ran`/`cargo_ok`/`all_passed` → `status`; `guard.set_succeeded(...)` at `:156` | **confirmed** |
| `src/bin/eos-e2e.rs:172-196` | `report::Summary { … }` literal — fold `observability` field in here | **confirmed** |
| `src/bin/eos-e2e.rs:197` | `report::write_summary(&config.run_root, &summary)` (first write) | **confirmed** |
| `src/bin/eos-e2e.rs:200` | `let cleanup = guard.teardown()` — cleanup; poller joined before this | **confirmed** |
| `src/bin/eos-e2e.rs:203-209` | second `write_summary` when run_root is kept (`!cleanup.removed_run_root`) | **confirmed** |
| `src/bin/eos-e2e.rs:213-222` | exit branch (`ExitCode::SUCCESS` / `1` / `2`) — unchanged by Phase 4 | **confirmed** |
| `src/bin/eos-e2e.rs:301-315` | `run_cargo_test` applies `STAGE1_DEFAULT_TARGET` (`:304`), `--test-threads` (`:309`), exports `RUN_ROOT_ENV` (`:310`) | **confirmed** |
| `src/cli_client.rs:11-20` | `CallRecord { argv, request_json, response_json, exit_code, stdout, stderr, latency_ms }` | **confirmed** |
| `src/cli_client.rs:38-42` | `CliClient::manager(op, args) -> CallRecord` — the poll call path | **confirmed** |
| `src/cli_client.rs:64-70` | `Command::output()` blocks; `exit_code = status.code().unwrap_or(-1)` | **confirmed** |
| `src/cli_client.rs:97-100` | `CallRecord::response() -> &Value` (parsed tree) | **confirmed** |
| `src/report.rs:12-16` | `EXCHANGE/MANIFEST/RESULT/SUMMARY_SCHEMA_VERSION` consts — add `OBSERVABILITY_SCHEMA_VERSION` beside | **confirmed** |
| `src/report.rs:25-27` | `write_exchange` creates `reports/{id}/` via `create_dir_all` | **confirmed** |
| `src/report.rs:63-67` | `write_result` mirror for `write_observability` (create dir + `write_json_pretty`) | **confirmed** |
| `src/report.rs:190-205` | `#[derive(Serialize)] struct Summary { … }` — add additive `observability` field | **confirmed** |
| `src/report.rs:208-211` | `write_summary(run_root, &Summary)` | **confirmed** |
| `src/report.rs:217-264` | `build_tests` deserializes `TestOutcome` (not `Summary`) — adding a `Summary` field is safe | **confirmed** |
| `src/report.rs:266-269` | `write_json_pretty` helper reused by the new writer | **confirmed** |
| `src/fixtures.rs:139` | `report::write_exchange(...)` in `Sandbox::drop` — report dir created late (after test) | **confirmed** |
| `src/fixtures.rs:155` | `report::write_result(...)` in `Sandbox::drop` | **confirmed** |
| `src/fixtures.rs:157-159` | `Sandbox::drop` issues `destroy_sandbox` — sandboxes vanish as tests finish (⇒ poll during, not after) | **confirmed** |
| `src/cleanup.rs:80-94` | `remove_dir_all(run_root)` when `should_remove()` | **confirmed** |
| `src/cleanup.rs:104-110` | `should_remove`: `Always`/`OnSuccess(run_succeeded)`/`Never` | **confirmed** |
| `src/cleanup.rs:112-124` | survivor sweep keys on `reports/*/` dir names (the late-dir scheme Phase 4 avoids for discovery) | **confirmed** |
| `src/config.rs:265-268` | `--keep-artifacts` ⇒ `Never`; default `OnSuccess` | **confirmed** |
| `src/config.rs:347-358` | `utc_stamp()` colon-free `YYYYMMDDThhmmssZ` — reused for `captured_at` | **confirmed** |
| `Cargo.toml:16-21` | deps = `anyhow, serde, serde_json, clap, sha2, time` — no async; no new dep needed | **confirmed** |
| `tests/support/mod.rs:7-9` | `harness() = Harness::get()` — sole skip path (`EOS_E2E_RUN_ROOT` unset); no obs skip guard added | **confirmed** |
| `tests/manager/observability/get_observability_tree/returns_tree.rs:13-23` | existing M4 leaf's `manager(...)` call uses `--sandbox-id`/`--include-recent-traces 1`/`--trace-limit 100` | **confirmed** |
| `tests/manager/observability/get_observability_tree/returns_tree.rs:28-39` | M4 node-key assertions: `sandbox_id` (`:28`), `availability` (`:29`), and the `["resources","workspaces","recent_traces","errors"]` loop (`:37-39`) | **confirmed** |
| `crates/sandbox-manager/src/operation/impls/management/get_observability_tree.rs:33` | `--sandbox-id` flag | **confirmed** |
| `…/get_observability_tree.rs:43` | `--include-recent-traces` flag | **confirmed** |
| `…/get_observability_tree.rs:52` | `--trace-limit` flag | **confirmed** |
| `…/get_observability_tree.rs:62` | `--resource-window-ms` flag (parent/stage names match live) | **confirmed** |
| `…/get_observability_tree.rs:106` | response = `Response::ok(json!({ "sandboxes": sandboxes }))` | **confirmed** |
| `…/get_observability_tree.rs:134-139` | no `--sandbox-id` ⇒ all `Ready` sandboxes with daemon endpoints | **confirmed** |
| `…/get_observability_tree.rs:253-254` | node gets `sandbox_id` + `lifecycle_state` inserted | **confirmed** |
| `…/get_observability_tree.rs:257-270` | manager defaults node keys `errors/daemon/resources/workspaces/recent_traces` | **confirmed** |
| `…/get_observability_tree.rs:290-299` | `unavailable_node` carries `sandbox_id`, `availability:"unavailable"`, `resources` empty | **confirmed** |
| `…/get_observability_tree.rs:311-316` | `empty_resources_value = { latest:null, history:[] }` | **confirmed** |
| `…/get_observability_tree.rs:12-13` | daemon fan-out caps (8 concurrent, 1500 ms) — bounds the poll cost | **confirmed** |
| `crates/sandbox-daemon/src/observability/service.rs:30-31` | `MAX_TRACE_LIMIT = 100`, `MAX_RESOURCE_WINDOW_MS = 600_000` | **confirmed** |
| `…/service.rs:498,501` | trace_limit/resource_window clamped (`.min(MAX_…)`), not rejected | **confirmed** |
| `…/service.rs:538-557` | `snapshot_value` builds node: `availability`, `sampled_at_unix_ms`, `errors`, `resources`, `workspaces`, `recent_traces` | **confirmed** |
| `…/service.rs:612-629` | `resource_bundle_value` = `{ latest, history }`; empty latest ⇒ `null` (`:618`) | **confirmed** |
| `…/service.rs:631-641` | resource sample → nested `cgroup` object: key **`available`** (`:635`), `cpu_usage_usec` (`:636`), `memory_current_bytes` (`:637`), `memory_max_bytes` (`:638`), `memory_max_unlimited` (`:639`), `error` (`:640`) | **confirmed / corrects parent** (public key is `available`/`error`, not `cgroup_available`/`cgroup_error`) |
| `…/service.rs:263,416` | cgroup is hard-coded `CgroupSample::unavailable("cgroup path unavailable")` today (root + workspace samples) | **confirmed** (P1 unavailable is the current live default) |
| `…/service.rs:666-714` | recent-trace projection fields: `trace_id, kind, operation, status, duration_ms, error_kind` (among others) | **confirmed** |
| `crates/sandbox-daemon/src/observability/cgroup.rs:1-10` | internal `CgroupSample { cgroup_path, cgroup_available, cgroup_error, cpu_usage_usec, memory_current_bytes, memory_max_bytes, memory_max_unlimited }` — internal names ≠ public-tree keys | **confirmed** |
| `crates/sandbox-daemon/src/observability/namespace_execution.rs:50,53` | only `started_at_unix_ms` recorded; **no** `enqueued_at_unix_ms`/`running_at_unix_ms` (P2 deferred, absent in live) | **confirmed** |

---

## 9. Verification and acceptance

Run from the workspace root (`export PATH="$PWD/bin:$PATH"` first).

| # | Command | Pass criterion |
|---|---|---|
| 1 | `cargo build -p sandbox-e2e-live-test` | exit 0 |
| 2 | `cargo clippy -p sandbox-e2e-live-test --all-targets` | exit 0; no new `unwrap_used`/`dbg_macro`/`undocumented_unsafe_blocks` |
| 3 | `cargo fmt --check` | exit 0 |
| 4 | `cargo test -p sandbox-e2e-live-test` (env `EOS_E2E_RUN_ROOT` **unset**) | every leaf **skips cleanly**, no panic, **nothing written** (no `result.json`, no `observability.json`, no `run_root`). The skip path (`tests/support/mod.rs:7-9`) is unchanged; the optional `tests/observability_writer.rs` unit test runs offline and passes. |
| 5 | `eos-e2e --gateway-socket {real} --image ubuntu:24.04 --max-parallel 8 --keep-artifacts` (Linux + Docker + **external real-runtime gateway**, manager-only Stage 1) | writes `{run_root}/reports/{sandbox_id}/observability.json` for each **observed** sandbox; `run_root` kept (`--keep-artifacts`); the **Phase 3 pass/fail gate is unchanged** (exits 0 iff cargo-test exit 0 AND every `result.json == passed`). |
| 6 | inspect a kept `observability.json` | records `schema_version == 1`, `source_call.argv` showing the four flags, the latest `node`, and `p1.available == false` with a `reason` (current live default, cgroup unavailable) — **a warning, not a failure**. |
| 7 | inspect `{run_root}/summary.json` | `summary.observability` present with `snapshots_written`, `p1_available`, `warnings`; all Phase 3 `summary` fields unchanged; `summary.status` computed exactly as Phase 3. |
| 8 | default cleanup unchanged | a successful run **without** `--keep-artifacts` removes `run_root` (`OnSuccess`); inspection requires `--keep-artifacts`. No new flag/policy was added. |
| 9 | `rg -n "for_test\|sandbox_observability\|store::" crates/sandbox-e2e-live-test/` | **no matches** — no internal store reader, no `*_for_test`, no internal-crate dep. |
| 10 | `rg -n "enqueued_at_unix_ms\|running_at_unix_ms\|queue_wait_ms" crates/sandbox-e2e-live-test/` | **no matches** — no P2 queue-wait field treated as a requirement. |
| 11 | `git diff --name-status {phase-3-baseline}.. -- crates/sandbox-e2e-live-test/tests/runtime/` | Phase 4 adds **no NEW** runtime test leaf. The runtime tree already exists at the Phase 3 baseline (one leaf, `tests/runtime/command/exec_command/one_shot.rs`, plus `tests/runtime.rs`), so a bare `ls`/`rg` is vacuous (the leaf is present regardless). The real pass criterion: this diff over `tests/runtime/` shows **no `A` (added) and no `M` (modified) entries** — the file set under `tests/runtime/` is byte-for-byte unchanged from baseline. |
| 12 | `git diff --name-status {phase-3-baseline}..` (after implementation) | the **only** changed paths are `src/report.rs` (`M`), `src/bin/eos-e2e.rs` (`M`), and (optional) `tests/observability_writer.rs` (`A`); `Cargo.toml`/`build.rs`/`config.rs`/`cleanup.rs`/`fixtures.rs`, all of `tests/runtime/`, and all of `tests/manager/` are **absent** from the diff. |

**Honest gate note.** A *green* Stage 1 observability run (#5–#8) requires an
externally started `sandbox-gateway` wired with the real Docker runtime, attached
via `--gateway-socket` (Open Items #1). The shipped gateway wires `Unconfigured*`
stubs; against it the Phase 3 preflight fails fast by design. Code is complete
and skip-safe (#4) regardless; only the live-green proof waits on the
real-runtime gateway. P1 will read `available:false` until the daemon fills real
cgroup counters (`service.rs:263,416`) — that is the designed degraded-resolution
default, not a failure.

---

## 10. Conventions checklist

| Convention | How Phase 4 Stage 1 satisfies it |
|---|---|
| **SRP / one job per unit** | `report.rs` keeps its single job (artifact DTOs + writers): `write_observability` + projection helpers join `write_exchange`/`write_result`/`write_summary`/`write_run_manifest` — all artifact writing. `eos-e2e.rs` keeps its single job (run orchestration): the poller is one side-thread it owns, not a new owner type. No new module. |
| **Prefer less** | **Zero** new crate deps (std::thread + `Arc<AtomicBool>`, no tokio/uuid). Latest-only snapshot (no history file). One additive `Summary` field, not a separate summary file. P1 read straight from the tree, no parallel reader. Fixed-const interval/caps, not CLI knobs. |
| **Black-box only** | The poller drives `sandbox-cli manager get_observability_tree` over the gateway socket via `CliClient` (`cli_client.rs:38`) and reads only `{run_root}`. **No** internal-crate dep, **no** `*_for_test`, **no** `sandbox-observability` store reader (§9 #9). P1 names taken from the public-tree projection (`service.rs:631-641`), not the internal `CgroupSample` (`cgroup.rs`). |
| **No Stage 2 leakage** | `STAGE1_DEFAULT_TARGET` untouched (manager-only; zero runtime ops). No P2 field is a requirement (§9 #10). No **new** runtime test leaf added (§9 #11; the one baseline leaf is unchanged). No runtime assertion helper. P2/runtime traces named only as deferred (§5.3). |
| **No inline comments / no test code in `src/`** | Doc comments on public items only; the projection has no inline `//`. The only test is `tests/observability_writer.rs` (offline unit), never under `src/`. |
| **Non-gating** | Every observability failure is a recorded warning (§6.1); the gate stays the Phase 3 cargo-exit + `result.json` gate (`eos-e2e.rs:140,145-156,213-222`). Cleanup order/policy unchanged (`cleanup.rs`); poller joined before teardown. |
