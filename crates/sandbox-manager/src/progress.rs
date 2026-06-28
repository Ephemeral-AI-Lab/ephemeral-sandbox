use std::sync::Arc;
use std::time::Instant;

#[derive(Clone)]
pub struct ProgressSink {
    started: Instant,
    emit: Arc<dyn Fn(ManagerProgressEvent) + Send + Sync>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagerProgressEvent {
    pub op: String,
    pub phase: String,
    pub state: String,
    pub message: String,
    pub sandbox_id: Option<String>,
    pub elapsed_ms: u128,
}

impl ProgressSink {
    #[must_use]
    pub fn new<F>(emit: F) -> Self
    where
        F: Fn(ManagerProgressEvent) + Send + Sync + 'static,
    {
        Self {
            started: Instant::now(),
            emit: Arc::new(emit),
        }
    }

    #[must_use]
    pub fn noop() -> Self {
        Self::new(|_| {})
    }

    pub fn emit(
        &self,
        op: impl Into<String>,
        phase: impl Into<String>,
        state: impl Into<String>,
        message: impl Into<String>,
        sandbox_id: Option<&str>,
    ) {
        (self.emit)(ManagerProgressEvent {
            op: op.into(),
            phase: phase.into(),
            state: state.into(),
            message: message.into(),
            sandbox_id: sandbox_id.map(str::to_owned),
            elapsed_ms: self.started.elapsed().as_millis(),
        });
    }
}
