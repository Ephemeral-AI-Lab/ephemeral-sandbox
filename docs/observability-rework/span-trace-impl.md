# Span/Trace Instrumentation + Domain Events (Phase A)

Status: ready-to-implement (depends on `crate-core-impl.md`).

This is the **implementation** spec for the producer side of the rework — the
main spec's (`README.md` §9) **Phases 3–4**. It wires the `Observer` (built in
`crate-core-impl.md`) into the runtime so that an `exec_command` reproduces
Case A's waterfall (`README.md` §4.1), and emits the layerstack lease/publish
domain events. It is the **in-process** correlation work ("Phase A"); the
cross-process `np-*` span is deferred to `removal-and-phaseb-impl.md` ("Phase B").

Prerequisites from `crate-core-impl.md` are assumed present: the `Observer`/
`SpanGuard`/`SpanRegistry`/`TraceContext` API with a thread-local context, the
generic `TerminalHook<K>` interface + generic `NoopHook`, and the one-file
`Sink`/`Reader` over `observability.ndjson`.

The ad-hoc timing instrumentation that used to mark these seams was **already
removed** (commit `aa401c2f0`, "Remove ad-hoc timing instrumentation across
crates") — so there is no `timing::` to swap; the dotted labels it used (e.g.
`runtime.exec.total`, `ns_runner.shell.spawn_child`) are the span `name`s this spec
**re-adds** as structured spans at the same seams, under the §6 label grammar —
renamed where it applies (`ns_runner.shell.spawn_child` → `namespace.runner.spawn_child`,
Phase B; `M10`) and minus `workspace.create`, which is **not** re-added (dropped, C1).

---

## 1. The instrumentation model — three shapes, three seams

| Shape | Where | Mechanism |
|---|---|---|
| **sync span** | in-daemon synchronous scopes (dispatch, exec_command, session create+destroy, mount_overlay) | `obs.span(name)` → `SpanGuard`, ends on drop on the same thread; nests via the thread-local parent; `.attr()`/`.status()` carry facts + outcome; a fallible seam wraps `obs.scope(name, …)` so an `Err` self-sets `Error` (§5, M2) |
| **async span** | the namespace exec **shell** that outlives the call | the `SpanRegistry<NamespaceExecutionId>` itself, wired as the engine's `TerminalHook` (`crate-core-impl.md` §3.4, m1); parked at launch, recorded on the watcher thread at child-exit (recorded **before** teardown, so it self-stamps the right instant — no timestamp argument); a launch that fails before the work runs is discarded by `launch`'s internal `cancel` (M3) — `Cancelled` is the shutdown-sweep backstop for a watcher that dies before recording |
| **event** | point-in-time domain facts (lease acquire/release) | `obs.event(name, attrs)`, written immediately, hung off the enclosing span via the thread-local parent |

Hook the **existing** lifecycle edges (`README.md` §5); do not sprinkle inline
timing.

---

## 2. Trace id threading (`trace = Request.request_id`)

The trace id is `Request.request_id` (`sandbox-protocol/src/request.rs:11`,
`pub request_id: String`), required on every daemon request. **Do not** use the
layerstack `owner_request_id` (`acquire_snapshot`'s param, `stack/mod.rs:75`) — it
is validated and discarded, only ever the literal `"workspace-session"` in
production, so it can correlate nothing (`README.md` §5).

**Set the thread-local at dispatch — inside the blocking closure.** `dispatch_request`
(`dispatch.rs:45-78`) hands the operation to
`tokio::task::spawn_blocking(move || sandbox_runtime::dispatch_operation(&operations,
&request))` (`:58-60`). `spawn_blocking` runs on a **separate** blocking-pool
thread, so a thread-local set in the async context would **not** be visible to the
operation. Set it as the first thing inside the closure, where `request` is moved
(the daemon server holds the one process `Observer`, built in `crate-core-impl.md`
§4.1, and clones it into the closure):

```rust
let task = tokio::task::spawn_blocking(move || {
    let ctx = TraceContext { trace: Arc::from(request.request_id.as_str()), parent: None };
    let response = observer.with_context(ctx, || {
        let dispatch = observer.span("daemon.dispatch");   // root span (d-0)
        dispatch.attr("op", request.op.clone());           // attrs via the guard (crate-core §3.4)
        let response = sandbox_runtime::dispatch_operation(&operations, &request);
        if response.is_fault() { dispatch.status(SpanStatus::Error); }  // root reflects a fault Response (M2)
        response
    });                                                    // `dispatch` drops here → writes d-0, still in ctx
    response.into_json_value()
});
```

`dispatch_operation` runs synchronously on that one thread, so every in-daemon
sync span below it nests into the per-request tree via the thread-local parent
(`README.md` §5). The `daemon.dispatch` guard is the root (`d-0` in Case A); its
`attrs` carry `op = request.op`, set with `.attr()` — without that method the root
could not record `op`, which is why the sync guard carries attrs (`crate-core-impl.md`
§3.4 / C1).

The `daemon.dispatch` guard **cannot** use the `obs.scope` combinator (M2) — dispatch
returns a `Response`, not a `Result` — so it inspects the returned `Response` once and
calls `.status(SpanStatus::Error)` on a fault, via a one-line `Response::is_fault()`
(`self.value.get("error").is_some()` on the `Response` value wrapper). Without this the
root renders a green ✓ on every rejected op (active-command rejection, invalid-arg exec,
command-not-found), and for a synchronous rejection the root is often the *only* span on
the trace (M2).

**Dispatch duration for poll-loop ops (m13).** A `daemon.dispatch op=write_command_stdin`
(or `read_command_output`) span measures the `yield_time_ms` poll window, **not** the
write/read cost — read it as the yield window, not the I/O. A `Ctrl-D` that ends a
one-shot attributes the teardown tail to the **originating exec trace** by design (the
model is a tree, not a DAG; write→termination causality is not a parent edge). If write
intent must be greppable, emit at most a `command.signaled` event under the dispatch span
with `{command_session_id, kill}` (it links by attr, not parent) — do **not** add
span-links now. These single-node poll-loop traces are otherwise low-value and are
mitigated by config-gating + the M2 root-status fix (no more misleading green).

---

## 3. Wire the `Observer` into the runtime

The runtime crates do **not** depend on `sandbox-observability` today
(`operation/Cargo.toml`, `layerstack/Cargo.toml`, `namespace-execution/Cargo.toml`
have no obs dep). This is the slice where they gain it — exactly as
`layerstack-impl.md` §4 deferred ("the runtime does not gain an obs-crate
dependency in this slice; only the span phase does").

- Add `sandbox-observability.workspace = true` to `operation`, `layerstack`,
  `namespace-execution`, and `workspace` (the crates with emit seams — `workspace`
  for the one sync mount guard, C1). The obs crate stays a leaf
  (`crate-core-impl.md` §5); the edge points **into** it, graph stays acyclic.
- **Repoint the boundary test.** `operation/tests/observability_snapshot.rs:91-95`
  (`runtime_observability_snapshot_keeps_observability_crate_out`) asserts the
  operation manifest contains neither `sandbox-observability` nor `rusqlite`. The
  `sandbox-observability` half is now intentionally false. Repoint per `README.md`
  §8.6: keep the `rusqlite` assertion (the runtime must still never pull SQLite),
  drop the `sandbox-observability` assertion, and rely on the obs crate's own
  `dependency_guard.rs` (leaf forbids `runtime`/`daemon`/`manager`) as the
  canonical boundary. Final wording lands in `removal-and-phaseb-impl.md` §A.

**Threading the `Observer` to the seams.** The daemon builds the one process
`Observer` (proc `record::proc::DAEMON`) in `crate-core-impl.md` §4.1; the runtime gets a **clone**
of that same `Observer` — there is no per-component handle, because there is no
per-record `component` (`crate-core-impl.md` §2.1/§3.4). The clone is held as an
`Arc<Observer>` (or `Observer`, it is `Clone`) and threaded into **every emitting
service** — `CommandOperationService` (`core.rs:21-26`), the workspace service, and
the layerstack service — through the operations-assembly constructor. (This is a
wider wiring change than one struct field: each emitting service gains the handle;
pass it once into the operations builder and hand clones down.) Sharing one
`Observer` (one `Sink`, one `SpanIds`, one thread-local context) is what makes the
daemon and runtime spans one id sequence under one `<proc>` and lets a runtime span
nest under the daemon's `daemon.dispatch` parent (`crate-core-impl.md` §3.4) — two
independent `Observer`s would collide on `d-0` and lose the parent link.

---

## 4. The async exec span — `SpanRegistry` as the engine's terminal hook

`ExecutionObserver` (`namespace-execution/src/types.rs:19-27`) is wired as
`NoopObserver` (`operation/src/command/service/core.rs:34`, `Arc::new(NoopObserver)`).
Replace the whole interface with the generic `TerminalHook<K>` from the obs leaf
(`crate-core-impl.md` §3.4); the recording side is the `SpanRegistry` itself (m1), not
a bespoke adapter:

- **Engine swap.** `NamespaceExecutionEngine` holds `observer: Arc<dyn
  ExecutionObserver>` (`engine.rs:21`). Rename the field to `terminal_hook` and change
  its type to `Arc<dyn TerminalHook<NamespaceExecutionId>>`. Delete the old
  `ExecutionObserver` trait + `NoopObserver` from `types.rs` (the generic
  `TerminalHook<K>`/`NoopHook` now live in the leaf). The **second** mount engine in the
  `workspace` crate keeps a `NoopHook` — its mount is instrumented by a sync guard, not
  this hook (C1, §5); there is no observer swap on that engine.
- **Drop `on_running`.** The engine calls `self.terminal_hook.on_running(&id)`
  (`:105` shell, `:135` mount). Remove both — `TerminalHook` has no `on_running`
  (one-record-at-completion needs only the terminal edge; live "running" state is the
  engine's own `ExecutionRegistry`, `crate-core-impl.md` §3.4). This also removes the
  old observer's only non-terminal duty.
- **Record at child-exit — call `on_terminal` before finalize.** In `spawn_watcher`
  (`engine.rs:163-197`) the watcher runs `child.wait_completion()` (`:175`), then
  `finalize` (`:183`), then `terminal_hook.on_terminal` (`:195`). Move the `on_terminal`
  call to **right after `wait_completion`, before finalize**, and let it self-stamp:
  the span's true end is child-exit, so recording it there gives the correct `ts`/`dur`
  with **no timestamp argument**. The span's `status` is then the execution's own
  outcome; a teardown/finalize failure lands on the `workspace_session.destroy` span
  (§7), not on this one — the intended attribution. The engine's own live
  `ExecutionRegistry` keeps its finalize-adjusted status (Case B unaffected):

```rust
let wait_result = child.wait_completion();
let (result, status, exit_code) = match wait_result {
    Ok(run_result) => {
        let outcome = RunnerOutcome::new(run_result).with_cancelled(cancelled.load(Acquire));
        let exec_status = outcome.status();
        let exit_code   = Some(outcome.exit_code());
        terminal_hook.on_terminal(&id, exec_status.to_span_status(), exit_code);  // self-stamps now() == child-exit
        let result = mount_exit_error(mount_error_mode, &outcome)            // teardown runs AFTER the span is recorded
            .map_or_else(|| finalize_outcome(finalize, outcome), Err);
        let live_status = if result.is_ok() { exec_status } else { NamespaceExecutionTerminalStatus::Error };
        (result, live_status, exit_code)
    }
    Err(error) => { terminal_hook.on_terminal(&id, SpanStatus::Error, None); (Err(error), NamespaceExecutionTerminalStatus::Error, None) }
};
registry.complete(&id, status, exit_code);    // ExecutionRegistry live state — NOT the span store's `record` (C2)
promise.resolve(result);
```

  `NamespaceExecutionTerminalStatus` (`Ok|Error|TimedOut|Cancelled`, `shell.rs:8-14`)
  maps to `SpanStatus` via a small `to_span_status()` on the local type (avoids an
  orphan `From` impl).

With m1's blanket `impl<K> TerminalHook<K> for SpanRegistry<K>` (`crate-core-impl.md`
§3.4), the `SpanRegistry<NamespaceExecutionId>` **is** the engine's terminal hook —
there is no bespoke `NamespaceExecutionObserver` adapter struct. The blanket impl folds
the generic terminal payload (`async:true`, `exit_code`) into the one Span record at
`on_terminal`, which calls `record`:

```rust
impl<K> TerminalHook<K> for SpanRegistry<K> {
    fn on_terminal(&self, id: &K, status: SpanStatus, exit_code: Option<i64>) {
        let mut attrs = Attrs::new();
        attrs.insert("async".into(), true.into());
        if let Some(code) = exit_code { attrs.insert("exit_code".into(), code.into()); }
        self.record(id, status, attrs);   // pop + self-stamp end + write the one Span
    }
}
```

The namespace's `exec_id = id.0` is the single domain residual; the wiring records it
from the span key (m1: "the adapter shrinks to that one attr or disappears"). The caller
drives the registry directly — `spans.launch(id, ctx, name, || …)` at launch (M3),
`on_terminal` → `record` at child-exit. No bespoke map, lock, or `remove`-then-`end`
dance: it is all in `SpanRegistry`, so the next async source (background compaction, GC,
prefetch) reuses the same primitive with a different `K` and the **same** blanket hook —
the extensibility the rework is built for (`crate-core-impl.md` §3.4/§3.6).

**Wiring (one registry, both sides).** `core.rs:new` builds
`let exec_obs = Arc::new(SpanRegistry::new(observer.clone()));` and wires the **same**
`exec_obs` to both the engine (as `Arc<dyn TerminalHook<NamespaceExecutionId>>`, via the
blanket impl m1) and the service — `with_engine(workspace, config, engine, exec_obs)`
(m4) — so `launch` and `on_terminal` share one registry instance and a span can never
park where nothing records. The engine only ever calls the trait method `on_terminal`;
the service calls `launch`.

- **Launch the shell span (M3).** `exec_command` (`exec_command.rs:35-65`) allocates the
  id (`:35`) and calls `run_shell_interactive(..., id.clone(), ...)` (`:59`). Wrap that
  launch in `exec_obs.launch(id.clone(), obs.context(), "namespace.exec.shell", |child_ctx| …)`
  (thread-local `TraceContext`, §2): `launch` opens the parked span when context exists,
  passes the child ctx to the launch body, and `cancel`s the parked span **internally** if
  the body returns `Err` before the watcher exists (`exec_command.rs:66-76`) — so there is
  no manual `register`/`cancel` dance and no bogus swept `cancelled`. **The mount is no
  longer launched here** — it is a sync guard (C1, §5). The engine stays context-agnostic
  and only forwards the id at `on_terminal`.
- **Phase B handoff (M7).** `open` (invoked inside `launch`) mints the span id **at
  launch** and returns a child `TraceContext { trace, parent: <new id> }`. Phase A ignores
  the closure argument (no fork yet); Phase B passes that child ctx into `build_request`,
  letting the namespace-process stamp `np-0.parent = the shell span id (d-5)` — which
  exists at launch because the id is minted there, not at completion
  (`removal-and-phaseb-impl.md` §B.2).
- **Record / write.** `on_terminal` (watcher thread, right after child-exit) calls
  `exec_obs.record(id, …)`, which pops the parked span and writes the **one** span record
  — on whichever thread finishes the work (`README.md` §3.1), self-stamping the end. If a
  watcher dies before `on_terminal`, the registry's shutdown sweep emits the span as
  `cancelled` at process teardown (`crate-core-impl.md` §3.4) — a backstop, not a
  per-span guarantee. This is Case A's `d-5` (the shell, which lands at ~4.27 s, after
  the call returned at ~1.05 s); `d-4` (`mount_overlay`) is now a sync guard (§5), not
  this hook.
- `attrs`: `exec_id = id.0`, `async = true`, `exit_code` (in attrs, not a field —
  `crate-core-impl.md` §2.1). No command line (redaction, `crate-core-impl.md` §3.4).

---

## 5. Sync span seams (the re-added dotted labels)

Add `obs.span(name)` guards at the lifecycle edges below. Names follow the §6 label
grammar — the former timing labels, renamed where it applies (`README.md` §8.2). Wrap a
**fallible** seam (one that returns `Result`) in the `obs.scope(name, |span| …)`
combinator (`crate-core-impl.md` §3.4 / M2) so an early-return or explicit `Err`
self-sets `status:"error"` before the guard drops — a bare `obs.span(name)` guard that
just drops records `completed`, which is correct only for an infallible
success-with-no-facts scope. Attach facts with `.attr()`; an explicit `Err` arm can chain
`.status(SpanStatus::Error)` (m2, now `-> &Self`). The Case A waterfall (`README.md`
§4.1) fixes the minimum set:

| Span `name` | Site | Notes |
|---|---|---|
| `daemon.dispatch` | `dispatch.rs` blocking closure (§2) | root (`d-0`); `.attr("op", …)`; fault `Response` → `.status(Error)` (M2, §2) |
| `command.exec` | `exec_command.rs:18` (`exec_command`) | `d-1`; span label is `command.exec`, but the op name / `attrs.op` stay `exec_command` (M10); `.attr("one_shot", …)`; the sync call body (returns at yield) |
| `workspace_session.create` | `create_workspace_session.rs:9` | `d-2`; `lease.acquired` (§6 event) + the mount span nest inside it (C1) |
| `namespace.exec.mount_overlay` | `workspace/src/namespace/setns_runner.rs:37` (wrap the `.wait()`) | `d-4`; **sync** `SpanGuard`, status from the `wait()` `Result`; nested under `workspace_session.create` (C1/M8); the `d-3` slot is vacant after dropping `workspace.create` |
| `workspace_session.capture_changes` | `capture_session_changes.rs:7` | `d-6`; one-shot finalize tail (§7), parent `d-1` |
| `workspace_session.destroy` | `destroy_session.rs:7` | `d-8`; one-shot finalize tail (§7), parent `d-1` |

The engine-side `namespace.exec.shell` span is async (§4), not a sync guard;
`namespace.exec.mount_overlay` is now a **sync** guard (above, C1). Other former labels
(`runtime.exec.*`, `namespace.runner.*`, `workspace.destroy.*`) may be added as nested
guards where cheap, but the table above is the contract the integration test asserts.
Each guard nests automatically via the thread-local parent set by its enclosing guard.

---

## 6. Layerstack domain events (Phase 4)

With the runtime now depending on the obs leaf (§3), emit the lease facts as `event`
records. They nest under the enclosing span via the thread-local parent, so each is a
plain `obs.event(name, attrs)` — no `ctx` threading. **Label grammar (M10):** events are
`subsystem.fact` (past-tense), spans are `subsystem[.area].action` (imperative); state
this rule next to `record::names` so the on-disk vocabulary does not drift. The Case A
lease facts are `lease.acquired` (at session create) and `lease.released` (at one-shot
destroy after publish), served as
`events` view rows (`cli-observability.md` §3.4, via `Reader::raw(kind=event, name=…)`,
`crate-core-impl.md` §3.3).

| Event `name` | Site | `attrs` |
|---|---|---|
| `lease.acquired` | `stack/mod.rs:acquire_snapshot` (after `leases.acquire`, `:78-81`) | `revision` (= manifest newest layer / version) |
| `lease.released` | `cleanup.rs:release_lease_locked` after `leases.release` (`:16`) | `revision` of the released lease (Case A releases the original `r5` lease even when the publish span creates `r6`) |

**`layerstack.publish` is a sync span, not an event (M9).** `publish_layer_unlocked`
(`publish.rs:65-116`) does real variable-duration I/O (per-file `fsync`, `fsync_dir`,
`rename`, `write_manifest`), so model the publish boundary (`publish_validated_changes`,
`publish.rs:41-46`) as a **sync `SpanGuard`** named `layerstack.publish`, with
`attrs{base, revision, layers_added, bytes, no_op}`, and `status=error` +
`attrs.reason="manifest_conflict"` on the `ManifestConflict` path (`publish.rs:89-97`) —
this **folds the deleted `layerstack.publish_rejected` event** into the span's status.
The publish span fires in Case A's one-shot finalization (`exec_command.rs:finalize_one_shot`):
capture session changes, publish, refresh the session handle on success, then destroy. The
cross-trace publish audit therefore moves from `events --name layerstack.publish` to
`raw --kind span --name layerstack.publish` (`cli-observability.md` §4.3, M9); the event
mechanism still serves `lease.*`.

- `layers_added` / `bytes`: compute at the operation boundary before moving the captured
  changes into `publish_changes`, or add the values to `PublishChangesResult`; the current
  result exposes `no_op` and `revision` but not byte/layer counts.
- **One trace path for both session kinds.** These events fire under a span on a
  thread whose thread-local is set: the dispatch thread for a persistent session, and
  the watcher thread for the one-shot tail — which §7 wraps in `with_context` so its
  thread-local is set too. So both cases use the *same* plain `obs.event(name, attrs)`
  reading the thread-local parent; there is **no** one-shot-vs-persistent bifurcation
  and no captured-ctx threading into the events (the captured ctx is set once, at the
  top of the tail, §7). In Case A `lease.released` lands under `d-8` (the destroy span
  pushed itself as the thread-local parent), while `lease.acquired` lands under
  `workspace_session.create` (`d-2`) at create time (C1).
- Redaction: revisions/ids only — never source paths.

---

## 7. One-shot finalize — capture the trace once

The one-shot finalize (`exec_command.rs:finalize_closure:164-178`) runs inside the
watcher thread and today captures only `one_shot_handler` + `workspace`
(`:172`), then calls `workspace.destroy_session(...)` (`:175`). It **captures no
trace context** — so its `destroy` span and the `lease.released` event would
land under no trace (`README.md` §5, §9.3).

Capture the request's `TraceContext` into the closure at creation time (on the
dispatch thread, where the thread-local is still set — its `parent` is the
`command.exec` span, `d-1`), and set the watcher's thread-local from it **once**, so
the `destroy` span and every event under it nest normally:

```rust
let obs = self.obs.clone();            // thread the Observer into the closure (M4)
let ctx = obs.context();               // snapshot on the dispatch thread (parent = command.exec, d-1)
let one_shot_handler = self.one_shot.then(|| self.handler.clone());
move |_result| {
    if let Some(handler) = one_shot_handler {   // one-shot gate ONLY — not an observability gate (M4)
        obs.with_context(ctx, || {     // with_context accepts Option<TraceContext> (M4): None → teardown still runs, uncorrelated
            let _ = workspace.destroy_session(handler, DestroyWorkspaceRequest::default());
        });
    }
}
```

Threading the `Observer` in and making `with_context` accept `Option<TraceContext>`
(M4) is the load-bearing fix: the old `if let (Some(handler), Some(ctx))` form
**skipped `destroy_session` entirely** when `ctx` was `None`, coupling a
correctness-critical teardown to observability presence — a breach of "instrumentation
cannot change behavior." Now only the one-shot handler gates the teardown; a `None` ctx
just runs it uncorrelated.

Inside `destroy_session`, `obs.span("workspace_session.destroy")` reads the
thread-local (parent = `d-1`) and pushes `d-6`; the `lease.released` event
(`obs.event(...)`, §6) then reads the thread-local and attaches under `d-6`. So
**`workspace_session.destroy` (`d-6`) is a child of `command.exec` (`d-1`)** — the
one-shot teardown belongs to the originating command — and the event nests under
`d-6`. (This is the corrected Case A shape: the teardown is a sibling of
`namespace.exec.shell` under `d-1`, not a child of the shell span; `README.md` §4.1 /
`cli-observability.md` §3.3 render it that way.)

This in-process threading is what makes Case A's async tail (`d-5`, `d-6`, and the
`lease.released` event) correlate in Phase A — distinct from the cross-process
threading deferred to Phase B (`removal-and-phaseb-impl.md`).

---

## 8. Boundary

- `operation`/`layerstack`/`namespace-execution`/`workspace` gain a dependency on the
  obs **leaf** (§3; `workspace` for the one sync mount guard, C1); the obs crate gains
  nothing (still `serde`/`serde_json`/`thiserror`). Edges point into the leaf; the graph
  stays acyclic. The generic `TerminalHook<K>` interface lives in the leaf and is
  consumed here at `K = NamespaceExecutionId`.
- The operation boundary test is repointed (§3); the obs `dependency_guard.rs`
  (forbids `runtime`/`daemon`/`manager`) remains the canonical leaf invariant.
- Emit is config-gated (`crate-core-impl.md` §3.5) and never-fail; instrumentation
  cannot change operation behavior or error paths.

---

## 9. Rollout (ordered)

1. **Trace plumbing** (§2–§3) — obs deps + repointed boundary test; thread-local
   set in the dispatch blocking closure; `daemon.dispatch` root span (`.attr("op")`,
   fault-`Response` → `.status(Error)`, M2); `Observer` clone threaded into the runtime
   services.
2. **Async exec span** (§4) — generic `TerminalHook<K>` swap in the engine (field
   `observer` → `terminal_hook`, drop `on_running`, record at child-exit before finalize);
   the `SpanRegistry` itself is the hook via the blanket impl (m1, replacing `NoopHook`);
   the shell launch uses `SpanRegistry::launch` (M3, folding register+cancel); `on_terminal`
   → `record` writes the span. `mount_overlay` is no longer launched here — it is a sync
   guard (step 3).
3. **Sync seams** (§5) — the guard set in the §5 table (incl. the sync `mount_overlay`),
   with `.attr()`/`.status()`; fallible seams via `obs.scope` (M2).
4. **One-shot finalize capture** (§7) — `Observer` + `TraceContext` snapshot into the
   finalize closure + `with_context` (Option-accepting) once around `destroy_session`; no
   teardown-skip when ctx is `None` (M4).
5. **Layerstack events** (§6) — `lease.acquired`/`lease.released` events nesting via the
   thread-local parent (publish is now the `layerstack.publish` span, M9, not in Case A).

Steps build strictly on `crate-core-impl.md`; nothing here touches SQLite (already
gone) or the cross-process fork (Phase B).

---

## 10. Testing

- **Integration (Case A):** an `exec_command` with no existing session produces, in
  one `observability.ndjson`: one record per span; `namespace.exec.shell` written on
  terminal under `req-…` with `start = ts - dur_ms` before its own `ts` and **before**
  the `lease.released` tail (recorded at child-exit, before finalize); the finalize
  `workspace_session.destroy` span has **parent `d-1`** (the `command.exec` span) with
  `lease.released` nested under it; `lease.acquired` + the sync
  `namespace.exec.mount_overlay` span (`d-4`) nest under `workspace_session.create`
  (`d-2`); the `d-3` slot is vacant (no `workspace.create`); there is **no**
  `layerstack.publish` (evict-only, C1/C3) — all sharing the trace. `Reader::trace`
  renders the corrected `README.md` §4.1 / `cli-observability.md` §3.3 shape (offsets,
  nesting, `[async]` bars).
- **Sync outcome:** a fallible sync seam whose body returns `Err` records
  `status:"error"` (via `obs.scope`/`.status`, M2), a fault `Response` flips the
  `daemon.dispatch` root to `error` (M2), and `daemon.dispatch`/`command.exec` carry
  their `.attr()` facts (`op`, `one_shot`).
- **Case B (live in-flight):** while a persistent-session command runs, the async
  shell span has **no** record yet; the `snapshot` view shows it in-flight from the
  live registry (`crate-core-impl.md` §4.1), with no log dependency.
- **Events view:** `events --name lease.acquired` (i.e. `raw(kind=event, name=…)`)
  returns the flat cross-trace lease stream with `revision`; the publish audit is now a
  span query, `raw --kind span --name layerstack.publish` (M9, `cli-observability.md`
  §3.4/§4.3).
- **Launch failure:** a launch that errors before the watcher exists writes **no**
  span for that exec id (`cancel`), not a bogus `cancelled`.
- **Never-fail:** with `observability.enabled = false`, the same `exec_command`
  runs identically and writes nothing; a `Sink` error mid-exec does not surface to
  the command result.
- **Gates:** `cargo build`, `cargo test`, `cargo clippy --all-targets`,
  `cargo fmt` clean; the repointed boundary test green.
