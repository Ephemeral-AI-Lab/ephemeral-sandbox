//! Request-scoped sandbox provisioning: `prepare_for_run` either starts an
//! explicit sandbox id or creates a fresh one labelled `origin=workflow,
//! request_id=<id>`.
//!
//! The production path uses typed calls into [`SandboxLifecycle`] wired by
//! `eos-runtime`; test substitutability comes from the `#[cfg(test)]` mock
//! adapter. A created sandbox always has a valid id because
//! [`SandboxInfo::id`](crate::SandboxInfo) is a non-empty `SandboxId`
//! (parse-don't-validate).

use std::sync::Arc;

use async_trait::async_trait;
use eos_types::{RequestId, SandboxId};

use crate::error::SandboxHostError;
use crate::lifecycle::SandboxLifecycle;
use crate::provider::{CreateSandboxSpec, Labels};

/// The resolved sandbox↔request binding produced by the provisioner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestSandboxBinding {
    /// The sandbox the request runs in.
    pub sandbox_id: SandboxId,
    /// The originating request.
    pub request_id: RequestId,
}

/// Request-scoped sandbox provisioning contract.
///
/// This is the host-side boundary runtime composition depends on: callers either
/// provide an explicit sandbox id to start, or ask the host to create and bind a
/// fresh request sandbox.
#[async_trait]
pub trait RequestProvisioner: Send + Sync + std::fmt::Debug {
    /// Resolve the sandbox binding for one request.
    async fn prepare_for_run(
        &self,
        request_id: &RequestId,
        sandbox_id: Option<&str>,
    ) -> Result<RequestSandboxBinding, SandboxHostError>;
}

/// Builds the create spec for the fresh-sandbox branch: a `request-<8 hex>`
/// name, the `origin=workflow, request_id=<id>` labels, and the configured
/// Docker default snapshot when present (AC-09).
pub(crate) fn fresh_create_spec(
    request_id: &RequestId,
    default_snapshot: Option<&str>,
) -> CreateSandboxSpec {
    let mut labels = Labels::new();
    labels.insert("origin".to_owned(), "workflow".to_owned());
    labels.insert("request_id".to_owned(), request_id.to_string());
    CreateSandboxSpec {
        name: format!(
            "request-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        ),
        snapshot: clean_optional_text(default_snapshot),
        labels,
        ..Default::default()
    }
}

fn clean_optional_text(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// Provisions the sandbox a request runs in, over the typed lifecycle seam.
#[derive(Debug)]
pub struct RequestSandboxProvisioner {
    lifecycle: Arc<SandboxLifecycle>,
    default_snapshot: Option<String>,
}

impl RequestSandboxProvisioner {
    /// Build a provisioner with the configured Docker default snapshot used for
    /// fresh sandbox creation when the caller did not provide an explicit id.
    #[must_use]
    pub fn with_default_snapshot(
        lifecycle: Arc<SandboxLifecycle>,
        default_snapshot: Option<&str>,
    ) -> Self {
        Self {
            lifecycle,
            default_snapshot: clean_optional_text(default_snapshot),
        }
    }

    /// Prepare the sandbox for a run: start an explicit id (return value
    /// discarded), or create a fresh labelled sandbox. `request_id` flows
    /// through unchanged (never trimmed); the explicit/created ids are trimmed.
    pub async fn prepare_for_run(
        &self,
        request_id: &RequestId,
        sandbox_id: Option<&str>,
    ) -> Result<RequestSandboxBinding, SandboxHostError> {
        let explicit_id = sandbox_id.map(str::trim).filter(|s| !s.is_empty());
        if let Some(explicit) = explicit_id {
            // The explicit id is non-empty, so it parses (SandboxId only rejects
            // empty). start's return value is intentionally discarded.
            let id: SandboxId = explicit
                .parse()
                .map_err(|_| SandboxHostError::InvalidRequest("empty sandbox id".to_owned()))?;
            self.lifecycle.start(&id).await?;
            return Ok(RequestSandboxBinding {
                sandbox_id: id,
                request_id: request_id.clone(),
            });
        }
        let info = self
            .lifecycle
            .create(&fresh_create_spec(
                request_id,
                self.default_snapshot.as_deref(),
            ))
            .await?;
        Ok(RequestSandboxBinding {
            sandbox_id: info.id,
            request_id: request_id.clone(),
        })
    }
}

#[async_trait]
impl RequestProvisioner for RequestSandboxProvisioner {
    async fn prepare_for_run(
        &self,
        request_id: &RequestId,
        sandbox_id: Option<&str>,
    ) -> Result<RequestSandboxBinding, SandboxHostError> {
        RequestSandboxProvisioner::prepare_for_run(self, request_id, sandbox_id).await
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use std::path::PathBuf;
    use std::sync::Arc;

    use super::*;
    use crate::daemon_client::DaemonClient;
    use crate::registry::ProviderRegistry;
    use crate::support::MockAdapter;

    fn provisioner(adapter: MockAdapter) -> RequestSandboxProvisioner {
        let registry = ProviderRegistry::new();
        registry.set_default(Arc::new(adapter));
        let lifecycle = SandboxLifecycle::new(
            Arc::new(DaemonClient::new(Arc::new(registry))),
            PathBuf::from("/nonexistent"),
        );
        RequestSandboxProvisioner::with_default_snapshot(Arc::new(lifecycle), None)
    }

    fn rid() -> RequestId {
        "req-1".parse().unwrap()
    }

    #[test]
    fn fresh_create_spec_has_request_name_and_labels() {
        let spec = fresh_create_spec(&rid(), None);
        assert!(spec.name.starts_with("request-"));
        assert_eq!(spec.name.len(), "request-".len() + 8);
        assert!(spec.name["request-".len()..]
            .bytes()
            .all(|b| b.is_ascii_hexdigit()));
        assert_eq!(
            spec.labels.get("origin").map(String::as_str),
            Some("workflow")
        );
        assert_eq!(
            spec.labels.get("request_id").map(String::as_str),
            Some("req-1")
        );
        assert_eq!(spec.language, "python");
        assert!(spec.snapshot.is_none());
    }

    #[test]
    fn fresh_create_spec_applies_configured_snapshot() {
        let spec = fresh_create_spec(&rid(), Some("  py:3.11  "));
        assert_eq!(spec.snapshot.as_deref(), Some("py:3.11"));

        let blank = fresh_create_spec(&rid(), Some("  "));
        assert!(blank.snapshot.is_none());
    }

    // AC-09: explicit-id path starts that id and binds it; fresh path creates and
    // binds the created id. (Setup is a no-op: the mock returns no project_dir.)
    #[tokio::test]
    async fn prepare_explicit_and_fresh() {
        // explicit id (whitespace-trimmed).
        let prov = provisioner(MockAdapter::new().with_id("box"));
        let binding = prov
            .prepare_for_run(&rid(), Some("  sb-explicit  "))
            .await
            .unwrap();
        assert_eq!(binding.sandbox_id.as_str(), "sb-explicit");
        assert_eq!(binding.request_id.as_str(), "req-1");

        // fresh create (blank/None id → create branch; binding uses the created id).
        let prov = provisioner(MockAdapter::new().with_id("created-box"));
        let binding = prov.prepare_for_run(&rid(), None).await.unwrap();
        assert_eq!(binding.sandbox_id.as_str(), "created-box");
        assert_eq!(binding.request_id.as_str(), "req-1");

        // whitespace-only id is treated as "no id" (create branch).
        let prov = provisioner(MockAdapter::new().with_id("created-box"));
        let binding = prov.prepare_for_run(&rid(), Some("   ")).await.unwrap();
        assert_eq!(binding.sandbox_id.as_str(), "created-box");
    }
}
