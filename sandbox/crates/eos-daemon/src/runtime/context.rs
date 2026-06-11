use eos_config::configs::daemon::FileLimitsConfig;

use crate::runtime::invocation_registry::InFlightRegistry;

/// Per-dispatch daemon services used by handlers that need runtime state.
#[derive(Clone, Copy, Default)]
pub struct DispatchContext<'ctx> {
    invocation_registry: Option<&'ctx InFlightRegistry>,
    file_limits: Option<FileLimitsConfig>,
    read_request_s: Option<f64>,
}

impl<'ctx> DispatchContext<'ctx> {
    /// Empty context for direct unit dispatch.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            invocation_registry: None,
            file_limits: None,
            read_request_s: None,
        }
    }

    /// Context carrying the server's invocation registry.
    #[must_use]
    pub const fn with_invocation_registry(invocation_registry: &'ctx InFlightRegistry) -> Self {
        Self {
            invocation_registry: Some(invocation_registry),
            file_limits: None,
            read_request_s: None,
        }
    }

    /// Context carrying the server's invocation registry, file byte limits,
    /// and measured request read duration.
    #[must_use]
    pub const fn with_runtime_config(
        invocation_registry: &'ctx InFlightRegistry,
        file_limits: FileLimitsConfig,
        read_request_s: f64,
    ) -> Self {
        Self {
            invocation_registry: Some(invocation_registry),
            file_limits: Some(file_limits),
            read_request_s: Some(read_request_s),
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
            invocation_registry: None,
            file_limits: None,
            read_request_s: Some(read_request_s),
        }
    }
}
