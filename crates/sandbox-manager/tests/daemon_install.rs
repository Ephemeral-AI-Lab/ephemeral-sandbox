use std::path::PathBuf;

use sandbox_manager::{
    ManagerError, ProgressSink, SandboxDaemonEndpoint, SandboxId, SandboxRecord, SandboxState,
};

#[allow(
    dead_code,
    unused_imports,
    reason = "test harness path-includes the private daemon_install module and exercises only launch_spec"
)]
#[path = "../src/daemon_install.rs"]
mod daemon_install;

use daemon_install::LocalSandboxDaemonInstaller;

#[test]
fn launch_spec_passes_dynamic_sandbox_id() {
    let installer = LocalSandboxDaemonInstaller::new(
        "/bin/sandbox-daemon",
        "/etc/eos/prd.yml",
        "/tmp/eos-daemons",
    );
    let record = SandboxRecord::new(
        SandboxId::new("container-1").expect("valid sandbox id"),
        PathBuf::from("/testbed"),
        SandboxState::Ready,
    );

    let spec = installer
        .launch_spec(&record)
        .expect("launch spec builds from record");

    assert_eq!(spec.executable, PathBuf::from("/bin/sandbox-daemon"));
    assert_eq!(
        spec.socket_path,
        PathBuf::from("/tmp/eos-daemons/container-1/runtime.sock")
    );
    assert_eq!(
        spec.pid_path,
        PathBuf::from("/tmp/eos-daemons/container-1/runtime.pid")
    );
    assert!(spec
        .args
        .windows(2)
        .any(|window| window[0] == "--sandbox-id" && window[1] == "container-1"));
    assert!(spec
        .args
        .windows(2)
        .any(|window| window[0] == "--workspace-root" && window[1] == "/testbed"));
    assert!(!spec.args.iter().any(|arg| arg == "secret-token"));
}
