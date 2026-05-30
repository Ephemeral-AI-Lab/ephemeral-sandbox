# Module `sandbox` — Class Inventory

> Generated class & field reference. Source of truth is the code under
> `backend/src/sandbox/`. Field/type/default data is extracted directly from
> the AST; one-line purposes come from class docstrings (or, where absent, a
> reviewer summary). This generated inventory is distinct from the hand-curated
> `docs/architecture/` memory layer.

**200 classes across 75 files.**

The sandbox module is the tool-execution environment that agents call through provider-backed APIs for file, shell, search, plugin, and workspace-lifecycle actions, routing writes through a layer-stack plus optimistic-concurrency-control (OCC) commit path so parallel coding agents can mutate a shared workspace safely. Its main class groups are: typed request/result domain models in `shared/models.py` (e.g. `SandboxCaller`, `ReadFile`/`WriteFile`/`EditFile`/`Shell`/`Grep` request-result pairs, `Intent`, and isolated-workspace enter/exit lifecycle results); the OCC layer in `occ/` defining source-tagged mutation intents (`WriteChange`, `EditChange`, `DeleteChange`, `SymlinkChange`), prepared/routed changesets with stable replay ids, file/changeset status enums, and the `Protocol` ports (`OccLayerStackPort`, snapshot reader, commit transaction/publisher) that decouple OCC from storage; and the daemon audit schema (`daemon/audit_schema.py`) supplying per-family typed event sections (layer-stack, OCC, ephemeral/isolated overlay workspace, plugin, background-tool, tool-call, OS-resource) plus their builders and fail-safe emit helpers. Surrounding subpackages (`provider`, `layer_stack`, `overlay`, `ephemeral_workspace`, `isolated_workspace`, `host`, `daemon`, `api`) implement provider selection, manifest/layer storage, the overlay execution pipeline, the two workspace modes, and workspace-routing dispatch on top of these shared contracts.

## Contents

- **`sandbox/api/transport.py`** — `SandboxTransport`, `DaemonSandboxTransport`
- **`sandbox/audit/events.py`** — `IsolatedWorkspaceAuditEvent`
- **`sandbox/daemon/audit_buffer.py`** — `BufferedEvent`, `_LaneCounters`, `_PressureTracker`, `_Snapshot`, `AuditBuffer`
- **`sandbox/daemon/audit_schema.py`** — `DaemonSection`, `LayerStackSection`, `OverlayWorkspaceSection`, `IsolatedWorkspaceSection`, `OccSection`, `PluginSection`, `BackgroundToolSection`, `ToolCallSection`, `OsResourceSection`
- **`sandbox/daemon/occ_runtime_services.py`** — `OccRuntimeServices`
- **`sandbox/daemon/rpc/in_flight.py`** — `InFlightInvocation`, `InFlightInvocationRegistry`
- **`sandbox/daemon/workspace_binding_reader.py`** — `LayerStackBindingReader`
- **`sandbox/daemon/workspace_tool/dispatch.py`** — `LifecycleInProgressError`, `AgentQuiesceState`
- **`sandbox/ephemeral_workspace/events.py`** — `WorkspacePathChange`, `WorkspaceChangeEvent`, `WorkspaceChangeEventBus`, `_WorkspaceChangeSubscriber`
- **`sandbox/ephemeral_workspace/operation_overlay.py`** — `OperationOverlayMixin`
- **`sandbox/ephemeral_workspace/pipeline.py`** — `_PreparedOverlaySnapshot`, `EphemeralPipeline`
- **`sandbox/ephemeral_workspace/plugin/install.py`** — `PluginInstallError`
- **`sandbox/ephemeral_workspace/plugin/op_context.py`** — `WorkspaceProjectionLike`, `PluginOpContext`
- **`sandbox/ephemeral_workspace/plugin/op_registry.py`** — `PluginOpRegistrationError`, `PluginOpConflictError`, `_PendingRegistration`
- **`sandbox/ephemeral_workspace/plugin/overlay_child.py`** — `_PluginOverlayInvocation`, `_MountedPluginProjection`, `_MountedPluginWorkspace`
- **`sandbox/ephemeral_workspace/plugin/projection.py`** — `ProjectionHandle`, `WorkspaceProjection`
- **`sandbox/ephemeral_workspace/plugin/runtime_api.py`** — `PluginEnsureError`, `_LoadedPluginRuntime`
- **`sandbox/ephemeral_workspace/workspace_publish.py`** — `WorkspacePublishMixin`
- **`sandbox/host/chunked_upload.py`** — `RawExecCallable`
- **`sandbox/host/daemon_client.py`** — `_DaemonTcpEndpoint`, `_DaemonDispatchError`, `_DaemonReadinessError`, `_TcpConnectFailed`, `_TcpIoFailed`, `_DaemonExec`
- **`sandbox/isolated_workspace/_control_plane/namespace_runtime.py`** — `_KernelNamespaceRuntime`
- **`sandbox/isolated_workspace/_control_plane/orphan_reaper.py`** — `_NamespaceHolderProcess`, `_OrphanResourceReaperMixin`
- **`sandbox/isolated_workspace/_control_plane/pipeline_registry.py`** — `_JsonlAuditSink`
- **`sandbox/isolated_workspace/_control_plane/types.py`** — `IsolatedWorkspaceError`, `IsolatedWorkspaceAuditSink`, `IsolatedWorkspaceHandle`, `_PipelineConfig`, `_PhaseTimer`, `NamespaceRuntimePort`
- **`sandbox/isolated_workspace/_control_plane/workspace_handle_lifecycle.py`** — `_WorkspaceHandleLifecycleMixin`
- **`sandbox/isolated_workspace/network.py`** — `IsolatedNetworkUnavailable`, `VethAllocation`, `BridgeAddressPool`, `IsolatedNetwork`
- **`sandbox/isolated_workspace/pipeline.py`** — `IsolatedPipeline`
- **`sandbox/layer_stack/changes.py`** — `DigestSink`, `LayerChange`, `PreparedLayerChange`
- **`sandbox/layer_stack/commit_staging.py`** — `CommitStagingArea`
- **`sandbox/layer_stack/layer_index.py`** — `LayerIndex`
- **`sandbox/layer_stack/lease.py`** — `LayerStackLeaseRecord`, `LeaseRegistry`
- **`sandbox/layer_stack/manifest.py`** — `ManifestConflictError`, `LayerRef`, `Manifest`
- **`sandbox/layer_stack/publisher.py`** — `LayerPublisher`
- **`sandbox/layer_stack/squash.py`** — `CheckpointSegment`, `SquashPlan`, `LayerCheckpointSquasher`
- **`sandbox/layer_stack/stack.py`** — `LayerStackSnapshotLease`, `LayerStack`
- **`sandbox/layer_stack/storage_lock.py`** — `_StorageWriterLock`, `StorageWriterLockLease`
- **`sandbox/layer_stack/transaction.py`** — `LayerStackTransaction`
- **`sandbox/layer_stack/view.py`** — `LayerStackStorageError`, `_VisibleLayerEntry`, `MergedView`
- **`sandbox/layer_stack/workspace_base.py`** — `WorkspaceBaseAlreadyExistsError`, `WorkspaceBaseIncompleteError`, `_DirectoryEntry`, `_FileEntry`, `_SymlinkEntry`
- **`sandbox/layer_stack/workspace_binding.py`** — `WorkspaceBindingError`, `WorkspaceBinding`
- **`sandbox/occ/changeset.py`** — `ChangeSource`, `Change`, `WritePayload`, `WriteChange`, `EditChange`, `DeleteChange`, `SymlinkChange`, `OpaqueDirChange`, `FileStatus`, `FileResult`, `ChangesetResult`, `RouteDecision`, `PreparedPathGroup`, `CommitOptions`, `PreparedChangeset`
- **`sandbox/occ/changeset_preparation.py`** — `ChangesetPreparer`
- **`sandbox/occ/client.py`** — `OccClient`
- **`sandbox/occ/commit_queue.py`** — `_WorkItem`, `_StopItem`, `CommitQueue`
- **`sandbox/occ/commit_transaction.py`** — `CommitTransaction`, `_FileSystemLayerChangeStager`
- **`sandbox/occ/content_hashing.py`** — `ContentHasher`
- **`sandbox/occ/gitignore.py`** — `GitignoreMatcher`, `SnapshotGitignoreMatcher`, `PathspecGitignoreOracle`, `SnapshotGitignoreOracle`
- **`sandbox/occ/layer_stack_adapter.py`** — `LayerStackPortAdapter`
- **`sandbox/occ/maintenance.py`** — `MaintenancePolicy`, `_LayerSquashPort`, `AutoSquashMaintenancePolicy`
- **`sandbox/occ/path_staging.py`** — `_StagingRouteProfile`, `_StagedPathState`, `_PathGroupStager`, `DirectStager`, `GatedStager`
- **`sandbox/occ/ports.py`** — `WorkspaceBindingSnapshot`, `LayerSnapshotReader`, `LayerCommitStagingAllocator`, `LayerCommitTransaction`, `LayerCommitPublisher`, `OccLayerStackPort`, `WorkspaceBindingReader`
- **`sandbox/occ/service.py`** — `OccService`
- **`sandbox/overlay/handle.py`** — `OverlayHandle`
- **`sandbox/overlay/kernel_mount.py`** — `MountInputs`
- **`sandbox/overlay/mount_syscalls.py`** — `MountSyscallsUnavailable`
- **`sandbox/overlay/namespace_entrypoint.py`** — `WorkspaceMountMode`, `_OverlayMountRequest`
- **`sandbox/overlay/path_change.py`** — `OverlayPathChange`
- **`sandbox/overlay/writable_dirs.py`** — `OverlayWritableRootUnavailable`, `OverlayWritableDirs`
- **`sandbox/provider/daytona/adapter.py`** — `DaytonaProviderAdapter`
- **`sandbox/provider/daytona/errors.py`** — `DaytonaUnavailableError`, `AsyncDaytonaUnavailableError`
- **`sandbox/provider/daytona/runtime_context.py`** — `DaytonaContextPreparer`
- **`sandbox/provider/docker/adapter.py`** — `DockerProviderAdapter`
- **`sandbox/provider/docker/runtime_context.py`** — `DockerContextPreparer`
- **`sandbox/provider/protocol.py`** — `ProviderAdapter`
- **`sandbox/shared/command_exec_contract.py`** — `CommandExecRequest`, `SnapshotManifest`, `WorkspaceSnapshotLease`, `OCCMutationClient`, `ChangesetResultLike`, `WorkspaceCapturePublishResult`
- **`sandbox/shared/command_exec_policy.py`** — `CommandExecPolicy`
- **`sandbox/shared/edit_apply.py`** — `SearchReplaceError`
- **`sandbox/shared/layer_stack_port.py`** — `LayerStackPort`
- **`sandbox/shared/lease_guard.py`** — `_LeasedHandle`, `LeaseGuard`
- **`sandbox/shared/models.py`** — `Intent`, `SandboxCaller`, `SandboxRequestBase`, `SandboxResultBase`, `ToolCallRequest`, `ConflictInfo`, `GuardedResultBase`, `RawExecResult`, `ReadFileRequest`, `ReadFileResult`, `WriteFileRequest`, `WriteFileResult`, `SearchReplaceEdit`, `EditFileRequest`, `EditFileResult`, `ShellRequest`, `ShellResult`, `GlobRequest`, `GlobResult`, `GrepRequest`, `GrepResult`, `LifecycleError`, `LifecycleResultBase`, `EnterIsolatedWorkspaceRequest`, `EnterIsolatedWorkspaceResult`, `ExitIsolatedWorkspaceRequest`, `ExitIsolatedWorkspaceResult`
- **`sandbox/shared/ordered_lock.py`** — `OrderedLock`
- **`sandbox/shared/timing_keys.py`** — `TimingKey`
- **`sandbox/shared/tool_primitives/cancellation.py`** — `VerbCancellation`, `_NoopCancellation`, `ShellPgrpCancellation`
- **`sandbox/shared/tool_primitives/grep.py`** — `_GrepOptions`
- **`sandbox/shared/tool_primitives/workspace_filesystem.py`** — `_OpenHow`

---

## `sandbox/api/transport.py`

#### `SandboxTransport`  ·  _protocol_  ·  bases: `Protocol`  ·  [L28]

Transport used by public workspace operations to call the sandbox daemon.

<details><summary>Methods (1)</summary>

`call`

</details>

#### `DaemonSandboxTransport`  ·  _class_  ·  [L48]

SandboxTransport implementation backed by the resident daemon.

<details><summary>Methods (1)</summary>

`call`

</details>

---

## `sandbox/audit/events.py`

#### `IsolatedWorkspaceAuditEvent`  ·  _enum_  ·  bases: `str, Enum`  ·  [L60]

Daemon-side isolated-workspace audit event types.

**Enum members**: `ENTER = 'sandbox_isolated_workspace_enter'`, `EXIT = 'sandbox_isolated_workspace_exit'`, `TOOL_CALL = 'sandbox_isolated_workspace_tool_call'`, `EVICTED = 'sandbox_isolated_workspace_evicted'`, `GC_ORPHAN = 'sandbox_isolated_workspace_gc_orphan'`

---

## `sandbox/daemon/audit_buffer.py`

#### `BufferedEvent`  ·  _dataclass_  ·  decorators: `@dataclass`  ·  [L69]

A single buffered audit event with its sequence, lane, encoded size, and payload.

**Fields**

| name | type | default |
|------|------|---------|
| `seq` | `int` |  |
| `lane` | `Lane` |  |
| `encoded_bytes` | `int` |  |
| `payload` | `dict[str, Any]` |  |

#### `_LaneCounters`  ·  _dataclass_  ·  decorators: `@dataclass`  ·  [L84]

Per-lane running totals of events, bytes, and dropped events within the audit buffer.

**Fields**

| name | type | default |
|------|------|---------|
| `events` | `int` | `0` |
| `bytes` | `int` | `0` |
| `dropped` | `int` | `0` |

#### `_PressureTracker`  ·  _dataclass_  ·  decorators: `@dataclass`  ·  [L91]

Edge-triggered pressure cross detector for `daemon.audit_buffer_pressure`.

**Fields**

| name | type | default |
|------|------|---------|
| `threshold` | `float` | `0.8` |
| `above` | `bool` | `False` |

<details><summary>Methods (1)</summary>

`cross_rising`

</details>

#### `_Snapshot`  ·  _dataclass_  ·  decorators: `@dataclass`  ·  [L107]

An immutable point-in-time snapshot of the audit buffer's retention, pressure, and drop metrics.

**Fields**

| name | type | default |
|------|------|---------|
| `retained_events` | `int` |  |
| `retained_bytes` | `int` |  |
| `max_events` | `int` |  |
| `max_bytes` | `int` |  |
| `pressure` | `float` |  |
| `dropped_event_count` | `int` |  |
| `dropped_event_count_by_lane` | `dict[str, int]` |  |
| `lost_before_seq` | `int` |  |
| `next_seq` | `int` |  |
| `boot_epoch_id` | `int` |  |

#### `AuditBuffer`  ·  _class_  ·  [L120]

Bounded in-memory ring buffer with lane-priority eviction.

**Instance attributes**: `_max_events`, `_max_bytes`, `_boot_epoch_id`, `_lock`, `_next_seq`, `_lost_before_seq`, `_dropped_total`, `_lanes`, `_counters`, `_tracker`, `_all`, `_on_pressure_cross`

<details><summary>Methods (14)</summary>

`__init__`, `boot_epoch_id`, `max_events`, `max_bytes`, `register_pressure_cross_callback`, `append`, `pull`, `snapshot`, `_enforce_caps_locked`, `_evict_one_locked`, `_pressure_locked`, `_snapshot_locked`, `_buffer_block`, `_snapshot_block`

</details>

---

## `sandbox/daemon/audit_schema.py`

#### `DaemonSection`  ·  _dataclass_  ·  decorators: `@dataclass`  ·  [L29]

Payload shape for ``daemon.*`` events.

**Fields**

| name | type | default |
|------|------|---------|
| `boot_epoch_id` | `int \| None` | `None` |
| `pid` | `int \| None` | `None` |
| `pressure` | `float \| None` | `None` |
| `retained_events` | `int \| None` | `None` |
| `retained_bytes` | `int \| None` | `None` |

<details><summary>Methods (1)</summary>

`as_dict`

</details>

#### `LayerStackSection`  ·  _dataclass_  ·  decorators: `@dataclass`  ·  [L43]

Payload shape for ``layer_stack.*`` events.

**Fields**

| name | type | default |
|------|------|---------|
| `operation_id` | `str \| None` | `None` |
| `operation_step` | `int \| None` | `None` |
| `lease_id` | `str \| None` | `None` |
| `owner_request_id` | `str \| None` | `None` |
| `manifest_version` | `int \| None` | `None` |
| `manifest_root_hash` | `str \| None` | `None` |
| `layer_count` | `int \| None` | `None` |
| `lease_wait_ms` | `float \| None` | `None` |
| `lock_wait_ms` | `float \| None` | `None` |
| `lease_hold_ms` | `float \| None` | `None` |
| `prepare_snapshot_ms` | `float \| None` | `None` |
| `squash_trigger_reason` | `str \| None` | `None` |
| `squash_input_layers` | `int \| None` | `None` |
| `squash_result_layers` | `int \| None` | `None` |
| `squash_failure_kind` | `str \| None` | `None` |

<details><summary>Methods (1)</summary>

`as_dict`

</details>

#### `OverlayWorkspaceSection`  ·  _dataclass_  ·  decorators: `@dataclass`  ·  [L76]

Payload shape for ``overlay_workspace.*`` events (ephemeral mode).

**Fields**

| name | type | default |
|------|------|---------|
| `operation_id` | `str \| None` | `None` |
| `workspace_mode` | `str` | `'ephemeral'` |
| `workspace_handle_id` | `str \| None` | `None` |
| `lease_id` | `str \| None` | `None` |
| `manifest_root_hash` | `str \| None` | `None` |
| `mount_ms` | `float \| None` | `None` |
| `cleanup_ms` | `float \| None` | `None` |
| `scratch_removed` | `bool \| None` | `None` |
| `cleanup_failure_kind` | `str \| None` | `None` |
| `committed_layer_id` | `str \| None` | `None` |
| `publish_layer_ms` | `float \| None` | `None` |
| `changed_path_count` | `int \| None` | `None` |
| `upperdir_bytes` | `int \| None` | `None` |

<details><summary>Methods (1)</summary>

`as_dict`

</details>

#### `IsolatedWorkspaceSection`  ·  _dataclass_  ·  decorators: `@dataclass`  ·  [L107]

Payload shape for ``isolated_workspace.*`` events.

**Fields**

| name | type | default |
|------|------|---------|
| `operation_id` | `str \| None` | `None` |
| `workspace_mode` | `str` | `'isolated'` |
| `workspace_handle_id` | `str \| None` | `None` |
| `agent_id` | `str \| None` | `None` |
| `holder_pid` | `int \| None` | `None` |
| `holder_pid_alive` | `bool \| None` | `None` |
| `cgroup_id` | `str \| None` | `None` |
| `cgroup_removed` | `bool \| None` | `None` |
| `scratch_removed` | `bool \| None` | `None` |
| `upperdir_bytes` | `int \| None` | `None` |
| `upperdir_cap_bytes` | `int \| None` | `None` |
| `memory_current_bytes` | `int \| None` | `None` |
| `memory_peak_bytes` | `int \| None` | `None` |
| `cpu_usage_usec_delta` | `int \| None` | `None` |
| `orphan_holder_count` | `int` | `0` |
| `orphan_cgroup_count` | `int` | `0` |
| `orphan_scratch_count` | `int` | `0` |
| `sampled_at_monotonic_s` | `float \| None` | `None` |

<details><summary>Methods (1)</summary>

`as_dict`

</details>

#### `OccSection`  ·  _dataclass_  ·  decorators: `@dataclass`  ·  [L143]

Payload shape for ``occ.*`` events.

**Fields**

| name | type | default |
|------|------|---------|
| `operation_id` | `str \| None` | `None` |
| `operation_step` | `int \| None` | `None` |
| `changeset_id` | `str \| None` | `None` |
| `changed_path_count` | `int \| None` | `None` |
| `transaction_lock_wait_ms` | `float \| None` | `None` |
| `prepare_ms` | `float \| None` | `None` |
| `apply_ms` | `float \| None` | `None` |
| `commit_ms` | `float \| None` | `None` |
| `committed_layer_id` | `str \| None` | `None` |
| `publish_layer_ms` | `float \| None` | `None` |
| `committed_layer_bytes` | `int \| None` | `None` |
| `conflict_kind` | `str \| None` | `None` |
| `conflict_path` | `str \| None` | `None` |
| `conflict_reason` | `str \| None` | `None` |
| `base_manifest_version` | `int \| None` | `None` |
| `current_manifest_version` | `int \| None` | `None` |

<details><summary>Methods (1)</summary>

`as_dict`

</details>

#### `PluginSection`  ·  _dataclass_  ·  decorators: `@dataclass`  ·  [L175]

Payload shape for ``plugin.*`` events. Generic — no vendor names.

**Fields**

| name | type | default |
|------|------|---------|
| `plugin_id` | `str` |  |
| `plugin_kind` | `str` |  |
| `plugin_version` | `str \| None` | `None` |
| `plugin_tool_name` | `str \| None` | `None` |
| `request_bytes` | `int \| None` | `None` |
| `response_bytes` | `int \| None` | `None` |
| `duration_ms` | `float \| None` | `None` |
| `status` | `str \| None` | `None` |
| `error_kind` | `str \| None` | `None` |
| `message_hash` | `str \| None` | `None` |
| `workspace_handle_id` | `str \| None` | `None` |
| `agent_id` | `str \| None` | `None` |
| `peak_resident_bytes` | `int \| None` | `None` |

<details><summary>Methods (1)</summary>

`as_dict`

</details>

#### `BackgroundToolSection`  ·  _dataclass_  ·  decorators: `@dataclass`  ·  [L204]

Payload shape for ``background_tool.*`` events.

**Fields**

| name | type | default |
|------|------|---------|
| `background_task_id` | `str` |  |
| `task_kind` | `str \| None` | `None` |
| `tool_name` | `str \| None` | `None` |
| `agent_id` | `str \| None` | `None` |
| `uptime_ms` | `float \| None` | `None` |
| `status` | `str \| None` | `None` |
| `exit_code` | `int \| None` | `None` |
| `duration_ms` | `float \| None` | `None` |
| `error_kind` | `str \| None` | `None` |
| `cancel_reason` | `str \| None` | `None` |
| `delivery_latency_ms` | `float \| None` | `None` |

<details><summary>Methods (1)</summary>

`as_dict`

</details>

#### `ToolCallSection`  ·  _dataclass_  ·  decorators: `@dataclass`  ·  [L233]

Payload shape for ``tool_call.*`` events.

**Fields**

| name | type | default |
|------|------|---------|
| `tool_use_id` | `str` |  |
| `tool_name` | `str` |  |
| `agent_id` | `str \| None` | `None` |
| `workspace_mode` | `str \| None` | `None` |
| `workspace_handle_id` | `str \| None` | `None` |
| `phase` | `str \| None` | `None` |
| `duration_ms` | `float \| None` | `None` |
| `total_ms` | `float \| None` | `None` |
| `exit_status` | `str \| None` | `None` |
| `bytes_in` | `int \| None` | `None` |
| `bytes_out` | `int \| None` | `None` |
| `phase_totals_rollup` | `dict[str, float] \| None` | `field(default=None)` |

<details><summary>Methods (1)</summary>

`as_dict`

</details>

#### `OsResourceSection`  ·  _dataclass_  ·  decorators: `@dataclass`  ·  [L263]

Payload shape for ``os_resource.sampled`` events.

**Fields**

| name | type | default |
|------|------|---------|
| `sampled_at_monotonic_s` | `float` |  |
| `rss_bytes` | `int \| None` | `None` |
| `cpu_user_s` | `float \| None` | `None` |
| `cpu_system_s` | `float \| None` | `None` |
| `cpu_throttled_us` | `int \| None` | `None` |
| `io_read_bytes` | `int \| None` | `None` |
| `io_write_bytes` | `int \| None` | `None` |
| `io_read_ops` | `int \| None` | `None` |
| `io_write_ops` | `int \| None` | `None` |

<details><summary>Methods (1)</summary>

`as_dict`

</details>

---

## `sandbox/daemon/occ_runtime_services.py`

#### `OccRuntimeServices`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L29]

The OCC service bundle shared by every daemon runtime peer.

**Fields**

| name | type | default |
|------|------|---------|
| `layer_stack` | `LayerStackPortAdapter` |  |
| `occ_service` | `OccService` |  |
| `occ_client` | `OccClient` |  |
| `gitignore` | `SnapshotGitignoreOracle` |  |
| `layer_stack_manager` | `LayerStack` |  |

---

## `sandbox/daemon/rpc/in_flight.py`

#### `InFlightInvocation`  ·  _dataclass_  ·  decorators: `@dataclass`  ·  [L21]

Tracks one in-flight daemon RPC invocation's asyncio task and metadata for TTL reaping.

**Fields**

| name | type | default |
|------|------|---------|
| `invocation_id` | `str` |  |
| `task` | `asyncio.Task[object]` |  |
| `agent_id` | `str` |  |
| `op` | `str` |  |
| `last_seen` | `float` |  |
| `background` | `bool` | `False` |
| `ttl_reaped` | `bool` | `False` |

#### `InFlightInvocationRegistry`  ·  _class_  ·  [L31]

Tracks daemon-side asyncio tasks by invocation id.

**Instance attributes**: `_ttl_seconds`, `_reaper_interval_s`, `_by_invocation`, `_ttl_reaped_total`, `_reaper_task`

<details><summary>Methods (10)</summary>

`__init__`, `register`, `deregister`, `cancel_task`, `heartbeat`, `count_by_agent`, `metrics`, `ttl_reaper_loop`, `reap_stale`, `_ensure_reaper_started`

</details>

---

## `sandbox/daemon/workspace_binding_reader.py`

#### `LayerStackBindingReader`  ·  _class_  ·  [L18]

Binding reader that fails closed before layer-stack OCC dispatch.

<details><summary>Methods (1)</summary>

`require_workspace_binding`

</details>

---

## `sandbox/daemon/workspace_tool/dispatch.py`

#### `LifecycleInProgressError`  ·  _exception_  ·  bases: `Exception`  ·  [L48]

Raised when a dispatch arrives while ``exit_isolated_workspace`` drains.

**Instance attributes**: `agent_id`

<details><summary>Methods (1)</summary>

`__init__`

</details>

#### `AgentQuiesceState`  ·  _dataclass_  ·  decorators: `@dataclass`  ·  [L65]

Per-agent quiesce state for daemon RPC paths that observe routing.

**Fields**

| name | type | default |
|------|------|---------|
| `entry_lock` | `OrderedLock` | `field(default_factory=lambda: OrderedLock('entry_lock'))` |
| `inflight` | `int` | `0` |
| `inflight_zero` | `asyncio.Event` | `field(default_factory=asyncio.Event)` |
| `exit_pending` | `bool` | `False` |

<details><summary>Methods (1)</summary>

`__post_init__`

</details>

---

## `sandbox/ephemeral_workspace/events.py`

#### `WorkspacePathChange`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L20]

Describes a single workspace path mutation (write, delete, symlink, or opaque dir).

**Fields**

| name | type | default |
|------|------|---------|
| `path` | `str` |  |
| `kind` | `Literal['write', 'delete', 'symlink', 'opaque_dir']` |  |
| `existed_before` | `bool` |  |

<details><summary>Methods (1)</summary>

`from_overlay_change`

</details>

#### `WorkspaceChangeEvent`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L35]

A versioned workspace change notification bundling a reason and its set of path changes.

**Fields**

| name | type | default |
|------|------|---------|
| `reason` | `WorkspaceChangeReason` |  |
| `from_version` | `int` |  |
| `to_version` | `int` |  |
| `changes` | `tuple[WorkspacePathChange, ...]` | `()` |

#### `WorkspaceChangeEventBus`  ·  _class_  ·  [L42]

Small bounded fanout bus for daemon-local workspace change events.

**Instance attributes**: `_subscribers`

<details><summary>Methods (4)</summary>

`__init__`, `subscribe`, `unsubscribe`, `emit`

</details>

#### `_WorkspaceChangeSubscriber`  ·  _class_  ·  [L68]

Per-subscriber bounded queue that buffers workspace-change events, collapsing to a full-resync event when the queue overflows.

**Instance attributes**: `queue`

<details><summary>Methods (2)</summary>

`__init__`, `put`

</details>

---

## `sandbox/ephemeral_workspace/operation_overlay.py`

#### `OperationOverlayMixin`  ·  _class_  ·  [L13]

Mixin giving the pipeline overlay-acquisition support and attaching resource-timing metrics to overlay-backed tool-call results.

<details><summary>Methods (2)</summary>

`_attach_resource_timings`, `acquire_operation_overlay`

</details>

---

## `sandbox/ephemeral_workspace/pipeline.py`

#### `_PreparedOverlaySnapshot`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L49]

Internal record bundling a snapshot's lease id, manifest, and layer paths prepared for overlay use.

**Fields**

| name | type | default |
|------|------|---------|
| `lease_id` | `str` |  |
| `manifest` | `SnapshotManifest` |  |
| `layer_paths` | `tuple[Path, ...]` |  |

#### `EphemeralPipeline`  ·  _class_  ·  bases: `OperationOverlayMixin, WorkspacePublishMixin`  ·  [L55]

Facade hiding overlay freshness, capture, and OCC behind the daemon boundary.

**Instance attributes**: `_occ_client`, `_workspace_ref`, `_layer_stack`, `_workspace_root`, `event_bus`, `_active_manifest_key`, `_active_manifest_version`, `_mounted`, `_active_lease_id`, `_operation_lock`, `_shell_mount_maintenance_lock`, `_foreign_watch_task`, `_lease_guard`, `_writable_root`, `_runtime_dir_path`, `_upperdir`, `_workdir`

<details><summary>Methods (29)</summary>

`__init__`, `workspace_root`, `is_mounted`, `upperdir`, `writable_root`, `runtime_dir`, `workspace_operation`, `run_tool_call`, `_attach_operation_timing_aliases`, `_run_shell_pre_mount_maintenance`, `active_manifest_key`, `current_manifest`, `start`, `stop`, `subscribe_workspace_changes`, `unsubscribe_workspace_changes`, `ensure_current`, `_mark_active`, `_manifest_key`, `_prepare_mount_dirs`, `_remount_active`, `_detach_active_mount`, `_mount_active`, `_mount_layer_paths`, `_lease_overlay_snapshot`, `_release_lease`, `_start_foreign_publish_watcher`, `_stop_foreign_publish_watcher`, `_watch_foreign_publishes`

</details>

---

## `sandbox/ephemeral_workspace/plugin/install.py`

#### `PluginInstallError`  ·  _exception_  ·  bases: `RuntimeError`  ·  [L100]

Raised when plugin install fails (upload, setup.sh, or marker write).

**Instance attributes**: `kind`, `plugin_name`, `setup_step`, `command`, `stderr_excerpt`

<details><summary>Methods (1)</summary>

`__init__`

</details>

---

## `sandbox/ephemeral_workspace/plugin/op_context.py`

#### `WorkspaceProjectionLike`  ·  _protocol_  ·  bases: `Protocol`  ·  [L64]

Protocol describing the workspace-projection interface a plugin op context uses to acquire overlays and query the manifest.

<details><summary>Methods (4)</summary>

`layer_stack_root`, `acquire`, `acquire_overlay`, `active_manifest_key`

</details>

#### `PluginOpContext`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L81]

Plugin handler context.

**Fields**

| name | type | default |
|------|------|---------|
| `layer_stack_root` | `str` |  |
| `caller` | `SandboxCaller` |  |
| `projection` | `WorkspaceProjectionLike` |  |
| `overlay` | `Any` |  |
| `intent` | `Intent` | `Intent.READ_ONLY` |
| `metadata` | `dict[str, Any]` | `field(default_factory=dict)` |

---

## `sandbox/ephemeral_workspace/plugin/op_registry.py`

#### `PluginOpRegistrationError`  ·  _exception_  ·  bases: `RuntimeError`  ·  [L59]

Raised when register_plugin_op is invoked from a forbidden module.

#### `PluginOpConflictError`  ·  _exception_  ·  bases: `RuntimeError`  ·  [L63]

Raised when two distinct handlers try to register the same op.

#### `_PendingRegistration`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L68]

Record of a deferred plugin-op handler registration capturing its plugin/op names, handler, intent, and overlay flag.

**Fields**

| name | type | default |
|------|------|---------|
| `plugin_name` | `str` |  |
| `op_name` | `str` |  |
| `handler` | `PluginOpHandler` |  |
| `intent` | `Intent` |  |
| `auto_workspace_overlay` | `bool` | `True` |

---

## `sandbox/ephemeral_workspace/plugin/overlay_child.py`

#### `_PluginOverlayInvocation`  ·  _class_  ·  [L77]

Parses and validates the overlay child-process payload describing a single plugin op invocation's layers, manifest, and caller.

**Instance attributes**: `plugin_name`, `op_name`, `args`, `layer_stack_root`, `workspace_root`, `layer_paths`, `upperdir`, `workdir`, `output_ref`, `manifest_key`, `manifest_version`, `root_hash`, `intent`, `caller`, `metadata`

<details><summary>Methods (1)</summary>

`__init__`

</details>

#### `_MountedPluginProjection`  ·  _class_  ·  [L143]

Read-only LayerStack projection stub exposing an already-mounted plugin overlay's manifest and a no-op lease.

**Instance attributes**: `_invocation`, `layer_stack_root`

<details><summary>Methods (3)</summary>

`__init__`, `active_manifest_key`, `acquire`

</details>

#### `_MountedPluginWorkspace`  ·  _class_  ·  [L164]

Workspace stub for a pre-mounted plugin overlay, returning its fixed manifest without performing real ensure operations.

**Instance attributes**: `_invocation`, `workspace_root`

<details><summary>Methods (5)</summary>

`__init__`, `active_manifest_key`, `ensure_current`, `current_manifest`, `workspace_operation`

</details>

---

## `sandbox/ephemeral_workspace/plugin/projection.py`

#### `ProjectionHandle`  ·  _dataclass_  ·  decorators: `@dataclass`  ·  [L27]

Lease-backed view of the active layer-stack manifest.

**Fields**

| name | type | default |
|------|------|---------|
| `lease_id` | `str` |  |
| `manifest_key` | `str` |  |
| `manifest_version` | `int` |  |
| `root_hash` | `str` |  |
| `manifest` | `object \| None` |  |
| `layer_paths` | `tuple[str, ...] \| None` |  |
| `_layer_stack` | `LayerStack` |  |
| `_released` | `bool` | `False` |

<details><summary>Methods (2)</summary>

`release`, `released`

</details>

#### `WorkspaceProjection`  ·  _class_  ·  [L51]

Layer-stack projection used by stateful plugin runtimes.

**Instance attributes**: `_layer_stack_root`, `_layer_stack`

<details><summary>Methods (6)</summary>

`__init__`, `layer_stack_root`, `acquire`, `acquire_overlay`, `active_manifest_key`, `active_lease_count`

</details>

---

## `sandbox/ephemeral_workspace/plugin/runtime_api.py`

#### `PluginEnsureError`  ·  _exception_  ·  bases: `RuntimeError`  ·  [L55]

Raised when api.plugin.ensure fails to load a plugin runtime.

#### `_LoadedPluginRuntime`  ·  _dataclass_  ·  decorators: `@dataclass`  ·  [L60]

Record tracking a loaded plugin runtime's available operations and content digest in the dispatcher registry.

**Fields**

| name | type | default |
|------|------|---------|
| `ops` | `list[str]` |  |
| `digest` | `str` |  |

---

## `sandbox/ephemeral_workspace/workspace_publish.py`

#### `WorkspacePublishMixin`  ·  _class_  ·  [L34]

Mixin that commits overlay path changes into the workspace and attaches publish timings and changed-path metadata to results.

**Instance attributes**: `_active_manifest_version`, `_active_manifest_key`

<details><summary>Methods (6)</summary>

`_commit_and_attach`, `publish_cycle`, `publish_pending_changes`, `_publish_upperdir`, `run_maintenance_after_publish`, `_apply_workspace_capture`

</details>

---

## `sandbox/host/chunked_upload.py`

#### `RawExecCallable`  ·  _protocol_  ·  bases: `Protocol`  ·  [L13]

Protocol describing an async raw sandbox-exec callable used to upload base64 content in chunks.

<details><summary>Methods (1)</summary>

`__call__`

</details>

---

## `sandbox/host/daemon_client.py`

#### `_DaemonTcpEndpoint`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L52]

Value object holding the host, ports, and auth token for connecting to the sandbox daemon over TCP.

**Fields**

| name | type | default |
|------|------|---------|
| `host` | `str` |  |
| `port` | `int` |  |
| `internal_port` | `int \| None` |  |
| `auth_token` | `str` |  |

#### `_DaemonDispatchError`  ·  _exception_  ·  bases: `RuntimeError`  ·  [L59]

Raised when daemon dispatch fails before typed decoding.

**Instance attributes**: `kind`, `message`, `details`

<details><summary>Methods (1)</summary>

`__init__`

</details>

#### `_DaemonReadinessError`  ·  _exception_  ·  bases: `_DaemonDispatchError`  ·  [L74]

Raised when a relaunched daemon does not become ready.

#### `_TcpConnectFailed`  ·  _exception_  ·  bases: `RuntimeError`  ·  [L78]

Raised when the host cannot connect to the daemon TCP endpoint.

#### `_TcpIoFailed`  ·  _exception_  ·  bases: `RuntimeError`  ·  [L82]

Raised when an established daemon TCP stream fails.

#### `_DaemonExec`  ·  _protocol_  ·  bases: `Protocol`  ·  [L86]

Protocol for an async callable that executes a command in a sandbox.

<details><summary>Methods (1)</summary>

`__call__`

</details>

---

## `sandbox/isolated_workspace/_control_plane/namespace_runtime.py`

#### `_KernelNamespaceRuntime`  ·  _class_  ·  [L65]

Default runtime — calls real Linux kernel syscalls / utilities.

**Instance attributes**: `_holders`, `_grandchildren`

<details><summary>Methods (11)</summary>

`__init__`, `spawn_ns_holder`, `open_ns_fds`, `mount_overlay`, `configure_dns`, `signal_net_ready`, `create_cgroup`, `kill_holder`, `_wait_tracked_holder`, `_wait_untracked_holder`, `run_in_handle`

</details>

---

## `sandbox/isolated_workspace/_control_plane/orphan_reaper.py`

#### `_NamespaceHolderProcess`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L46]

Snapshot of a namespace-holder process's PID, parent, state, and command line.

**Fields**

| name | type | default |
|------|------|---------|
| `pid` | `int` |  |
| `ppid` | `int` |  |
| `state` | `str` |  |
| `comm` | `str` |  |
| `cmdline` | `str` |  |

#### `_OrphanResourceReaperMixin`  ·  _class_  ·  [L54]

Mixin that reaps orphaned isolated-workspace resources and reconciles the IP pool after daemon restart.

<details><summary>Methods (6)</summary>

`reap_startup_orphans`, `_reap_orphans`, `_release_orphan_lease`, `_reap_orphan_cgroup`, `_kill_remaining_pids`, `_reap_orphan_holder_processes`

</details>

---

## `sandbox/isolated_workspace/_control_plane/pipeline_registry.py`

#### `_JsonlAuditSink`  ·  _class_  ·  [L55]

Append-only JSON-line audit sink for iws events.

**Instance attributes**: `_path`

<details><summary>Methods (2)</summary>

`__init__`, `emit`

</details>

---

## `sandbox/isolated_workspace/_control_plane/types.py`

#### `IsolatedWorkspaceError`  ·  _exception_  ·  bases: `Exception`  ·  [L87]

Base class for isolated-workspace lifecycle errors.

**Instance attributes**: `kind`, `details`

<details><summary>Methods (1)</summary>

`__init__`

</details>

#### `IsolatedWorkspaceAuditSink`  ·  _protocol_  ·  bases: `Protocol`  ·  [L99]

Protocol for a sink that emits isolated-workspace audit events.

<details><summary>Methods (1)</summary>

`emit`

</details>

#### `IsolatedWorkspaceHandle`  ·  _dataclass_  ·  decorators: `@dataclass`  ·  [L104]

Per-workspace state. Not a subclass of ``OperationOverlayHandle`` (C1).

**Fields**

| name | type | default |
|------|------|---------|
| `workspace_handle_id` | `str` |  |
| `agent_id` | `str` |  |
| `lease_id` | `str` |  |
| `manifest_version` | `int` |  |
| `manifest_root_hash` | `str` |  |
| `workspace_root` | `str` |  |
| `scratch_dir` | `Path` |  |
| `upperdir` | `Path` |  |
| `workdir` | `Path` |  |
| `ns_fds` | `dict[str, int]` | `field(default_factory=dict)` |
| `holder_pid` | `int` | `0` |
| `readiness_fd` | `int` | `-1` |
| `control_fd` | `int` | `-1` |
| `veth` | `VethAllocation \| None` | `None` |
| `cgroup_path` | `Path \| None` | `None` |
| `created_at` | `float` | `0.0` |
| `last_activity` | `float` | `0.0` |
| `active_calls` | `int` | `0` |

<details><summary>Methods (1)</summary>

`to_persisted`

</details>

#### `_PipelineConfig`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L145]

Environment-derived configuration for the isolated-workspace pipeline's limits, timeouts, and sampling.

**Fields**

| name | type | default |
|------|------|---------|
| `enabled` | `bool` |  |
| `ttl_s` | `float` |  |
| `total_cap` | `int` |  |
| `upperdir_bytes` | `int` |  |
| `memavail_fraction` | `float` |  |
| `setup_timeout_s` | `float` |  |
| `exit_grace_s` | `float` |  |
| `rfc1918_egress` | `Literal['allow', 'deny']` |  |
| `fallback_dns` | `str` |  |
| `sample_interval_s` | `float` | `0.5` |

<details><summary>Methods (1)</summary>

`from_env`

</details>

#### `_PhaseTimer`  ·  _class_  ·  [L188]

Per-operation phase-timing helper (PLAN §14).

**Instance attributes**: `_clock`, `_start`, `_phases`

<details><summary>Methods (4)</summary>

`__init__`, `measure`, `total_ms`, `phases_ms`

</details>

#### `NamespaceRuntimePort`  ·  _protocol_  ·  bases: `Protocol`  ·  [L221]

Kernel-touching operations the isolated-workspace pipeline delegates to.

<details><summary>Methods (8)</summary>

`spawn_ns_holder`, `open_ns_fds`, `mount_overlay`, `configure_dns`, `signal_net_ready`, `create_cgroup`, `kill_holder`, `run_in_handle`

</details>

---

## `sandbox/isolated_workspace/_control_plane/workspace_handle_lifecycle.py`

#### `_WorkspaceHandleLifecycleMixin`  ·  _class_  ·  [L38]

Mixin managing isolated-workspace handle lifecycle: enter, snapshot acquisition, quota checks, and teardown.

<details><summary>Methods (6)</summary>

`enter`, `_wire_handle`, `_rollback_partial`, `exit`, `_post_exit_orphan_check`, `_teardown`

</details>

---

## `sandbox/isolated_workspace/network.py`

#### `IsolatedNetworkUnavailable`  ·  _exception_  ·  bases: `RuntimeError`  ·  [L37]

Linux network primitives (ip / nft / CAP_NET_ADMIN) are not available.

#### `VethAllocation`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L42]

Records a host-side veth name paired with its namespace IPv4 address for an isolated workspace's network link.

**Fields**

| name | type | default |
|------|------|---------|
| `host_name` | `str` |  |
| `ns_ip` | `ipaddress.IPv4Address` |  |

#### `BridgeAddressPool`  ·  _class_  ·  [L47]

Allocates /32s from ``10.244.0.2 - 10.244.0.254``.

**Instance attributes**: `_range`, `_allocated`

<details><summary>Methods (4)</summary>

`__init__`, `reserve`, `allocate`, `free`

</details>

#### `IsolatedNetwork`  ·  _class_  ·  [L78]

Owns ``eos-shared0`` bridge + static nft rules + per-ws veth wiring.

**Instance attributes**: `rfc1918_egress`, `pool`, `_initialized`

<details><summary>Methods (9)</summary>

`__init__`, `initialized`, `initialize`, `install_veth`, `teardown_veth`, `daemon_private_routes`, `_ensure_bridge`, `_install_static_rules`, `_require_tools`

</details>

---

## `sandbox/isolated_workspace/pipeline.py`

#### `IsolatedPipeline`  ·  _class_  ·  bases: `_WorkspaceHandleLifecycleMixin, _OrphanResourceReaperMixin`  ·  [L51]

Owns isolated workspace lifecycle, namespace runtime, capacity, TTL, and GC state.

**Instance attributes**: `_scratch_root`, `_layer_stack`, `_audit`, `_config`, `_network`, `_runtime`, `_clock`, `_id_factory`, `_meminfo_reader`, `_handles`, `_by_agent`, `_map_lock`, `_init_complete`, `_ttl_task`, `_sampler_task`

<details><summary>Methods (22)</summary>

`__init__`, `scratch_root`, `persisted_handles_path`, `_check_host_capacity`, `_compute_host_budget`, `_read_persisted_handles`, `get_handle`, `_require_handle`, `run_tool_call`, `_overlay_handle`, `initialize`, `_ttl_loop`, `_sampler_loop`, `ttl_sweep`, `_emit_isolated_workspace_sample`, `run_in_handle`, `shutdown`, `list_open_agents`, `test_reset`, `_persist`, `_exit_open_agents`, `_emit`

</details>

---

## `sandbox/layer_stack/changes.py`

#### `DigestSink`  ·  _protocol_  ·  bases: `Protocol`  ·  [L23]

Protocol for any incremental hashing target accepting bytes via an update method.

<details><summary>Methods (1)</summary>

`update`

</details>

#### `LayerChange`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L44]

Tagged-union storage-level layer change.

**Fields**

| name | type | default |
|------|------|---------|
| `kind` | `LayerChangeKind` |  |
| `path` | `str` |  |
| `source_path` | `str \| None` | `None` |
| `content_hash` | `str \| None` | `None` |

<details><summary>Methods (1)</summary>

`__post_init__`

</details>

#### `PreparedLayerChange`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L72]

Pairs a layer change with its materialized write content ready for application to storage.

**Fields**

| name | type | default |
|------|------|---------|
| `change` | `LayerChange` |  |
| `write_content` | `bytes \| None` | `None` |

---

## `sandbox/layer_stack/commit_staging.py`

#### `CommitStagingArea`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L14]

Identifies a temporary on-disk staging directory used while committing OCC layer changes.

**Fields**

| name | type | default |
|------|------|---------|
| `staging_id` | `str` |  |
| `path` | `Path` |  |

---

## `sandbox/layer_stack/layer_index.py`

#### `LayerIndex`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L31]

Summarizes a layer's directory contents as sets of files, whiteouts, and opaque directories for overlay resolution.

**Fields**

| name | type | default |
|------|------|---------|
| `files` | `frozenset[str]` |  |
| `whiteouts` | `frozenset[str]` |  |
| `opaque_dirs` | `frozenset[str]` |  |

---

## `sandbox/layer_stack/lease.py`

#### `LayerStackLeaseRecord`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L15]

Represents an active snapshot lease tying a lease ID to the manifest of layers it retains.

**Fields**

| name | type | default |
|------|------|---------|
| `lease_id` | `str` |  |
| `manifest` | `Manifest` |  |

#### `LeaseRegistry`  ·  _class_  ·  [L20]

Tracks active snapshot leases and the layers they retain on disk.

**Instance attributes**: `_id_factory`, `_lock`, `_leases`, `_refcounts`

<details><summary>Methods (6)</summary>

`__init__`, `acquire`, `release`, `leased_layers`, `lease_head_layers`, `active_count`

</details>

---

## `sandbox/layer_stack/manifest.py`

#### `ManifestConflictError`  ·  _exception_  ·  bases: `RuntimeError`  ·  [L18]

Raised when an active-manifest compare-and-swap check fails.

#### `LayerRef`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, order=True)`  ·  [L47]

Validated immutable reference to one overlay layer by id and relative storage path.

**Fields**

| name | type | default |
|------|------|---------|
| `layer_id` | `str` |  |
| `path` | `str` |  |

<details><summary>Methods (3)</summary>

`__post_init__`, `to_dict`, `from_dict`

</details>

#### `Manifest`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L73]

Versioned, schema-checked ordered collection of layer references defining a layer stack's composition.

**Fields**

| name | type | default |
|------|------|---------|
| `version` | `int` |  |
| `layers` | `tuple[LayerRef, ...]` |  |
| `schema_version` | `int` | `MANIFEST_SCHEMA_VERSION` |

<details><summary>Methods (4)</summary>

`__post_init__`, `depth`, `to_dict`, `from_dict`

</details>

---

## `sandbox/layer_stack/publisher.py`

#### `LayerPublisher`  ·  _class_  ·  [L36]

Writes accepted changes into immutable layers and publishes manifests.

**Instance attributes**: `_storage_root`, `_manifest_file`, `_id_factory`

<details><summary>Methods (2)</summary>

`__init__`, `publish_layer`

</details>

---

## `sandbox/layer_stack/squash.py`

#### `CheckpointSegment`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L21]

A contiguous run of two or more layers slated to be squashed into one checkpoint.

**Fields**

| name | type | default |
|------|------|---------|
| `layers` | `tuple[LayerRef, ...]` |  |

<details><summary>Methods (1)</summary>

`__post_init__`

</details>

#### `SquashPlan`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L33]

Plan describing how active layers and checkpoint segments collapse during a squash operation.

**Fields**

| name | type | default |
|------|------|---------|
| `active_version` | `int` |  |
| `active_layers` | `tuple[LayerRef, ...]` |  |
| `entries` | `tuple[_SquashPlanEntry, ...]` |  |

<details><summary>Methods (2)</summary>

`__post_init__`, `checkpoint_segments`

</details>

#### `LayerCheckpointSquasher`  ·  _class_  ·  [L51]

Plans runs between lease heads and projects each run into a checkpoint layer.

**Instance attributes**: `_storage_root`, `_view`

<details><summary>Methods (6)</summary>

`__init__`, `plan`, `build_checkpoint`, `relabel_checkpoint`, `discard_checkpoint`, `_allocate_checkpoint_paths`

</details>

---

## `sandbox/layer_stack/stack.py`

#### `LayerStackSnapshotLease`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L53]

Immutable handle to a leased, pinned manifest snapshot with its layer paths and timings.

**Fields**

| name | type | default |
|------|------|---------|
| `lease_id` | `str` |  |
| `manifest_version` | `int` |  |
| `root_hash` | `str` |  |
| `manifest` | `Manifest` |  |
| `timings` | `dict[str, float]` |  |
| `layer_paths` | `tuple[str, ...]` |  |

<details><summary>Methods (1)</summary>

`to_dict`

</details>

#### `LayerStack`  ·  _class_  ·  [L73]

Coordinates active manifests, snapshot leases, reads, and publishes.

**Instance attributes**: `storage_root`, `_storage_writer_lock`, `_manifest_file`, `_lock`, `_leases`, `_view`, `_publisher`, `_checkpoint_squasher`

<details><summary>Methods (26)</summary>

`__init__`, `read_active_manifest`, `acquire_lease_record`, `acquire_snapshot`, `release_lease`, `leased_layers`, `active_lease_count`, `can_squash`, `read_bytes`, `read_text`, `read_symlink`, `list_dir`, `iter_paths`, `project`, `begin_transaction`, `allocate_commit_staging`, `drop_commit_staging`, `publish_changes`, `squash`, `commit_to_workspace`, `_storage_write_guard`, `_require_storage_writer_lock`, `_layer_path`, `_unreferenced_layers`, `_remove_layers`, `close`

</details>

---

## `sandbox/layer_stack/storage_lock.py`

#### `_StorageWriterLock`  ·  _dataclass_  ·  decorators: `@dataclass`  ·  [L19]

Internal record pairing a file-lock fd, refcount, and mutex for one storage root.

**Fields**

| name | type | default |
|------|------|---------|
| `fd` | `int` |  |
| `refcount` | `int` |  |
| `mutex` | `threading.RLock` |  |

#### `StorageWriterLockLease`  ·  _class_  ·  [L25]

A refcounted lease granting a process-local write mutex for a storage root, auto-released via weakref finalizer.

**Instance attributes**: `_mutex`, `_finalizer`

<details><summary>Methods (3)</summary>

`__init__`, `exclusive`, `close`

</details>

---

## `sandbox/layer_stack/transaction.py`

#### `LayerStackTransaction`  ·  _class_  ·  [L21]

Process-local active-manifest transaction shell.

**Instance attributes**: `_lock`, `_manifest_path`, `_publisher`, `_storage_writer_lock`, `_storage_guard`, `_manifest`, `_entered`, `_lock_acquired_at`, `_lock_held_s`, `_lock_wait_s`

<details><summary>Methods (9)</summary>

`__init__`, `__enter__`, `__exit__`, `snapshot`, `publish_layer`, `lock_wait_s`, `lock_held_s`, `_require_manifest`, `_release_storage_guard`

</details>

---

## `sandbox/layer_stack/view.py`

#### `LayerStackStorageError`  ·  _exception_  ·  bases: `RuntimeError`  ·  [L28]

Raised when a manifest references missing or invalid layer storage.

**Instance attributes**: `layer_id`

<details><summary>Methods (1)</summary>

`__init__`

</details>

#### `_VisibleLayerEntry`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L37]

Associates a layer reference with the filesystem path where its content is visible during a merged view.

**Fields**

| name | type | default |
|------|------|---------|
| `layer` | `LayerRef` |  |
| `path` | `Path` |  |

#### `MergedView`  ·  _class_  ·  [L45]

Reads paths through a frozen manifest without mutating layer state.

**Instance attributes**: `_storage_root`, `_layer_index_cache`

<details><summary>Methods (12)</summary>

`__init__`, `_layer_index`, `evict_layer_index`, `read_bytes`, `read_text`, `read_symlink`, `_visible_entry`, `list_dir`, `iter_paths`, `project`, `_layer_dir`, `_apply_layer`

</details>

---

## `sandbox/layer_stack/workspace_base.py`

#### `WorkspaceBaseAlreadyExistsError`  ·  _exception_  ·  bases: `RuntimeError`  ·  [L35]

Raised when a workspace base is requested for non-empty stack state.

#### `WorkspaceBaseIncompleteError`  ·  _exception_  ·  bases: `RuntimeError`  ·  [L39]

Raised when a full workspace base cannot represent every workspace path.

**Instance attributes**: `special_file_rejections`, `unstable_paths`

<details><summary>Methods (1)</summary>

`__init__`

</details>

#### `_DirectoryEntry`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L58]

Represents a directory entry within a workspace base manifest.

**Fields**

| name | type | default |
|------|------|---------|
| `path` | `str` |  |
| `kind` | `Literal['directory']` | `'directory'` |

#### `_FileEntry`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L64]

Represents a regular file entry in a workspace base manifest, tracking its source, size, and content hash.

**Fields**

| name | type | default |
|------|------|---------|
| `path` | `str` |  |
| `source_path` | `Path` |  |
| `size` | `int` |  |
| `content_hash` | `str` |  |
| `kind` | `Literal['file']` | `'file'` |

#### `_SymlinkEntry`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L73]

Represents a symlink entry in a workspace base manifest, recording its link target.

**Fields**

| name | type | default |
|------|------|---------|
| `path` | `str` |  |
| `link_target` | `str` |  |
| `kind` | `Literal['symlink']` | `'symlink'` |

---

## `sandbox/layer_stack/workspace_binding.py`

#### `WorkspaceBindingError`  ·  _exception_  ·  bases: `RuntimeError`  ·  [L17]

Raised when layer-stack workspace binding state is invalid or missing.

#### `WorkspaceBinding`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, kw_only=True)`  ·  [L22]

Serializable record binding a workspace root to its layer stack with active and base manifest versions and root hashes.

**Fields**

| name | type | default |
|------|------|---------|
| `workspace_root` | `str` |  |
| `layer_stack_root` | `str` |  |
| `active_manifest_version` | `int` |  |
| `active_root_hash` | `str` |  |
| `base_manifest_version` | `int` |  |
| `base_root_hash` | `str` |  |

<details><summary>Methods (4)</summary>

`to_dict`, `from_dict`, `layer_path_from_relative`, `layer_path_from_absolute`

</details>

---

## `sandbox/occ/changeset.py`

#### `ChangeSource`  ·  _enum_  ·  bases: `str, Enum`  ·  [L18]

Enum classifying how a mutation entered OCC (API write, API edit, or overlay capture).

**Enum members**: `API_WRITE = 'api_write'`, `API_EDIT = 'api_edit'`, `OVERLAY_CAPTURE = 'overlay_capture'`

<details><summary>Methods (1)</summary>

`__str__`

</details>

#### `Change`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L28]

Base mutation intent entering OCC.

**Fields**

| name | type | default |
|------|------|---------|
| `path` | `str` |  |
| `source` | `ChangeSource` | `ChangeSource.API_WRITE` |

<details><summary>Methods (1)</summary>

`__post_init__`

</details>

#### `WritePayload`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L40]

Write payload: eager bytes, an on-disk path, or both.

**Fields**

| name | type | default |
|------|------|---------|
| `content` | `bytes \| None` | `None` |
| `content_path` | `str \| None` | `None` |
| `precomputed_hash` | `str \| None` | `None` |

<details><summary>Methods (1)</summary>

`read_bytes`

</details>

#### `WriteChange`  ·  _dataclass_  ·  bases: `Change`  ·  decorators: `@dataclass(frozen=True, kw_only=True)`  ·  [L61]

Whole-file write intent.

**Fields**

| name | type | default |
|------|------|---------|
| `payload` | `WritePayload` |  |
| `base_hash` | `str \| None` | `None` |

<details><summary>Methods (4)</summary>

`final_content`, `content_path`, `precomputed_hash`, `with_base_hash`

</details>

#### `EditChange`  ·  _dataclass_  ·  bases: `Change`  ·  decorators: `@dataclass(frozen=True)`  ·  [L89]

Search/replace edit intent.

**Fields**

| name | type | default |
|------|------|---------|
| `source` | `ChangeSource` | `ChangeSource.API_EDIT` |
| `old_text` | `str \| None` | `None` |
| `new_text` | `str \| None` | `None` |
| `replace_all` | `bool` | `False` |

<details><summary>Methods (1)</summary>

`__post_init__`

</details>

#### `DeleteChange`  ·  _dataclass_  ·  bases: `Change`  ·  decorators: `@dataclass(frozen=True)`  ·  [L109]

Delete intent pinned to a base hash when known.

**Fields**

| name | type | default |
|------|------|---------|
| `base_hash` | `str \| None` | `None` |

<details><summary>Methods (1)</summary>

`with_base_hash`

</details>

#### `SymlinkChange`  ·  _dataclass_  ·  bases: `Change`  ·  decorators: `@dataclass(frozen=True)`  ·  [L126]

Replace path with symlink to target.

**Fields**

| name | type | default |
|------|------|---------|
| `source` | `ChangeSource` | `ChangeSource.OVERLAY_CAPTURE` |
| `target` | `str` | `''` |

<details><summary>Methods (1)</summary>

`__post_init__`

</details>

#### `OpaqueDirChange`  ·  _dataclass_  ·  bases: `Change`  ·  decorators: `@dataclass(frozen=True)`  ·  [L138]

Prune lower-layer children of a directory.

**Fields**

| name | type | default |
|------|------|---------|
| `source` | `ChangeSource` | `ChangeSource.OVERLAY_CAPTURE` |

#### `FileStatus`  ·  _enum_  ·  bases: `str, Enum`  ·  [L144]

Enum of per-file OCC outcomes (accepted, committed, aborted, dropped, rejected, failed).

**Enum members**: `ACCEPTED = 'accepted'`, `COMMITTED = 'committed'`, `ABORTED_VERSION = 'aborted_version'`, `ABORTED_OVERLAP = 'aborted_overlap'`, `DROPPED = 'dropped'`, `REJECTED = 'rejected'`, `FAILED = 'failed'`

#### `FileResult`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L167]

Frozen record of a single path's OCC commit status, message, and timings.

**Fields**

| name | type | default |
|------|------|---------|
| `path` | `str` |  |
| `status` | `FileStatus` |  |
| `message` | `str` | `''` |
| `timings` | `dict[str, float]` | `field(default_factory=dict)` |

#### `ChangesetResult`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L175]

Frozen aggregate result of an OCC changeset commit across all its files.

**Fields**

| name | type | default |
|------|------|---------|
| `files` | `tuple[FileResult, ...]` |  |
| `timings` | `dict[str, float]` | `field(default_factory=dict)` |
| `published_manifest_version` | `int \| None` | `None` |

<details><summary>Methods (1)</summary>

`success`

</details>

#### `RouteDecision`  ·  _enum_  ·  bases: `str, Enum`  ·  [L188]

Enum of how a prepared path is routed during OCC (gated, direct, drop, reject).

**Enum members**: `GATED = 'gated'`, `DIRECT = 'direct'`, `DROP = 'drop'`, `REJECT = 'reject'`

#### `PreparedPathGroup`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L196]

Ordered changes for one normalized path and route decision.

**Fields**

| name | type | default |
|------|------|---------|
| `path` | `str` |  |
| `route` | `RouteDecision` |  |
| `changes` | `tuple[Change, ...]` |  |
| `message` | `str \| None` | `None` |

#### `CommitOptions`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L206]

Request-level OCC commit options.

**Fields**

| name | type | default |
|------|------|---------|
| `atomic` | `bool` | `True` |

#### `PreparedChangeset`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L220]

Routed changeset consumed by the commit transaction.

**Fields**

| name | type | default |
|------|------|---------|
| `snapshot` | `Manifest \| None` |  |
| `path_groups` | `tuple[PreparedPathGroup, ...]` |  |
| `atomic` | `bool` |  |
| `timings` | `dict[str, float]` | `field(default_factory=dict)` |
| `changeset_id` | `str` | `''` |

---

## `sandbox/occ/changeset_preparation.py`

#### `ChangesetPreparer`  ·  _class_  ·  [L33]

Prepare direct and gated path groups for a typed changeset.

**Instance attributes**: `_gitignore`, `_snapshot_gitignore`

<details><summary>Methods (6)</summary>

`__init__`, `prepare_sync`, `_group_by_route`, `_route_change`, `_is_gitignored`, `_prepare_group`

</details>

---

## `sandbox/occ/client.py`

#### `OccClient`  ·  _class_  ·  [L17]

Command-exec-facing client for submitting typed mutation changesets.

**Instance attributes**: `_service`, `_binding_reader`, `_workspace_ref`

<details><summary>Methods (5)</summary>

`__init__`, `_require_binding`, `apply_changeset`, `commit_prepared`, `run_maintenance_after_publish`

</details>

---

## `sandbox/occ/commit_queue.py`

#### `_WorkItem`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L45]

Internal queue entry pairing a prepared changeset with its result future and enqueue time.

**Fields**

| name | type | default |
|------|------|---------|
| `prepared` | `PreparedChangeset` |  |
| `future` | `concurrent.futures.Future[ChangesetResult]` |  |
| `enqueued_at` | `float` |  |

#### `_StopItem`  ·  _class_  ·  [L51]

Sentinel marker enqueued to signal the commit queue worker thread to stop processing.

**Class variables**: `__slots__ = ()`

#### `CommitQueue`  ·  _class_  ·  [L59]

Serialize OCC publish while batching disjoint prepared changesets.

**Instance attributes**: `_transaction`, `_max_batch_size`, `_batch_window_s`, `_max_cas_retries`, `_queue`, `_thread`, `_state_lock`, `_closed`

<details><summary>Methods (9)</summary>

`__init__`, `start`, `close`, `submit`, `apply`, `apply_sync`, `_run`, `_drain_ready`, `_commit_batch`

</details>

---

## `sandbox/occ/commit_transaction.py`

#### `CommitTransaction`  ·  _class_  ·  [L45]

Revalidate prepared OCC path groups and publish one immutable layer.

**Instance attributes**: `_staging`, `_publisher`, `_hasher`, `_direct_stager`, `_gated_stager`

<details><summary>Methods (3)</summary>

`__init__`, `revalidate_and_publish`, `_validate_group`

</details>

#### `_FileSystemLayerChangeStager`  ·  _class_  ·  [L163]

Stages layer changes to a filesystem staging area, hashing and tracking write timing during a commit.

**Instance attributes**: `_staging`, `_hasher`, `_counter`, `_staging_id`, `_staging_path`, `_write_total_s`, `_write_count`

<details><summary>Methods (8)</summary>

`__init__`, `write_total_s`, `write_count`, `staging_path`, `__enter__`, `__exit__`, `write`, `write_from_path`

</details>

---

## `sandbox/occ/content_hashing.py`

#### `ContentHasher`  ·  _class_  ·  [L11]

Hash bytes with the layer-stack OCC hash policy.

<details><summary>Methods (2)</summary>

`hash_bytes`, `hash_current`

</details>

---

## `sandbox/occ/gitignore.py`

#### `GitignoreMatcher`  ·  _protocol_  ·  bases: `Protocol`  ·  [L23]

Small contract consumed by OCC routing.

<details><summary>Methods (1)</summary>

`is_ignored`

</details>

#### `SnapshotGitignoreMatcher`  ·  _protocol_  ·  bases: `GitignoreMatcher, Protocol`  ·  decorators: `@runtime_checkable`  ·  [L30]

Gitignore contract for routing against a known layer-stack snapshot.

<details><summary>Methods (1)</summary>

`is_ignored_in_snapshot`

</details>

#### `PathspecGitignoreOracle`  ·  _class_  ·  [L36]

Pure-Python gitignore evaluator backed by the ``pathspec`` library.

**Instance attributes**: `_workspace_root`, `_read`, `_path_cache`, `_dir_cache`, `_spec_cache`

<details><summary>Methods (7)</summary>

`__init__`, `is_ignored`, `_evaluate_file`, `_is_dir_excluded`, `_match_with_inheritance`, `_spec_for_dir`, `_read_from_disk`

</details>

#### `SnapshotGitignoreOracle`  ·  _class_  ·  [L150]

Evaluate gitignore rules directly from a layer-stack snapshot.

**Instance attributes**: `_snapshot_reader`, `_oracles`, `cache_hits`, `cache_misses`

<details><summary>Methods (5)</summary>

`__init__`, `is_ignored`, `is_ignored_in_snapshot`, `_oracle_for_snapshot`, `_build_pathspec_oracle`

</details>

---

## `sandbox/occ/layer_stack_adapter.py`

#### `LayerStackPortAdapter`  ·  _class_  ·  [L17]

Adapter from the in-process layer-stack manager to OCC/pipeline ports.

**Instance attributes**: `manager`

<details><summary>Methods (12)</summary>

`__init__`, `storage_root`, `read_active_manifest`, `read_bytes`, `read_text`, `begin_transaction`, `allocate_commit_staging`, `drop_commit_staging`, `acquire_snapshot`, `release_lease`, `can_squash`, `squash`

</details>

---

## `sandbox/occ/maintenance.py`

#### `MaintenancePolicy`  ·  _protocol_  ·  bases: `Protocol`  ·  [L15]

Post-publish maintenance hook for OCC service commits.

<details><summary>Methods (1)</summary>

`after_publish_sync`

</details>

#### `_LayerSquashPort`  ·  _protocol_  ·  bases: `Protocol`  ·  [L21]

Layer-stack maintenance capability consumed by auto-squash.

<details><summary>Methods (2)</summary>

`can_squash`, `squash`

</details>

#### `AutoSquashMaintenancePolicy`  ·  _class_  ·  [L29]

Synchronous layer-stack squash after successful publishes.

**Instance attributes**: `_snapshot_reader`, `_squasher`, `_max_depth`, `_audit`, `_squash_lock`

<details><summary>Methods (4)</summary>

`__init__`, `after_publish_sync`, `_run_squash_for_active`, `_emit_audit`

</details>

---

## `sandbox/occ/path_staging.py`

#### `_StagingRouteProfile`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L79]

Route-specific configuration for path-group staging.

**Fields**

| name | type | default |
|------|------|---------|
| `name` | `str` |  |
| `check_hash` | `bool` |  |
| `supports_symlinks` | `bool` |  |
| `missing_file_status` | `FileStatus` |  |
| `timing_read` | `TimingKey` |  |
| `timing_apply` | `TimingKey` |  |
| `timing_stage` | `TimingKey` |  |

#### `_StagedPathState`  ·  _dataclass_  ·  decorators: `@dataclass`  ·  [L113]

Tracks a single path's staged content and final write/delete kind during commit staging.

**Fields**

| name | type | default |
|------|------|---------|
| `content` | `bytes \| None` |  |
| `initial_exists` | `bool` |  |
| `final_kind` | `FinalLayerChangeKind` |  |
| `symlink_target` | `str \| None` | `None` |
| `final_content_path` | `str \| None` | `None` |
| `final_precomputed_hash` | `str \| None` | `None` |

<details><summary>Methods (3)</summary>

`is_present_after`, `materialize_content`, `set_final`

</details>

#### `_PathGroupStager`  ·  _class_  ·  [L148]

Validate and stage one prepared path group, parameterised by route.

**Instance attributes**: `_snapshot_reader`, `_profile`, `_hasher`

<details><summary>Methods (6)</summary>

`__init__`, `stage_group`, `_stage_group`, `_apply_change`, `_hash_mismatch`, `_build_delta`

</details>

#### `DirectStager`  ·  _class_  ·  bases: `_PathGroupStager`  ·  [L350]

Stage direct (gitignored / untracked) changes with last-writer-wins.

<details><summary>Methods (1)</summary>

`__init__`

</details>

#### `GatedStager`  ·  _class_  ·  bases: `_PathGroupStager`  ·  [L357]

Stage gated changes, validating each step's base-hash chain.

<details><summary>Methods (1)</summary>

`__init__`

</details>

---

## `sandbox/occ/ports.py`

#### `WorkspaceBindingSnapshot`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L17]

Immutable record binding a workspace reference to its workspace root and layer-stack root paths.

**Fields**

| name | type | default |
|------|------|---------|
| `workspace_ref` | `str` |  |
| `workspace_root` | `str` |  |
| `layer_stack_root` | `str` |  |

#### `LayerSnapshotReader`  ·  _protocol_  ·  bases: `Protocol`  ·  [L23]

Read immutable snapshot content without exposing storage layout.

<details><summary>Methods (3)</summary>

`read_active_manifest`, `read_bytes`, `read_text`

</details>

#### `LayerCommitStagingAllocator`  ·  _protocol_  ·  bases: `Protocol`  ·  [L41]

Allocate and drop OCC-owned staging directories.

<details><summary>Methods (2)</summary>

`allocate_commit_staging`, `drop_commit_staging`

</details>

#### `LayerCommitTransaction`  ·  _protocol_  ·  bases: `Protocol`  ·  [L49]

Active layer-stack commit transaction used by one OCC publish.

<details><summary>Methods (4)</summary>

`lock_wait_s`, `lock_held_s`, `snapshot`, `publish_layer`

</details>

#### `LayerCommitPublisher`  ·  _protocol_  ·  bases: `Protocol`  ·  [L69]

Publish accepted staged changes through the storage CAS primitive.

<details><summary>Methods (1)</summary>

`begin_transaction`

</details>

#### `OccLayerStackPort`  ·  _protocol_  ·  bases: `LayerSnapshotReader, LayerCommitStagingAllocator, LayerCommitPublisher, Protocol`  ·  [L75]

Combined layer-stack capability needed by the OCC service.

#### `WorkspaceBindingReader`  ·  _protocol_  ·  bases: `Protocol`  ·  [L84]

Fail-closed binding lookup used by OCC-facing clients.

<details><summary>Methods (1)</summary>

`require_workspace_binding`

</details>

---

## `sandbox/occ/service.py`

#### `OccService`  ·  _class_  ·  [L37]

Prepare typed OCC changesets and commit them through the layer stack.

**Instance attributes**: `_layer_stack`, `_preparer`, `_owns_commit_queue`, `_commit_queue`, `_maintenance`

<details><summary>Methods (12)</summary>

`__init__`, `apply_changeset`, `commit_prepared`, `apply_changeset_sync`, `commit_prepared_sync`, `run_maintenance_after_publish`, `_maintenance_after_publish`, `_maintenance_after_publish_sync`, `_finalize_commit_result`, `prepare_changeset`, `prepare_changeset_sync`, `close`

</details>

---

## `sandbox/overlay/handle.py`

#### `OverlayHandle`  ·  _dataclass_  ·  decorators: `@dataclass`  ·  [L12]

State-bearing handle for a mounted overlay.

**Fields**

| name | type | default |
|------|------|---------|
| `workspace_root` | `str` |  |
| `layer_paths` | `tuple[str, ...]` |  |
| `upperdir` | `Path` |  |
| `workdir` | `Path` |  |
| `lease_id` | `str` |  |
| `holder_pid` | `int \| None` |  |
| `run_dir` | `Path` |  |
| `snapshot_manifest` | `object \| None` | `None` |
| `snapshot_timings` | `dict[str, float]` | `field(default_factory=dict)` |
| `manifest_key` | `str` | `''` |
| `manifest_version` | `int` | `0` |
| `root_hash` | `str` | `''` |
| `operation_id` | `str` | `''` |
| `_released` | `bool` | `False` |
| `_release_lock` | `threading.Lock` | `field(default_factory=threading.Lock, repr=False, compare=False)` |
| `_release` | `Callable[[], None] \| None` | `field(default=None, repr=False, compare=False)` |

<details><summary>Methods (2)</summary>

`release`, `released`

</details>

---

## `sandbox/overlay/kernel_mount.py`

#### `MountInputs`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L32]

Bundles resolved paths and file descriptors needed to mount an overlay filesystem, with cleanup.

**Fields**

| name | type | default |
|------|------|---------|
| `workspace_root` | `Path` |  |
| `layer_paths` | `tuple[Path, ...]` |  |
| `upperdir` | `Path` |  |
| `workdir` | `Path` |  |
| `fds` | `tuple[int, ...]` |  |

<details><summary>Methods (1)</summary>

`close`

</details>

---

## `sandbox/overlay/mount_syscalls.py`

#### `MountSyscallsUnavailable`  ·  _exception_  ·  bases: `OSError`  ·  [L69]

Raised when required mount syscalls are not accessible.

---

## `sandbox/overlay/namespace_entrypoint.py`

#### `WorkspaceMountMode`  ·  _enum_  ·  bases: `str, Enum`  ·  [L26]

Enumerates whether the namespace helper should mount a new overlay or reuse an existing mount.

**Enum members**: `MOUNT_OVERLAY = 'mount_overlay'`, `EXISTING_MOUNT = 'existing_mount'`

<details><summary>Methods (1)</summary>

`__str__`

</details>

#### `_OverlayMountRequest`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L62]

Parsed request describing layers, dirs, output refs, and policy for an overlay mount in the namespace entrypoint.

**Fields**

| name | type | default |
|------|------|---------|
| `workspace_root` | `Path` |  |
| `layer_paths` | `tuple[Path, ...]` |  |
| `upperdir` | `Path` |  |
| `workdir` | `Path` |  |
| `stdout_ref` | `Path` |  |
| `stderr_ref` | `Path` |  |
| `timings_ref` | `Path` |  |
| `policy` | `CommandExecPolicy` |  |

---

## `sandbox/overlay/path_change.py`

#### `OverlayPathChange`  ·  _dataclass_  ·  decorators: `@dataclass`  ·  [L16]

Represents a single overlay filesystem change (write, symlink, opaque dir) with validation of content path and hash.

**Fields**

| name | type | default |
|------|------|---------|
| `path` | `str` |  |
| `kind` | `OverlayPathChangeKind` |  |
| `content_path` | `str \| None` |  |
| `final_hash` | `str \| None` |  |

<details><summary>Methods (1)</summary>

`__post_init__`

</details>

---

## `sandbox/overlay/writable_dirs.py`

#### `OverlayWritableRootUnavailable`  ·  _exception_  ·  bases: `RuntimeError`  ·  [L16]

Raised when the canonical upper/work root is not available.

#### `OverlayWritableDirs`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L21]

Per-overlay writable directories created beside each other.

**Fields**

| name | type | default |
|------|------|---------|
| `run_dir` | `Path` |  |
| `upperdir` | `Path` |  |
| `workdir` | `Path` |  |

---

## `sandbox/provider/daytona/adapter.py`

#### `DaytonaProviderAdapter`  ·  _class_  ·  [L107]

Provider adapter backed directly by the AsyncDaytona SDK.

**Fields**

| name | type | default |
|------|------|---------|
| `name` | `ClassVar[str]` | `'daytona'` |

**Instance attributes**: `_resolver`

<details><summary>Methods (14)</summary>

`__init__`, `get_health`, `list_snapshots`, `create`, `get`, `list`, `start`, `stop`, `delete`, `set_labels`, `get_signed_preview_url`, `get_build_logs_url`, `exec`, `context_preparer`

</details>

---

## `sandbox/provider/daytona/errors.py`

#### `DaytonaUnavailableError`  ·  _exception_  ·  bases: `RuntimeError`  ·  [L4]

Raised when Daytona SDK is not installed or not configured.

#### `AsyncDaytonaUnavailableError`  ·  _exception_  ·  bases: `RuntimeError`  ·  [L8]

Raised when Async Daytona SDK is not installed or not configured.

---

## `sandbox/provider/daytona/runtime_context.py`

#### `DaytonaContextPreparer`  ·  _class_  ·  [L19]

Inject sandbox runtime state for sandbox tools.

**Instance attributes**: `sandbox_id`, `_sandbox`, `_sandbox_loop_id`

<details><summary>Methods (5)</summary>

`__init__`, `_get_sandbox`, `_get_sandbox_async`, `prepare_context`, `prepare_context_async`

</details>

---

## `sandbox/provider/docker/adapter.py`

#### `DockerProviderAdapter`  ·  _class_  ·  [L94]

Docker SDK-backed implementation of ``ProviderAdapter``.

**Class variables**: `name = 'docker'`

**Instance attributes**: `_client`

<details><summary>Methods (17)</summary>

`__init__`, `_get_client`, `_get_async_client`, `get_health`, `list_snapshots`, `create`, `get`, `list`, `start`, `stop`, `delete`, `set_labels`, `get_signed_preview_url`, `get_build_logs_url`, `get_daemon_tcp_endpoint`, `exec`, `context_preparer`

</details>

---

## `sandbox/provider/docker/runtime_context.py`

#### `DockerContextPreparer`  ·  _class_  ·  [L26]

Inject Docker container runtime state for sandbox tools.

**Instance attributes**: `sandbox_id`, `_container`, `_container_loop_id`

<details><summary>Methods (5)</summary>

`__init__`, `_get_container`, `_get_container_async`, `prepare_context`, `prepare_context_async`

</details>

---

## `sandbox/provider/protocol.py`

#### `ProviderAdapter`  ·  _protocol_  ·  bases: `Protocol`  ·  [L21]

Container CRUD + exec primitives implemented by each provider.

**Fields**

| name | type | default |
|------|------|---------|
| `name` | `str` |  |

<details><summary>Methods (13)</summary>

`get_health`, `list_snapshots`, `create`, `get`, `list`, `start`, `stop`, `delete`, `set_labels`, `get_signed_preview_url`, `get_build_logs_url`, `exec`, `context_preparer`

</details>

---

## `sandbox/shared/command_exec_contract.py`

#### `CommandExecRequest`  ·  _dataclass_  ·  decorators: `@dataclass`  ·  [L18]

One command against a workspace replacement mount.

**Fields**

| name | type | default |
|------|------|---------|
| `invocation_id` | `str` |  |
| `workspace_ref` | `str` |  |
| `workspace_root` | `str` |  |
| `command` | `tuple[str, ...]` |  |
| `cwd` | `str` | `'.'` |
| `env` | `Mapping[str, str]` | `field(default_factory=dict)` |
| `timeout_seconds` | `float \| None` | `None` |
| `agent_id` | `str` | `''` |
| `description` | `str` | `'shell'` |

<details><summary>Methods (1)</summary>

`__post_init__`

</details>

#### `SnapshotManifest`  ·  _protocol_  ·  bases: `Protocol`  ·  [L67]

Snapshot manifest shape needed by command execution.

**Fields**

| name | type | default |
|------|------|---------|
| `version` | `int` |  |
| `layers` | `tuple[object, ...]` |  |

#### `WorkspaceSnapshotLease`  ·  _protocol_  ·  bases: `Protocol`  ·  [L74]

Protocol for a leased workspace snapshot exposing its manifest version, layer paths, and timing metadata.

**Fields**

| name | type | default |
|------|------|---------|
| `lease_id` | `str` |  |
| `manifest_version` | `int` |  |
| `manifest` | `SnapshotManifest` |  |
| `layer_paths` | `tuple[str, ...] \| None` |  |
| `timings` | `Mapping[str, float]` |  |

#### `OCCMutationClient`  ·  _protocol_  ·  bases: `Protocol`  ·  [L82]

OCC mutation client used for command-exec capture submission.

<details><summary>Methods (2)</summary>

`apply_changeset`, `run_maintenance_after_publish`

</details>

#### `ChangesetResultLike`  ·  _protocol_  ·  bases: `Protocol`  ·  [L103]

Minimal committed changeset result shape consumed by command execution.

**Fields**

| name | type | default |
|------|------|---------|
| `files` | `Sequence[FileResult]` |  |
| `timings` | `Mapping[str, float]` |  |
| `published_manifest_version` | `int \| None` |  |

<details><summary>Methods (1)</summary>

`success`

</details>

#### `WorkspaceCapturePublishResult`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L115]

Result returned by the daemon-owned overlay publish facade.

**Fields**

| name | type | default |
|------|------|---------|
| `path_changes` | `Sequence[OverlayPathChange]` |  |
| `changeset` | `ChangesetResultLike` |  |
| `timings` | `Mapping[str, float]` | `field(default_factory=dict)` |

---

## `sandbox/shared/command_exec_policy.py`

#### `CommandExecPolicy`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L11]

Tenant/test-injectable command execution policy.

**Fields**

| name | type | default |
|------|------|---------|
| `host_env_keys` | `frozenset[str] \| None` | `None` |
| `restricted_env_keys` | `frozenset[str]` | `field(default_factory=lambda: frozenset({'LD_PRELOAD', 'LD_LIBRARY_PATH', 'LD_AUDIT', 'DYLD_INSERT_LIBRARIES', 'DYLD_LIBRARY_PATH', 'PATH', 'PYTHONPATH', 'BASH_ENV', 'ENV'}))` |
| `forbidden_overlay_path_chars` | `tuple[str, ...]` | `(',', ':', '\\', '\n', '\r', '\t', '\x00')` |
| `command_env_defaults` | `Mapping[str, str]` | `field(default_factory=lambda: {'GIT_OPTIONAL_LOCKS': '0'})` |

<details><summary>Methods (4)</summary>

`command_environment`, `validate_overlay_path_text`, `to_payload`, `from_payload`

</details>

---

## `sandbox/shared/edit_apply.py`

#### `SearchReplaceError`  ·  _exception_  ·  bases: `ValueError`  ·  [L13]

Raised when a search/replace edit cannot be applied as requested.

**Instance attributes**: `message`

<details><summary>Methods (1)</summary>

`__init__`

</details>

---

## `sandbox/shared/layer_stack_port.py`

#### `LayerStackPort`  ·  _protocol_  ·  bases: `Protocol`  ·  [L19]

Layer-stack surface a workspace pipeline needs.

**Fields**

| name | type | default |
|------|------|---------|
| `storage_root` | `Path` |  |

<details><summary>Methods (3)</summary>

`acquire_snapshot`, `release_lease`, `read_active_manifest`

</details>

---

## `sandbox/shared/lease_guard.py`

#### `_LeasedHandle`  ·  _protocol_  ·  bases: `Protocol`  ·  [L22]

Minimum surface ``release`` needs from a handle.

**Fields**

| name | type | default |
|------|------|---------|
| `lease_id` | `str` |  |
| `_released` | `bool` |  |

#### `LeaseGuard`  ·  _class_  ·  [L29]

Lease-id-keyed lock + released-set composed by pipelines that

**Instance attributes**: `_lease_locks`, `_released_lease_ids`

<details><summary>Methods (4)</summary>

`__init__`, `_lock_for`, `release`, `mark_released`

</details>

---

## `sandbox/shared/models.py`

#### `Intent`  ·  _enum_  ·  bases: `str, Enum`  ·  [L15]

High-level execution intent for a foreground sandbox tool call.

**Enum members**: `READ_ONLY = 'read_only'`, `WRITE_ALLOWED = 'write_allowed'`, `LIFECYCLE = 'lifecycle'`

#### `SandboxCaller`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, kw_only=True)`  ·  [L24]

Caller identity threaded onto every audit-aware request.

**Fields**

| name | type | default |
|------|------|---------|
| `agent_id` | `str` |  |
| `run_id` | `str` | `''` |
| `agent_run_id` | `str` | `''` |
| `task_id` | `str` | `''` |
| `task_center_run_id` | `str` | `''` |
| `task_center_task_id` | `str` | `''` |
| `task_center_attempt_id` | `str` | `''` |
| `task_center_workflow_id` | `str` | `''` |
| `task_center_request_id` | `str` | `''` |
| `tool_name` | `str` | `''` |
| `tool_id` | `str` | `''` |

<details><summary>Methods (1)</summary>

`audit_fields`

</details>

#### `SandboxRequestBase`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, kw_only=True)`  ·  [L52]

Base request shape for audit-aware public sandbox operations.

**Fields**

| name | type | default |
|------|------|---------|
| `caller` | `SandboxCaller` |  |
| `description` | `str` | `''` |
| `invocation_id` | `str` | `''` |

<details><summary>Methods (1)</summary>

`default_description`

</details>

#### `SandboxResultBase`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, kw_only=True)`  ·  [L64]

Base result shape for public sandbox operations.

**Fields**

| name | type | default |
|------|------|---------|
| `success` | `bool` | `True` |
| `workspace` | `Literal['ephemeral', 'isolated']` | `'ephemeral'` |
| `timings` | `dict[str, float]` | `field(default_factory=dict)` |
| `conflict` | `'ConflictInfo \| None'` | `None` |
| `conflict_reason` | `str \| None` | `None` |
| `changed_paths` | `list[str] \| tuple[str, ...]` | `field(default_factory=list)` |
| `error` | `dict[str, object] \| None` | `None` |

#### `ToolCallRequest`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, kw_only=True)`  ·  [L80]

One tool invocation routed through a workspace pipeline.

**Fields**

| name | type | default |
|------|------|---------|
| `invocation_id` | `str` |  |
| `agent_id` | `str` |  |
| `verb` | `str` |  |
| `intent` | `Intent` |  |
| `args` | `Mapping[str, object]` |  |
| `background` | `bool` | `False` |

<details><summary>Methods (2)</summary>

`to_payload`, `from_payload`

</details>

#### `ConflictInfo`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, kw_only=True)`  ·  [L116]

Structured guarded-operation conflict details.

**Fields**

| name | type | default |
|------|------|---------|
| `reason` | `str` |  |
| `conflict_file` | `str \| None` | `None` |
| `message` | `str` | `''` |

<details><summary>Methods (2)</summary>

`rejected`, `overlap`

</details>

#### `GuardedResultBase`  ·  _dataclass_  ·  bases: `SandboxResultBase`  ·  decorators: `@dataclass(frozen=True, kw_only=True)`  ·  [L137]

Base result for OCC/overlay-guarded operations.

**Fields**

| name | type | default |
|------|------|---------|
| `changed_paths` | `tuple[str, ...]` | `()` |
| `changed_path_kinds` | `dict[str, str]` | `field(default_factory=dict)` |
| `mutation_source` | `str` | `''` |
| `status` | `str` | `''` |
| `conflict` | `ConflictInfo \| None` | `None` |
| `conflict_reason` | `str \| None` | `None` |

#### `RawExecResult`  ·  _dataclass_  ·  bases: `SandboxResultBase`  ·  decorators: `@dataclass(frozen=True, kw_only=True)`  ·  [L149]

Result of a one-shot raw provider exec call.

**Fields**

| name | type | default |
|------|------|---------|
| `exit_code` | `int` |  |
| `stdout` | `str` |  |
| `stderr` | `str` | `''` |

#### `ReadFileRequest`  ·  _dataclass_  ·  bases: `SandboxRequestBase`  ·  decorators: `@dataclass(frozen=True, kw_only=True)`  ·  [L158]

Request to read a file at a given path within the sandbox workspace.

**Fields**

| name | type | default |
|------|------|---------|
| `path` | `str` |  |

#### `ReadFileResult`  ·  _dataclass_  ·  bases: `SandboxResultBase`  ·  decorators: `@dataclass(frozen=True, kw_only=True)`  ·  [L163]

Result of a sandbox file read carrying the content, existence flag, and encoding.

**Fields**

| name | type | default |
|------|------|---------|
| `content` | `str` |  |
| `exists` | `bool` | `True` |
| `encoding` | `str` | `'utf-8'` |

#### `WriteFileRequest`  ·  _dataclass_  ·  bases: `SandboxRequestBase`  ·  decorators: `@dataclass(frozen=True, kw_only=True)`  ·  [L170]

Request to write content to a sandbox file path, with optional overwrite control.

**Fields**

| name | type | default |
|------|------|---------|
| `path` | `str` |  |
| `content` | `str` |  |
| `overwrite` | `bool` | `True` |

#### `WriteFileResult`  ·  _dataclass_  ·  bases: `GuardedResultBase`  ·  decorators: `@dataclass(frozen=True, kw_only=True)`  ·  [L177]

Result of a guarded sandbox write-file operation.

#### `SearchReplaceEdit`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, kw_only=True)`  ·  [L182]

One exact-match replacement applied as part of an ``EditFileRequest``.

**Fields**

| name | type | default |
|------|------|---------|
| `old_text` | `str` |  |
| `new_text` | `str` |  |
| `replace_all` | `bool` | `False` |

#### `EditFileRequest`  ·  _dataclass_  ·  bases: `SandboxRequestBase`  ·  decorators: `@dataclass(frozen=True, kw_only=True)`  ·  [L191]

Request to apply ordered exact-match search-replace edits to a sandbox file.

**Fields**

| name | type | default |
|------|------|---------|
| `path` | `str` |  |
| `edits` | `tuple[SearchReplaceEdit, ...]` |  |

#### `EditFileResult`  ·  _dataclass_  ·  bases: `GuardedResultBase`  ·  decorators: `@dataclass(frozen=True, kw_only=True)`  ·  [L197]

Result of a guarded edit-file operation reporting how many edits were applied.

**Fields**

| name | type | default |
|------|------|---------|
| `applied_edits` | `int` | `0` |

#### `ShellRequest`  ·  _dataclass_  ·  bases: `SandboxRequestBase`  ·  decorators: `@dataclass(frozen=True, kw_only=True)`  ·  [L202]

Request to run a shell command in the sandbox with optional cwd, timeout, stdin, and background flag.

**Fields**

| name | type | default |
|------|------|---------|
| `command` | `str` |  |
| `cwd` | `str \| None` | `None` |
| `timeout` | `int \| None` | `None` |
| `stdin` | `str \| None` | `None` |
| `background` | `bool` | `False` |

#### `ShellResult`  ·  _dataclass_  ·  bases: `GuardedResultBase`  ·  decorators: `@dataclass(frozen=True, kw_only=True)`  ·  [L213]

Result of a guarded shell command, carrying exit code, stdout, stderr, and warnings.

**Fields**

| name | type | default |
|------|------|---------|
| `exit_code` | `int` |  |
| `stdout` | `str` |  |
| `stderr` | `str` | `''` |
| `warnings` | `tuple[str, ...]` | `()` |

#### `GlobRequest`  ·  _dataclass_  ·  bases: `SandboxRequestBase`  ·  decorators: `@dataclass(frozen=True, kw_only=True)`  ·  [L221]

Request to glob files in the sandbox by pattern and optional base path.

**Fields**

| name | type | default |
|------|------|---------|
| `pattern` | `str` |  |
| `path` | `str \| None` | `None` |

#### `GlobResult`  ·  _dataclass_  ·  bases: `SandboxResultBase`  ·  decorators: `@dataclass(frozen=True, kw_only=True)`  ·  [L227]

Result of a sandbox glob operation reporting matched filenames and truncation status.

**Fields**

| name | type | default |
|------|------|---------|
| `filenames` | `tuple[str, ...]` | `()` |
| `num_files` | `int` | `0` |
| `truncated` | `bool` | `False` |

#### `GrepRequest`  ·  _dataclass_  ·  bases: `SandboxRequestBase`  ·  decorators: `@dataclass(frozen=True, kw_only=True)`  ·  [L234]

Request describing a sandbox grep search with pattern, path, and output options.

**Fields**

| name | type | default |
|------|------|---------|
| `pattern` | `str` |  |
| `path` | `str \| None` | `None` |
| `glob_filter` | `str \| None` | `None` |
| `output_mode` | `str` | `'files_with_matches'` |
| `head_limit` | `int \| None` | `None` |
| `offset` | `int` | `0` |
| `case_insensitive` | `bool` | `False` |
| `line_numbers` | `bool` | `False` |
| `multiline` | `bool` | `False` |

#### `GrepResult`  ·  _dataclass_  ·  bases: `SandboxResultBase`  ·  decorators: `@dataclass(frozen=True, kw_only=True)`  ·  [L247]

Result of a sandbox grep search carrying matched files, content, and match counts.

**Fields**

| name | type | default |
|------|------|---------|
| `output_mode` | `str` | `'files_with_matches'` |
| `filenames` | `tuple[str, ...]` | `()` |
| `content` | `str` | `''` |
| `num_files` | `int` | `0` |
| `num_lines` | `int` | `0` |
| `num_matches` | `int` | `0` |
| `applied_limit` | `int \| None` | `None` |
| `applied_offset` | `int` | `0` |
| `truncated` | `bool` | `False` |

#### `LifecycleError`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, kw_only=True)`  ·  [L260]

Categorical isolated-workspace lifecycle error.

**Fields**

| name | type | default |
|------|------|---------|
| `kind` | `str` |  |
| `message` | `str` | `''` |
| `details` | `dict[str, str]` | `field(default_factory=dict)` |

#### `LifecycleResultBase`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, kw_only=True)`  ·  [L269]

Base result for lifecycle operations, separate from OCC conflicts.

**Fields**

| name | type | default |
|------|------|---------|
| `success` | `bool` | `True` |
| `timings` | `dict[str, float]` | `field(default_factory=dict)` |
| `error` | `LifecycleError \| None` | `None` |

#### `EnterIsolatedWorkspaceRequest`  ·  _dataclass_  ·  bases: `SandboxRequestBase`  ·  decorators: `@dataclass(frozen=True, kw_only=True)`  ·  [L278]

Request to enter an isolated workspace rooted at a given layer-stack path.

**Fields**

| name | type | default |
|------|------|---------|
| `layer_stack_root` | `str` |  |

#### `EnterIsolatedWorkspaceResult`  ·  _dataclass_  ·  bases: `LifecycleResultBase`  ·  decorators: `@dataclass(frozen=True, kw_only=True)`  ·  [L283]

Result of entering an isolated workspace reporting the resulting manifest version and hash.

**Fields**

| name | type | default |
|------|------|---------|
| `manifest_version` | `str` | `''` |
| `manifest_root_hash` | `str` | `''` |

#### `ExitIsolatedWorkspaceRequest`  ·  _dataclass_  ·  bases: `SandboxRequestBase`  ·  decorators: `@dataclass(frozen=True, kw_only=True)`  ·  [L289]

Request to exit an isolated workspace with a grace period for draining work.

**Fields**

| name | type | default |
|------|------|---------|
| `grace_s` | `float` | `5.0` |

#### `ExitIsolatedWorkspaceResult`  ·  _dataclass_  ·  bases: `LifecycleResultBase`  ·  decorators: `@dataclass(frozen=True, kw_only=True)`  ·  [L294]

Result of exiting an isolated workspace, reporting evicted bytes, lifetime, and per-phase timing.

**Fields**

| name | type | default |
|------|------|---------|
| `evicted_upperdir_bytes` | `int` | `0` |
| `lifetime_s` | `float` | `0.0` |
| `phases_ms` | `dict[str, float]` | `field(default_factory=dict)` |

---

## `sandbox/shared/ordered_lock.py`

#### `OrderedLock`  ·  _class_  ·  [L94]

``asyncio.Lock`` wrapper that records acquisitions for AC9 assertions.

**Instance attributes**: `_name`, `_lock`

<details><summary>Methods (7)</summary>

`__init__`, `name`, `locked`, `acquire`, `release`, `__aenter__`, `__aexit__`

</details>

---

## `sandbox/shared/timing_keys.py`

#### `TimingKey`  ·  _enum_  ·  bases: `str, Enum`  ·  [L8]

String enum of canonical metric keys naming OCC apply, commit, and direct-path timing measurements.

**Enum members**: `APPLY_COMMIT = 'occ.apply.commit_s'`, `APPLY_COMMIT_QUEUE_WAIT = 'occ.apply.commit_queue_wait_s'`, `APPLY_COMMIT_RESUME_WAIT = 'occ.apply.commit_resume_wait_s'`, `APPLY_COMMIT_WORKER = 'occ.apply.commit_worker_s'`, `APPLY_MANIFEST_LAG = 'occ.apply.manifest_lag'`, `APPLY_TOTAL = 'occ.apply.total_s'`, `COMMIT_COLLECT_CHANGES = 'occ.commit.collect_changes_s'`, `COMMIT_DIRECT_APPLY_TOTAL = 'occ.commit.direct_apply_changes_total_s'`, `COMMIT_DIRECT_PATH_COUNT = 'occ.commit.direct_path_count'`, `COMMIT_DIRECT_READ_TOTAL = 'occ.commit.direct_read_current_total_s'`, `COMMIT_DIRECT_STAGE_TOTAL = 'occ.commit.direct_stage_delta_total_s'`, `COMMIT_GATED_APPLY_TOTAL = 'occ.commit.gated_apply_changes_total_s'`, `COMMIT_GATED_PATH_COUNT = 'occ.commit.gated_path_count'`, `COMMIT_GATED_READ_TOTAL = 'occ.commit.gated_read_current_total_s'`, `COMMIT_GATED_STAGE_TOTAL = 'occ.commit.gated_stage_delta_total_s'`, `COMMIT_PUBLISH_LAYER = 'occ.commit.publish_layer_s'`, `COMMIT_QUEUE_BATCH_SIZE = 'occ.serial.batch_size'`, `COMMIT_QUEUE_CAS_ATTEMPTS = 'occ.serial.cas_attempts'`, `COMMIT_QUEUE_CAS_EXHAUSTED = 'occ.serial.cas_exhausted'`, `COMMIT_QUEUE_COMMIT = 'occ.serial.commit_s'`, `COMMIT_QUEUE_RESULT_READY_AT = '_occ.serial.result_ready_at_s'`, `COMMIT_QUEUE_WAIT = 'occ.serial.queue_wait_s'`, `COMMIT_SNAPSHOT = 'occ.commit.snapshot_s'`, `COMMIT_STAGER_WRITE_COUNT = 'occ.commit.stager_write_count'`, `COMMIT_STAGER_WRITE_TOTAL = 'occ.commit.stager_write_total_s'`, `COMMIT_TOTAL = 'occ.commit.total_s'`, `COMMIT_VALIDATE_GROUPS = 'occ.commit.validate_groups_s'`, `DIRECT_APPLY_CHANGES = 'occ.direct.apply_changes_s'`, `DIRECT_READ_CURRENT = 'occ.direct.read_current_s'`, `DIRECT_STAGE_DELTA = 'occ.direct.stage_delta_s'`, `GATED_APPLY_CHANGES = 'occ.gated.apply_changes_s'`, `GATED_READ_CURRENT = 'occ.gated.read_current_s'`, `GATED_STAGE_DELTA = 'occ.gated.stage_delta_s'`, `GITIGNORE_CACHE_HITS_TOTAL = 'gitignore.cache_hits_total'`, `GITIGNORE_CACHE_MISSES_TOTAL = 'gitignore.cache_misses_total'`, `LAYER_AUTO_SQUASH_DEPTH_AFTER = 'layer_stack.auto_squash.depth_after'`, `LAYER_AUTO_SQUASH_DEPTH_BEFORE = 'layer_stack.auto_squash.depth_before'`, `LAYER_AUTO_SQUASH_MANIFEST_VERSION = 'layer_stack.auto_squash.manifest_version'`, `LAYER_AUTO_SQUASH_MAX_DEPTH = 'layer_stack.auto_squash.max_depth'`, `LAYER_AUTO_SQUASH_RACED = 'layer_stack.auto_squash.raced'`, `LAYER_AUTO_SQUASH_RECHECK_TRIGGERED = 'layer_stack.auto_squash.recheck_triggered'`, `LAYER_AUTO_SQUASH_SKIPPED_IN_FLIGHT = 'layer_stack.auto_squash.skipped_in_flight'`, `LAYER_AUTO_SQUASH_TOTAL = 'layer_stack.auto_squash.total_s'`, `LAYER_TRANSACTION_LOCK_HELD = 'layer_stack.transaction.lock_held_s'`, `LAYER_TRANSACTION_LOCK_WAIT = 'layer_stack.transaction.lock_wait_s'`, `PREPARE_CURRENT_SNAPSHOT = 'occ.prepare.current_snapshot_s'`, `PREPARE_GITIGNORE = 'occ.prepare.gitignore_s'`, `PREPARE_GROUP_BY_ROUTE = 'occ.prepare.group_by_route_s'`, `PREPARE_GROUPS = 'occ.prepare.prepare_groups_s'`, `PREPARE_ROUTE_AND_BASE_HASH = 'occ.prepare.route_and_base_hash_s'`, `PREPARE_TOTAL = 'occ.prepare.total_s'`

---

## `sandbox/shared/tool_primitives/cancellation.py`

#### `VerbCancellation`  ·  _protocol_  ·  bases: `Protocol`  ·  [L11]

Cancellation hook supplied by a tool verb.

<details><summary>Methods (3)</summary>

`cancel_event`, `record_pid`, `on_cancel`

</details>

#### `_NoopCancellation`  ·  _class_  ·  [L22]

No-op cancellation handle providing the cancellation interface without signalling any process group.

<details><summary>Methods (3)</summary>

`cancel_event`, `record_pid`, `on_cancel`

</details>

#### `ShellPgrpCancellation`  ·  _class_  ·  [L34]

Signal the namespace child process group when a shell request is cancelled.

**Instance attributes**: `_cancel_event`, `_pgrp`

<details><summary>Methods (4)</summary>

`__init__`, `cancel_event`, `record_pid`, `on_cancel`

</details>

---

## `sandbox/shared/tool_primitives/grep.py`

#### `_GrepOptions`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L26]

Parsed configuration for a grep search: root, pattern, and matching/output flags.

**Fields**

| name | type | default |
|------|------|---------|
| `root` | `Path` |  |
| `pattern` | `str` |  |
| `case_insensitive` | `bool` | `False` |
| `glob_filter` | `str \| None` | `None` |
| `output_mode` | `_GrepOutputMode` | `'files_with_matches'` |
| `line_numbers` | `bool` | `False` |
| `multiline` | `bool` | `False` |

---

## `sandbox/shared/tool_primitives/workspace_filesystem.py`

#### `_OpenHow`  ·  _class_  ·  bases: `ctypes.Structure`  ·  [L17]

ctypes mirror of the kernel open_how struct used by the openat2 syscall.

**Class variables**: `_fields_`

