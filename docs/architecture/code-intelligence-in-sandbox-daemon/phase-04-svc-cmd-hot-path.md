# Phase 4 — `svc.cmd` hot path through the daemon

**Estimated effort:** 4-5 days (2 days engineering + 2-3 days E2E)
**Risk profile:** HIGH — `svc.cmd` is the most-used hot path; any regression hits every shell op
**Status:** Superseded by the Phase 3.5 / 3.6 closure pass
**Blocks on:** Phase 3.5 complete (perf safety net + SQLite-backed index landed)

> Current implementation note (2026-05-03): this plan is retained as
> historical design context, not an open Phase 4 deferral list.
> `ci_daemon.DISPATCH["svc_cmd"]` exists, `RpcCiBackend.cmd` routes through
> it, daemon-local subprocess execution is wired in
> `overlay/command_executor.py`, and the result-shape contract is covered by
> `test_rpc_ci_backend.py`, `test_ci_daemon_dispatch.py`, and
> `test_overlay_dispatch.py`. The first live Phase 3.5 / 3.6 runs exposed a
> ~5.5 s public-call floor, but follow-up instrumentation traced that to
> `run_sync(...)` creating a fresh event loop for sync callers without a
> registered `sandbox_io_loop`. The stable-loop fix in
> `sandbox.client.async_bridge` drops daemon `status` to p50 0.448 s and
> `query_symbols("Array")` to p50 0.540 s through the same sync facade.
> Future work should be framed as transport optimization, explicit batching,
> or streaming enhancement, not completion of this Phase 4 plan.

## ⚠️ Result-shape preservation (LOAD-BEARING)

`svc.cmd` returns a `SimpleNamespace` with **16 fields**, not just `(stdout, stderr, exit_code)`. Every field must round-trip through the daemon byte-for-byte. The full set:

| Field | Type | Source |
|---|---|---|
| `result` | `str` | stdout from the user command |
| `exit_code` | `int` | command exit code |
| `changed_paths` | `list[str]` | gitinclude paths committed via OCC |
| `ambient_changed_paths` | `list[str]` | paths whose changes were dropped (`attribute_changes=False`) |
| `files_written` | `int` | count of OCC-committed files |
| `git_commit_status` | `str \| None` | `"committed"` / `"noop"` / `"aborted_version"` / `"rejected"` / `None` |
| `git_conflict_file` | `str \| None` | first conflicted path on abort |
| `git_conflict_reason` | `str \| None` | abort reason text |
| `gitinclude_changed_paths` | `list[str]` | gitinclude route paths |
| `gitignore_direct_merged_paths` | `list[str]` | gitignore route paths (direct-merged, not OCC) |
| `gitignore_direct_merged_count` | `int` | count of direct-merged paths |
| `mixed_gitinclude_gitignore` | `bool` | both routes had changes |
| `mixed_partial_apply` | `bool` | OCC aborted while gitignore writes already landed |
| `warnings` | `list[str]` | overlay-side warnings |
| `git_snapshot_timings` | `dict[str, float]` | snapshot-script per-step timings |
| `overlay_run_timings` | `dict[str, float]` | overlay runtime per-step timings |

**Downstream callers in `backend/src/sandbox/lifecycle/commit.py` rely on the full set.** Losing any field would break attribution, conflict reporting, or the gitignore direct-merge contract. Task 4.3's parity test is the gate.

## Goal

Route `svc.cmd(sandbox, command, ...)` through the daemon. The two dominant per-`svc.cmd` costs (`_commit_changes` ~0.65s + `overlay_run` ~0.43s, per `memory/codeact_overlay_cost_breakdown.md`) execute in-sandbox without orchestrator round-trips. The orchestrator-side `AuditedCommandExecutor.cmd` becomes a thin wrapper that ships a single `svc_cmd` RPC and reconstructs the `SimpleNamespace` result.

**This is the headline phase for the migration's perf claim.** Phase 4's E2E proves the user-facing latency win.

## Why fifth

Three reasons:

1. **OCC must be daemon-side first.** Phase 3 moved `WriteCoordinator` and `OverlayCommandCommitter` into the daemon. Without that, every `svc.cmd` would still bounce back through the transport for OCC commit, killing the perf win. Phase 3 had to land first.
2. **All the moving parts of `svc.cmd` are now daemon-resident.** `git_snapshot` (Phase 3), `OverlayAuditor` (Phase 3), `OverlayCommandCommitter` (Phase 3), `WriteCoordinator` (Phase 3). Phase 4 just wires the existing `cmd()` async method through one new RPC op.
3. **Phase 5 default-flag-flip needs evidence.** The success criterion "svc.cmd warm-path latency strictly lower than today" must be proven before Phase 5 flips the default. Phase 4 produces that evidence.

## What ships

| Artifact | File | Purpose |
|---|---|---|
| `svc_cmd` daemon op | `backend/src/sandbox/code_intelligence/in_sandbox/ci_daemon.py` (extended) | Receives `(command, kwargs)`, runs the full overlay+commit pipeline locally, returns the assembled result dict |
| `RpcCiBackend.cmd` | `backend/src/sandbox/code_intelligence/backend.py` (extended) | Async one-line call to the daemon's `svc_cmd` op; reconstructs `SimpleNamespace` |
| Orchestrator-side `AuditedCommandExecutor` | `backend/src/sandbox/code_intelligence/overlay/command_executor.py` (modified) | Flag-on path delegates to backend; flag-off keeps existing |
| Phase 4 live E2E | `backend/tests/test_e2e/test_live_ci_phase4_svc_cmd.py` | Real shell commands, gitinclude OCC + gitignore direct-merge, byte-identical result |
| Result-shape parity test | `backend/tests/test_sandbox/test_code_intelligence/test_svc_cmd_shape_parity.py` | Daemon return dict reconstructs to a `SimpleNamespace` byte-identical to in-process baseline |

## Detailed task list

### Task 4.1 — `svc_cmd` daemon op

**File to modify:** `backend/src/sandbox/code_intelligence/in_sandbox/ci_daemon.py`

**Add to `DISPATCH`:**

```python
DISPATCH["svc_cmd"] = handle_svc_cmd
```

**Handler signature:**

```python
async def handle_svc_cmd(args: dict) -> dict:
    """Wraps the daemon's OverlayAuditor.execute(...) and returns a serializable
    result dict that the orchestrator reconstructs into a SimpleNamespace."""
    command:           str          = args["command"]
    timeout:           int | None   = args.get("timeout")
    description:       str          = args.get("description", "")
    agent_id:          str          = args.get("agent_id", "")
    run_id:            str          = args.get("run_id", "")
    agent_run_id:      str          = args.get("agent_run_id", "")
    task_id:           str          = args.get("task_id", "")
    stdin:             str | None   = args.get("stdin")
    attribute_changes: bool         = args.get("attribute_changes", True)
    # on_progress_line is NOT shipped — see Task 4.4 for the streaming approach

    # Run the daemon-resident OverlayAuditor — sandbox is None because we're inside
    # the sandbox; the auditor's _do_exec uses local subprocess.
    auditor: OverlayAuditor = _DAEMON_STATE.overlay_auditor  # constructed at startup
    result_ns = await auditor.execute(
        sandbox=None,
        command=command,
        timeout=timeout,
        description=description,
        agent_id=agent_id,
        run_id=run_id,
        agent_run_id=agent_run_id,
        task_id=task_id,
        stdin=stdin,
        attribute_changes=attribute_changes,
        on_progress_line=None,  # see Task 4.4
    )

    # Convert SimpleNamespace -> dict for msgpack
    return {
        "result": result_ns.result,
        "exit_code": result_ns.exit_code,
        "changed_paths": list(result_ns.changed_paths),
        "ambient_changed_paths": list(result_ns.ambient_changed_paths),
        "files_written": result_ns.files_written,
        "git_commit_status": result_ns.git_commit_status,
        "git_conflict_file": result_ns.git_conflict_file,
        "git_conflict_reason": result_ns.git_conflict_reason,
        "gitinclude_changed_paths": list(result_ns.gitinclude_changed_paths),
        "gitignore_direct_merged_paths": list(result_ns.gitignore_direct_merged_paths),
        "gitignore_direct_merged_count": result_ns.gitignore_direct_merged_count,
        "mixed_gitinclude_gitignore": result_ns.mixed_gitinclude_gitignore,
        "mixed_partial_apply": result_ns.mixed_partial_apply,
        "warnings": list(result_ns.warnings),
        "git_snapshot_timings": dict(result_ns.git_snapshot_timings or {}),
    }
```

**Daemon state:** `_DAEMON_STATE.overlay_auditor` is constructed once at daemon startup (in `run_daemon`). It binds to:
- `workspace_root` (from `--workspace-root` arg)
- `_local_subprocess_exec` (Phase 3, Task 3.3)
- The daemon-resident `WriteCoordinator` (Phase 3)
- No `transport`, no `sandbox` — all I/O is local FS / `subprocess.run`.

### Task 4.2 — Orchestrator-side wiring

**File to modify:** `backend/src/sandbox/code_intelligence/backend.py`

```python
class RpcCiBackend:
    async def cmd(self, sandbox: Any, command: str, **kwargs: Any) -> SimpleNamespace:
        # Filter kwargs to only msgpack-friendly types; on_progress_line is callback (skip)
        on_progress = kwargs.pop("on_progress_line", None)
        if on_progress is not None:
            # Phase 4 ships WITHOUT progress streaming. Document this gap loudly.
            # Phase 5 (or 4.5) can add a streaming op via separate RPC channel.
            logger.warning("svc_cmd via daemon: on_progress_line callback dropped")

        args = {"command": command, **kwargs}
        raw = await self._client.call("svc_cmd", args, timeout=kwargs.get("timeout") or 600)
        return SimpleNamespace(**raw)
```

**`AuditedCommandExecutor.cmd` (orchestrator-side, `overlay/command_executor.py`):** No change needed — the `cmd` call already routes through `CodeIntelligenceService.cmd` which goes through `_impl.cmd`. With Phase 0's facade in place, the routing is automatic.

### Task 4.3 — Result-shape parity test

**File:** `backend/tests/test_sandbox/test_code_intelligence/test_svc_cmd_shape_parity.py`

Construct a fake `OverlayAuditor.execute` return value (with every field populated). Round-trip through:
1. Convert to dict (Task 4.1's serialization).
2. msgpack pack/unpack.
3. `SimpleNamespace(**unpacked)`.

Assert every attribute on the reconstructed namespace matches the original byte-for-byte (lists, dicts, scalars, None/missing values).

This protects Phase 4 from a silent serialization bug where downstream `commit/submit_shell_cmd` relies on a specific field shape.

### Task 4.4 — `on_progress_line` streaming (historical decision)

**Today's behavior:** `OverlayAuditor.execute` accepts an `on_progress_line: Callable[[str], None]` callback that gets fed lines from the running command's stdout as they appear (via `_run_overlay_with_progress` polling `stdout.bin` on a 2s interval).

**Phase 4 problem:** A callback can't cross the RPC boundary. Three options:
- **(A)** DROP the callback — `RpcCiBackend.cmd` ignores it. User loses live progress for `svc.cmd`. Acceptable if all current callers tolerate batched stdout.
- **(B)** Stream via a separate RPC op `svc_cmd_progress(request_id)` polled by the orchestrator. Adds complexity.
- **(C)** Use a server-push pattern over the same socket (push frames mid-request). Requires protocol extension.

**Current outcome:** `RpcCiBackend.cmd` does not stream mid-command
progress over the RPC socket, but it does replay final stdout to the
callback after the daemon response is reconstructed. True streaming should
be framed as a future transport enhancement, not a Phase 4 completion
blocker.

If product feedback says final-stdout replay is not enough, future
transport work can add option (B): orchestrator polls
`svc_cmd_progress(req_id)` every 2s in parallel with the main RPC; daemon
serves stdout deltas from `stdout.bin`.

**Document this decision in the PR description so reviewers don't silently lose a feature.**

### Task 4.5 — Phase 4 live E2E

**File:** `backend/tests/test_e2e/test_live_ci_phase4_svc_cmd.py`

#### 4.5.A — Real shell command, byte-identical result

```python
async def test_svc_cmd_byte_identical_to_baseline(live_sweevo_env):
    h = TimingHarness(phase=4, test_name="svc_cmd_byte_identical")
    env = live_sweevo_env

    cmd = "find /testbed -name '*.py' | wc -l"

    with h.step("svc_cmd_baseline_inprocess"):
        # Force in-process backend
        with mock.patch.dict(os.environ, {"EOS_CI_IN_SANDBOX": "0"}):
            svc_in = env.make_ci_service()
            baseline = await svc_in.cmd(env.raw_sandbox, cmd)

    with h.step("svc_cmd_via_daemon"):
        with mock.patch.dict(os.environ, {"EOS_CI_IN_SANDBOX": "1"}):
            svc_d = env.make_ci_service()
            daemon_result = await svc_d.cmd(env.raw_sandbox, cmd)

    # Byte-identical comparison
    assert baseline.exit_code == daemon_result.exit_code
    assert baseline.result.strip() == daemon_result.result.strip()  # same wc -l output
    assert sorted(baseline.changed_paths) == sorted(daemon_result.changed_paths)
    assert sorted(baseline.ambient_changed_paths) == sorted(daemon_result.ambient_changed_paths)
    assert baseline.git_commit_status == daemon_result.git_commit_status

    # HEADLINE PERF ASSERTION
    inprocess_time = h.steps["svc_cmd_baseline_inprocess"]
    daemon_time = h.steps["svc_cmd_via_daemon"]
    assert daemon_time < inprocess_time, (
        f"svc_cmd via daemon ({daemon_time:.3f}s) NOT faster than in-process ({inprocess_time:.3f}s) "
        f"— migration perf claim FAILED"
    )

    print(h.report())
    print(h.compare_to(latest_phase0_baseline()))
    h.dump_json()
```

#### 4.5.B — Real `pytest` invocation

```python
async def test_svc_cmd_runs_real_pytest(live_sweevo_env):
    h = TimingHarness(phase=4, test_name="svc_cmd_runs_pytest")
    env = live_sweevo_env
    svc = env.make_ci_service_flag_on()

    # Pick the smallest, fastest test in dask
    cmd = "cd /testbed && python -m pytest dask/tests/test_base.py::test_normalize_function_limited -x"

    with h.step("svc_cmd_pytest"):
        result = await svc.cmd(env.raw_sandbox, cmd, timeout=120)

    assert result.exit_code == 0, f"pytest failed: {result.result[-2000:]}"
    assert "passed" in result.result.lower()

    print(h.report())
    h.dump_json()
```

#### 4.5.C — Gitinclude OCC commit path

```python
async def test_svc_cmd_gitinclude_occ(live_sweevo_env):
    h = TimingHarness(phase=4, test_name="svc_cmd_gitinclude_occ")
    env = live_sweevo_env
    svc = env.make_ci_service_flag_on()

    # Edit a tracked file via shell — should commit through OCC route
    target = "/testbed/dask/__init__.py"  # tracked file
    cmd = f"echo '# phase4 marker' >> {target}"

    with h.step("svc_cmd_modifies_tracked_file"):
        result = await svc.cmd(env.raw_sandbox, cmd)

    assert result.exit_code == 0
    assert result.git_commit_status == "committed"
    assert target in result.gitinclude_changed_paths or any(target in p for p in result.changed_paths)

    # Verify the change actually landed
    _, content = env.exec(f"tail -1 {target}")
    assert "phase4 marker" in content

    print(h.report())
    h.dump_json()
```

#### 4.5.D — Gitignore direct-merge path

```python
async def test_svc_cmd_gitignore_direct_merge(live_sweevo_env):
    h = TimingHarness(phase=4, test_name="svc_cmd_gitignore_direct_merge")
    env = live_sweevo_env
    svc = env.make_ci_service_flag_on()

    # Write to a gitignored path (e.g., __pycache__ or a custom .gitignore'd dir)
    env.exec(f"cd /testbed && echo 'build_artifact' >> .gitignore")
    cmd = "cd /testbed && mkdir -p _phase4_artifacts && echo 'gitignored content' > _phase4_artifacts/x.bin"
    env.exec("cd /testbed && echo '_phase4_artifacts/' >> .gitignore")

    with h.step("svc_cmd_writes_gitignored"):
        result = await svc.cmd(env.raw_sandbox, cmd)

    assert result.exit_code == 0
    assert "_phase4_artifacts/x.bin" in result.gitignore_direct_merged_paths or \
           result.gitignore_direct_merged_count > 0

    print(h.report())
    h.dump_json()
```

#### 4.5.E — Cross-test compare-to baseline

After all subtests run, programmatically load each `phase_4_*_<ts>.json` and produce a consolidated `compare_to(phase_0_baseline)` summary table. Print it as the test class teardown.

**Run command:** `uv run pytest backend/tests/test_e2e/test_live_ci_phase4_svc_cmd.py -m live -v -s`

### Task 4.6 — Regression check

- `.venv/bin/pytest backend/tests/test_sandbox/ backend/tests/test_tools/ -q` — green with flag off.
- Re-run Phases 0, 1, 2, 3 live E2Es — all green.

## Definition of done

- [ ] `svc_cmd` op exists in daemon dispatch; serializes/deserializes the full `SimpleNamespace` shape.
- [ ] Result-shape parity test (Task 4.3) passes — every field round-trips.
- [ ] `RpcCiBackend.cmd` ships args + reconstructs `SimpleNamespace`.
- [x] `on_progress_line` behavior documented: final stdout replay is implemented; true live streaming is future transport enhancement.
- [ ] **Phase 4 live E2E (all 4 subtests A-D) passes against `dask__dask_2023.3.2_2023.4.0`.**
- [ ] **HEADLINE PERF ASSERTION (4.5.A): `svc_cmd_via_daemon` < `svc_cmd_baseline_inprocess` for the warm path.** This is the migration's headline win.
- [ ] Real `pytest` invocation succeeds end-to-end (4.5.B).
- [ ] Gitinclude OCC commit path verified live (4.5.C) — tracked file edit lands via OCC.
- [ ] Gitignore direct-merge path verified live (4.5.D) — gitignored writes go through direct-merge.
- [ ] Regression check: Phases 0, 1, 2, 3 E2Es + full unit suite green.
- [ ] PR description includes: 4 E2E reports + headline perf delta in big bold letters + `on_progress_line` decision note.

## Risk callouts (Phase 4 specific)

| Severity | Risk | Mitigation |
|---|---|---|
| **HIGH** | `svc_cmd_via_daemon` is SLOWER than baseline → migration perf claim fails | Historical Phase 4 gate. Follow-up measurements traced the public-call floor to sync-bridge loop churn and fixed it; remaining sub-second transport cost belongs to Phase 5 `ci_rpc` / batching work if needed. |
| **HIGH** | Result `SimpleNamespace` shape drift → downstream `commit/submit_shell_cmd` callers break in subtle ways | Result-shape parity test (4.3) catches this; downstream callers in `backend/src/sandbox/lifecycle/commit.py` should also have unit tests added |
| **HIGH** | `on_progress_line` loses live streaming → CodeAct UI shows stdout only when the command completes | Current implementation replays final stdout; if product blocks, add a future streaming transport op such as option (B) |
| **MEDIUM** | Daemon-resident `OverlayAuditor` doesn't share the orchestrator-side `_overlay_runtime_bundle_bytes()` upload | The overlay runtime under `/tmp/eos-shell-overlay/` is sandbox-resident already; no orchestrator dependency. Verify in 4.5.A that `overlay_run.py` is found |
| **MEDIUM** | `git_snapshot` running locally in daemon disagrees with the orchestrator-shipped snapshot script (different snap SHA) | Drift guard `test_git_snapshot_local_parity.py` (Phase 3 should ship this; verify here) |
| **MEDIUM** | RPC timeout default (30s in `CiRpcClient.call`) is too short for long-running commands | `cmd` op uses `timeout=kwargs.get("timeout") or 600` — match today's `_DEFAULT_SANDBOX_COMMAND_TIMEOUT` |
| **LOW** | `attribute_changes=False` path (ambient-only writes) doesn't go through OCC | Current behavior: `_audit_result` returns ambient-only when `attribute_changes=False`. Same in daemon. 4.5.A asserts gitinclude path implicitly |

## Hand-off to Phase 5

Phase 5 picks up with:
- `svc.cmd` running through the daemon end-to-end.
- The headline perf claim already validated by 4.5.A.
- A complete `RpcCiBackend` — every method routed.
- The `socat`/`nc`/python shim becoming the obvious next bottleneck (visible in Phase 4's timing reports).
- Phase 5 promotes `ci_rpc` to a first-class transport verb, eliminating shim overhead, and flips the `EOS_CI_IN_SANDBOX` default to `1`.
