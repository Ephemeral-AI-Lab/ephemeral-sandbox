use std::sync::Arc;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::{broadcast, Mutex};

use crate::artifacts::{ArtifactError, ArtifactId, ArtifactStore};

pub const EVENT_SCHEMA_NAME: &str = "eos_benchmark_event";
pub const EVENT_SCHEMA_VERSION: u32 = 1;
const EVENT_CHANNEL_CAPACITY: usize = 1_024;
const MAX_EVENT_TEXT_BYTES: usize = 4_096;
const MAX_EVENT_ID_BYTES: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Planned,
    Queued,
    Preparing,
    Running,
    Verifying,
    TearingDown,
    Cancelling,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkState {
    Pending,
    Preparing,
    Running,
    Verifying,
    TearingDown,
    Completed,
    Failed,
    Cancelled,
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestState {
    WaitingAtBarrier,
    InFlight,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecyclePhase {
    Setup,
    Operation,
    Verify,
    Teardown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum EventData {
    RunState {
        state: RunState,
    },
    FamilyState {
        family: String,
        state: WorkState,
    },
    CellState {
        cell_id: String,
        state: WorkState,
    },
    TrialState {
        cell_id: String,
        trial_id: String,
        warmup: bool,
        state: WorkState,
    },
    TrialPhase {
        cell_id: String,
        trial_id: String,
        warmup: bool,
        phase: LifecyclePhase,
        state: WorkState,
    },
    RequestState {
        cell_id: String,
        trial_id: String,
        request_id: String,
        state: RequestState,
    },
    ResourceWindow {
        cell_id: String,
        trial_id: String,
        metric_id: String,
        value: Option<f64>,
        unavailable_reason: Option<String>,
    },
    Correctness {
        cell_id: String,
        trial_id: String,
        check_id: String,
        passed: bool,
        expected: String,
        actual: String,
        artifact_id: Option<String>,
    },
    Warning {
        code: String,
        message: String,
    },
    Log {
        level: LogLevel,
        message: String,
    },
    ReportReady {
        provisional: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventRecord {
    pub sequence: u64,
    pub run_id: String,
    pub monotonic_offset_ns: u64,
    pub data: EventData,
}

#[derive(Debug, Error)]
pub enum EventError {
    #[error(transparent)]
    Artifact(#[from] ArtifactError),
    #[error("event field {field} exceeds its {limit}-byte cap")]
    FieldTooLong { field: &'static str, limit: usize },
    #[error("event field {0} must not be empty")]
    EmptyField(&'static str),
    #[error("event resource value must be finite")]
    NonFiniteResourceValue,
    #[error("event persistence task failed: {0}")]
    PersistenceTask(String),
    #[error("event serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
}

#[derive(Debug)]
pub struct EventJournal {
    run_id: String,
    store: ArtifactStore,
    sequence: Mutex<u64>,
    sender: broadcast::Sender<EventRecord>,
}

impl EventData {
    #[must_use]
    pub const fn event_name(&self) -> &'static str {
        match self {
            Self::RunState { .. } => "run_state",
            Self::FamilyState { .. } => "family_state",
            Self::CellState { .. } => "cell_state",
            Self::TrialState { .. } => "trial_state",
            Self::TrialPhase { .. } => "trial_phase",
            Self::RequestState { .. } => "request_state",
            Self::ResourceWindow { .. } => "resource_window",
            Self::Correctness { .. } => "correctness",
            Self::Warning { .. } => "warning",
            Self::Log { .. } => "log",
            Self::ReportReady { .. } => "report_ready",
        }
    }

    fn validate(&self) -> Result<(), EventError> {
        match self {
            Self::RunState { .. } | Self::ReportReady { .. } => Ok(()),
            Self::FamilyState { family, .. } => bounded_id("family", family),
            Self::CellState { cell_id, .. } => bounded_id("cell_id", cell_id),
            Self::TrialState {
                cell_id, trial_id, ..
            }
            | Self::TrialPhase {
                cell_id, trial_id, ..
            } => {
                bounded_id("cell_id", cell_id)?;
                bounded_id("trial_id", trial_id)
            }
            Self::RequestState {
                cell_id,
                trial_id,
                request_id,
                ..
            } => {
                bounded_id("cell_id", cell_id)?;
                bounded_id("trial_id", trial_id)?;
                bounded_id("request_id", request_id)
            }
            Self::ResourceWindow {
                cell_id,
                trial_id,
                metric_id,
                value,
                unavailable_reason,
            } => {
                bounded_id("cell_id", cell_id)?;
                bounded_id("trial_id", trial_id)?;
                bounded_id("metric_id", metric_id)?;
                if value.is_some_and(|number| !number.is_finite()) {
                    return Err(EventError::NonFiniteResourceValue);
                }
                if let Some(reason) = unavailable_reason {
                    bounded_text("unavailable_reason", reason)?;
                }
                Ok(())
            }
            Self::Correctness {
                cell_id,
                trial_id,
                check_id,
                expected,
                actual,
                artifact_id,
                ..
            } => {
                bounded_id("cell_id", cell_id)?;
                bounded_id("trial_id", trial_id)?;
                bounded_id("check_id", check_id)?;
                bounded_text("expected", expected)?;
                bounded_text("actual", actual)?;
                if let Some(id) = artifact_id {
                    bounded_id("artifact_id", id)?;
                }
                Ok(())
            }
            Self::Warning { code, message } => {
                bounded_id("code", code)?;
                bounded_text("message", message)
            }
            Self::Log { message, .. } => bounded_text("message", message),
        }
    }
}

impl EventJournal {
    pub async fn open(store: ArtifactStore, run_id: &str) -> Result<Arc<Self>, EventError> {
        bounded_id("run_id", run_id)?;
        let replay_store = store.clone();
        let replay_run_id = run_id.to_owned();
        let events = tokio::task::spawn_blocking(move || {
            replay_store.read_records::<EventRecord>(
                &replay_run_id,
                ArtifactId::Events,
                EVENT_SCHEMA_NAME,
                EVENT_SCHEMA_VERSION,
            )
        })
        .await
        .map_err(|error| EventError::PersistenceTask(error.to_string()))??;
        let latest = events.last().map_or(0, |event| event.sequence);
        if events
            .windows(2)
            .any(|pair| pair[0].sequence >= pair[1].sequence)
        {
            return Err(EventError::PersistenceTask(
                "persisted event sequences are not strictly increasing".to_owned(),
            ));
        }
        let (sender, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        Ok(Arc::new(Self {
            run_id: run_id.to_owned(),
            store,
            sequence: Mutex::new(latest),
            sender,
        }))
    }

    pub async fn emit(
        &self,
        monotonic_offset_ns: u64,
        data: EventData,
    ) -> Result<EventRecord, EventError> {
        data.validate()?;
        let mut sequence = self.sequence.lock().await;
        let record = EventRecord {
            sequence: sequence.saturating_add(1),
            run_id: self.run_id.clone(),
            monotonic_offset_ns,
            data,
        };
        let store = self.store.clone();
        let run_id = self.run_id.clone();
        let persisted = record.clone();
        tokio::task::spawn_blocking(move || {
            store.append_record(
                &run_id,
                ArtifactId::Events,
                EVENT_SCHEMA_NAME,
                EVENT_SCHEMA_VERSION,
                &persisted,
            )
        })
        .await
        .map_err(|error| EventError::PersistenceTask(error.to_string()))??;
        *sequence = record.sequence;
        let _ = self.sender.send(record.clone());
        Ok(record)
    }

    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<EventRecord> {
        self.sender.subscribe()
    }

    pub async fn replay_after(&self, sequence: u64) -> Result<Vec<EventRecord>, EventError> {
        let store = self.store.clone();
        let run_id = self.run_id.clone();
        let records = tokio::task::spawn_blocking(move || {
            store.read_records::<EventRecord>(
                &run_id,
                ArtifactId::Events,
                EVENT_SCHEMA_NAME,
                EVENT_SCHEMA_VERSION,
            )
        })
        .await
        .map_err(|error| EventError::PersistenceTask(error.to_string()))??;
        Ok(records
            .into_iter()
            .filter(|event| event.sequence > sequence)
            .collect())
    }
}

pub fn encode_sse(record: &EventRecord) -> Result<String, EventError> {
    let data = serde_json::to_string(record)?;
    Ok(format!(
        "id: {}\nevent: {}\ndata: {data}\n\n",
        record.sequence,
        record.data.event_name()
    ))
}

fn bounded_id(field: &'static str, value: &str) -> Result<(), EventError> {
    if value.is_empty() {
        return Err(EventError::EmptyField(field));
    }
    if value.len() > MAX_EVENT_ID_BYTES {
        return Err(EventError::FieldTooLong {
            field,
            limit: MAX_EVENT_ID_BYTES,
        });
    }
    Ok(())
}

fn bounded_text(field: &'static str, value: &str) -> Result<(), EventError> {
    if value.len() > MAX_EVENT_TEXT_BYTES {
        return Err(EventError::FieldTooLong {
            field,
            limit: MAX_EVENT_TEXT_BYTES,
        });
    }
    Ok(())
}
