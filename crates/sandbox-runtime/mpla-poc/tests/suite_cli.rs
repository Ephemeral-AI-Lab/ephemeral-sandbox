use std::process::Command;

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
