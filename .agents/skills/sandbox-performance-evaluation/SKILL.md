---
name: sandbox-performance-evaluation
description: Use when evaluating EphemeralOS sandbox correctness or performance for unified layerstack, overlay, OCC, isolated_workspace, command_exec, plugin dispatch, Pyright/LSP, high-concurrency live_e2e scenarios, mount(2) overlay O(1) disk usage, CPU, memory, or .sweevo_runs scenario artifacts.
---

# Sandbox Performance Evaluation

Use this skill to verify whether EphemeralOS sandbox operations preserve the intended filesystem semantics and remain fast under concurrency. Work from the current checkout and current run artifacts; never present old timings as current without rechecking.

## Contract To Verify

Check the design as a set of observable claims:

1. `command_exec` and plugin dispatch both go through layerstack snapshot lease, per-operation overlay upperdir, upperdir capture, and OCC publish. They may use different runner functions, but the semantics must match.
2. The overlay is not itself "a lowerdir". Correct wording: the overlay mount is built from shared leased snapshot layers as lowerdirs plus an independent per-operation `upperdir` and `workdir`.
3. Generic plugin operations should default to automatic workspace overlay dispatch. Stateful runtimes such as Pyright/LSP may opt out only if they manage their own leased overlay lifecycle.
4. Pyright/LSP must see the latest layerstack snapshot at the bound workspace root, not a stale projected copy. A snapshot refresh should remount or refresh the long-lived session, not restart the server on every normal write.
5. The mounted process should see a normal container filesystem with only the bound workspace root replaced by the overlay. Files outside the workspace are normal container files and are not captured by workspace OCC unless another mechanism captures them.
6. In the private namespace/new mount API path, overlay disk use should be O(1) with respect to workspace size and number of parallel readers/writers. Per-operation disk should scale with changed files and scratch metadata only.

## Isolated Workspace Contract

Use this path when the task names `isolated_workspace`, `iws`, pinned
workspace handles, per-agent network namespaces, cgroup freezer behavior,
daemon restart GC, or Tier 8 soak tests.

Observable claims:

1. `isolated_workspace` is structurally separate from OCC publish. It uses a
   distinct `IsolatedWorkspaceHandle`, a pinned layer-stack snapshot lease, a
   private net/pid/mount/user namespace, a tmpfs upperdir, and an explicit
   discard-on-exit path. It must not call OCC or sandbox-overlay publish code.
2. Enter events carry snapshot and setup evidence:
   `manifest_version`, `manifest_root_hash`, `ns_ip`,
   `lowerdir_layer_count`, `materialize=false`, `total_ms`, and `phases_ms`.
3. Tool-call events carry execution evidence: `argv0`, `exit_code`,
   `duration_s`, `total_ms`, and `phases_ms`.
4. Exit, TTL eviction, and startup-GC events carry discard/reap evidence:
   `upperdir_bytes_discarded` or orphan `kind`, `identifier`, `total_ms`, and
   `phases_ms`.
5. `phases_ms` follows conditional-key emission: a key appears only when that
   phase ran to completion. Do not emit `0.0` for skipped or stubbed phases.
6. SUBSET-COVER must hold on every isolated-workspace audit event:
   `sum(phases_ms.values()) <= total_ms + max(2.0, 0.05 * total_ms)`.
7. Pinned handles freeze between tool calls, discard upperdir contents on exit,
   preserve lowerdir snapshot pinning against peer publishes, and isolate
   filesystem, network, cgroup memory, and bridge ports across agents.
8. The Tier 8 soak standard is opt-in: `TOTAL_CAP=5`, `install_veth` contention
   bounded by `max <= 5 * median`, idle disk <= 10 MiB after 60 s, idle frozen
   cgroup CPU does not grow, public-internet pip/httpx succeeds when egress is
   available, and 100 create/destroy cycles do not grow daemon FD/veth counts.

## Code Pointers

Before making causal claims, inspect the live code paths:

```bash
rg -n "register_plugin_op|auto_workspace_overlay|run_plugin_op_with_workspace_overlay|acquire_operation_overlay|publish_cycle" backend/src/sandbox backend/src/plugins
rg -n "prepare_workspace_snapshot|LayerPathsLayout|mount_workspace_s|mount_overlay|new_mount_api_supported" backend/src/sandbox
rg -n "PyrightSession|refresh_manifest|namespace_remount|auto_workspace_overlay=False|lsp.session" backend/src/plugins/catalog/lsp
rg -n "IsolatedWorkspaceManager|sandbox_isolated_workspace|phases_ms|install_veth|ttl_sweep|startup_gc" backend/src/sandbox/isolated_workspace backend/src/task_center_runner/tests/mock/sandbox/isolated_workspace
```

Expected anchors:

- `backend/src/sandbox/execution/service.py`: command_exec lease -> run -> publish lifecycle.
- `backend/src/sandbox/execution/strategies/namespace_child.py`: command child mounts overlay at `workspace_root`.
- `backend/src/sandbox/execution/overlay/kernel_mount.py`: new mount API overlay construction.
- `backend/src/sandbox/plugin/op_registry.py`: plugin ops default to `auto_workspace_overlay=True`.
- `backend/src/sandbox/plugin/overlay_dispatch.py`: one-shot plugin overlay lease, child dispatch, publish, release.
- `backend/src/sandbox/plugin/overlay_child.py`: plugin child mounts overlay at the workspace binding root.
- `backend/src/sandbox/daemon/service/sandbox_overlay.py`: operation overlay handle, upperdir allocation, OCC publish.
- `backend/src/plugins/catalog/lsp/runtime/session_manager.py`: long-lived LSP session snapshot refresh.
- `backend/src/plugins/catalog/lsp/runtime/pyright_session.py` and `namespace_remount.py`: Pyright private namespace remount.
- `backend/src/sandbox/isolated_workspace/manager.py`: isolated handle
  lifecycle, phase timing, freeze/thaw, discard, TTL, and startup GC.
- `backend/src/sandbox/isolated_workspace/handlers.py`: JSONL audit sink at
  `/tmp/sandbox_isolated_workspace_events.jsonl` unless
  `EOS_ISOLATED_WORKSPACE_AUDIT_PATH` overrides it.
- `backend/src/sandbox/isolated_workspace/ops_handlers.py`: bounded tool-call
  handlers; import fence must keep OCC and sandbox-overlay publish out.
- `backend/src/sandbox/isolated_workspace/network.py`: bridge, MASQUERADE,
  IMDS/RFC1918 policy, veth install/teardown, and IP pool.
- `backend/src/task_center_runner/tests/mock/sandbox/isolated_workspace/`:
  Tier 0-9 contract tests and `RUNNING-LIVE-TESTS.md` /
  `RUNNING-SOAK-TESTS.md`.

## Scenario Selection

Locate current tests first; paths move:

```bash
rg -n "high_concurrency_layerstack_overlay_occ|complex_project_build_shell_edit_lsp|full_system_capacity_matrix|heavy_io_zoned_concurrent|background_shell_" backend/src/task_center_runner/tests backend/src/task_center_runner/scenarios
rg -n "isolated_workspace|sandbox_isolated_workspace|live_e2e_soak|phases_ms|install_veth" backend/src/task_center_runner/tests/mock/sandbox/isolated_workspace backend/src/sandbox/isolated_workspace
```

For isolated_workspace changes, run the focused ladder first.

Static surface, always on and fast:

```bash
uv run pytest \
  backend/src/task_center_runner/tests/mock/sandbox/isolated_workspace/pre_flight/ \
  backend/tests/unit_test/test_sandbox/test_daemon/ \
  backend/tests/unit_test/test_sandbox/test_import_fence.py \
  backend/tests/unit_test/test_audit/ \
  backend/tests/unit_test/test_task_center/test_audit/ \
  -q
```

Quick isolated_workspace live smoke:

```bash
EOS_SWEEVO_INSTANCE=dask__dask_2023.3.2_2023.4.0 \
EOS_SANDBOX_PROVIDER=docker \
EOS_ISOLATED_WORKSPACE_ENABLED=true \
EOS__RUNNER__LIVE_E2E__HEAVY_ENABLED=true \
EPHEMERALOS_DATABASE_URL="sqlite:///./.ephemeralos/ephemeralos.db" \
uv run pytest \
  backend/src/task_center_runner/tests/mock/sandbox/isolated_workspace/pre_flight/ \
  backend/src/task_center_runner/tests/mock/sandbox/isolated_workspace/happy_path/ \
  -v
```

Full isolated_workspace live gate, excluding opt-in soak:

```bash
EOS_SWEEVO_INSTANCE=dask__dask_2023.3.2_2023.4.0 \
EOS_SANDBOX_PROVIDER=docker \
EOS_ISOLATED_WORKSPACE_ENABLED=true \
EOS__RUNNER__LIVE_E2E__HEAVY_ENABLED=true \
EOS__RUNNER__SANDBOX_REUSE_MODE=reuse \
EPHEMERALOS_DATABASE_URL="sqlite:///./.ephemeralos/ephemeralos.db" \
uv run pytest \
  backend/src/task_center_runner/tests/mock/sandbox/isolated_workspace/ \
  -m "not live_e2e_soak" \
  --tb=short --durations=20 -v -p no:randomly
```

Tier 8 soak tests are nightly-style and expensive. Prefer one test at a time
while iterating, then run the whole stress directory:

```bash
EOS_SWEEVO_INSTANCE=dask__dask_2023.3.2_2023.4.0 \
EOS_SANDBOX_PROVIDER=docker \
EOS_ISOLATED_WORKSPACE_ENABLED=true \
EOS__RUNNER__LIVE_E2E__HEAVY_ENABLED=true \
EOS__RUNNER__SANDBOX_REUSE_MODE=reuse \
EPHEMERALOS_DATABASE_URL="sqlite:///./.ephemeralos/ephemeralos.db" \
uv run pytest \
  backend/src/task_center_runner/tests/mock/sandbox/isolated_workspace/stress/ \
  -m live_e2e_soak \
  --tb=short --durations=20 -v -p no:randomly
```

If the runner lacks public internet, isolate that infrastructure problem by
deselecting only the internet-bound soak test:

```bash
uv run pytest \
  backend/src/task_center_runner/tests/mock/sandbox/isolated_workspace/stress/ \
  -m live_e2e_soak \
  -k "not test_pip_install_then_run_e2e" \
  --tb=short --durations=20 -v -p no:randomly
```

For the older SWE-EVO scenario coverage, run sequentially. Start with smoke
tests:

```bash
uv run pytest -q -x --tb=short --durations=20 \
  backend/src/task_center_runner/tests/mock/sandbox/test_complex_project_build_smoke.py

uv run pytest -q -x --tb=short --durations=20 \
  backend/src/task_center_runner/tests/mock/sandbox/test_complex_project_build_shell_edit_lsp_smoke.py
```

Then run the full targeted scenarios:

```bash
uv run pytest -q -x --tb=short --durations=20 \
  backend/src/task_center_runner/tests/mock/sandbox/test_complex_project_build_full.py

uv run pytest -q -x --tb=short --durations=20 \
  backend/src/task_center_runner/tests/mock/sandbox/test_complex_project_build_shell_edit_lsp_full.py

uv run pytest -q -x --tb=short --durations=20 \
  backend/src/task_center_runner/tests/mock/sandbox/test_high_concurrency_layerstack_overlay_occ.py

uv run pytest -q -x --tb=short --durations=20 \
  backend/src/task_center_runner/tests/mock/sandbox/test_heavy_io_zoned_concurrent.py

uv run pytest -q -x --tb=short --durations=20 \
  backend/src/task_center_runner/tests/mock/sandbox/capacity/test_full_system_capacity_matrix.py
```

Background-shell suite (`shell(background=True)` daemon-native path,
T1-T8). Runs the seven harness-driven scenarios plus the in-process
TTL-reaper unit test:

```bash
EOS_SWEEVO_INSTANCE=dask__dask_2023.3.2_2023.4.0 \
uv run pytest -q --tb=short --durations=20 \
  backend/src/task_center_runner/tests/mock/sandbox/background_tool/
```

```bash
uv run pytest -q -x --tb=short --durations=20 \
  /Users/yifanxu/machine_learning/LoVC/EphemeralOS/backend/src/task_center_runner/tests/mock/sandbox
```

Scenario coverage cheatsheet:

- `complex_project_build_shell_edit_lsp`: serial mixed shell-edit + Pyright
  workload with diagnostics. Use when LSP remount/start counts or
  shell-vs-edit_file routing is suspect.
- `high_concurrency_layerstack_overlay_occ`: 20-way concurrent write/edit
  pressure on a shared OCC target. Use when suspecting OCC commit-queue
  contention, layer-stack lock_wait, or auto-squash regression.
- `heavy_io_zoned_concurrent`: 5 concurrent workers running long shells
  (~30-50s, ~33 MB each) into three placement zones — gitincluded
  (`/testbed/perf_load_tracked/`), gitignored (`/testbed/build/`), and
  outside-workspace (`/tmp/heavy_io_zoned/`). Use when characterizing
  layerstack lease hold under long shells, OCC merge correctness across
  zones, or .gitignore-aware snapshot behavior. Asserts O(1) overlay disk
  (`workspace_tree_bytes == 0`) and outside-zone OCC isolation (`/tmp`
  paths never leak into workspace OCC `changed_paths`).
- `full_system_capacity_matrix`: broad sweep that intentionally exercises
  synthetic OCC conflicts (SymlinkChange, anchor-not-found, non-zero
  shell). Use to sanity-check the whole stack and to validate that typed
  conflicts stay typed.
- `background_shell_*` (T1-T8): seven harness scenarios plus one unit
  test covering the daemon-native `shell(background=True)` launch/poll/
  cancel/reap surface. Each scenario uses a single executor action that
  drives the matching probe in
  `backend/src/task_center_runner/agent/mock/background_shell_probe.py`;
  probes call `shell_tool` with `background_task_id` set, write a JSON
  summary to `/testbed/.ephemeralos/sweevo-mock/background_shell/<mode>/
  summary.json`, and the tests read it back via `sandbox_api.read_file`.
  - `background_shell_golden` (T1): 3 concurrent 5-s sleeps; confirms
    natural-exit reap, exit_code 0, populated stdout. Sanity for the
    launch/reap roundtrip and the rmtree-before-read regression.
  - `background_shell_cancel` (T2): 3 long shells cancelled at 1 s via
    `asyncio.wait_for`. Asserts AC-3 (post-cancel foreground mount under
    5 s) and AC-6 (zero changed_paths from cancelled jobs).
  - `background_shell_interleave` (T3): 1 long background lease + 5
    foreground shells. Records foreground p95
    `command_exec.mount_workspace_s`; AC-3 expects p95 under 5 s while a
    background lease is held.
  - `background_shell_exhaustion` (T5): 80 launches cancelled at 2 s.
    AC-14 — post-exhaustion `read_file` under 1 s proves the daemon RPC
    dispatcher is decoupled from the `ShellExecutor`. Watch
    `command_exec.mount_workspace_s` p95 — concurrency contention shows
    up here (~5-8× T1 baseline).
  - `background_shell_partial_write_cancel` (T6): 800 MB dd into a
    tracked path, cancelled at 2 s. Reads back the target after cancel;
    AC-6 — upperdir is discarded and the OCC publish is skipped. Probe
    wraps dd in `for ... do ... done` so it doesn't match the
    DestructiveShellPreHook regex `[;&|]\s*dd\s+.*of=/`, and seeds a
    sentinel file so OCC persists the parent dir across leases.
  - `background_shell_cancel_during_maintenance` (T7): short shell that
    writes one file + maintenance pass. Asserts the workspace OCC stays
    consistent (target in `changed_paths`, follow-up read returns the
    written content).
  - `background_shell_late_cancel_race` (T8): await a 1-s shell to
    completion. AC-10 — exit_code 0 and stdout preserved (completed >
    failed > cancelled precedence holds when the natural exit wins the
    race).
  - `test_background_shell_engine_kill` (T4): in-process unit test for
    `ShellJobRegistry` TTL reaper. No sandbox, no scenario harness —
    leave alone.

  Background-mode plumbing: `runner._call_tool(background_task_id=...)`
  pipes the bg-id into `ExecutionMetadata.with_overrides` so
  `shell.py:154` flips to the daemon launch/poll/cancel/reap surface.
  Cancel propagation matches the production engine path: an
  `asyncio.wait_for` timeout becomes a `CancelledError` that
  `_shell_background_dispatch._send_cancel_then_reap` handles.

## Run Configuration

Treat `ephemeralos.yaml` as the source of truth for live-run gates and sandbox
reuse. Do not add removed one-off runner-policy env toggles to run commands.

Required YAML shape:

```yaml
runner:
  sandbox_reuse_mode: reuse
  live_e2e:
    heavy_enabled: true
    capacity_enabled: true
```

Use `runner.sandbox_reuse_mode` rather than adding a separate `reuse_sandbox`
field. The supported values are `fresh`, `reuse`, and `force_fresh`.

Sourcing `.env` is still acceptable for secrets such as database and provider
credentials, but not for live-run gate or sandbox-reuse policy:

```bash
set -a; source .env; set +a
uv run pytest -q -x --tb=short --durations=20 \
  backend/src/task_center_runner/tests/mock/sandbox/capacity/test_full_system_capacity_matrix.py
```

If a heavy or capacity test skips unexpectedly, inspect `ephemeralos.yaml` and
`backend/src/task_center_runner/tests/_live_config.py` before changing the
command line.

## Autonomous Run Loop

While tests run, operate an artifact-backed monitoring loop instead of waiting
for pytest summaries. Repeat the loop every 30 seconds until the run finishes or
the first actionable failure appears.

1. Resolve the active run directory. Prefer the newest `run.json` under
   `.sweevo_runs/scenario_logs`.

```bash
RUN_DIR="$(find .sweevo_runs/scenario_logs -maxdepth 3 -name run.json | sort | tail -1 | xargs dirname)"
printf 'RUN_DIR=%s\n' "$RUN_DIR"
```

2. Poll run state and progress counters.

```bash
jq '{status, started_ts, finished_ts, scenario_name, sandbox_id}' "$RUN_DIR/run.json"
test -f "$RUN_DIR/message.jsonl" && wc -l "$RUN_DIR/message.jsonl"
test -f "$RUN_DIR/sandbox_events.jsonl" && wc -l "$RUN_DIR/sandbox_events.jsonl"
```

3. Inspect the newest activity.

```bash
test -f "$RUN_DIR/message.jsonl" && tail -n 40 "$RUN_DIR/message.jsonl"
test -f "$RUN_DIR/sandbox_events.jsonl" && tail -n 80 "$RUN_DIR/sandbox_events.jsonl"
```

4. Search for stop signals before the suite completes.

```bash
test -f "$RUN_DIR/sandbox_events.jsonl" && \
  rg -n "internal_error|manifest references missing layer|stale lowerdir|untyped conflict|mount_failed|import failure|remount failure" "$RUN_DIR/sandbox_events.jsonl"
test -f "$RUN_DIR/message.jsonl" && \
  rg -n "failed|cancelled|internal_error|Traceback|TimeoutError" "$RUN_DIR/message.jsonl"
```

5. When a performance report appears, summarize it immediately.

```bash
test -f "$RUN_DIR/performance_report.json" && \
  python3 .agents/skills/sandbox-performance-evaluation/scripts/summarize_sandbox_perf.py "$RUN_DIR"
```

Healthy progress means `run.json` is still running, `message.jsonl` or
`sandbox_events.jsonl` line counts advance between loops, and tool calls are not
stuck outstanding. Stop the active pytest run and diagnose the first actionable
signal when any of these happen:

- `run.json` or task artifacts show failed, cancelled, or no forward progress.
- `message.jsonl` stops advancing while tool calls are outstanding.
- `sandbox_events.jsonl` repeats internal errors, stale lowerdirs, missing layers, untyped conflicts, mount failures, import failures, or remount failures.
- `performance_report.json` shows incomplete tool calls, high error rate outside expected synthetic conflicts, or a clear latency step-up.
- Resource metrics show workspace copies in namespace mode or upperdir/scratch growth proportional to repository size.

After every fix, rerun the narrowest scenario that exposed it, inspect artifacts, then resume the broader sweep.

## Artifact Analysis

Use the bundled summarizer for compact evidence:

```bash
python3 .agents/skills/sandbox-performance-evaluation/scripts/summarize_sandbox_perf.py \
  .sweevo_runs/scenario_logs/<scenario>/<run>
```

Inspect these files directly when needed:

- `run.json`: status, scenario, timestamps, sandbox id.
- `task.json`: task terminal state and failure detail.
- `message.jsonl`: live progress and repeated tool errors.
- `sandbox_events.jsonl`: low-level timings, resource snapshots, conflict events.
- `metrics.json`: scenario-level counters.
- `performance_report.json` or `.md`: per-tool call speed, slow calls, error totals, sandbox timings.
- isolated_workspace daemon audit JSONL:
  `/tmp/sandbox_isolated_workspace_events.jsonl` inside the SWE-EVO container,
  read through the test fixture or `raw_exec`. The path can be overridden with
  `EOS_ISOLATED_WORKSPACE_AUDIT_PATH`.

Key timing fields:

- command execution: `command_exec.mount_workspace_s`, `command_exec.run_command_s`, `command_exec.capture_upperdir_s`, `command_exec.total_s`, `api.shell.total_s`.
- layerstack: `layer_stack.prepare_workspace_snapshot.total_s`, `layer_stack.publish.total_s`, `layer_stack.transaction.lock_wait_s`, `layer_stack.transaction.lock_held_s`.
- OCC: `occ.apply.total_s`, `occ.serial.queue_wait_s`, `occ.apply.commit_queue_wait_s`, `occ.apply.commit_worker_s`.
- file APIs: `api.read.lease_acquire_s`, `api.write.lease_acquire_s`, `api.edit.lease_acquire_s`, `api.write.total_s`, `api.edit.total_s`.
- LSP: `lsp.total_s`, `lsp.<op>.body_s`, `lsp.session.start_count_delta`, `lsp.session.refresh_count_delta`, `lsp.session.remount_count_delta`, `lsp.session.private_overlay_namespace`, `lsp.session.has_overlay_handle`.

Key resource fields (per-op, shown under `resource_max` in the summary):

- O(1) overlay disk: `resource.command_exec.workspace_tree_exists` should be `0` and `resource.command_exec.workspace_tree_bytes` should be `0` in private namespace mode.
- Per-op writes: `resource.command_exec.upperdir_tree_bytes` should scale with changed paths, not repository size or concurrency.
- Scratch: `resource.command_exec.run_dir_tree_bytes` and scratch filesystem used bytes should stay small and transient.
- Layer depth: `resource.layer_stack.manifest_depth` and `resource.layer_stack.manifest_path_count` should stay below operational squash targets.

Isolated workspace audit fields:

- enter: `total_ms`, `phases_ms.prepare_snapshot`,
  `phases_ms.spawn_ns_holder`, `phases_ms.open_ns_fds`,
  `phases_ms.install_veth`, `phases_ms.mount_overlay`,
  `phases_ms.configure_dns`, `phases_ms.create_cgroup`,
  `lowerdir_layer_count`, `materialize`, `ns_ip`, `rfc1918_egress_mode`.
- tool call: `total_ms`, `duration_s`, `argv0`, `exit_code`,
  `phases_ms.unfreeze`, `phases_ms.exec`, `phases_ms.freeze`.
- exit: `total_ms`, `lifetime_s`, `upperdir_bytes_discarded`,
  `phases_ms.kill_holder`, `phases_ms.teardown_veth`,
  `phases_ms.release_snapshot`, `phases_ms.cgroup_rmdir`,
  `phases_ms.rmtree_scratch`.
- eviction/GC: `reason`, `kind`, `identifier`, `released`, `total_ms`,
  `phases_ms.discover`, `phases_ms.reap`.

Isolated workspace performance standards:

- Every emitted `phases_ms` map passes SUBSET-COVER and contains only
  documented keys for that event type.
- `sandbox_isolated_workspace_enter` appears once per successful enter and
  carries distinct `handle_id`s under concurrent N=5 enter.
- `install_veth` contention in Tier 8 stays within `max <= 5 * median`.
- `upperdir_bytes_discarded` reflects discard size, and re-entering the same
  agent after exit must not see the previous upperdir contents.
- Lowerdir paths remain shared across concurrent handles; lowerdir disk usage
  is O(1), while upperdir/scratch usage scales with each handle's writes.
- The isolated path must not produce OCC publish events during full
  enter/tool/exit cycles.

### Cgroup counters — lifetime vs run delta

`resource.cgroup.*` values are **monotonic cumulative counters** maintained by
the Linux kernel against the sandbox cgroup. In `sandbox_reuse_mode: reuse`,
the same Daytona sandbox is shared across many test sessions, so the raw
value at any sample point is **sandbox lifetime since cgroup creation**, not
this test's contribution. Always read the run delta first; only consult the
lifetime value when watching for hard limits (memory.max, disk quota).

The summarizer splits this for you:

- `cgroup_run_delta`: last_sample − first_sample for this run. The right
  number for "how much did this test write / use".
- `cgroup_lifetime`: end-of-run cumulative value. Useful only as a sanity
  check against cgroup `memory.max` or storage quotas.

Memory fields (`memory_current_bytes`, `memory_peak_bytes`) are gauges, not
counters. Read the `cgroup_lifetime` peak as "highest observed in-flight
resident memory" — it does not accumulate across test sessions, so it is
already meaningful as an absolute.

Healthy SWE-EVO-sandbox baselines (image already loads conda + dask +
Pyright):

- `memory_peak_bytes` (lifetime): **1.0-1.5 GB** baseline. Concerning above
  4 GB or when growing run-over-run on stable workloads.
- `io_wbytes` (run delta): scales with on-disk workload. Expect
  **~5× write amplification** over the dd payload because overlay scratch
  staging, OCC commit copy, and the audit DB all write to the same cgroup.
  (e.g. heavy_io_zoned writes ~165 MB of raw dd payload across 15 shells
  and reports ~520 MB run-delta `io_wbytes` — within band.)
- `cpu_usage_usec` (run delta): scales with shell body CPU plus daemon
  overhead. Expect tens of seconds of CPU on a multi-minute heavy run.

## Interpretation Rules

- If `layer_stack.prepare_workspace_snapshot.total_s` is low but tool latency is high, do not blame layerstack. Split mount, command body, OCC queue, LSP body, and provider/runtime overhead.
- If high concurrency slows writes, check OCC queue timing before blaming overlay mount.
- If shell calls are slow but `command_exec.mount_workspace_s` is small, the bottleneck is usually command body, process/runtime scheduling, or CPU contention.
- If LSP gets slow after writes, compare `lsp.session.start_count_delta` with `remount_count_delta`. Repeated restarts are a correctness/performance regression; remounts are expected.
- If `layer_stack.materialize_s` is nonzero in a private namespace run, verify whether the code fell back from mount(2) overlay to materialized/copy-backed mode.
- If workspace tree bytes are nonzero in namespace mode, treat it as a possible O(1) disk regression.
- Expected synthetic OCC conflicts must be typed conflicts, not internal errors. Count them separately from correctness failures.
- Never quote `cgroup_lifetime` `io_*bytes` as "this test wrote X GB" in a report. Quote the `cgroup_run_delta` instead, and only mention lifetime if the sandbox is approaching a quota or limit.
- For isolated_workspace, do not expect `.sweevo_runs/scenario_logs` for the
  direct pytest-tier tests. Use the daemon JSONL audit file plus pytest
  failure output as the primary evidence. Scenario logs still apply when an
  isolated workspace issue is exercised through a broader scenario harness.
- If isolated_workspace full live passes one-at-a-time but fails in the
  combined non-soak run, inspect daemon lifetime artifacts first: open handles,
  zombie `ns_holder`/`unshare` processes, veth count, cgroup directories, and
  `manager.json`. Treat accumulation as a cleanup or test-isolation bug, not a
  proof that the individual correctness invariant failed.

## Report Shape

Final reports should include:

- Exact commands run.
- Exact scenario log directories inspected.
- Pass/fail status and whether failures were expected synthetic conflicts.
- Per-tool latency summary, especially mean/p95/max for shell, write/edit/read, plugin, and LSP tools.
- Sandbox-operation timing evidence for mount, layerstack prepare/publish, OCC apply/queue, and LSP remount/restart behavior.
- Disk evidence for workspace tree bytes, upperdir bytes, scratch bytes, and manifest depth.
- CPU/memory/IO evidence from `cgroup_run_delta` (per-run) and `cgroup_lifetime` (sandbox-lifetime) — never conflate the two.
- Fixes made, targeted verification after each fix, and the broader rerun result.
