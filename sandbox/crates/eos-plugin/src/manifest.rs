//! Plugin manifest contracts.

use std::collections::{BTreeMap, BTreeSet};

use eos_protocol::Intent;
use serde::{Deserialize, Serialize};

use crate::error::{PluginError, Result};
use crate::service::{
    require_non_empty, validate_identifier, validate_plugin_id, RefreshStrategy, ServiceMode,
};

/// Digest marker written after a package tree is accepted.
pub const PACKAGE_SHA256_MARKER: &str = ".package-sha256";

/// Digest marker written after setup completes successfully.
pub const SETUP_SHA256_MARKER: &str = ".setup-sha256";

/// Top-level plugin manifest consumed by `api.plugin.ensure`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginManifest {
    pub plugin_id: String,
    pub plugin_version: String,
    pub plugin_digest: String,
    #[serde(default)]
    pub package: PluginPackageManifest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup: Option<PluginSetupManifest>,
    #[serde(default)]
    pub services: Vec<PluginServiceManifest>,
    #[serde(default)]
    pub operations: Vec<PluginOperationManifest>,
}

impl PluginManifest {
    /// Validate manifest identity, service uniqueness, and operation references.
    ///
    /// # Errors
    ///
    /// Returns [`PluginError::Manifest`] when identifiers are malformed, required
    /// fields are empty, services or operations are duplicated, or an operation
    /// references an unknown service.
    pub fn validate(&self) -> Result<()> {
        validate_plugin_id("plugin_id", &self.plugin_id)?;
        require_non_empty("plugin_version", &self.plugin_version)?;
        require_non_empty("plugin_digest", &self.plugin_digest)?;
        self.package.validate()?;
        if let Some(setup) = &self.setup {
            setup.validate()?;
        }

        let mut service_ids = BTreeSet::new();
        let mut service_modes = BTreeMap::new();
        for service in &self.services {
            service.validate()?;
            if !service_ids.insert(service.service_id.as_str()) {
                return Err(PluginError::Manifest(format!(
                    "duplicate service_id {}",
                    service.service_id
                )));
            }
            service_modes.insert(service.service_id.as_str(), service.service_mode);
        }

        let mut op_names = BTreeSet::new();
        for operation in &self.operations {
            operation.validate(&service_modes)?;
            if !op_names.insert(operation.op_name.as_str()) {
                return Err(PluginError::Manifest(format!(
                    "duplicate op_name {}",
                    operation.op_name
                )));
            }
        }
        Ok(())
    }

    /// Digest recorded in [`PACKAGE_SHA256_MARKER`] for package idempotency.
    #[must_use]
    pub fn package_marker_digest(&self) -> &str {
        &self.plugin_digest
    }

    /// Digest recorded in [`SETUP_SHA256_MARKER`] after setup succeeds.
    #[must_use]
    pub fn setup_marker_digest(&self) -> Option<&str> {
        self.setup
            .as_ref()
            .map(|setup| setup.setup_marker_digest.as_str())
    }
}

/// Package-root contract for an installed plugin payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginPackageManifest {
    #[serde(default = "default_runtime_dir")]
    pub runtime_dir: String,
    #[serde(default)]
    pub dependency_scope: PluginDependencyScope,
}

impl Default for PluginPackageManifest {
    fn default() -> Self {
        Self {
            runtime_dir: default_runtime_dir(),
            dependency_scope: PluginDependencyScope::PackageDigest,
        }
    }
}

impl PluginPackageManifest {
    fn validate(&self) -> Result<()> {
        validate_relative_package_path("package.runtime_dir", &self.runtime_dir)
    }
}

/// Dependency isolation scope for package-managed runtime dependencies.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum PluginDependencyScope {
    /// Dependencies live under `/eos/runtime/packages/<plugin>/<digest>/`.
    #[default]
    PackageDigest,
}

/// Optional setup command executed by the daemon after package publish.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginSetupManifest {
    pub command: Vec<String>,
    pub working_dir: String,
    pub setup_marker_digest: String,
    pub timeout_ms: u64,
}

impl PluginSetupManifest {
    fn validate(&self) -> Result<()> {
        if self.command.is_empty() {
            return Err(PluginError::Manifest(
                "setup.command must not be empty".to_owned(),
            ));
        }
        validate_relative_package_path("setup.working_dir", &self.working_dir)?;
        require_non_empty("setup_marker_digest", &self.setup_marker_digest)?;
        if self.timeout_ms == 0 {
            return Err(PluginError::Manifest(
                "setup.timeout_ms must be positive".to_owned(),
            ));
        }
        Ok(())
    }
}

/// One service declared by a plugin payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginServiceManifest {
    pub service_id: String,
    pub service_profile_digest: String,
    pub service_mode: ServiceMode,
    pub refresh_strategy: RefreshStrategy,
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
    #[serde(default = "default_ppc_protocol")]
    pub ppc_protocol_version: u32,
}

impl PluginServiceManifest {
    /// Validate this service declaration.
    ///
    /// # Errors
    ///
    /// Returns [`PluginError::Manifest`] when the service identity/profile is
    /// invalid, the PPC protocol is zero, or an executable service mode lacks a
    /// launch command.
    pub fn validate(&self) -> Result<()> {
        validate_identifier("service_id", &self.service_id)?;
        require_non_empty("service_profile_digest", &self.service_profile_digest)?;
        if self.ppc_protocol_version == 0 {
            return Err(PluginError::Manifest(
                "ppc_protocol_version must be positive".to_owned(),
            ));
        }
        if let Some(working_dir) = &self.working_dir {
            validate_relative_package_path("service.working_dir", working_dir)?;
        }
        if matches!(
            self.service_mode,
            ServiceMode::WorkspaceSnapshotRefresh | ServiceMode::OneshotOverlay
        ) && self.command.is_empty()
        {
            return Err(PluginError::Manifest(format!(
                "service {} requires a launch command",
                self.service_id
            )));
        }
        Ok(())
    }
}

/// One public `plugin.<plugin>.<op>` operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginOperationManifest {
    pub op_name: String,
    pub intent: Intent,
    #[serde(default = "default_auto_workspace_overlay")]
    pub auto_workspace_overlay: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

impl PluginOperationManifest {
    fn validate(&self, service_modes: &BTreeMap<&str, ServiceMode>) -> Result<()> {
        require_non_empty("op_name", &self.op_name)?;
        if self.intent == Intent::Lifecycle {
            return Err(PluginError::Manifest(
                "Intent::Lifecycle is reserved for sandbox lifecycle ops".to_owned(),
            ));
        }
        if self.intent == Intent::ReadOnly && self.service_id.is_none() {
            return Err(PluginError::Manifest(format!(
                "read-only op {} must reference a service_id",
                self.op_name
            )));
        }
        if let Some(service_id) = &self.service_id {
            validate_identifier("service_id", service_id)?;
            if !service_modes.contains_key(service_id.as_str()) {
                return Err(PluginError::Manifest(format!(
                    "op {} references unknown service_id {}",
                    self.op_name, service_id
                )));
            }
        }
        if self.intent == Intent::WriteAllowed && self.auto_workspace_overlay {
            let Some(service_id) = &self.service_id else {
                return Err(PluginError::Manifest(format!(
                    "write op {} with auto_workspace_overlay requires an oneshot_overlay service",
                    self.op_name
                )));
            };
            if service_modes.get(service_id.as_str()) != Some(&ServiceMode::OneshotOverlay) {
                return Err(PluginError::Manifest(format!(
                    "write op {} with auto_workspace_overlay requires an oneshot_overlay service",
                    self.op_name
                )));
            }
        }
        if self.timeout_ms == Some(0) {
            return Err(PluginError::Manifest(format!(
                "op {} timeout_ms must be positive",
                self.op_name
            )));
        }
        Ok(())
    }
}

const fn default_ppc_protocol() -> u32 {
    1
}

const fn default_auto_workspace_overlay() -> bool {
    true
}

fn default_runtime_dir() -> String {
    "runtime".to_owned()
}

fn validate_relative_package_path(field: &str, value: &str) -> Result<()> {
    require_non_empty(field, value)?;
    if value.starts_with('/') {
        return Err(PluginError::Manifest(format!("{field} must be relative")));
    }
    for component in value.split('/') {
        match component {
            ".." => {
                return Err(PluginError::Manifest(format!(
                    "{field} must not contain path traversal"
                )));
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = std::result::Result<(), PluginError>;

    fn manifest() -> PluginManifest {
        PluginManifest {
            plugin_id: "generic".to_owned(),
            plugin_version: "0.1.0".to_owned(),
            plugin_digest: "digest-a".to_owned(),
            package: PluginPackageManifest::default(),
            setup: Some(PluginSetupManifest {
                command: vec!["./setup.sh".to_owned()],
                working_dir: "runtime".to_owned(),
                setup_marker_digest: "setup-a".to_owned(),
                timeout_ms: 30_000,
            }),
            services: vec![PluginServiceManifest {
                service_id: "worker".to_owned(),
                service_profile_digest: "profile-a".to_owned(),
                service_mode: ServiceMode::WorkspaceSnapshotRefresh,
                refresh_strategy: RefreshStrategy::RemountWorkspaceAndNotify,
                command: vec!["generic-service".to_owned(), "--stdio".to_owned()],
                working_dir: Some("runtime".to_owned()),
                ppc_protocol_version: 1,
            }],
            operations: vec![PluginOperationManifest {
                op_name: "hover".to_owned(),
                intent: Intent::ReadOnly,
                auto_workspace_overlay: true,
                service_id: Some("worker".to_owned()),
                timeout_ms: Some(5_000),
            }],
        }
    }

    #[test]
    fn validates_read_only_service_manifest() -> TestResult {
        manifest().validate()?;
        Ok(())
    }

    #[test]
    fn rejects_read_only_op_without_service() {
        let mut manifest = manifest();
        manifest.operations[0].service_id = None;
        assert!(matches!(
            manifest.validate(),
            Err(PluginError::Manifest(message)) if message.contains("must reference")
        ));
    }

    #[test]
    fn rejects_duplicate_operation_names() {
        let mut manifest = manifest();
        manifest.operations.push(manifest.operations[0].clone());
        assert!(matches!(
            manifest.validate(),
            Err(PluginError::Manifest(message)) if message.contains("duplicate op_name")
        ));
    }

    #[test]
    fn plugin_id_matches_rust_name_rule() {
        let mut valid_manifest = manifest();
        valid_manifest.plugin_id = "_Lsp9".to_owned();
        assert!(valid_manifest.validate().is_ok());

        for invalid in ["my-plugin", "my.plugin", "9plugin", ""] {
            let mut manifest = manifest();
            manifest.plugin_id = invalid.to_owned();
            assert!(matches!(
                manifest.validate(),
                Err(PluginError::Manifest(message)) if message.contains("plugin_id")
            ));
        }
    }

    #[test]
    fn op_name_is_only_non_empty_at_manifest_boundary() -> TestResult {
        let mut manifest = manifest();
        manifest.operations[0].op_name = "1 weird.op".to_owned();
        manifest.validate()?;

        manifest.operations[0].op_name = "   ".to_owned();
        assert!(matches!(
            manifest.validate(),
            Err(PluginError::Manifest(message)) if message.contains("op_name is required")
        ));
        Ok(())
    }

    #[test]
    fn accepts_package_and_setup_contract() -> TestResult {
        let manifest = manifest();
        manifest.validate()?;
        assert_eq!(manifest.package.runtime_dir, "runtime");
        assert_eq!(manifest.package_marker_digest(), "digest-a");
        assert_eq!(manifest.setup_marker_digest(), Some("setup-a"));
        assert_eq!(PACKAGE_SHA256_MARKER, ".package-sha256");
        assert_eq!(SETUP_SHA256_MARKER, ".setup-sha256");
        Ok(())
    }

    #[test]
    fn rejects_unknown_manifest_field() {
        let value = serde_json::json!({
            "plugin_id": "generic",
            "plugin_version": "0.1.0",
            "plugin_digest": "digest-a",
            "unexpected": true,
            "services": [],
            "operations": []
        });
        assert!(serde_json::from_value::<PluginManifest>(value).is_err());
    }

    #[test]
    fn rejects_package_and_setup_paths_outside_package_tree() {
        let mut absolute_package = manifest();
        absolute_package.package.runtime_dir = "/runtime".to_owned();
        assert!(matches!(
            absolute_package.validate(),
            Err(PluginError::Manifest(message)) if message.contains("package.runtime_dir")
        ));

        let mut traversing_package = manifest();
        traversing_package.package.runtime_dir = "../runtime".to_owned();
        assert!(matches!(
            traversing_package.validate(),
            Err(PluginError::Manifest(message)) if message.contains("path traversal")
        ));

        let mut traversing_setup = manifest();
        traversing_setup
            .setup
            .as_mut()
            .expect("test fixture includes setup")
            .working_dir = "runtime/../escape".to_owned();
        assert!(matches!(
            traversing_setup.validate(),
            Err(PluginError::Manifest(message)) if message.contains("setup.working_dir")
        ));
    }

    #[test]
    fn rejects_service_working_dir_outside_package_tree() {
        let mut manifest = manifest();
        manifest.services[0].working_dir = Some("/runtime".to_owned());
        assert!(matches!(
            manifest.validate(),
            Err(PluginError::Manifest(message)) if message.contains("service.working_dir")
        ));
    }

    #[test]
    fn digest_fields_drive_marker_and_service_identity() -> TestResult {
        let base = manifest();
        let mut package_changed = base.clone();
        package_changed.plugin_digest = "digest-b".to_owned();
        assert_ne!(
            base.package_marker_digest(),
            package_changed.package_marker_digest()
        );

        let mut setup_changed = base.clone();
        setup_changed
            .setup
            .as_mut()
            .expect("test fixture includes setup")
            .setup_marker_digest = "setup-b".to_owned();
        assert_ne!(
            base.setup_marker_digest(),
            setup_changed.setup_marker_digest()
        );

        let mut service_changed = base.clone();
        service_changed.services[0].service_profile_digest = "profile-b".to_owned();
        assert_ne!(
            base.services[0].service_profile_digest,
            service_changed.services[0].service_profile_digest
        );
        Ok(())
    }

    #[test]
    fn auto_overlay_write_requires_oneshot_service() {
        let mut manifest = manifest();
        manifest.operations[0].intent = Intent::WriteAllowed;
        manifest.operations[0].auto_workspace_overlay = true;
        assert!(matches!(
            manifest.validate(),
            Err(PluginError::Manifest(message)) if message.contains("oneshot_overlay")
        ));

        manifest.services[0].service_mode = ServiceMode::OneshotOverlay;
        manifest.services[0].refresh_strategy = RefreshStrategy::RestartService;
        assert!(manifest.validate().is_ok());
    }

    #[test]
    fn oneshot_overlay_service_requires_command() {
        let mut manifest = manifest();
        manifest.services[0].service_mode = ServiceMode::OneshotOverlay;
        manifest.services[0].command.clear();
        assert!(matches!(
            manifest.validate(),
            Err(PluginError::Manifest(message)) if message.contains("requires a launch command")
        ));
    }
}
