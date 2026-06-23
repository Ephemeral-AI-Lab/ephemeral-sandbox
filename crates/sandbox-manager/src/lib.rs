#![forbid(unsafe_code)]

pub mod daemon_client;
pub mod daemon_install;
pub mod error;
pub mod model;
pub mod operation;
pub mod router;
pub mod runtime;
pub mod store;

pub use daemon_client::SandboxDaemonClient;
pub use daemon_install::{
    LocalSandboxDaemonInstaller, SandboxDaemonInstaller, SandboxDaemonLaunchSpec,
};
pub use error::ManagerError;
pub use model::{SandboxDaemonEndpoint, SandboxId, SandboxRecord, SandboxState};
pub use operation::{
    cli_operation_catalog, cli_operation_families, cli_operation_specs, dispatch_operation,
    ManagerServices,
};
pub use router::SandboxManagerRouter;
pub use runtime::{CreateSandboxRequest, CreateSandboxResult, SandboxRuntime};
pub use store::SandboxStore;
