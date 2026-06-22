//! Shared workspace runtime primitives plus concrete workspace isolation
//! profiles.
//!
//! Every profile creates a private mounted workspace: fresh overlay directories
//! plus the holder-owned namespace stack used to run and remount commands.
//! `WorkspaceProfile` selects the isolation profile applied to that workspace; higher
//! layers decide when a workspace is created, destroyed, captured, or published.
//!
//! The host-compatible profile keeps the private workspace overlay and holder
//! namespace stack without adding a dedicated network boundary. The isolated
//! profile adds a dedicated network boundary with veth and network policy.
//! `overlay` holds the filesystem and telemetry contracts both profiles share,
//! while common lifecycle code owns holder, namespace FD, scratch, and cgroup
//! behavior.
#![forbid(unsafe_code)]

pub mod error;
mod isolated_setup;
mod lifecycle;
mod metrics;
pub mod model;
mod namespace;
pub mod overlay;
pub mod profile;
pub mod service;

pub use error::WorkspaceError;
pub use metrics::{
    noop_runtime_metrics_recorder, CgroupReadErrorKind, CommandCancellationReason,
    NoopRuntimeMetricsRecorder, PublishRejectionReason, RemountFailureReason, RuntimeMetricStatus,
    RuntimeMetricsRecorder, RuntimeMetricsRecorderHandle, RuntimeOperationName, WorkspacePhase,
};
pub use model::{
    BaseRevision, CaptureChangesRequest, CapturedWorkspaceChanges, ChangedPathKind,
    CreateWorkspaceRequest, DestroyWorkspaceRequest, DestroyWorkspaceResult, LayerStackSnapshotRef,
    LayerStackSnapshotView, LeaseId, ProtectedPathDrop, ProtectedPathDropReason,
    ReadonlySnapshotHandle, RemountWorkspaceRequest, RemountWorkspaceResult, WorkspaceEntry,
    WorkspaceEntryError, WorkspaceEntryFds, WorkspaceHandle, WorkspaceProfile, WorkspaceSessionId,
};
pub use namespace::cgroup::enable_cgroup_controllers_for_children;
pub use namespace::cgroup_monitor::{
    build_cgroup_monitor_sample, command_cgroup_path, session_cgroup_path, CgroupCleanupState,
    CgroupCpuSample, CgroupDiskSample, CgroupIoSample, CgroupMemoryEvents, CgroupMemorySample,
    CgroupMonitorConfig, CgroupMonitorRegistry, CgroupMonitorSample, CgroupMonitorSampleWindow,
    CgroupMonitorSnapshot, CgroupMonitorState, CgroupMonitorTarget, CgroupMonitorTargetKind,
    CgroupPidSample, CgroupPressureSample, CgroupRuntimeState, CgroupSampleKind,
    CgroupSampleRequest, PressureResourceSample,
};
pub use service::{WorkspaceRuntimeHooks, WorkspaceRuntimeService};
