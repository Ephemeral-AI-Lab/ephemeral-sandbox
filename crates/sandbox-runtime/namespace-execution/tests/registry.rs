use sandbox_runtime_namespace_execution::{
    ExecutionRegistry, NamespaceExecutionError, NamespaceExecutionId,
    NamespaceExecutionTerminalStatus,
};

fn id(n: u32) -> NamespaceExecutionId {
    NamespaceExecutionId(format!("namespace_execution_{n}"))
}

#[test]
fn admits_up_to_capacity_then_refuses() {
    let registry = ExecutionRegistry::<()>::new(2);
    registry.try_reserve(&id(1)).expect("first slot");
    registry.try_reserve(&id(2)).expect("second slot");
    let refused = registry.try_reserve(&id(3)).expect_err("over capacity");
    assert!(matches!(
        refused,
        NamespaceExecutionError::Admission { max_active: 2 }
    ));
}

#[test]
fn complete_moves_live_to_completed() {
    let registry = ExecutionRegistry::<()>::new(1);
    registry.try_reserve(&id(1)).expect("slot");
    assert!(registry.is_live(&id(1)));
    assert!(!registry.is_completed(&id(1)));

    registry.complete(&id(1), NamespaceExecutionTerminalStatus::Ok, Some(0));

    assert!(!registry.is_live(&id(1)));
    assert!(registry.is_completed(&id(1)));
}

#[test]
fn attach_records_the_caller_value() {
    let registry = ExecutionRegistry::new(1);
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
}

#[test]
fn abort_releases_a_reservation() {
    let registry = ExecutionRegistry::<()>::new(1);
    registry.try_reserve(&id(1)).expect("slot");
    registry.abort(&id(1));
    assert!(!registry.is_live(&id(1)));
    registry
        .try_reserve(&id(2))
        .expect("slot freed after abort");
}
