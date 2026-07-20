use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use crate::{
    ManagerError, SandboxDaemonEndpoint, SandboxHttpEndpoint, SandboxId, SandboxRecord,
    SandboxResourceProfile, SandboxState, SharedBaseMount,
};

#[derive(Debug, Default)]
pub struct SandboxStore {
    records: Mutex<HashMap<SandboxId, SandboxRecord>>,
    registry_path: Option<PathBuf>,
}

impl SandboxStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Open a registry backed by a JSON snapshot at `path`, loading any
    /// records a previous process persisted. Every mutation rewrites the
    /// snapshot atomically (temp file + rename), so a crash leaves either the
    /// old snapshot or the new one, never a torn file.
    ///
    /// # Errors
    /// Returns an error when the snapshot exists but cannot be read or parsed.
    pub fn load(path: PathBuf) -> Result<Self, ManagerError> {
        let records = match fs::read(&path) {
            Ok(bytes) => {
                let snapshot: Vec<SandboxRecord> = serde_json::from_slice(&bytes)
                    .map_err(|error| registry_error(&path, "parse", &error))?;
                snapshot
                    .into_iter()
                    .map(|record| (record.id.clone(), record))
                    .collect()
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
            Err(error) => return Err(registry_error(&path, "read", &error)),
        };
        Ok(Self {
            records: Mutex::new(records),
            registry_path: Some(path),
        })
    }

    /// Merge runtime-recovered records into the registry; the runtime wins on
    /// every conflict. Loaded records with no surviving container are marked
    /// `Failed`, and their ids are returned so callers can surface the loss.
    pub fn reconcile(&self, recovered: Vec<SandboxRecord>) -> Result<Vec<SandboxId>, ManagerError> {
        let mut records = self.records()?;
        let recovered_ids: HashSet<SandboxId> =
            recovered.iter().map(|record| record.id.clone()).collect();
        let mut orphaned = Vec::new();
        for record in records.values_mut() {
            if !recovered_ids.contains(&record.id) && record.state != SandboxState::Failed {
                record.state = SandboxState::Failed;
                orphaned.push(record.id.clone());
            }
        }
        for record in recovered {
            records.insert(record.id.clone(), record);
        }
        orphaned.sort();
        self.persist(&records)?;
        Ok(orphaned)
    }

    pub fn create(
        &self,
        id: SandboxId,
        workspace_root: PathBuf,
    ) -> Result<SandboxRecord, ManagerError> {
        self.create_with_shared_base(id, workspace_root, None)
    }

    pub fn create_with_shared_base(
        &self,
        id: SandboxId,
        workspace_root: PathBuf,
        shared_base: Option<SharedBaseMount>,
    ) -> Result<SandboxRecord, ManagerError> {
        self.create_with_shared_base_and_profile(id, workspace_root, shared_base, None)
    }

    pub fn create_with_shared_base_and_profile(
        &self,
        id: SandboxId,
        workspace_root: PathBuf,
        shared_base: Option<SharedBaseMount>,
        resource_profile: Option<SandboxResourceProfile>,
    ) -> Result<SandboxRecord, ManagerError> {
        let mut records = self.records()?;
        if records.contains_key(&id) {
            return Err(ManagerError::DuplicateSandbox { id });
        }
        let mut record = SandboxRecord::new(id.clone(), workspace_root, SandboxState::Creating);
        record.shared_base = shared_base;
        record.resource_profile = resource_profile;
        records.insert(id, record.clone());
        self.persist(&records)?;
        Ok(record)
    }

    pub fn insert(&self, record: SandboxRecord) -> Result<SandboxRecord, ManagerError> {
        let mut records = self.records()?;
        if records.contains_key(&record.id) {
            return Err(ManagerError::DuplicateSandbox {
                id: record.id.clone(),
            });
        }
        records.insert(record.id.clone(), record.clone());
        self.persist(&records)?;
        Ok(record)
    }

    pub fn update(&self, record: SandboxRecord) -> Result<SandboxRecord, ManagerError> {
        let mut records = self.records()?;
        if !records.contains_key(&record.id) {
            return Err(ManagerError::MissingSandbox {
                id: record.id.clone(),
            });
        }
        records.insert(record.id.clone(), record.clone());
        self.persist(&records)?;
        Ok(record)
    }

    pub fn list(&self) -> Result<Vec<SandboxRecord>, ManagerError> {
        let mut records = self.records()?.values().cloned().collect::<Vec<_>>();
        records.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(records)
    }

    pub fn ready_ids(&self) -> Result<Vec<SandboxId>, ManagerError> {
        let mut ids = self
            .records()?
            .iter()
            .filter(|(_, record)| record.state == SandboxState::Ready)
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        ids.sort();
        Ok(ids)
    }

    pub fn is_ready(&self, id: &SandboxId) -> Result<bool, ManagerError> {
        self.records()?
            .get(id)
            .map(|record| record.state == SandboxState::Ready)
            .ok_or_else(|| ManagerError::MissingSandbox { id: id.clone() })
    }

    pub fn inspect(&self, id: &SandboxId) -> Result<SandboxRecord, ManagerError> {
        self.records()?
            .get(id)
            .cloned()
            .ok_or_else(|| ManagerError::MissingSandbox { id: id.clone() })
    }

    pub fn remove(&self, id: &SandboxId) -> Result<SandboxRecord, ManagerError> {
        let mut records = self.records()?;
        let record = records
            .remove(id)
            .ok_or_else(|| ManagerError::MissingSandbox { id: id.clone() })?;
        self.persist(&records)?;
        Ok(record)
    }

    pub fn transition_state(
        &self,
        id: &SandboxId,
        from: SandboxState,
        to: SandboxState,
    ) -> Result<SandboxRecord, ManagerError> {
        let mut records = self.records()?;
        let record = records
            .get_mut(id)
            .ok_or_else(|| ManagerError::MissingSandbox { id: id.clone() })?;
        if record.state != from {
            return Err(ManagerError::InvalidStateTransition {
                id: id.clone(),
                from: record.state,
                to,
            });
        }
        record.state = to;
        let record = record.clone();
        self.persist(&records)?;
        Ok(record)
    }

    pub fn set_state(
        &self,
        id: &SandboxId,
        state: SandboxState,
    ) -> Result<SandboxRecord, ManagerError> {
        let mut records = self.records()?;
        let record = records
            .get_mut(id)
            .ok_or_else(|| ManagerError::MissingSandbox { id: id.clone() })?;
        record.state = state;
        let record = record.clone();
        self.persist(&records)?;
        Ok(record)
    }

    pub fn update_endpoints(
        &self,
        id: &SandboxId,
        daemon: Option<SandboxDaemonEndpoint>,
        daemon_http: Option<SandboxHttpEndpoint>,
    ) -> Result<SandboxRecord, ManagerError> {
        let mut records = self.records()?;
        let record = records
            .get_mut(id)
            .ok_or_else(|| ManagerError::MissingSandbox { id: id.clone() })?;
        record.daemon = daemon;
        record.daemon_http = daemon_http;
        let record = record.clone();
        self.persist(&records)?;
        Ok(record)
    }

    pub fn advance_activity_revision(&self, id: &SandboxId) -> Result<SandboxRecord, ManagerError> {
        let mut records = self.records()?;
        let record = records
            .get_mut(id)
            .ok_or_else(|| ManagerError::MissingSandbox { id: id.clone() })?;
        record.activity_revision =
            record
                .activity_revision
                .checked_add(1)
                .ok_or_else(|| ManagerError::RuntimeFailed {
                    message: format!("activity revision exhausted for {id}"),
                })?;
        let record = record.clone();
        self.persist(&records)?;
        Ok(record)
    }

    /// Host path of the registry snapshot, when this store persists one.
    /// The export dest guard denies destinations that would overwrite it.
    #[must_use]
    pub fn registry_path(&self) -> Option<&Path> {
        self.registry_path.as_deref()
    }

    fn records(&self) -> Result<MutexGuard<'_, HashMap<SandboxId, SandboxRecord>>, ManagerError> {
        self.records.lock().map_err(|_| ManagerError::StorePoisoned)
    }

    fn persist(&self, records: &HashMap<SandboxId, SandboxRecord>) -> Result<(), ManagerError> {
        let Some(path) = &self.registry_path else {
            return Ok(());
        };
        let mut snapshot: Vec<&SandboxRecord> = records.values().collect();
        snapshot.sort_by(|left, right| left.id.cmp(&right.id));
        let bytes = serde_json::to_vec_pretty(&snapshot)
            .map_err(|error| registry_error(path, "serialize", &error))?;
        write_snapshot(path, &bytes).map_err(|error| registry_error(path, "write", &error))
    }
}

fn write_snapshot(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let staged = staged_snapshot_path(path);
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&staged)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&staged, path)
}

fn staged_snapshot_path(path: &Path) -> PathBuf {
    let mut file_name = path
        .file_name()
        .map(OsStr::to_os_string)
        .unwrap_or_default();
    file_name.push(".tmp");
    path.with_file_name(file_name)
}

fn registry_error(path: &Path, action: &str, error: &dyn fmt::Display) -> ManagerError {
    ManagerError::RegistryPersistFailed {
        message: format!("{action} {}: {error}", path.display()),
    }
}
