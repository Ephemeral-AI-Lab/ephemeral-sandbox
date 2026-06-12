use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailBudget {
    RequestArgsSummary,
    SpanFields,
    EventDetails,
    ResponseSummary,
    HeartbeatDetails,
    SidecarRecord,
    Custom(usize),
}

impl DetailBudget {
    #[must_use]
    pub const fn bytes(self) -> usize {
        match self {
            Self::RequestArgsSummary => 4 * 1024,
            Self::SpanFields => 2 * 1024,
            Self::EventDetails => 1024,
            Self::ResponseSummary => 2 * 1024,
            Self::HeartbeatDetails => 4 * 1024,
            Self::SidecarRecord => 64 * 1024,
            Self::Custom(bytes) => bytes,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BoundedJson {
    pub value: Value,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    pub original_len: usize,
}

impl BoundedJson {
    #[must_use]
    pub fn capture(value: Value, budget: DetailBudget) -> Self {
        let serialized = serde_json::to_vec(&value).expect("serde_json::Value serializes");
        if serialized.len() <= budget.bytes() {
            return Self {
                value,
                truncated: false,
                sha256: None,
                original_len: serialized.len(),
            };
        }

        Self {
            value: json!({
                "truncated": true,
                "sha256": sha256_hex(&serialized),
                "original_len": serialized.len(),
            }),
            truncated: true,
            sha256: Some(sha256_hex(&serialized)),
            original_len: serialized.len(),
        }
    }

    #[must_use]
    pub fn empty_object() -> Self {
        Self::capture(json!({}), DetailBudget::Custom(2))
    }

    #[must_use]
    pub fn encoded_value(&self) -> String {
        serde_json::to_string(&self.value).expect("serde_json::Value serializes")
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}
