use crate::{ManagerError, SandboxDaemonEndpoint};

pub trait SandboxDaemonClient: Send + Sync {
    fn invoke(
        &self,
        endpoint: &SandboxDaemonEndpoint,
        request: sandbox_protocol::Request,
    ) -> Result<sandbox_protocol::Response, ManagerError>;
}
