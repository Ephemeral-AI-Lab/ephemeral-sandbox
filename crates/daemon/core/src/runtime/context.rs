use config::configs::daemon::FileLimitsConfig;
use serde_json::Value;

use crate::error::DaemonError;
use crate::invocation_registry::InFlightRegistry;
use crate::trace::{RequestTraceEvent, RequestTraceEventSink};
use crate::RuntimeServices;

/// Per-dispatch daemon services used by handlers that need runtime state.
#[derive(Clone, Default)]
pub struct DispatchContext<'ctx> {
    services: Option<&'ctx RuntimeServices>,
    invocation_registry: Option<&'ctx InFlightRegistry>,
    file_limits: Option<FileLimitsConfig>,
    trace_events: Option<RequestTraceEventSink>,
    trace_id: Option<String>,
    request_id: Option<String>,
}

impl<'ctx> DispatchContext<'ctx> {
    /// Empty context for direct unit dispatch.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            services: None,
            invocation_registry: None,
            file_limits: None,
            trace_events: None,
            trace_id: None,
            request_id: None,
        }
    }

    /// Context carrying the server's owned services.
    #[must_use]
    pub const fn with_services(services: &'ctx RuntimeServices) -> Self {
        Self {
            services: Some(services),
            invocation_registry: None,
            file_limits: None,
            trace_events: None,
            trace_id: None,
            request_id: None,
        }
    }

    /// Context carrying the server's invocation registry.
    #[must_use]
    pub const fn with_invocation_registry(invocation_registry: &'ctx InFlightRegistry) -> Self {
        Self {
            services: None,
            invocation_registry: Some(invocation_registry),
            file_limits: None,
            trace_events: None,
            trace_id: None,
            request_id: None,
        }
    }

    /// Context carrying the server's services, invocation registry, and file
    /// byte limits.
    #[must_use]
    pub const fn with_runtime_config(
        services: &'ctx RuntimeServices,
        invocation_registry: &'ctx InFlightRegistry,
        file_limits: FileLimitsConfig,
    ) -> Self {
        Self {
            services: Some(services),
            invocation_registry: Some(invocation_registry),
            file_limits: Some(file_limits),
            trace_events: None,
            trace_id: None,
            request_id: None,
        }
    }

    #[must_use]
    pub(crate) fn with_trace_events(mut self, trace_events: RequestTraceEventSink) -> Self {
        self.trace_events = Some(trace_events);
        self
    }

    #[must_use]
    pub(crate) fn with_trace_identity(mut self, trace_id: String, request_id: String) -> Self {
        self.trace_id = Some(trace_id);
        self.request_id = Some(request_id);
        self
    }

    /// The owned daemon services, when threaded. Operations that can degrade
    /// (e.g. isolated-workspace routing checks) treat `None` as "no state".
    pub(crate) const fn services(&self) -> Option<&'ctx RuntimeServices> {
        self.services
    }

    /// The owned daemon services, required. Operations that cannot operate
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
    /// fall back to the `config` defaults when this is `None`.
    pub(crate) const fn file_limits(&self) -> Option<FileLimitsConfig> {
        self.file_limits
    }

    pub(crate) fn trace_id(&self) -> Option<&str> {
        self.trace_id.as_deref()
    }

    pub(crate) fn request_id(&self) -> Option<&str> {
        self.request_id.as_deref()
    }

    pub(crate) fn record_trace_event(
        &self,
        module: impl Into<String>,
        name: impl Into<String>,
        details: Value,
    ) {
        if let Some(events) = &self.trace_events {
            events.push(RequestTraceEvent::operation(module, name, details));
        }
    }
}
