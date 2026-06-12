use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceError {
    pub message: String,
}

impl SourceError {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct FaultDetails {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_chain: Vec<SourceError>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub io_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal: Option<i32>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fields: BTreeMap<String, Value>,
}

impl FaultDetails {
    #[must_use]
    pub fn with_field(mut self, key: impl Into<String>, value: Value) -> Self {
        self.fields.insert(key.into(), value);
        self
    }

    #[must_use]
    pub fn with_source(mut self, message: impl Into<String>) -> Self {
        self.source_chain.push(SourceError::new(message));
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OperationFault {
    pub kind: String,
    pub message: String,
    #[serde(default)]
    pub details: FaultDetails,
}

impl OperationFault {
    #[must_use]
    pub fn new(kind: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            message: message.into(),
            details: FaultDetails::default(),
        }
    }

    #[must_use]
    pub fn with_details(mut self, details: FaultDetails) -> Self {
        self.details = details;
        self
    }

    #[must_use]
    pub fn internal(message: impl Into<String>, details: FaultDetails) -> Self {
        Self::new("internal_error", message).with_details(details.with_field(
            "error_id",
            Value::String(uuid::Uuid::new_v4().simple().to_string()),
        ))
    }
}
