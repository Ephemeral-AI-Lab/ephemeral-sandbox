use std::path::PathBuf;

use thiserror::Error;

use crate::merge::MergeConflict;
use crate::yaml;

/// Errors produced by sandbox config loading and section deserialization.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ConfigError {
    /// The sandbox workspace root could not be resolved from the crate layout.
    #[error("failed to resolve sandbox workspace root from {manifest_dir}")]
    WorkspaceRoot { manifest_dir: PathBuf },

    /// A production or test config file could not be read.
    #[error("failed to read config file {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// A config file contained invalid YAML.
    #[error("failed to parse YAML config file {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: yaml::Error,
    },

    /// A merged config document could not be serialized back to YAML.
    #[error("failed to serialize YAML config document: {source}")]
    Serialize {
        #[source]
        source: yaml::Error,
    },

    /// A test override path violated the sandbox config path policy.
    #[error("invalid test override path {path}: {reason}")]
    InvalidOverridePath { path: PathBuf, reason: String },

    /// A top-level config section was requested but not present.
    #[error("missing config section {section}")]
    MissingSection { section: String },

    /// The YAML document root must be an object.
    #[error("config document root must be a YAML mapping")]
    InvalidDocumentRoot,

    /// Deep merge failed.
    #[error(transparent)]
    Merge(#[from] MergeConflict),

    /// A top-level section failed typed deserialization.
    #[error("failed to deserialize config section {section}: {source}")]
    DeserializeSection {
        section: String,
        #[source]
        source: serde_path_to_error::Error<yaml::Error>,
    },

    /// A complete config document failed typed deserialization.
    #[error("failed to deserialize config document: {source}")]
    DeserializeDocument {
        #[source]
        source: serde_path_to_error::Error<yaml::Error>,
    },
}
