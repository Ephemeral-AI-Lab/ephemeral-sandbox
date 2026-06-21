#[path = "../src/configs/mod.rs"]
pub mod configs;
#[path = "../src/document.rs"]
mod document;
#[path = "../src/error.rs"]
mod error;
#[path = "../src/merge.rs"]
mod merge;
#[path = "../src/paths.rs"]
mod paths;
#[path = "../src/yaml.rs"]
mod yaml;

use std::path::Path;

pub use document::ConfigDocument;
pub use error::ConfigError;
pub use paths::ConfigPath;

pub fn load_baseline() -> Result<ConfigDocument, ConfigError> {
    let path = ConfigPath::prd()?;
    load_path(path.as_path())
}

pub fn load_path(path: impl AsRef<Path>) -> Result<ConfigDocument, ConfigError> {
    ConfigDocument::read(path.as_ref())
}

pub fn load_test_override(path: impl AsRef<Path>) -> Result<ConfigDocument, ConfigError> {
    let prd = ConfigPath::prd()?;
    let override_path = ConfigPath::test_override(path.as_ref())?;
    let mut baseline = ConfigDocument::read(prd.as_path())?;
    let override_doc = ConfigDocument::read(override_path.as_path())?;
    baseline.merge(override_doc)?;
    Ok(baseline)
}

#[path = "unit/lib.rs"]
mod lib_tests;

mod daemon_tests {
    pub(crate) use crate::configs::daemon::*;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/unit/configs/daemon.rs"
    ));
}

mod isolated_tests {
    pub(crate) use crate::configs::isolated::*;
    pub(crate) use std::path::PathBuf;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/unit/configs/isolated.rs"
    ));
}

mod runner_tests {
    pub(crate) use crate::configs::runner::*;
    pub(crate) use std::path::PathBuf;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/unit/configs/runner.rs"
    ));
}
