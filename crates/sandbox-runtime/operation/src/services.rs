use std::path::Path;
use std::sync::Arc;

#[cfg(target_os = "linux")]
use rustix::io::Errno;
#[cfg(target_os = "linux")]
use rustix::mount::{unmount, UnmountFlags};
use sandbox_observability::Observer;
use sandbox_runtime_layerstack::service::StackObservation;
use sandbox_runtime_namespace_process::runner::protocol::CommandSecurityPolicy;

use crate::command::CommandOperationService;
use crate::file::FileService;
use crate::layerstack::LayerStackService;
use crate::observability::RuntimeObservabilitySnapshot;
use crate::workspace_crate::{session::WorkspaceManager, WorkspaceRuntimeService};
use crate::workspace_session::WorkspaceSessionService;

#[derive(Clone)]
pub struct SandboxRuntimeOperations {
    pub command: Arc<CommandOperationService>,
    pub workspace_session: Arc<WorkspaceSessionService>,
    pub layerstack: Arc<LayerStackService>,
    pub file: Arc<FileService>,
}

impl SandboxRuntimeOperations {
    #[must_use]
    pub fn new(
        command: Arc<CommandOperationService>,
        workspace_session: Arc<WorkspaceSessionService>,
        layerstack: Arc<LayerStackService>,
        file: Arc<FileService>,
    ) -> Self {
        Self {
            command,
            workspace_session,
            layerstack,
            file,
        }
    }

    /// Assemble the runtime services over one shared process `Observer` (a clone
    /// of the daemon's). Every emitting service holds that same handle, so daemon
    /// and runtime spans share one id sequence and one parent chain.
    #[must_use]
    pub fn from_config(config: SandboxRuntimeConfig, observer: Observer) -> Self {
        let layer_stack_root = config.workspace.layer_stack_root.clone();
        let file = Arc::new(
            FileService::open(file_auditability_dir(&layer_stack_root))
                .expect("file auditability store initialization failed"),
        );
        let workspace_runtime = Arc::new(WorkspaceRuntimeService::new(
            WorkspaceManager::new(
                config
                    .workspace
                    .workspace_root
                    .to_string_lossy()
                    .into_owned(),
                config.workspace.caps.into(),
                config.workspace.scratch_root,
                observer.clone(),
            ),
            layer_stack_root.clone(),
        ));
        cli_log(format!(
            "ensuring workspace base for {}",
            config.workspace.workspace_root.display()
        ));
        let base_result = sandbox_runtime_layerstack::ensure_workspace_base(
            &layer_stack_root,
            &config.workspace.workspace_root,
        );
        match base_result {
            Ok((_binding, built)) => cli_log(if built {
                "workspace base built"
            } else {
                "workspace base already exists"
            }),
            Err(error) => {
                cli_log(error.to_string());
                panic!("layerstack workspace base initialization failed: {error}");
            }
        }
        detach_workspace_bind_after_base(&config.workspace.workspace_root);
        let layerstack = Arc::new(
            LayerStackService::new(layer_stack_root, observer.clone(), Arc::clone(&file))
                .expect("layerstack service initialization failed"),
        );
        let workspace_session = Arc::new(WorkspaceSessionService::with_cgroup_root(
            workspace_runtime,
            Arc::clone(&layerstack),
            config.cgroup_root.clone(),
            observer.clone(),
        ));
        let command = Arc::new(CommandOperationService::new(
            Arc::clone(&workspace_session),
            crate::command::CommandConfig {
                scratch_root: config.namespace_execution.scratch_root,
                command_security: config.command_security,
            },
            observer.clone(),
        ));
        boot_reap_then_sweep(&workspace_session, &layerstack, &observer);
        Self::new(command, workspace_session, layerstack, file)
    }

    #[must_use]
    pub fn observability_snapshot(&self) -> RuntimeObservabilitySnapshot {
        let (workspaces, partial_errors) = self.workspace_session.snapshot_workspaces();
        let active_namespace_executions = self.command.active_namespace_executions();
        RuntimeObservabilitySnapshot {
            workspaces,
            active_namespace_executions,
            partial_errors,
        }
    }

    /// Live per-layer lease breakdown of the active manifest (in-memory state).
    ///
    /// The daemon merges this with the observability leaf reader's disk byte
    /// sizes (keyed by layer id) to render the `layerstack` inventory.
    pub fn observe_layerstack(
        &self,
    ) -> Result<StackObservation, crate::layerstack::LayerStackServiceError> {
        self.layerstack.observe()
    }

    /// Storage root of the layer stack, for the observability leaf byte reader.
    #[must_use]
    pub fn layer_stack_root(&self) -> &std::path::Path {
        self.layerstack.layer_stack_root()
    }
}

/// Boot cleanup, once, before serving: assert the kernel floor, reap every
/// persisted session (each is provably dead — PDEATHSIG), then run the
/// fail-closed storage sweep. Reap records are emitted before any sweep
/// deletion record; both ride existing record names, so the feature's
/// record budget stays at three.
fn boot_reap_then_sweep(
    workspace_session: &Arc<WorkspaceSessionService>,
    layerstack: &Arc<LayerStackService>,
    observer: &Observer,
) {
    assert_kernel_floor();
    probe_and_set_remount_gate(layerstack, observer);
    let reaped = workspace_session
        .workspace()
        .reap_persisted_sessions()
        .unwrap_or_default();
    for session in &reaped {
        observer.event(
            sandbox_observability::record::names::WORKSPACE_SESSION_DESTROY,
            serde_json::json!({
                "boot_reap": true,
                "workspace_handle_id": session.workspace_handle_id,
                "run_dir_removed": session.run_dir_removed,
            }),
        );
    }
    cli_log(format!(
        "boot reap removed {} dead session(s)",
        reaped.len()
    ));
    let sweep =
        sandbox_runtime_layerstack::LayerStack::open(layerstack.layer_stack_root().to_path_buf())
            .and_then(|mut stack| stack.sweep_storage());
    match sweep {
        Ok(report) => {
            observer.event(
                sandbox_observability::record::names::LAYERSTACK_SQUASH,
                serde_json::json!({
                    "boot_sweep": true,
                    "removed_layer_ids": report.removed_layer_ids,
                    "removed_staging_entries": report.removed_staging_entries,
                    "skipped_reason": report.skipped_reason,
                }),
            );
            cli_log(format!(
                "boot storage sweep: removed {} layer id(s), {} staging entries{}",
                report.removed_layer_ids.len(),
                report.removed_staging_entries,
                report
                    .skipped_reason
                    .map(|reason| format!(", skipped: {reason}"))
                    .unwrap_or_default()
            ));
        }
        Err(error) => cli_log(format!("boot storage sweep failed: {error}")),
    }
}

/// Probe the same-upperdir / userxattr kernel gate once and flip live
/// remount on only if it holds; otherwise squash stays commit-only and every
/// session reports `leased(unsupported:kernel_gate_not_proven)`.
fn probe_and_set_remount_gate(layerstack: &Arc<LayerStackService>, observer: &Observer) {
    // The probe mounts a scratch overlay, so its scratch must be on a real
    // (non-overlay) filesystem — the layer-stack volume is ext4, unlike the
    // container's overlay rootfs at /eos.
    let scratch = layerstack.layer_stack_root().join("staging");
    let proven = crate::workspace_crate::probe_and_set_live_remount_gate(&scratch);
    observer.event(
        sandbox_observability::record::names::NAMESPACE_EXEC_REMOUNT_OVERLAY,
        serde_json::json!({ "boot_gate": true, "live_remount_enabled": proven }),
    );
    cli_log(format!(
        "live remount kernel gate: {}",
        if proven {
            "PROVEN (enabled)"
        } else {
            "NOT PROVEN (squash commit-only)"
        }
    ));
}

/// The supported daemon environment is Linux ≥ 5.8 (`syncfs` writeback error
/// reporting); refuse to serve on anything older.
#[cfg(target_os = "linux")]
fn assert_kernel_floor() {
    let release = std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
    let mut parts = release.trim().split(['.', '-']);
    let major: u32 = parts.next().and_then(|part| part.parse().ok()).unwrap_or(0);
    let minor: u32 = parts.next().and_then(|part| part.parse().ok()).unwrap_or(0);
    assert!(
        (major, minor) >= (5, 8),
        "unsupported kernel {release}: the sandbox daemon requires Linux >= 5.8"
    );
}

#[cfg(not(target_os = "linux"))]
fn assert_kernel_floor() {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkspaceBindDetach {
    #[cfg(target_os = "linux")]
    Unmounted,
    NotMounted,
}

fn detach_workspace_bind_after_base(workspace_root: &Path) {
    cli_log(format!(
        "unmounting workspace bind {}",
        workspace_root.display()
    ));
    match detach_workspace_bind(workspace_root) {
        #[cfg(target_os = "linux")]
        Ok(WorkspaceBindDetach::Unmounted) => cli_log(format!(
            "workspace bind unmounted {}",
            workspace_root.display()
        )),
        Ok(WorkspaceBindDetach::NotMounted) => cli_log(format!(
            "workspace bind not mounted {}",
            workspace_root.display()
        )),
        Err(error) => {
            cli_log(format!(
                "workspace bind unmount failed {}: {error}",
                workspace_root.display()
            ));
            panic!(
                "workspace bind unmount failed for {}: {error}",
                workspace_root.display()
            );
        }
    }
    if !workspace_root.is_dir() {
        let message = format!(
            "workspace mountpoint missing after unmount {}",
            workspace_root.display()
        );
        cli_log(&message);
        panic!("{message}");
    }
}

#[cfg(target_os = "linux")]
fn detach_workspace_bind(workspace_root: &Path) -> Result<WorkspaceBindDetach, std::io::Error> {
    match unmount(workspace_root, UnmountFlags::empty()) {
        Ok(()) => Ok(WorkspaceBindDetach::Unmounted),
        Err(Errno::INVAL) => Ok(WorkspaceBindDetach::NotMounted),
        Err(error) => Err(std::io::Error::from(error)),
    }
}

#[cfg(not(target_os = "linux"))]
fn detach_workspace_bind(_workspace_root: &Path) -> Result<WorkspaceBindDetach, std::io::Error> {
    Ok(WorkspaceBindDetach::NotMounted)
}

fn cli_log(message: impl AsRef<str>) {
    let escaped = serde_json::to_string(message.as_ref()).unwrap_or_else(|_| "\"\"".to_owned());
    eprintln!("cli_log({escaped})");
}

/// The file-auditability log lives beside the layer stack, under
/// `<layer_stack_root>/../storage/file_auditability` (C3 spec §7.1) — the only
/// root this crate can reach from `config.workspace.layer_stack_root`.
fn file_auditability_dir(layer_stack_root: &Path) -> std::path::PathBuf {
    layer_stack_root
        .parent()
        .unwrap_or(layer_stack_root)
        .join("storage")
        .join("file_auditability")
}

#[derive(Debug, Clone, PartialEq)]
pub struct SandboxRuntimeConfig {
    pub workspace: WorkspaceRuntimeConfig,
    pub namespace_execution: NamespaceExecutionRuntimeConfig,
    pub cgroup_root: Option<std::path::PathBuf>,
    pub command_security: CommandSecurityPolicy,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkspaceRuntimeConfig {
    pub workspace_root: std::path::PathBuf,
    pub layer_stack_root: std::path::PathBuf,
    pub scratch_root: std::path::PathBuf,
    pub caps: WorkspaceResourceCaps,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NamespaceExecutionRuntimeConfig {
    pub scratch_root: std::path::PathBuf,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkspaceResourceCaps {
    pub setup_timeout_s: f64,
    pub exit_grace_s: f64,
    pub rfc1918_egress: Rfc1918Egress,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rfc1918Egress {
    Allow,
    Deny,
}

impl From<WorkspaceResourceCaps> for crate::workspace_crate::session::ResourceCaps {
    fn from(caps: WorkspaceResourceCaps) -> Self {
        Self {
            setup_timeout_s: caps.setup_timeout_s,
            exit_grace_s: caps.exit_grace_s,
            rfc1918_egress: match caps.rfc1918_egress {
                Rfc1918Egress::Allow => crate::workspace_crate::session::Rfc1918Egress::Allow,
                Rfc1918Egress::Deny => crate::workspace_crate::session::Rfc1918Egress::Deny,
            },
        }
    }
}
