#![cfg(unix)]

use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::{Child, Command};

use sandbox_manager::{
    ManagerError, SandboxDaemonInstaller, SandboxId, SandboxRecord, SandboxState,
};

#[allow(
    dead_code,
    unused_imports,
    reason = "test harness path-includes the private local daemon installer module"
)]
#[path = "../src/local_daemon_installer.rs"]
mod local_daemon_installer;

use local_daemon_installer::LocalSandboxDaemonInstaller;

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

#[cfg(unix)]
#[test]
fn stop_daemon_terminates_pid_file_process() -> Result<(), Box<dyn std::error::Error>> {
    let root = temp_root("daemon-stop")?;
    let workspace_root = root.join("workspace");
    let runtime_root = root.join("runtime");
    std::fs::create_dir_all(&workspace_root)?;
    let installer = LocalSandboxDaemonInstaller::new(
        "/bin/sandbox-daemon",
        root.join("config.yml"),
        runtime_root.clone(),
    );
    let record = SandboxRecord::new(id("container-1"), workspace_root, SandboxState::Ready);
    let (socket_path, pid_path) = daemon_file_paths(&runtime_root, "container-1");
    std::fs::create_dir_all(pid_path.parent().expect("pid path parent"))?;
    std::fs::write(&socket_path, b"socket placeholder")?;

    let child = Command::new("/bin/sleep").arg("30").spawn()?;
    let pid = child.id();
    let _cleanup = ChildCleanup::new(child);
    std::fs::write(&pid_path, pid.to_string())?;

    installer.stop_daemon(&record)?;

    assert!(
        !pid_exists(pid),
        "daemon pid {pid} should be gone after stop_daemon"
    );
    assert!(!pid_path.exists());
    assert!(!socket_path.exists());

    let _ = std::fs::remove_dir_all(root);
    Ok(())
}

#[cfg(unix)]
#[test]
fn stop_daemon_rejects_socket_without_pid_file() -> Result<(), Box<dyn std::error::Error>> {
    let root = temp_root("daemon-stop-missing-pid")?;
    let workspace_root = root.join("workspace");
    let runtime_root = root.join("runtime");
    std::fs::create_dir_all(&workspace_root)?;
    let installer = LocalSandboxDaemonInstaller::new(
        "/bin/sandbox-daemon",
        root.join("config.yml"),
        runtime_root.clone(),
    );
    let record = SandboxRecord::new(id("container-1"), workspace_root, SandboxState::Ready);
    let (socket_path, _pid_path) = daemon_file_paths(&runtime_root, "container-1");
    std::fs::create_dir_all(socket_path.parent().expect("socket path parent"))?;
    std::fs::write(&socket_path, b"socket placeholder")?;

    let error = installer
        .stop_daemon(&record)
        .expect_err("socket without pid is not silently cleaned up");

    assert!(
        matches!(error, ManagerError::DaemonInstallFailed { .. }),
        "unexpected error: {error}"
    );
    assert!(
        socket_path.exists(),
        "socket artifact should remain for failed stop diagnosis"
    );

    let _ = std::fs::remove_dir_all(root);
    Ok(())
}

fn id(value: &str) -> SandboxId {
    SandboxId::new(value).expect("valid sandbox id")
}

#[cfg(unix)]
fn temp_root(label: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    Ok(std::env::temp_dir().join(format!(
        "sandbox-gateway-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    )))
}

#[cfg(unix)]
fn daemon_file_paths(runtime_root: &Path, sandbox_id: &str) -> (PathBuf, PathBuf) {
    let runtime_dir = runtime_root.join(sandbox_id);
    (
        runtime_dir.join("runtime.sock"),
        runtime_dir.join("runtime.pid"),
    )
}

#[cfg(unix)]
struct ChildCleanup {
    child: Child,
}

#[cfg(unix)]
impl ChildCleanup {
    fn new(child: Child) -> Self {
        Self { child }
    }
}

#[cfg(unix)]
impl Drop for ChildCleanup {
    fn drop(&mut self) {
        match self.child.try_wait() {
            Ok(Some(_)) | Err(_) => {}
            Ok(None) => {
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
        }
    }
}

#[cfg(unix)]
fn pid_exists(pid: u32) -> bool {
    let pid = nix::unistd::Pid::from_raw(pid.try_into().expect("test pid fits nix pid"));
    nix::sys::signal::kill(pid, None).is_ok()
}
