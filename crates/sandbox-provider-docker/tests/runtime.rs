use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use sandbox_manager::{CreateSandboxRequest, ManagerError, SandboxRuntime, SharedBaseMount};
use sandbox_provider_docker::{DockerRuntimeConfig, DockerSandboxRuntime};

#[test]
fn create_sandbox_rejects_missing_shared_base_mount() {
    let runtime = DockerSandboxRuntime::new(DockerRuntimeConfig::default());
    let error = runtime
        .create_sandbox(&CreateSandboxRequest {
            image: "ubuntu:24.04".to_owned(),
            workspace_root: PathBuf::from("/workspace"),
            shared_base: None,
        })
        .expect_err("missing shared base rejected before docker");

    assert!(matches!(error, ManagerError::RuntimeFailed { .. }));
    assert!(error.to_string().contains("shared base mount is required"));
}

#[test]
fn create_sandbox_rejects_missing_shared_base_source() {
    let runtime = DockerSandboxRuntime::new(DockerRuntimeConfig::default());
    let missing_source =
        std::env::temp_dir().join(format!("eos-missing-shared-base-{}", unique_test_suffix()));
    let error = runtime
        .create_sandbox(&CreateSandboxRequest {
            image: "ubuntu:24.04".to_owned(),
            workspace_root: PathBuf::from("/workspace"),
            shared_base: Some(SharedBaseMount {
                source: missing_source,
                target: PathBuf::from("/eos/layer-stack/base"),
                root_hash: "root-hash".to_owned(),
                readonly: true,
            }),
        })
        .expect_err("missing shared base source rejected before docker");

    assert!(matches!(error, ManagerError::RuntimeFailed { .. }));
    assert!(error.to_string().contains("shared base source"));
}

fn unique_test_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time after epoch")
        .as_nanos();
    format!("{nanos}-{}", std::process::id())
}
