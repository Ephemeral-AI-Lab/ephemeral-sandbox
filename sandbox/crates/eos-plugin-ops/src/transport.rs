//! Daemon-side PPC request/reply transport.
//!
//! This is the boundary the daemon uses once a plugin service has connected its
//! `AF_UNIX` socket. Daemon callers use a synchronous API, but plugin operation
//! serialization is forbidden: the connection itself can carry many in-flight
//! operation requests. A dedicated reader thread routes reply messages by
//! `message_id`; self-managed plugin operations can also service
//! plugin-originated callback requests on the same socket before their final
//! operation reply arrives. Concurrent callback-capable operations are routed by
//! `parent_message_id` in the callback body.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

use eos_plugin::{PluginError, PpcDirection, PpcMessage};
use serde_json::json;

use crate::PpcError;

const MAX_PPC_MESSAGE_BYTES: usize = eos_plugin::wire::MAX_PPC_MESSAGE_BYTES;

type CallbackHandler = Arc<dyn Fn(PpcMessage) -> Result<PpcMessage, PpcError> + Send + Sync>;
type PpcResult = Result<PpcMessage, PpcError>;

/// A connected plugin service's PPC client: a synchronous request/reply façade
/// over the service socket, multiplexed by a background reader thread.
pub struct PpcClient {
    writer: MessageWriter,
    pending: PendingCalls,
}

impl std::fmt::Debug for PpcClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("PpcClient").finish_non_exhaustive()
    }
}

impl PpcClient {
    /// Wrap a connected service `stream`, spawning the reply-routing reader.
    pub fn new(stream: UnixStream) -> Result<Self, PpcError> {
        let reader_stream = stream.try_clone()?;
        let writer = MessageWriter::new(stream);
        let pending = PendingCalls::default();
        spawn_reader_thread(reader_stream, writer.clone(), pending.clone())?;
        Ok(Self { writer, pending })
    }

    /// Send a request and await its reply (no callbacks serviced).
    pub fn round_trip(
        &self,
        request: &PpcMessage,
        timeout: Duration,
    ) -> Result<PpcMessage, PpcError> {
        self.send_request(request, timeout, None)
    }

    /// Send a request and await its reply, servicing plugin-originated callback
    /// requests with `handle_callback` until the final reply arrives.
    pub fn round_trip_with_callbacks<F>(
        &self,
        request: &PpcMessage,
        timeout: Duration,
        handle_callback: F,
    ) -> Result<PpcMessage, PpcError>
    where
        F: FnMut(PpcMessage) -> Result<PpcMessage, PpcError> + Send + 'static,
    {
        let callback = Arc::new(Mutex::new(handle_callback));
        let handler: CallbackHandler = Arc::new(move |message| {
            let mut callback = callback
                .lock()
                .map_err(|_| PpcError::LockPoisoned("plugin ppc callback handler"))?;
            callback(message)
        });
        self.send_request(request, timeout, Some(handler))
    }

    fn send_request(
        &self,
        request: &PpcMessage,
        timeout: Duration,
        callback_handler: Option<CallbackHandler>,
    ) -> Result<PpcMessage, PpcError> {
        if request.direction != PpcDirection::Request {
            return Err(PluginError::Ppc(
                "daemon PPC round trip requires a request message".to_owned(),
            )
            .into());
        }

        let message_id = request.message_id.clone();
        let reply_rx = self
            .pending
            .register(message_id.clone(), callback_handler)?;

        if let Err(err) = self.writer.write_with_timeout(request, timeout) {
            let _ = self.pending.discard(&message_id);
            return Err(err);
        }

        match reply_rx.recv_timeout(timeout) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let _ = self.pending.discard(&message_id);
                Err(PluginError::Ppc(format!(
                    "timed out waiting for plugin PPC reply {message_id}"
                ))
                .into())
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(PluginError::Ppc(format!(
                "plugin PPC reply channel closed for {message_id}"
            ))
            .into()),
        }
    }
}

fn spawn_reader_thread(
    mut stream: UnixStream,
    writer: MessageWriter,
    pending: PendingCalls,
) -> Result<(), PpcError> {
    thread::Builder::new()
        .name("eos-plugin-ppc-reader".to_owned())
        .spawn(move || reader_loop(&mut stream, &writer, &pending))?;
    Ok(())
}

fn reader_loop(stream: &mut UnixStream, writer: &MessageWriter, pending: &PendingCalls) {
    loop {
        let message = match read_message(stream) {
            Ok(message) => message,
            Err(err) => {
                pending.fail_all(err.to_string());
                return;
            }
        };

        match message.direction {
            PpcDirection::Reply => pending.complete_reply(message),
            PpcDirection::Request => handle_callback(message, writer, pending),
        }
    }
}

fn handle_callback(message: PpcMessage, writer: &MessageWriter, pending: &PendingCalls) {
    let callback_message_id = message.message_id.clone();
    let (owner_id, handler) = match pending.callback_handler_for_message(&message) {
        Ok(found) => found,
        Err(message) => {
            let _ = write_callback_error(writer, &callback_message_id, &message);
            return;
        }
    };

    match handler(message) {
        Ok(reply) => {
            if reply.direction != PpcDirection::Reply {
                pending.fail_one(
                    &owner_id,
                    "plugin PPC callback response must use reply direction".to_owned(),
                );
                return;
            }
            if reply.message_id != callback_message_id {
                pending.fail_one(
                    &owner_id,
                    format!(
                        "plugin PPC callback response message_id {} did not match callback {}",
                        reply.message_id, callback_message_id
                    ),
                );
                return;
            }
            if let Err(err) = writer.write(&reply) {
                pending.fail_one(&owner_id, err.to_string());
            }
        }
        Err(err) => {
            let message = err.to_string();
            let _ = write_callback_error(writer, &callback_message_id, &message);
            pending.fail_one(&owner_id, message);
        }
    }
}

fn write_callback_error(
    writer: &MessageWriter,
    callback_message_id: &str,
    message: &str,
) -> Result<(), PpcError> {
    let body = json!({
        "success": false,
        "error": {
            "kind": "ppc_callback_error",
            "message": message,
        },
    });
    writer.write(&PpcMessage {
        message_id: callback_message_id.to_owned(),
        direction: PpcDirection::Reply,
        op: "reply".to_owned(),
        body: body.to_string(),
    })
}

#[derive(Clone)]
struct MessageWriter {
    stream: Arc<Mutex<UnixStream>>,
}

impl MessageWriter {
    fn new(stream: UnixStream) -> Self {
        Self {
            stream: Arc::new(Mutex::new(stream)),
        }
    }

    fn write_with_timeout(&self, message: &PpcMessage, timeout: Duration) -> Result<(), PpcError> {
        self.write_inner(message, Some(timeout))
    }

    fn write(&self, message: &PpcMessage) -> Result<(), PpcError> {
        self.write_inner(message, None)
    }

    fn write_inner(&self, message: &PpcMessage, timeout: Option<Duration>) -> Result<(), PpcError> {
        let mut writer = self
            .stream
            .lock()
            .map_err(|_| PpcError::LockPoisoned("plugin ppc writer"))?;
        if let Some(timeout) = timeout {
            writer.set_write_timeout(Some(timeout))?;
        }
        writer.write_all(&message.encode()?)?;
        writer.flush()?;
        Ok(())
    }
}

fn read_message(stream: &mut UnixStream) -> Result<PpcMessage, PpcError> {
    let bytes = read_message_bytes(stream)?;
    PpcMessage::decode(&bytes).map_err(PpcError::from)
}

/// Read one newline-terminated PPC message from `stream`, capped at the protocol
/// request-byte ceiling.
pub fn read_message_bytes(stream: &mut UnixStream) -> Result<Vec<u8>, PpcError> {
    let mut bytes = Vec::new();
    let mut one = [0_u8; 1];
    loop {
        let read = stream.read(&mut one)?;
        if read == 0 {
            return Err(
                PluginError::Ppc("plugin PPC stream closed before reply".to_owned()).into(),
            );
        }
        bytes.push(one[0]);
        if one[0] == b'\n' {
            return Ok(bytes);
        }
        if bytes.len() >= MAX_PPC_MESSAGE_BYTES {
            return Err(PluginError::Ppc(format!(
                "plugin PPC reply exceeds {MAX_PPC_MESSAGE_BYTES} byte limit"
            ))
            .into());
        }
    }
}

#[derive(Clone, Default)]
struct PendingCalls {
    inner: Arc<Mutex<HashMap<String, PendingRequest>>>,
}

struct PendingRequest {
    reply_tx: mpsc::Sender<PpcResult>,
    callback_handler: Option<CallbackHandler>,
}

impl PendingCalls {
    fn register(
        &self,
        message_id: String,
        callback_handler: Option<CallbackHandler>,
    ) -> Result<mpsc::Receiver<PpcResult>, PpcError> {
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

    fn discard(&self, message_id: &str) -> Result<(), PpcError> {
        self.lock()?.remove(message_id);
        Ok(())
    }

    fn complete_reply(&self, message: PpcMessage) {
        let message_id = message.message_id.clone();
        let Ok(pending_request) = self.take(&message_id) else {
            return;
        };
        if let Some(pending_request) = pending_request {
            let _ = pending_request.reply_tx.send(Ok(message));
        }
    }

    fn fail_one(&self, message_id: &str, message: String) {
        let Ok(pending_request) = self.take(message_id) else {
            return;
        };
        if let Some(pending_request) = pending_request {
            let _ = pending_request
                .reply_tx
                .send(Err(PluginError::Ppc(message).into()));
        }
    }

    fn fail_all(&self, message: String) {
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

    fn callback_handler_for_message(
        &self,
        message: &PpcMessage,
    ) -> Result<(String, CallbackHandler), String> {
        let pending = self
            .inner
            .lock()
            .map_err(|_| "plugin ppc pending lock poisoned".to_owned())?;
        if let Some(parent_id) = callback_parent_message_id(message) {
            let pending_request = pending.get(&parent_id).ok_or_else(|| {
                format!(
                    "plugin PPC callback {} referenced unknown parent_message_id {}",
                    message.message_id, parent_id
                )
            })?;
            let handler = pending_request.callback_handler.as_ref().ok_or_else(|| {
                format!(
                    "plugin PPC callback {} referenced read-only request {}",
                    message.message_id, parent_id
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
                message.op
            )),
            _ => Err(format!(
                "ambiguous plugin PPC callback {} without parent_message_id while {} callback-enabled operations are in flight",
                message.op,
                callback_ready.len()
            )),
        }
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, HashMap<String, PendingRequest>>, PpcError> {
        self.inner
            .lock()
            .map_err(|_| PpcError::LockPoisoned("plugin ppc pending"))
    }

    fn take(&self, message_id: &str) -> Result<Option<PendingRequest>, PpcError> {
        Ok(self.lock()?.remove(message_id))
    }
}

fn callback_parent_message_id(message: &PpcMessage) -> Option<String> {
    let body = serde_json::from_str::<serde_json::Value>(&message.body).ok()?;
    body.get("parent_message_id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}
