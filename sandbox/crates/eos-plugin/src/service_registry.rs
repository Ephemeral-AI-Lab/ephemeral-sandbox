//! Logical plugin-service registry.
//!
//! This registry deliberately performs no process I/O. `eos-daemon` wraps this
//! contract with live process, namespace, and PPC management.

use serde::{Deserialize, Serialize};

use crate::error::{PluginError, Result};
use crate::service::PluginServiceKey;

/// Lifecycle state reported by a plugin service.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum PluginServiceState {
    Starting,
    Ready,
    Refreshing,
    Stale,
    Restarting,
    Stopped,
    Failed,
}

/// Serializable status for `api.plugin.status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginServiceStatus {
    pub key: PluginServiceKey,
    pub state: PluginServiceState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_key: Option<String>,
    #[serde(default)]
    pub registered_ops: Vec<String>,
    #[serde(default)]
    pub refresh_count: u64,
    #[serde(default)]
    pub restart_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

impl PluginServiceStatus {
    #[must_use]
    pub const fn new(key: PluginServiceKey) -> Self {
        Self {
            key,
            state: PluginServiceState::Starting,
            manifest_key: None,
            registered_ops: Vec::new(),
            refresh_count: 0,
            restart_count: 0,
            last_error: None,
        }
    }

    /// Ensure this status is current before a read-only request answers.
    ///
    /// # Errors
    ///
    /// Returns [`PluginError::ProjectionStale`] when the service is not ready or
    /// is ready for a different manifest key.
    pub fn require_ready_on_manifest(&self, target_manifest_key: &str) -> Result<()> {
        if self.state != PluginServiceState::Ready {
            return Err(PluginError::ProjectionStale(format!(
                "service is {:?}, not ready",
                self.state
            )));
        }
        if self.manifest_key.as_deref() != Some(target_manifest_key) {
            return Err(PluginError::ProjectionStale(format!(
                "service manifest {:?} is not target {}",
                self.manifest_key, target_manifest_key
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::PluginServiceKeyParts;
    use crate::service::{RefreshStrategy, ServiceMode};

    type TestResult = std::result::Result<(), PluginError>;

    fn key(profile: &str) -> Result<PluginServiceKey> {
        PluginServiceKey::new(PluginServiceKeyParts {
            layer_stack_root: "/eos/plugin/layer-stack".to_owned(),
            workspace_root: "/eos/plugin/workspace".to_owned(),
            plugin_id: "generic".to_owned(),
            plugin_digest: "digest-a".to_owned(),
            service_id: "worker".to_owned(),
            service_profile_digest: profile.to_owned(),
            service_mode: ServiceMode::WorkspaceSnapshotRefresh,
            refresh_strategy: RefreshStrategy::RemountWorkspaceAndNotify,
        })
    }

    #[test]
    fn ready_check_rejects_stale_manifest() -> TestResult {
        let mut status = PluginServiceStatus::new(key("profile-a")?);
        status.state = PluginServiceState::Ready;
        status.manifest_key = Some("manifest@1".to_owned());
        assert!(status.require_ready_on_manifest("manifest@2").is_err());
        Ok(())
    }
}
