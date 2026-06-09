# workspace-runtime-command-session — Test Coverage Review

Scope: `sandbox/crates/eos-e2e-test/tests/workspace-runtime-command-session`. This is a review of
`exec_command` / `write_stdin` coverage plus the four behaviors asked about
(natural return, cancelled via `write_stdin`/cancel, killed-by-other-process, long-lived
output-emitting-but-running). The §4 drafts below were turned into real tests;
the generated `readme.md` / `readme.json` / `index.html` bundle is maintained
from the module readme data.

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
not testable behaviors** — see §6. `workspace-runtime-command-session-uncollected-completion-gc`
(no GC exists) and the matrix `signal` / `background` families (harness mismatch +
redundant) were intentionally **not** shipped as passing tests; the stdin item
shipped only in its safe bounded form for the same reason (the real backpressure
case wedges the daemon).

---

## 1. Verdicts (direct answers)

| Question | Verdict | Evidence (existing tests) | Gap |
|---|---|---|---|
| Good coverage of `exec_command`? | **Mostly yes** | `exec_simple`, `exec_returns_session_id`, `exec_timeout`, `exec_command_outputs_timestamped_transcript_lines`, `nonzero_exit_and_stderr_are_structured`, `missing_command_*`, external signal tests, 12 `command_matrix_*` families | Matrix remains clean-exit foreground by design |
| Good coverage of `write_stdin`? | **Mostly yes** | `write_stdin_echo`, `read_command_progress_returns_stateless_tail_snapshot`, `write_stdin_to_completed_session_is_structured`, `command_sessions_accept_stdin_and_release_on_cancel`, Ctrl-C/Ctrl-D cancel tests, prompt/backpressure reads | Full over-buffer stdin backpressure remains a product gap |
| Natural return of a session? | **Covered** | `exec_simple` (exit 0), `collect_completed_drains`, `session_completes_only_after_all_subprocesses_exit` | — |
| Cancelled through `write_stdin` controls or cancel API? | **Covered** | `write_stdin_ctrl_d_reaps_marker_process`, `ctrl_c_char_cancels_command_session`, `cancel_kills_whole_session`, `command_sessions_cancel_cleans_descendant_processes` | — |
| Killed by **other** process (external signal)? | **Covered** | `external_signal_kill_is_structured`, `self_kill_reports_signal_exit`, `external_sigterm_child_finalizes_via_collect_completed`, `external_sigkill_process_group_is_observed_by_write_stdin` | — |
| Long-lived, **emits output but stays running** (nohup / invisible bg)? | **Covered** | `live_background_emitter_keeps_session_running`, `running_stderr_only_emitter_is_visible`, `silent_redirected_subprocess_keeps_session_running`, `setsid_nohup_contract` | Escaped descendant policy is explicit rather than accidental |

Bottom line: the happy paths, API-driven cancel paths, external signal paths,
long-running output emitters, and escaped-descendant contract are all covered.
The remaining issue called out by this review is product behavior rather than
test coverage: truly over-buffer stdin writes are still blocking.

---

## 2. Implementation grounding (why the covered edges matter)

- **Running vs completed is pgid scope-wait, not single-process.**
  `eos-runner/src/fresh_ns/child.rs:79` returns only when the root exited **and**
  `process_group_has_other_live_members(pgid)` is false (`child.rs:94`). A
  same-pgid background child therefore keeps the session `running` — this is the
  whole "invisible background process" mechanism.
- **Signal death is encoded and asserted E2E.**
  `eos-workspace-runtime/src/command_session/process/runner.rs:39`:
  `status.signal().map(|signal| -i64::from(signal))` → an externally-killed
  process yields a **negative** `exit_code` (e.g. `-9`, `-15`, `-11`).
  External/self signal tests now read this path; API-driven teardown still goes
  through cancel.
- **Reaping is asymmetric between modes** (the key to gap C):
  - *Ephemeral / fresh-ns (default):* `unshare(NEWUSER | NEWNS)` only — **no
    `NEWPID`** (`fresh_ns.rs:124`). Teardown =
    `DaemonEphemeralCommandPort::release_snapshot` → `LayerStack::release_lease`
    **only** (`ports/ephemeral.rs:73`); there is no `cgroup_path` in
    `EphemeralCommandPrepareContext`. The code says so out loud: *"We
    deliberately do not `killpg` the old children … lease cleanup is left to
    LayerStack GC"* (`eos-daemon/src/adapters/workspace_run/commands.rs:383`).
    Process reaping is **pgid-only** (`killpg` on cancel/timeout). → a `setsid`/double-fork
    escapee gets a new pgid, dodges `killpg`, has no PID-ns and no cgroup
    backstop, and **survives session completion and lease release**.
  - *Isolated:* allocates a `cgroup_path`
    (`eos-workspace-runtime/src/isolated/session/lifecycle.rs:76`) and GC does
    `kill_cgroup_pids` + `reap_named_cgroup_orphans`
    (`eos-workspace-runtime/src/isolated/session/gc.rs:91-156`). → escapees
    **are** reaped at exit/GC.
  This contained-vs-leaky asymmetry is real and now explicitly tested.
- **stderr is merged into the single PTY stream**; the `output.stderr` field is
  always empty. `nonzero_exit_and_stderr_are_structured` covers foreground
  completion, and `running_stderr_only_emitter_is_visible` covers a still-running
  stderr-only emitter.
- **Ctrl-C/Ctrl-D teardown controls are API cancel shortcuts.** Both control
  chars route to command-session cancel instead of a separate interrupt path.

---

## 3. Covered Headline Cases

### A — Killed by another process (external signal)
Covered by `external_signal_kill_is_structured`,
`self_kill_reports_signal_exit`,
`external_sigterm_child_finalizes_via_collect_completed`, and
`external_sigkill_process_group_is_observed_by_write_stdin`. These tests exercise
signal-derived exit codes, lease release, transcript recycling, and one-shot
completion after out-of-band process death.

### B — Long-lived background **emitter** (incl. stderr-only)
Covered by `live_background_emitter_keeps_session_running`,
`running_stderr_only_emitter_is_visible`, and
`silent_redirected_subprocess_keeps_session_running`. These tests prove
same-pgid background work can keep the session `running`, that read_progress
surfaces new transcript output without replay, and that stderr-only output
appears in merged `output.stdout` while `status == running`.

### C — Escaped / invisible descendant contract (the critical one)
Covered by `setsid_descendant_escapes_and_leaks_in_ephemeral`,
`nonsetsid_detach_vectors_stay_tracked`, `setsid_descendant_reaped_on_isolated_exit`,
and `setsid_nohup_contract`. The contract is pinned explicitly in both modes:
ephemeral mode can leak an escaped descendant after lease release, while isolated
mode reaps the same class of escape through its cgroup cleanup.

> **Structural follow-up (not a test):** if tracking invisible background
> processes is a goal, the fix is a teardown backstop for the ephemeral path —
> a cgroup (as isolated already has) or a PID namespace — so lease release reaps
> escapees. The tests below will fail/﻿flip the moment that lands, which
> is the point.

---

## 4. Implemented tests & checklist items, per module

Checklist items use the repo's `workspace-runtime-command-session-<slug>: <description>` style
so they drop into the module checklist. **H** = headline, **S** = secondary.

### `command_session_lifecycle.rs` (core exec/write_stdin + nohup/setsid)
Implemented tests:
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
- [ ] `workspace-runtime-command-session-external-signal-kill`: A session killed by an
  out-of-band signal (second-session `pkill`, self `kill -9 $$`, `SIGSEGV`)
  finalizes with a signal-derived `exit_code`, a non-`ok` status, released lease,
  and exactly one parked completion.
- [ ] `workspace-runtime-command-session-signal-kill-keeps-group`: Externally killing only the
  foreground while a same-pgid peer survives keeps the session `running` and
  completes only after the peer exits.
- [ ] `workspace-runtime-command-session-write-stdin-to-completed`: `write_stdin`/`cancel`
  against a completed-but-uncollected session returns a structured terminal
  status, not a generic not-found.
- [ ] `workspace-runtime-command-session-teardown-control-cancel`: `\x03` and `\x04` through
  `write_stdin` both route to command-session cancel and share the same cleanup
  behavior.

### `command_session_ephemeral_workspace.rs` (process-group semantics)
Implemented tests:
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
- [ ] `workspace-runtime-command-session-live-background-emitter`: A same-pgid child that keeps
  emitting after the foreground exits keeps the session `running`, surfaces new
  output on read_progress reads without replay, and delivers late output in the final
  completion.
- [ ] `workspace-runtime-command-session-running-stderr-visibility`: A still-running
  stderr-only emitter surfaces its stderr in merged `output.stdout` (stderr field
  empty), so never-exiting sessions don't silently drop stderr.
- [ ] `workspace-runtime-command-session-detached-descendant-leak-contract`: An unbounded
  `setsid`/double-fork descendant in ephemeral mode escapes the pgid; the session
  completes and the lease releases while the descendant survives (pgid-only
  reaping, no cgroup/PID-ns backstop). Self-healing bound keeps CI clean.
- [ ] `workspace-runtime-command-session-detach-vector-matrix`: `disown`, `( cmd & )`, bare
  `setsid`, and `&`-then-`exit` each have a pinned tracked-vs-escaped contract.

### `command_session_isolated_workspace.rs` (isolated mode)
Implemented tests:
- **[H-C] `unbounded_setsid_descendant_reaped_on_isolated_exit`** — same unbounded
  `setsid` descendant, but inside `enter_isolated_workspace`; assert
  `exit_isolated_workspace` reaps it via the isolated cgroup
  (`gc.rs` `kill_cgroup_pids`) — marker count → 0 after exit. This is the
  contained counterpart that proves the ephemeral-vs-isolated asymmetry.

Checklist:
- [ ] `workspace-runtime-command-session-detached-descendant-isolated-reap`: An escaped
  `setsid`/double-fork descendant launched in an isolated workspace is reaped by
  the isolated cgroup on exit, establishing the contained-vs-leaky contrast with
  the ephemeral path.

### `command_session_error_and_backpressure.rs` (errors / backpressure)
Implemented / deferred items:
- **[S] `stdin_to_non_reading_consumer_stays_bounded_and_cancellable`** —
  exercises the safe bounded stdin path; the call returns promptly, the session
  remains cancellable, and no lease leaks.
- **[S] completed-result retention** — completed command sessions remain parked
  until the internal background collector pulls them by session id.

Checklist:
- [ ] `workspace-runtime-command-session-stdin-backpressure`: A large stdin payload to a slow
  consumer is bounded and cancellable without leaked sessions or leases.
- [ ] `workspace-runtime-command-session-completed-retention`: Completed-but-not-yet-collected
  sessions remain available until the internal collector drains them.

### `command_session_command_matrix.rs` (family matrix + parallel load)
Observation: all 12 families (`builtin`/`pipeline`/`grep`/`sed`/`awk`/`python`/
`stderr`/`json-and-bytes`/…) are **clean-exit foreground** commands. The matrix
has no signal/kill family and no background/detach family.
Intentionally not added:
- **`signal` family** — signal-death coverage now lives in lifecycle and
  external-process-death tests, where non-`ok` assertions fit naturally.
- **`background` family** — background/detach coverage now lives in lifecycle,
  ephemeral, and isolated tests, where running/escaped/reaped outcomes fit
  naturally.

Checklist:
- [ ] `workspace-runtime-command-session-command-matrix-clean-exit-family`: The matrix remains
  a clean-exit foreground breadth harness; non-`ok` signal and background
  contracts stay in dedicated lifecycle/process tests.

### `command_session_protocol_smoke.rs` (raw protocol smoke)
No new tests required; smoke coverage is adequate. Optional: a single raw-protocol
external-kill smoke if gap-A coverage should also be visible at the wire layer.

---

## 5. Current Follow-ups

1. **Shipped coverage** — escaped/invisible descendants, external/signal kill,
   live background emitters, running stderr visibility, write-stdin-to-completed,
   Ctrl-C/Ctrl-D cancellation, and completed-result retention are covered.
2. **Remaining product follow-up** — true over-buffer stdin backpressure still
   needs a nonblocking/time-sliced writer before it can have a safe live test.

---

## 6. Findings — deferred items that are product gaps, not tests

Investigating the last secondary items surfaced one behavior that **does not
exist to be asserted as passing**. It is recorded here as a finding (with code
evidence) instead of being forced into a green test.

### F1 — stdin write is an unbounded blocking `write_all`
`runner.rs:158` is `lock(&self.writer).write_all(bytes)`, and the PTY master is
opened without `O_NONBLOCK` (`pty.rs:7`). A payload exceeding the kernel PTY input
buffer, sent to a consumer that never reads stdin, blocks the writer thread while
holding the writer mutex — a per-session wedge.
- **Shipped instead:** `stdin_to_non_reading_consumer_stays_bounded_and_cancellable`
  exercises the *safe* path (1 KiB, one line, under the buffer bound) and asserts
  the write returns promptly and the session stays cancellable. The full
  >buffer backpressure case is deliberately not a test because it would hang CI.
- **Recommendation:** bound / time-slice / non-block the stdin write, then a true
  `workspace-runtime-command-session-stdin-backpressure` test becomes safe to add.

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
