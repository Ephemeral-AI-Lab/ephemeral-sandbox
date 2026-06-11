use std::collections::HashSet;
use std::path::PathBuf;

use super::capacity::{
    check_host_capacity_against_budget, host_capacity_budget_bytes_from_memavailable_kib,
    parse_memavailable_kib, required_host_capacity_bytes,
};
use super::resources::next_handle_id;
use super::{IsolatedSessions, IsolatedSnapshot};
use crate::caps::ResourceCaps;
use crate::error::IsolatedError;

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
    let mut sessions = IsolatedSessions::stubbed(enabled_caps(), scratch_root.clone());
    let caller = "caller-1";

    let handle = sessions.enter(caller, snapshot())?;
    let upperdir = handle.upperdir.clone();
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
fn ttl_sweep_skips_callers_with_active_command_sessions() -> Result<(), Box<dyn std::error::Error>>
{
    let scratch_root = unique_temp_dir("isolated-ttl");
    let caps = ResourceCaps {
        ttl_s: 0.000_001,
        ..enabled_caps()
    };
    let mut sessions = IsolatedSessions::stubbed(caps, scratch_root.clone());
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
    let evicted = sessions.ttl_sweep(&protected);

    assert_eq!(evicted.len(), 1, "only the idle caller is evicted");
    assert_eq!(evicted[0].caller_id, "idle");
    assert_eq!(evicted[0].lease_id, "lease-2");
    assert!(sessions.get_handle("busy").is_some());

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
