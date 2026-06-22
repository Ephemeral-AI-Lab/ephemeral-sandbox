use std::sync::Arc;

use crate::{SandboxDaemonClient, SandboxDaemonInstaller, SandboxRuntime, SandboxStore};

#[derive(Clone, Copy)]
pub(crate) struct ManagerOperationEntry {
    pub(crate) spec: &'static sandbox_protocol::CliOperationSpec,
    pub(crate) dispatch:
        fn(&ManagerServices, &sandbox_protocol::Request) -> sandbox_protocol::Response,
}

impl ManagerOperationEntry {
    #[must_use]
    pub(crate) const fn new(
        spec: &'static sandbox_protocol::CliOperationSpec,
        dispatch: fn(&ManagerServices, &sandbox_protocol::Request) -> sandbox_protocol::Response,
    ) -> Self {
        Self { spec, dispatch }
    }
}

pub struct ManagerServices {
    pub store: Arc<SandboxStore>,
    pub runtime: Arc<dyn SandboxRuntime>,
    pub daemon_installer: Arc<dyn SandboxDaemonInstaller>,
    pub daemon_client: Arc<dyn SandboxDaemonClient>,
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
        }
    }
}

#[must_use]
pub fn dispatch_operation(
    services: &ManagerServices,
    request: &sandbox_protocol::Request,
) -> sandbox_protocol::Response {
    super::impls::operation_entries()
        .iter()
        .find(|entry| entry.spec.name == request.op)
        .map_or_else(sandbox_protocol::Response::unknown_op, |entry| {
            (entry.dispatch)(services, request)
        })
}
