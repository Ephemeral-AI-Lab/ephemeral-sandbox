use eos_types::{JsonObject, ToolUseId};
use serde_json::json;

use super::*;

fn ask_advisor_turn(tool_use_id: &str, target_tool: &str) -> Message {
    let mut input = JsonObject::new();
    input.insert("tool_name".to_owned(), json!(target_tool));
    Message {
        role: MessageRole::Assistant,
        content: vec![ContentBlock::ToolUse {
            tool_use_id: tool_use_id.parse::<ToolUseId>().expect("id"),
            name: "ask_advisor".to_owned(),
            input,
        }],
    }
}

fn advisor_result(tool_use_id: &str, verdict: Option<&str>, is_error: bool) -> Message {
    let mut metadata = JsonObject::new();
    metadata.insert("helper_role".to_owned(), json!("advisor"));
    if let Some(v) = verdict {
        metadata.insert("verdict".to_owned(), json!(v));
    }
    Message {
        role: MessageRole::User,
        content: vec![ContentBlock::ToolResult {
            tool_use_id: tool_use_id.parse::<ToolUseId>().expect("id"),
            content: "review".to_owned(),
            is_error,
            metadata,
            is_terminal: false,
        }],
    }
}

fn classify(messages: &[Message], target: ToolName) -> Option<&'static str> {
    let (result, originating) = find_latest_advisor_pair(messages);
    classify_advisor_approval(result, originating, target)
}

#[test]
fn missing_when_no_advisor_result() {
    let msgs = [Message::from_user_text("hi")];
    assert_eq!(
        classify(&msgs, ToolName::SubmitRootTaskOutcome),
        Some("missing")
    );
}

#[test]
fn approve_paired_to_target_passes() {
    let msgs = [
        ask_advisor_turn("t1", "submit_root_task_outcome"),
        advisor_result("t1", Some("approve"), false),
    ];
    assert_eq!(classify(&msgs, ToolName::SubmitRootTaskOutcome), None);
}

#[test]
fn classification_order_covers_each_tag() {
    // advisor_failed: result is an error.
    let failed = [
        ask_advisor_turn("t1", "submit_root_task_outcome"),
        advisor_result("t1", Some("approve"), true),
    ];
    assert_eq!(
        classify(&failed, ToolName::SubmitRootTaskOutcome),
        Some("advisor_failed")
    );

    // structural: verdict not in the valid set.
    let structural = [
        ask_advisor_turn("t1", "submit_root_task_outcome"),
        advisor_result("t1", None, false),
    ];
    assert_eq!(
        classify(&structural, ToolName::SubmitRootTaskOutcome),
        Some("structural")
    );

    // rejected.
    let rejected = [
        ask_advisor_turn("t1", "submit_root_task_outcome"),
        advisor_result("t1", Some("reject"), false),
    ];
    assert_eq!(
        classify(&rejected, ToolName::SubmitRootTaskOutcome),
        Some("rejected")
    );

    // unpaired: an advisor result with no originating ask_advisor.
    let unpaired = [advisor_result("t1", Some("approve"), false)];
    assert_eq!(
        classify(&unpaired, ToolName::SubmitRootTaskOutcome),
        Some("unpaired")
    );

    // wrong_tool: ask_advisor targeted a different terminal.
    let wrong = [
        ask_advisor_turn("t1", "submit_plan_outcome"),
        advisor_result("t1", Some("approve"), false),
    ];
    assert_eq!(
        classify(&wrong, ToolName::SubmitRootTaskOutcome),
        Some("wrong_tool")
    );
}
