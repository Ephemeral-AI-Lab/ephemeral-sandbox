//! Sandbox host service group.

use std::sync::Arc;

use eos_sandbox_api::SandboxTransport;
use eos_sandbox_host::RequestProvisioner;

/// Sandbox host access for request provisioning and daemon RPC.
#[derive(Clone)]
pub(crate) struct SandboxService {
    pub(crate) transport: Arc<dyn SandboxTransport>,
    pub(crate) provisioner: Arc<dyn RequestProvisioner>,
}

impl std::fmt::Debug for SandboxService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SandboxService").finish_non_exhaustive()
    }
}
