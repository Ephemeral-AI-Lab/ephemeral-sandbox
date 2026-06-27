# Adversarial Review Prompt — Post-Fix Regression & New-Decision Audit

Use this to drive a skeptical, **multi-agent** review of the observability rework
**after** the 41 findings from the first review (`review-prompt-arch.md`) were applied
across all six design docs. Its job is **not** to re-find the original 41 — it is to
(a) verify those fixes actually landed, completely and *consistently across every doc*;
(b) adversarially attack the **new decisions the fixes introduced** (which were *not*
themselves reviewed); and (c) take a fresh skeptical pass for anything the first review
missed or the fixes broke. **A review that rubber-stamps the fixes is a failed review.**

Paste into a fresh orchestrator with repo access. It is self-contained.

---

## Role

You are an adversarial design reviewer. The previous round produced 41 findings and
they were applied — renames, deletions, an API-surface reshape, a re-rendered canonical
trace, and several brand-new constructs (`Observer::scope`, `SpanRegistry::launch`,
mint-at-launch `open`, a blanket `TerminalHook` impl, `layerstack.publish` as a span,
mount-as-sync). Fixes are exactly where regressions hide: a rename applied in four docs
but not the fifth; a deleted record still rendered in one example; two docs that now
disagree on the same trace; a "simpler" combinator that added ceremony; a new return
value that quietly breaks an invariant. Your north star is unchanged — **simplicity &
extensibility over complexity, ergonomics over cleverness** — but your *target* is the
delta. Be concrete: cite the doc, the section, the line/record, propose the exact
correction, state the cost. Bias to subtraction. Assume the author is convinced the
fixes are clean; your value is the inconsistency or the bad new trade they didn't see.

## Multi-agent orchestration (how to run this)

1. **Phase 1 — area reviewers (parallel, 5 agents).** One agent per Area 1–5. Each gets
   the area's targets, the *Settled* (do-not-relitigate) block, and *What to read*. Each
   returns **Findings + Proposed change** in the required format. The Area 5 agent
   additionally produces the **consolidated cross-doc diff punch-list**.
2. **Phase 2 — adversarial verification (parallel, per non-trivial finding).** Spawn ≥3
   skeptics per finding, each on a distinct lens — *is it really inconsistent or did I
   misread the doc?* / *is the proposed change actually simpler, or lateral?* / *does the
   "regression" break a Settled invariant or just offend taste?* Drop any finding a
   majority refute.
3. **Phase 3 — synthesis (1 agent).** Dedupe across areas (naming/consistency overlap),
   resolve conflicts, emit the consolidated report + the punch-list, and render the
   **implementation-readiness verdict**.

Keep area agents blind to each other in Phase 1; only Phase 3 sees all.

## What to read

1. **The six edited specs** (the review target), under `docs/observability-rework/`:
   - `crate-core-impl.md` — record model §2, `Sink`/`Reader` §3.2–3.3, the emit API
     §3.4 (`Observer`/`SpanGuard`/`SpanRegistry`/`TerminalHook`/`NoopHook`/`TraceContext`),
     the §3.7 API reference table, the open/closed contract §3.6, config gate §3.5.
   - `span-trace-impl.md` — the three shapes §1, trace-id threading §2, runtime wiring
     §3–§4, sync seams §5, layerstack §6, one-shot finalize §7.
   - `README.md` — record model §3, Case A/B/C §4, emit seams §5, crate rework §6,
     fetch §7, rollout §9, testing §10.
   - `cli-observability.md` — the CLI surface + rendered trace/cgroup/events/raw examples.
   - `cli-observability-examples.md` — concrete span/trace for the six operations.
   - `removal-and-phaseb-impl.md` — Phase B `np-*` cross-process span.
2. **The first-round intent**, to check fidelity of the applied fixes:
   - `review-prompt-arch.md` (the original review's scope + fixed intent).
   - Optionally `git log -p -- docs/observability-rework/` / `git diff` to see exactly
     what the fixes changed (the new constructs are the diff).
3. **The code the specs claim to instrument**, to check the new structural decisions are
   faithful to reality:
   - mount: `workspace/src/namespace/setns_runner.rs:37` (the `.wait()`ed `mount_overlay`).
   - publish: `layerstack/.../publish_changes.rs`, `publish.rs` (caller reachability).
   - teardown: the one-shot finalize/destroy path (`exec_command.rs`, `destroy_session.rs`).
   - engine hook: `namespace-execution/src/{types.rs,engine.rs,registry.rs}`.

## Settled — do NOT relitigate (the record-model floor)

These are the load-bearing constraints from round one; do not re-argue them. Everything
*else* the fixes introduced is in scope.

- Leaf crate; append-only NDJSON; one write per line; **one record per span** at
  completion (`ts`+`dur_ms`); emit never fails the op; config-gated; time-free
  self-stamping (no method takes a clock).
- Record envelope = `ts` + `trace` (+`kind`/`span`/`parent`); **no** per-record
  `sandbox`/`component`/`pid`; `exit_code` in `attrs`.
- `trace = Request.request_id`; sync nesting via a thread-local parent.
- The first review's *direction* is settled (drop-`component`, generic `SpanObserver<K>`
  seam, etc.). What is **open for attack** is whether the *applied fixes* are correct,
  complete, mutually consistent, and whether the **new constructs** are the right shape.

If a fix breaches the floor above, that itself is a finding.

---

## Areas under review

Each area MUST produce **Findings → Proposed change** and lead with a one-line
**verdict**: `clean` / `inconsistent` / `regressed`.

### Area 1 — Fix completeness & cross-doc consistency (the regression hunt)

The single highest-value area. Treat the six docs as one corpus that must agree.

- **Rename completeness.** For each rename — `exec_command`→`command.exec` (span name
  only; op name stays), `ns_runner.shell.spawn_child`→`namespace.runner.spawn_child`,
  `SpanObserver`→`TerminalHook`, `NoopObserver`→`NoopHook` (leaf type only — the
  *existing* engine `ExecutionObserver`/`NoopObserver` code is a separate thing),
  `set_status`→`status`, `SpanRegistry::complete`→`record`, `event_in`→deleted,
  `layerstack.publish_rejected`→deleted, `exec.terminal`→deleted — confirm it landed in
  **every** doc and **every** occurrence (prose, tables, code blocks, NDJSON `name`
  fields, rendered waterfalls). Flag any surviving old token that is *not* an explicit
  deletion/rename note.
- **The three Case A renderings must agree.** Diff the one-shot `exec_command` trace as
  it appears in `README.md` §4.1, `cli-observability.md` §4.2, and
  `cli-observability-examples.md` §1A: same spans, same `<proc>-<seq>` ids, same
  `parent` edges, same nesting, same `[async]`/sync marking, same statuses, coherent
  offsets. Any divergence (e.g. a vacant-id slot in one but not another, a different
  `dur_ms`, mount marked `[async]` in one) is a finding.
- **NDJSON internal coherence.** For every raw block in every doc: each `parent`
  resolves to a `span` id in the same `trace`; `start = ts − dur_ms` matches the
  rendered offset; append order ≈ `ts` order; `exit_code` only in `attrs`; no
  `sandbox`/`component`/`pid`; a child's bar never longer than its parent's.
- **Spec ↔ call-site agreement.** The §3.7 API table in `crate-core-impl.md` must match
  how the methods are *used* in `span-trace-impl.md`, `README.md` §5/§6, and the
  examples doc — signatures, arg names (`id` vs `key`), `impl Into<Value>` everywhere,
  `with_context` accepting `Option`, `open` returning `TraceContext`.

### Area 2 — The new naming decisions

The fixes introduced names that were never themselves reviewed. Attack them.

- **`layerstack.publish` as a span (imperative).** The first review said "publish →
  span" (M9) *and* "rename to `layerstack.published`" (M10) — contradictory, since the
  span grammar is imperative. The fix chose imperative `layerstack.publish`. Is that the
  right call? Does it collide with the now-deleted event of the same name (any reader
  that keys on name)? Is the grammar rule (spans = `subsystem[.area].action` imperative;
  events = `subsystem.fact` past-tense) now applied **uniformly** to every on-disk
  label? Audit the full vocabulary.
- **`ObserverConfig` (leaf-owned) vs `ObservabilityConfig` (sandbox-config).** The fix
  introduced a leaf-local `ObserverConfig { proc, enabled }` because the leaf can't
  import `sandbox-config`. Is the two-type story (daemon maps its config section + a
  `record::proc` const into the leaf type) clear, or confusing? Is `ObserverConfig` even
  the right name, or does it collide conceptually with `Observer`?
- **Did `TerminalHook`/`NoopHook` fully resolve the `Observer`/`SpanObserver`
  collision?** `NamespaceExecutionObserver` was kept. With the engine hook now
  `TerminalHook` and the facade `Observer`, does "…Observer" still mislead at the one
  remaining site, or is it now unambiguous?

### Area 3 — The new API constructs: scope / launch / mint-at-launch / blanket impl

The fixes added machinery. Machinery is where ergonomic regressions live.

- **`Observer::scope(name, |g| -> Result<T,E>)`.** Does it actually remove the
  forgot-to-`set_status(Error)` footgun *without* adding ceremony? Write the real call
  site for a fallible sync op (e.g. `workspace_session.create`) both ways and compare.
  Does it compose with `?`, early returns, and explicit `Err` arms? Is the bare `span()`
  still there for the infallible case (it must be)?
- **`SpanRegistry::launch(id, ctx, name, f)`.** Does it correctly fold register +
  cancel-on-`Err`, and is the residual `open`/`cancel` surface now smaller (ideally
  non-public)? Does the one real launch site (the shell exec) actually get simpler, or
  did the closure threading add weight?
- **Mint-at-launch `open(...) -> TraceContext` (M7).** Allocating the span id at `open`
  and returning a child `TraceContext` — does it break **one-record-per-span**, the
  no-public-`AsyncSpan` rule, or `cancel` semantics (a span id now exists before any
  record is written; what does a `cancel` after a returned-ctx leak)? Trace the Phase B
  handoff end-to-end (`open` → returned ctx → `build_request` stamps `np-0.parent`).
- **Blanket `impl<K> TerminalHook<K> for SpanRegistry<K>`.** Is it coherent with (a) how
  the engine calls `on_terminal`, (b) the m4 "same registry instance to service +
  engine" wiring, and (c) folding `exit_code`/`async:true` into `record`? Or does the
  blanket impl + the kept `NamespaceExecutionObserver` now overlap/conflict?
- **`with_context(impl Into<Option<TraceContext>>)`.** Did accepting `Option` quietly
  reintroduce a silent-drop footgun (a `None` that drops a span/event out of the trace
  with no error)? Is the behavior on `None` specified?

### Area 4 — The new structural decisions in the trace shapes

- **Mount as a sync `SpanGuard`.** The fix models `mount_overlay` as sync because it is
  `.wait()`ed. Verify against `setns_runner.rs:37` that there is genuinely no async
  escape and the `.wait()` truly blocks the dispatch thread. Does dropping its async-ness
  lose any signal? Does the status-from-`wait()`-`Result` path actually thread through?
- **Dropping `workspace.create`.** Is the claim "near-coextensive with
  `workspace_session.create`" true, or does the collapsed span hide a real sub-duration
  (the mount blocks inside it)? Confirm the vacant-id-slot story is rendered identically
  everywhere (and that nothing still parents to the dropped span's old id).
- **Evict-only one-shot tail (no publish).** Is "a one-shot discards its diff, never
  publishes" faithful to the real teardown code? Confirm `lease.released` carries the
  *same* revision as `lease.acquired` in every Case A rendering.
- **`layerstack.publish` span is still an orphan seam.** The first review flagged
  `publish_changes` has **no production caller** (A5-1). The fix modeled it as a span but
  did it wire a real flow, or is the marquee publish example still emitted by nothing?
  If still orphaned, the doc must say so at every publish example.

### Area 5 — Fresh pass + the consolidated punch-list (deliverable)

- A genuinely fresh skeptical read: anything the first review missed, any *new*
  awkwardness the fixes created, any place two fixes interact badly (e.g. `scope`'s
  `Error`-on-`Err` vs the dispatch-level `Response::is_fault()` path — do they
  double-set or disagree?).
- Is `cli-observability-examples.md` still the canonical reference, and does it agree
  with the now-edited README/cli-observability/crate-core/span-trace on every shared
  artifact? Does any op's trace now read worse than before the fix?
- **Deliverable:** a single, ordered **punch-list of concrete diffs** — file → section
  → exact change — that, applied, makes the corpus fully consistent and
  implementation-ready. Each item: severity, the doc(s) affected, the before/after.

---

## Required output format (per area)

- **Findings** — severity-ordered (Critical → Minor). Each: a one-line claim; the cited
  doc §/line/record; the concrete inconsistency/regression/bad-trade. No prose nitpicks
  that don't change the design or its correctness.
- **Proposed change** — the minimal concrete edit (the exact rename/deletion/re-render,
  or the smaller construct), with the adoption cost.

Lead each area with: `clean` / `inconsistent` / `regressed`.

End with a **Phase 3 synthesis**:
- **Regression list** — fixes that broke or half-broke something (the most important
  output).
- **Residual inconsistencies** — cross-doc disagreements still standing.
- **New-decision verdict** — for each new construct (`scope`, `launch`, mint-at-launch,
  blanket impl, `layerstack.publish` span, `ObserverConfig`, mount-as-sync): keep / adjust
  / revert, with the reason.
- **Implementation-readiness verdict** — are the six specs now internally consistent and
  unambiguous enough to build from? Name every spot that still blocks a clean
  implementation and the cheapest fix.
- **Punch-list** — the Area 5 consolidated diff list.

## Rules of engagement

- Target the **delta**, not the settled floor. Bias to subtraction and consistency.
- Every "this is wrong/inconsistent" ships with the exact correction and its cost.
- No rubber-stamping; no prose-only nitpicks. Only findings that change correctness,
  consistency, risk, or usability.
- A breach of the Settled floor is itself a finding, costed.
