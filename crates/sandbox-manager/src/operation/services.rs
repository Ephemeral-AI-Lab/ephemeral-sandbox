use std::sync::Arc;

use crate::export_apply::ExportApplyCaps;
use crate::{SandboxDaemonClient, SandboxDaemonInstaller, SandboxRuntime, SandboxStore};

pub struct ManagerServices {
    pub store: Arc<SandboxStore>,
    pub runtime: Arc<dyn SandboxRuntime>,
    pub daemon_installer: Arc<dyn SandboxDaemonInstaller>,
    pub daemon_client: Arc<dyn SandboxDaemonClient>,
    /// `manager.export` apply caps; the gateway overwrites the default with
    /// the configured values before serving.
    pub export_caps: ExportApplyCaps,
}

impl ManagerServices {
    #[must_use]
    pub fn new(
        store: Arc<SandboxStore>,
        runtime: Arc<dyn SandboxRuntime>,
        daemon_installer: Arc<dyn SandboxDaemonInstaller>,
        daemon_client: Arc<dyn SandboxDaemonClient>,
    ) -> Self {
        Self {
            store,
            runtime,
            daemon_installer,
            daemon_client,
            export_caps: ExportApplyCaps::default(),
        }
    }
}
