mod config;
mod connection;
mod dispatch;
mod error;
mod forward;
mod lifecycle;

use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::ManagerServices;

pub use config::ServerConfig;
pub use error::ServerError;

pub struct SandboxManagerServer {
    pub config: ServerConfig,
    pub services: Arc<ManagerServices>,
    pub shutdown: CancellationToken,
}

impl SandboxManagerServer {
    #[must_use]
    pub fn new(config: ServerConfig, services: Arc<ManagerServices>) -> Self {
        Self::with_shutdown(config, services, CancellationToken::new())
    }

    #[must_use]
    pub const fn with_shutdown(
        config: ServerConfig,
        services: Arc<ManagerServices>,
        shutdown: CancellationToken,
    ) -> Self {
        Self {
            config,
            services,
            shutdown,
        }
    }
}
