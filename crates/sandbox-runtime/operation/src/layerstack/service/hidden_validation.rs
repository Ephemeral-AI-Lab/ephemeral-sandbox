use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::{self, JoinHandle};

use sandbox_runtime_layerstack::{
    HiddenQueuedWork, HiddenValidationObservation, HiddenValidationPublication, LayerStack,
};

use crate::layerstack::LayerStackServiceError;

const HIDDEN_VALIDATION_QUEUE_CAPACITY: usize = 2;

struct HiddenValidationJob {
    publication: HiddenValidationPublication,
    source_lease_id: String,
    queued_work: HiddenQueuedWork,
}

pub(super) struct HiddenValidationWorker {
    sender: Mutex<Option<SyncSender<HiddenValidationJob>>>,
    join: Mutex<Option<JoinHandle<()>>>,
    observation: HiddenValidationObservation,
    force_next_mismatch: Arc<AtomicBool>,
    last_correlation: Arc<Mutex<Option<String>>>,
    pause: Arc<(Mutex<bool>, Condvar)>,
}

impl HiddenValidationWorker {
    pub(super) fn spawn(
        mut stack: LayerStack,
        layer_stack_root: PathBuf,
        observation: HiddenValidationObservation,
    ) -> Result<Self, LayerStackServiceError> {
        let (sender, receiver) =
            mpsc::sync_channel::<HiddenValidationJob>(HIDDEN_VALIDATION_QUEUE_CAPACITY);
        let force_next_mismatch = Arc::new(AtomicBool::new(false));
        let worker_force_next_mismatch = Arc::clone(&force_next_mismatch);
        let last_correlation = Arc::new(Mutex::new(None));
        let worker_last_correlation = Arc::clone(&last_correlation);
        let pause = Arc::new((Mutex::new(false), Condvar::new()));
        let worker_pause = Arc::clone(&pause);
        let worker_observation = observation.clone();
        let join = thread::Builder::new()
            .name("layerstack-hidden-validation".to_string())
            .spawn(move || {
                while let Ok(job) = receiver.recv() {
                    let _worker_guard = worker_observation.begin_worker();
                    let _task = job.queued_work.start();
                    let (pause_lock, pause_changed) = &*worker_pause;
                    let mut paused = pause_lock.lock().unwrap_or_else(PoisonError::into_inner);
                    while *paused {
                        paused = pause_changed
                            .wait(paused)
                            .unwrap_or_else(PoisonError::into_inner);
                    }
                    drop(paused);
                    let forced_mismatch = worker_force_next_mismatch.swap(false, Ordering::AcqRel);
                    match stack.publish_hidden_validation(job.publication) {
                        Ok(outcome) => {
                            *worker_last_correlation
                                .lock()
                                .unwrap_or_else(PoisonError::into_inner) =
                                Some(outcome.correlation_id);
                            worker_observation
                                .record_completion(outcome.matched && !forced_mismatch);
                        }
                        Err(error) => {
                            eprintln!("hidden validation publication failed: {error}");
                            worker_observation.record_fallback();
                        }
                    }
                    if stack.release_lease(&job.source_lease_id).is_err() {
                        worker_observation.record_fallback();
                    }
                }
            })
            .map_err(|error| LayerStackServiceError::Init {
                layer_stack_root,
                error: format!("spawn hidden-validation worker: {error}"),
            })?;
        Ok(Self {
            sender: Mutex::new(Some(sender)),
            join: Mutex::new(Some(join)),
            observation,
            force_next_mismatch,
            last_correlation,
            pause,
        })
    }

    pub(super) fn submit(
        &self,
        publication: HiddenValidationPublication,
        source_lease_id: String,
        bytes: u64,
    ) -> Result<(), String> {
        let job = HiddenValidationJob {
            publication,
            source_lease_id,
            queued_work: self.observation.enqueue(bytes),
        };
        let sender = self.sender.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(sender) = sender.as_ref() else {
            return Err(job.source_lease_id);
        };
        match sender.try_send(job) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(job) | TrySendError::Disconnected(job)) => {
                self.observation.record_fallback();
                Err(job.source_lease_id)
            }
        }
    }

    pub(super) fn force_next_mismatch(&self) {
        self.force_next_mismatch.store(true, Ordering::Release);
    }

    pub(super) fn record_fallback(&self) {
        self.observation.record_fallback();
    }

    pub(super) fn last_correlation(&self) -> Option<String> {
        self.last_correlation
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub(super) fn set_paused(&self, paused: bool) {
        let (pause_lock, pause_changed) = &*self.pause;
        *pause_lock.lock().unwrap_or_else(PoisonError::into_inner) = paused;
        if !paused {
            pause_changed.notify_all();
        }
    }
}

impl Drop for HiddenValidationWorker {
    fn drop(&mut self) {
        self.set_paused(false);
        self.sender
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(join) = self
            .join
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            let _ = join.join();
        }
    }
}
