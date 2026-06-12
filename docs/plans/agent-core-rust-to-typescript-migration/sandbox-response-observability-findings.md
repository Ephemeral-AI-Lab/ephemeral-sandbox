# Sandbox Response Observability Findings

Date: 2026-06-12

Scope: read-only scan of `sandbox/crates` for response-visible logging, tracing, audit, timing, CPU, disk, memory, mount, LayerStack, and overlay status surfaces.

## Summary

The sandbox Rust crates do not currently use first-party structured tracing dependencies or macros such as `tracing`, `log`, `env_logger`, `slog`, or OpenTelemetry. The durable observation surface is mostly the JSON operation response: `timings`, mutation/audit fields, command status, checkpoint metrics, isolated-workspace lifecycle fields, and plugin status/result fields.

The practical migration implication is that a TypeScript-side host or compatibility layer should preserve these response fields before adding any new tracing layer. OpenTelemetry or structured logs would be additive, not a replacement for the existing response/audit contract.

## Response Timing Injection

All daemon responses that contain a `timings` object are finalized through `sandbox/crates/eos-daemon/src/dispatch/dispatcher.rs`.

The dispatcher appends:

| Field | Meaning |
|---|---|
| `runtime.boot_to_dispatch_s` | Daemon uptime when the request reached dispatch. |
| `runtime.dispatch_s` | Dispatch and op handling time. |
| `runtime.read_request_s` | Request-read duration supplied by dispatch context, or `0.0` when absent. |

Source: `eos-daemon/src/dispatch/dispatcher.rs`.

## Command Responses

Command session elapsed time originates in `sandbox/crates/eos-command-session/src/session.rs` as `ReapedCommand.elapsed_s`. `sandbox/crates/eos-operation/src/command/service.rs` carries it into command settlement, and `sandbox/crates/eos-operation/src/command/settle.rs` renders the final response.

Terminal command responses expose:

| Field | Producer |
|---|---|
| `status` | `running`, `ok`, `cancelled`, `error`, or `timed_out` from command status mapping. |
| `exit_code` | Process or runner exit code. |
| `output.stdout` / `output.stderr` | PTY transcript / response output. |
| `workspace` | `ephemeral` or `isolated` when settled. |
| `success` | Combined command and publish outcome. |
| `changed_paths` / `changed_path_kinds` | Published or private write-set paths. |
| `mutation_source` | `overlay_capture`, `isolated_workspace`, or empty on discarded/cancelled paths. |
| `conflict` / `conflict_reason` | OCC or edit conflict details. |
| `timings.command_exec.total_s` | Command elapsed time. |
| `timings.api.exec_command.dispatch_total_s` | Same elapsed value for dispatch compatibility. |
| `timings.api.exec_command.total_s` | Included for isolated command settlement. |
| `timings.command_exec.capture_upperdir_s` | Overlay upperdir capture time. |
| `timings.command_exec.occ_apply_s` | OCC publish time, or `0.0` for isolated no-publish settlement. |

`eos-namespace/src/runner/fresh_ns.rs` records runner-side `workspace.mount_s` and `workspace.tool_s`. Plugin overlay response shaping aliases those to `command_exec.mount_workspace_s` and `command_exec.run_command_s`.

## Resource Timings

Resource gauges and counters are response-visible through `timings`, mainly from `eos-operation/src/command/settle.rs` and the daemon helper `eos-daemon/src/runtime/response.rs`.

| Category | Fields |
|---|---|
| Changed paths | `resource.command_exec.changed_path_count` |
| LayerStack shape | `resource.layer_stack.manifest_depth`, `resource.layer_stack.manifest_path_count` |
| Upperdir/run-dir/workspace tree | `resource.command_exec.{run_dir,workspace,upperdir}_tree_exists`, `_bytes`, `_file_count`, `_dir_count`, `_entry_count`, `_truncated` |
| Cgroup CPU | `resource.cgroup.cpu_*` from `/sys/fs/cgroup/cpu.stat` |
| Cgroup memory | `resource.cgroup.memory_current_bytes`, `resource.cgroup.memory_peak_bytes` |
| Cgroup disk I/O | `resource.cgroup.io_rbytes`, `io_wbytes`, `io_rios`, `io_wios`, `io_dbytes`, `io_dios` |
| Daemon process memory | `resource.process.rss_bytes`, `resource.process.max_rss_bytes` from `/proc/self/status` |

Direct file APIs are enriched at the daemon boundary in `eos-daemon/src/op_adapter/files.rs`, so `sandbox.file.read`, `sandbox.file.write`, and `sandbox.file.edit` can expose the same resource gauges for direct LayerStack routes. Isolated file APIs are intentionally thinner because publication is deferred and private.

## LayerStack And OCC Status

OCC commit timing is produced by `sandbox/crates/eos-layerstack/src/commit/worker.rs` and is copied into command, file, plugin overlay, and callback responses.

| Field | Meaning |
|---|---|
| `occ.commit.total_s` | Total OCC commit worker time. |
| `occ.commit.validate_groups_s` | Conflict validation time. |
| `occ.commit.publish_layer_s` | Layer publication time. |
| `occ.commit.gated_path_count` | Paths that required OCC gating. |
| `occ.commit.direct_path_count` | Paths routed direct. |

Auto-squash timings are also response-visible when squashing runs:

| Field | Meaning |
|---|---|
| `layer_stack.auto_squash.total_s` | Squash duration. |
| `layer_stack.auto_squash.max_depth` | Configured squash target. |
| `layer_stack.auto_squash.depth_before` | Depth before squash. |
| `layer_stack.auto_squash.depth_after` | Depth after squash, when successful. |
| `layer_stack.auto_squash.manifest_version` | Squashed manifest version. |
| `layer_stack.auto_squash.raced` | Set to `1.0` when squash lost a race. |

There is no separate runtime `OVL_MAX_STACK_GUARD` response path in the scanned code. Current runtime visibility is through manifest depth, auto-squash timings, storage metrics, and kernel/overlay mount errors when the actual mount boundary fails.

## Checkpoint And Storage Metrics

`sandbox.checkpoint.layer_metrics` is implemented in `eos-daemon/src/op_adapter/checkpoint.rs` and returns top-level metrics, not `timings` keys:

| Field | Meaning |
|---|---|
| `manifest_version` | Active manifest version. |
| `manifest_depth` | Active manifest depth. |
| `active_leases` / `leased_layers` | Snapshot lease pressure. |
| `layer_dirs` / `staging_dirs` | Storage directory counts. |
| `referenced_layers` | Active manifest layer count. |
| `storage_bytes` | Total LayerStack storage bytes. |
| `workspace_bound` / `workspace_root` / `base_root_hash` | Workspace binding status. |
| `occ_runtime_service_cache` | Service cache counters including hits, misses, creates, evictions, and lock wait timings. |

`sandbox.checkpoint.commit_to_git` returns:

| Field | Meaning |
|---|---|
| `worktree_mode` | `"overlay"` when the overlay worktree path is used, `"projection"` on fallback. |
| `timings.api.commit_to_git.overlay_mount_s` | Overlay mount time when overlay mode succeeds. |
| `timings.api.commit_to_git.project_worktree_s` | Projection time when fallback mode is used. |
| `timings.api.commit_to_git.git_add_s` | Git add duration. |
| `timings.api.commit_to_git.git_diff_cached_s` | Cached diff check duration. |
| `timings.api.commit_to_git.git_commit_s` | Git commit duration when a commit is created. |
| `timings.api.commit_to_git.total_s` | Total commit pipeline duration. |
| `timings.resource.layer_stack.manifest_depth` | Snapshot depth used for commit. |
| `timings.resource.layer_stack.manifest_path_count` | Snapshot layer-path count. |

## Isolated Workspace Status And Teardown

Isolated workspace responses are shaped in `eos-daemon/src/op_adapter/isolation.rs`; lifecycle work is in `eos-workspace/src/isolated_workspace/manager/lifecycle.rs`.

Enter/status responses expose:

| Field | Meaning |
|---|---|
| `open` | Status response open/closed state. |
| `manifest_version` / `manifest_root_hash` | Pinned LayerStack snapshot. |
| `workspace_handle_id` | Isolated workspace handle. |
| `workspace_root` | Visible workspace root. |
| `created_at` / `last_activity` | Monotonic lifecycle timestamps. |

Exit responses expose:

| Field | Meaning |
|---|---|
| `evicted_upperdir_bytes` | Discarded private upperdir byte count. |
| `lifetime_s` | Handle lifetime. |
| `total_ms` | Exit operation duration. |
| `phases_ms` | Teardown phase durations such as `kill_holder`, `teardown_veth`, `cgroup_rmdir`, and `rmtree_scratch`. |
| `inspection` | Post-teardown audit object. |

The inspection object includes registration status, holder PID, namespace FD count, readiness/control FD flags, veth names, cgroup path/existence, scratch/upperdir/workdir existence after cleanup, `mountinfo_reference_count_after`, `lease_released`, and `active_leases_after`.

Isolated command and file tool responses also carry `workspace: "isolated"`, `mutation_source: "isolated_workspace"`, and `published: false` under isolated metadata. These are the response-visible no-publish audit fields.

## Plugin Status And Plugin Overlay

`sandbox.plugin.ensure` and `sandbox.plugin.status` parse request-side `audit` fields (`invocation_id`, `caller`, `caller_id`) but do not serialize those audit fields back into output.

Plugin status outputs include:

| Field | Meaning |
|---|---|
| `loaded_plugins` | Loaded plugin registry view. |
| `running_service_processes` | Process snapshots with PID, process group, running flag, exit status, and socket path. |
| `connected_ppc_routes` / `connected_ppc_services` | Connected PPC surfaces. |
| `setup_failures` | Stored setup failures. |
| `service_health` | Probe status, accepted flag, manifest key, error, and teardown error. |

Registered plugin ops that use oneshot overlay get the richer mutation response from `eos-daemon/src/op_adapter/plugin.rs` and `eos-daemon/src/runtime/response.rs`:

| Field | Meaning |
|---|---|
| `mutation_source` | Literal `"plugin_overlay"`. |
| `plugin_result` | Worker-provided result object. |
| `plugin_overlay.changed_paths` | Paths observed from the overlay. |
| `plugin_overlay.published_manifest_version` | Published manifest version from OCC. |
| `plugin_overlay.worker_exit_code` | Worker exit code. |
| `status` | `committed` or `failed` after worker/OCC synthesis. |
| `timings.layer_stack.acquire_snapshot.total_s` | Snapshot lease acquisition time. |
| `timings.command_exec.capture_upperdir_s` | Overlay capture time. |
| `timings.command_exec.occ_apply_s` | OCC publish time. |
| `timings.command_exec.total_s` | End-to-end plugin overlay dispatch time. |
| `stdout` / `stderr` / `warnings` | Runner shell fields spliced onto the response. |

Plugin OCC callbacks return file-level audit/status details in the callback reply body: `files[].path`, `files[].status`, `files[].message`, `published_manifest_version`, and `timings`.

## Logging And Persistence

The scanned sandbox crates do not have structured log instrumentation. The log-like persisted surfaces are:

| Surface | Source |
|---|---|
| Daemon spawn log file | `eosd/src/daemon.rs` redirects daemon stdout/stderr to `--log-file` when provided. |
| PTY transcript | `eos-command-session/src/process.rs` writes timestamp-prefixed command output to a transcript file. |
| Final command response | `eos-command-session/src/session.rs` writes pretty JSON final response for crash recovery. |
| CLI stdout/stderr | `eosd`, `eos-sandbox-gateway`, and e2e helpers use limited `println!` / `eprintln!` for version, usage, and process messages. |

## TypeScript Migration Notes

Preserve the response-visible contract first:

1. Keep `timings` keys stable for command, file, checkpoint, plugin overlay, runtime ready, and dispatcher responses.
2. Keep mutation/audit fields stable: `status`, `success`, `workspace`, `published`, `mutation_source`, `changed_paths`, `changed_path_kinds`, `conflict`, and `conflict_reason`.
3. Keep resource gauges explicitly best-effort: cgroup and `/proc/self/status` fields are absent off Linux or when the kernel file is unavailable.
4. Keep `sandbox.checkpoint.layer_metrics` top-level metrics distinct from per-op `timings`.
5. Treat OpenTelemetry or structured tracing as additive instrumentation. It should not replace response metadata, OCC result accounting, isolated teardown inspection, or LayerStack metrics.
6. Keep overlay health represented through `worktree_mode`, `workspace.mount_s`, `api.commit_to_git.overlay_mount_s`, manifest depth, storage bytes, and auto-squash telemetry rather than reintroducing a separate pre-mount overlay depth guard.

## Verification Anchors

Existing live E2E tests assert these response surfaces:

| Area | Test file |
|---|---|
| Command timing and changed paths | `sandbox/crates/eos-e2e-test/tests/ephemeral_workspace/test_ephemeral_workspace_overlay_exec.rs` |
| Cgroup/process memory gauges | `sandbox/crates/eos-e2e-test/tests/pressure/test_pressure_resource_report.rs` |
| Commit-to-git overlay mode and timing | `sandbox/crates/eos-e2e-test/tests/eos-layerstack/test_eos_layerstack_git_overlay_commit.rs` |
| Isolated no-publish and teardown audit | `sandbox/crates/eos-e2e-test/tests/workspace-runtime-isolated/readme.md` |
| LayerStack depth/storage metrics | `sandbox/crates/eos-e2e-test/tests/eos-layerstack/*` and `sandbox/crates/eos-e2e-test/tests/pressure/*` |
