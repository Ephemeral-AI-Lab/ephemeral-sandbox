# Sandbox E2E Jun 13

Append-only report for Phase 05+ E2E attempts from
`docs/plans/sandbox-event-tracing-and-response-contract_SPEC.md`.

Rules for this run:

- Run target failures first after any failure.
- Do not retry a suite after an early success or repeated good result.
- For each attempt, record command, result, finding, and fix.

## Attempts

### 2026-06-13 Attempt 1 - Phase 05 e2e inventory list

- Command: `cargo test -p eos-e2e-test -- --list`
- Result: stopped; the command compiled successfully, printed the library test
  inventory, then moved to `tests/core/mod.rs` without completing the full
  inventory in the allowed observation window.
- Finding: the broad inventory gate is not a useful first retry target because
  it can stall inside a specific suite after partial success.
- Fix: do not rerun the broad list immediately; run the targeted suite
  inventory first (`core -- --list`) and only return to the broad gate after the
  stuck suite is understood.

### 2026-06-13 Attempt 2 - Targeted core inventory

- Command: `cargo test -p eos-e2e-test --test core -- --list`
- Result: passed; listed 32 tests.
- Finding: `core` inventory itself is healthy, so the stopped broad list was
  not caused by a `core` test-binary startup problem.
- Fix: continue targeted inventory runs for the remaining suite binaries before
  retrying the broad list gate.

### 2026-06-13 Attempt 3 - Parallel targeted inventory batch

- Command: parallel `cargo test -p eos-e2e-test --test {daemon,ephemeral_workspace,workspace-runtime-isolated,eos-layerstack} -- --list`
- Result: mixed; `ephemeral_workspace` passed and listed 12 tests, while
  `daemon`, `workspace-runtime-isolated`, and `eos-layerstack` were stopped
  after entering their test binaries without producing inventory output.
- Finding: parallel `--list` runs introduce enough lock/contention noise that
  stopped suites cannot be treated as product failures.
- Fix: avoid parallel inventory retries; rerun stopped suites one at a time.

### 2026-06-13 Attempt 4 - Targeted daemon inventory

- Command: `cargo test -p eos-e2e-test --test daemon -- --list`
- Result: passed; listed 12 tests.
- Finding: `daemon` inventory is healthy when run alone.
- Fix: no code fix needed; keep subsequent inventory retries serial.

### 2026-06-13 Attempt 5 - Targeted isolated inventory

- Command: `cargo test -p eos-e2e-test --test workspace-runtime-isolated -- --list`
- Result: passed; listed 21 tests.
- Finding: `workspace-runtime-isolated` inventory is healthy when run alone.
- Fix: no code fix needed; keep inventory retries serial.

### 2026-06-13 Attempt 6 - Targeted layerstack inventory

- Command: `cargo test -p eos-e2e-test --test eos-layerstack -- --list`
- Result: passed; listed 20 tests.
- Finding: `eos-layerstack` inventory is healthy when run alone.
- Fix: no code fix needed; keep inventory retries serial.

### 2026-06-13 Attempt 7 - Targeted workspace-publish-gate inventory

- Command: `cargo test -p eos-e2e-test --test workspace-publish-gate -- --list`
- Result: passed; listed 14 tests.
- Finding: `workspace-publish-gate` inventory is healthy when run alone.
- Fix: no code fix needed; keep inventory retries serial.

### 2026-06-13 Attempt 8 - Targeted command runtime inventory

- Command: `cargo test -p eos-e2e-test --test workspace-runtime-command -- --list`
- Result: passed; listed 67 tests.
- Finding: `workspace-runtime-command` inventory is healthy when run alone,
  although startup is slower than smaller suites.
- Fix: no code fix needed; keep inventory retries serial and allow a longer
  observation window for large suites.

### 2026-06-13 Attempt 9 - Targeted pressure inventory

- Command: `cargo test -p eos-e2e-test --test pressure -- --list`
- Result: stopped; the binary entered `tests/pressure/mod.rs` and produced no
  inventory output for roughly 90 seconds.
- Finding: `sample` showed the pressure test binary stuck at `_dyld_start`
  before Rust test discovery, so this is a targeted startup/list failure.
- Fix: inspect pressure test startup/linkage before retrying pressure or the
  broad list gate.

### 2026-06-13 Attempt 10 - Direct pressure binary inventory

- Command: `./target/debug/deps/pressure-a70af65e34da6dad --list`
- Result: passed; listed 23 tests.
- Finding: pressure test discovery is healthy in the built binary; the prior
  failure was in the Cargo-launched process startup path, not the pressure test
  registry.
- Fix: retry the Cargo pressure inventory once to confirm whether the startup
  stall was transient before returning to the broad list gate.

### 2026-06-13 Attempt 11 - Cargo pressure inventory retry

- Command: `cargo test -p eos-e2e-test --test pressure -- --list`
- Result: passed; listed 23 tests.
- Finding: the earlier Cargo-launched pressure startup stall was transient.
- Fix: no code fix needed; do not repeat this successful pressure inventory.

### 2026-06-13 Attempt 12 - Targeted plugin inventory

- Command: `cargo test -p eos-e2e-test --test plugin -- --list`
- Result: passed; listed 15 tests.
- Finding: `plugin` inventory is healthy when run alone.
- Fix: no code fix needed; retry the broad `-- --list` gate once because all
  targeted inventories now pass.

### 2026-06-13 Attempt 13 - Phase 05 aggregate e2e inventory list

- Command: `cargo test -p eos-e2e-test -- --list`
- Result: passed; listed the library test plus all suite inventories
  (`core`, `daemon`, `eos-layerstack`, `ephemeral_workspace`, `plugin`,
  `pressure`, `workspace-publish-gate`, `workspace-runtime-command`, and
  `workspace-runtime-isolated`).
- Finding: after serial targeted inventories warmed and proved each suite, the
  aggregate list gate is healthy.
- Fix: no code fix needed; proceed to focused live E2E suites without repeating
  the successful list gate.

### 2026-06-13 Attempt 14 - E2E stale-container cleanup

- Command: `cargo run -p eos-e2e-test --bin e2e-reap`
- Result: passed; removed 1 stale `eos-e2e` container.
- Finding: stopped inventory attempts left one stale live-test container.
- Fix: cleanup complete; start focused live suites from a clean pool.

### 2026-06-13 Attempt 15 - Focused live core suite

- Command: `cargo test -p eos-e2e-test --features e2e --test core -- --nocapture`
- Result: passed; 32 passed, 0 failed.
- Finding: core direct-file, protocol, runtime-readiness, and wire guard
  checks pass against the live daemon.
- Fix: no code fix needed; do not rerun `core`.

### 2026-06-13 Attempt 16 - Focused live daemon suite

- Command: `cargo test -p eos-e2e-test --features e2e --test daemon -- --nocapture`
- Result: passed; 12 passed, 0 failed.
- Finding: daemon runtime identity, envelope-meta, inflight, heartbeat,
  cancellation, TTL reaper, and plugin background-control checks pass live.
- Fix: no code fix needed; do not rerun `daemon`.

### 2026-06-13 Attempt 17 - Focused live eos-layerstack suite

- Command: `cargo test -p eos-e2e-test --features e2e --test eos-layerstack -- --nocapture`
- Result: passed; 20 passed, 0 failed.
- Finding: live LayerStack lease, squash, workspace commit, git overlay commit,
  and trace phase checks pass.
- Fix: no code fix needed; do not rerun `eos-layerstack`.

### 2026-06-13 Attempt 18 - Focused live ephemeral_workspace suite

- Command: `cargo test -p eos-e2e-test --features e2e --test ephemeral_workspace -- --nocapture`
- Result: failed; 11 passed, 1 failed:
  `test_ephemeral_workspace_overlay_exec::live_trace_ephemeral_exec_records_command_overlay_resource_and_response_facts`.
- Finding: the failed assertion inspected a `sandbox.command.poll` trace sidecar
  containing `progress_read`, not the original `exec_command` sidecar that
  carries `command.prepared` and `command.spawned`. The helper returned the poll
  response because this run did not finish within the test's foreground yield.
- Fix: update that trace-sidecar test to use the longer foreground yield used
  by heavier trace/resource checks, then rerun only the failing test.

### 2026-06-13 Attempt 19 - Targeted ephemeral trace retry

- Command: `cargo test -p eos-e2e-test --features e2e --test ephemeral_workspace test_ephemeral_workspace_overlay_exec::live_trace_ephemeral_exec_records_command_overlay_resource_and_response_facts -- --exact --nocapture`
- Result: passed; 1 passed, 0 failed.
- Finding: the longer foreground yield keeps this trace assertion on the
  original `exec_command` sidecar and the command/overlay/resource facts are
  present.
- Fix: no additional code fix needed; rerun the focused
  `ephemeral_workspace` suite once to verify the failed suite.

### 2026-06-13 Attempt 20 - Focused live ephemeral_workspace suite retry

- Command: `cargo test -p eos-e2e-test --features e2e --test ephemeral_workspace -- --nocapture`
- Result: failed; 11 passed, 1 failed:
  `test_ephemeral_workspace_overlay_exec::exec_upperdir_is_flat_across_base_sizes`.
- Finding: the prior trace assertion passed, but the base-size sweep hit
  `read response: Resource temporarily unavailable (os error 35)`.
- Fix: rerun only `exec_upperdir_is_flat_across_base_sizes` before making code
  changes so the failure is classified as deterministic or transient.

### 2026-06-13 Attempt 21 - Targeted ephemeral base-size sweep retry

- Command: `cargo test -p eos-e2e-test --features e2e --test ephemeral_workspace test_ephemeral_workspace_overlay_exec::exec_upperdir_is_flat_across_base_sizes -- --exact --nocapture`
- Result: passed; 1 passed, 0 failed.
- Finding: the EAGAIN response-read failure did not reproduce in the targeted
  test; the base-size sweep logic is healthy in isolation.
- Fix: no code fix for this transient; rerun the focused
  `ephemeral_workspace` suite once to verify the suite after the earlier
  trace-yield fix.

### 2026-06-13 Attempt 22 - Focused live ephemeral_workspace suite final retry

- Command: `cargo test -p eos-e2e-test --features e2e --test ephemeral_workspace -- --nocapture`
- Result: passed; 12 passed, 0 failed.
- Finding: ephemeral overlay exec, trace/resource, cancellation, stale conflict,
  and O(1) overlay disk checks pass live after the trace-yield fix.
- Fix: no additional code fix needed; do not rerun `ephemeral_workspace`.

### 2026-06-13 Attempt 23 - Focused live workspace-publish-gate suite

- Command: `cargo test -p eos-e2e-test --features e2e --test workspace-publish-gate -- --nocapture`
- Result: passed; 14 passed, 0 failed.
- Finding: live OCC route gating, conflict handling, direct/drop routing, and
  publish audit accounting checks pass.
- Fix: no code fix needed; do not rerun `workspace-publish-gate`.

### 2026-06-13 Attempt 24 - Focused live workspace-runtime-command suite

- Command: `cargo test -p eos-e2e-test --features e2e --test workspace-runtime-command -- --nocapture`
- Result: passed; 67 passed, 0 failed.
- Finding: command lifecycle, stdin/progress/cancel, process-group tracking,
  background finalization, command matrix, and isolated command checks pass live.
- Fix: no code fix needed; do not rerun `workspace-runtime-command`.

### 2026-06-13 Attempt 25 - Focused live workspace-runtime-isolated suite

- Command: `cargo test -p eos-e2e-test --features e2e --test workspace-runtime-isolated -- --nocapture`
- Result: stopped; the test binary entered
  `tests/workspace-runtime-isolated/mod.rs` but produced no Rust test output
  for roughly two minutes.
- Finding: `sample` showed the process parked at `_dyld_start` before Rust test
  discovery, and Docker had multiple stale `eos-e2e-*` containers from prior
  live attempts.
- Fix: stop the stalled suite process, run `e2e-reap`, then retry the isolated
  suite once from a clean container pool.

### 2026-06-13 Attempt 26 - E2E stale-container cleanup before isolated retry

- Command: `cargo run -p eos-e2e-test --bin e2e-reap`
- Result: passed; removed 12 stale `eos-e2e` containers.
- Finding: live suite runs had accumulated stale containers, plausibly
  contributing to startup/resource stalls.
- Fix: cleanup complete; retry `workspace-runtime-isolated` once.

### 2026-06-13 Attempt 27 - Focused live workspace-runtime-isolated retry

- Command: `cargo test -p eos-e2e-test --features e2e --test workspace-runtime-isolated -- --nocapture`
- Result: failed; 15 passed, 6 failed:
  `isolated_workspace_network_isolation::same_mode_same_port_conflicts`,
  `isolated_workspace_tool_routing::isolated_edit_conflict_response_fields`,
  `isolated_workspace_tool_routing::isolated_enter_status_reports_manifest_pin`,
  `isolated_workspace_tool_routing::isolated_read_after_exit_routes_ephemeral`,
  `isolated_workspace_tool_routing::isolated_read_file_sees_private_upperdir`,
  and `isolated_workspace_tool_routing::isolated_write_response_fields`.
- Finding: the first failure expected a second same-mode port bind to fail but
  got an `ok` command result; later failures show isolated manager/root leakage
  with `active callers`, consistent with cascading state after the first failed
  network-isolation case.
- Fix: target `same_mode_same_port_conflicts` first before rerunning any broad
  isolated suite.

### 2026-06-13 Attempt 28 - Targeted isolated same-port retry

- Command: `cargo test -p eos-e2e-test --features e2e --test workspace-runtime-isolated isolated_workspace_network_isolation::same_mode_same_port_conflicts -- --exact --nocapture`
- Result: passed; 1 passed, 0 failed.
- Finding: the same-port conflict contract is healthy in isolation; the suite
  failure is caused by parallel isolated tests sharing daemon/root lifecycle
  state, not by the port-conflict assertion itself.
- Fix: constrain the workspace-runtime-isolated E2E pool to one sandbox so the
  default test harness serializes leases for this suite.

### 2026-06-13 Attempt 29 - E2E stale-container cleanup after isolated config change

- Command: `cargo run -p eos-e2e-test --bin e2e-reap`
- Result: passed; removed 2 stale `eos-e2e` containers.
- Finding: isolated-suite config changed the pool digest; existing containers
  should not be reused for the verification run.
- Fix: cleanup complete; rerun `workspace-runtime-isolated` once with
  `pool.sandboxes: 1`.

### 2026-06-13 Attempt 30 - Focused live workspace-runtime-isolated retry after pool serialization

- Command: `cargo test -p eos-e2e-test --features e2e --test workspace-runtime-isolated -- --nocapture`
- Result: failed; 15 passed, 6 failed:
  `isolated_workspace_tool_routing::isolated_edit_conflict_response_fields`,
  `isolated_workspace_tool_routing::isolated_enter_status_reports_manifest_pin`,
  `isolated_workspace_tool_routing::isolated_read_after_exit_routes_ephemeral`,
  `isolated_workspace_tool_routing::isolated_read_file_sees_private_upperdir`,
  `isolated_workspace_tool_routing::isolated_write_does_not_publish_or_release_lease`,
  and `isolated_workspace_tool_routing::isolated_write_response_fields`.
- Finding: pool serialization removed the same-port race, and the first failure
  is now a Phase 05 migration miss: `isolated_edit_conflict_response_fields`
  inspected top-level fields on an `OperationEnvelope` instead of the nested
  result, then exited early and left an active isolated caller that cascaded
  into later tool-routing failures.
- Fix: unwrap the edit conflict envelope result and ensure the isolated session
  exits before returning assertion failures; rerun only
  `isolated_edit_conflict_response_fields` next.

### 2026-06-13 Attempt 31 - Targeted isolated edit conflict retry

- Command: `cargo test -p eos-e2e-test --features e2e --test workspace-runtime-isolated isolated_workspace_tool_routing::isolated_edit_conflict_response_fields -- --exact --nocapture`
- Result: failed; 0 passed, 1 failed.
- Finding: the target did not reach the edited conflict assertion. It failed at
  `sandbox.isolation.enter` because the recycled live daemon still had active
  isolated callers from the prior failed suite.
- Fix: add the existing best-effort `reset_isolated_workspaces` guard to the
  remaining tool-routing tests before they enter isolation, run `e2e-reap` to
  clear the leaked live container, then retry this exact target.

### 2026-06-13 Attempt 32 - E2E stale-container cleanup before isolated exact retry

- Command: `cargo run -p eos-e2e-test --bin e2e-reap`
- Result: passed; removed 1 stale `eos-e2e` container.
- Finding: the failed isolated target left a live container with active
  isolated callers.
- Fix: cleanup complete; retry
  `isolated_edit_conflict_response_fields` exactly once with the reset guard in
  place.

### 2026-06-13 Attempt 33 - Targeted isolated edit conflict retry after cleanup

- Command: `cargo test -p eos-e2e-test --features e2e --test workspace-runtime-isolated isolated_workspace_tool_routing::isolated_edit_conflict_response_fields -- --exact --nocapture`
- Result: passed; 1 passed, 0 failed.
- Finding: the edit-conflict response now unwraps the `OperationEnvelope`
  result correctly, and the isolated session exits cleanly after assertions.
- Fix: no additional code fix needed for this target; rerun the focused
  `workspace-runtime-isolated` suite once.

### 2026-06-13 Attempt 34 - Focused live workspace-runtime-isolated suite after fixes

- Command: `cargo test -p eos-e2e-test --features e2e --test workspace-runtime-isolated -- --nocapture`
- Result: passed; 21 passed, 0 failed.
- Finding: isolated lifecycle, network isolation, private no-publish,
  cross-mode consistency, daemon restart cleanup, trace chain, and tool-routing
  response-envelope checks pass with serialized leases and per-test isolated
  reset guards.
- Fix: no additional code fix needed; do not rerun
  `workspace-runtime-isolated`.

### 2026-06-13 Attempt 35 - Focused live plugin suite

- Command: `cargo test -p eos-e2e-test --features e2e --test plugin -- --nocapture`
- Result: stopped; the test binary produced no Rust test output for roughly 90
  seconds and was sampled at `_dyld_start`. After termination, buffered output
  showed 6 of 15 plugin tests had passed before the process was stopped.
- Finding: the run is inconclusive because it was killed during live execution,
  not because a plugin assertion failed.
- Fix: reap the live E2E container left by the interrupted run and retry the
  plugin suite once from a clean pool.

### 2026-06-13 Attempt 36 - E2E stale-container cleanup before plugin retry

- Command: `cargo run -p eos-e2e-test --bin e2e-reap`
- Result: passed; removed 2 stale `eos-e2e` containers.
- Finding: the interrupted plugin run left live E2E containers behind.
- Fix: cleanup complete; retry the focused live plugin suite once.

### 2026-06-13 Attempt 37 - Focused live plugin suite retry

- Command: `cargo test -p eos-e2e-test --features e2e --test plugin -- --nocapture`
- Result: passed; 15 passed, 0 failed.
- Finding: plugin package lifecycle, setup/manifest failures, isolated
  rejection, callback OCC tracing, overlay publish, reload races, LSP dispatch,
  and service-health probes pass live after cleanup.
- Fix: no additional code fix needed; do not rerun `plugin`.

### 2026-06-13 Attempt 38 - Focused live pressure suite

- Command: `cargo test -p eos-e2e-test --features e2e --test pressure -- --nocapture`
- Result: passed; 23 passed, 0 failed.
- Finding: pressure ladders, mixed file/OCC/overlay workloads, cancellation and
  recovery, multi-caller conflicts, isolated caps, plugin refresh pressure,
  resource reporting, and soak counters pass live.
- Fix: no additional code fix needed; do not rerun `pressure`.

### 2026-06-13 Attempt 39 - E2E stale-container cleanup before full live gate

- Command: `cargo run -p eos-e2e-test --bin e2e-reap`
- Result: passed; removed 4 stale `eos-e2e` containers.
- Finding: the pressure suite left live containers running after the focused
  pass.
- Fix: cleanup complete; run the serialized full live e2e gate once.

### 2026-06-13 Attempt 40 - Serialized full live e2e gate

- Command: `cargo test -p eos-e2e-test --features e2e -- --test-threads=1 --nocapture`
- Result: stopped; the gate compiled and entered the first unit-test binary
  (`src/lib.rs`) but produced no Rust test output for roughly 100 seconds.
- Finding: `sample` showed the first test binary parked at `_dyld_start`, and
  no `eos-e2e` containers were running, so this was a pre-discovery process
  startup stall rather than a live test assertion failure.
- Fix: stop the hung aggregate run and target the first binary with
  `cargo test -p eos-e2e-test --features e2e --lib -- --test-threads=1 --nocapture`
  before any full-gate retry.

### 2026-06-13 Attempt 41 - Targeted first binary from full gate

- Command: `cargo test -p eos-e2e-test --features e2e --lib -- --test-threads=1 --nocapture`
- Result: passed; 1 passed, 0 failed.
- Finding: the `eos-e2e-test` unit-test binary is healthy when targeted
  directly, confirming Attempt 40 was a transient process launch stall.
- Fix: no code fix needed; retry the serialized full live e2e gate once.

### 2026-06-13 Attempt 42 - Serialized full live e2e gate retry

- Command: `cargo test -p eos-e2e-test --features e2e -- --test-threads=1 --nocapture`
- Result: failed in `ephemeral_workspace`; the unit test binary passed 1/1,
  core passed 32/32, daemon passed 12/12, layerstack passed 20/20, and
  `ephemeral_workspace` failed with 11 passed, 1 failed:
  `test_ephemeral_workspace_overlay_exec::exec_upperdir_is_flat_across_base_sizes`.
- Finding: the failure was `read response: Resource temporarily unavailable
  (os error 35)`, matching the earlier transient EAGAIN path for this
  base-size sweep.
- Fix: target
  `exec_upperdir_is_flat_across_base_sizes` exactly before any broader full-gate
  retry.

### 2026-06-13 Attempt 43 - Targeted ephemeral base-size sweep after full-gate failure

- Command: `cargo test -p eos-e2e-test --features e2e --test ephemeral_workspace test_ephemeral_workspace_overlay_exec::exec_upperdir_is_flat_across_base_sizes -- --exact --nocapture`
- Result: passed; 1 passed, 0 failed.
- Finding: the EAGAIN response-read failure did not reproduce in the exact
  target, matching the earlier targeted pass from Attempt 21.
- Fix: inspect the e2e client response-read path for a nonblocking-read retry
  gap before rerunning broader live gates.

### 2026-06-13 Attempt 44 - E2E stale-container cleanup after ephemeral timeout fix

- Command: `cargo run -p eos-e2e-test --bin e2e-reap`
- Result: passed; removed 4 stale `eos-e2e` containers.
- Finding: the EAGAIN came from the client socket request timeout: the
  ephemeral base-size test permits a 35s command timeout and 30s foreground
  yield, while the suite inherited `timeouts.request_s: 30`.
- Fix: set `eos_e2e_test.timeouts.request_s: 90` in the ephemeral workspace
  suite override and clear old-digest live containers before targeted
  verification.

### 2026-06-13 Attempt 45 - Targeted ephemeral base-size sweep after timeout fix

- Command: `cargo test -p eos-e2e-test --features e2e --test ephemeral_workspace test_ephemeral_workspace_overlay_exec::exec_upperdir_is_flat_across_base_sizes -- --exact --nocapture`
- Result: passed; 1 passed, 0 failed.
- Finding: the base-size sweep passes under the suite's raised socket request
  timeout.
- Fix: no additional code fix needed for this target; clean the target
  container before the next full live gate.

### 2026-06-13 Attempt 46 - E2E stale-container cleanup before full live gate retry

- Command: `cargo run -p eos-e2e-test --bin e2e-reap`
- Result: passed; removed 1 stale `eos-e2e` container.
- Finding: the targeted ephemeral verification left its live container running.
- Fix: cleanup complete; rerun the serialized full live e2e gate once.

### 2026-06-13 Attempt 47 - Serialized full live e2e gate after ephemeral timeout fix

- Command: `cargo test -p eos-e2e-test --features e2e -- --test-threads=1 --nocapture`
- Result: failed in `ephemeral_workspace`; the unit test binary passed 1/1,
  core passed 32/32, daemon passed 12/12, layerstack passed 20/20, and
  `ephemeral_workspace` failed with 11 passed, 1 failed:
  `test_ephemeral_workspace_overlay_exec::exec_run_dir_scratch_stays_bounded`.
- Finding: the failing assertion inspected a `sandbox.command.poll` sidecar
  from `completed_buffer`, which has no `resource.command_exec.run_dir` tree
  resources. The command outlasted the test's short 8s foreground yield, so the
  helper settled through poll instead of returning the original
  `exec_command` sidecar that carries the resource facts.
- Fix: raise this run-dir resource test's foreground yield and command timeout
  to the same longer bounds used by the other heavy trace/resource assertions,
  then target this exact test.

### 2026-06-13 Attempt 48 - Targeted ephemeral run-dir resource retry

- Command: `cargo test -p eos-e2e-test --features e2e --test ephemeral_workspace test_ephemeral_workspace_overlay_exec::exec_run_dir_scratch_stays_bounded -- --exact --nocapture`
- Result: passed; 1 passed, 0 failed.
- Finding: the run-dir resource assertion passes when the test keeps the
  response on the original `exec_command` sidecar.
- Fix: no additional code fix needed for this target; rerun the focused
  `ephemeral_workspace` suite once.

### 2026-06-13 Attempt 49 - Focused live ephemeral_workspace suite after timeout/yield fixes

- Command: `cargo test -p eos-e2e-test --features e2e --test ephemeral_workspace -- --nocapture`
- Result: passed; 12 passed, 0 failed.
- Finding: ephemeral overlay exec, cancellation, stale conflict, trace/resource
  assertions, base-size O(1) sweep, and run-dir scratch checks pass together
  with the raised request timeout and longer resource-test foreground yield.
- Fix: no additional code fix needed; clean the suite container before the next
  full live gate.

### 2026-06-13 Attempt 50 - E2E stale-container cleanup before full live gate retry

- Command: `cargo run -p eos-e2e-test --bin e2e-reap`
- Result: passed; removed 5 stale `eos-e2e` containers.
- Finding: the failed/full and focused ephemeral runs left live containers.
- Fix: cleanup complete; rerun the serialized full live e2e gate once.

### 2026-06-13 Attempt 51 - Serialized full live e2e gate after run-dir yield fix

- Command: `cargo test -p eos-e2e-test --features e2e -- --test-threads=1 --nocapture`
- Result: failed in `ephemeral_workspace`; the unit test binary passed 1/1,
  core passed 32/32, daemon passed 12/12, layerstack passed 20/20, and
  `ephemeral_workspace` failed with 11 passed, 1 failed:
  `test_ephemeral_workspace_overlay_exec::exec_run_dir_scratch_stays_bounded`.
- Finding: even with a 30s foreground yield, the full-gate context still
  returned a `sandbox.command.poll` completed-buffer sidecar for this
  resource assertion, so the test remained detached from the original
  `exec_command` resource sidecar under aggregate load.
- Fix: raise only this run-dir resource test to a 60s foreground yield and 75s
  command timeout, then target this exact test again.

### 2026-06-13 Attempt 52 - Targeted ephemeral run-dir resource retry after 60s yield

- Command: `cargo test -p eos-e2e-test --features e2e --test ephemeral_workspace test_ephemeral_workspace_overlay_exec::exec_run_dir_scratch_stays_bounded -- --exact --nocapture`
- Result: passed; 1 passed, 0 failed.
- Finding: the run-dir resource assertion passes with a 60s foreground window
  and 75s command timeout.
- Fix: no additional code fix needed for this target; rerun the focused
  `ephemeral_workspace` suite once.

### 2026-06-13 Attempt 53 - Focused live ephemeral_workspace suite after 60s run-dir yield

- Command: `cargo test -p eos-e2e-test --features e2e --test ephemeral_workspace -- --nocapture`
- Result: passed; 12 passed, 0 failed.
- Finding: the full ephemeral suite passes with the 90s request timeout and the
  60s run-dir resource foreground window.
- Fix: no additional code fix needed; clean the suite container before the next
  full live gate.

### 2026-06-13 Attempt 54 - E2E stale-container cleanup before full live gate retry

- Command: `cargo run -p eos-e2e-test --bin e2e-reap`
- Result: passed; removed 5 stale `eos-e2e` containers.
- Finding: the failed/full and focused ephemeral runs left live containers.
- Fix: cleanup complete; rerun the serialized full live e2e gate once.

### 2026-06-13 Attempt 55 - Serialized full live e2e gate after 60s run-dir yield

- Command: `cargo test -p eos-e2e-test --features e2e -- --test-threads=1 --nocapture`
- Result: failed in `workspace-runtime-command`; the unit test binary passed
  1/1, core passed 32/32, daemon passed 12/12, layerstack passed 20/20,
  `ephemeral_workspace` passed 12/12, plugin passed 15/15, pressure passed
  23/23, workspace-publish-gate passed 14/14, and
  `workspace-runtime-command` failed with 66 passed, 1 failed:
  `command_isolated_workspace::setsid_descendant_reaped_on_isolated_exit`.
- Finding: the failing command returned `{"status":"error","stderr":"command_not_found"}` where the test expected the isolated escaped-child
  command to complete.
- Fix: target
  `command_isolated_workspace::setsid_descendant_reaped_on_isolated_exit`
  exactly before changing code or rerunning broader command gates.

### 2026-06-13 Attempt 56 - Targeted command isolated setsid retry

- Command: `cargo test -p eos-e2e-test --features e2e --test workspace-runtime-command command_isolated_workspace::setsid_descendant_reaped_on_isolated_exit -- --exact --nocapture`
- Result: stopped; the exact target compiled and launched but produced no Rust
  test output for roughly 90 seconds.
- Finding: `sample` showed the test binary parked at `_dyld_start`, so this
  attempt did not reach the command-isolated test body.
- Fix: stop the pre-discovery process, reap live E2E containers left by the
  full gate, and retry the same exact target once from a clean pool.

### 2026-06-13 Attempt 57 - E2E stale-container cleanup before command exact retry

- Command: `cargo run -p eos-e2e-test --bin e2e-reap`
- Result: passed; removed 4 stale `eos-e2e` containers.
- Finding: the failed full gate and stopped exact target left live command-suite
  containers.
- Fix: cleanup complete; retry the exact command-isolated target.

### 2026-06-13 Attempt 58 - Targeted command isolated setsid retry after cleanup

- Command: `cargo test -p eos-e2e-test --features e2e --test workspace-runtime-command command_isolated_workspace::setsid_descendant_reaped_on_isolated_exit -- --exact --nocapture`
- Result: stopped; the exact target launched but produced no assertion result
  for roughly two minutes.
- Finding: no live E2E container was running during the stall, and `sample`
  again showed the test binary at `_dyld_start`; after termination it emitted
  only `running 1 test`, so the attempt still did not produce test-body
  evidence.
- Fix: inspect the failing test source from Attempt 55 directly before making a
  code change; do not rerun broader command gates until the target behavior is
  understood.

### 2026-06-13 Attempt 59 - Targeted command isolated setsid retry after yield fix

- Command: `cargo test -p eos-e2e-test --features e2e --test workspace-runtime-command command_isolated_workspace::setsid_descendant_reaped_on_isolated_exit -- --exact --nocapture`
- Result: failed; 0 passed, 1 failed.
- Finding: this run reached the test binary but failed at
  `sandbox.isolation.enter` because a previous stopped run left the recycled
  daemon bound to an old isolated workspace root with active callers.
- Fix: reap stale E2E containers and retry the exact command-isolated target
  from a clean daemon.

### 2026-06-13 Attempt 60 - E2E stale-container cleanup before command exact retry

- Command: `cargo run -p eos-e2e-test --bin e2e-reap`
- Result: passed; removed 1 stale `eos-e2e` container.
- Finding: the command-isolated exact run left a stale container with active
  isolated callers.
- Fix: cleanup complete; retry the exact command-isolated target from a clean
  daemon.

### 2026-06-13 Attempt 61 - Targeted command isolated setsid retry from clean daemon

- Command: `cargo test -p eos-e2e-test --features e2e --test workspace-runtime-command command_isolated_workspace::setsid_descendant_reaped_on_isolated_exit -- --exact --nocapture`
- Result: passed; 1 passed, 0 failed.
- Finding: the command-isolated setsid descendant test passes from a clean
  daemon after avoiding the short-yield completed-buffer finalization race.
- Fix: no additional code fix needed for this target; rerun the focused command
  suite after adding the requested per-test Git reset.

### 2026-06-13 Attempt 62 - E2E stale-container cleanup before Git-reset target

- Command: `cargo run -p eos-e2e-test --bin e2e-reap`
- Result: passed; removed 1 stale `eos-e2e` container.
- Finding: the command-isolated target left a warm command-suite container.
- Fix: cleanup complete; run the new exact core lease Git-reset assertion.

### 2026-06-13 Attempt 63 - Targeted core lease Git-reset assertion

- Command: `cargo test -p eos-e2e-test --features e2e --test core test_core_runtime_readiness_and_base::lease_checkout_resets_stale_git_workspace_state -- --exact --nocapture`
- Result: passed; 1 passed, 0 failed.
- Finding: a stale `.git` marker created in one lease is absent in the next
  lease, proving checkout setup removes Git state before each test workspace is
  bound.
- Fix: no additional code fix needed; regenerate E2E inventory docs because the
  core suite now has one additional test.

### 2026-06-13 Attempt 64 - E2E stale-container cleanup before command suite retry

- Command: `cargo run -p eos-e2e-test --bin e2e-reap`
- Result: passed; removed 1 stale `eos-e2e` container.
- Finding: the targeted core Git-reset assertion left a live E2E container.
- Fix: cleanup complete; rerun the focused live `workspace-runtime-command`
  suite once.

### 2026-06-13 Attempt 65 - Focused live workspace-runtime-command suite after fixes

- Command: `cargo test -p eos-e2e-test --features e2e --test workspace-runtime-command -- --nocapture`
- Result: passed; 67 passed, 0 failed.
- Finding: command lifecycle, command matrix, stdin/backpressure, cancellation,
  external process death, ephemeral command behavior, isolated command behavior,
  and protocol smoke checks pass with the isolated setsid yield fix.
- Fix: no additional code fix needed; clean the suite containers before the
  next serialized full live gate.

### 2026-06-13 Attempt 66 - E2E stale-container cleanup before full live gate retry

- Command: `cargo run -p eos-e2e-test --bin e2e-reap`
- Result: passed; removed 4 stale `eos-e2e` containers.
- Finding: the focused command suite left live E2E containers.
- Fix: cleanup complete; rerun the serialized full live e2e gate once.

### 2026-06-13 Attempt 67 - Serialized full live e2e gate after command suite fix

- Command: `cargo test -p eos-e2e-test --features e2e -- --test-threads=1 --nocapture`
- Result: stopped; the run initially appeared stalled in the first unit-test
  binary and was sampled at `_dyld_start`, then produced buffered output after
  termination: the unit test binary passed 1/1 and the core suite had started
  with 33 tests before the process was stopped.
- Finding: this attempt is inconclusive because it was interrupted by the
  diagnostic stop, not by a test assertion failure.
- Fix: clean any partial-run containers and run the focused core suite once,
  since core now includes the new Git-reset lease test.

### 2026-06-13 Attempt 68 - E2E stale-container cleanup before focused core suite

- Command: `cargo run -p eos-e2e-test --bin e2e-reap`
- Result: passed; removed 1 stale `eos-e2e` container.
- Finding: the interrupted aggregate run left a partial core-suite container.
- Fix: cleanup complete; run the focused live core suite once.

### 2026-06-13 Attempt 69 - Focused live core suite after Git-reset test

- Command: `cargo test -p eos-e2e-test --features e2e --test core -- --nocapture`
- Result: passed; 33 passed, 0 failed.
- Finding: the core direct-file, protocol, readiness/base, and wire-message
  guards pass with the new lease Git-reset assertion included.
- Fix: no additional code fix needed; clean the suite containers before the
  next serialized full live gate.

### 2026-06-13 Attempt 70 - E2E stale-container cleanup before full live gate retry

- Command: `cargo run -p eos-e2e-test --bin e2e-reap`
- Result: passed; removed 2 stale `eos-e2e` containers.
- Finding: the focused core suite left live E2E containers.
- Fix: cleanup complete; rerun the serialized full live e2e gate once.

### 2026-06-13 Attempt 71 - Serialized full live e2e gate after focused core suite

- Command: `cargo test -p eos-e2e-test --features e2e -- --test-threads=1 --nocapture`
- Result: stopped; the unit test binary passed 1/1, core passed 33/33,
  daemon passed 12/12, layerstack passed 20/20, ephemeral_workspace passed
  12/12, then the plugin binary produced no Rust test output.
- Finding: `sample` showed the plugin binary parked at `_dyld_start`, so this
  was a plugin pre-discovery startup stall, not a plugin assertion failure.
- Fix: stop the aggregate run, clean the live containers it left, and target
  the focused plugin suite once before another full-gate attempt.

### 2026-06-13 Attempt 72 - E2E stale-container cleanup after plugin startup stall

- Command: `cargo run -p eos-e2e-test --bin e2e-reap`, then
  `./target/debug/e2e-reap`, then direct
  `docker rm -f eos-e2e-63323-18b884496aecbd10-1 eos-e2e-60527-18b8843e47e6b948-1 eos-e2e-59379-18b884144bf13330-1 eos-e2e-58570-18b8840ebefa52e0-1`
- Result: passed via direct Docker cleanup; removed 4 stale `eos-e2e`
  containers.
- Finding: both reaper entrypoints stalled during this cleanup, while the four
  aggregate-run containers remained visible and removable by exact name.
- Fix: cleanup complete; run the focused live plugin suite once.

### 2026-06-13 Attempt 73 - Focused live plugin suite after aggregate startup stall

- Command: `cargo test -p eos-e2e-test --features e2e --test plugin -- --nocapture`
- Result: passed; 15 passed, 0 failed.
- Finding: plugin lifecycle, overlay publish, callback tracing, reload races,
  isolated rejection, LSP dispatch, and service health checks pass live; the
  aggregate failure was a startup stall, not a plugin assertion failure.
- Fix: no additional code fix needed; clean plugin containers before the next
  serialized full live gate.

### 2026-06-13 Attempt 74 - Serialized full live e2e gate after focused plugin suite

- Command: `cargo test -p eos-e2e-test --features e2e -- --test-threads=1 --nocapture`
- Result: stopped; the unit test binary passed 1/1, core passed 33/33,
  daemon passed 12/12, layerstack passed 20/20, ephemeral_workspace passed
  12/12, plugin passed 15/15, then the pressure binary produced no Rust test
  output.
- Finding: `sample` showed the pressure binary parked at `_dyld_start` and no
  live E2E containers were running, so this was a pre-discovery pressure startup
  stall, not a pressure assertion failure.
- Fix: stop the aggregate run and target the focused pressure suite once before
  another full-gate attempt.

### 2026-06-13 Attempt 75 - Focused live pressure suite after aggregate startup stall

- Command: `cargo test -p eos-e2e-test --features e2e --test pressure -- --nocapture`
- Result: passed; 23 passed, 0 failed.
- Finding: the pressure binary also sat at `_dyld_start` initially, but it moved
  past startup after a longer wait and all pressure tests passed.
- Fix: no code fix needed; allow longer startup patience for pressure in the
  next serialized full gate.

### 2026-06-13 Attempt 76 - E2E stale-container cleanup before full live gate retry

- Command: `docker rm -f eos-e2e-86206-18b88510516f8c38-1 eos-e2e-86206-18b88510516fd288-7 eos-e2e-86206-18b88510516f97f0-3 eos-e2e-86206-18b88510516fa3a8-4`
- Result: passed; removed 4 stale `eos-e2e` containers.
- Finding: the focused pressure suite left live E2E containers.
- Fix: cleanup complete; rerun the serialized full live e2e gate once.

### 2026-06-13 Attempt 77 - Serialized full live e2e gate final pass

- Command: `cargo test -p eos-e2e-test --features e2e -- --test-threads=1 --nocapture`
- Result: passed; unit test binary 1/1, core 33/33, daemon 12/12,
  eos-layerstack 20/20, ephemeral_workspace 12/12, plugin 15/15, pressure
  23/23, workspace-publish-gate 14/14, workspace-runtime-command 67/67,
  workspace-runtime-isolated 21/21, doc-tests 0/0.
- Finding: the full live E2E gate is green with response-envelope migration,
  trace/resource assertions, per-test Git workspace reset, serialized isolated
  leases, and longer foreground windows for resource-sidecar assertions.
- Fix: no additional E2E code fix needed for Phase 05; proceed to non-live
  checks and spec tracker update.

### 2026-06-13 Attempt 78 - E2E stale-container cleanup after full live gate pass

- Command: `docker rm -f eos-e2e-11588-18b885893b9e5c58-1 eos-e2e-1364-18b8857594cf6da8-98f eos-e2e-1364-18b88563694d4ba8-1 eos-e2e-97438-18b88561c6d7a3b0-1 eos-e2e-96464-18b88537a53d4e28-1`
- Result: passed; removed 5 stale `eos-e2e` containers.
- Finding: the successful full live gate left warm containers running.
- Fix: cleanup complete; continue Phase 05 non-live verification.
