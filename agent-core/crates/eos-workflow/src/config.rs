//! Workflow lifecycle and per-attempt runtime tunables.

use serde::{Deserialize, Serialize};

use eos_types::{AttemptBudget, ConfigError};

/// Per-Attempt run-stage tunables.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct AttemptConfig {
    /// Per-attempt cap on concurrently launched worker agent runs.
    /// Range-checked `>= 1` by [`AttemptConfig::validate`].
    pub max_concurrent_task_runs: usize,
}

impl Default for AttemptConfig {
    fn default() -> Self {
        Self {
            max_concurrent_task_runs: 8,
        }
    }
}

impl AttemptConfig {
    /// Enforce numeric-range constraints.
    ///
    /// # Errors
    /// Returns [`ConfigError::OutOfRange`] when `max_concurrent_task_runs < 1`.
    pub fn validate(&self) -> Result<(), ConfigError> {
        self.validate_with_field("attempt.max_concurrent_task_runs")
    }

    pub(crate) fn validate_with_field(&self, field: &str) -> Result<(), ConfigError> {
        if self.max_concurrent_task_runs < 1 {
            return Err(ConfigError::OutOfRange {
                field: field.to_owned(),
                detail: "must be >= 1".to_owned(),
            });
        }
        Ok(())
    }
}

/// Default deepest workflow depth still allowed to defer.
pub const DEFAULT_WORKFLOW_MAX_DEPTH: u32 = 2;

/// Workflow runtime configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct WorkflowConfig {
    /// Deepest workflow depth still allowed to set a deferred goal.
    #[serde(default = "default_workflow_max_depth", rename = "max-depth")]
    pub max_depth: u32,
    /// Per-attempt run tunables.
    #[serde(default)]
    pub attempt: AttemptConfig,
}

impl Default for WorkflowConfig {
    fn default() -> Self {
        Self {
            max_depth: DEFAULT_WORKFLOW_MAX_DEPTH,
            attempt: AttemptConfig::default(),
        }
    }
}

impl WorkflowConfig {
    /// Enforce numeric-range constraints.
    ///
    /// # Errors
    /// Returns [`ConfigError::OutOfRange`] when `max-depth < 1` or the nested
    /// attempt config is invalid.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.max_depth < 1 {
            return Err(ConfigError::OutOfRange {
                field: "workflow.max-depth".to_owned(),
                detail: "must be >= 1".to_owned(),
            });
        }
        self.attempt
            .validate_with_field("workflow.attempt.max_concurrent_task_runs")
    }
}

/// Per-workflow lifecycle knobs injected by backend composition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WorkflowLifecycleConfig {
    /// Attempts allowed per iteration before the iteration closes failed.
    pub default_attempt_budget: AttemptBudget,
}

const fn default_workflow_max_depth() -> u32 {
    DEFAULT_WORKFLOW_MAX_DEPTH
}
