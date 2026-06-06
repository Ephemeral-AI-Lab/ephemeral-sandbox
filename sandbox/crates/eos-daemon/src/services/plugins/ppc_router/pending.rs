//! In-flight PPC request tracking and callback ownership resolution.

use std::collections::HashMap;
use std::sync::{mpsc, Arc, Mutex};

use eos_plugin::{PluginError, PpcEnvelope};

use crate::error::DaemonError;

pub(super) type CallbackHandler =
    Arc<dyn Fn(PpcEnvelope) -> Result<PpcEnvelope, DaemonError> + Send + Sync>;
pub(super) type PpcResult = Result<PpcEnvelope, DaemonError>;

#[derive(Clone, Default)]
pub(super) struct PendingCalls {
    inner: Arc<Mutex<HashMap<String, PendingRequest>>>,
}

struct PendingRequest {
    reply_tx: mpsc::Sender<PpcResult>,
    callback_handler: Option<CallbackHandler>,
}

impl PendingCalls {
    pub(super) fn register(
        &self,
        message_id: String,
        callback_handler: Option<CallbackHandler>,
    ) -> Result<mpsc::Receiver<PpcResult>, DaemonError> {
        let (reply_tx, reply_rx) = mpsc::channel();
        let mut pending = self.lock()?;
        if pending.contains_key(&message_id) {
            return Err(PluginError::Ppc(format!(
                "duplicate in-flight plugin PPC message_id {message_id}"
            ))
            .into());
        }
        pending.insert(
            message_id,
            PendingRequest {
                reply_tx,
                callback_handler,
            },
        );
        Ok(reply_rx)
    }

    pub(super) fn discard(&self, message_id: &str) -> Result<(), DaemonError> {
        self.lock()?.remove(message_id);
        Ok(())
    }

    pub(super) fn complete_reply(&self, frame: PpcEnvelope) {
        let message_id = frame.message_id.clone();
        let Ok(pending_request) = self.take(&message_id) else {
            return;
        };
        // A reply matching no in-flight request is a benign late/duplicate reply
        // (its caller already timed out and `discard`ed the entry) or a stray
        // frame. Drop it: failing the whole pending set here would cascade-fail
        // healthy concurrent requests multiplexed on the same client. A genuinely
        // dead connection is still surfaced by the reader loop's IO-error path,
        // which is the only caller of `fail_all`.
        if let Some(pending_request) = pending_request {
            let _ = pending_request.reply_tx.send(Ok(frame));
        }
    }

    pub(super) fn fail_one(&self, message_id: &str, message: String) {
        let Ok(pending_request) = self.take(message_id) else {
            return;
        };
        if let Some(pending_request) = pending_request {
            let _ = pending_request
                .reply_tx
                .send(Err(PluginError::Ppc(message).into()));
        }
    }

    pub(super) fn fail_all(&self, message: String) {
        let pending_requests = match self.lock() {
            Ok(mut pending) => pending
                .drain()
                .map(|(_, request)| request)
                .collect::<Vec<_>>(),
            Err(_) => return,
        };
        for pending_request in pending_requests {
            let _ = pending_request
                .reply_tx
                .send(Err(PluginError::Ppc(message.clone()).into()));
        }
    }

    pub(super) fn callback_handler_for_frame(
        &self,
        frame: &PpcEnvelope,
    ) -> Result<(String, CallbackHandler), String> {
        let pending = self
            .inner
            .lock()
            .map_err(|_| "plugin ppc pending lock poisoned".to_owned())?;
        if let Some(parent_id) = callback_parent_message_id(frame) {
            let pending_request = pending.get(&parent_id).ok_or_else(|| {
                format!(
                    "plugin PPC callback {} referenced unknown parent_message_id {}",
                    frame.message_id, parent_id
                )
            })?;
            let handler = pending_request.callback_handler.as_ref().ok_or_else(|| {
                format!(
                    "plugin PPC callback {} referenced read-only request {}",
                    frame.message_id, parent_id
                )
            })?;
            return Ok((parent_id, Arc::clone(handler)));
        }

        let callback_ready = pending
            .iter()
            .filter_map(|(message_id, request)| {
                request
                    .callback_handler
                    .as_ref()
                    .map(|handler| (message_id.clone(), Arc::clone(handler)))
            })
            .collect::<Vec<_>>();
        match callback_ready.as_slice() {
            [(message_id, handler)] => Ok((message_id.clone(), Arc::clone(handler))),
            [] => Err(format!(
                "unexpected plugin PPC callback {} while no callback-enabled operation is in flight",
                frame.op
            )),
            _ => Err(format!(
                "ambiguous plugin PPC callback {} without parent_message_id while {} callback-enabled operations are in flight",
                frame.op,
                callback_ready.len()
            )),
        }
    }

    fn lock(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, HashMap<String, PendingRequest>>, DaemonError> {
        self.inner
            .lock()
            .map_err(|_| DaemonError::StateLockPoisoned("plugin ppc pending"))
    }

    fn take(&self, message_id: &str) -> Result<Option<PendingRequest>, DaemonError> {
        Ok(self.lock()?.remove(message_id))
    }
}

fn callback_parent_message_id(frame: &PpcEnvelope) -> Option<String> {
    let body = serde_json::from_str::<serde_json::Value>(&frame.body).ok()?;
    body.get("parent_message_id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}
