use std::collections::BTreeMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use thiserror::Error;
use tokio::sync::{Mutex, MutexGuard};
use uuid::Uuid;

use crate::artifacts::{ArtifactError, ArtifactStore};
use crate::config::{ConfigError, SettingsResponse, StartupConfig, WorkspaceRootSource};
use crate::events::{EventError, EventJournal};
use crate::scheduler::{CampaignGate, RunArtifacts};

/// Canonical fixed executables proven usable by startup preflight. Plans never
/// supply or override these paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionDependencies {
    pub gateway_binary: PathBuf,
    pub daemon_binary: PathBuf,
    pub docker_binary: PathBuf,
    pub git_binary: PathBuf,
    pub stat_binary: PathBuf,
    pub df_binary: PathBuf,
    pub docker_engine_version: String,
}

#[derive(Debug)]
struct RuntimePaths {
    config: StartupConfig,
    settings_source: WorkspaceRootSource,
    artifacts: ArtifactStore,
}

#[derive(Debug)]
pub struct AppState {
    runtime: RwLock<RuntimePaths>,
    authority: String,
    origin: String,
    nonce: String,
    instance_id: String,
    execution_ready: AtomicBool,
    execution_dependencies: RwLock<Option<ExecutionDependencies>>,
    execution_unavailable_reason: RwLock<Option<String>>,
    pub campaigns: CampaignGate,
    run_registration: Mutex<()>,
    active_run_artifacts: RwLock<BTreeMap<String, Arc<RunArtifacts>>>,
    journals: Mutex<BTreeMap<String, Arc<EventJournal>>>,
}

#[derive(Debug, Error)]
pub enum AppError {
    #[error(transparent)]
    Artifact(#[from] ArtifactError),
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Event(#[from] EventError),
    #[error("application path state lock was poisoned")]
    LockPoisoned,
    #[error("active run artifact identity does not match run {0}")]
    RunArtifactMismatch(String),
}

impl AppState {
    pub fn new(
        config: StartupConfig,
        settings_source: WorkspaceRootSource,
        authority: String,
        execution_ready: bool,
    ) -> Result<Arc<Self>, AppError> {
        let artifacts = ArtifactStore::new(&config.paths.results)?;
        let origin = format!("http://{authority}");
        Ok(Arc::new(Self {
            runtime: RwLock::new(RuntimePaths {
                config,
                settings_source,
                artifacts,
            }),
            authority,
            origin,
            nonce: format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple()),
            instance_id: Uuid::new_v4().to_string(),
            execution_ready: AtomicBool::new(execution_ready),
            execution_dependencies: RwLock::new(None),
            execution_unavailable_reason: RwLock::new(None),
            campaigns: CampaignGate::default(),
            run_registration: Mutex::new(()),
            active_run_artifacts: RwLock::new(BTreeMap::new()),
            journals: Mutex::new(BTreeMap::new()),
        }))
    }

    #[must_use]
    pub fn authority(&self) -> &str {
        &self.authority
    }

    #[must_use]
    pub fn origin(&self) -> &str {
        &self.origin
    }

    #[must_use]
    pub fn nonce(&self) -> &str {
        &self.nonce
    }

    #[must_use]
    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    #[must_use]
    pub fn execution_ready(&self) -> bool {
        self.execution_ready.load(Ordering::Acquire)
    }

    pub fn set_execution_ready(&self, ready: bool) {
        self.execution_ready.store(ready, Ordering::Release);
    }

    pub fn install_execution_dependencies(
        &self,
        dependencies: ExecutionDependencies,
    ) -> Result<(), AppError> {
        *self
            .execution_dependencies
            .write()
            .map_err(|_| AppError::LockPoisoned)? = Some(dependencies);
        *self
            .execution_unavailable_reason
            .write()
            .map_err(|_| AppError::LockPoisoned)? = None;
        self.set_execution_ready(true);
        Ok(())
    }

    pub fn mark_execution_unavailable(&self, reason: impl Into<String>) -> Result<(), AppError> {
        *self
            .execution_dependencies
            .write()
            .map_err(|_| AppError::LockPoisoned)? = None;
        *self
            .execution_unavailable_reason
            .write()
            .map_err(|_| AppError::LockPoisoned)? = Some(reason.into());
        self.set_execution_ready(false);
        Ok(())
    }

    pub fn execution_dependencies(&self) -> Result<Option<ExecutionDependencies>, AppError> {
        self.execution_dependencies
            .read()
            .map(|dependencies| dependencies.clone())
            .map_err(|_| AppError::LockPoisoned)
    }

    pub fn execution_unavailable_reason(&self) -> Result<Option<String>, AppError> {
        self.execution_unavailable_reason
            .read()
            .map(|reason| reason.clone())
            .map_err(|_| AppError::LockPoisoned)
    }

    pub fn config(&self) -> Result<StartupConfig, AppError> {
        self.runtime
            .read()
            .map(|runtime| runtime.config.clone())
            .map_err(|_| AppError::LockPoisoned)
    }

    pub fn artifacts(&self) -> Result<ArtifactStore, AppError> {
        self.runtime
            .read()
            .map(|runtime| runtime.artifacts.clone())
            .map_err(|_| AppError::LockPoisoned)
    }

    pub fn settings(&self) -> Result<SettingsResponse, AppError> {
        self.runtime
            .read()
            .map(|runtime| {
                runtime
                    .config
                    .settings_response(runtime.settings_source.clone())
            })
            .map_err(|_| AppError::LockPoisoned)
    }

    pub fn update_workspace_root(&self, root: &Path) -> Result<SettingsResponse, AppError> {
        let mut runtime = self.runtime.write().map_err(|_| AppError::LockPoisoned)?;
        runtime.config.persist_workspace_root(root)?;
        runtime.artifacts = ArtifactStore::new(&runtime.config.paths.results)?;
        runtime.settings_source = WorkspaceRootSource::ApiUpdate;
        Ok(runtime
            .config
            .settings_response(runtime.settings_source.clone()))
    }

    pub async fn register_journal(&self, run_id: &str, journal: Arc<EventJournal>) {
        self.journals
            .lock()
            .await
            .insert(run_id.to_owned(), journal);
    }

    pub async fn lock_run_registration(&self) -> MutexGuard<'_, ()> {
        self.run_registration.lock().await
    }

    pub fn register_run_artifacts(
        &self,
        run_id: &str,
        artifacts: Arc<RunArtifacts>,
    ) -> Result<(), AppError> {
        if artifacts.run_id != run_id {
            return Err(AppError::RunArtifactMismatch(run_id.to_owned()));
        }
        let mut runs = self
            .active_run_artifacts
            .write()
            .map_err(|_| AppError::LockPoisoned)?;
        if runs
            .get(run_id)
            .is_some_and(|registered| !Arc::ptr_eq(registered, &artifacts))
        {
            return Err(AppError::RunArtifactMismatch(run_id.to_owned()));
        }
        runs.insert(run_id.to_owned(), artifacts);
        Ok(())
    }

    pub fn run_artifacts(&self, run_id: &str) -> Result<Option<Arc<RunArtifacts>>, AppError> {
        self.active_run_artifacts
            .read()
            .map(|runs| runs.get(run_id).cloned())
            .map_err(|_| AppError::LockPoisoned)
    }

    pub fn forget_run_artifacts(&self, run_id: &str) -> Result<(), AppError> {
        self.active_run_artifacts
            .write()
            .map_err(|_| AppError::LockPoisoned)?
            .remove(run_id);
        Ok(())
    }

    pub async fn journal(&self, run_id: &str) -> Result<Arc<EventJournal>, AppError> {
        if let Some(journal) = self.journals.lock().await.get(run_id).cloned() {
            return Ok(journal);
        }
        let journal = EventJournal::open(self.artifacts()?, run_id).await?;
        self.register_journal(run_id, journal.clone()).await;
        Ok(journal)
    }

    pub async fn forget_journal(&self, run_id: &str) {
        self.journals.lock().await.remove(run_id);
    }
}
