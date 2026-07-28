//! Typed schema for the runtime section of `eos-sandbox/config/prd.yml`.
//!
//! The sandbox daemon loads this section and injects it into sandbox-runtime
//! services during startup.

use std::path::PathBuf;

use serde::Deserialize;

use crate::configs::validate::{
    require_f64_at_least, require_f64_gt, require_i32_in_range, require_u64_at_least,
    require_unix_absolute, require_usize_at_least, require_usize_at_most, ConfigFieldError,
};

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeConfig {
    pub workspace: WorkspaceConfig,
    pub namespace_execution: NamespaceExecutionConfig,
    #[serde(default)]
    pub layerstack: LayerstackConfig,
    #[serde(default)]
    pub command: CommandConfig,
    #[serde(default)]
    pub file: FileConfig,
    /// Server-owned storage-helper profile selection. Public MPLA requests
    /// must echo this fixed policy and cannot use the field to self-select.
    #[serde(default)]
    pub mpla_storage_admin: MplaStorageAdminConfig,
}

impl RuntimeConfig {
    /// Validate semantic constraints that YAML deserialization cannot express.
    ///
    /// # Errors
    /// Returns an error when a field violates runtime policy.
    pub fn validate(&self) -> Result<(), ConfigFieldError> {
        self.workspace.validate()?;
        self.namespace_execution.validate()?;
        reject_runtime_scratch_overlap(
            &self.workspace.scratch_root,
            &self.workspace.layer_stack_root,
            "runtime.workspace.scratch_root",
        )?;
        if let Some(legacy_root) = self.namespace_execution.scratch_root.as_deref() {
            reject_runtime_scratch_overlap(
                legacy_root,
                &self.workspace.scratch_root,
                "runtime.namespace_execution.scratch_root",
            )?;
            reject_runtime_scratch_overlap(
                legacy_root,
                &self.workspace.layer_stack_root,
                "runtime.namespace_execution.scratch_root",
            )?;
        }
        self.layerstack.validate()?;
        self.command.validate()?;
        self.file.validate()?;
        self.mpla_storage_admin.validate()
    }
}

/// The capability profile loaded by the daemon for its fixed MPLA helper.
/// Production stays at `production`; the qualification variant is explicit
/// and has no environment-, image-, or host-specific branches.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MplaStorageAdminProfile {
    #[default]
    Production,
    OverlayfsDacOverrideQualification,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MplaStorageAdminConfig {
    pub profile: MplaStorageAdminProfile,
}

impl MplaStorageAdminConfig {
    /// The type admits only the two explicitly enumerated profiles.  Keeping
    /// validation explicit makes this a policy boundary rather than a free
    /// form capability configuration.
    pub const fn validate(&self) -> Result<(), ConfigFieldError> {
        Ok(())
    }
}

/// Command-operation caps the daemon injects into the command service.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CommandConfig {
    /// Concurrent live commands the engine admits.
    pub max_active: usize,
    /// `read_command_lines` window when the caller names no limit.
    pub read_lines_default: usize,
    /// Hard cap on a caller-requested `read_command_lines` window.
    pub read_lines_max: usize,
}

impl Default for CommandConfig {
    fn default() -> Self {
        Self {
            max_active: 32,
            read_lines_default: 200,
            read_lines_max: 1000,
        }
    }
}

impl CommandConfig {
    /// Validate semantic constraints that YAML deserialization cannot express.
    ///
    /// # Errors
    /// Returns an error when a field violates command-operation policy.
    pub fn validate(&self) -> Result<(), ConfigFieldError> {
        require_usize_at_least(self.max_active, 1, "runtime.command.max_active")?;
        require_usize_at_most(self.max_active, 1024, "runtime.command.max_active")?;
        require_usize_at_least(
            self.read_lines_default,
            1,
            "runtime.command.read_lines_default",
        )?;
        require_usize_at_least(self.read_lines_max, 1, "runtime.command.read_lines_max")?;
        if self.read_lines_default > self.read_lines_max {
            return Err(ConfigFieldError::new(
                "runtime.command.read_lines_default",
                "must not exceed read_lines_max",
            ));
        }
        Ok(())
    }
}

/// File-operation caps the daemon injects into the file service.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FileConfig {
    /// `file_read` line window when the caller names no limit.
    pub read_lines_default: usize,
    /// Byte cap on one rendered read window.
    pub max_output_bytes: usize,
    /// Byte cap on a file accepted for ordered edits.
    pub max_edit_bytes: usize,
    /// Entry cap for one directory listing.
    pub max_list_entries: usize,
}

impl Default for FileConfig {
    fn default() -> Self {
        Self {
            read_lines_default: 2000,
            max_output_bytes: 256 * 1024,
            max_edit_bytes: 4 * 1024 * 1024,
            max_list_entries: 2000,
        }
    }
}

impl FileConfig {
    /// Validate semantic constraints that YAML deserialization cannot express.
    ///
    /// # Errors
    /// Returns an error when a field violates file-operation policy.
    pub fn validate(&self) -> Result<(), ConfigFieldError> {
        require_usize_at_least(
            self.read_lines_default,
            1,
            "runtime.file.read_lines_default",
        )?;
        require_usize_at_least(self.max_output_bytes, 1, "runtime.file.max_output_bytes")?;
        require_usize_at_least(self.max_edit_bytes, 1, "runtime.file.max_edit_bytes")?;
        require_usize_at_least(self.max_list_entries, 1, "runtime.file.max_list_entries")
    }
}

/// Layer-stack tuning knobs the daemon injects into the runtime layerstack
/// service at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LayerstackConfig {
    pub rollout_mode: StorageRolloutMode,
    /// Remount-sweep concurrency width. The default is the measured
    /// `sweep_wall` knee (`W-tuning.md`, `N=200`/`M=1.0`): overlap peaks and
    /// wall bottoms out at `W=4`; `1` restores the serial sweep.
    pub remount_sweep_width: usize,
    /// Byte cap per `read_export_chunk` page (the export stream fallback).
    pub export_chunk_bytes: u64,
    /// zstd compression level for the export spool.
    pub spool_zstd_level: i32,
    /// Optional internal maintenance policies. Omitting the section keeps
    /// autosquash disabled for custom configurations.
    pub autosquash_policies: AutosquashPoliciesConfig,
}

impl Default for LayerstackConfig {
    fn default() -> Self {
        Self {
            rollout_mode: StorageRolloutMode::Legacy,
            remount_sweep_width: 4,
            export_chunk_bytes: 2 * 1024 * 1024,
            spool_zstd_level: 3,
            autosquash_policies: AutosquashPoliciesConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageRolloutMode {
    #[default]
    Legacy,
    Validation,
    StrictCandidate,
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
        )?;
        if let Some(threshold) = self.autosquash_policies.squash_at_n_layers {
            require_usize_at_least(
                threshold,
                3,
                "runtime.layerstack.autosquash_policies.squash_at_n_layers",
            )?;
        }
        Ok(())
    }
}

/// Optional autosquash policy values. Each omitted policy is disabled.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AutosquashPoliciesConfig {
    pub squash_at_n_layers: Option<usize>,
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
        require_unix_absolute(&self.layer_stack_root, "runtime.workspace.layer_stack_root")?;
        require_unix_absolute(&self.scratch_root, "runtime.workspace.scratch_root")?;
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

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NamespaceExecutionConfig {
    /// Deprecated global execution root. New writes never use this path; when
    /// present it is only a bounded boot-time compatibility-reaper input.
    pub scratch_root: Option<PathBuf>,
    /// Freeze-poll budget for the remount quiesce, in seconds. Measured on the
    /// supported environment: the full stop → poll-`T` → membership-recheck
    /// shape for 100 tasks takes under 4 ms, so the 0.5 s default bounds only
    /// D-state stragglers.
    #[serde(default = "default_freeze_budget_s")]
    pub freeze_budget_s: f64,
    /// PTY stdin backpressure deadline, in seconds.
    #[serde(default = "default_stdin_write_deadline_s")]
    pub stdin_write_deadline_s: f64,
    /// Terminal registry entries retained after completion.
    #[serde(default = "default_max_terminal_entries")]
    pub max_terminal_entries: usize,
    /// Byte window scanned from the transcript tail per read.
    #[serde(default = "default_max_transcript_window_bytes")]
    pub max_transcript_window_bytes: u64,
    /// ns-runner result-pipe drain cap in bytes.
    #[serde(default = "default_max_runner_result_bytes")]
    pub max_runner_result_bytes: usize,
}

impl Default for NamespaceExecutionConfig {
    fn default() -> Self {
        Self {
            scratch_root: Some(PathBuf::from("/eos/namespace_execution")),
            freeze_budget_s: default_freeze_budget_s(),
            stdin_write_deadline_s: default_stdin_write_deadline_s(),
            max_terminal_entries: default_max_terminal_entries(),
            max_transcript_window_bytes: default_max_transcript_window_bytes(),
            max_runner_result_bytes: default_max_runner_result_bytes(),
        }
    }
}

impl NamespaceExecutionConfig {
    /// Validate semantic constraints that YAML deserialization cannot express.
    ///
    /// # Errors
    /// Returns an error when a field violates namespace-execution runtime policy.
    pub fn validate(&self) -> Result<(), ConfigFieldError> {
        if let Some(scratch_root) = self.scratch_root.as_deref() {
            require_unix_absolute(scratch_root, "runtime.namespace_execution.scratch_root")?;
            reject_dangerous_root(scratch_root, "runtime.namespace_execution.scratch_root")?;
        }
        require_f64_at_least(
            self.freeze_budget_s,
            0.0,
            "runtime.namespace_execution.freeze_budget_s",
        )?;
        require_f64_gt(
            self.stdin_write_deadline_s,
            0.0,
            "runtime.namespace_execution.stdin_write_deadline_s",
        )?;
        require_usize_at_least(
            self.max_terminal_entries,
            1,
            "runtime.namespace_execution.max_terminal_entries",
        )?;
        require_u64_at_least(
            self.max_transcript_window_bytes,
            1,
            "runtime.namespace_execution.max_transcript_window_bytes",
        )?;
        require_usize_at_least(
            self.max_runner_result_bytes,
            1,
            "runtime.namespace_execution.max_runner_result_bytes",
        )?;
        Ok(())
    }
}

fn reject_runtime_scratch_overlap(
    left: &std::path::Path,
    right: &std::path::Path,
    field: &'static str,
) -> Result<(), ConfigFieldError> {
    if left.starts_with(right) || right.starts_with(left) {
        Err(ConfigFieldError::new(
            field,
            format!("must not overlap {}", right.display()),
        ))
    } else {
        Ok(())
    }
}

fn default_freeze_budget_s() -> f64 {
    0.5
}

fn default_stdin_write_deadline_s() -> f64 {
    2.0
}

fn default_max_terminal_entries() -> usize {
    512
}

fn default_max_transcript_window_bytes() -> u64 {
    1024 * 1024
}

fn default_max_runner_result_bytes() -> usize {
    8 * 1024 * 1024
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
