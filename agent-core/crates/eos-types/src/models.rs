//! Shared model-registry config, config errors, and transitional JSON aliases.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Errors raised while loading, parsing, or deserializing config.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    /// A requested top-level config section was absent from the document.
    #[error("config section '{section}' is missing")]
    MissingSection {
        /// The missing top-level section name.
        section: String,
    },
    /// The config document root was not a YAML mapping.
    #[error("config document root must be a YAML mapping")]
    InvalidDocumentRoot,
    /// A network database url (a `postgres`/`mysql` scheme, or a credentialed
    /// `//host` authority) was supplied; agent-core is sqlite-only and rejects it.
    #[error("network database urls are not supported in agent-core: {0}")]
    NetworkDatabaseUrl(String),
    /// A database url that is neither a `sqlite:` scheme nor a local `.db` path.
    #[error("unsupported database url (expected local sqlite): {0}")]
    UnsupportedDatabaseUrl(String),
    /// A numeric field fell outside its allowed range.
    #[error("config value '{field}' is out of range: {detail}")]
    OutOfRange {
        /// Dotted config path of the offending field (e.g. `database.pool_size`).
        field: String,
        /// The range constraint that was violated (e.g. `must be >= 1`).
        detail: String,
    },
    /// A required config value is absent or empty.
    #[error("config value '{field}' is required")]
    MissingValue {
        /// Dotted config path of the missing value.
        field: String,
    },
    /// A config file could not be read from disk.
    #[error("failed to read config file")]
    ReadFile(#[source] std::io::Error),
    /// A config file or a deserialized section failed to parse.
    #[error("failed to parse config yaml")]
    ParseYaml(#[source] serde_yaml::Error),
}

/// Untyped JSON object map used for transitional metadata (plan §1). The owned
/// transitional-metadata contract enumerated in spec-conventions §5.
pub type JsonObject = serde_json::Map<String, serde_json::Value>;

/// Model-registry seed configuration.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ModelsConfig {
    /// Active model key. Runtime synthesizes this row when it is not present in
    /// `registrations`, so simple configs can set only this field.
    #[serde(default)]
    pub active: Option<String>,
    /// Configured model registration rows.
    #[serde(default)]
    pub registrations: Vec<ModelRegistrationConfig>,
}

impl ModelsConfig {
    /// Borrow the trimmed active key when configured.
    #[must_use]
    pub fn active_key(&self) -> Option<&str> {
        self.active
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
    }

    /// Enforce config-shape constraints.
    ///
    /// # Errors
    /// Returns [`ConfigError`] when the active key is blank, a registration key is
    /// blank, or registration keys are duplicated.
    pub fn validate(&self) -> Result<(), ConfigError> {
        self.validate_at("models")
    }

    /// Enforce config-shape constraints using a caller-supplied field prefix.
    ///
    /// # Errors
    /// Returns [`ConfigError`] when the active key is blank, a registration key is
    /// blank, or registration keys are duplicated.
    pub fn validate_at(&self, field_prefix: &str) -> Result<(), ConfigError> {
        if self
            .active
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            return Err(ConfigError::MissingValue {
                field: format!("{field_prefix}.active"),
            });
        }

        let mut keys = BTreeSet::new();
        for (idx, registration) in self.registrations.iter().enumerate() {
            let field = format!("{field_prefix}.registrations[{idx}].key");
            let key = registration.key();
            if key.is_empty() {
                return Err(ConfigError::MissingValue { field });
            }
            if !keys.insert(key.to_owned()) {
                return Err(ConfigError::OutOfRange {
                    field,
                    detail: "must be unique".to_owned(),
                });
            }
        }

        Ok(())
    }

    /// Return the active registration, synthesizing one from `active` when the
    /// active key is not explicitly listed.
    #[must_use]
    pub fn active_registration(&self) -> Option<ModelRegistrationConfig> {
        let active_key = self.active_key()?;
        self.registrations
            .iter()
            .find(|registration| registration.key() == active_key)
            .cloned()
            .or_else(|| {
                Some(ModelRegistrationConfig {
                    key: active_key.to_owned(),
                    label: None,
                    class_path: String::new(),
                    kwargs: Map::new(),
                })
            })
    }
}

/// One model registration row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ModelRegistrationConfig {
    /// Persisted model key. This is the provider model id sent on `LlmRequest`.
    pub key: String,
    /// Optional human-readable label. Defaults to `key` when empty or absent.
    #[serde(default)]
    pub label: Option<String>,
    /// Migration-only class path retained for parity metadata.
    #[serde(default)]
    pub class_path: String,
    /// Opaque provider/model kwargs stored with the registration.
    #[serde(default)]
    pub kwargs: Map<String, Value>,
}

impl ModelRegistrationConfig {
    /// Borrow the trimmed model key.
    #[must_use]
    pub fn key(&self) -> &str {
        self.key.trim()
    }

    /// Return the label, defaulting to the trimmed model key.
    #[must_use]
    pub fn label(&self) -> String {
        self.label
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| self.key())
            .to_owned()
    }

    /// Borrow the trimmed migration-only class path.
    #[must_use]
    pub fn class_path(&self) -> &str {
        self.class_path.trim()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use serde_json::json;

    use super::*;

    #[test]
    fn defaults_are_empty() {
        let cfg = ModelsConfig::default();

        assert_eq!(cfg.active_key(), None);
        assert!(cfg.registrations.is_empty());
        cfg.validate().unwrap();
    }

    #[test]
    fn parses_registrations() {
        let cfg: ModelsConfig = serde_yaml::from_str(
            r#"
active: claude-sonnet-4-6
registrations:
  - key: claude-sonnet-4-6
    label: Claude Sonnet
    class_path: legacy.Claude
    kwargs:
      effort: high
"#,
        )
        .unwrap();

        assert_eq!(cfg.active_key(), Some("claude-sonnet-4-6"));
        assert_eq!(cfg.registrations[0].key(), "claude-sonnet-4-6");
        assert_eq!(cfg.registrations[0].label(), "Claude Sonnet");
        assert_eq!(cfg.registrations[0].class_path(), "legacy.Claude");
        assert_eq!(cfg.registrations[0].kwargs["effort"], json!("high"));
        cfg.validate().unwrap();
    }

    #[test]
    fn active_only_config_is_valid() {
        let cfg: ModelsConfig = serde_yaml::from_str("active: claude-sonnet-4-6\n").unwrap();

        assert_eq!(cfg.active_key(), Some("claude-sonnet-4-6"));
        cfg.validate().unwrap();
        assert_eq!(
            cfg.active_registration().unwrap().key(),
            "claude-sonnet-4-6"
        );
    }

    #[test]
    fn rejects_blank_and_duplicate_keys() {
        let blank_active: ModelsConfig = serde_yaml::from_str("active: '  '\n").unwrap();
        assert!(matches!(
            blank_active.validate().unwrap_err(),
            ConfigError::MissingValue { field } if field == "models.active"
        ));

        let blank_key: ModelsConfig = serde_yaml::from_str(
            r#"
registrations:
  - key: ''
"#,
        )
        .unwrap();
        assert!(matches!(
            blank_key.validate().unwrap_err(),
            ConfigError::MissingValue { field } if field == "models.registrations[0].key"
        ));

        let duplicate: ModelsConfig = serde_yaml::from_str(
            r#"
registrations:
  - key: one
  - key: one
"#,
        )
        .unwrap();
        assert!(matches!(
            duplicate.validate().unwrap_err(),
            ConfigError::OutOfRange { field, .. } if field == "models.registrations[1].key"
        ));
    }

    #[test]
    fn validate_at_uses_nested_field_prefix() {
        let cfg: ModelsConfig = serde_yaml::from_str(
            r#"
registrations:
  - key: ''
"#,
        )
        .unwrap();

        assert!(matches!(
            cfg.validate_at("providers.codex_coding_plan.models")
                .unwrap_err(),
            ConfigError::MissingValue { field }
                if field == "providers.codex_coding_plan.models.registrations[0].key"
        ));
    }
}
