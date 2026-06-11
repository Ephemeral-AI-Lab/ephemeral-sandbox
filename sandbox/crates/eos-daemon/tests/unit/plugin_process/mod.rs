//! Unit tests for plugin service process specs (spec/env, spawn, PPC connect).
//!
//! Referenced from `src/services/plugin/process.rs` via `#[path]` so they can
//! reach the `pub(super)` spec/process types and `ENV_*` constants.

use super::*;
use eos_plugin::host::route::{
    ENV_PLUGIN_DEPENDENCY_ROOT, ENV_PLUGIN_ID, ENV_PLUGIN_LAYER_STACK_ROOT,
    ENV_PLUGIN_PACKAGE_ROOT, ENV_PLUGIN_PPC_PROTOCOL_VERSION, ENV_PLUGIN_PPC_SOCKET,
    ENV_PLUGIN_SERVICE_ID, ENV_PLUGIN_WORKSPACE_ROOT,
};
use eos_plugin::{PluginServiceKey, PluginServiceKeyParts, RefreshStrategy, ServiceMode};

type TestResult = std::result::Result<(), Box<dyn std::error::Error + Send + Sync>>;

fn key(profile: &str) -> std::result::Result<PluginServiceKey, PluginError> {
    PluginServiceKey::new(PluginServiceKeyParts {
        layer_stack_root: "/eos/plugin/layer-stack".to_owned(),
        workspace_root: "/eos/plugin/workspace".to_owned(),
        plugin_id: "demo".to_owned(),
        plugin_digest: "digest-a".to_owned(),
        service_id: "indexer".to_owned(),
        service_profile_digest: profile.to_owned(),
        service_mode: ServiceMode::WorkspaceSnapshotRefresh,
        refresh_strategy: RefreshStrategy::RemountWorkspaceAndNotify,
    })
}

#[test]
fn process_spec_uses_stable_eos_plugin_socket_and_env() -> TestResult {
    let spec = new_spec_for_test(
        key("profile-a")?,
        vec!["demo-indexer".to_owned(), "--stdio".to_owned()],
        1,
    )?;
    let env = spec.environment();

    assert!(env[ENV_PLUGIN_PPC_SOCKET].starts_with("/eos/plugin/ppc/"));
    assert!(std::path::Path::new(&env[ENV_PLUGIN_PPC_SOCKET])
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("sock")));
    assert_eq!(env[ENV_PLUGIN_LAYER_STACK_ROOT], "/eos/plugin/layer-stack");
    assert_eq!(env[ENV_PLUGIN_WORKSPACE_ROOT], "/eos/plugin/workspace");
    assert_eq!(
        env[ENV_PLUGIN_PACKAGE_ROOT],
        "/eos/runtime/plugins/catalog/demo/digest-a"
    );
    assert_eq!(
        env[ENV_PLUGIN_DEPENDENCY_ROOT],
        "/eos/runtime/packages/demo/digest-a"
    );
    assert_eq!(env[ENV_PLUGIN_ID], "demo");
    assert_eq!(env[ENV_PLUGIN_SERVICE_ID], "indexer");
    assert_eq!(env[ENV_PLUGIN_PPC_PROTOCOL_VERSION], "1");
    Ok(())
}

#[test]
fn process_spec_key_changes_socket_path() -> TestResult {
    let first = new_spec_for_test(key("profile-a")?, vec!["svc".to_owned()], 1)?;
    let second = new_spec_for_test(key("profile-b")?, vec!["svc".to_owned()], 1)?;

    assert_ne!(
        first.environment()[ENV_PLUGIN_PPC_SOCKET],
        second.environment()[ENV_PLUGIN_PPC_SOCKET]
    );
    Ok(())
}

#[test]
fn process_spec_rejects_empty_command() -> TestResult {
    let service_key = key("profile-a")?;
    assert!(matches!(
        new_spec_for_test(service_key, Vec::new(), 1),
        Err(PluginError::Manifest(message)) if message.contains("launch command")
    ));
    Ok(())
}

#[test]
fn spawned_process_reports_running_then_tears_down() -> TestResult {
    let spec = new_spec_for_test(
        key("profile-a")?,
        vec![
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            "test \"$EOS_PLUGIN_SERVICE_ID\" = indexer && sleep 30".to_owned(),
        ],
        1,
    )?;
    let mut process = spawn(&spec)?;

    let status = process.status_json();
    assert_eq!(status["service_id"], "indexer");
    assert_eq!(status["running"], true);
    let pid = status["pid"]
        .as_u64()
        .ok_or_else(|| std::io::Error::new(ErrorKind::InvalidData, "missing process pid"))?;
    assert!(pid > 0);

    process.teardown();
    let status = process.status_json();
    assert_eq!(status["running"], false);
    Ok(())
}

#[test]
fn spawn_connected_accepts_ppc_socket() -> TestResult {
    let root = test_socket_root("spawn-connected");
    let spec = new_spec_with_socket_root(
        key("profile-a")?,
        vec!["/bin/sh".to_owned(), "-c".to_owned(), "sleep 30".to_owned()],
        1,
        &root,
    )?;
    let socket_root = root.clone();
    let connector = std::thread::spawn(move || {
        let socket = wait_for_socket(&socket_root)?;
        std::os::unix::net::UnixStream::connect(socket).map(|_| ())
    });

    let (mut process, _client) =
        match spawn_connected_with_overlay(&spec, None, Duration::from_secs(5)) {
            Ok(pair) => pair,
            Err(err) => {
                let _ = connector.join();
                return Err(err.into());
            }
        };
    match connector.join() {
        Ok(result) => result?,
        Err(_) => {
            return Err(std::io::Error::other("connector thread panicked").into());
        }
    }
    process.teardown();
    let _ = std::fs::remove_dir_all(root);
    Ok(())
}

fn test_socket_root(name: &str) -> PathBuf {
    let root = PathBuf::from("target").join(format!("ppc-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    root
}

fn wait_for_socket(root: &Path) -> std::io::Result<PathBuf> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(entries) = std::fs::read_dir(root) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|ext| ext.to_str()) == Some("sock") {
                    return Ok(path);
                }
            }
        }
        if Instant::now() >= deadline {
            return Err(std::io::Error::new(
                ErrorKind::TimedOut,
                format!("timed out waiting for socket under {}", root.display()),
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
