//! The single typed error enum for this crate (spec-conventions §8,
//! `err-thiserror-lib`).

use std::path::PathBuf;

/// Failures raised when loading, parsing, or validating an agent profile.
///
/// `#[non_exhaustive]` because the set may grow (`api-non-exhaustive`); messages
/// are lowercase with no trailing punctuation (`err-lowercase-msg`) and chain the
/// underlying cause via `#[source]` (`err-source-chain`).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AgentDefError {
    /// A resolved agent `name` was empty after trimming surrounding whitespace.
    #[error("agent name must be non-empty")]
    EmptyName,

    /// `terminals` was empty (or all-blank) — every agent must declare at least
    /// one terminal-capable tool.
    #[error("agent definition terminals must be non-empty")]
    EmptyTerminals,

    /// `tool_call_limit` was not strictly positive.
    #[error("tool_call_limit must be positive")]
    NonPositiveToolCallLimit,

    /// The profile file could not be read from disk.
    #[error("could not read agent profile {}", path.display())]
    Read {
        /// The file (or directory) that could not be read.
        path: PathBuf,
        /// The underlying I/O failure.
        #[source]
        cause: std::io::Error,
    },

    /// The YAML frontmatter failed to parse, or carried an unknown key.
    #[error("invalid frontmatter in {}", path.display())]
    Frontmatter {
        /// The profile file whose frontmatter failed to parse.
        path: PathBuf,
        /// The underlying YAML deserialization failure.
        #[source]
        cause: serde_yaml::Error,
    },

    /// A declared `skill:` path did not resolve to an existing file.
    #[error("agent profile {} declares skill {declared}, but {} does not exist", path.display(), resolved.display())]
    SkillNotFound {
        /// The profile file that declared the skill.
        path: PathBuf,
        /// The relative `skill:` value as authored.
        declared: String,
        /// The resolved (absolute) path that did not exist.
        resolved: PathBuf,
    },
}
