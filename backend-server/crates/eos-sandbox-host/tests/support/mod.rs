//! Shared `#[cfg(test)]` mock [`ProviderAdapter`] for registry / daemon-client /
//! lifecycle / provisioning unit tests (LSP substitutability, spec §9). It needs
//! no real Docker daemon: `exec` is driven by an injected closure (which may
//! carry interior-mutable call counters to script the recovery state machine),
//! and `daemon_tcp_endpoint` records resolve counts for the single-flight test.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use eos_types::SandboxId;

use crate::error::SandboxHostError;
use crate::provider::{
    sealed, ContextPreparer, CreateSandboxSpec, DaemonTcpEndpoint, DockerContextPreparer, ExecOpts,
    Labels, ProviderAdapter, ProviderHealth, ProviderKind, RawExecResult, SandboxInfo,
    SnapshotInfo,
};

type ExecHandler = Arc<dyn Fn(&str) -> RawExecResult + Send + Sync>;
type ArchiveLog = Arc<Mutex<Vec<ArchiveCall>>>;

pub(crate) struct ArchiveCall {
    pub(crate) dest_dir: String,
    pub(crate) tar_stream: Vec<u8>,
}

/// A scriptable in-crate `ProviderAdapter` mock.
pub(crate) struct MockAdapter {
    id: String,
    project_dir: Option<String>,
    exec_handler: ExecHandler,
    calls: Arc<Mutex<Vec<String>>>,
    archive_calls: ArchiveLog,
    tcp_endpoint: Option<DaemonTcpEndpoint>,
    tcp_resolves: Arc<AtomicUsize>,
    tcp_delay_ms: u64,
}

impl std::fmt::Debug for MockAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MockAdapter")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl MockAdapter {
    pub(crate) fn new() -> Self {
        Self {
            id: "sb-mock".to_owned(),
            project_dir: None,
            exec_handler: Arc::new(|_cmd| RawExecResult {
                exit_code: 0,
                stdout: String::new(),
                stderr: String::new(),
                success: true,
            }),
            calls: Arc::new(Mutex::new(Vec::new())),
            archive_calls: Arc::new(Mutex::new(Vec::new())),
            tcp_endpoint: None,
            tcp_resolves: Arc::new(AtomicUsize::new(0)),
            tcp_delay_ms: 0,
        }
    }

    pub(crate) fn with_id(mut self, id: &str) -> Self {
        self.id = id.to_owned();
        self
    }

    pub(crate) fn with_exec<F>(mut self, f: F) -> Self
    where
        F: Fn(&str) -> RawExecResult + Send + Sync + 'static,
    {
        self.exec_handler = Arc::new(f);
        self
    }

    pub(crate) fn with_tcp(mut self, endpoint: DaemonTcpEndpoint) -> Self {
        self.tcp_endpoint = Some(endpoint);
        self
    }

    pub(crate) fn with_tcp_delay_ms(mut self, ms: u64) -> Self {
        self.tcp_delay_ms = ms;
        self
    }

    /// A shared handle to the recorded exec command log.
    pub(crate) fn call_log(&self) -> Arc<Mutex<Vec<String>>> {
        Arc::clone(&self.calls)
    }

    pub(crate) fn archive_log(&self) -> ArchiveLog {
        Arc::clone(&self.archive_calls)
    }

    /// A shared handle to the `daemon_tcp_endpoint` resolve counter — grab it
    /// before moving the adapter behind `Arc<dyn ProviderAdapter>` so the
    /// single-flight test can read the count through the type-erased boundary.
    pub(crate) fn tcp_resolve_counter(&self) -> Arc<AtomicUsize> {
        Arc::clone(&self.tcp_resolves)
    }

    fn info(&self) -> SandboxInfo {
        SandboxInfo {
            id: self.id.parse().expect("mock id non-empty"),
            name: self.id.clone(),
            image: None,
            snapshot: None,
            state: "running".to_owned(),
            labels: Labels::new(),
            project_dir: self.project_dir.clone(),
            managed_by_app: true,
        }
    }
}

impl sealed::Sealed for MockAdapter {}

#[async_trait]
impl ProviderAdapter for MockAdapter {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Docker
    }

    async fn health(&self) -> Result<ProviderHealth, SandboxHostError> {
        Ok(ProviderHealth {
            provider: "docker".to_owned(),
            healthy: true,
            server_version: None,
            containers_running: None,
            kernel_version: None,
            operating_system: None,
            error: None,
        })
    }

    async fn list_snapshots(&self) -> Result<Vec<SnapshotInfo>, SandboxHostError> {
        Ok(Vec::new())
    }

    async fn create(&self, _spec: &CreateSandboxSpec) -> Result<SandboxInfo, SandboxHostError> {
        Ok(self.info())
    }

    async fn get(&self, _id: &SandboxId) -> Result<SandboxInfo, SandboxHostError> {
        Ok(self.info())
    }

    async fn list(&self) -> Result<Vec<SandboxInfo>, SandboxHostError> {
        Ok(vec![self.info()])
    }

    async fn start(&self, _id: &SandboxId) -> Result<SandboxInfo, SandboxHostError> {
        Ok(self.info())
    }

    async fn stop(&self, _id: &SandboxId) -> Result<SandboxInfo, SandboxHostError> {
        Ok(self.info())
    }

    async fn delete(&self, _id: &SandboxId) -> Result<(), SandboxHostError> {
        Ok(())
    }

    async fn set_labels(
        &self,
        _id: &SandboxId,
        _labels: &Labels,
    ) -> Result<SandboxInfo, SandboxHostError> {
        Ok(self.info())
    }

    async fn daemon_tcp_endpoint(
        &self,
        _id: &SandboxId,
    ) -> Result<Option<DaemonTcpEndpoint>, SandboxHostError> {
        self.tcp_resolves.fetch_add(1, Ordering::SeqCst);
        if self.tcp_delay_ms > 0 {
            tokio::time::sleep(Duration::from_millis(self.tcp_delay_ms)).await;
        }
        Ok(self.tcp_endpoint.clone())
    }

    async fn exec(
        &self,
        _id: &SandboxId,
        command: &str,
        _opts: &ExecOpts,
    ) -> Result<RawExecResult, SandboxHostError> {
        self.calls
            .lock()
            .expect("mock calls lock not poisoned")
            .push(command.to_owned());
        Ok((self.exec_handler)(command))
    }

    async fn put_archive(
        &self,
        _id: &SandboxId,
        tar_stream: &[u8],
        dest_dir: &str,
    ) -> Result<(), SandboxHostError> {
        self.archive_calls
            .lock()
            .expect("mock archive calls lock not poisoned")
            .push(ArchiveCall {
                dest_dir: dest_dir.to_owned(),
                tar_stream: tar_stream.to_owned(),
            });
        Ok(())
    }

    fn context_preparer(&self, id: &SandboxId) -> ContextPreparer {
        ContextPreparer::Docker(DockerContextPreparer::new(id.clone()))
    }
}
