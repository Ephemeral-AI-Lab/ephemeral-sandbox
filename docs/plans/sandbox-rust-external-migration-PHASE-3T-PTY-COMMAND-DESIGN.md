# Sandbox Rust Migration - Phase 3T PTY Command Design Addendum

**Status:** Implemented for the Docker live path. Unit/static gates, Linux-target compile, full tiered Docker live E2E tiers 0-6, and Docker Phase 3T PTY/load/p95 gates passed. Daytona remains out of scope for this run.
**Date:** 2026-06-01.
**Companion plan:** `docs/plans/sandbox-rust-external-migration-PHASE-3T-SHELL-SESSIONS.md`.
**Parent plan:** `docs/plans/sandbox-rust-external-migration-PLAN.md`.

This addendum records the final command/session contract for Phase 3T. It
refines the shell-session plan with the explicit PTY tool surface, native Rust
PTY lifecycle, daemon-owned transcript storage, Python background-manager
notifications, and the test/load/performance gates required before Phase 3T can
close.

Where this addendum conflicts with the earlier `shell_session_id`,
`check_shell_progress`, or generic background-shell wording in the companion
plan, use the names and semantics in this document.

## Progress Update - 2026-06-01 14:15 CST

Current implementation status:

- model-facing Python tools and sandbox API wrappers exist for
  `exec_command`, `write_pty_command_stdin`, `check_pty_command_progress`, and
  `cancel_pty_command`;
- Rust protocol models and daemon op registrations exist for
  `api.v1.exec_command`, PTY controls, and the internal completion collector;
- finite `exec_command(tty=false)` routes through the shared shell/overlay/OCC
  path and rejects raw argv at the new public contract boundary;
- Rust daemon workspace-base control ops now cover `api.ensure_workspace_base`,
  `api.build_workspace_base`, and `api.workspace_binding`;
- Rust/Python layer-stack whiteout handling uses kernel overlay whiteouts on the
  existing Docker scratch tmpfs and keeps the xattr/logical fallbacks for
  incompatible filesystems;
- native Linux PTY code compiles for `x86_64-unknown-linux-musl`;
- Rust/Python unit and focused static gates passed locally;
- Docker-only Phase 3T PTY command correctness, load, and p95 gates passed
  through `backend/scripts/bench_rust_daemon_phase3t_pty.py`;
- full tiered Docker live E2E tiers 0-6 passed under `EOS_SANDBOX_RUNTIME=rust`;
- PTY natural exit, timeout, progress polling, stdin write, cancel, and
  `nohup ... 2>&1 &` descendant cleanup are covered by the Docker PTY report for
  both `tty=false` and `tty=true`;
- Daytona live execution is explicitly out of scope for this run.

Fresh progress is tracked in
`docs/plans/sandbox-rust-external-migration-PHASE-3T-PTY-COMMAND-DESIGN-iteration-report.md`.

Latest evidence:

- Linux target compile:
  `cargo check -p eos-daemon --target x86_64-unknown-linux-musl`.
- Docker artifact upload gate:
  `bench/local-eosd-amd64-phase3t-pty-cleanup-conditional-20260601.json`.
- Strict Docker PTY/load/p95 gate with explicit `tty=true`/`tty=false` nohup
  cleanup:
  `bench/phase3t-pty-command-docker-20260601-pty-cleanup-conditional.json`.
- Full tiered Docker E2E summary:
  `.omc/results/progressive-test-summary-phase3t-rust-scratch-full-final-20260601.jsonl`.

## 1. Final Tool Contract

Model-facing tools:

```text
exec_command(cmd, tty, yield_time_ms?, timeout?)
write_pty_command_stdin(pty_session_id, chars, yield_time_ms?, max_tokens?)
check_pty_command_progress(pty_session_id, time, max_tokens)
cancel_pty_command(pty_session_id)
```

`exec_command` always executes a shell-format command string as:

```text
/bin/bash --noprofile --norc -c <cmd>
```

There is no login shell, no `/bin/sh` fallback, no host-shell fallback, and no
model-facing raw-argv contract. If `/bin/bash` is unavailable, command startup
fails as a sandbox readiness/setup error.

Command startup must provide the sandbox command environment explicitly.
Correctness must not depend on `BASH_ENV`, `ENV`, `/etc/profile`,
`.bash_profile`, or `.bashrc`. In the Dask image, `python` must resolve through
the command environment `PATH` to `/opt/miniconda3/envs/testbed/bin/python`, or
by an absolute executable path.

All command tools return this public shape:

```json
{
  "status": "running | ok | error | timed_out | cancelled",
  "exit_code": null,
  "output": {
    "stdout": "",
    "stderr": ""
  },
  "pty_session_id": "pty_1"
}
```

`pty_session_id` is present only when `exec_command(..., tty=true, ...)`
returns a still-running PTY session. `exit_code` is `null` while running.

### `tty=false`

`tty=false` is the finite command path.

1. Spawn non-login Bash without a PTY.
2. Close stdin. Phase 3T does not support stdin for finite commands.
3. Capture real stdout and stderr as separate pipes.
4. Wait until the top-level Bash process exits or timeout fires.
5. When Bash exits, kill any remaining descendants in the process group/cgroup.
6. Drain stdout/stderr produced before cleanup.
7. Capture and publish/discard workspace changes.
8. Release all workspace and process resources.
9. Return full `output.stdout` and `output.stderr`; never return
   `pty_session_id`.

Detached shell patterns do not escape the finite command boundary. For example,
`nohup python train.py 2>&1 &` under `tty=false` exits Bash quickly; then the
remaining Python descendant is terminated before the command result is returned.

### `tty=true`

`tty=true` is the interactive and long-running session path.

1. Allocate a native daemon-owned PTY.
2. Spawn non-login Bash with stdin/stdout/stderr connected to the PTY slave.
3. Read the PTY master into daemon-owned transcript storage.
4. If the process exits before `yield_time_ms`, finalize inline and return no
   `pty_session_id`.
5. If it is still running after `yield_time_ms`, return `status=running`,
   recent terminal transcript in `output.stdout`, `output.stderr=""`, and a
   `pty_session_id`.
6. Keep PTY fd, process group/cgroup, transcript storage, and workspace
   resources alive until natural exit, timeout, cancellation, external kill, or
   daemon shutdown finalization.

PTY stdout and stderr are naturally merged by the terminal. For PTY commands,
the public result always puts the visible terminal transcript in
`output.stdout` and leaves `output.stderr=""`.

`write_pty_command_stdin` writes `chars` literally to the PTY. It does not
report submitted input as output. It returns only terminal output observed
during the post-write `yield_time_ms` window.

`check_pty_command_progress` returns terminal lines observed during the last
`time` seconds, bounded by `max_tokens`. It is a time-window view, not a
cursor, so repeated checks may repeat output.

`check_pty_command_progress`, `write_pty_command_stdin`, and
`cancel_pty_command` are active-session-only controls. Finished, cancelled,
expired, wrong-agent, and never-created PTY ids all return the same generic
not-found error:

```json
{
  "status": "error",
  "exit_code": null,
  "output": {
    "stdout": "",
    "stderr": "pty_session_not_found"
  }
}
```

These tools must not reveal whether the session previously existed.

## 2. Native PTY Requirement

Production implementation must not use `script(1)`.

`script(1)` is allowed only as temporary benchmark/prototype scaffolding. The
real Rust daemon path allocates and owns the PTY directly, using either
`nix::pty::openpty`, `portable-pty`, or small Linux wrappers around
`posix_openpt` / `grantpt` / `unlockpt` / `ptsname` when wrapper coverage is
insufficient.

Production PTY requirements:

- daemon owns the PTY master fd;
- child Bash owns the PTY slave as stdin/stdout/stderr;
- the daemon can write bytes, read bytes, and close/kill/finalize the full
  process tree without a utility wrapper;
- process-group/cgroup ownership is direct and auditable;
- output behavior is not modified by `script(1)` banners, typescript behavior,
  or wrapper-specific line discipline.

The current overlay-inclusive PTY proxy evidence remains useful as a
conservative performance baseline:

| Case | Evidence | p50 | p95 |
| --- | --- | ---: | ---: |
| non-PTY non-login Bash `true` | `bench/phase3-overlay-bash-lc-rerun-20260601.json` | 42.7 ms | 43.1 ms |
| PTY proxy non-login Bash `true` | `bench/phase3-overlay-pty-bash-rerun-20260601.json` | 79.0 ms | 82.4 ms |

Native Rust PTY should match or beat the proxy result. The hard Phase 3T
acceptance gate is p95 <= 100 ms for `tty=true` non-login Bash `true` through
the real overlay path.

## 3. Daemon-Owned PTY Lifecycle

The Rust sandbox daemon owns the actual PTY session. Python does not hold the
transcript and does not own process resources.

```text
Requested
  -> FinishedInline     exits within exec_command yield window
  -> Running            still active after exec_command yield window

Running
  -> Running            write_pty_command_stdin
  -> Running            check_pty_command_progress
  -> Finalizing         natural process exit
  -> Finalizing         timeout
  -> Finalizing         cancel_pty_command
  -> Finalizing         external process kill
  -> Finalizing         daemon shutdown / startup orphan reap

Finalizing
  -> Removed            drain, kill descendants, publish/discard, release
```

The active PTY registry is the only control surface. Once finalization starts,
the session is no longer controllable. After registry removal,
`check_pty_command_progress`, `write_pty_command_stdin`, and
`cancel_pty_command` all return `pty_session_not_found`.

### Load-Bearing Session State

For a returned `pty_session_id`, the daemon must have all load-bearing state:

```text
PtySession {
  pty_session_id
  agent_id
  invocation_id
  command
  workspace_mode
  pty_master_fd
  leader_pid
  process_group_id
  cgroup_path
  output_ring
  transcript_spool_dir
  workspace_lease_or_isolated_ref
  timeout_deadline
  finalizing
  finalized
}
```

If any load-bearing component cannot be created, `exec_command(tty=true)` must
fail before returning a session id.

### Transcript Storage

Active PTY output is daemon-owned and stored in two layers:

```text
runtime/pty-sessions/<pty_session_id>/
  metadata.json
  transcript.log
  final.json
  lock
```

- `output_ring`: bounded in-memory timestamped line/chunk ring used by
  `exec_command`, `write_pty_command_stdin`, and
  `check_pty_command_progress`.
- `transcript.log`: append-only spool for active-session durability and
  backpressure. It prevents unbounded memory growth and gives finalization
  enough tail material even when the ring wrapped.
- `final.json`: terminal completion payload written after finalization.

Suggested caps:

```text
PTY_RING_MAX_BYTES       1-4 MiB per active PTY
PTY_SPOOL_MAX_BYTES      32-128 MiB per active PTY
PTY_COMPLETION_TTL_S     300 seconds after terminal completion
```

If spool output exceeds the cap, use a deterministic tail/drop policy and set
`spool_truncated=true` in metadata and final completion. Never allow PTY output
to exhaust daemon memory or disk.

### Finalization

Finalization is idempotent and guarded by a per-session latch.

Natural exit, timeout, explicit cancel, external kill detection, daemon
shutdown, and startup orphan recovery all converge on the same finalizer:

1. stop accepting writes/checks for the session;
2. drain remaining PTY bytes;
3. kill remaining descendants in the process group/cgroup;
4. close PTY fds;
5. capture shared overlay upperdir or record isolated changed paths;
6. publish/discard through OCC for shared workspace, or keep private scratch
   for isolated workspace;
7. release the LayerStack lease or isolated active-session reference;
8. write `final.json`;
9. remove from active registry;
10. expose completion to Python through the completion mailbox;
11. delete the session dir after Python collection or TTL expiry.

Startup orphan recovery scans `runtime/pty-sessions/*`. For metadata marked
running, it kills surviving pgid/cgroup resources when possible, releases stale
workspace references when recoverable, writes an `orphan_reaped` final record,
and deletes the session directory after the orphan completion TTL.

## 4. Workspace Semantics

### Shared Ephemeral Workspace

`tty=false` and `tty=true` both run through the shared overlay pipeline at
`exec_command` startup:

```text
LayerStack snapshot lease
  -> overlay upper/work allocation
  -> overlay mount at workspace root
  -> command execution
  -> upperdir capture
  -> OCC publish/discard
  -> lease release and runtime cleanup
```

For `tty=true`, the lease, overlay dirs, PTY, process group/cgroup, output
ring, and transcript spool remain alive until terminal finalization.
`write_pty_command_stdin`, `check_pty_command_progress`, and
`cancel_pty_command` do not mount a new overlay; they operate on the already
active PTY session and its retained workspace resources.

The command process sees the normal sandbox/container filesystem, with the
target workspace root replaced by the mounted overlay. That overlay is composed
from leased LayerStack snapshot layers as lowerdirs plus one per-command or
per-session `upperdir` and `workdir`. Files outside the target workspace remain
ordinary container files and are not captured by workspace OCC.

The target workspace root is the command workspace. `exec_command` does not
expose `workdir`, and every new `exec_command` starts with cwd at the command
workspace root so relative paths initially resolve inside that workspace.
Absolute cwd/path requests accepted by the runner must remain under the
declared workspace root; outside-workspace absolute cwd requests are rejected.
If a shell command later runs `cd /tmp` or writes outside the workspace, those
effects are local to that command process and are not captured as workspace OCC
changes. A `cd` in one `exec_command` is not inherited by the next
`exec_command`. Within one still-running PTY shell, normal shell cwd semantics
apply until that PTY session exits.

If a long-running PTY publishes after shared workspace state moved, OCC must
report the conflict in terminal metadata/notification. It must not overwrite
shared workspace state.

### Isolated Workspace

When an agent has entered isolated workspace mode, `exec_command` routes through
the active isolated handle for that `agent_id`.

In isolated mode:

- the isolated handle already has a persistent overlay mounted at
  `enter_isolated_workspace`; all sandbox tools for that agent run inside that
  handle until exit;
- do not allocate a publishable per-command OCC overlay;
- do not publish command writes to the shared workspace;
- keep writes inside the isolated private upperdir;
- keep the isolated handle alive while any PTY session is active;
- reject `exit_isolated_workspace` while PTY sessions are active unless an
  explicit force-cancel option is implemented;
- on normal PTY exit, keep changes in isolated scratch until isolated exit;
- on isolated exit, discard scratch state and release the pinned snapshot
  lease.

`check_pty_command_progress`, `write_pty_command_stdin`, and
`cancel_pty_command` do not expose whether a session was shared or isolated.
They only operate on active PTY ids owned by the calling agent.

## 5. Python Background Manager Integration

Do not create a new `live_work` module. Reuse the existing Python background
manager/handler modules.

Primary files:

```text
backend/src/engine/background/task_supervisor.py
backend/src/engine/background/dispatch.py
backend/src/engine/background/history.py
backend/src/tools/background/_lib/task_output.py
```

Optional splits are acceptable only if `task_supervisor.py` becomes too large:

```text
backend/src/engine/background/pty_records.py
backend/src/engine/background/notifications.py
```

The existing manager tracks typed records:

```text
task_type = "pty_command" | "subagent" | existing legacy type
```

PTY background record fields:

```text
PtyCommandRecord {
  pty_session_id
  sandbox_id
  sandbox_invocation_id
  agent_id
  command
  status
  exit_code
  terminal_reported_by_tool
  cancelled_by_cancel_tool
  started_at
  last_seen_at
}
```

Python stores lightweight supervision state only. It does not store the active
PTY transcript. The transcript stays in the Rust daemon.

Critical manager functions:

```text
register_pty_command(...)
collect_pty_completions(...)
mark_pty_reported_by_tool(...)
cancel_pty_by_agent(...)
count_by_agent(...)
terminate_for_parent_exit(...)
```

`count_by_agent` must include active sandbox-bound PTY sessions so terminal
tools and isolated workspace lifecycle gates cannot proceed while PTY work is
still running.

### Notification Flow

PTY natural exit:

1. daemon finalizer writes completion into a daemon completion mailbox;
2. Python background heartbeat/collector polls
   `api.v1.pty.collect_completed` for PTY ids it tracks;
3. when completion arrives, the manager marks the PTY record terminal;
4. it fires one typed notification unless the terminal state was already
   reported by `cancel_pty_command` or by a tool call that observed the exit;
5. after notification/result delivery, Python marks the record delivered and
   evicts it after provider-history TTL.

Subagent natural exit:

1. `run_subagent` remains Python-owned background work;
2. the existing asyncio done callback observes the `ToolResult`;
3. metadata `subagent_terminal_called=true` means completed;
4. no terminal tool means failed;
5. the manager queues a typed subagent notification.

Subagent termination by non-cancellation parent exit:

1. parent terminal/non-continuing flow calls
   `terminate_for_parent_exit(reason="non_cancellation_tool_request")`;
2. manager early-stops/cancels running subagents;
3. manager records a terminal status distinct from explicit `cancel_subagent`;
4. manager emits a notification/audit event for the termination.

Explicit `cancel_pty_command` and `cancel_subagent` are user/model-requested
cancellations and should not produce duplicate surprise notifications.

## 6. Implementation File Plan

Python tool/API layer:

```text
backend/src/tools/sandbox/exec_command/
backend/src/tools/sandbox/write_pty_command_stdin/
backend/src/tools/sandbox/check_pty_command_progress/
backend/src/tools/sandbox/cancel_pty_command/
backend/src/sandbox/api/tool/command.py
backend/src/sandbox/api/__init__.py
backend/src/sandbox/shared/models.py
```

Key Python models/functions:

```text
ExecCommandRequest
ExecCommandResult
PtyWriteRequest
PtyProgressRequest
PtyCancelRequest
exec_command(...)
write_pty_command_stdin(...)
check_pty_command_progress(...)
cancel_pty_command(...)
collect_pty_completions(...)
```

Rust protocol:

```text
sandbox/crates/eos-protocol/src/command.rs
sandbox/crates/eos-protocol/src/models.rs
sandbox/crates/eos-protocol/src/lib.rs
```

Rust daemon:

```text
sandbox/crates/eos-daemon/src/command/mod.rs
sandbox/crates/eos-daemon/src/command/finite.rs
sandbox/crates/eos-daemon/src/command/pty.rs
sandbox/crates/eos-daemon/src/command/registry.rs
sandbox/crates/eos-daemon/src/command/output.rs
sandbox/crates/eos-daemon/src/command/workspace.rs
sandbox/crates/eos-daemon/src/command/gc.rs
sandbox/crates/eos-daemon/src/dispatcher.rs
sandbox/crates/eos-daemon/src/server.rs
sandbox/crates/eos-daemon/src/in_flight.rs
```

Rust runner:

```text
sandbox/crates/eos-runner/src/shell.rs
sandbox/crates/eos-runner/src/pty.rs
sandbox/crates/eos-runner/src/fresh_ns.rs
sandbox/crates/eos-runner/src/setns.rs
sandbox/crates/eos-runner/src/request.rs
```

Daemon ops:

```text
api.v1.exec_command
api.v1.pty.write_stdin
api.v1.pty.progress
api.v1.pty.cancel
api.v1.pty.collect_completed
```

`api.v1.pty.collect_completed` is internal to the Python background manager. It
is not a model-facing tool and is not a control surface.

## 7. Verification Strategy

Phase 3T must be proven in layers: unit tests for semantics, daemon integration
tests for process/resource ownership, mock-loop tests for model-facing manager
behavior, live E2E tests for real sandbox behavior, and load/performance tests
for the gates.

Every accepted shell/session sample must run through the real LayerStack lease,
overlay mount, command execution, capture, OCC publish/discard, cleanup, and
lease release path unless the test is explicitly isolated-workspace-only.

### Unit Tests

Rust protocol/model tests:

- serialize/deserialize `ExecCommandArgs`, `ExecCommandResult`, PTY request
  models, and `CommandOutput`;
- reject empty `cmd`, empty/invalid `pty_session_id`, invalid timeout/yield
  fields, and invalid `max_tokens`;
- preserve `output.stdout` / `output.stderr` field names exactly.

Rust daemon/runner tests:

- `tty=false` uses non-login Bash command vector exactly:
  `/bin/bash --noprofile --norc -c <cmd>`;
- `tty=false` captures stdout and stderr separately;
- `tty=false` closes stdin and does not expose stdin in the result;
- `tty=false` kills detached descendants after Bash exits;
- `tty=false` timeout kills process group/cgroup;
- `tty=true` uses native PTY allocation, not `script(1)`;
- `tty=true` short command exits inline with no `pty_session_id`;
- `tty=true` long command returns `pty_session_id`;
- PTY output is stored in `output.stdout` and `output.stderr=""`;
- write/progress/cancel return `pty_session_not_found` for finished and
  unknown ids without distinguishing them;
- finalizer is idempotent under natural-exit/cancel races;
- output ring cap and spool cap set truncation metadata without unbounded
  memory/disk growth;
- startup orphan recovery reaps running metadata dirs and deletes completed
  dirs after TTL.

Python tests:

- `exec_command(tty=true)` registers `task_type="pty_command"` only when a
  session id is returned;
- `exec_command(tty=false)` never registers a background-manager record;
- background manager collects PTY completions through the internal API and
  emits one notification;
- explicit `cancel_pty_command` marks cancellation and suppresses duplicate
  spontaneous-exit notification;
- `count_by_agent` includes active PTY records;
- terminal tool prehooks reject while PTY records are active;
- isolated enter/exit gates see active PTY records the same way they see active
  sandbox-bound background work;
- subagent completed/no-terminal/non-cancellation-parent-exit notifications are
  distinct.

### Daemon Integration Tests

Add Rust daemon tests under `sandbox/crates/eos-daemon/tests/` for:

- finite command success, failure, timeout, stdout/stderr split;
- finite `nohup ... &` descendant cleanup;
- PTY start/write/progress/cancel;
- PTY natural exit completion mailbox;
- PTY external kill detection;
- PTY not-found behavior after completion;
- session directory creation and deletion;
- daemon startup orphan recovery;
- process group/cgroup absence after finalization;
- active registry removal only after workspace cleanup.

These tests should inspect process state where possible, not only returned JSON.

### Mock Agent / Model-Facing Tests

Use the existing `ScenarioLoopRunner` path under
`backend/src/task_center_runner/tests/mock`.

Coverage:

- agent starts a long PTY, receives `pty_session_id`, checks progress, writes
  input, then cancels;
- agent starts a PTY that exits naturally after the tool turn, and receives a
  background-manager notification;
- agent tries to use `check_pty_command_progress` after natural exit and gets
  `pty_session_not_found`;
- agent tries terminal submission while PTY is active and is blocked;
- subagent completes naturally and parent receives typed subagent notification;
- subagent exits without terminal and parent sees failed subagent result;
- parent terminal submission terminates active subagent as
  `non_cancellation_tool_request`.

These tests prove tool schema, provider-history compaction, notifications,
terminal gating, and background-manager behavior without relying on provider
variability.

## 8. Live E2E Coverage

Live E2E coverage must prove the real sandbox semantics in Docker/Daytona-backed
runtime, not only Rust unit behavior.

### Shared Ephemeral Workspace

Required live scenarios:

1. `exec_command(tty=false, cmd="echo out; echo err >&2")` returns separated
   `output.stdout` and `output.stderr`.
2. `exec_command(tty=false, cmd="python - <<'PY' ...")` sees explicit command
   environment where `python` resolves to testbed Python without login Bash.
3. `exec_command(tty=false, cmd="nohup sh -c 'sleep 60' >/tmp/eos-nohup 2>&1 &")`
   returns after Bash exit and leaves no descendant.
4. `exec_command(tty=false)` writes a file; subsequent `read_file` sees it
   after OCC publish.
5. two concurrent finite commands write disjoint files; both publish and final
   LayerStack manifest sees both files.
6. two concurrent finite commands write the same file from stale bases; OCC
   reports conflict/atomic drop as expected and does not silently clobber.
7. PTY command writes a file and exits after the initial yield; publish happens
   on finalization and later `read_file` sees it.
8. PTY command keeps an overlay lease while running; peer shared writes advance
   main workspace; PTY final publish reports OCC conflict rather than
   overwriting peer state.
9. PTY cancel kills full process group and releases overlay lease/run dirs.
10. daemon restart during active PTY reaps orphan resources and does not leave
    mounted overlays, cgroups, session dirs, or leaked leases.

### Isolated Workspace

Required live scenarios:

1. enter isolated workspace, run `exec_command(tty=false)` that writes a file,
   verify shared `read_file` outside that agent cannot see it;
2. isolated finite command exits normally and keeps changes in isolated scratch
   until isolated exit;
3. isolated PTY starts and remains active; `exit_isolated_workspace` rejects
   without explicit force-cancel;
4. isolated PTY is cancelled, then `exit_isolated_workspace` succeeds and
   discards scratch;
5. isolated PTY natural exit leaves private changes visible to later isolated
   commands for the same agent, but never publishes to shared OCC;
6. peer shared workspace publish during isolated PTY does not change the pinned
   isolated lowerdir view;
7. plugin/LSP operations remain blocked while isolated mode is active, even
   with PTY sessions present;
8. isolated startup GC and TTL eviction clean abandoned PTY/handle state.

### Notification and Subagent Live Scenarios

Required live scenarios:

1. long PTY exits naturally after the parent has moved on; Python background
   manager fires one typed notification;
2. explicit `cancel_pty_command` reports cancellation through the tool response
   and does not produce a duplicate natural-exit notification;
3. subagent returns terminal output and parent receives typed subagent
   completion;
4. subagent exits without terminal and parent receives failed subagent outcome;
5. parent terminal submission while subagent is active terminates the subagent
   as `non_cancellation_tool_request` and records that reason.

### Live Artifacts To Inspect

Each live E2E run should preserve:

```text
.sweevo_runs/scenario_logs/.../run.json
.sweevo_runs/scenario_logs/.../message.jsonl
.sweevo_runs/scenario_logs/.../sandbox_events.jsonl
.sweevo_runs/scenario_logs/.../performance_report.json
.sweevo_runs/scenario_logs/.../performance_report.md
```

For direct isolated workspace pytest tiers, also inspect the daemon-local
isolated audit file:

```text
/tmp/sandbox_isolated_workspace_events.jsonl
```

or the `EOS_ISOLATED_WORKSPACE_AUDIT_PATH` override.

Acceptance requires evidence of:

- no daemon audit drops: `dropped_event_count == 0`, `lost_before_seq == 0`;
- PTY lifecycle events for start, write/progress when emitted, completion,
  cancel, timeout, orphan reap, and GC;
- overlay resource timings: queued, mount, exec/session, capture, OCC publish,
  release;
- changed-path and conflict metadata;
- session runtime dir cleanup;
- process/cgroup cleanup;
- LayerStack lease count returning to baseline after finalization.

## 9. Load and Contention Testing

Load tests must cover 1, 3, 5, and 10 concurrent operations, matching the
Phase 3T closeout requirement. Where practical, add a 25-session stress tier as
non-blocking soak evidence.

### Command Load Matrix

Run these mixes at 1/3/5/10 concurrency:

- `tty=false` no-op: `true`;
- `tty=false` read-heavy: `ls >/dev/null`, `cat README.rst >/dev/null`;
- `tty=false` write-heavy: unique `mkdir`/`touch` per command;
- `tty=false` conflict-heavy: multiple commands write the same path;
- `tty=true` start/exit: `true` through native PTY;
- `tty=true` long session: `sleep`, periodic output, then natural exit;
- `tty=true` input-heavy: command waits for input lines and echoes results;
- mixed shared load: read_file, write_file, edit_file, finite exec, PTY exec,
  glob, grep, LayerStack squash/GC;
- mixed isolated load: multiple agents with isolated finite and PTY commands.

### OCC / LayerStack Consistency

Load tests must prove:

- disjoint file writes batch/publish correctly;
- overlapping writes produce OCC conflicts and never clobber silently;
- PTY sessions hold snapshot leases while active;
- final lease release allows LayerStack GC to reclaim unleased layers;
- manifest depth and lowerdir path counts remain within configured squash
  thresholds;
- LayerStack head-dedup still works for no-op/duplicate captures;
- forward/back Rust/Python parity remains canonically equal for published
  states and `layer_digest` streams.

### Overlay Resource Bounds

Load tests must prove:

- shared lowerdir storage remains O(1) with respect to concurrent sessions;
- per-session scratch grows with changed bytes and transcript spool only;
- workspace materialized tree is not created for command execution;
- run dirs/session dirs are removed after finalization or GC;
- spool caps prevent noisy PTYs from unbounded disk growth.

### Audit and Report Load

Under CP-4 load:

- daemon audit pull must be drop-free;
- `performance_report.json` schema must keep command/session sections;
- report must include per-tool p50/p95/max, phase breakdown, resource samples,
  daemon audit pull stats, and artifact size;
- host `sandbox_events.jsonl` rotation must stay within artifact-bound gate;
- daemon in-memory audit ring pressure must stay below configured threshold.

## 10. Performance Gates

Use overlay-inclusive timings only. Microbenchmarks that bypass lease, mount,
capture, OCC, cleanup, or release are diagnostic and cannot close Phase 3T.

Hard gates:

```text
tty=false non-login Bash true:
  p95 <= 60 ms with overlay

tty=true native PTY non-login Bash true:
  p95 <= 100 ms with overlay

check_pty_command_progress:
  p95 <= 20 ms for bounded recent ring reads

write_pty_command_stdin to visible echo:
  p95 <= 100 ms for a waiting echo/read command

cancel_pty_command:
  p95 <= 500 ms to process-tree-gone for normal SIGTERM path
  hard cleanup <= 2.5 s including SIGKILL escalation

finalization cleanup:
  lease count, session dir, process group, and cgroup return to baseline
```

Track but do not initially gate:

- native PTY p95 <= 75 ms stretch target;
- 25 concurrent PTY sessions;
- noisy transcript spool throughput;
- daemon RSS/PSS delta per active PTY;
- artifact size under long-running session output.

## 11. Exit Criteria

This addendum is complete only when:

- model-facing command tools use the final names in this document;
- `output` is the public result field, with `stdout` and `stderr` only;
- stdin is not part of public output;
- `tty=false` is finite, no-stdin, non-PTY, and kills detached descendants;
- `tty=true` uses native Rust PTY, not `script(1)`;
- PTY controls are active-only and do not reveal finished-vs-never-created;
- daemon owns PTY transcript storage, finalization, and GC;
- Python background manager owns typed notifications and active-work gates;
- shared and isolated workspace semantics are both covered by live E2E;
- OCC conflicts, LayerStack leases, overlay cleanup, audit drop-free behavior,
  and performance gates have artifact-backed evidence.
