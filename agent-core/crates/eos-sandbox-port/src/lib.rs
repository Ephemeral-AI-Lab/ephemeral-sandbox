//! eos-sandbox-port — the host-facing sandbox protocol boundary.
//!
//! This crate is the typed contract agent-core uses to call the existing
//! sandbox daemon. Its single responsibility is to define:
//!
//! - the request/result DTOs and [`Intent`] for each daemon operation
//!   ([`models`]);
//! - the typed daemon op constants ([`DaemonOp`]);
//! - the [`SandboxTransport`] async trait seam (DIP — implemented downstream in
//!   `eos-sandbox-host`, injected by `eos-runtime`);
//! - the [`SandboxGateway`] single-handle injection seam that bundles the
//!   transport and provisioner (implemented by the backend `SandboxManager`);
//! - the timeout policy ([`exec_dispatch_timeout`] and the `*_TIMEOUT_S`
//!   constants); and
//! - the pure `tool_api` helpers that build a daemon payload, call a transport,
//!   and parse the JSON envelope into a typed result.
//!
//! It deliberately does **not** implement the daemon-backed transport, stamp the
//! protocol version, emit audit events (audit wrapping lives in `eos-tools`),
//! select a sandbox provider, or own a Tokio runtime.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod error;
mod gateway;
mod models;
mod ops;
mod provision;
mod timeouts;
mod tool_api;
mod transport;

pub use error::SandboxPortError;
pub use gateway::SandboxGateway;
pub use provision::{RequestProvisioner, RequestSandboxBinding, SandboxProvisionError};
pub use models::{
    CommandOutput, CommandSessionCancelRequest, CommandStatusView, ConflictInfo, EditFileRequest,
    EditFileResult, EnterIsolatedWorkspaceRequest, EnterIsolatedWorkspaceResult,
    ExecCommandRequest, ExecCommandResult, ExecStdinRequest, ExitIsolatedWorkspaceRequest,
    ExitIsolatedWorkspaceResult, Intent, KnownCommandStatus, LifecycleError, LifecycleResultBase,
    ReadFileRequest, ReadFileResult, SandboxRequestBase, SandboxResultBase, SearchReplaceEdit,
    ToolCallRequest, Workspace, WriteFileRequest, WriteFileResult,
};
pub use ops::DaemonOp;
pub use timeouts::{
    exec_dispatch_timeout, EDIT_FILE_TIMEOUT_S, EXEC_DEFAULT_COMMAND_TIMEOUT_S,
    EXEC_DISPATCH_GRACE_S, READ_FILE_TIMEOUT_S, WRITE_FILE_TIMEOUT_S,
};
pub use tool_api::{
    cancel, cancel_command_session, collect_command_completions, command_session_count, edit_file,
    ensure_plugin_package, enter_isolated_workspace, exec_command, exec_stdin,
    exit_isolated_workspace, heartbeat, inflight_count, isolated_active, plugin_dispatch,
    plugin_ensure, read_file, write_file, PluginDependencyScope, PluginDispatchRequest,
    PluginEnsureRequest, PluginManifestDescriptor, PluginOperationDescriptor,
    PluginPackageContract, PluginPackageDescriptor, PluginPackageEnsureRequest, PluginPackageFile,
    PluginPackageTree, PluginRefreshStrategy, PluginServiceDescriptor, PluginServiceMode,
    PluginSetupDescriptor,
};
pub use transport::SandboxTransport;
