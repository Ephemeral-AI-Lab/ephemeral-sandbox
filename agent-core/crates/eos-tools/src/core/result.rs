//! In-band tool result contracts.

use eos_types::JsonObject;
use serde::de::DeserializeOwned;

/// A normalized in-band tool result. Both success and tool-domain failure are
/// values of this type; only framework faults are `Err(crate::ToolError)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResult {
    /// The model-facing output text.
    pub output: String,
    /// Whether this is an in-band tool-domain error.
    pub is_error: bool,
    /// Heterogeneous result metadata.
    pub metadata: JsonObject,
    /// Set by the tool pipeline when a terminal tool succeeds.
    pub is_terminal: bool,
}

impl ToolResult {
    /// A successful plain result.
    #[must_use]
    pub fn ok(output: impl Into<String>) -> Self {
        Self {
            output: output.into(),
            is_error: false,
            metadata: JsonObject::new(),
            is_terminal: false,
        }
    }

    /// An in-band tool-domain error result.
    #[must_use]
    pub fn error(output: impl Into<String>) -> Self {
        Self {
            output: output.into(),
            is_error: true,
            metadata: JsonObject::new(),
            is_terminal: false,
        }
    }

    /// Attach result metadata.
    #[must_use]
    pub fn with_metadata(mut self, metadata: JsonObject) -> Self {
        self.metadata = metadata;
        self
    }

    /// Insert one metadata key.
    #[must_use]
    pub fn meta(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.metadata.insert(key.into(), value);
        self
    }
}

/// The declared shape of a tool's successful output.
#[derive(Clone)]
pub enum OutputShape {
    /// Plain text.
    Text,
    /// Structured JSON that must deserialize into the named model.
    Json {
        /// The output model name.
        model_name: &'static str,
        /// Validator for the serialized output string.
        validate: fn(&str) -> Result<(), String>,
    },
}

impl std::fmt::Debug for OutputShape {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OutputShape::Text => f.write_str("OutputShape::Text"),
            OutputShape::Json { model_name, .. } => {
                write!(f, "OutputShape::Json({model_name})")
            }
        }
    }
}

impl OutputShape {
    /// Build a [`OutputShape::Json`] for output model `T`.
    #[must_use]
    pub fn json<T: DeserializeOwned>(model_name: &'static str) -> Self {
        OutputShape::Json {
            model_name,
            validate: validate_json::<T>,
        }
    }
}

fn validate_json<T: DeserializeOwned>(output: &str) -> Result<(), String> {
    serde_json::from_str::<T>(output)
        .map(|_| ())
        .map_err(|err| err.to_string())
}
