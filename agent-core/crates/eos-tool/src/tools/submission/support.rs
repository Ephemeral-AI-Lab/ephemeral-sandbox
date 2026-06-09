//! Shared terminal submission helpers.

use eos_types::{JsonObject, SubmissionAck, SubmissionStatus};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ToolResult;

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub(in crate::tools::submission) struct OutcomeInput {
    pub(in crate::tools::submission) status: SubmissionStatus,
    pub(in crate::tools::submission) outcome: String,
}

pub(in crate::tools::submission) fn is_blank(s: &str) -> bool {
    s.trim().is_empty()
}

pub(in crate::tools::submission) fn meta_obj(pairs: &[(&str, Value)]) -> JsonObject {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect()
}

pub(in crate::tools::submission) fn submission_ack_result(
    ack: SubmissionAck,
    success: &str,
    metadata: &JsonObject,
) -> ToolResult {
    match ack {
        SubmissionAck::Accepted => ToolResult::ok(success).with_metadata(metadata.clone()),
        SubmissionAck::Rejected(message) => ToolResult::error(message),
    }
}
