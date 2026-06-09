//! The advisor-approval pre-hook — a **stateless** gate that infers the verdict
//! from the conversation transcript.
//!
//! There is no port and no engine/agent state: the verdict exists only as a
//! `submit_advisor_outcome` result block in the transcript, surfaced as the
//! `ask_advisor` result. The gate reverse-walks [`ExecutionMetadata::conversation`]
//! for the latest advisor result, pairs it to the originating `ask_advisor`
//! tool-use, and classifies — re-deriving the decision on demand, never reading a
//! cached verdict (advisor remediation plan §2b / §9).

use eos_llm_client::{ContentBlock, Message, MessageRole};
use eos_types::ToolUseId;

use eos_tool::{ExecutionMetadata, ToolError, ToolName};

use super::{HookDenial, HookOutcome};

const ADVISOR_HELPER_ROLE: &str = "advisor";
const VALID_VERDICTS: [&str; 2] = ["approve", "reject"];

const ADVISOR_APPROVAL_MESSAGE_PREFIX: &str =
    "BLOCKED: You must get approval from advisor before submitting this terminal. \
     Call ask_advisor(tool_name=\"";
const ADVISOR_APPROVAL_MESSAGE_SUFFIX: &str =
    "\", tool_payload=...) and resubmit only after the advisor returns verdict=\"approve\".";

fn blocked_message(tool: ToolName) -> String {
    format!(
        "{ADVISOR_APPROVAL_MESSAGE_PREFIX}{}{ADVISOR_APPROVAL_MESSAGE_SUFFIX}",
        tool.as_str()
    )
}

/// `AdvisorApprovalPreHook.run`: scan the transcript, classify, deny or pass. The
/// missing-conversation case (no advisor result) classifies `missing` and denies,
/// matching Rust's "no conversation → reason `missing`".
pub(crate) async fn run_advisor_approval(
    tool: ToolName,
    ctx: &ExecutionMetadata,
) -> Result<HookOutcome, ToolError> {
    let (result, originating) = find_latest_advisor_pair(&ctx.conversation);
    match classify_advisor_approval(result, originating, tool) {
        None => Ok(HookOutcome::pass()),
        Some(reason) => Ok(HookOutcome::Deny(
            HookDenial::new(blocked_message(tool), "advisor_approval").with_reason(reason),
        )),
    }
}

/// Reverse-walk `User` messages for the latest `tool_result` block whose metadata
/// carries `helper_role == "advisor"`; pair it with the originating `ask_advisor`
/// tool-use (matched by `tool_use_id`). Port of `_find_latest_advisor_pair`.
fn find_latest_advisor_pair(
    messages: &[Message],
) -> (Option<&ContentBlock>, Option<&ContentBlock>) {
    for msg in messages.iter().rev() {
        if msg.role != MessageRole::User {
            continue;
        }
        for block in msg.content.iter().rev() {
            if let ContentBlock::ToolResult {
                tool_use_id,
                metadata,
                ..
            } = block
            {
                if metadata.get("helper_role").and_then(|v| v.as_str()) == Some(ADVISOR_HELPER_ROLE)
                {
                    let originating = find_originating_ask_advisor(messages, tool_use_id);
                    return (Some(block), originating);
                }
            }
        }
    }
    (None, None)
}

/// Forward-walk `Assistant` messages for the `ask_advisor` tool-use with the given
/// id. Port of `_find_originating_ask_advisor`.
fn find_originating_ask_advisor<'a>(
    messages: &'a [Message],
    tool_use_id: &ToolUseId,
) -> Option<&'a ContentBlock> {
    for msg in messages {
        if msg.role != MessageRole::Assistant {
            continue;
        }
        for block in &msg.content {
            if let ContentBlock::ToolUse {
                tool_use_id: id,
                name,
                ..
            } = block
            {
                if id == tool_use_id && name == "ask_advisor" {
                    return Some(block);
                }
            }
        }
    }
    None
}

/// Classify the advisor pair against `target_tool`. `Some(tag)` denies; `None`
/// passes. Order (verbatim `_classify`): `missing` → `advisor_failed` →
/// `structural` → `rejected` → `unpaired` → `wrong_tool` → pass.
fn classify_advisor_approval(
    result: Option<&ContentBlock>,
    originating: Option<&ContentBlock>,
    target_tool: ToolName,
) -> Option<&'static str> {
    let Some(ContentBlock::ToolResult {
        is_error, metadata, ..
    }) = result
    else {
        return Some("missing");
    };
    if *is_error {
        return Some("advisor_failed");
    }
    let Some(verdict) = metadata
        .get("verdict")
        .and_then(|v| v.as_str())
        .filter(|v| VALID_VERDICTS.contains(v))
    else {
        return Some("structural");
    };
    if verdict == "reject" {
        return Some("rejected");
    }
    // verdict == "approve" — require the originating ask_advisor to target this terminal.
    let Some(ContentBlock::ToolUse { input, .. }) = originating else {
        return Some("unpaired");
    };
    if input.get("tool_name").and_then(|v| v.as_str()) != Some(target_tool.as_str()) {
        return Some("wrong_tool");
    }
    None
}

#[cfg(test)]
#[path = "../../../tests/tool_call/hooks/advisor_approval/mod.rs"]
mod tests;
