use std::collections::HashMap;
use std::fs::File;
use std::os::fd::IntoRawFd;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use sandbox_observability_telemetry::Observer;

use crate::lifecycle::destroy::{ExitOutcome, TeardownLedger, TeardownStep, TeardownStepExecutor};
use crate::model::{
    LayerStackSnapshotRef, LeaseId, NetworkProfile, WorkspaceHandle, WorkspaceSessionId,
};
use crate::namespace::holder::HolderRegistration;
use crate::overlay::dirs::OverlayDirs;
use crate::session::{HolderNsFds, MountedWorkspace};
use crate::session::{ResourceCaps, WorkspaceManager};

#[derive(Default)]
struct CountingExecutor {
    calls: HashMap<TeardownStep, usize>,
    fail_once: Option<TeardownStep>,
}

impl TeardownStepExecutor for CountingExecutor {
    fn execute(&mut self, step: TeardownStep) -> Result<(), String> {
        *self.calls.entry(step).or_default() += 1;
        if self.fail_once == Some(step) {
            self.fail_once = None;
            return Err("injected failure".to_owned());
        }
        Ok(())
    }
}

#[test]
fn every_teardown_step_retries_only_its_failure_and_never_double_releases() {
    for failed_step in TeardownStep::ORDER {
        let mut ledger = TeardownLedger::default();
        let mut executor = CountingExecutor {
            fail_once: Some(failed_step),
            ..CountingExecutor::default()
        };

        let first = ledger
            .run(&mut executor)
            .expect_err("injected teardown failure remains visible");
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].step, failed_step);
        assert!(!ledger.is_complete());

        ledger.run(&mut executor).expect("bounded retry completes");
        assert!(ledger.is_complete());
        for step in TeardownStep::ORDER {
            let expected = if step == failed_step { 2 } else { 1 };
            assert_eq!(
                executor.calls.get(&step).copied(),
                Some(expected),
                "failed={failed_step:?}, observed={step:?}"
            );
        }
    }
}

#[test]
fn teardown_attempts_independent_steps_after_one_failure() {
    let mut ledger = TeardownLedger::default();
    let mut executor = CountingExecutor {
        fail_once: Some(TeardownStep::Holder),
        ..CountingExecutor::default()
    };

    let failures = ledger
        .run(&mut executor)
        .expect_err("holder failure is reported");

    assert_eq!(failures[0].step, TeardownStep::Holder);
    assert_eq!(executor.calls.get(&TeardownStep::Persistence), Some(&1));
    assert_eq!(executor.calls.get(&TeardownStep::Scratch), Some(&1));
}

#[test]
fn lease_accounting_failure_retries_without_repeating_raw_teardown() {
    #[derive(Default)]
    struct AccountingFailureExecutor {
        calls: HashMap<TeardownStep, usize>,
        accounting_failed: bool,
    }

    impl TeardownStepExecutor for AccountingFailureExecutor {
        fn execute(&mut self, step: TeardownStep) -> Result<(), String> {
            *self.calls.entry(step).or_default() += 1;
            if step == TeardownStep::LeaseAccounting && !self.accounting_failed {
                self.accounting_failed = true;
                return Err("injected active-lease accounting failure".to_owned());
            }
            if step == TeardownStep::Persistence
                && self.calls.get(&TeardownStep::LeaseAccounting) == Some(&1)
            {
                return Err("deferred until lease accounting succeeds".to_owned());
            }
            Ok(())
        }
    }

    let mut ledger = TeardownLedger::default();
    let mut executor = AccountingFailureExecutor::default();

    let first = ledger
        .run(&mut executor)
        .expect_err("post-close accounting failure must retain the transaction");
    assert_eq!(
        first.iter().map(|failure| failure.step).collect::<Vec<_>>(),
        vec![TeardownStep::LeaseAccounting, TeardownStep::Persistence]
    );

    ledger
        .run(&mut executor)
        .expect("retry joins the retained transaction");
    assert!(ledger.is_complete());
    for raw_step in [
        TeardownStep::Holder,
        TeardownStep::Commands,
        TeardownStep::NamespaceFds,
        TeardownStep::Network,
        TeardownStep::Mounts,
        TeardownStep::Scratch,
        TeardownStep::Leases,
    ] {
        assert_eq!(
            executor.calls.get(&raw_step),
            Some(&1),
            "raw teardown step {raw_step:?} repeated"
        );
    }
    assert_eq!(executor.calls.get(&TeardownStep::LeaseAccounting), Some(&2));
    assert_eq!(executor.calls.get(&TeardownStep::Persistence), Some(&2));
}

#[test]
fn completed_teardown_cache_is_bounded_and_cleared_by_workspace_id() {
    let base = temp_root("completed-cache");
    let mut manager = WorkspaceManager::new(
        base.join("workspace").to_string_lossy(),
        ResourceCaps::default(),
        base.join("scratch"),
        Observer::disabled(),
    );
    for index in 0..10_000 {
        let workspace_session_id = WorkspaceSessionId(format!("workspace-{index}"));
        manager.record_completed_teardown(
            workspace_session_id.clone(),
            HolderRegistration::unmanaged(workspace_session_id.clone(), index + 1),
            outcome(workspace_session_id),
        );
    }
    assert_eq!(manager.completed_teardowns.len(), 128);

    let retained = WorkspaceSessionId("workspace-9999".to_owned());
    manager.forget_completed_teardowns(&retained);
    assert_eq!(manager.completed_teardowns.len(), 127);

    let committed = WorkspaceSessionId("workspace-commit".to_owned());
    let handle = WorkspaceHandle::without_launch_for_test(
        committed.clone(),
        PathBuf::from("/workspace"),
        NetworkProfile::Shared,
        LayerStackSnapshotRef {
            lease_id: LeaseId("lease-commit".to_owned()),
            manifest_version: 1,
            root_hash: "root".to_owned(),
            manifest: sandbox_runtime_layerstack::Manifest::new(1, Vec::new(), 1)
                .expect("valid manifest"),
            layer_paths: Vec::new(),
        },
    );
    manager.record_completed_teardown(
        committed.clone(),
        handle.holder_registration().clone(),
        outcome(committed),
    );
    assert!(manager.owns_handle_generation(&handle));
    assert_eq!(manager.completed_teardowns.len(), 128);

    manager.forget_completed_teardown(&handle);
    assert!(!manager.owns_handle_generation(&handle));
    assert_eq!(manager.completed_teardowns.len(), 127);
}

#[test]
fn shutdown_all_attempts_every_exact_workspace_and_retries_only_incomplete_steps(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let base = temp_root("shutdown-all");
    let scratch_root = base.join("scratch");
    let layer_stack_root = base.join("layer-stack");
    std::fs::create_dir_all(&layer_stack_root)?;
    let mut manager = WorkspaceManager::new(
        base.join("workspace").to_string_lossy(),
        ResourceCaps::default(),
        scratch_root.clone(),
        Observer::disabled(),
    );
    manager.bind_layer_stack_root(layer_stack_root);

    let failed_id = WorkspaceSessionId("workspace-a-fails-once".to_owned());
    let peer_id = WorkspaceSessionId("workspace-z-peer".to_owned());
    let (peer, peer_fds) = mounted_workspace(&scratch_root, peer_id.clone(), false)?;
    let (failed, failed_fds) = mounted_workspace(&scratch_root, failed_id.clone(), true)?;
    manager.handles.insert(failed_id.clone(), failed);
    manager.handles.insert(peer_id.clone(), peer);

    let first = manager.shutdown_all();

    assert_eq!(
        first.attempted_workspace_ids,
        vec![failed_id.clone(), peer_id.clone()]
    );
    assert_eq!(first.closed_workspace_ids, vec![peer_id.clone()]);
    assert_eq!(first.retryable_failures.len(), 1);
    assert_eq!(first.retryable_failures[0].workspace_session_id, failed_id);
    assert!(first.retryable_failures[0]
        .failures
        .iter()
        .any(|failure| failure.starts_with("NamespaceFds:")));
    assert_eq!(first.remaining_workspace_ids, vec![failed_id.clone()]);
    for fd in failed_fds.iter().chain(&peer_fds) {
        assert!(
            nix::fcntl::fcntl(*fd, nix::fcntl::FcntlArg::F_GETFD).is_err(),
            "shutdown retained fd {fd}"
        );
    }
    assert!(!scratch_root.join(&peer_id.0).exists());
    assert!(!scratch_root.join(&failed_id.0).exists());

    let retry = manager.shutdown_all();

    assert_eq!(retry.attempted_workspace_ids, vec![failed_id.clone()]);
    assert_eq!(retry.closed_workspace_ids, vec![failed_id]);
    assert!(retry.retryable_failures.is_empty());
    assert!(retry.remaining_workspace_ids.is_empty());
    assert_eq!(manager.ownership_snapshot(), Default::default());
    std::fs::remove_dir_all(base)?;
    Ok(())
}

fn mounted_workspace(
    scratch_root: &std::path::Path,
    workspace_session_id: WorkspaceSessionId,
    inject_invalid_namespace_fd: bool,
) -> Result<(MountedWorkspace, Vec<i32>), Box<dyn std::error::Error + Send + Sync>> {
    let run_dir = scratch_root.join(&workspace_session_id.0);
    let upperdir = run_dir.join("upper");
    let workdir = run_dir.join("work");
    std::fs::create_dir_all(&upperdir)?;
    std::fs::create_dir_all(&workdir)?;
    let readiness_fd = File::open("/dev/null")?.into_raw_fd();
    let control_fd = File::open("/dev/null")?.into_raw_fd();
    let mnt_fd = File::open("/dev/null")?.into_raw_fd();
    let pid_fd = File::open("/dev/null")?.into_raw_fd();
    let invalid_fd = File::open("/dev/null")?.into_raw_fd();
    nix::unistd::close(invalid_fd)?;
    let user_fd = if inject_invalid_namespace_fd {
        invalid_fd
    } else {
        File::open("/dev/null")?.into_raw_fd()
    };
    let mut fds_to_verify = vec![readiness_fd, control_fd, mnt_fd, pid_fd];
    if !inject_invalid_namespace_fd {
        fds_to_verify.push(user_fd);
    }

    Ok((
        MountedWorkspace {
            workspace_id: workspace_session_id.clone(),
            network: NetworkProfile::Shared,
            snapshot: LayerStackSnapshotRef {
                lease_id: LeaseId(format!("lease-{}", workspace_session_id.0)),
                manifest_version: 1,
                root_hash: "root".to_owned(),
                manifest: sandbox_runtime_layerstack::Manifest::new(1, Vec::new(), 1)?,
                layer_paths: Vec::new(),
            },
            workspace_root: "/workspace".to_owned(),
            dirs: OverlayDirs {
                run_dir,
                upperdir,
                workdir,
            },
            ns_fds: HolderNsFds {
                user: Some(user_fd),
                mnt: Some(mnt_fd),
                pid: Some(pid_fd),
                net: None,
            },
            holder_pid: 0,
            holder_registration: HolderRegistration::unmanaged(workspace_session_id, 0),
            readiness_fd,
            control_fd,
            veth: None,
            created_at: 0.0,
            last_activity: 0.0,
            parked_lease_id: None,
        },
        fds_to_verify,
    ))
}

fn temp_root(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    std::env::current_dir()
        .expect("workspace tests run from a current directory")
        .join("target")
        .join(format!(
            "workspace-internal-{label}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ))
}

fn outcome(workspace_id: WorkspaceSessionId) -> ExitOutcome {
    ExitOutcome {
        workspace_id,
        lease_id: "lease".to_owned(),
        parked_lease_id: None,
        active_leases_after: 0,
        evicted_upperdir_bytes: 0,
        lifetime_s: 0.0,
        total_ms: 0.0,
        phases_ms: HashMap::new(),
        inspection: serde_json::Value::Null,
    }
}
