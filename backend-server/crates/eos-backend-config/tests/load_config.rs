//! Config loader contract: the committed baseline loads, overrides merge, range
//! validation fires, and a `providers:` / `workflow:` section is rejected — the
//! enforceable form of "`ServerConfig` embeds no provider/workflow config" (AC11).
#![allow(clippy::unwrap_used)] // unwrap is permitted in tests

use std::path::{Path, PathBuf};

use eos_backend_config::{load_from_paths, ConfigError};

/// `backend-server/config/backend.yml`, resolved from this crate's manifest dir
/// (`backend-server/crates/eos-backend-config`).
fn baseline() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap()
        .join("config/backend.yml")
}

/// Write a uniquely-named temp YAML override and return its path.
fn temp_yaml(name: &str, content: &str) -> PathBuf {
    let path =
        std::env::temp_dir().join(format!("eos_backend_config_{}_{name}.yml", std::process::id()));
    std::fs::write(&path, content).unwrap();
    path
}

#[test]
fn loads_committed_baseline() {
    let config = load_from_paths(&[baseline()]).unwrap();
    assert_eq!(config.bind.port(), 8080);
    assert_eq!(config.sandbox.max_owned_sandboxes, 16);
    assert!(config.sandbox.destroy_on_finish);
    assert_eq!(config.sandbox.startup_timeout_ms, 30000);
    assert!(!config.obs.include_sandbox_audit);
    assert!(config.obs.event_queue_capacity >= 1);
    assert!(!config.agent_core.database_url.is_empty());
}

#[test]
fn override_merges_over_baseline() {
    let over = temp_yaml("merge", "sandbox:\n  max_owned_sandboxes: 4\n");
    let config = load_from_paths(&[baseline(), over.clone()]).unwrap();
    let _ = std::fs::remove_file(&over);

    assert_eq!(config.sandbox.max_owned_sandboxes, 4, "override wins");
    assert!(config.sandbox.destroy_on_finish, "baseline field survives");
    assert_eq!(config.bind.port(), 8080, "untouched baseline field survives");
}

#[test]
fn rejects_providers_section() {
    let over = temp_yaml("providers", "providers:\n  default: anthropic\n");
    let result = load_from_paths(&[baseline(), over.clone()]);
    let _ = std::fs::remove_file(&over);

    let err = result.unwrap_err();
    assert!(matches!(err, ConfigError::ParseYaml(_)));
    assert!(err.to_string().contains("parse"), "{err}");
}

#[test]
fn rejects_workflow_section() {
    let over = temp_yaml("workflow", "workflow:\n  max_depth: 3\n");
    let result = load_from_paths(&[baseline(), over.clone()]);
    let _ = std::fs::remove_file(&over);
    assert!(matches!(result, Err(ConfigError::ParseYaml(_))));
}

#[test]
fn rejects_unknown_field() {
    let over = temp_yaml("unknown", "bogus: 1\n");
    let result = load_from_paths(&[baseline(), over.clone()]);
    let _ = std::fs::remove_file(&over);
    assert!(matches!(result, Err(ConfigError::ParseYaml(_))));
}

#[test]
fn rejects_out_of_range_max_owned_sandboxes() {
    let over = temp_yaml("range", "sandbox:\n  max_owned_sandboxes: 0\n");
    let result = load_from_paths(&[baseline(), over.clone()]);
    let _ = std::fs::remove_file(&over);
    assert!(matches!(
        result,
        Err(ConfigError::OutOfRange {
            field: "sandbox.max_owned_sandboxes",
            ..
        })
    ));
}
