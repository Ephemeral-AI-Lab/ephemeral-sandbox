# CLI Observability — Concrete Span/Trace Examples (per-operation)

Status: ready-to-implement (extends `cli-observability.md` + `README.md` §4).

`cli-observability.md` fixes the *rendered* trace/cgroup/events shapes; `README.md`
§4.1 gives one worked case (one-shot exec). This doc grounds the span/event/trace
model in **each** of the six instrumented operations — reading the **real handler**
under `crates/sandbox-runtime/operation/src` — and renders every one in the exact
waterfall style of `cli-observability.md` §4.2.

Where the grounded shape diverges from `README.md` §4.1 / `cli-observability.md`
§4.2 the row is flagged `⚠ (A5-n)` and the divergence is explained in the per-op
**Critique**; the findings live in the review's findings array. Where a finding shows
an op's trace is *poor*, this doc renders the **recommended** shape and flags the
as-specified alternative inline.

---

## 0. Conventions (record shape + render legend)

**Record envelope (new shape).** One JSON object per line. Envelope = `ts` +
`trace` (+ `kind`; for spans `span`/`parent`). **No** `sandbox`/`component`/`pid`.
`exit_code` rides in `attrs`. `ts` is completion time; `start = ts − dur_ms`.

```json
{"ts":<unix_ms>,"kind":"span","trace":"<req>","span":"<proc>-<seq>","parent":"<proc>-<seq>|null","name":"<dotted>","dur_ms":<f64>,"status":"completed|error|cancelled|timed_out","attrs":{…}}
{"ts":<unix_ms>,"kind":"event","trace":"<req>","parent":"<proc>-<seq>","name":"<dotted>","attrs":{…}}
```

**Seam legend.**

| Mark | Mechanism | Recorded where / when |
|---|---|---|
| `span (sync)` | `obs.span(name)` → `SpanGuard` (`crate-core-impl.md` §3.4) | on drop, on the dispatch (`spawn_blocking`) thread |
| `span (async)` | caller `register()` → `SpanRegistry::open` (returns the child `TraceContext`) … engine `on_terminal` → `record` | on the engine watcher thread, at child-exit, **before** finalize |
| `span (cross-proc)` | child `obs.with_context(ctx, ‖ obs.span(name))` (Phase B, `removal-and-phaseb-impl.md` §B.3) | on the forked namespace-process (`np` proc token) |
| `event` | `obs.event(name, attrs)` | immediately, on the thread that hit the seam (thread-local parent) |

**Render legend (matches `cli-observability.md` §4.2).** `+SS.mmm` = `(ts − dur_ms) −
trace_start`; tree by `parent`; siblings ordered by start; `[async]` = recorded on
another thread; `✓`/`✗` = `status`; `exit0` = `attrs.exit_code`. Events render as
`• name args` with no bar, no duration, no status.

> **Trace = `Request.request_id`** (`span-trace-impl.md` §2). Every operation below is
> a *separate* daemon request, so read/write/create/destroy each get their **own** trace
> id; they do **not** share the trace of the `exec_command` that created the session they
> touch. The lone exception is a write that *terminates* a one-shot command (§3B): the
> teardown effect lands on the **originating exec** trace, because the watcher context
> was captured at exec launch (`span-trace-impl.md` §7).

---

## 1. `exec_command` — Case A (one-shot) and persistent-session

### 1A. One-shot (`workspace_session_id` omitted) — grounded Case A

Handler chain: `cli_definition/command_operations.rs:dispatch_exec_command` →
`exec_command.rs:18` → `resolve_exec_workspace` (`:93`, no id → `create_one_shot_workspace_session`,
`core.rs:126`) → `workspace_session/.../create_workspace_session.rs:9` → workspace-crate
`create_workspace.rs:7` (`acquire_snapshot_with_lease` + `manager.open` →
`initialize_handle` → `mount_overlay`, **`.wait()`ed**) → engine `run_shell_interactive`
(`exec_command.rs:59`, async shell) → `wait_for_command_yield` (`:90`, default 1000 ms) →
yield-return. On child-exit the `finalize_closure` (`exec_command.rs:168`) runs
`destroy_session` (`:175`) on the **watcher** thread.

**Seams that fire**

| Record | Kind | Site | Parent | Thread / when |
|---|---|---|---|---|
| `daemon.dispatch` `d-0` | span (sync) | `dispatch.rs` closure (`span-trace-impl.md` §2) | — | dispatch thread, on return (~1.05s) |
| `command.exec` `d-1` | span (sync) | `exec_command.rs:18` | `d-0` | dispatch thread, at yield (~1.05s) |
| `workspace_session.create` `d-2` | span (sync) | `create_workspace_session.rs:9` | `d-1` | dispatch thread |
| `lease.acquired` | event | `stack/mod.rs:acquire_snapshot` (`:78-81`) | `d-2` | dispatch thread |
| `namespace.exec.mount_overlay` `d-4` ⚠ (A5-6) | span (sync) | `setns_runner.rs:37` (status from `.wait()` `Result`) | `d-2` | dispatch thread (sync mount guard) |
| `namespace.exec.shell` `d-5` | span (async) | command engine `on_terminal` | `d-1` | command-engine watcher, at child-exit (~4.27s) |
| `namespace.runner.spawn_child` `np-0` | span (cross-proc) | `shell_exec.rs:40-63` (Phase B) | `d-5` | forked namespace-process |
| `workspace_session.destroy` `d-6` | span (sync) | `destroy_session.rs:7` | `d-1` | **watcher** thread (finalize closure, under `with_context`) |
| `lease.released` | event | `cleanup.rs:release_lease_locked` (`:16`) | `d-6` | watcher thread |

Removed vs `README.md` §4.1: **`workspace.create`** (C1/A5-9 — near-coextensive with
`workspace_session.create`, dropped), **`exec.terminal`** (A5-8, redundant with `d-5`'s
own `status`+`exit_code`), and **`layerstack.publish`** (A5-1 — the one-shot teardown
*evicts* the upperdir and *releases* the lease; it never publishes, and `publish_changes`
has no production caller). `lease.released` therefore carries the **same** revision
(`r5`) as `lease.acquired` — no publish bumped it.

**Raw `observability.ndjson` (append order ≈ `ts` order)**

```json
{"ts":1719500000009,"kind":"event","trace":"req-7f3","parent":"d-2","name":"lease.acquired","attrs":{"revision":"r5"}}
{"ts":1719500000040,"kind":"span","trace":"req-7f3","span":"d-4","parent":"d-2","name":"namespace.exec.mount_overlay","dur_ms":27.0,"status":"completed"}
{"ts":1719500000042,"kind":"span","trace":"req-7f3","span":"d-2","parent":"d-1","name":"workspace_session.create","dur_ms":39.0,"status":"completed"}
{"ts":1719500000061,"kind":"span","trace":"req-7f3","span":"np-0","parent":"d-5","name":"namespace.runner.spawn_child","dur_ms":6.0,"status":"completed","attrs":{"exec_id":"ns-9"}}
{"ts":1719500001050,"kind":"span","trace":"req-7f3","span":"d-1","parent":"d-0","name":"command.exec","dur_ms":1048.0,"status":"completed","attrs":{"one_shot":true}}
{"ts":1719500001051,"kind":"span","trace":"req-7f3","span":"d-0","name":"daemon.dispatch","dur_ms":1051.0,"status":"completed","attrs":{"op":"exec_command"}}
{"ts":1719500004273,"kind":"span","trace":"req-7f3","span":"d-5","parent":"d-1","name":"namespace.exec.shell","dur_ms":4231.0,"status":"completed","attrs":{"exec_id":"ns-9","async":true,"exit_code":0}}
{"ts":1719500004295,"kind":"event","trace":"req-7f3","parent":"d-6","name":"lease.released","attrs":{"revision":"r5"}}
{"ts":1719500004300,"kind":"span","trace":"req-7f3","span":"d-6","parent":"d-1","name":"workspace_session.destroy","dur_ms":25.0,"status":"completed","attrs":{"one_shot":true}}
```

Consistency: every `parent` resolves to a `span` id under the one trace `req-7f3`;
`d-1`/`d-0` complete at ~1.05s while `d-5` is still running (no record yet). `d-5`
carries the child-exit instant (4.273s, stamped **before** teardown); the teardown tail
(`d-6` + `lease.released`) is written just after, on the watcher thread, under `d-1`.
`np-0` (`ts` 4ms earlier than the shell launch records on disk only because it appears
in append order before `d-1`/`d-0`) starts at `61 − 6 = 55 ms`. The `d-3` id slot is
**vacant** — `workspace.create` was dropped (C1); the surviving spans keep their ids, so
the shell stays `d-5` and Phase B's `np-0.parent = d-5` resolves.

**Rendered — `sandbox-cli observability trace --sandbox-id eos-abc --id req-7f3`**

```
trace req-7f3   sandbox eos-abc   wall 4.30s   (call returned at 1.05s)

  +00.000  daemon.dispatch op=exec_command                 1051ms  ✓
  +00.002   └ command.exec one_shot                        1048ms  ✓
  +00.003      ├ workspace_session.create                    39ms  ✓
  +00.009      │   • lease.acquired r5
  +00.013      │   └ namespace.exec.mount_overlay            27ms  ✓
  +00.042      ├ namespace.exec.shell           [async]    4231ms  ✓ exit0   ← outlives call
  +00.055      │   └ namespace.runner.spawn_child            6ms  ✓   [Phase B: cross-process]
  +04.275      └ workspace_session.destroy one_shot         25ms  ✓
  +04.295         • lease.released r5
```

vs `README.md` §4.1 / `cli-observability.md` §4.2: `mount_overlay` is a **sync span**
nested directly under `workspace_session.create` (C1/A5-6), not the async sibling
README §4.1 renders — it is `.wait()`ed synchronously on the dispatch thread, so it
carries **no** `[async]` mark and does **not** outlive the call (only the
`← outlives call` annotation on the shell line carries that meaning). `workspace.create`,
`exec.terminal`, and `layerstack.publish` are all gone.

**Critique.**
- **A5-1 (Critical):** no `layerstack.publish` row. The one-shot teardown
  (`destroy_session.rs:19` → workspace-crate `destroy_workspace.rs`: `manager.close`
  evicts the upperdir, then `release_lease`) **never publishes**; `publish_changes` has
  no production caller (§6). Rendering this tail evict-only is the corrected Case A.
- **A5-2 / C1 (Critical, resolved):** the mount originally ran on the workspace crate's
  *private* `NamespaceRuntime` engine (`namespace/mod.rs:111`, `Arc::new(NoopHook)`),
  which `span-trace-impl.md` §3/§4 never wire (§4 swaps only the *command* engine's
  `terminal_hook` at `command/service/core.rs:33-34`) — so an *async* mount span there
  could never emit. C1 resolves this by modeling `mount_overlay` as a **sync `SpanGuard`**
  at `setns_runner.rs:37` (status from the `.wait()` `Result`): the workspace crate needs
  only the obs handle for that one guard — no second async hook, no engine swap, no
  `exec_id` collision.
- **A5-9 / C1 (adopted):** `workspace.create` (formerly `d-3`) is **dropped** —
  near-coextensive with `workspace_session.create` (`dispatch ≈ session.create ≈
  workspace.create`, three bars for one create). `lease.acquired` and the mount span now
  nest directly under `workspace_session.create` (`d-2`), removing one bar *and* the need
  to obs-wire the workspace crate for an async source.

### 1B. Persistent session (`workspace_session_id` supplied)

No create, no mount, no one-shot teardown — `resolve_exec_workspace` resolves the
existing session (`exec_command.rs:97`, `resolve_workspace_session`), and the
`finalize_closure` is a no-op (`self.one_shot.then(...)` is `None`,
`exec_command.rs:172`). The session **outlives** the command, so the trace has **no**
destroy/lease tail.

**Seams:** `d-0 daemon.dispatch`; `d-1 command.exec` (attrs `workspace_session`,
`one_shot=false`); `d-2 namespace.exec.shell` (async, parent `d-1`, `exec_id ns-42`);
`np-0 namespace.runner.spawn_child` (cross-proc, parent `d-2`, Phase B).

**Raw (after the shell completes; Phase B carries `np-0` into the trace)**

```json
{"ts":1719500100039,"kind":"span","trace":"req-9a1","span":"np-0","parent":"d-2","name":"namespace.runner.spawn_child","dur_ms":6.0,"status":"completed","attrs":{"exec_id":"ns-42"}}
{"ts":1719500101021,"kind":"span","trace":"req-9a1","span":"d-1","parent":"d-0","name":"command.exec","dur_ms":1020.0,"status":"completed","attrs":{"workspace_session":"ws-7","one_shot":false}}
{"ts":1719500101021,"kind":"span","trace":"req-9a1","span":"d-0","name":"daemon.dispatch","dur_ms":1021.0,"status":"completed","attrs":{"op":"exec_command"}}
{"ts":1719500107320,"kind":"span","trace":"req-9a1","span":"d-2","parent":"d-1","name":"namespace.exec.shell","dur_ms":7300.0,"status":"completed","attrs":{"exec_id":"ns-42","async":true,"exit_code":0}}
```

**Rendered (completed)**

```
trace req-9a1   sandbox eos-abc   wall 7.32s   (call returned at 1.02s)

  +00.000  daemon.dispatch op=exec_command                 1021ms  ✓
  +00.001   └ command.exec ws-7                            1020ms  ✓
  +00.020      └ namespace.exec.shell  ns-42  [async]      7300ms  ✓ exit0   ← outlives call
  +00.033          └ namespace.runner.spawn_child            6ms  ✓   [Phase B: cross-process]
```

In **Phase A** (`np-0` not yet threaded across the fork) the `namespace.runner.spawn_child`
row is absent here — it lands under its own trace until `removal-and-phaseb-impl.md`
Part B carries `(trace, parent)` over the fork.

**Rendered while still running (`--id last`)** — the shell has **no record yet**; the
open span merges from the live registry (`cli-observability.md` §4.2):

```
trace req-9a1   sandbox eos-abc   wall — (in flight)   1 span open

  +00.000  command.exec ws-7                                1020ms  ✓
  +00.020   └ namespace.exec.shell  ns-42  [async]          running  (live, from registry)
```

**Critique (A5-4).** `np-0`'s `parent = d-5`/`d-2` is constructible only because the
async shell span's id exists **at launch**: the corrected `SpanRegistry::open`
(`crate-core-impl.md` §3.4) allocates the span id immediately, stores it in `OpenSpan`,
and **returns a child** `TraceContext { trace, parent: <new span id> }` that the caller
threads into the fork (Phase B `build_request`). Without that, `register` would hold only
`obs.context()` — parent `d-1` (the `command.exec` span), not the shell span — and could
not set `np-0.parent = <shell id>`. No new public handle (respects the no-`AsyncSpan`
intent); a return value + one field.

---

## 2. `read_command_lines` — a fast synchronous buffered read

Handler: `read_command_lines.rs:12` → `engine().with_value(&id, read_command_window)`
(`:20`) — a pure in-memory transcript-window read (no async, no child, no I/O),
sub-millisecond. It is **not** in `span-trace-impl.md` §5's sync-seam table, so it gets
**no span of its own**.

**Seams that fire — recommended (A5-5): none.**

`read_command_lines` runs in its **own** trace (`req-rd1`), disconnected from the
command's trace (`req-9a1`). A span would record only the duration of a buffer copy
(never interesting), so if a fast synchronous read is instrumented at all it warrants an
**event** (a past-tense fact) over a span; but here even that event would be an **orphan**
(nothing to hang it under). The recommended shape is therefore
**nothing** — and the unavoidable `daemon.dispatch` root, minted ~1500×
by a 200 ms polling UI over a 5-minute command, would crowd the rich exec traces out of
the file under rotation. **Recommendation:** keep a `const` allowlist of traced ops and
**skip the `daemon.dispatch` guard for read-only ops**; at minimum, never add a read
span/event.

**Recommended — no records (allowlisted out)**

```
trace req-rd1   sandbox eos-abc   (no records — read ops are not traced)
```

**⚠ Alternative (as-specified, no allowlist — a lone root span)**

```json
{"ts":1719500050000,"kind":"span","trace":"req-rd1","span":"d-0","name":"daemon.dispatch","dur_ms":0.4,"status":"completed","attrs":{"op":"read_command_lines"}}
```

```
trace req-rd1   sandbox eos-abc   wall 0.4ms

  +00.000  daemon.dispatch op=read_command_lines              0ms  ✓
```

This single-node trace conveys nothing; the recommended shape suppresses it.

---

## 3. `write_command_stdin` — stdin write (may trigger terminal completion)

Handler: `write_command_stdin.rs:6`. Resolves the live target (`exec.output_len`,
`:15`), writes stdin (`:49`) — or `exec.cancel()` (`:43`) when `is_kill_input` (`:74`,
Ctrl-C `\u{3}` / Ctrl-D `\u{4}`) — then `wait_for_command_yield` (`:64`, up to
`yield_time_ms`, default 1000; forced to 1000 on kill, `:63`). Like read, it is **not**
in the §5 table, so only `daemon.dispatch` fires in the write's **own** trace. It is a
*mutation* (not read-only), so it is **not** a candidate for the A5-5 allowlist.

**3A. Plain write (command keeps running)**

```json
{"ts":1719500060312,"kind":"span","trace":"req-wr1","span":"d-0","name":"daemon.dispatch","dur_ms":312.0,"status":"completed","attrs":{"op":"write_command_stdin"}}
```

```
trace req-wr1   sandbox eos-abc   wall 0.31s

  +00.000  daemon.dispatch op=write_command_stdin            312ms  ✓
```

**3B. Kill input (Ctrl-D) terminates a one-shot command.** The write returns a trivial
trace; the **effect** — the shell async span completing and the one-shot teardown — lands
on the **originating** exec trace (`req-7f3`), because the watcher thread's context and
the finalize closure were captured at *exec* launch (`span-trace-impl.md` §7), not at
this write.

```json
{"ts":1719500061002,"kind":"span","trace":"req-wr2","span":"d-0","name":"daemon.dispatch","dur_ms":1002.0,"status":"completed","attrs":{"op":"write_command_stdin"}}
```

```
trace req-wr2   sandbox eos-abc   wall 1.00s

  +00.000  daemon.dispatch op=write_command_stdin           1002ms  ✓
```

…while, under `req-7f3`, `namespace.exec.shell` (`d-5`) closes as `cancelled` and the
`workspace_session.destroy` (`d-6`) + `lease.released` tail append — possibly long after
`exec_command` "returned."

**Critique (A5-10).** Two awkwardnesses, both inherent to `trace = request_id`:
(1) the dispatch **duration** is dominated by the yield-wait poll (`yield.rs`), not the
write — 1002 ms reads as "write cost" but is the wait window; (2) the write that *caused*
termination shows an empty node, while the whole teardown is attributed to the exec
trace. The write→termination causality is inexpressible (tree, not DAG —
`crate-core-impl.md` §3.6). **Recommendation:** document the duration semantics, and that
termination effects attribute to the originating exec trace by design. If write intent
must be greppable in the `events` view, emit at most a `command.signal` **event** under
dispatch carrying `{command_session_id, kill}` (links by attr, not parent). Do **not** add
span-links now.

---

## 4. `create_workspace_session` (standalone) — mount_overlay (`.wait()`ed) + lease event

Handler: `cli_definition/workspace_session_operations.rs:dispatch_create_workspace_session`
(`:101`) → `create_workspace_session.rs:9` → workspace-crate `create_workspace.rs:7`
(`acquire_snapshot_with_lease` lease, then `manager.open` → `initialize_handle` →
`mount_overlay`, **`.wait()`ed**) → `prepare_workspace_cgroup` (`:15`) + sessions insert
(`:19`).

**Seams that fire**

| Record | Kind | Site | Parent | Thread / when |
|---|---|---|---|---|
| `daemon.dispatch` `d-0` | span (sync) | `dispatch.rs` closure | — | dispatch thread |
| `workspace_session.create` `d-1` | span (sync) | `create_workspace_session.rs:9` | `d-0` | dispatch thread |
| `lease.acquired` | event | `stack/mod.rs:acquire_snapshot` (`:78-81`) | `d-1` | dispatch thread |
| `namespace.exec.mount_overlay` `d-3` ⚠ (A5-6) | span (sync) | `setns_runner.rs:37` (status from `.wait()` `Result`) | `d-1` | dispatch thread (sync mount guard) |

**Raw**

```json
{"ts":1719500070009,"kind":"event","trace":"req-c1","parent":"d-1","name":"lease.acquired","attrs":{"revision":"r5"}}
{"ts":1719500070040,"kind":"span","trace":"req-c1","span":"d-3","parent":"d-1","name":"namespace.exec.mount_overlay","dur_ms":27.0,"status":"completed"}
{"ts":1719500070042,"kind":"span","trace":"req-c1","span":"d-1","parent":"d-0","name":"workspace_session.create","dur_ms":41.0,"status":"completed"}
{"ts":1719500070043,"kind":"span","trace":"req-c1","span":"d-0","name":"daemon.dispatch","dur_ms":43.0,"status":"completed","attrs":{"op":"create_workspace_session"}}
```

Consistency: `trace_start = 1719500070000` (dispatch start). The mount (`d-3`, start
`+13`, ends `ts 70040`) sits **inside** `workspace_session.create` (`d-1`, start `+01`,
ends `ts 70042`), which sits inside `daemon.dispatch` — every parent's `dur_ms` brackets
its children's spans. (`workspace.create` is dropped, so the `d-2` id slot is vacant.)

**Rendered**

```
trace req-c1   sandbox eos-abc   wall 43ms

  +00.000  daemon.dispatch op=create_workspace_session        43ms  ✓
  +00.001   └ workspace_session.create                        41ms  ✓
  +00.009      • lease.acquired r5
  +00.013      └ namespace.exec.mount_overlay                 27ms  ✓
```

**Critique.**
- **A5-2 / C1 (Critical, resolved):** `mount_overlay` originally ran on the workspace
  crate's *private* `NamespaceRuntime` engine (`namespace/mod.rs:111`,
  `Arc::new(NoopHook)`), which §3/§4 never wire (§4 swaps only the *command* engine's
  `terminal_hook`, `command/service/core.rs:34`) — so an async mount span there could
  never emit. C1 models it as a **sync `SpanGuard`** at `setns_runner.rs:37` instead, so
  the workspace crate needs only the obs handle for that one guard.
- **A5-9 / C1 (adopted):** `workspace.create` is **dropped** (near-coextensive with
  `workspace_session.create` — `dispatch ≈ session.create ≈ workspace.create`);
  `lease.acquired` and the mount span nest directly under `workspace_session.create`,
  leaving the `d-2` id slot vacant.
- **A5-6:** the mount is `.wait()`ed synchronously on the dispatch thread, so it carries
  **no** `[async]` mark, lands before the sync parents close, and does **not** outlive
  the call.

---

## 5. `destroy_workspace_session` (standalone) — admission gate, lease tail (no publish)

Handler: `dispatch_destroy_workspace_session` (`workspace_session_operations.rs:116`) →
`destroy_workspace_session_with_admission` (`command/service/core.rs:90`: lock lifecycle
→ **active-command admission check** `:96-105` → `resolve_session` → `destroy_session`).
The `workspace_session.destroy` span sits at `destroy_session.rs:7` — **inside** the
destroy, **after** admission passes. `destroy_workspace` (workspace-crate
`destroy_workspace.rs`) **evicts** the upperdir (`manager.close`) and **releases** the
lease (`release_lease`); it does **not** publish (A5-1).

**5A. Success**

| Record | Kind | Site | Parent | Thread |
|---|---|---|---|---|
| `daemon.dispatch` `d-0` | span (sync) | `dispatch.rs` closure | — | dispatch thread |
| `workspace_session.destroy` `d-1` | span (sync) | `destroy_session.rs:7` | `d-0` | dispatch thread |
| `lease.released` | event | `cleanup.rs:release_lease_locked` (`:16`) | `d-1` | dispatch thread |

```json
{"ts":1719500080090,"kind":"event","trace":"req-d1","parent":"d-1","name":"lease.released","attrs":{"revision":"r6"}}
{"ts":1719500080095,"kind":"span","trace":"req-d1","span":"d-1","parent":"d-0","name":"workspace_session.destroy","dur_ms":24.0,"status":"completed"}
{"ts":1719500080096,"kind":"span","trace":"req-d1","span":"d-0","name":"daemon.dispatch","dur_ms":26.0,"status":"completed","attrs":{"op":"destroy_workspace_session"}}
```

```
trace req-d1   sandbox eos-abc   wall 26ms

  +00.000  daemon.dispatch op=destroy_workspace_session       26ms  ✓
  +00.001   └ workspace_session.destroy                       24ms  ✓
  +00.020      • lease.released r6
```

**5B. Admission-reject (active commands exist).** `destroy_session` is **never reached**
(`core.rs:101-105` returns `ActiveCommands` before `resolve_session`), so there is **no**
`workspace_session.destroy` span — only the root. The dispatch closure returns
`active_command_rejection` → `Response::fault_with_details` (`workspace_session_operations.rs:179`).
With today's dispatch closure the root records **green** (A5-3); with the recommended
fault→`status` fix it is **red**:

```json
{"ts":1719500081001,"kind":"span","trace":"req-d2","span":"d-0","name":"daemon.dispatch","dur_ms":1.0,"status":"error","attrs":{"op":"destroy_workspace_session"}}
```

```
trace req-d2   sandbox eos-abc   wall 1ms

  +00.000  daemon.dispatch op=destroy_workspace_session        1ms  ✗   (rejected: active commands)
```

**Critique.**
- **A5-1:** no `layerstack.publish` — destroy evicts; Case A's publish tail does not
  occur here either.
- **A5-3 (recommended):** `span-trace-impl.md` §2's dispatch closure returns the
  `Response` but never inspects it, so the guard drops as `Completed` — a rejected
  destroy, a command-not-found read, an invalid-argument exec all render green. The
  admission reject (and the parse-error returns at `:138`/`:147`) short-circuit *before*
  any inner span opens, so the root is the only span and the whole trace would be green.
  **Fix:** in the dispatch closure, `dispatch.status(SpanStatus::Error)` when the
  returned `Response` is a fault (one branch before the guard drops; no new type). The
  rejection reason (active command ids) still survives only in the in-band response,
  not the trace.
- **Span placement:** `destroy_session.rs:7` puts the span *after* admission, so the
  admission check + lock contention (the actual work on a reject) are span-less. Moving
  it to `destroy_workspace_session_with_admission` (`core.rs:90`) would cover the gate —
  **but** the one-shot path calls `destroy_session` **directly** (`exec_command.rs:175`),
  bypassing the wrapper, so the span must stay at `destroy_session` for Case A's `d-6`.
  Cheapest fix is the universal fault→`status(Error)` at dispatch (A5-3); add a
  `workspace_session.destroy_rejected` event only if rejects must be greppable.

---

## 6. `publish_changes` — publish to the layerstack

Handler: `layerstack/service/impls/publish_changes.rs:7` (`LayerStackService::publish_changes`)
→ `LayerStack::publish_validated_changes` → `publish_layer_unlocked` (write layer dir,
`fsync_tree_files`, `fsync_dir`, `rename`, `write_manifest`, `write_layer_bytes` — real
durational I/O). `span-trace-impl.md` §6 (M9) now wires a sync `layerstack.publish`
**span** over `publish_layer_unlocked` (`publish.rs`), `status=error` + `attrs.reason`
on the `ManifestConflict` path (`publish.rs:89-97`, mapped at `publish_changes.rs:50`)
— folding what was a `layerstack.publish` / `layerstack.publish_rejected` event pair.

> **⚠ A5-1: orphan seam.** `publish_changes` has **no production caller** — only
> `tests/layerstack_publish.rs` reaches it, and `service_graph.rs` *asserts* it stays out
> of the CLI catalog; no daemon/runtime op routes to it. So this seam fires under **no**
> real flow today; the example below is what it *would* emit once a flow calls it. It
> nests under that flow's span via the thread-local parent (shown under a hypothetical
> caller span `d-1`).

**Recommended — one span (A5-7).** Publishing is durational I/O (multiple fsyncs +
rename + manifest write); an event discards that cost — the one place a developer wants a
bar — and `SpanStatus` already encodes success/rejection, so the second event name is
redundant. Model it as a sync span with `status=error` + `attrs.reason` on conflict:

```json
{"ts":1719500090014,"kind":"span","trace":"req-p1","span":"d-2","parent":"d-1","name":"layerstack.publish","dur_ms":12.0,"status":"completed","attrs":{"base":"r5","revision":"r6","layers_added":1,"bytes":40960,"no_op":false}}
```
On conflict:
```json
{"ts":1719500090014,"kind":"span","trace":"req-p1","span":"d-2","parent":"d-1","name":"layerstack.publish","dur_ms":3.0,"status":"error","attrs":{"base":"r5","reason":"manifest_conflict"}}
```

**Rendered (span-shape, nested under its caller)**

```
trace req-p1   sandbox eos-abc   wall 16ms

  +00.000  daemon.dispatch op=<caller>                        16ms  ✓
  +00.001   └ <caller span d-1>                               14ms  ✓
  +00.002      └ layerstack.publish r5→r6 +1 layer 40KB       12ms  ✓
```

**⚠ Superseded (pre-M9 — two events).** The earlier spec emitted a
point-in-time event pair; this throws away the I/O duration and splits the outcome axis
across two names:

```json
{"ts":1719500090014,"kind":"event","trace":"req-p1","parent":"d-1","name":"layerstack.publish","attrs":{"base":"r5","revision":"r6","layers_added":1,"bytes":40960,"no_op":false}}
{"ts":1719500090014,"kind":"event","trace":"req-p1","parent":"d-1","name":"layerstack.publish_rejected","attrs":{"reason":"manifest_conflict"}}
```

**Critique.**
- **A5-1:** no production flow emits this — wire it (or a teardown-publish) before relying
  on the `events` view's publish rows. Note the cost the span recommendation carries:
  promoting `publish` out of `kind:"event"` removes it from `events --name
  layerstack.publish` (`cli-observability.md` §3.4's flagship example), which folds
  `raw{kind:"event", name}`; the capacity attrs (`base`/`revision`/`layers_added`/`bytes`)
  survive on the span and stay queryable via `raw --kind span`, and the help example
  shifts to `lease.acquired`.
- **A5-7:** prefer a span (captures the duration of real I/O); fold
  `layerstack.publish_rejected` into `status=error` + `attrs.reason`. `no_op=true`
  (digest unchanged) renders as a 0-layer ✓.

---

## 7. Cross-op summary

| Op | Own span(s) | Async | Events (grounded) | Trace richness | Key finding |
|---|---|---|---|---|---|
| exec_command (one-shot) | dispatch, exec, ws_session.create, [workspace.create], destroy | mount, shell | lease.acquired, lease.released | rich; async tail | A5-1, A5-2, A5-6 |
| exec_command (persistent) | dispatch, exec | shell | — | rich; no teardown | A5-4 (np-0 id) |
| read_command_lines | **none (recommended)** / dispatch-only (today) | — | — | single node, no value | **A5-5 (emit nothing)** |
| write_command_stdin | dispatch only | — | (recommend `command.signal`) | single node; effect on other trace | A5-10 |
| create_workspace_session | dispatch, ws_session.create, [workspace.create] | mount | lease.acquired | rich; partly un-wired | A5-2, A5-9 |
| destroy_workspace_session | dispatch, ws_session.destroy | — | lease.released (no publish) | thin on reject | A5-1, A5-3 |
| publish_changes | **span (recommended)** | — | layerstack.publish[_rejected] (as-spec) | orphan (no caller) | **A5-1, A5-7** |

**Net subtractions this doc adopts:** drop `exec.terminal` (A5-8); drop
`layerstack.publish` from the one-shot teardown — render it evict-only (A5-1); emit
nothing for `read_command_lines` (A5-5). **Recommended further:** drop `workspace.create`
(A5-9, also fixes A5-2's un-wired crate); model `publish_changes` as a span and fold
`layerstack.publish_rejected` into `status=error` (A5-7). **Net corrections:** re-nest
`mount_overlay` under `workspace.create`/`workspace_session.create` and reserve `←
outlives call` for the shell (A5-6); map fault→`status(Error)` at dispatch so
rejects render red (A5-3); make `SpanRegistry::open` return the span id so Phase B's
`np-0.parent` is constructible (A5-4).