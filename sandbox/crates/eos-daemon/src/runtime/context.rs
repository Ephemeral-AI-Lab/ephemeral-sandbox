use eos_config::configs::daemon::FileLimitsConfig;
use eos_runtime::RuntimeServices;

use crate::error::DaemonError;
use crate::invocation_registry::InFlightRegistry;

/// Per-dispatch daemon services used by handlers that need runtime state.
#[derive(Clone, Copy, Default)]
pub struct DispatchContext<'ctx> {
    services: Option<&'ctx RuntimeServices>,
    invocation_registry: Option<&'ctx InFlightRegistry>,
    file_limits: Option<FileLimitsConfig>,
    read_request_s: Option<f64>,
}

impl<'ctx> DispatchContext<'ctx> {
    /// Empty context for direct unit dispatch.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            services: None,
            invocation_registry: None,
            file_limits: None,
            read_request_s: None,
        }
    }

    /// Context carrying the server's owned services.
    #[must_use]
    pub const fn with_services(services: &'ctx RuntimeServices) -> Self {
        Self {
            services: Some(services),
            ..Self::empty()
        }
    }

    /// Context carrying the server's invocation registry.
    #[must_use]
    pub const fn with_invocation_registry(invocation_registry: &'ctx InFlightRegistry) -> Self {
        Self {
            invocation_registry: Some(invocation_registry),
            ..Self::empty()
        }
    }

    /// Context carrying the server's services, invocation registry, file byte
    /// limits, and measured request read duration.
    #[must_use]
    pub const fn with_runtime_config(
        services: &'ctx RuntimeServices,
        invocation_registry: &'ctx InFlightRegistry,
        file_limits: FileLimitsConfig,
        read_request_s: f64,
    ) -> Self {
        Self {
            services: Some(services),
            invocation_registry: Some(invocation_registry),
            file_limits: Some(file_limits),
            read_request_s: Some(read_request_s),
        }
    }

    /// The owned daemon services, when threaded. Handlers that can degrade
    /// (e.g. isolated-workspace routing checks) treat `None` as "no state".
    pub(crate) const fn services(&self) -> Option<&'ctx RuntimeServices> {
        self.services
    }

    /// The owned daemon services, required. Handlers that cannot operate
    /// without service state fail closed with a structured internal error.
    pub(crate) const fn require_services(&self) -> Result<&'ctx RuntimeServices, DaemonError> {
        match self.services {
            Some(services) => Ok(services),
            None => Err(DaemonError::ServicesUnavailable),
        }
    }

    pub(crate) const fn invocation_registry(&self) -> Option<&'ctx InFlightRegistry> {
        self.invocation_registry
    }

    /// Per-file read/write byte caps, when runtime config was threaded. File ops
    /// fall back to the `eos_config` defaults when this is `None`.
    pub(crate) const fn file_limits(&self) -> Option<FileLimitsConfig> {
        self.file_limits
    }

    pub(crate) const fn read_request_s(&self) -> Option<f64> {
        self.read_request_s
    }

    #[cfg(test)]
    pub(crate) const fn with_read_request_s(read_request_s: f64) -> Self {
        Self {
            read_request_s: Some(read_request_s),
            ..Self::empty()
        }
    }
}
