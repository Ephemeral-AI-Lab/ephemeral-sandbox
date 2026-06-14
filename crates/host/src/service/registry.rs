use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use anyhow::{bail, Context, Result};
use serde_json::Value;

use crate::container::{container_labels, resolve_published_addr, running_container_ids};

pub(crate) const SANDBOX_ID_LABEL: &str = "eos.sandbox_id";
pub(crate) const TCP_PORT_LABEL: &str = "eos.tcp_port";
pub(crate) const CREATED_BY_LABEL: &str = "eos.created_by";

#[derive(Debug)]
pub(crate) struct SandboxRecord {
    pub(crate) sandbox_id: String,
    pub(crate) container: String,
    pub(crate) token: String,
    pub(crate) forward_token: String,
    pub(crate) tcp_port: u16,
    pub(crate) created_by: String,
    endpoint: Mutex<Option<SocketAddr>>,
    lifecycle: SandboxLifecycle,
}

impl SandboxRecord {
    #[cfg(test)]
    pub(crate) fn new(
        sandbox_id: String,
        container: String,
        token: String,
        tcp_port: u16,
        created_by: String,
        endpoint: Option<SocketAddr>,
    ) -> Self {
        Self::new_with_forward_token(
            sandbox_id,
            container,
            token.clone(),
            token,
            tcp_port,
            created_by,
            endpoint,
        )
    }

    pub(crate) fn new_with_forward_token(
        sandbox_id: String,
        container: String,
        token: String,
        forward_token: String,
        tcp_port: u16,
        created_by: String,
        endpoint: Option<SocketAddr>,
    ) -> Self {
        Self {
            sandbox_id,
            container,
            token,
            forward_token,
            tcp_port,
            created_by,
            endpoint: Mutex::new(endpoint),
            lifecycle: SandboxLifecycle::default(),
        }
    }
    pub(crate) fn cached_endpoint(&self) -> Option<SocketAddr> {
        *self.endpoint.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn cache_endpoint(&self, addr: SocketAddr) {
        *self.endpoint.lock().unwrap_or_else(PoisonError::into_inner) = Some(addr);
    }

    pub(crate) fn invalidate_endpoint(&self) {
        *self.endpoint.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }

    pub(crate) fn begin_forward(&self) -> SandboxForwardGuard<'_> {
        self.lifecycle.begin_forward()
    }

    pub(crate) fn begin_respawn(&self) -> SandboxRespawnGuard<'_> {
        self.lifecycle.begin_respawn()
    }
}

#[derive(Debug, Default)]
struct SandboxLifecycle {
    state: Mutex<LifecycleState>,
    changed: Condvar,
}

#[derive(Debug, Default)]
struct LifecycleState {
    active_forwards: usize,
    waiting_respawns: usize,
    respawning: bool,
}

impl SandboxLifecycle {
    fn begin_forward(&self) -> SandboxForwardGuard<'_> {
        let mut state = self.lock();
        while state.respawning || state.waiting_respawns > 0 {
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
        state.active_forwards += 1;
        SandboxForwardGuard { lifecycle: self }
    }

    fn begin_respawn(&self) -> SandboxRespawnGuard<'_> {
        let mut state = self.lock();
        state.waiting_respawns += 1;
        while state.respawning || state.active_forwards > 0 {
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
        state.waiting_respawns -= 1;
        state.respawning = true;
        SandboxRespawnGuard { lifecycle: self }
    }

    fn end_forward(&self) {
        let mut state = self.lock();
        state.active_forwards = state.active_forwards.saturating_sub(1);
        if state.active_forwards == 0 {
            self.changed.notify_all();
        }
    }

    fn end_respawn(&self) {
        let mut state = self.lock();
        state.respawning = false;
        self.changed.notify_all();
    }

    fn lock(&self) -> MutexGuard<'_, LifecycleState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

pub(crate) struct SandboxForwardGuard<'a> {
    lifecycle: &'a SandboxLifecycle,
}

impl Drop for SandboxForwardGuard<'_> {
    fn drop(&mut self) {
        self.lifecycle.end_forward();
    }
}

pub(crate) struct SandboxRespawnGuard<'a> {
    lifecycle: &'a SandboxLifecycle,
}

impl Drop for SandboxRespawnGuard<'_> {
    fn drop(&mut self) {
        self.lifecycle.end_respawn();
    }
}

pub(crate) struct SandboxRegistry {
    state_dir: PathBuf,
    records: Mutex<HashMap<String, Arc<SandboxRecord>>>,
}

impl SandboxRegistry {
    pub(crate) fn open(state_dir: PathBuf) -> Result<Self> {
        fs::create_dir_all(&state_dir)
            .with_context(|| format!("create host state dir {}", state_dir.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = fs::Permissions::from_mode(0o700);
            fs::set_permissions(&state_dir, perms)
                .with_context(|| format!("chmod 700 {}", state_dir.display()))?;
        }
        Ok(Self {
            state_dir,
            records: Mutex::new(HashMap::new()),
        })
    }

    pub(crate) fn rebuild_from_docker(&self) -> usize {
        let ids = running_container_ids(&[SANDBOX_ID_LABEL]);
        let Ok(label_maps) = container_labels(&ids) else {
            return 0;
        };
        let mut adopted = 0;
        for labels in label_maps {
            let label = |key: &str| labels.get(key).and_then(Value::as_str);
            let Some(sandbox_id) = label(SANDBOX_ID_LABEL) else {
                continue;
            };
            let Ok(token) = self.load_token(sandbox_id) else {
                continue;
            };
            let forward_token = self
                .load_forward_token(sandbox_id)
                .unwrap_or_else(|_| token.clone());
            let Some(tcp_port) = label(TCP_PORT_LABEL).and_then(|port| port.parse::<u16>().ok())
            else {
                continue;
            };
            let created_by = label(CREATED_BY_LABEL).unwrap_or("unknown").to_owned();
            let record = SandboxRecord::new_with_forward_token(
                sandbox_id.to_owned(),
                sandbox_id.to_owned(),
                token,
                forward_token,
                tcp_port,
                created_by,
                None,
            );
            self.lock().insert(sandbox_id.to_owned(), Arc::new(record));
            adopted += 1;
        }
        adopted
    }

    pub(crate) fn insert(&self, record: SandboxRecord) -> Result<Arc<SandboxRecord>> {
        self.persist_token(&record.sandbox_id, &record.token)?;
        self.persist_forward_token(&record.sandbox_id, &record.forward_token)?;
        let record = Arc::new(record);
        self.lock()
            .insert(record.sandbox_id.clone(), Arc::clone(&record));
        Ok(record)
    }
    pub(crate) fn get(&self, sandbox_id: &str) -> Option<Arc<SandboxRecord>> {
        self.lock().get(sandbox_id).cloned()
    }

    pub(crate) fn remove(&self, sandbox_id: &str) -> Option<Arc<SandboxRecord>> {
        let removed = self.lock().remove(sandbox_id);
        if removed.is_some() {
            let _ = fs::remove_file(self.token_path(sandbox_id));
            let _ = fs::remove_file(self.forward_token_path(sandbox_id));
        }
        removed
    }
    pub(crate) fn list(&self) -> Vec<Arc<SandboxRecord>> {
        let mut records: Vec<_> = self.lock().values().cloned().collect();
        records.sort_by(|a, b| a.sandbox_id.cmp(&b.sandbox_id));
        records
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<SandboxRecord>>> {
        self.records.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn token_path(&self, sandbox_id: &str) -> PathBuf {
        self.state_dir.join(format!("{sandbox_id}.token"))
    }

    fn forward_token_path(&self, sandbox_id: &str) -> PathBuf {
        self.state_dir.join(format!("{sandbox_id}.forward-token"))
    }

    fn persist_token(&self, sandbox_id: &str, token: &str) -> Result<()> {
        let path = self.token_path(sandbox_id);
        self.persist_secret(&path, token)
    }

    fn persist_forward_token(&self, sandbox_id: &str, token: &str) -> Result<()> {
        let path = self.forward_token_path(sandbox_id);
        self.persist_secret(&path, token)
    }

    fn persist_secret(&self, path: &Path, token: &str) -> Result<()> {
        let parent = path.parent().ok_or_else(|| {
            anyhow::anyhow!("token path has no parent directory: {}", path.display())
        })?;
        fs::create_dir_all(parent)
            .with_context(|| format!("create token directory {}", parent.display()))?;
        let tmp = path.with_file_name(format!(
            ".{}.{}.tmp",
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("token"),
            uuid::Uuid::new_v4()
        ));
        let result = (|| -> Result<()> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)
                .with_context(|| format!("open temporary token {}", tmp.display()))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                file.set_permissions(fs::Permissions::from_mode(0o600))
                    .with_context(|| format!("chmod 600 {}", tmp.display()))?;
            }
            file.write_all(token.as_bytes())
                .with_context(|| format!("write temporary token {}", tmp.display()))?;
            file.sync_all()
                .with_context(|| format!("fsync temporary token {}", tmp.display()))?;
            drop(file);
            fs::rename(&tmp, path)
                .with_context(|| format!("rename token {} to {}", tmp.display(), path.display()))?;
            fsync_parent(path)?;
            Ok(())
        })();
        if let Err(err) = result {
            let _ = fs::remove_file(&tmp);
            return Err(err);
        }
        Ok(())
    }

    pub(crate) fn load_token(&self, sandbox_id: &str) -> Result<String> {
        let path = self.token_path(sandbox_id);
        let token =
            fs::read_to_string(&path).with_context(|| format!("read token {}", path.display()))?;
        Ok(token.trim().to_owned())
    }

    pub(crate) fn load_forward_token(&self, sandbox_id: &str) -> Result<String> {
        let path = self.forward_token_path(sandbox_id);
        let token = fs::read_to_string(&path)
            .with_context(|| format!("read forward token {}", path.display()))?;
        Ok(token.trim().to_owned())
    }
}

fn fsync_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::File::open(parent)
            .and_then(|file| file.sync_all())
            .with_context(|| format!("fsync token directory {}", parent.display()))?;
    }
    Ok(())
}

pub(crate) fn cached_or_resolve_endpoint(record: &SandboxRecord) -> Result<SocketAddr> {
    if let Some(addr) = record.cached_endpoint() {
        return Ok(addr);
    }
    resolve_endpoint(record)
}

pub(crate) fn resolve_endpoint(record: &SandboxRecord) -> Result<SocketAddr> {
    record.invalidate_endpoint();
    match resolve_published_addr(&record.container, record.tcp_port)? {
        Some(addr) => {
            record.cache_endpoint(addr);
            Ok(addr)
        }
        None => bail!(
            "no published port {} for container {}",
            record.tcp_port,
            record.container
        ),
    }
}
