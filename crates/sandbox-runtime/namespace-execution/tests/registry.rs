use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};

use sandbox_runtime_namespace_execution::{
    ExecutionRegistry, NamespaceExecutionError, NamespaceExecutionId,
    NamespaceExecutionTerminalStatus,
};

fn id(n: u32) -> NamespaceExecutionId {
    NamespaceExecutionId(format!("namespace_execution_{n}"))
}

#[test]
fn admits_up_to_capacity_then_refuses() {
    let registry = ExecutionRegistry::<()>::new(2, 512);
    registry.try_reserve(&id(1)).expect("first slot");
    registry.try_reserve(&id(2)).expect("second slot");
    let refused = registry.try_reserve(&id(3)).expect_err("over capacity");
    assert!(matches!(
        refused,
        NamespaceExecutionError::Admission { max_active: 2 }
    ));
}

#[test]
fn live_duplicate_is_rejected_without_consuming_capacity() {
    let registry = ExecutionRegistry::<()>::new(2, 512);
    let duplicate_id = id(1);
    registry.try_reserve(&duplicate_id).expect("first slot");

    let error = registry
        .try_reserve(&duplicate_id)
        .expect_err("live duplicate must be rejected");

    assert!(matches!(
        error,
        NamespaceExecutionError::Duplicate { execution_id }
            if execution_id == duplicate_id.0
    ));
    registry
        .try_reserve(&id(2))
        .expect("duplicate attempt did not consume capacity");
    assert!(matches!(
        registry.try_reserve(&id(3)),
        Err(NamespaceExecutionError::Admission { max_active: 2 })
    ));
}

#[test]
fn terminal_id_is_rejected_until_retention_evicts_it() {
    let registry = ExecutionRegistry::<()>::new(2, 1);
    let retained_id = id(1);
    registry.try_reserve(&retained_id).expect("first slot");
    registry.complete(&retained_id, NamespaceExecutionTerminalStatus::Ok, Some(0));

    let error = registry
        .try_reserve(&retained_id)
        .expect_err("retained terminal id must not be reused");
    assert!(matches!(
        error,
        NamespaceExecutionError::Duplicate { execution_id }
            if execution_id == retained_id.0
    ));

    let evicting_id = id(2);
    registry.try_reserve(&evicting_id).expect("second slot");
    registry.complete(&evicting_id, NamespaceExecutionTerminalStatus::Ok, Some(0));
    assert!(!registry.is_completed(&retained_id));
    registry
        .try_reserve(&retained_id)
        .expect("evicted terminal id may be reused");
}

#[test]
fn zero_terminal_retention_releases_synchronous_execution_immediately() {
    let registry = ExecutionRegistry::<()>::new(1, 0);
    let completed_id = id(1);
    registry.try_reserve(&completed_id).expect("slot");

    registry.complete(&completed_id, NamespaceExecutionTerminalStatus::Ok, Some(0));

    assert_eq!(registry.active_count(), 0);
    assert!(!registry.is_completed(&completed_id));
    registry
        .try_reserve(&completed_id)
        .expect("synchronous execution leaves no terminal lookup record");
}

#[test]
fn completion_abort_race_releases_capacity_exactly_once() {
    for round in 0..32 {
        let registry = Arc::new(ExecutionRegistry::<()>::new(1, 1));
        let raced_id = id(round);
        registry.try_reserve(&raced_id).expect("raced slot");
        let start = Arc::new(Barrier::new(3));

        let complete_registry = Arc::clone(&registry);
        let complete_start = Arc::clone(&start);
        let complete_id = raced_id.clone();
        let complete = std::thread::spawn(move || {
            complete_start.wait();
            complete_registry.complete(&complete_id, NamespaceExecutionTerminalStatus::Ok, Some(0));
        });

        let abort_registry = Arc::clone(&registry);
        let abort_start = Arc::clone(&start);
        let abort_id = raced_id.clone();
        let abort = std::thread::spawn(move || {
            abort_start.wait();
            abort_registry.abort(&abort_id);
        });

        start.wait();
        complete.join().expect("completion thread");
        abort.join().expect("abort thread");

        assert!(!registry.is_live(&raced_id));
        registry
            .try_reserve(&id(round + 1_000))
            .expect("race released the only active slot exactly once");
    }
}

#[test]
fn abort_after_completion_preserves_the_terminal_result() {
    let registry = ExecutionRegistry::<()>::new(1, 1);
    let completed_id = id(1);
    registry.try_reserve(&completed_id).expect("first slot");
    registry.complete(&completed_id, NamespaceExecutionTerminalStatus::Ok, Some(0));

    registry.abort(&completed_id);

    assert!(registry.is_completed(&completed_id));
    assert!(matches!(
        registry.try_reserve(&completed_id),
        Err(NamespaceExecutionError::Duplicate { .. })
    ));
    registry
        .try_reserve(&id(2))
        .expect("terminal entry does not consume active capacity");
}

#[test]
fn duplicate_rejection_recovers_capacity_after_abort_and_completion() {
    let registry = ExecutionRegistry::<()>::new(1, 1);
    let first_id = id(1);
    registry.try_reserve(&first_id).expect("first slot");
    assert!(matches!(
        registry.try_reserve(&first_id),
        Err(NamespaceExecutionError::Duplicate { .. })
    ));
    registry.abort(&first_id);

    let second_id = id(2);
    registry
        .try_reserve(&second_id)
        .expect("abort restored capacity");
    assert!(matches!(
        registry.try_reserve(&second_id),
        Err(NamespaceExecutionError::Duplicate { .. })
    ));
    registry.complete(&second_id, NamespaceExecutionTerminalStatus::Ok, Some(0));

    registry
        .try_reserve(&id(3))
        .expect("completion restored capacity");
}

#[test]
fn complete_moves_live_to_completed() {
    let registry = ExecutionRegistry::<()>::new(1, 512);
    registry.try_reserve(&id(1)).expect("slot");
    assert!(registry.is_live(&id(1)));
    assert!(!registry.is_completed(&id(1)));

    registry.complete(&id(1), NamespaceExecutionTerminalStatus::Ok, Some(0));

    assert!(!registry.is_live(&id(1)));
    assert!(registry.is_completed(&id(1)));
}

#[test]
fn attach_records_the_caller_value() {
    let registry = ExecutionRegistry::new(1, 512);
    registry.try_reserve(&id(1)).expect("slot");
    registry.attach(&id(1), "command-handle".to_owned());
    assert_eq!(
        registry.with_value(&id(1), Clone::clone),
        Some("command-handle".to_owned())
    );
    assert_eq!(
        registry.live_values(|value| Some(value.clone())),
        vec!["command-handle".to_owned()]
    );

    registry.complete(&id(1), NamespaceExecutionTerminalStatus::Ok, Some(0));

    assert!(registry.live_values(|value| Some(value.clone())).is_empty());
    assert_eq!(
        registry.retained_values(|value| Some(value.clone())),
        vec!["command-handle".to_owned()]
    );
}

#[test]
fn abort_releases_a_reservation() {
    let registry = ExecutionRegistry::<()>::new(1, 512);
    registry.try_reserve(&id(1)).expect("slot");
    registry.abort(&id(1));
    assert!(!registry.is_live(&id(1)));
    registry
        .try_reserve(&id(2))
        .expect("slot freed after abort");
}

struct DropProbe(Arc<AtomicUsize>);

impl Drop for DropProbe {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn terminal_retention_evicts_oldest_terminal_entry_and_drops_its_value() {
    let drops = Arc::new(AtomicUsize::new(0));
    let registry = ExecutionRegistry::new(8, 512);
    registry.set_terminal_retention(2);
    for n in 1..=4 {
        registry.try_reserve(&id(n)).expect("slot");
        registry.attach(&id(n), DropProbe(Arc::clone(&drops)));
        registry.complete(&id(n), NamespaceExecutionTerminalStatus::Ok, Some(0));
    }

    assert_eq!(
        drops.load(Ordering::SeqCst),
        2,
        "the two oldest terminal values were dropped"
    );
    assert!(
        !registry.is_completed(&id(1)) && !registry.is_completed(&id(2)),
        "evicted entries are gone entirely"
    );
    assert!(registry.with_value(&id(1), |_| ()).is_none());
    assert!(registry.is_completed(&id(3)) && registry.is_completed(&id(4)));
    assert!(registry.with_value(&id(4), |_| ()).is_some());
}

#[test]
fn terminal_retention_never_evicts_live_entries() {
    let drops = Arc::new(AtomicUsize::new(0));
    let registry = ExecutionRegistry::new(8, 512);
    registry.set_terminal_retention(1);
    registry.try_reserve(&id(1)).expect("slot");
    registry.attach(&id(1), DropProbe(Arc::clone(&drops)));
    registry.try_reserve(&id(2)).expect("slot");
    registry.attach(&id(2), DropProbe(Arc::clone(&drops)));
    registry.try_reserve(&id(3)).expect("slot");
    registry.attach(&id(3), DropProbe(Arc::clone(&drops)));

    registry.complete(&id(2), NamespaceExecutionTerminalStatus::Ok, Some(0));
    registry.complete(&id(3), NamespaceExecutionTerminalStatus::Ok, Some(0));

    assert!(registry.is_live(&id(1)), "live entries are never evicted");
    assert_eq!(drops.load(Ordering::SeqCst), 1, "only id 2 was evicted");
    assert!(!registry.is_completed(&id(2)));
    assert!(registry.is_completed(&id(3)));
}

#[test]
fn selective_terminal_release_preserves_other_terminal_and_live_entries() {
    let registry = ExecutionRegistry::new(8, 512);
    for (number, value) in [(1, "workspace-a"), (2, "workspace-b"), (3, "workspace-a")] {
        registry.try_reserve(&id(number)).expect("slot");
        registry.attach(&id(number), value.to_owned());
    }
    registry.complete(&id(1), NamespaceExecutionTerminalStatus::Ok, Some(0));
    registry.complete(&id(2), NamespaceExecutionTerminalStatus::Ok, Some(0));

    assert_eq!(
        registry.remove_terminal_values(|value| value == "workspace-a"),
        1
    );
    assert!(registry.with_value(&id(1), |_| ()).is_none());
    assert!(registry.is_completed(&id(2)));
    assert!(registry.is_live(&id(3)));
    assert_eq!(registry.active_count(), 1);
}
