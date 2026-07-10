#![forbid(unsafe_code)]

mod daemon_client;
mod daemon_install;
mod error;
mod export_apply;
mod model;
mod operation;
mod progress;
pub(crate) mod router;
mod runtime;
mod store;

pub use daemon_client::{SandboxDaemonClient, TcpSandboxDaemonClient};
pub use daemon_install::{LocalSandboxDaemonInstaller, SandboxDaemonInstaller, StartedDaemon};
pub use error::ManagerError;
pub use export_apply::ExportApplyCaps;
pub use model::{
    SandboxDaemonEndpoint, SandboxHttpEndpoint, SandboxId, SandboxRecord, SandboxState,
    SharedBaseMount,
};
pub use operation::{
    cli_operation_catalog, cli_operation_families, cli_operation_specs, dispatch_operation,
    dispatch_operation_with_progress, ManagerServices,
};
pub use progress::ProgressSink;
pub use router::SandboxManagerRouter;
pub use runtime::{CreateSandboxRequest, CreateSandboxResult, SandboxRuntime};
pub use store::SandboxStore;
