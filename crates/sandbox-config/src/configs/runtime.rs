//! Typed schema for the runtime section of `eos-sandbox/config/prd.yml`.
//!
//! The sandbox daemon loads this section and injects it into sandbox-runtime
//! services during startup.

use std::path::PathBuf;

use serde::Deserialize;

use crate::configs::validate::{
    require_absolute, require_f64_at_least, require_f64_gt, require_i32_in_range,
    require_u64_at_least, require_usize_at_least, ConfigFieldError,
};

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeConfig {
    pub workspace: WorkspaceConfig,
    pub namespace_execution: NamespaceExecutionConfig,
    #[serde(default)]
    pub layerstack: LayerstackConfig,
}

impl RuntimeConfig {
    /// Validate semantic constraints that YAML deserialization cannot express.
    ///
    /// # Errors
    /// Returns an error when a field violates runtime policy.
    pub fn validate(&self) -> Result<(), ConfigFieldError> {
        self.workspace.validate()?;
        self.namespace_execution.validate()?;
        self.layerstack.validate()
    }
}

/// Layer-stack tuning knobs the daemon injects into the runtime layerstack
/// service at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LayerstackConfig {
    /// Remount-sweep concurrency width. The default is the measured
    /// `sweep_wall` knee (`W-tuning.md`, `N=200`/`M=1.0`): overlap peaks and
    /// wall bottoms out at `W=4`; `1` restores the serial sweep.
    pub remount_sweep_width: usize,
    /// Byte cap per `read_export_chunk` page (the export stream fallback).
    pub export_chunk_bytes: u64,
    /// zstd compression level for the export spool.
    pub spool_zstd_level: i32,
}

impl Default for LayerstackConfig {
    fn default() -> Self {
        Self {
            remount_sweep_width: 4,
            export_chunk_bytes: 2 * 1024 * 1024,
            spool_zstd_level: 3,
        }
    }
}

impl LayerstackConfig {
    /// Validate semantic constraints that YAML deserialization cannot express.
    ///
    /// # Errors
    /// Returns an error when a field violates layerstack runtime policy.
    pub fn validate(&self) -> Result<(), ConfigFieldError> {
        require_usize_at_least(
            self.remount_sweep_width,
            1,
            "runtime.layerstack.remount_sweep_width",
        )?;
        require_u64_at_least(
            self.export_chunk_bytes,
            1,
            "runtime.layerstack.export_chunk_bytes",
        )?;
        require_i32_in_range(
            self.spool_zstd_level,
            1,
            22,
            "runtime.layerstack.spool_zstd_level",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceConfig {
    pub layer_stack_root: PathBuf,
    pub scratch_root: PathBuf,
    pub setup_timeout_s: f64,
    pub exit_grace_s: f64,
    pub rfc1918_egress: Rfc1918Egress,
}

impl Default for WorkspaceConfig {
    fn default() -> Self {
        Self {
            layer_stack_root: PathBuf::from("/eos/layer-stack"),
            scratch_root: PathBuf::from("/eos/workspace"),
            setup_timeout_s: 30.0,
            exit_grace_s: 0.25,
            rfc1918_egress: Rfc1918Egress::Allow,
        }
    }
}

impl WorkspaceConfig {
    /// Validate semantic constraints that YAML deserialization cannot express.
    ///
    /// # Errors
    /// Returns an error when a field violates workspace runtime policy.
    pub fn validate(&self) -> Result<(), ConfigFieldError> {
        require_absolute(&self.layer_stack_root, "runtime.workspace.layer_stack_root")?;
        require_absolute(&self.scratch_root, "runtime.workspace.scratch_root")?;
        require_f64_gt(
            self.setup_timeout_s,
            0.0,
            "runtime.workspace.setup_timeout_s",
        )?;
        require_f64_at_least(self.exit_grace_s, 0.0, "runtime.workspace.exit_grace_s")?;
        reject_dangerous_root(&self.layer_stack_root, "runtime.workspace.layer_stack_root")?;
        reject_dangerous_root(&self.scratch_root, "runtime.workspace.scratch_root")?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NamespaceExecutionConfig {
    pub scratch_root: PathBuf,
}

impl Default for NamespaceExecutionConfig {
    fn default() -> Self {
        Self {
            scratch_root: PathBuf::from("/eos/namespace_execution"),
        }
    }
}

impl NamespaceExecutionConfig {
    /// Validate semantic constraints that YAML deserialization cannot express.
    ///
    /// # Errors
    /// Returns an error when a field violates namespace-execution runtime policy.
    pub fn validate(&self) -> Result<(), ConfigFieldError> {
        require_absolute(
            &self.scratch_root,
            "runtime.namespace_execution.scratch_root",
        )?;
        reject_dangerous_root(
            &self.scratch_root,
            "runtime.namespace_execution.scratch_root",
        )?;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rfc1918Egress {
    Allow,
    Deny,
}

fn reject_dangerous_root(
    path: &std::path::Path,
    field: &'static str,
) -> Result<(), ConfigFieldError> {
    if is_filesystem_root(path) {
        return Err(ConfigFieldError::new(
            field,
            "must not be the filesystem root",
        ));
    }
    Ok(())
}

fn is_filesystem_root(path: &std::path::Path) -> bool {
    path.parent().is_none()
        || path
            .canonicalize()
            .ok()
            .is_some_and(|canonical| canonical.parent().is_none())
}
