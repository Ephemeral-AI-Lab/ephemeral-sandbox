use serde::de::DeserializeOwned;
use serde::de::IntoDeserializer;

use std::fs;
use std::path::Path;

use crate::error::ConfigError;
use crate::merge;
use crate::yaml::{self, Mapping, Value};

/// Parsed sandbox configuration document.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfigDocument {
    value: Value,
}

impl ConfigDocument {
    pub(crate) fn read(path: &Path) -> Result<Self, ConfigError> {
        let text = fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse(path, &text)
    }

    pub(crate) fn parse(path: &Path, text: &str) -> Result<Self, ConfigError> {
        let value = yaml::from_str(text).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(Self { value })
    }

    pub(crate) fn merge(&mut self, override_doc: Self) -> Result<(), ConfigError> {
        merge::ensure_mapping_root(&self.value)?;
        merge::merge_values(&mut self.value, override_doc.value)?;
        Ok(())
    }

    /// Serialize the current document to YAML.
    ///
    /// This is primarily used by test harnesses that load one approved
    /// `*.test.yml` override and need to hand the merged document to a daemon
    /// that still loads its production filename.
    ///
    /// # Errors
    /// Returns an error if the in-memory YAML value cannot be serialized.
    pub fn to_yaml_string(&self) -> Result<String, ConfigError> {
        yaml::to_string(&self.value).map_err(|source| ConfigError::Serialize { source })
    }

    /// Deserialize a top-level section into its owner crate's typed schema.
    ///
    /// # Errors
    /// Returns an error if the section is missing or if typed deserialization
    /// fails.
    pub fn section<T>(&self, name: &str) -> Result<T, ConfigError>
    where
        T: DeserializeOwned,
    {
        let section = self
            .root_mapping()?
            .get(Value::String(name.to_owned()))
            .ok_or_else(|| ConfigError::MissingSection {
                section: name.to_owned(),
            })?;
        let section = section.clone();
        let deserializer = section.into_deserializer();
        serde_path_to_error::deserialize(deserializer).map_err(|source| {
            ConfigError::DeserializeSection {
                section: name.to_owned(),
                source,
            }
        })
    }

    /// Deserialize the complete document into an owner crate's typed schema.
    ///
    /// # Errors
    /// Returns an error if typed deserialization fails.
    pub fn document<T>(&self) -> Result<T, ConfigError>
    where
        T: DeserializeOwned,
    {
        let deserializer = self.value.clone().into_deserializer();
        serde_path_to_error::deserialize(deserializer)
            .map_err(|source| ConfigError::DeserializeDocument { source })
    }

    fn root_mapping(&self) -> Result<&Mapping, ConfigError> {
        match &self.value {
            Value::Mapping(mapping) => Ok(mapping),
            _ => Err(ConfigError::InvalidDocumentRoot),
        }
    }
}
