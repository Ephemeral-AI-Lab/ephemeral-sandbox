use std::path::PathBuf;
use std::sync::Arc;

use operation_service::command::{
    CommandCallContext, CommandFinalizationOptions, CommandOperationService, ExecCommandInput,
    OperationTraceContext,
};
use operation_service::workspace_remount::{
    CommandRemountCoordinator, RemountWorkspaceSession, WorkspaceRemountOptions,
    WorkspaceRemountService,
};
use operation_service::workspace_session::WorkspaceSessionService;
use operation_service::OperationServices;
use workspace::{
    CallerId, CaptureChangesRequest, CreateWorkspaceRequest, DestroyWorkspaceRequest,
    LatestSnapshotRequest, RemountWorkspaceRequest, WorkspaceError, WorkspaceHandle, WorkspaceId,
    WorkspaceRuntimeHooks, WorkspaceRuntimeService,
};

fn workspace_session() -> Arc<WorkspaceSessionService> {
    Arc::new(WorkspaceSessionService::new(noop_workspace_runtime()))
}

fn noop_workspace_runtime() -> Arc<WorkspaceRuntimeService> {
    Arc::new(WorkspaceRuntimeService::from_hooks_for_test(
        WorkspaceRuntimeHooks {
            create_workspace: Box::new(|_request: CreateWorkspaceRequest| {
                Err(WorkspaceError::Setup {
                    step: "not configured".to_owned(),
                })
            }),
            capture_changes: Box::new(
                |_handle: &WorkspaceHandle, _request: CaptureChangesRequest| {
                    Err(WorkspaceError::Capture {
                        message: "not configured".to_owned(),
                    })
                },
            ),
            remount_workspace: Box::new(
                |_handle: &WorkspaceHandle, _request: RemountWorkspaceRequest| {
                    Err(WorkspaceError::Setup {
                        step: "not configured".to_owned(),
                    })
                },
            ),
            destroy_workspace: Box::new(
                |_handle: WorkspaceHandle, _request: DestroyWorkspaceRequest| {
                    Err(WorkspaceError::Setup {
                        step: "not configured".to_owned(),
                    })
                },
            ),
            latest_snapshot: Box::new(|_request: LatestSnapshotRequest| {
                Err(WorkspaceError::SnapshotAcquire {
                    source: "not configured".to_owned(),
                })
            }),
        },
    ))
}

#[test]
fn operation_services_wires_top_level_domains() {
    let workspace = workspace_session();
    let command = Arc::new(CommandOperationService::new(
        Arc::clone(&workspace),
        command::CommandConfig::default(),
    ));
    let remount_workspace: Arc<dyn RemountWorkspaceSession> = workspace.clone();
    let remount_command: Arc<dyn CommandRemountCoordinator> = command.clone();
    let remount = Arc::new(WorkspaceRemountService::new(
        remount_workspace,
        remount_command,
        WorkspaceRemountOptions::default(),
    ));

    let services = OperationServices::new(
        Arc::clone(&workspace),
        Arc::clone(&command),
        Arc::clone(&remount),
    );

    assert!(Arc::ptr_eq(&services.workspace, &workspace));
    assert!(Arc::ptr_eq(&services.command, &command));
    assert!(Arc::ptr_eq(&services.remount, &remount));
}

#[test]
fn command_contract_keeps_roots_and_trace_context_separate() {
    let input = ExecCommandInput {
        caller_id: CallerId("caller-1".to_owned()),
        workspace_root: PathBuf::from("/workspace"),
        workspace_session_id: Some(WorkspaceId("workspace-1".to_owned())),
        cmd: "pwd".to_owned(),
        cwd: None,
        timeout_seconds: None,
        yield_time_ms: Some(100),
    };
    let context = CommandCallContext {
        caller_id: input.caller_id.clone(),
        trace: OperationTraceContext,
    };

    assert_eq!(input.workspace_root, PathBuf::from("/workspace"));
    assert_eq!(
        input.workspace_session_id,
        Some(WorkspaceId("workspace-1".to_owned()))
    );
    assert_eq!(context.caller_id, CallerId("caller-1".to_owned()));
}

#[test]
fn command_service_retains_one_shot_finalization_options() {
    let workspace = workspace_session();
    let config = command::CommandConfig::default();
    let options = CommandFinalizationOptions {
        one_shot_publish: layerstack::CommitOptions::new(3),
    };

    let command = CommandOperationService::with_finalization_options(
        Arc::clone(&workspace),
        config.clone(),
        options,
    );

    assert!(Arc::ptr_eq(command.workspace(), &workspace));
    assert_eq!(command.config(), &config);
    assert_eq!(command.finalization_options(), &options);
}

#[test]
fn workspace_remount_options_are_constructor_owned() {
    let workspace = workspace_session();
    let command = Arc::new(CommandOperationService::new(
        Arc::clone(&workspace),
        command::CommandConfig::default(),
    ));
    let options = WorkspaceRemountOptions {
        live_quiesce_timeout_ms: 7_500,
    };
    let remount_workspace: Arc<dyn RemountWorkspaceSession> = workspace.clone();
    let remount_command: Arc<dyn CommandRemountCoordinator> = command.clone();
    let remount = WorkspaceRemountService::new(remount_workspace, remount_command, options);

    assert_eq!(remount.options(), options);
}
