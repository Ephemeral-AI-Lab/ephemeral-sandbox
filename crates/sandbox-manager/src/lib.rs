#![forbid(unsafe_code)]

pub mod daemon_client;
pub mod daemon_install;
pub mod error;
pub mod model;
pub mod operation;
pub mod runtime;
pub mod server;
pub mod store;

pub use daemon_client::SandboxDaemonClient;
pub use daemon_install::SandboxDaemonInstaller;
pub use error::{ManagerError, ManagerResult};
pub use model::{SandboxDaemonEndpoint, SandboxId, SandboxRecord, SandboxState};
pub use operation::{
    dispatch_operation, operation_catalog, operation_specs, ManagerOperationDispatch,
    ManagerOperationEntry, ManagerServices,
};
pub use runtime::SandboxRuntime;
pub use server::{SandboxManagerServer, ServerConfig, ServerError};
pub use store::SandboxStore;
