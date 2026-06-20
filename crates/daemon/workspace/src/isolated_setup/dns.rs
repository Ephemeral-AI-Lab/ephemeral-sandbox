#[cfg(target_os = "linux")]
use std::process::Stdio;

#[cfg(target_os = "linux")]
use ::namespace_process::runner::protocol::{NamespaceCommandRequest, RunResult};
#[cfg(target_os = "linux")]
use serde_json::{json, Value};

use crate::namespace::NamespaceRuntime;
#[cfg(target_os = "linux")]
use crate::namespace::{ns_command_request, run_child};
use crate::profile::IsolatedNetworkError;
use crate::profile::{DnsConfiguration, WorkspaceModeHandle};

impl NamespaceRuntime {
    pub(crate) fn configure_dns(
        &self,
        handle: &WorkspaceModeHandle,
        fallback_dns: &str,
        setup_timeout_s: f64,
    ) -> Result<DnsConfiguration, IsolatedNetworkError> {
        if self.bypasses_kernel_setup() || handle.holder_pid <= 0 {
            return Ok(DnsConfiguration::default());
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (handle, fallback_dns, setup_timeout_s);
            Ok(DnsConfiguration::default())
        }
        #[cfg(target_os = "linux")]
        {
            let request = ns_command_request(
                handle,
                "configure-dns",
                json!({"fallback_dns": fallback_dns}),
                Vec::new(),
            );
            configure_dns_child(&request, setup_timeout_s)
        }
    }
}

#[cfg(target_os = "linux")]
fn configure_dns_child(
    request: &NamespaceCommandRequest,
    setup_timeout_s: f64,
) -> Result<DnsConfiguration, IsolatedNetworkError> {
    let output = run_child(request, "--configure-dns", Stdio::piped(), setup_timeout_s)?;
    if !output.status.success() {
        return Err(IsolatedNetworkError::SetupFailed {
            step: format!(
                "ns-runner configure dns failed with status {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            ),
        });
    }
    let result = serde_json::from_slice::<RunResult>(&output.stdout).map_err(|err| {
        IsolatedNetworkError::SetupFailed {
            step: format!("invalid ns-runner configure dns output: {err}"),
        }
    })?;
    Ok(DnsConfiguration {
        fallback_applied: result
            .payload
            .get("applied_fallback")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        previous_first_nameserver: result
            .payload
            .get("previous_first_nameserver")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}
