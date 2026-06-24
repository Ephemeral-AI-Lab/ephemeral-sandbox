# Phase 2 — Adversarial Correctness & Completeness Review (Findings)

Review target: `sandbox-runtime-namespace-execution` crate + the one re-export
shim `operation/src/namespace_execution.rs`, against
[`phase-2-spec.md`](./phase-2-spec.md) (§7 Acceptance Criteria, §2 Resolved
Design Decisions) and the [`migration-phases.md`](./migration-phases.md) §"Phase 2"
boundary. Read from source on disk at commit `294f4c726` (engine wiring
`8befb8d49`, fakes relocation `705aa86f0`). Method: refute-first; every finding
below survived an attempt to refute it. Code was read as-is, not trusted from
comments or commit messages.

---

## Executive verdict

**Qualified YES — Phase 2 is complete and correct per §7.** All fourteen §7
acceptance criteria are satisfied and were independently verified (gates below).
The load-bearing invariant — `registry.complete` **before** `promise.resolve` —
is correctly implemented and holds on **every** watcher path (shell Ok/Err, mount
Ok/Err, `wait_completion` Err, cancel): `engine.rs:134-136` is an unconditional
sequential tail after the result `match`. No boundary/phase leak exists. All five
sanctioned deviations are **sound** (assessed individually below).

The qualification is entirely about **regression protection and forward
robustness**, not present behavior:

- Four behaviors are correct-by-construction but have **thin, indirect, or absent
  test coverage** (`wait_completion`→Err path, the `run_mount` mode-flag plumbing
  that deviation #3 exists to justify, the §2.9 request fields beyond `request_id`,
  and the non-string/non-object `status()` inputs). A future regression in any of
  these would not be caught by the suite.
- The watcher is **not panic-safe around `finalize`/`parse`** — a latent hazard
  that does not fire in Phase 2 but will be reachable the moment Phase 3 wires
  real operation closures.
- The real `ForkRunnerLauncher`/`ForkRunnerChild` path is **compile-coverage only**
  on the darwin dev host (spec §6), so its runtime behavior (fork, start-ack
  handshake, `killpg`, result-fd EOF) is verified only by inspection + a
  byte-level relocation-fidelity comparison against the original
  `command/src/pty.rs`, never executed.

No Critical or High findings. Two Medium (both forward-looking / coverage), the
rest Low.

### Verification gates (all green)

| Gate | Result |
|---|---|
| `cargo test -p sandbox-runtime-namespace-execution` | **ok** — 7 engine + 2 pty + 5 registry + 4 status + 3 promise + 1 execution + 1 id |
| `cargo clippy -p … --all-targets --no-deps -- -D warnings` (with `test-support`) | clean |
| `cargo clippy -p … --no-deps -- -D warnings` (pure lib, no `test-support`) | clean |
| `cargo test -p sandbox-runtime --tests` (relocation regression) | **ok** — 25 passed |
| `cargo check -p sandbox-daemon` (consumes re-exported status) | clean |
| `cargo fmt --check` | clean |
| `xtask check-inline-tests` / `check-cfg` | "no forbidden inline attributes" / "no `#[cfg]`" |
| Phase 3-6 symbol grep over `…/src` | no leak |
| `start[-_]ack` grep | present (KEEP, §2.11) |
| command/workspace/daemon source touched by any Phase 2 commit | **none** |
| engine suite looped 30× + `--test-threads=1` | 0 failures (no observable flakiness) |

---

## Findings

### F1 — A panicking `finalize`/`parse` permanently leaks the admission slot and hangs `wait()` forever
- **Severity:** Medium · **Category:** Risk/smell (works today, fragile for Phase 3)
- **Evidence:** `crates/sandbox-runtime/namespace-execution/src/engine.rs:123` runs
  `finalize(outcome)` *inside* the result `match`; `registry.complete` (`:134`),
  `promise.resolve` (`:135`), and `observer.on_terminal` (`:136`) run **after** it.
- **Trigger:** A `ShellOperation::finalize` or a `run_mount` `parse` closure that
  panics (e.g. an `unwrap`/`expect`/index in real caller code). The watcher thread
  unwinds at `:123`; `:134-136` never execute.
- **Observable failure:** The reserved slot is never moved out of `live` and never
  `abort`ed → it is leaked **permanently**, so admission capacity silently shrinks
  by one for the process lifetime. The promise is never resolved → the caller's
  `ExecutionHandle::wait()` (`execution.rs:29`) blocks **forever**. `on_terminal`
  never fires → the observer never records a terminal.
- **Why it matters for the phase contract:** The engine *is* the Phase 2
  deliverable Phase 3 builds on, and Phase 3's `ExecCommand::finalize` runs exactly
  here. The watcher being the slot's only releaser makes a finalize panic
  unrecoverable. Not reachable in Phase 2 (the test ops return `Ok`/`Err`, never
  panic), so it is not a §7 failure — but it is a real fragility in shipped code.
- **Refutation attempted:** Could `complete` run before `finalize`? No — the
  `Error`-arm status (`:127`) is derived independently of finalize, but the code
  chose to compute the whole tuple first. Could a test catch it? No test exercises
  a panicking finalize.
- **Fix direction:** Wrap `finalize(outcome)` in `std::panic::catch_unwind` (the
  closure is already `Send + 'static`), mapping a caught panic to a terminal
  `Error` so `complete`/`resolve`/`on_terminal` still run; or release the slot with
  the wire `status` (known before finalize) and treat finalize purely as
  promise-payload production.

### F2 — `run_mount` mode-flag plumbing (the reason deviation #3 exists) is untested
- **Severity:** Medium · **Category:** Completeness gap (untested requirement)
- **Evidence:** Engine forwards the flag correctly — `engine.rs:91`
  `self.launcher.spawn_piped(mode_flag, request)`. But the fake **discards** it:
  `tests/support/mod.rs:143-150` `fn spawn_piped(&self, _mode_flag: &'static str, …)`
  records only `request` and never the flag. No assertion in `tests/engine.rs`
  (`mount_execution_resolves_parsed_output:116`, `mount_parse_error…:138`) inspects
  which flag was passed.
- **Trigger:** Edit `run_mount` to pass a constant, swap `--mount-overlay` ↔
  `--remount-overlay`, or drop the arg.
- **Observable failure:** None — the suite stays green. The daemon would silently
  run the wrong overlay mode once Phase 4 wires it.
- **Why it matters:** §2.9 requires mount to pass `--mount-overlay`/`--remount-overlay`
  and shell to pass none; sanctioned deviation #3 *only exists* to carry this flag
  through the trait box. The behavior is correct in code but has **zero** regression
  protection at the exact seam the deviation was created for.
- **Refutation attempted:** Is shell's "no flag" covered? Structurally yes —
  `spawn_pty` has no flag parameter, so shell cannot pass one (runner defaults to
  `Run`, `daemon/src/runner.rs:169`). Only the mount flag *value* is the gap.
- **Fix direction:** Have `FakeLauncher` record `(mode_flag, request_id)` for
  `spawn_piped`; assert the two mount tests recorded `"--mount-overlay"` /
  `"--remount-overlay"` respectively.

### F3 — §2.9 request construction is verified only for `request_id`; `args`/`timeout`/target fields are unchecked
- **Severity:** Low · **Category:** Completeness gap
- **Evidence:** The only request assertion is `tests/engine.rs:164`
  `fake.recorded_request_ids() == vec![id.0]`. `FakeLauncher::record`
  (`tests/support/mod.rs:119-125`) stores **only** `request.request_id`. The
  builders `engine.rs:141-161` (`shell_args` → `json!({command, cwd:"."})`, mount →
  empty object, `timeout_seconds`, `workspace_root`/`layer_paths`/`upperdir`/
  `workdir`/`ns_fds`) are never asserted.
- **Trigger:** Break `shell_args` (e.g. drop `cwd`), pass `op.timeout_seconds()`
  for mount, or mis-map a target field.
- **Observable failure:** Suite stays green; the runner receives a malformed
  request at Phase 3/4 runtime.
- **Why it matters:** §2.9 is a Phase 2 in-scope decision (the engine "builds the
  request … so the fake can assert it"). The fake captures the request but asserts
  almost none of it. §7's checklist only demands the `request_id == id.0` slice, so
  this is not a §7 failure — but the spec's stated intent ("so the fake can assert
  it") is only partially realized.
- **Fix direction:** Record the full `NamespaceRunnerRequest` in the fake; assert
  `args`, `timeout_seconds`, and the target fields for one shell and one mount case.

### F4 — `RunnerOutcome::status()` is correct for non-string / non-object payloads but those cases are untested
- **Severity:** Low · **Category:** Completeness gap (test only; code is correct)
- **Evidence:** `shell.rs:22-30` uses `payload.get("status").and_then(Value::as_str)`,
  which returns `None` (→ default `Error`, **no panic**) for a non-object payload
  (`Value::Null`/string/array) or a non-string `status` (e.g. `{"status":42}`).
  `tests/status.rs:40-50` covers only `absent` and `"bogus"`.
- **Trigger:** `RunResult { payload: json!(42) }` or `json!({"status":7})`.
- **Observable failure:** None at runtime (correctly maps to `Error`); the gap is
  purely missing coverage of the §2.6 "exhaust the payload space" intent.
- **Why it matters:** Minor — the code already satisfies §2.6's "default `Error`
  when absent/unrecognized" for every shape; only the test is non-exhaustive.
- **Fix direction:** Add two `status.rs` cases (non-string status, non-object
  payload) asserting `Error`.

### F5 — `wait_completion` → `Err` watcher path is never exercised
- **Severity:** Low · **Category:** Completeness gap
- **Evidence:** `FakeRunnerChild::wait_completion` (`tests/support/mod.rs:69-73`)
  returns `Ok(self.completion.wait())` **unconditionally** — it has no error mode.
  The watcher's `Err` arm `engine.rs:132` (`→ (Err, Error, None)`) is therefore
  dead in tests.
- **Trigger:** A fork `child.wait()`/result-read failure (real path only).
- **Observable failure:** None today. The invariant (`complete` before `resolve`)
  *does* hold on this path by inspection — `:134-136` are unconditional — but no
  test proves the spawn-error projection (`status=Error`, `exit_code=None`).
- **Why it matters:** The prompt's attack surface explicitly enumerates
  "`wait_completion` Err" as a path that must preserve the invariant; it is correct
  but unproven.
- **Fix direction:** Give the fake an opt-in error mode and add a test asserting a
  terminal `Error` with `exit_code == None` and that `is_completed` holds after
  `wait()` errors.

### F6 — complete-before-resolve is covered only indirectly and timing-dependently
- **Severity:** Low · **Category:** Test-quality / Risk
- **Evidence:** The sole behavioral check is
  `admission_refuses_when_full_then_readmits_after_completion`
  (`tests/engine.rs:79-106`): after `first.wait()` it expects a third admission to
  succeed (`:101-103`). This works because `complete` (`engine.rs:134`) precedes
  `resolve` (`:135`), so the slot is free when `wait()` returns. The engine does
  **not** expose its registry, so a direct `is_completed(id)` assertion after
  `wait()` is impossible from the test.
- **Trigger:** Hypothetically reorder to `resolve` before `complete`.
- **Observable failure:** The test would become **flaky** (the watcher races the
  main thread to the registry lock), not deterministically red — so it would not
  reliably catch the regression it is meant to guard. Looped 30× and run
  `--test-threads=1` here: 0 failures on the correct implementation.
- **Why it matters:** This is the invariant the admission/readmission design hinges
  on (§2.4); its only guard is a race-shaped proxy.
- **Fix direction:** Expose a test-only `engine.registry_is_completed(id)` (or
  reuse the `test-support` facade) and assert it `true` immediately after `wait()`
  returns, making the ordering deterministic to verify.

### F7 — Fork-backing error paths leak an unreaped child (zombie); compile-coverage only
- **Severity:** Low · **Category:** Risk/smell (fork path, not run in Phase 2)
- **Evidence:** In `launcher.rs:68` the child is spawned; if `PtyMaster::spawn`
  fails (`:73-78`) or `release_start_ack` fails (`:79`), the function returns `Err`
  and the `std::process::Child` is dropped **without** `wait()`/kill. The engine
  then calls `registry.abort` (`engine.rs:64`), so the **slot is not leaked**, but
  the OS process is left unreaped. The dropped `start_ack_write` closes the ack
  pipe, so the child hits EOF on `read_exact` (`daemon/src/runner.rs:183-188`) and
  exits on its own — it becomes a zombie rather than a runaway, but is never reaped.
- **Trigger:** `set_nonblocking`/`try_clone` failure in `PtyMaster::spawn`, or a
  write failure in `release_start_ack`, after a successful `command.spawn()`.
- **Observable failure:** Accumulating zombie processes under repeated rare
  failures. Not reachable in Phase 2 (fork path is darwin compile-coverage only).
- **Why it matters:** Inherited structure from the original two-phase
  `spawn_current_exe_ns_runner` + `allow_start` (which `terminate()`d on ack/request
  write failure — see `command/src/pty.rs:299-306`); the relocation **dropped the
  `terminate()` on the failure path**. So this is a small fidelity regression, not a
  faithful relocation, on the error branch.
- **Fix direction:** On the post-spawn error paths, `terminate_process_group(pgid)`
  + `child.wait()` before returning `Err`, mirroring the original `allow_start`.

### F8 — `ForkRunnerChild::wait_completion` (inline result read) can deadlock on a large payload
- **Severity:** Low · **Category:** Risk/smell — **sanctioned** (§2.2), informational
- **Evidence:** `launcher.rs:113-120` does `child.wait()` **then**
  `read_to_end(result_read)`. The result-fd reader thread that the original used
  (`command/src/pty.rs:443-451`) is intentionally removed (§2.2).
- **Trigger:** A runner that writes a `RunResult` larger than the pipe buffer
  (~64 KiB): the child blocks on `write` (pipe full, nobody draining), the parent
  blocks in `child.wait()` (child hasn't exited) → deadlock.
- **Why it matters:** §2.2 explicitly sanctions this ("safe for the small
  status/exit/mount-diagnostic payloads; a large-payload op would need a reader
  thread — a Future Extension"). Recorded here only because it is a **capability
  regression vs. the original** that Phase 3/4 must respect (do not route large
  payloads through `--result-fd`). Not a Phase 2 defect.
- **Fix direction:** None for Phase 2. Phase 3 should restore a result-fd reader
  thread if any operation can produce an unbounded payload.

### F9 — Concurrent `spawn_*` can leak a sibling's non-CLOEXEC fds into the wrong child; compile-coverage only
- **Severity:** Low · **Category:** Risk (fork path, pre-existing)
- **Evidence:** `request_pipe`/`start_ack_pipe` leave the read end **non-CLOEXEC**
  and `result_pipe` leaves the write end non-CLOEXEC (`launcher.rs:188-207`) — as
  required so the intended child inherits them. With `max_active > 1` the engine
  permits concurrent `spawn_pty`/`spawn_piped`; the classic fork/exec window means
  one spawn's `Command::spawn` can inherit another in-flight spawn's non-CLOEXEC
  fds.
- **Why it matters:** The new engine *promotes* concurrency (admission up to
  `max_active`) where the original command path was effectively serialized, so the
  latent race is more reachable post-Phase-3. Faithfully relocated fd discipline;
  compile-coverage only in Phase 2.
- **Fix direction:** Serialize the spawn critical section, or use
  `posix_spawn_file_actions`/explicit fd remap so only the intended fds are
  inheritable. Defer to Phase 3/4 when the path runs.

### F10 — `ExecutionRegistry::live_pgid` is added beyond the §2.8 API and is unused in production
- **Severity:** Low · **Category:** Smell (minor over-build)
- **Evidence:** `registry.rs:91-95` adds `live_pgid`, not in the §2.8 method list;
  only `tests/registry.rs:51` reads it (no `src/` caller).
- **Why it matters:** It is the Phase 5 cancel-handle reader surfaced early. Harmless
  (generic `Option<i32>`, no Phase-3/5 type leaks, `pub` so no `dead_code`), but it
  anticipates a later phase. The stored `LiveExecution.pgid` itself *is* in §2.8.
- **Fix direction:** Optional — drop until Phase 5, or keep as a tested accessor.

---

## Sanctioned-deviation soundness (each judged, not flagged as a violation)

| # | Deviation | Verdict | Basis |
|---|---|---|---|
| 1 | Tests in `tests/`, fakes in `tests/support/mod.rs`, seam surfaced via `#[cfg(feature="test-support")] pub mod test_support` | **Sound** | Facade is re-export-only — `lib.rs:40-45` contains zero logic. `src/` is production-only (`check-inline-tests`/`check-cfg` green). The traits are `pub` (not `pub(crate)`) **because** a `pub use` facade cannot re-export a crate-private item; the private `mod launcher;` keeps them unreachable in the default build, and `unreachable_pub` is **not** in the workspace lints (`Cargo.toml:73-86`), so both clippy gates stay clean. |
| 2 | `ExecutionRegistry`/`CompletedExecution` root-exported; `RunnerOutcome::new` `pub` | **Sound** | `CompletedExecution` carries only `{status, exit_code}` (`registry.rs:27-31`) — no command/Phase-3 types. Public export delta is exactly +4 (`Engine`, `TerminalStatus` per §2.12, plus `Registry`/`CompletedExecution`); the 8 Phase-1 re-exports are intact (`lib.rs:26-34`); seam types (`NsRunnerLauncher`, `RunnerChild`, `PtyMaster`, `CompletionPromise`) are public **only** behind the gated facade. Nothing else widened silently. |
| 3 | `spawn_piped(mode_flag, request)` carries the flag | **Sound but untested** | Engine passes none for shell (`spawn_pty` has no flag param) and the caller's flag for mount (`engine.rs:91`); daemon defaults to `Run` (`runner.rs:169`) and parses the two overlay flags (`runner.rs:121-122`). Correct — but see **F2**: no test asserts it. |
| 4 | Transcript drains **raw** bytes (no timestamp prefix) | **Sound** | `time` is not an approved dep (§2.10). `spawn_output_reader` (`pty.rs:114-138`) extends the buffer with raw bytes; `read_output_since`/`output_len` operate on raw bytes with bounds-safe slicing + `from_utf8_lossy` (`pty.rs:90-107`). Only consequence is the missing prefix; in-memory sink is correct. |
| 5 | `CompletionPromise::wait_timeout` gated behind `test-support` | **Sound** | `promise.rs:59-83` is the `bool` form per §2.5; engine never calls it; gate compiles out cleanly in the pure-lib clippy run (no `dead_code`). Used only by `tests/promise.rs`. |

A fidelity note on the fake vs. real cancel: a cancelled **fake** returns
`{"status":"cancelled"}` (→ `Cancelled`, `tests/support/mod.rs:45-50`), while the
**real** fork path's `synthesize_result` (`launcher.rs:171-180`) emits
`{"status":"error"}` for a signal-killed child (→ `Error`). This divergence is
sanctioned by §2.6 (the cancel→"cancelled" override is a Phase-3 `finalize`
concern); noted so Phase 3 does not assume the fake's status reflects the real
path.

---

## §7 acceptance-criterion coverage

| # | §7 criterion | Verdict | Proven by |
|---|---|---|---|
| 1 | child-exit → promise resolves finalized `Output` (shell + mount); `wait()` yields it; observer records `on_terminal` | **Covered** | `engine.rs` tests `shell_execution_resolves_finalized_output_and_records_terminal`, `mount_execution_resolves_parsed_output` (assert `wait()` value + `await_terminal` + first event `Running`) |
| 2 | finalize/parse error → terminal `NamespaceExecutionError`; `on_terminal` status `Error` | **Covered** | `shell_finalize_error_resolves_terminal_error`, `mount_parse_error_resolves_terminal_error` |
| 3 | `wait_timeout(Duration)->bool` blocks then `true` on resolve (no poll); `false` while pending | **Covered** | `promise.rs` `wait_timeout_blocks_until_resolved_from_another_thread`, `wait_timeout_returns_false_while_pending` |
| 4 | `cancel()` unblocks the watcher blocked in `wait_completion`; promise resolves promptly (real concurrent unblock) | **Covered** | `cancel_unblocks_the_blocked_watcher` (FakeCompletion is a genuine `Condvar` block tripped from another thread) |
| 5 | admission: (`max_active`+1)th `run_*` → `Err(Admission{max_active})`; readmits after one completes | **Covered** (invariant guard **Weak** — F6) | `admission_refuses_when_full_then_readmits_after_completion`; `registry.rs` `admits_up_to_capacity_then_refuses` |
| 6 | `run_mount(flag,…,parse)` resolves parsed `Output`; `.wait()` returns it (no PTY) | **Covered** (flag value **Missing** — F2) | `mount_execution_resolves_parsed_output` |
| 7 | `namespace_execution_id` IS runner `request_id`; `exec.id().0 == id.0` | **Covered** | `namespace_execution_id_is_the_runner_request_id` |
| 8 | launcher still passes `--start-ack-fd` + writes the ack byte | **Covered** (by grep + inspection; not runtime-run) | `launcher.rs:139-140` arg, `:151` `write_all(b"1")`; runner consumes it `daemon/src/runner.rs:183-188`. Fork path is darwin compile-coverage (spec §6) |
| 9 | `status()` maps `ok/error/timed_out/cancelled` (default `Error`); `payload()` → `&Value` | **Covered** (non-string/non-object **Weak** — F4) | `status.rs` `status_projects_…`, `status_defaults_to_error_when_absent_or_unknown`, `payload_exposes_the_raw_value` |
| 10 | `NamespaceExecutionTerminalStatus` defined once in `status.rs`, original derives/variants/strings; `operation` re-exports; both `sandbox_runtime::…` and `crate::namespace_execution::…` resolve | **Covered** | `status.rs` `as_str_strings_match_the_wire_vocabulary`; `operation/src/namespace_execution.rs:8-10` re-export; relocation regression `cargo test -p sandbox-runtime --tests` (25 ok); `cargo check -p sandbox-daemon`. Daemon mapping `observability/namespace_execution.rs:71-77` byte-identical |
| 11 | no Phase 3-6 symbol leaked; no command/workspace/daemon source changed (only operation shim) | **Covered** | absence grep clean; no Phase 2 commit touches the three dirs; operation diff is `namespace_execution.rs` only |
| 12 | `clippy --all-targets --no-deps -- -D warnings` clean (boxed-trait seam passes `private_interfaces`/`private_bounds`) | **Covered** | both clippy gates green (with and without `test-support`) |
| 13 | `cargo test -p sandbox-runtime --tests` + `cargo check -p sandbox-daemon` pass | **Covered** (daemon status test compile-checked only — see caveat) | 25 tests ok; daemon check clean |
| 14 | `git diff --check` passes; LOC reported via `--numstat` | **Covered** | `fmt --check` clean; LOC below |

**No criterion is Missing.** Two are Weak on the regression-protection axis (F2 →
#6 flag value; F6 → #5 invariant directness) and two have a minor coverage gap
(F4 → #9; the `wait_completion`-Err path F5 sits under #1/#2). All pass as written.

### Reported LOC (committed; `git show --numstat`)

Engine-crate `src/`: `engine.rs +161`, `launcher.rs +211`, `pty.rs +190`,
`registry.rs +89/-3`, `execution.rs +29/-8`, `promise.rs +28`, `shell.rs +23`,
`status.rs +24`, `observer.rs +7`, `lib.rs net (split across two commits)`,
`Cargo.toml +5`. Tests: `engine +166`, `registry +58/-1`, `status +58`, `pty +42`,
`promise +25`, `support/mod.rs ~320` (relocated from a former `src/fakes.rs`).
Operation shim: `namespace_execution.rs +3/-21`. Higher than the spec's ~+696
estimate because the suite + fakes live in `tests/` (deviation #1) rather than
inline `#[cfg(test)]` as the spec body assumed — a reorganization, not extra scope.

---

## Could NOT verify (and why)

1. **Real fork runtime behavior** — `ForkRunnerLauncher`/`ForkRunnerChild` (fork of
   `current_exe ns-runner`, the live start-ack handshake, `killpg` cancel reaching
   the child's own pgid, result-fd EOF after `child.wait()`). The dev host is darwin
   and the fork's runtime side is effectively Linux-only (spec §6), so this path is
   **compile-coverage only**. Cleared by inspection + a byte-level fidelity diff vs.
   `command/src/pty.rs`: pipe CLOEXEC discipline (`launcher.rs:188-207` ≡
   `pty.rs:377-396`), `open_pty_pair` (≡ `pty.rs:463-483`), and
   `terminate_process_group` (≡ `pty.rs:485-490`) are identical; the only behavioral
   divergences are the intentional inline result read (§2.2, F8) and the dropped
   post-spawn `terminate()` on the error branch (F7). Cannot assert runtime correctness.
2. **Daemon status-mapping at runtime** — §7 prescribes `cargo check -p sandbox-daemon`
   (compile only), so `daemon/tests/unit/observability.rs` was not executed here.
   `as_str` parity verified by source inspection (identical
   `"ok"/"error"/"timed_out"/"cancelled"`).
3. **`PtyMaster` reader EOF on darwin after the fake drops the slave** — the engine
   tests never read PTY output (the fake `spawn_pty` drops the slave immediately,
   `tests/support/mod.rs:139`), so reader drain/EOF on the *fake* path is untested;
   real PTY drain is covered separately on a live `openpt` pair (`tests/pty.rs`,
   which uses a 2 s polling wait — not racy).

---

## Bottom line

Phase 2 ships a correct, boundary-clean engine that meets every §7 criterion; the
`complete`-before-`resolve` invariant and the cancel/admission/dispatch behaviors
are right and (mostly) load-bearingly tested. The work to do before relying on it
in Phase 3 is **regression hardening, not bug-fixing**: make the watcher panic-safe
around `finalize` (F1), and close the untested seams the suite currently lets
through — the mount mode-flag (F2), the request fields (F3), and the
`wait_completion`-Err path (F5). The fork-path items (F7–F9) are deferrable until a
real caller exercises that path.
