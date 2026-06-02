// AC-workspace-07: the prompt-report golden round-trips as valid JSONL with the
// three recorder event kinds present, and the system-role anomaly is preserved
// and annotated. See parity/README.md for the annotation.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

fn parity_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn golden_has_three_event_kinds() {
    let path = parity_dir()
        .join("prompt_report")
        .join("session_golden.jsonl");
    let body = fs::read_to_string(&path).expect("read session_golden.jsonl");
    let mut kinds = BTreeSet::new();
    for line in body.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let event: serde_json::Value =
            serde_json::from_str(line).expect("jsonl line is valid json");
        let kind = event["event"].as_str().expect("event field").to_owned();
        kinds.insert(kind);
    }
    let expected: BTreeSet<String> = ["assistant", "llm_request", "tool_results"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    assert_eq!(
        kinds, expected,
        "golden must contain exactly the three recorder events"
    );
}

#[test]
fn system_role_anomaly_is_preserved() {
    let path = parity_dir()
        .join("prompt_report")
        .join("initial_messages_anomaly.json");
    let raw = fs::read_to_string(&path).expect("read initial_messages_anomaly.json");
    let value: serde_json::Value = serde_json::from_str(&raw).expect("anomaly fixture is json");
    let records = value["records"].as_array().expect("records array");
    let has_system = records
        .iter()
        .any(|record| record["role"].as_str() == Some("system"));
    assert!(
        has_system,
        "the system-role bug must be frozen in the fixture (anchor §4)"
    );
}

#[test]
fn anomaly_is_annotated_in_readme() {
    let readme = fs::read_to_string(parity_dir().join("README.md")).expect("read parity README");
    let lower = readme.to_lowercase();
    assert!(
        lower.contains("anomaly"),
        "README must annotate the anomaly"
    );
    assert!(
        lower.contains("system"),
        "README must explain the system-role bug"
    );
}
