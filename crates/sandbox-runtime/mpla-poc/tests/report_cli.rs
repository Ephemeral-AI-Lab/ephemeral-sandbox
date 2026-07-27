use std::fs;
use std::process::Command;

use uuid::Uuid;

#[test]
fn executable_seals_and_verifies_evidence() {
    let root = std::env::temp_dir().join(format!("mpla-report-cli-{}", Uuid::new_v4()));
    fs::create_dir(&root).expect("evidence root");
    fs::write(root.join("artifact.json"), b"{}\n").expect("artifact");
    let binary = env!("CARGO_BIN_EXE_mpla-poc");

    let seal = Command::new(binary)
        .args([
            "evidence-seal",
            "--evidence-root",
            root.to_str().expect("utf8 root"),
        ])
        .output()
        .expect("seal command");
    assert!(
        seal.status.success(),
        "{}",
        String::from_utf8_lossy(&seal.stderr)
    );

    let verify = Command::new(binary)
        .args([
            "evidence-verify",
            "--evidence-root",
            root.to_str().expect("utf8 root"),
        ])
        .output()
        .expect("verify command");
    assert!(
        verify.status.success(),
        "{}",
        String::from_utf8_lossy(&verify.stderr)
    );
    let body: serde_json::Value = serde_json::from_slice(&verify.stdout).expect("JSON receipt");
    assert_eq!(body["verified"], true);
    fs::remove_dir_all(&root).expect("cleanup exact test root");
}
