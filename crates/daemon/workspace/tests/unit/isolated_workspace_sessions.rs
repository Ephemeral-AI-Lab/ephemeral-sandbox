use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use super::{
    check_host_capacity_against_budget, host_capacity_budget_bytes_from_memavailable_kib,
    parse_memavailable_kib, required_host_capacity_bytes, IsolatedManager, IsolatedSnapshot,
};
use crate::isolated_workspace::caps::ResourceCaps;
use crate::isolated_workspace::error::IsolatedError;
use crate::isolated_workspace::namespace::NamespaceRuntime;
use crate::lifecycle::leases::next_handle_id;

#[test]
fn parses_memavailable_from_proc_meminfo() {
    let meminfo = "MemTotal:       1024 kB\nMemAvailable:    2048 kB\n";
    assert_eq!(parse_memavailable_kib(meminfo), Some(2_048));
}

#[test]
fn host_capacity_budget_matches_rust_floor() {
    assert_eq!(
        host_capacity_budget_bytes_from_memavailable_kib(1_001, 0.5),
        512_512
    );
}

#[test]
fn host_capacity_required_saturates() {
    assert_eq!(required_host_capacity_bytes(usize::MAX, u64::MAX), u64::MAX);
}

#[test]
fn host_capacity_rejects_when_required_exceeds_budget() -> Result<(), Box<dyn std::error::Error>> {
    let error = match check_host_capacity_against_budget(2, 10, 29) {
        Ok(()) => return Err("expected host RAM pressure rejection".into()),
        Err(error) => error,
    };
    let (required_bytes, budget_bytes) = match error {
        IsolatedError::HostRamPressure {
            required_bytes,
            budget_bytes,
        } => (required_bytes, budget_bytes),
        other => return Err(format!("expected host RAM pressure error, got {other}").into()),
    };
    assert_eq!(required_bytes, 30);
    assert_eq!(budget_bytes, 29);
    Ok(())
}

#[test]
fn next_handle_id_puts_counter_in_veth_name_prefix() {
    let first = next_handle_id();
    let second = next_handle_id();

    assert_eq!(first.len(), 22);
    assert_eq!(second.len(), 22);
    assert_ne!(&first[..6], &second[..6]);
}

fn snapshot() -> IsolatedSnapshot {
    IsolatedSnapshot {
        lease_id: "lease-1".to_owned(),
        manifest_version: 7,
        manifest_root_hash: "root-hash".to_owned(),
        layer_paths: vec![PathBuf::from("/lower")],
    }
}

fn enabled_caps() -> ResourceCaps {
    ResourceCaps {
        enabled: true,
        total_cap: 2,
        upperdir_bytes: 16 * 1024 * 1024,
        eos_workspace_root: "/workspace".to_owned(),
        ..ResourceCaps::default()
    }
}

#[test]
fn isolated_exit_discards_upperdir_and_returns_lease_for_release(
) -> Result<(), Box<dyn std::error::Error>> {
    let scratch_root = unique_temp_dir("isolated-no-publish");
    let mut sessions = IsolatedManager::stubbed(enabled_caps(), scratch_root.clone());
    let caller = "caller-1";

    let handle = sessions.enter(caller, snapshot())?;
    let upperdir = handle.dirs.upperdir.clone();
    std::fs::write(upperdir.join("private.txt"), b"private bytes")?;

    let exit = sessions.exit(caller, Some(0.0))?;

    assert!(!upperdir.exists(), "upperdir is discarded on exit");
    assert_eq!(
        exit.lease_id, "lease-1",
        "exit hands the lease back for the caller to release"
    );
    assert_eq!(exit.evicted_upperdir_bytes, 13);
    assert!(sessions.list_open_callers().is_empty());
    assert!(sessions.get_handle(caller).is_none());

    let _ = std::fs::remove_dir_all(scratch_root);
    Ok(())
}

#[test]
fn evict_idle_workspaces_skips_callers_with_active_commands(
) -> Result<(), Box<dyn std::error::Error>> {
    let scratch_root = unique_temp_dir("isolated-ttl");
    let caps = ResourceCaps {
        ttl_s: 0.000_001,
        ..enabled_caps()
    };
    let mut sessions = IsolatedManager::stubbed(caps, scratch_root.clone());
    sessions.enter("busy", snapshot())?;
    sessions.enter(
        "idle",
        IsolatedSnapshot {
            lease_id: "lease-2".to_owned(),
            ..snapshot()
        },
    )?;
    std::thread::sleep(std::time::Duration::from_millis(5));

    let mut protected = HashSet::new();
    protected.insert("busy".to_owned());
    let evicted = sessions.evict_idle_workspaces(&protected);

    assert_eq!(evicted.len(), 1, "only the idle caller is evicted");
    assert_eq!(evicted[0].caller_id, "idle");
    assert_eq!(evicted[0].lease_id, "lease-2");
    assert!(sessions.get_handle("busy").is_some());

    let _ = std::fs::remove_dir_all(scratch_root);
    Ok(())
}

#[test]
fn remount_pending_state_is_persisted_and_cleared() -> Result<(), Box<dyn std::error::Error>> {
    let scratch_root = unique_temp_dir("isolated-remount-state");
    let mut sessions = IsolatedManager::stubbed(enabled_caps(), scratch_root.clone());
    sessions.enter("caller", snapshot())?;

    assert_eq!(
        persisted_remount_state(&scratch_root)?,
        Some("active".to_owned())
    );
    assert_eq!(
        sessions
            .get_handle("caller")
            .expect("caller handle should exist")
            .remount_state
            .as_str(),
        "active"
    );

    sessions.mark_remount_pending("caller")?;

    assert_eq!(
        persisted_remount_state(&scratch_root)?,
        Some("remount_pending".to_owned())
    );
    assert_eq!(
        sessions
            .get_handle("caller")
            .expect("caller handle should exist")
            .remount_state
            .as_str(),
        "remount_pending"
    );

    sessions.clear_remount_pending("caller")?;

    assert_eq!(
        persisted_remount_state(&scratch_root)?,
        Some("active".to_owned())
    );
    assert_eq!(
        sessions
            .get_handle("caller")
            .expect("caller handle should exist")
            .remount_state
            .as_str(),
        "active"
    );

    sessions.exit("caller", Some(0.0))?;
    let _ = std::fs::remove_dir_all(scratch_root);
    Ok(())
}

#[test]
fn enter_persistence_failure_rolls_back_holder_and_state() -> Result<(), Box<dyn std::error::Error>>
{
    let scratch_root = unique_temp_dir("isolated-enter-persist-fail");
    std::fs::create_dir_all(&scratch_root)?;
    std::fs::create_dir(scratch_root.join("manager.json.tmp"))?;
    let killed_holders = Arc::new(Mutex::new(Vec::new()));
    let runtime = NamespaceRuntime::stubbed_with_holder(4242, Arc::clone(&killed_holders));
    let mut sessions = IsolatedManager::with_runtime(enabled_caps(), scratch_root.clone(), runtime);

    let error = sessions
        .enter("caller-persist-fail", snapshot())
        .expect_err("persist failure should fail enter");

    assert_eq!(error.kind(), "setup_failed");
    assert!(error.to_string().contains("manager_write"));
    assert!(sessions.list_open_callers().is_empty());
    assert!(sessions.get_handle("caller-persist-fail").is_none());
    assert_eq!(
        *killed_holders.lock().expect("stub holder kill log lock"),
        vec![4242]
    );
    let owned_root = scratch_root.join("eos-isolated");
    assert!(
        !owned_root.exists() || std::fs::read_dir(&owned_root)?.next().is_none(),
        "rollback should remove the allocated run dir"
    );

    let _ = std::fs::remove_dir_all(scratch_root);
    Ok(())
}

#[test]
fn exit_persistence_failure_is_reported_in_inspection() -> Result<(), Box<dyn std::error::Error>> {
    let scratch_root = unique_temp_dir("isolated-exit-persist-fail");
    let mut sessions = IsolatedManager::stubbed(enabled_caps(), scratch_root.clone());
    sessions.enter("caller", snapshot())?;
    std::fs::create_dir(scratch_root.join("manager.json.tmp"))?;

    let exit = sessions.exit("caller", Some(0.0))?;

    let persistence_error = exit
        .inspection
        .get("persistence_error")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    assert!(
        persistence_error.contains("manager_write"),
        "{persistence_error}"
    );

    let _ = std::fs::remove_dir_all(scratch_root);
    Ok(())
}

#[test]
fn recovery_reaps_only_owned_scratch_directories() -> Result<(), Box<dyn std::error::Error>> {
    let scratch_root = unique_temp_dir("isolated-owned-scratch");
    let owned_root = scratch_root.join("eos-isolated");
    let owned = owned_root.join("0000010123456789abcdef");
    let invalid_owned = owned_root.join("not-a-workspace");
    let foreign = scratch_root.join("foreign");
    std::fs::create_dir_all(&owned)?;
    std::fs::create_dir_all(&invalid_owned)?;
    std::fs::create_dir_all(&foreign)?;
    let mut sessions = IsolatedManager::stubbed(enabled_caps(), scratch_root.clone());

    let cleanup_error = sessions.reap_orphan_resources();

    assert_eq!(cleanup_error, None);
    assert!(!owned.exists(), "owned workspace scratch should be reaped");
    assert!(
        invalid_owned.exists(),
        "invalid owned-root directory should survive"
    );
    assert!(foreign.exists(), "foreign scratch sibling should survive");

    let _ = std::fs::remove_dir_all(scratch_root);
    Ok(())
}

#[test]
fn recovery_kills_persisted_holder_pid() -> Result<(), Box<dyn std::error::Error>> {
    let scratch_root = unique_temp_dir("isolated-persisted-holder");
    std::fs::create_dir_all(&scratch_root)?;
    std::fs::write(
        scratch_root.join("manager.json"),
        serde_json::json!({
            "schema_version": 1,
            "handles": [{
                "lease_id": "lease-orphan",
                "holder_pid": 5150
            }]
        })
        .to_string(),
    )?;
    let killed_holders = Arc::new(Mutex::new(Vec::new()));
    let runtime = NamespaceRuntime::stubbed_with_holder(0, Arc::clone(&killed_holders));
    let mut sessions = IsolatedManager::with_runtime(enabled_caps(), scratch_root.clone(), runtime);

    let report = sessions.reap_persisted_orphans()?;

    assert_eq!(report.orphan_lease_ids, vec!["lease-orphan"]);
    assert_eq!(
        *killed_holders.lock().expect("stub holder kill log lock"),
        vec![5150]
    );

    let _ = std::fs::remove_dir_all(scratch_root);
    Ok(())
}

fn unique_temp_dir(prefix: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "eos-{prefix}-{}-{}",
        std::process::id(),
        next_handle_id()
    ))
}

fn persisted_remount_state(
    scratch_root: &std::path::Path,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let raw = std::fs::read_to_string(scratch_root.join("manager.json"))?;
    let payload: Value = serde_json::from_str(&raw)?;
    Ok(payload
        .get("handles")
        .and_then(Value::as_array)
        .and_then(|handles| handles.first())
        .and_then(|handle| handle.get("remount_state"))
        .and_then(Value::as_str)
        .map(str::to_owned))
}
