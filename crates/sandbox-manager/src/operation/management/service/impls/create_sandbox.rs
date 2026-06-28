use std::path::PathBuf;

use crate::operation::ManagerServices;
use crate::{
    CreateSandboxRequest, ManagerError, ProgressSink, SandboxDaemonEndpoint, SandboxRecord,
    SandboxState,
};

const CREATE_SANDBOX_OP: &str = "create_sandbox";

pub(crate) struct CreateSandboxInput {
    pub(crate) image: String,
    pub(crate) workspace_root: PathBuf,
}

pub(crate) fn create_sandbox(
    services: &ManagerServices,
    input: CreateSandboxInput,
    progress: &ProgressSink,
) -> Result<SandboxRecord, ManagerError> {
    let CreateSandboxInput {
        image,
        workspace_root,
    } = input;
    progress.emit(
        CREATE_SANDBOX_OP,
        "runtime.create",
        "started",
        format!("creating runtime sandbox for {}", workspace_root.display()),
        None,
    );
    let create_request = CreateSandboxRequest {
        image,
        workspace_root: workspace_root.clone(),
    };
    let created = match services.runtime.create_sandbox(&create_request) {
        Ok(created) => {
            progress.emit(
                CREATE_SANDBOX_OP,
                "runtime.create",
                "completed",
                "runtime sandbox created",
                Some(created.id.as_str()),
            );
            created
        }
        Err(error) => {
            progress.emit(
                CREATE_SANDBOX_OP,
                "runtime.create",
                "failed",
                error.to_string(),
                None,
            );
            return Err(error);
        }
    };
    let id = created.id;
    progress.emit(
        CREATE_SANDBOX_OP,
        "store.record",
        "started",
        "recording sandbox",
        Some(id.as_str()),
    );
    let record = match services.store.create(id.clone(), workspace_root.clone()) {
        Ok(record) => record,
        Err(error) => {
            progress.emit(
                CREATE_SANDBOX_OP,
                "store.record",
                "failed",
                error.to_string(),
                Some(id.as_str()),
            );
            let untracked = SandboxRecord::new(id, workspace_root, SandboxState::Creating);
            let _ = services.runtime.destroy_sandbox(&untracked);
            return Err(error);
        }
    };
    progress.emit(
        CREATE_SANDBOX_OP,
        "store.record",
        "completed",
        "sandbox recorded",
        Some(record.id.as_str()),
    );
    let endpoint = match provision_daemon(services, &record, progress) {
        Ok(endpoint) => endpoint,
        Err(error) => {
            progress.emit(
                CREATE_SANDBOX_OP,
                "rollback",
                "started",
                "destroying failed sandbox",
                Some(record.id.as_str()),
            );
            rollback(services, &record);
            progress.emit(
                CREATE_SANDBOX_OP,
                "rollback",
                "completed",
                "failed sandbox destroyed",
                Some(record.id.as_str()),
            );
            return Err(error);
        }
    };
    if let Err(error) = services.store.update_endpoint(&id, Some(endpoint)) {
        progress.emit(
            CREATE_SANDBOX_OP,
            "store.endpoint",
            "failed",
            error.to_string(),
            Some(id.as_str()),
        );
        rollback(services, &record);
        return Err(error);
    }
    progress.emit(
        CREATE_SANDBOX_OP,
        "store.ready",
        "started",
        "marking sandbox ready",
        Some(id.as_str()),
    );
    match services
        .store
        .transition_state(&id, SandboxState::Creating, SandboxState::Ready)
    {
        Ok(ready) => {
            progress.emit(
                CREATE_SANDBOX_OP,
                "store.ready",
                "completed",
                "sandbox is ready",
                Some(id.as_str()),
            );
            Ok(ready)
        }
        Err(error) => {
            progress.emit(
                CREATE_SANDBOX_OP,
                "store.ready",
                "failed",
                error.to_string(),
                Some(id.as_str()),
            );
            rollback(services, &record);
            Err(error)
        }
    }
}

fn provision_daemon(
    services: &ManagerServices,
    record: &SandboxRecord,
    progress: &ProgressSink,
) -> Result<SandboxDaemonEndpoint, ManagerError> {
    progress.emit(
        CREATE_SANDBOX_OP,
        "daemon.install",
        "started",
        "installing daemon assets",
        Some(record.id.as_str()),
    );
    if let Err(error) = services.daemon_installer.install_daemon(record) {
        progress.emit(
            CREATE_SANDBOX_OP,
            "daemon.install",
            "failed",
            error.to_string(),
            Some(record.id.as_str()),
        );
        return Err(error);
    }
    progress.emit(
        CREATE_SANDBOX_OP,
        "daemon.install",
        "completed",
        "daemon assets installed",
        Some(record.id.as_str()),
    );

    progress.emit(
        CREATE_SANDBOX_OP,
        "daemon.start",
        "started",
        "starting daemon",
        Some(record.id.as_str()),
    );
    let endpoint = match services.daemon_installer.start_daemon(record) {
        Ok(endpoint) => {
            progress.emit(
                CREATE_SANDBOX_OP,
                "daemon.start",
                "completed",
                format!("daemon published on {}:{}", endpoint.host, endpoint.port),
                Some(record.id.as_str()),
            );
            endpoint
        }
        Err(error) => {
            progress.emit(
                CREATE_SANDBOX_OP,
                "daemon.start",
                "failed",
                error.to_string(),
                Some(record.id.as_str()),
            );
            return Err(error);
        }
    };

    progress.emit(
        CREATE_SANDBOX_OP,
        "daemon.readiness",
        "started",
        "waiting for daemon readiness",
        Some(record.id.as_str()),
    );
    if let Err(error) = services
        .daemon_installer
        .check_daemon_with_progress(record, &endpoint, progress)
    {
        progress.emit(
            CREATE_SANDBOX_OP,
            "daemon.readiness",
            "failed",
            error.to_string(),
            Some(record.id.as_str()),
        );
        return Err(error);
    }
    progress.emit(
        CREATE_SANDBOX_OP,
        "daemon.readiness",
        "completed",
        "daemon is ready",
        Some(record.id.as_str()),
    );
    Ok(endpoint)
}

fn rollback(services: &ManagerServices, record: &SandboxRecord) {
    let _ = services.daemon_installer.stop_daemon(record);
    let _ = services.runtime.destroy_sandbox(record);
    let _ = services.store.remove(&record.id);
}
