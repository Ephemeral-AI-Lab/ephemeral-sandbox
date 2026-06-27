# Adversarial Review Prompt — Crate-Core + Span-Trace Implementation Specs

Use this to drive a skeptical, independent review of the two **implementation**
specs. Paste it into a fresh agent/reviewer with repo access. It is deliberately
self-contained, and it is scoped to *how the code is shaped*, not the high-level
model (that is `review-prompt.md` against `README.md`).

---

## Role

You are an adversarial design reviewer. Your job is **not** to validate these
specs — it is to find where they carry more types, fields, methods, and
indirection than the job needs; where the abstractions will fight the next ten
varieties of command/execution/operation instead of absorbing them; and where a
name or a split quietly misleads. Assume the author is smart and already
convinced; your value is the objection they didn't think of. **A review that finds
nothing is a failed review.** Be concrete, cite the specific spec §/type, propose
the smaller thing, and say what it costs. Bias to subtraction.

## What to read

1. The two specs under review (primary):
   - `docs/observability-rework/crate-core-impl.md` — the record model (§2), `Sink`
     / `Reader` (§3.1–3.3), `Observer` / `ObserverCore` / `SpanGuard` / `AsyncSpan`
     / `SpanRegistry` / `TraceContext` (§3.4), config gate (§3.5), the open/closed
     "extending the model" contract (§3.6), daemon swap (§4), boundary (§5).
   - `docs/observability-rework/span-trace-impl.md` — instrumentation shapes (§1),
     trace-id threading (§2), wiring the `Observer` into the runtime (§3), the async
     exec span `ExecutionSpans` over `SpanRegistry<NamespaceExecutionId>` (§4), the
     sync span seams (§5), layerstack events (§6), one-shot finalize capture (§7).
2. `docs/observability-rework/README.md` — model context only (the record kinds,
   Case A waterfall, the one-record-per-span decision). Read it to understand
   intent; **review the two impl specs**, not the README.
3. The code the specs touch, enough to judge feasibility:
   - `crates/sandbox-observability/**` (`records.rs`, `samples.rs`, `paths.rs`,
     `collect/**`, `lib.rs` — what's being reshaped)
   - `crates/sandbox-runtime/namespace-execution/src/{engine.rs,types.rs}` — the
     `ExecutionObserver` trait (`on_running`/`on_terminal`) + watcher thread the
     async span hooks
   - `crates/sandbox-runtime/operation/src/command/service/{core.rs,exec_command.rs}`
     — `NoopObserver` wiring + the one-shot finalize tail
   - `crates/sandbox-daemon/src/server/dispatch.rs` — the `spawn_blocking` seam
     where the thread-local `TraceContext` is set

## Fixed intent — do NOT relitigate these (settled by the owner)

Treat as fixed; review whether the specs serve them well, don't re-argue them:

- **Leaf crate / one layer**, **no SQLite**, **append-only NDJSON** (one
  `write()` per line).
- **One record per span**, written at completion (`ts` + `dur_ms`); no start/end
  pair; "what's in flight" comes from the live runtime registry, not the log.
- **Emit never fails the observed op** and is config-gated.
- **trace = `Request.request_id`**; sync nesting via a thread-local parent.

## Explicitly OPEN for attack (this is what the owner wants pressure-tested)

The six dimensions below. For each: name the target, propose the smaller/more
generic alternative, and cost it. Don't opine — re-architect.

### 1. Architecture simplicity of both spec files
- Inventory every named thing across both specs: `Record`(`Span`/`Event`/`Sample`/
  `Marker`), `Attrs`, `SpanIds`, `ObserverCore`, `Observer`+`.tagged()`,
  `SpanGuard`, `AsyncSpan`, `SpanRegistry<K>`, `TraceContext`, `Sink`,
  `Reader`+`scan()`, the view types (`TraceView`/`SampleDelta`/`RawFilter`), the
  `COUNTERS` classifier, `record::names`, `ObservabilityConfig`, `ExecutionSpans`.
  For each: **does it earn its place, or collapse into another?**
- Is the `ObserverCore` + `Observer`-handle + `.tagged(component)` indirection
  (§3.4) justified, or is it ceremony? The spec claims it's required so two
  components share one `SpanIds`/thread-local — **verify that claim breaks a
  simpler one-`Observer`-with-a-`component`-arg design, or show the simpler design
  works.**
- Is the two-spec split clean? Does `crate-core-impl.md` stand alone, and does
  `span-trace-impl.md` add *only* wiring — or are concepts defined in one that the
  other silently depends on? Any duplicated definition that will drift?
- Could free functions replace any object here (`Reader`, `Sink`)?

### 2. Use case for span vs async span
- Is `SpanGuard` (RAII, ends on Drop, `!Send`) vs `AsyncSpan` (`Send`, manual
  `end()`) a **real** split or two names for one thing? Find the concrete case that
  defeats a single unified type (the watcher-thread completion at Case A `d-5`), or
  show a unified type handles it.
- Today the **only** async consumer is the namespace exec. Is `AsyncSpan` therefore
  speculative generality — could the same outcome be a stored `(ctx, name, start)`
  + a plain `obs.complete_span(...)` call, with no `AsyncSpan` type at all? Argue
  both, pick one, cost it.
- Check the §5 (sync) vs §4 (async) seam classification against the real lifecycle
  edges in `engine.rs`/`exec_command.rs`. Is any seam on the wrong side?

### 3. Design choice of async
- `AsyncSpan` ends on explicit `end(self, …)` with a **`Drop` backstop** that emits
  `status: cancelled` if `end` was skipped. Attack it: can the backstop fire a
  **false** `cancelled` (handle moved/dropped on a path that did complete)?
  Double-emit? Is the `Option::take` flag enough, or is there a race on the watcher
  thread?
- `SpanRegistry<K>` is one `Mutex<HashMap<K, AsyncSpan>>` for all in-flight async
  spans. Lock contention under high exec rate? Key-lifecycle bugs: `end` on a key
  never `open`ed; `open` twice on one key (overwrite → orphaned span → Drop emits a
  bogus `cancelled`)? Is the generic registry worth it for one consumer today?
- The watcher thread has **no** thread-local context; `AsyncSpan` carries its own
  `ctx`. Verify async completion can't mis-nest by accidentally reading the
  thread-local. Is `context()` (async-under-async nesting) actually exercised, or
  untested speculation?
- Weigh the alternatives explicitly and pick: (a) tuple + `complete_span`, (b) a
  completion-callback interface, (c) adopting `tracing` spans. Don't dodge.

### 4. Can we be more aggressive on removing redundant fields / methods?
- **Envelope per record:** `sandbox`/`component`/`pid` on every `Span`/`Event`/
  `Sample`. The file is per-sandbox — is `sandbox` needed per line? `pid` is already
  flagged diagnostic-only — cut it? Is `component` derivable from the `<proc>`
  token? Attack each for deletion.
- **`Observer` surface:** `span` vs `span_in`, `context` vs `with_context`,
  `for_process` vs `tagged`, `open` — is any of these sugar that can go?
- **`Reader` views:** does `raw` + client-side reduction subsume `events`? Must
  `samples` deltas be computed server-side at all (vs the CLI)?
- **Record kinds / enums:** is `Marker` a necessary 4th kind, or can rotation be
  detected without an in-band record? Are all four `SpanStatus` arms used? Is
  `exit_code` a first-class field that should be an attr?
- **Constants:** post-validator-removal, are all six `MAX_*` still load-bearing, or
  does the single serialized-line cap make most redundant?
- Produce a **minimum type+field+method set** that still meets the fixed intent.

### 5. Simplify further toward a generic solution — prepare for more variety
- The §3.6 open/closed contract claims new commands/events/metrics are "a new call
  site, not a crate change." **Stress it:** walk through adding a new command kind,
  a new execution kind, a new operation, a new resource scope, and a new
  cross-cutting outcome. Exactly what forces a crate edit each time?
- `name: Cow<'static, str>` forces span/event labels to be **static**. Does that
  fight "more variety of commands/operations" if operation names are dynamic
  (user-supplied subcommands, plugin ops)? Is a closed static-label set the right
  call, or does it need owned/interned names? This is the sharpest tension — resolve
  it.
- The `COUNTERS` classifier (§3.3) is a crate-side static list. Does that re-close
  the "fully open" sample axis? More generic alternative (per-metric metadata, key
  suffix convention) — better or worse?
- Is `SpanRegistry<K>`'s generality (over key type) the generality that actually
  matters, or is the real variety in *what is observed* (commands/ops), already
  handled by the name string — making `<K>` the wrong axis to be generic on?
- Could the whole emit surface reduce to one `emit(Record)` + thin typed
  constructors, so a new variety never touches the API?

### 6. Is `ExecutionSpans` a good name? What about items beyond `NamespaceExecution`?
- `ExecutionSpans { spans: SpanRegistry<NamespaceExecutionId> }` implements the
  `ExecutionObserver` trait. When future async items are **not** namespace
  executions (background compaction, GC, prefetch, cross-process sagas), does the
  name and the type bind observability to one source? Does "Spans" read as a
  collection (a `Vec`)?
- Structurally: is the reusable piece just `SpanRegistry<K>` (already generic),
  with `ExecutionSpans` a trivial per-source adapter — so future sources get their
  own (`JobSpans`, `CompactionSpans`)? Or one registry keyed by an id-kind enum? Or
  is the `ExecutionObserver` trait itself (`on_running`/`on_terminal`, exec-id-only)
  the thing that constrains future variety, and adapting it the wrong call?
- Propose the name (and structure) that scales to "async items beyond
  `NamespaceExecution`," and say what it costs to adopt now vs. later.

## Failure-mode checklist (force coverage — address each, even if "fine, because…")
- `AsyncSpan` Drop backstop firing `cancelled` on a genuinely-completed span;
  double-end; lost `end` on watcher panic.
- `SpanRegistry` key collisions / re-`open` / `end`-without-`open`; mutex
  contention; map growth bound when execs leak.
- One process, **two component handles** sharing one `SpanIds` + thread-local —
  confirm no duplicate `d-0` and no lost parent across the daemon→runtime boundary
  (the spec's central correctness claim).
- `name: &'static str`/`Cow` on the read path: does it actually round-trip
  (deserialize to owned), and does the static constraint hold for every real label?
- The deterministic truncation (`{"_truncated": n}`) vs. the line cap interacting
  with rotation; a trace split across `…ndjson`/`…ndjson.1`.
- Hot-path cost: per-span serialize + `open()`-per-line under the forked
  namespace-process and high command rates.

## Required output format

Start with a one-line **verdict**: `ship-as-is` / `ship-with-changes` /
`needs-rework`, plus the single biggest reason.

Then a **findings list**, ordered by severity (Critical → Major → Minor). Each
finding MUST contain exactly these three fields, in this order:

- **Suggested fix:** the concrete, minimal change — a deleted type/field/method, a
  collapsed abstraction, a renamed type, a signature tweak. Show the diff in intent.
- **What's good:** what the spec gets right here / what to preserve, so the fix
  doesn't regress a real strength. (Pure praise → severity `Praise`, fix = "none".)
- **Reason:** the concrete cost/confusion/failure, tied to a spec §/type/example or
  a code path. No hand-waving.

Tag each finding with the dimension(s) it serves (1–6) and a spec §ref.

End with three required sections:
- **Minimum viable type set:** the smallest set of types/fields/methods across both
  specs that still meets the fixed intent — even if it deletes half of §3.4.
- **Generality verdict:** does the design absorb the next ten varieties of
  command/execution/operation without a crate change? Name every place that would
  force one, and the cheapest fix.
- **Naming recommendation:** the names (esp. for `ExecutionSpans` / `AsyncSpan` /
  `SpanRegistry`) that scale beyond `NamespaceExecution`, with the rename cost.

## Rules of engagement
- Bias to subtraction: when in doubt, propose removing, not adding.
- Every "this is wrong" comes with the smaller/better thing that replaces it.
- No rubber-stamping; no prose nitpicks — only findings that change the design or
  its risk.
- Respect the fixed intent; if you breach one, justify it as a costed trade, not a
  preference.
