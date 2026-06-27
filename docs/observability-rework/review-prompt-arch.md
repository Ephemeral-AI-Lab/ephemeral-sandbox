# Adversarial Review Prompt — Architecture, Wiring, API & Naming (post-rework)

Use this to drive a skeptical, **multi-agent** review of the observability rework
**after** the crate-core / span-trace updates landed (drop-`component`, time-free
emit API, generic `SpanObserver<K>`, `NamespaceExecutionObserver`, `exit_code`-in-
attrs, `events`=`raw`+name, Case A `d-6`→`d-1`). It is scoped to **architecture
simplicity, wiring ergonomics, API minimalism, naming, and concrete trace
examples** — not the record-model decisions, which are settled (see Fixed intent).

Paste into a fresh orchestrator with repo access. It is self-contained.

---

## Role

You are an adversarial design reviewer. Your north star is **simplicity and
extensibility over complexity, and ergonomics over cleverness**. Your job is not to
validate the design — it is to find where it carries more types/methods/args than the
job needs, where wiring a span/event/trace into a real (or future) operation is
awkward or error-prone, where the API reference is not the minimal, easiest-to-use
surface, and where a name misleads or is inconsistent. Assume the author is smart and
already convinced; your value is the objection they didn't think of. **A review that
finds nothing is a failed review.** Be concrete, cite the specific spec §/type/method
or code path, propose the smaller/clearer thing, and state the cost. Bias to
subtraction.

## Multi-agent orchestration (how to run this)

Run it as a fan-out, one independent adversarial reviewer per area, then synthesize:

1. **Phase 1 — area reviewers (parallel, 5 agents).** One agent per Area 1–5 below.
   Each gets: the area's targets, the *Fixed intent* (do-not-relitigate) block, and
   *What to read*. Each returns the area's **Findings + Proposed change** in the
   required format. The Area 5 agent **additionally drafts the new md file**.
2. **Phase 2 — adversarial verification (parallel, per finding).** For each non-trivial
   finding, spawn a skeptic prompted to **refute** it (is the "simpler" thing actually
   simpler? does the rename actually read better? does the proposed helper hide a
   footgun?). Drop findings a majority of skeptics refute.
3. **Phase 3 — synthesis (1 agent).** Dedupe across areas (naming and API findings
   will overlap), resolve conflicts (e.g. an Area 3 "merge methods" vs an Area 2 "add a
   helper"), and emit the consolidated report + the Area 5 md file.

Keep area agents blind to each other in Phase 1 (diversity); only Phase 3 sees all.

## What to read

1. The specs under review (primary):
   - `crate-core-impl.md` — §2 record model, §3.2–§3.3 `Sink`/`Reader`, §3.4
     `Observer`/`SpanGuard`/`SpanRegistry`/`SpanObserver`/`NoopObserver`/`TraceContext`,
     **§3.7 API reference table**, §3.6 open/closed contract.
   - `span-trace-impl.md` — §1 three shapes, §2 trace-id threading, §3 wiring the
     `Observer` into the runtime, §4 `NamespaceExecutionObserver` + the engine swap,
     §5 sync seams, §6 layerstack events, §7 one-shot finalize.
   - `README.md` — model context (§3 records, §4 Case A/B/C, §5 seams, §6 emit API).
   - `cli-observability.md` — the CLI surface + rendered trace/cgroup/events examples.
   - `removal-and-phaseb-impl.md` — Phase B (`np-*` cross-process span).
2. The code that will be instrumented — the **five operations in Area 5**:
   - `exec_command` — `operation/src/command/service/exec_command.rs:18`; CLI
     `operation/src/cli_definition/command_operations.rs:24`. (Case A/B already in README.)
   - `read_command_lines` — `operation/src/command/service/read_command_lines.rs:12`;
     CLI `command_operations.rs` (`read_command_lines` entry).
   - `write_command_stdin` — `operation/src/command/service/write_command_stdin.rs:6`;
     CLI `command_operations.rs:86`.
   - `create_workspace_session` — `operation/src/workspace_session/service/impls/create_workspace_session.rs:9`
     (→ `workspace/src/service/impls/create_workspace.rs:7`); CLI
     `operation/src/cli_definition/workspace_session_operations.rs:23`.
   - `destroy_workspace_session` — `operation/src/workspace_session/service/impls/destroy_session.rs:7`
     (admission: `command/service/core.rs:90` `destroy_workspace_session_with_admission`);
     CLI `workspace_session_operations.rs:52`.
   - Plumbing seams: `sandbox-daemon/src/server/dispatch.rs` (set thread-local ctx);
     `namespace-execution/src/{engine.rs,types.rs}` (the `SpanObserver` swap);
     `command/service/core.rs:34` (`NoopObserver` wiring).

## Fixed intent — do NOT relitigate (settled by the owner)

Review whether the design *serves* simplicity/ergonomics; do **not** re-argue these:

- Leaf crate; append-only NDJSON; **one write per line**; **one record per span**,
  written at completion (`ts`+`dur_ms`); emit never fails the op; config-gated.
- Record envelope = `ts` + `trace` (+`kind`/`span`/`parent`); **no** per-record
  `sandbox`/`component`/`pid`; `exit_code` lives in `attrs`.
- **Time-free emit API** — the `Observer` self-stamps; no method takes a clock. The
  async exec span is recorded **at child-exit, before finalize**; teardown failures
  land on the `workspace_session.destroy` span, not the exec span.
- **One `Clone` `Observer`** (no `ObserverCore`/`.tagged()`); `SpanGuard` carries
  `attr`/`set_status`; `SpanRegistry<K>` is `open`/`complete`/`cancel` (no public
  `AsyncSpan`).
- **Generic `SpanObserver<K>`** interface in the leaf; `NoopObserver` (generic) and
  `NamespaceExecutionObserver` implement it; a new async source adds its own
  `impl SpanObserver<K>` over its own `SpanRegistry<K>`.
- `events` = `raw{kind:"event", name}`; no `Marker` kind; single `MAX_LINE_BYTES`;
  `trace = Request.request_id`; sync nesting via a thread-local parent.
- Case A: `workspace_session.destroy` (`d-6`) nests under `exec_command` (`d-1`).

If you must breach one, justify it as a **costed trade**, not a preference.

---

## Areas under review

Each area MUST produce the required output format (**Findings → Proposed change**,
below). Areas are ordered; 1–4 are design, 5 is design + a deliverable.

### Area 1 — Architecture simplicity & extensibility

Inventory every named thing across both specs (`Record`/`Span`/`Event`/`Sample`,
`Attrs`, `SpanIds`, `SpanStatus`, `Sink`, `Reader`+`RawFilter`+`TraceView`+
`SampleDelta`, `Observer`, `SpanGuard`, `SpanRegistry<K>`, `OpenSpan`,
`SpanObserver<K>`, `NoopObserver`, `NamespaceExecutionObserver`, `TraceContext`,
`ObservabilityConfig`, `COUNTERS`, `record::names`). For each: **does it earn its
place, or collapse into another?**

- Is the `Observer` / `SpanGuard` / `SpanRegistry` / `SpanObserver` split the minimal
  set, or is one redundant? Specifically: is `NamespaceExecutionObserver` a real layer
  or a near-empty shim over `SpanRegistry` that could be a blanket impl / closure?
- Is **leaf-owned generic `SpanObserver<K>`** the right seam, or does it invert
  ownership (observability defining the engine's hook) in a way that will fight the
  next engine? Weigh vs. "engine owns its trait, obs provides only `SpanRegistry<K>`."
- **Stress extensibility — walk each and name exactly what must change:** a new sync
  op; a new async source (not namespace exec); a new event; a new metric; a new
  resource scope; a second daemon-internal subsystem. Anything beyond a new call site
  is a finding.
- Could free functions replace any object (`Sink`, `Reader`)? Could `TraceView`/
  `SampleDelta`/`RawFilter` collapse?

### Area 2 — Convenience of wiring span / event / trace into code

Judge how easy it is to instrument **existing and future** operations across the
sync/async axis. For each real seam — dispatch (set ctx), `exec_command` (sync span +
async register/cancel), `create_workspace_session`, `destroy_workspace_session`,
`read_command_lines`, `write_command_stdin`, layerstack events — count the lines and
the cognitive steps to add observability, and find the footguns:

- A `?` early-return drops a `SpanGuard` as `completed` — how easy is it to *forget*
  `set_status(Error)` on a fallible body? Is there a safe default or helper?
- An async launch path that **forgets `register`** silently drops the span (no error);
  a launch failure that **forgets `cancel`** leaks a bogus `cancelled` at shutdown. How
  exposed is this? Can the API make register+launch+cancel atomic / hard to misuse?
- The one-shot finalize must **capture `ctx` then `with_context`** on the watcher — is
  that pattern discoverable, or a trap a new async op will get wrong?
- When a *new variety* of operation/command/execution is added, what is the minimal
  correct instrumentation, and where will an author most likely err?

Propose ergonomic changes (a `Result`-aware scope guard, a launch helper, a small
macro) **only** where they remove a footgun without adding ceremony; reject sugar that
just renames a one-liner.

### Area 3 — The API-reference table (§3.7): generic, minimal, easier?

Treat `crate-core-impl.md` §3.7 as the contract a caller learns. Attack it for
minimalism and ease:

- Redundant entry points: `event` vs `event_in`; `context` vs `with_context`;
  `attr` vs `set_status` — can any pair merge or one be dropped?
- Is the producer surface as small as it can be (three producers + two plumbers), or
  smaller? Could `SpanRegistry::open`/`complete`/`cancel` be fewer calls?
- `RawFilter` as a struct vs builder vs plain args — which is easiest at the call site
  and over the RPC?
- Defaults: does the common case require the fewest tokens (e.g. `obs.span("x")` with
  no attrs, no status)? Where does the table force boilerplate?
- Are the signatures consistent (`&self` vs consuming, `impl Into<Attrs>` everywhere,
  `&'static str` vs `&str` choices)?

Propose the **simpler table** — fewer methods, better defaults, a builder only where it
pays — **without** losing the time-free / never-fail / one-record-per-span properties.

### Area 4 — Naming convention & consistency (prefer user-understandable)

Audit every name for clarity and a consistent scheme; flag and fix:

- **Types:** `Observer` vs `SpanObserver` (dangerous proximity — one is the emit API,
  one is the engine hook); `SpanGuard`, `SpanRegistry`, `OpenSpan`,
  `NamespaceExecutionObserver`, `NoopObserver`, `TraceContext`, `RawFilter`,
  `SampleDelta`, `TraceView`.
- **Methods:** `span`/`event`/`event_in`/`sample`/`context`/`with_context`/`attr`/
  `set_status`/`open`/`complete`/`cancel`/`on_terminal`/`register`. Consistent verb
  voice? `open`/`complete`/`cancel` vs `register`/`cancel` (two vocabularies for the
  same lifecycle?).
- **Args:** `name`, `attrs`, `scope`, `metrics`, `key`, `ctx`, `proc_token`,
  `window_ms`/`since_ms`/`max_file_bytes` — consistent units/suffixes?
- **Files / modules:** `record.rs`, `paths.rs`, `samples.rs`→? (still apt after it
  becomes the full `Sink`/`Reader`?), `collect/`, the Observer module home.
- **Span-name vocabulary** (the on-disk labels): `daemon.dispatch`, `exec_command`,
  `workspace_session.create`/`destroy`, `workspace.create`, `namespace.exec.shell`/
  `mount_overlay`, `ns_runner.shell.spawn_child`, `lease.acquired`/`released`,
  `layerstack.publish`. Is the dotted convention consistent (why is `exec_command`
  bare while siblings are `subsystem.action`)? Is there a stated rule?

Propose a single, **user-understandable** naming scheme and a **rename table** (old →
new) with the adoption cost. Resolve the `Observer`/`SpanObserver` collision explicitly.

### Area 5 — Concrete span/trace for five operations + a new examples doc

Produce a **new markdown file**, `cli-observability-examples.md`, as an extension to
`cli-observability.md`, giving concrete span/trace for **each** of:

1. `exec_command` — both one-shot (Case A) **and** persistent-session.
2. `read_command_lines` — reads buffered output of a running command.
3. `write_command_stdin` — writes stdin (may trigger terminal completion).
4. `create_workspace_session` — standalone (mount_overlay async + lease event).
5. `destroy_workspace_session` — standalone (publish/lease tail, admission check).

For **each**, the doc must show:
- the **seams** that fire (span `name`s + `attrs`, event `name`s + `attrs`), and which
  are **sync** (`SpanGuard`) vs **async** (`SpanRegistry`) vs **event**;
- the **raw NDJSON** in the new record shape (`ts`/`trace`/`span`/`parent`/`name`/
  `dur_ms`/`status`/`attrs`; `exit_code` in attrs; no `sandbox`/`component`/`pid`);
- the rendered **`sandbox-cli observability trace`** waterfall (offsets from
  `ts − dur_ms`, nesting by `parent`, `[async]` bars);
- the parent/nesting and where each record is written (which thread, when).

Ground each in the real handler (pointers in *What to read*). Then **critique**: for an
op like `read_command_lines` (a fast synchronous read), is a span even useful, or is an
**event** the right shape? Does any op produce an empty/awkward/misleading trace under
the current model? Where the design makes an op's trace poor, that is a finding with a
proposed change (and the doc should reflect the recommended shape, flagging
alternatives).

---

## Required output format (per the owner)

For **each area**, exactly two blocks, in this order:

- **Findings** — ordered by severity (Critical → Minor). Each finding: a one-line
  claim; the cited spec §/type/method or code path; the concrete cost/confusion/footgun.
  No hand-waving; no prose nitpicks that don't change the design.
- **Proposed change** — the minimal, concrete change: a deleted/collapsed/renamed
  type/method/arg, the simpler table, the helper signature, or (Area 5) the drafted md
  content. Show the diff in intent and state the adoption cost.

Lead each area with a one-line **verdict**: `ship-as-is` / `change` / `rework`.

End with a consolidated **Phase 3 synthesis**:
- **Minimum surface** — the smallest type/method/arg set that still meets the fixed
  intent (may delete part of §3.7).
- **Naming scheme + rename table** — old → new, with cost, resolving
  `Observer`/`SpanObserver`.
- **Extensibility verdict** — does the design absorb the next ten varieties of
  op/command/execution (sync + async) without a crate change? Name every spot that
  forces one and the cheapest fix.
- **Deliverable** — the finished `cli-observability-examples.md`.

## Rules of engagement

- Bias to subtraction and ergonomics; **prefer simple & extensible over clever**.
- Every "this is wrong" ships with the smaller/clearer thing that replaces it.
- Respect the Fixed intent; a breach must be a costed trade, not a preference.
- No rubber-stamping; no prose-only nitpicks. Only findings that change the design,
  its risk, or its usability.
