//! Typed schema for the runner section of `sandbox/config/prd.yml`.
//!
//! The namespace runner loads this section for mount masking. Runtime
//! environment policy is still wired by the existing runner helpers until the
//! config-infra wiring phase.

use std::path::PathBuf;

use serde::Deserialize;

use crate::configs::validate::{
    require_absolute, require_non_empty, require_non_empty_items, require_u64_at_least,
    ConfigFieldError,
};

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunnerConfig {
    pub child_wait_poll_ms: u64,
    pub env: RunnerEnvConfig,
    pub mount_mask: RunnerMountMaskConfig,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunnerEnvConfig {
    pub inherit_keys: Vec<String>,
    pub restricted_keys: Vec<String>,
    pub default_path: String,
    pub testbed_path_prefix: Vec<String>,
    pub git_optional_locks: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunnerMountMaskConfig {
    pub hidden_paths: Vec<PathBuf>,
}

impl RunnerConfig {
    /// Validate semantic constraints that YAML deserialization cannot express.
    ///
    /// # Errors
    /// Returns an error when a field violates runner policy.
    pub fn validate(&self) -> Result<(), ConfigFieldError> {
        require_u64_at_least(self.child_wait_poll_ms, 1, "runner.child_wait_poll_ms")?;
        require_non_empty_items(&self.env.inherit_keys, "runner.env.inherit_keys")?;
        require_non_empty_items(&self.env.restricted_keys, "runner.env.restricted_keys")?;
        require_non_empty(&self.env.default_path, "runner.env.default_path")?;
        require_non_empty_items(
            &self.env.testbed_path_prefix,
            "runner.env.testbed_path_prefix",
        )?;
        if self.mount_mask.hidden_paths.is_empty() {
            return Err(ConfigFieldError::new(
                "runner.mount_mask.hidden_paths",
                "must not be empty",
            ));
        }
        for path in &self.mount_mask.hidden_paths {
            require_absolute(path, "runner.mount_mask.hidden_paths")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_prd_runner_section_deserializes_and_validates() {
        prd_config().validate().expect("prd runner config is valid");
    }

    #[test]
    fn config_validation_rejects_invalid_runner_values() {
        let mut cfg = prd_config();
        cfg.child_wait_poll_ms = 0;
        assert_invalid(cfg, "runner.child_wait_poll_ms");

        let mut cfg = prd_config();
        cfg.env.inherit_keys.push(String::new());
        assert_invalid(cfg, "runner.env.inherit_keys");

        let mut cfg = prd_config();
        cfg.env.default_path.clear();
        assert_invalid(cfg, "runner.env.default_path");

        let mut cfg = prd_config();
        cfg.mount_mask.hidden_paths.clear();
        assert_invalid(cfg, "runner.mount_mask.hidden_paths");

        let mut cfg = prd_config();
        cfg.mount_mask.hidden_paths.push(PathBuf::from("relative"));
        assert_invalid(cfg, "runner.mount_mask.hidden_paths");
    }

    fn prd_config() -> RunnerConfig {
        crate::load_prd()
            .expect("prd config loads")
            .section("runner")
            .expect("runner section deserializes")
    }

    fn assert_invalid(config: RunnerConfig, field: &str) {
        let err = config.validate().expect_err("config should be invalid");
        let message = err.to_string();
        assert!(message.contains(field), "{message}");
    }
}
