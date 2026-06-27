# Removal Closeout + Cross-Process Threading (Phase B)

Status: ready-to-implement (depends on `crate-core-impl.md` + `span-trace-impl.md`).

This is the **implementation** spec for the rework's tail — the main spec's
(`README.md` §9) **Phase 5 (removal checklist) and Phase 6 (Phase B)**. Two
independent parts:

- **Part A — Removal closeout & gates**: drive the scoped `README.md` §8 greps to
  zero, delete the code left dead by `crate-core-impl.md`, and settle the boundary
  tests into their final shape. Mostly verification + deletion; no new behavior.
- **Part B — Cross-process trace threading**: stitch the forked
  namespace-process span (Case A's `np-0`) into the per-request tree by carrying
  `trace`/`parent` across the fork. **No record-schema change** — the
  `trace`/`parent`/`span` fields exist from `crate-core-impl.md`; Phase B only
  populates them across the process boundary.

---

## Part A — Removal closeout & gates

### A.1 The scoped greps (must return zero)

`README.md` §8 / §10 gate the rework on scoped greps. Current status:

| Grep (scope) | Status | Owner |
|---|---|---|
| `timing::` in `sandbox-runtime` + `sandbox-daemon` | **already zero** (commit `aa401c2f0`) | — |
| `runtime_timing_env` in `crates` | **already zero** | — |
| `rusqlite` in `crates` | zero **after** `crate-core-impl.md` §4.4 | confirm here |

**Confirm** all three at the end of this slice; any non-zero is a regression to
fix, not to wave through.

### A.2 The gateway-timing point is moot

`README.md` §8.1 says to **retain** `sandbox-gateway/src/cli/timing.rs` (host-side
timing, out of scope). In fact the timing-removal commit (`aa401c2f0`) took the
gateway sites too — the file does **not** exist, `sandbox-gateway/src/cli/mod.rs`
declares only `client`/`config`/`observability_specs`/`output`/`request_builder`,
and `grep timing crates/sandbox-gateway/src` is empty. So there is **nothing to
retain and nothing to remove** here; the §8.1 retention note is superseded. If
host-side CLI timing is wanted again it is a fresh, separate feature — not part of
this rework. Record this so the checklist isn't read as "a file went missing."

### A.3 Delete the code left dead by `crate-core-impl.md`

`crate-core-impl.md` removed the SQLite store and switched `snapshot`/`cgroup` to
the live registry; that strands several helpers whose only job was the SQLite
snapshot path. Delete them:

| Dead code | Where | Why dead |
|---|---|---|
| `snapshot_record` + `bound_operation`/`bound_state`/`bound_string` | `sandbox-daemon/src/observability/namespace_execution.rs` | built `NamespaceExecutionSnapshotRecord`s for the SQLite upsert (gone) |
| the SQLite test helpers (`TestObservabilityStore`, `*_for_test`, `block_sqlite_writes_for_test`) + `use rusqlite::…` | `sandbox-daemon/tests/unit/observability.rs` | exercised the deleted store; rewrite remaining behavioral assertions against the `Reader`/views or delete |
| SQLite schema/introspection tests | `sandbox-observability/tests/{schema.rs, support/mod.rs}` | introspected migrations/tables (gone) |
| the old snapshot op alias, if `crate-core-impl.md` §4.5 left one | `dispatch.rs` (`PRIVATE_OBSERVABILITY_SNAPSHOT_OP`, `dispatch_private_observability_snapshot`) | superseded by `view=snapshot` |

After this, `sandbox-daemon/src/observability/` is the thin caller the rework
promised: `mod.rs` (`unix_ms`, `MAX_RESOURCE_WINDOW_MS`), `service.rs`
(`DaemonObservability`: build `Observer`, `collect()` → `obs.sample`, live snapshot
read), `view.rs` (the six views), `layerstack.rs` (render helpers) — and **no**
store, no reconcile, no delta caches, no per-record builders.

### A.4 Boundary tests — final shape

| Test | Final assertion |
|---|---|
| `sandbox-observability/tests/dependency_guard.rs` | **canonical leaf invariant** — `[dependencies]` excludes `sandbox-runtime`/`sandbox-daemon`/`sandbox-manager` (and now `rusqlite`). Unchanged except the added `rusqlite` rule from `crate-core-impl.md` §5. |
| `sandbox-daemon/tests/unit/dependency_guard.rs` | keep — `[dependencies]` excludes `rusqlite`/`host` (daemon's `rusqlite` was a dev-dependency, removed in `crate-core-impl.md` §4.4). |
| `operation/tests/observability_snapshot.rs` (`…keeps_observability_crate_out`, `:91-95`) | **repointed in `span-trace-impl.md` §3**: drop the `sandbox-observability` assertion (the runtime now depends on the leaf by design), keep the `rusqlite` assertion (the runtime must never pull SQLite). Confirm the final wording here. |

The net invariant after the rework: the **obs crate is a leaf** (its own guard),
and **no in-sandbox crate pulls `rusqlite`** (the daemon + operation guards). That
pair replaces the old "keep the obs crate out of the runtime" rule, which existed
only because SQLite made the crate heavy — the reason is gone.

### A.5 Final gates

`grep -rn 'timing::' crates/sandbox-runtime crates/sandbox-daemon` empty;
`grep -rn runtime_timing_env crates` empty; `grep -rn rusqlite crates` empty;
`cargo build`, `cargo test`, `cargo clippy --all-targets`, `cargo fmt` clean.

---

## Part B — Cross-process trace threading (Phase B)

### B.1 Goal + the one gap

After `span-trace-impl.md`, everything under a request correlates **in-process**:
the daemon/runtime spans, the async exec tail, the finalize events — all under
`trace = request_id`. The **one** span that doesn't is the work the forked
namespace-process does: `namespace.runner.spawn_child` (Case A's `np-0`,
`README.md` §4.1; the label follows the span grammar `subsystem[.area].action`
— `namespace.runner` subsystem, `spawn_child` imperative action). It runs in a
different process that today receives no trace context, so it lands under its own
trace.

Phase B closes that gap by carrying `(trace, parent)` across the fork so the child
emits `np-0` under the originating trace, with `parent` = the async exec span
(`d-5`). **No record-schema change** — `Span.trace`/`Span.parent`/`Span.span`
already exist (`crate-core-impl.md` §2.1); the child uses the `record::proc::NS`
proc token (§2.3) so its ids never collide with the daemon's `d-*`. That `np-*`
proc token is the authoritative marker of the process boundary.

### B.2 Carry `(trace, parent)` on `NamespaceRunnerRequest`

`NamespaceRunnerRequest` (`namespace-process/src/runner/protocol.rs:21-35`) is the
struct serialized across the fork. Add one optional field:

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NamespaceRunnerRequest {
    pub request_id: String,
    pub args: Value,
    // … workspace_root, layer_paths, upperdir, workdir, ns_fds, timeout_seconds …
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace: Option<TraceContext>,    // { trace, parent } — the daemon-side ctx of the launching exec span
}
```

- **Populate at build.** `build_request` (`engine.rs:248-264`) is the single
  construction point (called from `run_shell_interactive:93` and
  `mount_overlay:130`). It already copies `request_id = id.0`; add
  `trace: <the launching exec span's child TraceContext>`. The id this `parent`
  needs must already exist at build time, so `SpanRegistry::open`
  (`crate-core-impl.md` §3) **mints the exec span id AT LAUNCH** — at the moment
  the async source opens the span, the same call that precedes `build_request` —
  and **returns a child `TraceContext { trace, parent: <the new exec span id> }`**
  rather than minting the id later at terminal/`record`. There is no
  mint-at-complete step to wait on and no new public handle: the id lives in one
  added `OpenSpan` field and rides out on `open`'s return value. `build_request`
  (or its callers) is handed that returned child ctx — threaded alongside the `id`
  already passed in — and stamps it verbatim as `trace`, so on the wire
  `np-0.parent` = the exec span's id (`d-5`) and `np-0.trace` = the request trace.
- **Crosses the existing pipe unchanged.** The request is JSON-serialized
  (`encode_request`, `launcher.rs:343-347`), written to `--request-fd`
  (`write_request:216-221`, `into_child:122-145`), and read + deserialized on the
  child side (`sandbox-daemon/src/runner/mod.rs:14-27`,
  `serde_json::from_str::<NamespaceRunnerRequest>`). The new `Option` field rides
  along with **no transport change** (serde default keeps old payloads valid).

`TraceContext` lives in the obs leaf; `namespace-process` gains the obs dependency
(leaf, same as `span-trace-impl.md` §3) to name the type — or, to keep
`namespace-process` obs-free, define `trace`/`parent` as two `Option<String>`
fields on the request and reassemble `TraceContext` on the child side. Prefer the
two-string form: it keeps the protocol crate's dependency surface minimal and the
wire shape explicit.

### B.3 Emit `np-0` on the child side

The child process decodes the request (`daemon/src/runner/mod.rs:14-27`) and
dispatches to `shell::run` / `mount_overlay::run` (`dispatch_runner_mode:29-38`),
ultimately reaching `execute_shell_inner` (`namespace-process/src/runner/shell_exec.rs:24-69`,
the `command.spawn()` at `:51`). To emit `np-0`:

- **Build a child-side `Observer`** once per ns-runner invocation: **proc token
  `record::proc::NS`** (`crate-core-impl.md` §2.3), a
  `Sink` over the **same** `observability.ndjson` (the child shares the runtime
  dir; single-write `O_APPEND` keeps daemon + child lines from interleaving —
  `README.md` §6 atomicity, already tested in `crate-core-impl.md` §7).
- **Open the span under the carried context.** From `request.trace`/`request.parent`
  build the `TraceContext` and `obs.with_context(ctx, || obs.span("namespace.runner.spawn_child"))`
  around the spawn (`shell_exec.rs:40-63`). On terminal it writes one `Span` with
  `span = "np-0"`, `parent = d-5`, `trace = req-…` — Case A's cross-process row.
- The mount path may similarly emit `namespace.runner.overlay.*` if wanted; `np-0`
  (shell) is the contract.
- Config gate: the child reads the same `observability.enabled` (passed in the
  request or via the runner config) and no-ops when off.

### B.4 Why not the watcher, and why not the protocol

- **The watcher already carries the handle** (`engine.rs:spawn_watcher:163-197`
  owns `Box<dyn RunnerChild>` and calls `on_terminal` at `:195`) — but it runs in
  the **daemon** process. It can (and does, via `span-trace-impl.md` §4) write the
  *daemon-side* async exec span `d-5`. It cannot write the *child-side* `np-0`,
  because that span belongs to the forked process. So the parent ctx must travel in
  the **request** (§B.2), not via the watcher.
- **The daemon protocol** (`sandbox-protocol/src/request.rs`) needs no change:
  `request_id` already *is* the trace for gateway→daemon, and the gateway sets it
  per call. Extending `Request` with `trace`/`parent` is optional and only useful
  if a future caller wants a non-root parent across the gateway boundary; this
  slice does not require it. Keep the protocol untouched.

### B.5 Testing

- **Cross-process correlation:** an `exec_command` now yields `np-0`
  (`namespace.runner.spawn_child`) under `req-…` with `parent = d-5`; `Reader::trace`
  renders it at the `+00.055` depth shown in `README.md` §4.1 /
  `cli-observability.md` §4.2 (no longer a separate trace).
- **Two-writer integrity:** daemon (`d-*`) and namespace-process (`np-*`) appending
  to one `observability.ndjson` concurrently — every line parses, none interleave,
  ids never collide (extends the `crate-core-impl.md` §7 single-write test to two
  real processes).
- **Back-compat:** a request without `trace` (old payload / disabled child)
  deserializes and runs; the child simply emits nothing or under no parent.
- **Gates:** Part A's greps + `cargo build`/`test`/`clippy`/`fmt` clean.

---

## Sequencing

Part A can land immediately after `crate-core-impl.md` (it is the cleanup that
slice implies) and does not depend on `span-trace-impl.md`. Part B depends on
`span-trace-impl.md` (it extends the async exec span across the fork). Run **A → B**,
or land A as the tail of the crate-core work and B as the final follow-up — they do
not conflict.
