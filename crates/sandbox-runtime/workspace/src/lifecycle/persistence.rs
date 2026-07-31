use std::io::Write;
use std::path::Path;

use serde_json::{json, Value};

use crate::session::manager::PERSISTED_HANDLES_SCHEMA_VERSION;
use crate::session::{MountedWorkspace, WorkspaceManager, WorkspaceManagerError};

impl WorkspaceManager {
    fn persisted_handles_path(&self) -> std::path::PathBuf {
        self.scratch_root.join("manager.json")
    }

    pub(crate) fn persist_handles(&self) -> Result<(), WorkspaceManagerError> {
        std::fs::create_dir_all(&self.scratch_root)
            .map_err(|err| manager_setup_error("manager_root", err))?;
        let handles: Vec<Value> = self
            .handles
            .values()
            .chain(self.teardowns.values().filter_map(|transaction| {
                transaction
                    .has_persisted_handle()
                    .then_some(transaction.owned_handle())
            }))
            .map(persisted_handle_json)
            .collect();
        let payload = json!({
            "schema_version": PERSISTED_HANDLES_SCHEMA_VERSION,
            "handles": handles,
        });
        let path = self.persisted_handles_path();
        let tmp = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(&payload)
            .map_err(|err| manager_setup_error("manager_serialize", err))?;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&tmp)
            .map_err(|err| manager_setup_error("manager_write", err))?;
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|err| manager_setup_error("manager_write", err))?;
        drop(file);
        std::fs::rename(&tmp, &path).map_err(|err| manager_setup_error("manager_rename", err))?;
        sync_directory(&self.scratch_root)
            .map_err(|err| manager_setup_error("manager_fsync", err))?;
        Ok(())
    }
}

fn persisted_handle_json(handle: &MountedWorkspace) -> Value {
    let lease_id = (!handle.external_overlay_authority).then_some(&handle.snapshot.lease_id.0);
    json!({
        "workspace_handle_id": handle.workspace_id.0,
        "lease_id": lease_id,
        "parked_lease_id": handle.parked_lease_id,
        "candidate_admission": &handle.candidate_admission,
        "manifest_version": handle.snapshot.manifest_version,
        "manifest_root_hash": handle.snapshot.root_hash,
        "network_profile": handle.network.as_str(),
        "workspace_root": handle.workspace_root,
        "scratch_dir": handle.dirs.run_dir.to_string_lossy(),
        "upperdir": handle.dirs.upperdir.to_string_lossy(),
        "workdir": handle.dirs.workdir.to_string_lossy(),
        "layer_paths": handle.snapshot.layer_paths,
        "holder_pid": handle.holder_pid,
        "veth_host_name": handle.veth.as_ref().map(|veth| veth.host_name.as_str()),
        "veth_ns_name": handle.veth.as_ref().map(|veth| veth.ns_name.as_str()),
        "ns_ip": handle.veth.as_ref().map(|veth| veth.ns_ip.to_string()),
        "created_at": handle.created_at,
        "last_activity": handle.last_activity,
    })
}

/// One reaped boot leftover: every persisted handle is a dead session
/// (PDEATHSIG makes holders provably dead), so reap destroys its run dir and
/// drops the record — no lease recreation, no liveness proof.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReapedSession {
    pub workspace_handle_id: String,
    pub run_dir: std::path::PathBuf,
    pub run_dir_removed: bool,
    /// `None` is the one-release migration value for records written before
    /// lease ids were persisted. New records report an explicit result.
    pub lease_released: Option<bool>,
    pub lease_release_error: Option<String>,
    pub run_dir_cleanup_error: Option<String>,
    /// True only after the dead handle record was durably removed from
    /// `manager.json`. A failed peer record keeps the original file intact so
    /// every cleanup can be retried idempotently.
    pub persisted_handle_released: bool,
}

impl WorkspaceManager {
    pub(crate) fn reap_persisted_handles(
        &mut self,
    ) -> Result<Vec<ReapedSession>, WorkspaceManagerError> {
        let path = self.persisted_handles_path();
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Ok(Vec::new());
        };
        let Ok(payload) = serde_json::from_str::<Value>(&text) else {
            self.persist_handles()?;
            return Ok(Vec::new());
        };
        let empty = Vec::new();
        let records = payload
            .get("handles")
            .and_then(Value::as_array)
            .unwrap_or(&empty);
        let mut reaped = Vec::with_capacity(records.len());
        let mut all_terminal = true;
        for record in records {
            let workspace_handle_id = record
                .get("workspace_handle_id")
                .and_then(Value::as_str)
                .unwrap_or("<unknown>")
                .to_owned();
            let run_dir = record
                .get("scratch_dir")
                .and_then(Value::as_str)
                .map(std::path::PathBuf::from)
                .unwrap_or_default();
            let (run_dir_removed, run_dir_cleanup_error, scratch_retryable) =
                reap_persisted_run_dir(&self.scratch_root, &run_dir);
            let (lease_released, lease_release_error) =
                self.release_persisted_record_leases(record);
            if scratch_retryable || lease_released == Some(false) {
                all_terminal = false;
            }
            reaped.push(ReapedSession {
                workspace_handle_id,
                run_dir,
                run_dir_removed,
                lease_released,
                lease_release_error,
                run_dir_cleanup_error,
                persisted_handle_released: false,
            });
        }
        if all_terminal {
            self.persist_handles()?;
            for session in &mut reaped {
                session.persisted_handle_released = true;
            }
        }
        Ok(reaped)
    }

    fn release_persisted_record_leases(&self, record: &Value) -> (Option<bool>, Option<String>) {
        let lease_id = record.get("lease_id").and_then(Value::as_str);
        let parked_lease_id = record.get("parked_lease_id").and_then(Value::as_str);
        let candidate_lease = record
            .get("candidate_admission")
            .and_then(|admission| admission.get("lease"));
        if lease_id.is_none() && parked_lease_id.is_none() && candidate_lease.is_none() {
            return (None, None);
        }
        let Some(layer_stack_root) = self.layer_stack_root.as_deref() else {
            return (
                Some(false),
                Some("layer stack root is not bound to workspace manager".to_owned()),
            );
        };

        let mut failures = Vec::new();
        if let Some(candidate_lease) = candidate_lease {
            match serde_json::from_value::<
                sandbox_runtime_layerstack::service::CandidateGenerationLease,
            >(candidate_lease.clone())
            {
                Ok(candidate_lease) => {
                    if let Err(error) =
                        sandbox_runtime_layerstack::service::release_candidate_generation_lease(
                            layer_stack_root,
                            &candidate_lease,
                        )
                    {
                        failures.push(format!(
                            "release candidate generation lease {}: {error}",
                            candidate_lease.lease_id
                        ));
                    }
                }
                Err(error) => failures.push(format!(
                    "decode persisted candidate generation lease: {error}"
                )),
            }
        }
        if let Some(lease_id) = lease_id {
            if let Err(error) =
                sandbox_runtime_layerstack::service::release_lease(layer_stack_root, lease_id)
            {
                failures.push(format!("release lease {lease_id}: {error}"));
            }
        }
        if let Some(parked_lease_id) = parked_lease_id {
            if let Err(error) = sandbox_runtime_layerstack::service::release_lease(
                layer_stack_root,
                parked_lease_id,
            ) {
                failures.push(format!("release parked lease {parked_lease_id}: {error}"));
            }
        }

        if failures.is_empty() {
            (Some(true), None)
        } else {
            (Some(false), Some(failures.join("; ")))
        }
    }
}

fn reap_persisted_run_dir(scratch_root: &Path, run_dir: &Path) -> (bool, Option<String>, bool) {
    if run_dir.as_os_str().is_empty() {
        return (
            false,
            Some("persisted handle has no scratch_dir".to_owned()),
            false,
        );
    }
    if !run_dir.starts_with(scratch_root) {
        return (
            false,
            Some(format!(
                "refusing to remove scratch outside manager root: {}",
                run_dir.display()
            )),
            false,
        );
    }
    match std::fs::remove_dir_all(run_dir) {
        Ok(()) => (true, None, false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (true, None, false),
        Err(error) => (false, Some(error.to_string()), true),
    }
}

fn manager_setup_error(step: &str, err: impl std::fmt::Display) -> WorkspaceManagerError {
    WorkspaceManagerError::SetupFailed {
        step: format!("{step}: {err}"),
    }
}

fn sync_directory(path: &Path) -> std::io::Result<()> {
    match std::fs::File::open(path).and_then(|file| file.sync_all()) {
        Ok(()) => Ok(()),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::InvalidInput | std::io::ErrorKind::Unsupported
            ) =>
        {
            Ok(())
        }
        Err(error) => Err(error),
    }
}
