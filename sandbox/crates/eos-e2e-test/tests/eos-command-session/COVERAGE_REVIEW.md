# eos-command-session — Test Coverage Review

Scope: `sandbox/crates/eos-e2e-test/tests/eos-command-session`. This is a review of
`exec_command` / `write_stdin` coverage plus the four behaviors asked about
(natural return, cancelled via `write_stdin`/cancel, killed-by-other-process, long-lived
output-emitting-but-running). The §4 drafts below were turned into real tests;
the generated `readme.md` / `readme.json` / `index.html` bundle is left untouched
(regenerate it from the test files separately).

---

## 0. Status — headline gaps implemented & validated

Eleven tests were added across four modules. All compile, pass `clippy`, and
**pass live** against the Docker `linux/amd64` `sweevo-dask__dask-10042` image.

| Gap | New test | Module | Live |
|---|---|---|---|
| A | `external_signal_kill_is_structured` | lifecycle | ✅ |
| A | `self_kill_reports_signal_exit` | lifecycle | ✅ |
| A+C | `external_kill_of_foreground_keeps_group_running` | lifecycle | ✅ |
| E | `write_stdin_to_completed_session_is_structured` | lifecycle | ✅ |
| B | `live_background_emitter_keeps_session_running` | ephemeral | ✅ |
| B | `running_stderr_only_emitter_is_visible` | ephemeral | ✅ |
| C | `setsid_descendant_escapes_and_leaks_in_ephemeral` | ephemeral | ✅ |
| C | `nonsetsid_detach_vectors_stay_tracked` | ephemeral | ✅ |
| C | `setsid_descendant_reaped_on_isolated_exit` | isolated | ✅ |
| #1 teardown controls | `write_stdin_ctrl_d_reaps_marker_process`, `ctrl_c_char_cancels_command_session` | lifecycle | ✅ |
| #2 stdin (bounded) | `stdin_to_non_reading_consumer_stays_bounded_and_cancellable` | error_and_backpressure | ✅ |

Empirically confirmed by the live run: (1) external/self signal kill surfaces a
signal-coded `exit_code` and finalizes cleanly; (2) killing only the foreground
while a same-pgid peer survives keeps the session `running`; (3) the
**ephemeral path leaks** an escaped `setsid` descendant past lease release while
the **isolated path reaps** it via its cgroup — and cgroup delegation *is*
available in the live container (the flagged risk did not materialize);
(4) `\x03` and `\x04` chars through `write_stdin` both finalize the session as
cancelled with `exit_code == 130` through the same cancel path.

Two of the originally-deferred secondary items turned out to be **product gaps,
not testable behaviors** — see §6. `eos-command-session-uncollected-completion-gc`
(no GC exists) and the matrix `signal` / `background` families (harness mismatch +
redundant) were intentionally **not** shipped as passing tests; the stdin item
shipped only in its safe bounded form for the same reason (the real backpressure
case wedges the daemon).

---

## 1. Verdicts (direct answers)

| Question | Verdict | Evidence (existing tests) | Gap |
|---|---|---|---|
| Good coverage of `exec_command`? | **Mostly yes** | `exec_simple`, `exec_returns_session_id`, `exec_timeout`, `output_transcript_timestamp`, `nonzero_exit_and_stderr_are_structured`, `missing_command_*`, 12 `command_matrix_*` families | No external-kill/signal family; matrix is all clean-exit foreground |
| Good coverage of `write_stdin`? | **Mostly yes** | `write_stdin_echo`, `command_session_transcript_progress_no_replay`, `command_sessions_accept_stdin_and_release_on_cancel`, Ctrl-C/Ctrl-D cancel tests, prompt/backpressure reads | No write to a completed session; no large-stdin backpressure |
| Natural return of a session? | **Covered** | `exec_simple` (exit 0), `collect_completed_drains`, `session_completes_only_after_all_subprocesses_exit` | — |
| Cancelled through `write_stdin` controls or cancel API? | **Covered** | `write_stdin_ctrl_d_reaps_marker_process`, `ctrl_c_char_cancels_command_session`, `cancel_kills_whole_session`, `command_sessions_cancel_cleans_descendant_processes` | — |
| Killed by **other** process (external signal)? | **NOT covered** | none — only the API-driven cancel path is tested | **Headline gap A** |
| Long-lived, **emits output but stays running** (nohup / invisible bg)? | **Partial** | `lingering_child_keeps_session_running`, `nohup_child_keeps_session_running`, `setsid_nohup_contract` (all use **silent** sleepers; setsid uses a bounded `sleep 4`) | **Headline gaps B & C** |

Bottom line: the happy paths and the two API-driven kill paths are solid. The
three things missing are exactly the three the question circles: (A) death by an
**external** signal, (B) a background child that **keeps emitting** (incl.
stderr) while running, and (C) the **escaped/invisible** descendant contract,
which today is only exercised with a self-healing bounded sleep that hides the
real leak.

---

## 2. Implementation grounding (why the gaps matter)

- **Running vs completed is pgid scope-wait, not single-process.**
  `eos-runner/src/fresh_ns/child.rs:79` returns only when the root exited **and**
  `process_group_has_other_live_members(pgid)` is false (`child.rs:94`). A
  same-pgid background child therefore keeps the session `running` — this is the
  whole "invisible background process" mechanism.
- **Signal death is encoded, but never asserted E2E.**
  `eos-command-session/src/process/runner.rs:39`:
  `status.signal().map(|signal| -i64::from(signal))` → an externally-killed
  process yields a **negative** `exit_code` (e.g. `-9`, `-15`, `-11`). No test
  reads this path; API-driven teardown goes through cancel.
- **Reaping is asymmetric between modes** (the key to gap C):
  - *Ephemeral / fresh-ns (default):* `unshare(NEWUSER | NEWNS)` only — **no
    `NEWPID`** (`fresh_ns.rs:124`). Teardown =
    `DaemonEphemeralCommandPort::release_snapshot` → `LayerStack::release_lease`
    **only** (`ports/ephemeral.rs:73`); there is no `cgroup_path` in
    `EphemeralCommandPrepareContext`. The code says so out loud: *"We
    deliberately do not `killpg` the old children … lease cleanup is left to
    LayerStack GC"* (`services/command_session/mod.rs:347`). Process reaping is
    **pgid-only** (`killpg` on cancel/timeout). → a `setsid`/double-fork
    escapee gets a new pgid, dodges `killpg`, has no PID-ns and no cgroup
    backstop, and **survives session completion and lease release**.
  - *Isolated:* allocates a `cgroup_path` (`isolated-workspace/src/command_session/prepare.rs:79`)
    and GC does `kill_cgroup_pids` + `reap_named_cgroup_orphans`
    (`isolated-workspace/src/session/gc.rs:91-156`). → escapees **are** reaped at
    exit/GC.
  This contained-vs-leaky asymmetry is real and currently untested.
- **stderr is merged into the single PTY stream**; the `output.stderr` field is
  always empty (asserted today only for *foreground-completing* commands in
  `nonzero_exit_and_stderr_are_structured`). Whether a **still-running**
  stderr-only emitter surfaces its stderr is unverified.
- **Ctrl-C/Ctrl-D teardown controls are API cancel shortcuts.** There is no
  separate SIGINT tool path; both control chars route to command-session cancel.

---

## 3. Headline gaps

### A — Killed by another process (external signal)
No test drives a session to termination by a signal that did **not** come from
`cancel`. The negative-`exit_code` mapping (`runner.rs:39`), the
status reported for signal death, lease release, and one-shot completion under
external kill are all unverified. Includes self-kill (`kill -9 $$`), a second
`exec_command` doing `pkill -f <marker>`, and a crash (`SIGSEGV`).

### B — Long-lived background **emitter** (incl. stderr-only)
Every "stays running" test uses a **silent** sleeper. Missing: a foreground that
exits after backgrounding a same-pgid child that **keeps printing**, proving
(1) the session stays `running`, (2) later empty-`write_stdin` read_progress reads
surface the *new* output without replay, (3) the final completion carries the
late output. And the literal phrasing "returns a stderr but remains running":
a never-exiting stderr-only emitter should show its stderr in the merged
`output.stdout` while `status == running`.

### C — Escaped / invisible descendant contract (the critical one)
`setsid_nohup_contract` uses a bounded `sleep 4`, so it asserts only that the
session completes and the marker self-heals — it **cannot observe the leak**.
Per §2 the ephemeral path has no PID-ns and no cgroup backstop, so an *unbounded*
`setsid`/double-fork descendant is a true cross-lease ghost. The contract should
be pinned explicitly, in both modes (ephemeral = leaks, isolated = cgroup-reaped),
and across the common detach idioms (`disown`, `( cmd & )`, bare `setsid`,
`&`-then-`exit`) — not left as an accidental outcome of one bounded test.

> **Structural follow-up (not a test):** if tracking invisible background
> processes is a goal, the fix is a teardown backstop for the ephemeral path —
> a cgroup (as isolated already has) or a PID namespace — so lease release reaps
> escapees. The drafted tests below will fail/﻿flip the moment that lands, which
> is the point.

---

## 4. Drafted tests & checklist items, per module

Checklist items use the repo's `eos-command-session-<slug>: <description>` style
so they drop into the module checklist. **H** = headline, **S** = secondary.

### `test_eos_command_session_lifecycle.rs` (core exec/write_stdin + nohup/setsid)
Drafted tests:
- **[H-A] `external_signal_kill_is_structured`** — start a sleeper; from a
  *second* `exec_command` run `pkill -f <marker>` (or `kill -SEGV <pid>`). Assert
  the victim session finalizes with a non-`ok` status and a signal-derived
  `exit_code` (negative, e.g. `-9`/`-15`/`-11`, per `runner.rs:39`), then
  `wait_for_session_count(0)` and `wait_for_active_leases(0)`, and exactly one
  `collect_completed`.
- **[H-A] `self_kill_reports_signal_exit`** — `sh -c 'echo go; kill -9 $$'`;
  assert signal-coded `exit_code` and clean lease/session drain.
- **[H-C] discriminating: `external_kill_of_foreground_keeps_group_running`** —
  foreground reader + same-pgid background sleeper; externally kill **only the
  foreground**. Assert the session stays `running` (pgid scope-wait,
  `child.rs:79`) and completes **only** after the surviving peer exits. This is
  the single test at the intersection of "killed by other process" + "remains
  running" + invisible-background.
- **[S] `write_stdin_to_completed_session_is_structured`** — let a fast command
  finish, then `write_stdin`/`cancel` its id *before* collecting; assert a
  structured terminal status (already-done / completed), distinct from the
  `command_session_not_found` returned for a never-existing id.
- **[S] `ctrl_c_char_cancels_command_session` / `write_stdin_ctrl_d_reaps_marker_process`** —
  send `\x03` and `\x04` as standalone stdin payloads and assert both route to
  command-session cancel, return `exit_code == 130`, drain the session, and reap
  same-pgid marker children.

Checklist:
- [ ] `eos-command-session-external-signal-kill`: A session killed by an
  out-of-band signal (second-session `pkill`, self `kill -9 $$`, `SIGSEGV`)
  finalizes with a signal-derived `exit_code`, a non-`ok` status, released lease,
  and exactly one parked completion.
- [ ] `eos-command-session-signal-kill-keeps-group`: Externally killing only the
  foreground while a same-pgid peer survives keeps the session `running` and
  completes only after the peer exits.
- [ ] `eos-command-session-write-stdin-to-completed`: `write_stdin`/`cancel`
  against a completed-but-uncollected session returns a structured terminal
  status, not a generic not-found.
- [ ] `eos-command-session-teardown-control-cancel`: `\x03` and `\x04` through
  `write_stdin` both route to command-session cancel and share the same cleanup
  behavior.

### `test_eos_command_session_ephemeral_workspace.rs` (process-group semantics)
Drafted tests:
- **[H-B] `live_background_emitter_keeps_session_running`** — `sh -c 'echo up;
  (for i in $(seq 1 20); do echo tick-$i; sleep 0.3; done) & echo done'`.
  Foreground prints `done` and exits; assert `status == running`, then empty
  `write_stdin` polls surface *new* `tick-N` lines with no replay of earlier
  ticks, and the final completion (after the child exits) carries late ticks.
- **[H-B] `running_stderr_only_emitter_is_visible`** — a never-exiting process
  that writes only to stderr (`python3 -u -c 'import sys,time; …
  print("err-N", file=sys.stderr, flush=True); time.sleep(60)'` behind a
  backgrounded foreground). Assert the stderr text appears in merged
  `output.stdout` while `status == running` and the `output.stderr` field stays
  empty (confirms merged PTY doesn't drop stderr for non-exiting sessions).
- **[H-C] `unbounded_setsid_descendant_leaks_in_ephemeral`** — `setsid`/double-fork
  an **unbounded** marked sleeper; assert the protocol command completes
  (`status == ok`, no `command_session_id`) **and** the descendant is still alive
  after the lease releases (`wait_for_active_leases(0)` then marker count > 0),
  pinning the pgid-only-reaping leak. Bound the orphan with a self-healing cap
  (e.g. `sleep 30`) so CI never accumulates ghosts.
- **[H-C] `detach_vector_contract_matrix`** — table over `disown`, `( cmd & )`
  subshell daemonize, bare `setsid` (no nohup), `&`-then-shell-`exit`; each gets
  an explicit tracked-vs-escaped assertion under the pgid rule.

Checklist:
- [ ] `eos-command-session-live-background-emitter`: A same-pgid child that keeps
  emitting after the foreground exits keeps the session `running`, surfaces new
  output on read_progress reads without replay, and delivers late output in the final
  completion.
- [ ] `eos-command-session-running-stderr-visibility`: A still-running
  stderr-only emitter surfaces its stderr in merged `output.stdout` (stderr field
  empty), so never-exiting sessions don't silently drop stderr.
- [ ] `eos-command-session-detached-descendant-leak-contract`: An unbounded
  `setsid`/double-fork descendant in ephemeral mode escapes the pgid; the session
  completes and the lease releases while the descendant survives (pgid-only
  reaping, no cgroup/PID-ns backstop). Self-healing bound keeps CI clean.
- [ ] `eos-command-session-detach-vector-matrix`: `disown`, `( cmd & )`, bare
  `setsid`, and `&`-then-`exit` each have a pinned tracked-vs-escaped contract.

### `test_eos_command_session_isolated_workspace.rs` (isolated mode)
Drafted tests:
- **[H-C] `unbounded_setsid_descendant_reaped_on_isolated_exit`** — same unbounded
  `setsid` descendant, but inside `enter_isolated_workspace`; assert
  `exit_isolated_workspace` reaps it via the isolated cgroup
  (`gc.rs` `kill_cgroup_pids`) — marker count → 0 after exit. This is the
  contained counterpart that proves the ephemeral-vs-isolated asymmetry.

Checklist:
- [ ] `eos-command-session-detached-descendant-isolated-reap`: An escaped
  `setsid`/double-fork descendant launched in an isolated workspace is reaped by
  the isolated cgroup on exit, establishing the contained-vs-leaky contrast with
  the ephemeral path.

### `test_eos_command_session_error_and_backpressure.rs` (errors / backpressure)
Drafted tests:
- **[S] `large_stdin_payload_stays_bounded`** — push a large `chars` payload to a
  slow/non-reading consumer; assert the call stays bounded, the session remains
  cancellable, and no lease leaks (stdin-side backpressure, mirroring the
  existing stdout backpressure test).
- **[S] `uncollected_completion_is_swept`** — let a session complete, never
  collect it, and assert the reaper/TTL backstop (`command_session_reaper_sweep`,
  `mod.rs:337`) eventually drops it so completions don't accumulate unbounded.

Checklist:
- [ ] `eos-command-session-stdin-backpressure`: A large stdin payload to a slow
  consumer is bounded and cancellable without leaked sessions or leases.
- [ ] `eos-command-session-uncollected-completion-gc`: A completed-but-never-
  collected session is eventually swept by the timeout/TTL backstop.

### `test_eos_command_session_command_matrix.rs` (family matrix + parallel load)
Observation: all 12 families (`builtin`/`pipeline`/`grep`/`sed`/`awk`/`python`/
`stderr`/`json-and-bytes`/…) are **clean-exit foreground** commands. The matrix
has no signal/kill family and no background/detach family.
Drafted additions:
- **[H-A] add a `signal` family** — variants that exit via SIGTERM/SIGKILL/SIGSEGV
  and assert the signal-coded `exit_code` contract uniformly.
- **[H-C] add a `background` family** — variants for `&`, `nohup &`, `( & )`,
  `setsid &`, asserting the tracked-vs-escaped pgid contract as first-class matrix
  rows rather than one-off lifecycle tests.

Checklist:
- [ ] `eos-command-session-command-matrix-signal-family`: The matrix includes a
  signal-death family asserting the negative/`128+n` `exit_code` contract.
- [ ] `eos-command-session-command-matrix-background-family`: The matrix includes
  a backgrounding/detach family asserting tracked-vs-escaped per the pgid rule.

### `test_eos_command_session_protocol_smoke.rs` (raw protocol smoke)
No new tests required; smoke coverage is adequate. Optional: a single raw-protocol
external-kill smoke if gap-A coverage should also be visible at the wire layer.

---

## 5. Priority

1. **C** — escaped/invisible descendant contract (ephemeral leak + isolated reap +
   detach-vector matrix). Directly answers the "critically important" tracking
   concern and is currently masked by a bounded sleep.
2. **A** — external/signal kill, incl. the discriminating
   `external_kill_of_foreground_keeps_group_running`. Exercises the untested
   `runner.rs:39` signal path.
3. **B** — live background emitter + running stderr visibility.
4. **D/E/F** (secondary) — write-stdin-to-completed, teardown-control cancel, stdin
   backpressure, uncollected-completion GC.

---

## 6. Findings — deferred items that are product gaps, not tests

Investigating the last secondary items surfaced two behaviors that **do not exist
to be asserted as passing**. They are recorded here as findings (with code
evidence) instead of being forced into green tests.

### F1 — uncollected completions are unbounded (no TTL, no cap)
`registry.rs:22` holds `completed: Mutex<HashMap<String, CommandSessionCompletion>>`.
`push_completed` (`registry.rs:67`) only inserts; entries leave only via
`take_completed_result` / `collect_completed` (`registry.rs:71`, `:78`). The
`sweep_expired` / `is_expired` machinery operates on the **live** `sessions` map,
never on `completed`. So a caller that starts fire-and-forget sessions and never
calls `collect_completed` accumulates completion records — each carrying captured
stdout/stderr — for the daemon's lifetime.
- **Why no test:** a passing test would have to assert the *absence* of GC, which
  enshrines the gap. The honest artifact is this finding.
- **Recommendation:** bound the map — a TTL sweep inside
  `command_session_reaper_sweep` (`services/command_session/mod.rs:337`) or a
  max-entries eviction — then add `eos-command-session-uncollected-completion-gc`
  as a real test once the behavior exists.

### F2 — stdin write is an unbounded blocking `write_all`
`runner.rs:158` is `lock(&self.writer).write_all(bytes)`, and the PTY master is
opened without `O_NONBLOCK` (`pty.rs:7`). A payload exceeding the kernel PTY input
buffer, sent to a consumer that never reads stdin, blocks the writer thread while
holding the writer mutex — a per-session wedge.
- **Shipped instead:** `stdin_to_non_reading_consumer_stays_bounded_and_cancellable`
  exercises the *safe* path (1 KiB, one line, under the buffer bound) and asserts
  the write returns promptly and the session stays cancellable. The full
  >buffer backpressure case is deliberately not a test because it would hang CI.
- **Recommendation:** bound / time-slice / non-block the stdin write, then a true
  `eos-command-session-stdin-backpressure` test becomes safe to add.

### D1 — matrix `signal` / `background` families intentionally not added
`run_command_family` asserts `assert_command_ok` (status == `ok`,
`command_matrix.rs:768`), which is structurally incompatible with signal-death
(non-`ok`) and backgrounding (`running` / escaped) commands. Forcing them in would
need a separate assertion path and would duplicate the eight first-class
signal/background/detach tests already shipped (`external_signal_kill_*`,
`self_kill_*`, `external_kill_of_foreground_*`, `live_background_emitter_*`,
`setsid_descendant_*`, `nonsetsid_detach_vectors_*`). The lifecycle / ephemeral /
isolated modules are the right home for these behaviors; the matrix stays a
clean-exit-foreground breadth harness.
