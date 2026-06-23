//! Shared loader for the sandbox configuration document.
//!
//! This crate owns file loading, path validation, YAML parsing, merge semantics,
//! and typed schemas for gateway, CLI, daemon, runner, and runtime config
//! surfaces.

pub mod configs;
mod document;
mod error;
mod merge;
mod paths;
mod yaml;

use std::path::Path;

pub use document::ConfigDocument;
pub use error::ConfigError;
pub use paths::ConfigPath;

/// Load a sandbox configuration document from an explicit path.
///
/// # Errors
/// Returns an error when the path cannot be read or parsed.
pub fn load_path(path: impl AsRef<Path>) -> Result<ConfigDocument, ConfigError> {
    ConfigDocument::read(path.as_ref())
}

/// Load `prd.yml`, merge one test-local `*.test.yml` override, and return the
/// merged document.
///
/// The path parameter is for test code only; this crate intentionally exposes no
/// CLI or environment variable config path selection.
///
/// # Errors
/// Returns an error when the override path is not a valid sandbox-local
/// `*.test.yml`, when either file cannot be read or parsed, or when merging
/// fails.
pub fn load_test_override(path: impl AsRef<Path>) -> Result<ConfigDocument, ConfigError> {
    let prd = ConfigPath::prd()?;
    let override_path = ConfigPath::test_override(path.as_ref())?;
    let mut baseline = ConfigDocument::read(prd.as_path())?;
    let override_doc = ConfigDocument::read(override_path.as_path())?;
    baseline.merge(override_doc)?;
    Ok(baseline)
}
