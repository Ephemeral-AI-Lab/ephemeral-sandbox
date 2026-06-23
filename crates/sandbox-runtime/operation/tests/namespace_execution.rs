use sandbox_runtime::{
    BeginNamespaceExecution, CompleteNamespaceExecution, NamespaceExecutionLifecycle,
    NamespaceExecutionStore, NamespaceExecutionTerminalStatus, WorkspaceSessionId,
};

#[test]
fn namespace_store_lifecycle_duplicate_completion_and_ack() {
    let store = NamespaceExecutionStore::new();
    let id = store.allocate_namespace_execution_id();
    assert_eq!(id.0, "namespace_execution_1");

    store
        .begin_namespace_execution(
            id.clone(),
            BeginNamespaceExecution {
                workspace_session_id: WorkspaceSessionId("workspace-session".to_owned()),
                operation_name: "exec_command".to_owned(),
                request_id: Some("req-parent".to_owned()),
            },
        )
        .expect("begin succeeds");

    let active = store
        .snapshot_active_namespace_executions()
        .expect("snapshot succeeds");
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].namespace_execution_id, id);
    assert_eq!(
        active[0].lifecycle_state,
        NamespaceExecutionLifecycle::Starting
    );

    store
        .mark_namespace_execution_running(&id)
        .expect("mark running succeeds");
    assert_eq!(
        store
            .snapshot_active_namespace_executions()
            .expect("snapshot succeeds")[0]
            .lifecycle_state,
        NamespaceExecutionLifecycle::Running
    );

    let completed = store
        .complete_namespace_execution(
            &id,
            CompleteNamespaceExecution {
                terminal_status: NamespaceExecutionTerminalStatus::Ok,
                exit_code: Some(0),
                error_kind: None,
                error_message: None,
            },
        )
        .expect("completion succeeds");
    assert_eq!(
        completed.lifecycle_state,
        NamespaceExecutionLifecycle::Terminal
    );
    assert_eq!(
        completed.terminal_status,
        Some(NamespaceExecutionTerminalStatus::Ok)
    );
    assert_eq!(completed.exit_code, Some(0));
    assert!(store
        .snapshot_active_namespace_executions()
        .expect("snapshot succeeds")
        .is_empty());

    let duplicate = store
        .complete_namespace_execution(
            &id,
            CompleteNamespaceExecution {
                terminal_status: NamespaceExecutionTerminalStatus::Error,
                exit_code: Some(9),
                error_kind: Some("second".to_owned()),
                error_message: Some("ignored".to_owned()),
            },
        )
        .expect("duplicate completion is idempotent");
    assert_eq!(duplicate, completed);
    assert_eq!(
        store
            .drain_completed_namespace_executions(10)
            .expect("drain succeeds"),
        vec![completed.clone()]
    );

    store
        .ack_completed_namespace_executions(std::slice::from_ref(&id))
        .expect("ack succeeds");
    assert!(store
        .drain_completed_namespace_executions(10)
        .expect("drain succeeds")
        .is_empty());
    let after_ack_duplicate = store
        .complete_namespace_execution(
            &id,
            CompleteNamespaceExecution {
                terminal_status: NamespaceExecutionTerminalStatus::Cancelled,
                exit_code: Some(130),
                error_kind: None,
                error_message: None,
            },
        )
        .expect("recent projected completion stays idempotent");
    assert_eq!(after_ack_duplicate, completed);
}

#[test]
fn namespace_id_allocation_survives_forced_mutation_failure() {
    let store = NamespaceExecutionStore::new();
    let id = store.allocate_namespace_execution_id();
    store.set_force_mutation_errors_for_test(true);

    let error = store
        .begin_namespace_execution(
            id.clone(),
            BeginNamespaceExecution {
                workspace_session_id: WorkspaceSessionId("workspace-session".to_owned()),
                operation_name: "exec_command".to_owned(),
                request_id: None,
            },
        )
        .expect_err("forced mutation failure rejects begin");

    assert!(error.contains("begin_namespace_execution"));
    assert_eq!(id.0, "namespace_execution_1");
    assert_eq!(
        store.allocate_namespace_execution_id().0,
        "namespace_execution_2"
    );
    assert!(store
        .drain_partial_errors()
        .expect("partial errors drain")
        .iter()
        .any(|error| error.contains("begin_namespace_execution")));
}

#[test]
fn namespace_store_retention_drop_records_partial_error() {
    let store = NamespaceExecutionStore::with_limits(1, 4, 4);
    let first = complete_success(&store, "workspace-one");
    let second = complete_success(&store, "workspace-two");

    let pending = store
        .drain_completed_namespace_executions(10)
        .expect("drain succeeds");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].namespace_execution_id, second);

    let errors = store.drain_partial_errors().expect("partial errors drain");
    assert!(
        errors
            .iter()
            .any(|error| error.contains(&first.0) && error.contains("dropped")),
        "{errors:?}"
    );
}

fn complete_success(
    store: &NamespaceExecutionStore,
    workspace_session_id: &str,
) -> sandbox_runtime::NamespaceExecutionId {
    let id = store.allocate_namespace_execution_id();
    store
        .begin_namespace_execution(
            id.clone(),
            BeginNamespaceExecution {
                workspace_session_id: WorkspaceSessionId(workspace_session_id.to_owned()),
                operation_name: "exec_command".to_owned(),
                request_id: None,
            },
        )
        .expect("begin succeeds");
    store
        .complete_namespace_execution(
            &id,
            CompleteNamespaceExecution {
                terminal_status: NamespaceExecutionTerminalStatus::Ok,
                exit_code: Some(0),
                error_kind: None,
                error_message: None,
            },
        )
        .expect("complete succeeds");
    id
}
