use std::path::Path;
use std::process::{Command, Output};

use uuid::Uuid;

#[test]
fn executable_dispatches_suite_and_exact_smoke_cases_through_a_test_process() {
    let root = std::env::temp_dir().join(format!("mpla-suite-dispatch-{}", Uuid::new_v4()));
    std::fs::create_dir(&root).expect("create dispatch root");
    let binary = env!("CARGO_BIN_EXE_mpla-poc");
    let current_test = std::env::current_exe().expect("current test binary");

    for arguments in [
        vec!["suite", "smoke"],
        vec!["test", "SM-03", "--samples", "3"],
    ] {
        let status = Command::new(binary)
            .args(arguments)
            .env("MPLA_POC_CAMPAIGN_TEST_BIN", &current_test)
            .env("MPLA_POC_DISPATCH_TEST_ROOT", &root)
            .status()
            .expect("run PoC dispatcher");
        assert!(status.success());
    }

    let suite: serde_json::Value = serde_json::from_slice(
        &std::fs::read(root.join("suite.json")).expect("read suite dispatch"),
    )
    .expect("suite JSON");
    assert_eq!(suite["mode"], "suite");
    assert_eq!(suite["samples"], "1");
    assert!(suite["case"].is_null());

    let case: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("test.json")).expect("read test dispatch"))
            .expect("test JSON");
    assert_eq!(case["mode"], "test");
    assert_eq!(case["samples"], "3");
    assert_eq!(case["case"], "SM-03");
    std::fs::remove_dir_all(root).expect("remove dispatch root");
}

#[test]
fn lifecycle_metadata_command_persists_typed_success_failure_and_cancellation() {
    let root = std::env::temp_dir().join(format!("mpla-lifecycle-cli-{}", Uuid::new_v4()));
    let binary = env!("CARGO_BIN_EXE_mpla-poc");

    let initialize = invoke_lifecycle(
        binary,
        &root,
        &[
            "--operation-id",
            "initialize-main",
            "--action",
            "initialize",
            "--branch",
            "main",
            "--allocation-id",
            "allocation-1",
            "--root-id",
            "root-1",
            "--attribution-root-id",
            "attribution-1",
        ],
    );
    assert_eq!(initialize["status"], "succeeded");
    assert_eq!(initialize["selection"]["branch"], "main");
    assert_eq!(initialize["payload_objects_created"], 0);

    let fork = invoke_lifecycle(
        binary,
        &root,
        &[
            "--operation-id",
            "fork-0",
            "--action",
            "fork",
            "--branch",
            "fork-0",
            "--source",
            "main",
        ],
    );
    assert_eq!(fork["status"], "succeeded");
    assert_eq!(fork["selection"]["branch"], "fork-0");

    let rollback = invoke_lifecycle(
        binary,
        &root,
        &[
            "--operation-id",
            "rollback-main",
            "--action",
            "rollback",
            "--branch",
            "main",
            "--target",
            "fork-0",
        ],
    );
    assert_eq!(rollback["status"], "succeeded");
    assert_eq!(rollback["selection"]["sequence"], 2);

    let squash = invoke_lifecycle(
        binary,
        &root,
        &[
            "--operation-id",
            "squash-main",
            "--action",
            "squash",
            "--branch",
            "main",
        ],
    );
    assert_eq!(squash["status"], "succeeded");
    assert_eq!(squash["selection"]["sequence"], 3);
    assert_eq!(squash["selection"]["ancestry"], serde_json::json!([3]));

    let failure = invoke_lifecycle(
        binary,
        &root,
        &[
            "--operation-id",
            "missing-source",
            "--action",
            "fork",
            "--branch",
            "unselected",
            "--source",
            "missing",
        ],
    );
    assert_eq!(failure["status"], "failed");
    assert!(failure["selection"].is_null());
    assert!(failure["selector_path"].is_null());
    assert!(failure["error"]
        .as_str()
        .is_some_and(|error| error.contains("missing.json")));

    let cancelled = invoke_lifecycle(
        binary,
        &root,
        &[
            "--operation-id",
            "cancelled-rollback",
            "--action",
            "rollback",
            "--branch",
            "main",
            "--target",
            "fork-0",
            "--cancel",
        ],
    );
    assert_eq!(cancelled["status"], "cancelled");
    assert!(cancelled["selection"].is_null());
    assert!(cancelled["selector_path"].is_null());

    let replay = invoke_lifecycle(
        binary,
        &root,
        &[
            "--operation-id",
            "fork-0",
            "--action",
            "fork",
            "--branch",
            "fork-0",
            "--source",
            "missing",
        ],
    );
    assert_eq!(replay, fork);

    let mut branches = std::fs::read_dir(root.join("branches"))
        .expect("read lifecycle branches")
        .map(|entry| {
            entry
                .expect("read lifecycle branch entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect::<Vec<_>>();
    branches.sort();
    assert_eq!(branches, ["fork-0.json", "main.json"]);

    let outcomes = std::fs::read_dir(root.join("outcomes"))
        .expect("read lifecycle outcomes")
        .count();
    assert_eq!(outcomes, 6);
    std::fs::remove_dir_all(root).expect("remove lifecycle root");
}

fn invoke_lifecycle(binary: &str, state_root: &Path, arguments: &[&str]) -> serde_json::Value {
    let output = Command::new(binary)
        .arg("lifecycle-metadata")
        .arg("--state-root")
        .arg(state_root)
        .args(arguments)
        .output()
        .expect("run lifecycle metadata command");
    assert_success(&output);
    serde_json::from_slice(&output.stdout).expect("decode lifecycle metadata receipt")
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "command failed with status {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[ignore = "invoked only through mpla-poc suite/test"]
fn m1_smoke_campaign() {
    let root = std::path::PathBuf::from(
        std::env::var_os("MPLA_POC_DISPATCH_TEST_ROOT").expect("dispatch test root"),
    );
    let mode = std::env::var("MPLA_POC_CAMPAIGN_MODE").expect("campaign mode");
    let case = std::env::var("MPLA_POC_CASE_FILTER").ok();
    let samples = std::env::var("MPLA_POC_SAMPLES").expect("samples");
    let path = root.join(format!("{mode}.json"));
    std::fs::write(
        path,
        serde_json::to_vec(&serde_json::json!({
            "mode": mode,
            "case": case,
            "samples": samples,
        }))
        .expect("dispatch JSON"),
    )
    .expect("write dispatch witness");
}
