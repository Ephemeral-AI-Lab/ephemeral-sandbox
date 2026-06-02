//! Daemon-side PPC request/reply transport.
//!
//! This is the boundary the daemon uses once a plugin service has connected its
//! `AF_UNIX` socket. Daemon callers use a synchronous API, but plugin operation
//! serialization is forbidden: the connection itself can carry many in-flight
//! operation requests. A dedicated reader thread routes reply frames by
//! `message_id`; self-managed plugin operations can also service
//! plugin-originated callback requests on the same socket before their final
//! operation reply arrives. Concurrent callback-capable operations are routed by
//! `parent_message_id` in the callback body, with a legacy message-id prefix
//! fallback for older harnesses.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

use eos_plugin::{PluginError, PpcDirection, PpcEnvelope};
use serde_json::json;

use crate::error::DaemonError;

pub(super) const DEFAULT_PLUGIN_PPC_TIMEOUT_MS: u64 = 5_000;

const MAX_PPC_FRAME_BYTES: usize = eos_protocol::MAX_REQUEST_BYTES;

type CallbackHandler = Arc<dyn Fn(PpcEnvelope) -> Result<PpcEnvelope, DaemonError> + Send + Sync>;
type PpcResult = Result<PpcEnvelope, DaemonError>;

struct PendingRequest {
    reply_tx: mpsc::Sender<PpcResult>,
    callback_handler: Option<CallbackHandler>,
}

pub(super) struct PpcClient {
    writer: Arc<Mutex<UnixStream>>,
    pending: Arc<Mutex<HashMap<String, PendingRequest>>>,
}

impl std::fmt::Debug for PpcClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("PpcClient").finish_non_exhaustive()
    }
}

impl PpcClient {
    pub(super) fn new(stream: UnixStream) -> Result<Self, DaemonError> {
        let reader_stream = stream.try_clone()?;
        let writer = Arc::new(Mutex::new(stream));
        let pending = Arc::new(Mutex::new(HashMap::new()));
        spawn_reader_thread(reader_stream, Arc::clone(&writer), Arc::clone(&pending))?;
        Ok(Self { writer, pending })
    }

    pub(super) fn round_trip(
        &self,
        request: &PpcEnvelope,
        timeout: Duration,
    ) -> Result<PpcEnvelope, DaemonError> {
        self.send_request(request, timeout, None)
    }

    pub(super) fn round_trip_with_callbacks<F>(
        &self,
        request: &PpcEnvelope,
        timeout: Duration,
        handle_callback: F,
    ) -> Result<PpcEnvelope, DaemonError>
    where
        F: FnMut(PpcEnvelope) -> Result<PpcEnvelope, DaemonError> + Send + 'static,
    {
        let callback = Arc::new(Mutex::new(handle_callback));
        let handler: CallbackHandler = Arc::new(move |frame| {
            callback
                .lock()
                .map_err(|_| DaemonError::StateLockPoisoned("plugin ppc callback handler"))?(
                frame
            )
        });
        self.send_request(request, timeout, Some(handler))
    }

    fn send_request(
        &self,
        request: &PpcEnvelope,
        timeout: Duration,
        callback_handler: Option<CallbackHandler>,
    ) -> Result<PpcEnvelope, DaemonError> {
        if request.direction != PpcDirection::Request {
            return Err(PluginError::Ppc(
                "daemon PPC round trip requires a request envelope".to_owned(),
            )
            .into());
        }
        let message_id = request.message_id.clone();
        let (reply_tx, reply_rx) = mpsc::channel();
        {
            let mut pending = self
                .pending
                .lock()
                .map_err(|_| DaemonError::StateLockPoisoned("plugin ppc pending"))?;
            if pending.contains_key(&message_id) {
                return Err(PluginError::Ppc(format!(
                    "duplicate in-flight plugin PPC message_id {message_id}"
                ))
                .into());
            }
            pending.insert(
                message_id.clone(),
                PendingRequest {
                    reply_tx,
                    callback_handler,
                },
            );
        }

        let write_result = self.write_frame(request, timeout);
        if let Err(err) = write_result {
            let _ = self.remove_pending(&message_id);
            return Err(err);
        }

        match reply_rx.recv_timeout(timeout) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let _ = self.remove_pending(&message_id);
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

    fn write_frame(&self, frame: &PpcEnvelope, timeout: Duration) -> Result<(), DaemonError> {
        let mut writer = self
            .writer
            .lock()
            .map_err(|_| DaemonError::StateLockPoisoned("plugin ppc writer"))?;
        writer.set_write_timeout(Some(timeout))?;
        writer.write_all(&frame.encode()?)?;
        writer.flush()?;
        Ok(())
    }

    fn remove_pending(&self, message_id: &str) -> Result<(), DaemonError> {
        self.pending
            .lock()
            .map_err(|_| DaemonError::StateLockPoisoned("plugin ppc pending"))?
            .remove(message_id);
        Ok(())
    }
}

fn spawn_reader_thread(
    mut stream: UnixStream,
    writer: Arc<Mutex<UnixStream>>,
    pending: Arc<Mutex<HashMap<String, PendingRequest>>>,
) -> Result<(), DaemonError> {
    thread::Builder::new()
        .name("eos-plugin-ppc-reader".to_owned())
        .spawn(move || reader_loop(&mut stream, &writer, &pending))?;
    Ok(())
}

fn reader_loop(
    stream: &mut UnixStream,
    writer: &Arc<Mutex<UnixStream>>,
    pending: &Arc<Mutex<HashMap<String, PendingRequest>>>,
) {
    loop {
        let frame = match read_frame(stream)
            .and_then(|bytes| PpcEnvelope::decode(&bytes).map_err(DaemonError::from))
        {
            Ok(frame) => frame,
            Err(err) => {
                fail_all_pending(pending, err.to_string());
                return;
            }
        };

        match frame.direction {
            PpcDirection::Reply => route_reply(frame, pending),
            PpcDirection::Request => handle_callback(frame, writer, pending),
        }
    }
}

fn route_reply(frame: PpcEnvelope, pending: &Arc<Mutex<HashMap<String, PendingRequest>>>) {
    let pending_request = match pending.lock() {
        Ok(mut pending) => pending.remove(&frame.message_id),
        Err(_) => return,
    };
    if let Some(pending_request) = pending_request {
        let _ = pending_request.reply_tx.send(Ok(frame));
        return;
    }
    fail_all_pending(
        pending,
        format!(
            "plugin PPC reply message_id {} did not match any in-flight request",
            frame.message_id
        ),
    );
}

fn handle_callback(
    frame: PpcEnvelope,
    writer: &Arc<Mutex<UnixStream>>,
    pending: &Arc<Mutex<HashMap<String, PendingRequest>>>,
) {
    let callback_message_id = frame.message_id.clone();
    let (owner_id, handler) = match callback_handler_for_frame(&frame, pending) {
        Ok(found) => found,
        Err(message) => {
            let _ = write_callback_error(writer, &callback_message_id, &message);
            return;
        }
    };
    match handler(frame) {
        Ok(reply) => {
            if reply.direction != PpcDirection::Reply {
                fail_pending(
                    pending,
                    &owner_id,
                    "plugin PPC callback response must use reply direction".to_owned(),
                );
                return;
            }
            if reply.message_id != callback_message_id {
                fail_pending(
                    pending,
                    &owner_id,
                    format!(
                        "plugin PPC callback response message_id {} did not match callback {}",
                        reply.message_id, callback_message_id
                    ),
                );
                return;
            }
            if let Err(err) = write_frame_locked(writer, &reply) {
                fail_pending(pending, &owner_id, err.to_string());
            }
        }
        Err(err) => {
            let message = err.to_string();
            let _ = write_callback_error(writer, &callback_message_id, &message);
            fail_pending(pending, &owner_id, message);
        }
    }
}

fn callback_handler_for_frame(
    frame: &PpcEnvelope,
    pending: &Arc<Mutex<HashMap<String, PendingRequest>>>,
) -> Result<(String, CallbackHandler), String> {
    let pending = pending
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

    if let Some((prefix, _)) = frame.message_id.split_once(':') {
        if let Some(pending_request) = pending.get(prefix) {
            if let Some(handler) = &pending_request.callback_handler {
                return Ok((prefix.to_owned(), Arc::clone(handler)));
            }
        }
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

fn callback_parent_message_id(frame: &PpcEnvelope) -> Option<String> {
    let body = serde_json::from_str::<serde_json::Value>(&frame.body).ok()?;
    body.get("parent_message_id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn write_callback_error(
    writer: &Arc<Mutex<UnixStream>>,
    callback_message_id: &str,
    message: &str,
) -> Result<(), DaemonError> {
    let body = json!({
        "success": false,
        "error": {
            "kind": "ppc_callback_error",
            "message": message,
        },
    });
    write_frame_locked(
        writer,
        &PpcEnvelope {
            message_id: callback_message_id.to_owned(),
            direction: PpcDirection::Reply,
            op: "reply".to_owned(),
            body: body.to_string(),
        },
    )
}

fn write_frame_locked(
    writer: &Arc<Mutex<UnixStream>>,
    frame: &PpcEnvelope,
) -> Result<(), DaemonError> {
    let mut writer = writer
        .lock()
        .map_err(|_| DaemonError::StateLockPoisoned("plugin ppc writer"))?;
    writer.write_all(&frame.encode()?)?;
    writer.flush()?;
    Ok(())
}

fn fail_pending(
    pending: &Arc<Mutex<HashMap<String, PendingRequest>>>,
    message_id: &str,
    message: String,
) {
    let pending_request = match pending.lock() {
        Ok(mut pending) => pending.remove(message_id),
        Err(_) => return,
    };
    if let Some(pending_request) = pending_request {
        let _ = pending_request
            .reply_tx
            .send(Err(PluginError::Ppc(message).into()));
    }
}

fn fail_all_pending(pending: &Arc<Mutex<HashMap<String, PendingRequest>>>, message: String) {
    let pending_requests = match pending.lock() {
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

pub(super) fn read_frame(stream: &mut UnixStream) -> Result<Vec<u8>, DaemonError> {
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
        if bytes.len() >= MAX_PPC_FRAME_BYTES {
            return Err(PluginError::Ppc(format!(
                "plugin PPC reply exceeds {MAX_PPC_FRAME_BYTES} byte limit"
            ))
            .into());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;

    type TestResult = std::result::Result<(), Box<dyn std::error::Error + Send + Sync>>;

    #[test]
    fn ppc_client_round_trip_requires_matching_reply() -> TestResult {
        let (client_stream, mut server_stream) = UnixStream::pair()?;
        let server = thread::spawn(move || -> TestResult {
            let request = PpcEnvelope::decode(&read_frame(&mut server_stream)?)?;
            let reply = PpcEnvelope {
                message_id: request.message_id,
                direction: PpcDirection::Reply,
                op: "reply".to_owned(),
                body: r#"{"success":true}"#.to_owned(),
            };
            server_stream.write_all(&reply.encode()?)?;
            Ok(())
        });

        let client = PpcClient::new(client_stream)?;
        let reply = client.round_trip(
            &PpcEnvelope {
                message_id: "msg-1".to_owned(),
                direction: PpcDirection::Request,
                op: "plugin.echo.ping".to_owned(),
                body: r#"{"value":1}"#.to_owned(),
            },
            Duration::from_secs(1),
        )?;

        assert_eq!(reply.message_id, "msg-1");
        assert_eq!(reply.body, r#"{"success":true}"#);
        join_server(server)?;
        Ok(())
    }

    #[test]
    fn ppc_client_rejects_mismatched_message_id() -> TestResult {
        let (client_stream, mut server_stream) = UnixStream::pair()?;
        let server = thread::spawn(move || -> TestResult {
            let _request = PpcEnvelope::decode(&read_frame(&mut server_stream)?)?;
            let reply = PpcEnvelope {
                message_id: "different".to_owned(),
                direction: PpcDirection::Reply,
                op: "reply".to_owned(),
                body: "{}".to_owned(),
            };
            server_stream.write_all(&reply.encode()?)?;
            Ok(())
        });

        let client = PpcClient::new(client_stream)?;
        let Err(err) = client.round_trip(
            &PpcEnvelope {
                message_id: "msg-1".to_owned(),
                direction: PpcDirection::Request,
                op: "plugin.echo.ping".to_owned(),
                body: "{}".to_owned(),
            },
            Duration::from_secs(1),
        ) else {
            return Err("mismatched reply unexpectedly succeeded".into());
        };

        assert!(err
            .to_string()
            .contains("did not match any in-flight request"));
        join_server(server)?;
        Ok(())
    }

    #[test]
    fn ppc_client_matches_out_of_order_replies_by_message_id() -> TestResult {
        let (client_stream, mut server_stream) = UnixStream::pair()?;
        let (first_seen_tx, first_seen_rx) = mpsc::channel();
        let server = thread::spawn(move || -> TestResult {
            let first = PpcEnvelope::decode(&read_frame(&mut server_stream)?)?;
            assert_eq!(first.message_id, "msg-1");
            first_seen_tx.send(())?;
            let second = PpcEnvelope::decode(&read_frame(&mut server_stream)?)?;
            assert_eq!(second.message_id, "msg-2");

            server_stream.write_all(
                &PpcEnvelope {
                    message_id: second.message_id,
                    direction: PpcDirection::Reply,
                    op: "reply".to_owned(),
                    body: r#"{"success":true,"seq":2}"#.to_owned(),
                }
                .encode()?,
            )?;
            server_stream.write_all(
                &PpcEnvelope {
                    message_id: first.message_id,
                    direction: PpcDirection::Reply,
                    op: "reply".to_owned(),
                    body: r#"{"success":true,"seq":1}"#.to_owned(),
                }
                .encode()?,
            )?;
            Ok(())
        });

        let client = Arc::new(PpcClient::new(client_stream)?);
        let first_client = Arc::clone(&client);
        let first = thread::spawn(move || {
            first_client.round_trip(
                &PpcEnvelope {
                    message_id: "msg-1".to_owned(),
                    direction: PpcDirection::Request,
                    op: "plugin.echo.ping".to_owned(),
                    body: r#"{"seq":1}"#.to_owned(),
                },
                Duration::from_secs(1),
            )
        });
        first_seen_rx.recv_timeout(Duration::from_secs(1))?;
        let second_client = Arc::clone(&client);
        let second = thread::spawn(move || {
            second_client.round_trip(
                &PpcEnvelope {
                    message_id: "msg-2".to_owned(),
                    direction: PpcDirection::Request,
                    op: "plugin.echo.ping".to_owned(),
                    body: r#"{"seq":2}"#.to_owned(),
                },
                Duration::from_secs(1),
            )
        });

        let first_reply = first
            .join()
            .unwrap_or_else(|_| Err(std::io::Error::other("first thread panicked").into()))?;
        let second_reply = second
            .join()
            .unwrap_or_else(|_| Err(std::io::Error::other("second thread panicked").into()))?;
        assert_eq!(first_reply.message_id, "msg-1");
        assert_eq!(first_reply.body, r#"{"success":true,"seq":1}"#);
        assert_eq!(second_reply.message_id, "msg-2");
        assert_eq!(second_reply.body, r#"{"success":true,"seq":2}"#);
        join_server(server)?;
        Ok(())
    }

    #[test]
    fn ppc_client_services_callback_before_final_reply() -> TestResult {
        let (client_stream, mut server_stream) = UnixStream::pair()?;
        let server = thread::spawn(move || -> TestResult {
            let request = PpcEnvelope::decode(&read_frame(&mut server_stream)?)?;
            assert_eq!(request.message_id, "msg-1");

            let callback = PpcEnvelope {
                message_id: "callback-1".to_owned(),
                direction: PpcDirection::Request,
                op: "daemon.occ.apply_changeset".to_owned(),
                body: r#"{"changes":[]}"#.to_owned(),
            };
            server_stream.write_all(&callback.encode()?)?;

            let callback_reply = PpcEnvelope::decode(&read_frame(&mut server_stream)?)?;
            assert_eq!(callback_reply.message_id, "callback-1");
            assert_eq!(callback_reply.direction, PpcDirection::Reply);
            assert_eq!(callback_reply.body, r#"{"published":[]}"#);

            let reply = PpcEnvelope {
                message_id: request.message_id,
                direction: PpcDirection::Reply,
                op: "reply".to_owned(),
                body: r#"{"success":true}"#.to_owned(),
            };
            server_stream.write_all(&reply.encode()?)?;
            Ok(())
        });

        let client = PpcClient::new(client_stream)?;
        let reply = client.round_trip_with_callbacks(
            &PpcEnvelope {
                message_id: "msg-1".to_owned(),
                direction: PpcDirection::Request,
                op: "plugin.lsp.apply".to_owned(),
                body: r#"{"path":"main.py"}"#.to_owned(),
            },
            Duration::from_secs(1),
            |callback| {
                assert_eq!(callback.message_id, "callback-1");
                assert_eq!(callback.op, "daemon.occ.apply_changeset");
                Ok(PpcEnvelope {
                    message_id: callback.message_id,
                    direction: PpcDirection::Reply,
                    op: "reply".to_owned(),
                    body: r#"{"published":[]}"#.to_owned(),
                })
            },
        )?;

        assert_eq!(reply.message_id, "msg-1");
        assert_eq!(reply.body, r#"{"success":true}"#);
        join_server(server)?;
        Ok(())
    }

    #[test]
    fn ppc_client_services_multiple_callbacks_before_final_reply() -> TestResult {
        let (client_stream, mut server_stream) = UnixStream::pair()?;
        let server = thread::spawn(move || -> TestResult {
            let request = PpcEnvelope::decode(&read_frame(&mut server_stream)?)?;
            assert_eq!(request.message_id, "msg-1");

            for index in 0..2 {
                let callback = PpcEnvelope {
                    message_id: format!("callback-{index}"),
                    direction: PpcDirection::Request,
                    op: "daemon.occ.apply_changeset".to_owned(),
                    body: format!(r#"{{"changes":[{{"path":"file-{index}.txt"}}]}}"#),
                };
                server_stream.write_all(&callback.encode()?)?;

                let callback_reply = PpcEnvelope::decode(&read_frame(&mut server_stream)?)?;
                assert_eq!(callback_reply.message_id, format!("callback-{index}"));
                assert_eq!(callback_reply.direction, PpcDirection::Reply);
                assert_eq!(
                    callback_reply.body,
                    format!(r#"{{"published":["file-{index}.txt"]}}"#)
                );
            }

            let reply = PpcEnvelope {
                message_id: request.message_id,
                direction: PpcDirection::Reply,
                op: "reply".to_owned(),
                body: r#"{"success":true,"callback_count":2}"#.to_owned(),
            };
            server_stream.write_all(&reply.encode()?)?;
            Ok(())
        });

        let callback_count = Arc::new(AtomicUsize::new(0));
        let callback_counter = Arc::clone(&callback_count);
        let client = PpcClient::new(client_stream)?;
        let reply = client.round_trip_with_callbacks(
            &PpcEnvelope {
                message_id: "msg-1".to_owned(),
                direction: PpcDirection::Request,
                op: "plugin.lsp.apply_multi".to_owned(),
                body: r#"{"paths":["file-0.txt","file-1.txt"]}"#.to_owned(),
            },
            Duration::from_secs(1),
            move |callback| {
                let callback_count = callback_counter.fetch_add(1, Ordering::SeqCst);
                let expected_id = format!("callback-{callback_count}");
                let expected_body =
                    format!(r#"{{"changes":[{{"path":"file-{callback_count}.txt"}}]}}"#);
                assert_eq!(callback.message_id, expected_id);
                assert_eq!(callback.op, "daemon.occ.apply_changeset");
                assert_eq!(callback.body, expected_body);
                let body = format!(r#"{{"published":["file-{callback_count}.txt"]}}"#);
                Ok(PpcEnvelope {
                    message_id: callback.message_id,
                    direction: PpcDirection::Reply,
                    op: "reply".to_owned(),
                    body,
                })
            },
        )?;

        assert_eq!(callback_count.load(Ordering::SeqCst), 2);
        assert_eq!(reply.message_id, "msg-1");
        assert_eq!(reply.body, r#"{"success":true,"callback_count":2}"#);
        join_server(server)?;
        Ok(())
    }

    #[test]
    fn ppc_client_routes_concurrent_callbacks_by_parent_message_id() -> TestResult {
        let (client_stream, mut server_stream) = UnixStream::pair()?;
        let (first_seen_tx, first_seen_rx) = mpsc::channel();
        let server = thread::spawn(move || -> TestResult {
            let first = PpcEnvelope::decode(&read_frame(&mut server_stream)?)?;
            assert_eq!(first.message_id, "op-1");
            first_seen_tx.send(())?;
            let second = PpcEnvelope::decode(&read_frame(&mut server_stream)?)?;
            assert_eq!(second.message_id, "op-2");

            for (message_id, path) in [("op-2", "b.txt"), ("op-1", "a.txt")] {
                let callback_id = format!("{message_id}:occ");
                server_stream.write_all(
                    &PpcEnvelope {
                        message_id: callback_id.clone(),
                        direction: PpcDirection::Request,
                        op: "daemon.occ.apply_changeset".to_owned(),
                        body: format!(
                            r#"{{"parent_message_id":"{message_id}","changes":[{{"path":"{path}"}}]}}"#
                        ),
                    }
                    .encode()?,
                )?;
                let callback_reply = PpcEnvelope::decode(&read_frame(&mut server_stream)?)?;
                assert_eq!(callback_reply.message_id, callback_id);
                assert_eq!(
                    callback_reply.body,
                    format!(r#"{{"published":["{path}"]}}"#)
                );
                server_stream.write_all(
                    &PpcEnvelope {
                        message_id: message_id.to_owned(),
                        direction: PpcDirection::Reply,
                        op: "reply".to_owned(),
                        body: format!(r#"{{"success":true,"path":"{path}"}}"#),
                    }
                    .encode()?,
                )?;
            }
            Ok(())
        });

        let client = Arc::new(PpcClient::new(client_stream)?);
        let first_client = Arc::clone(&client);
        let first = thread::spawn(move || {
            first_client.round_trip_with_callbacks(
                &PpcEnvelope {
                    message_id: "op-1".to_owned(),
                    direction: PpcDirection::Request,
                    op: "plugin.lsp.apply".to_owned(),
                    body: r#"{"path":"a.txt"}"#.to_owned(),
                },
                Duration::from_secs(1),
                |callback| {
                    assert_eq!(callback.message_id, "op-1:occ");
                    Ok(PpcEnvelope {
                        message_id: callback.message_id,
                        direction: PpcDirection::Reply,
                        op: "reply".to_owned(),
                        body: r#"{"published":["a.txt"]}"#.to_owned(),
                    })
                },
            )
        });
        first_seen_rx.recv_timeout(Duration::from_secs(1))?;
        let second_client = Arc::clone(&client);
        let second = thread::spawn(move || {
            second_client.round_trip_with_callbacks(
                &PpcEnvelope {
                    message_id: "op-2".to_owned(),
                    direction: PpcDirection::Request,
                    op: "plugin.lsp.apply".to_owned(),
                    body: r#"{"path":"b.txt"}"#.to_owned(),
                },
                Duration::from_secs(1),
                |callback| {
                    assert_eq!(callback.message_id, "op-2:occ");
                    Ok(PpcEnvelope {
                        message_id: callback.message_id,
                        direction: PpcDirection::Reply,
                        op: "reply".to_owned(),
                        body: r#"{"published":["b.txt"]}"#.to_owned(),
                    })
                },
            )
        });

        let first_reply = first
            .join()
            .unwrap_or_else(|_| Err(std::io::Error::other("first thread panicked").into()))?;
        let second_reply = second
            .join()
            .unwrap_or_else(|_| Err(std::io::Error::other("second thread panicked").into()))?;
        assert_eq!(first_reply.message_id, "op-1");
        assert_eq!(first_reply.body, r#"{"success":true,"path":"a.txt"}"#);
        assert_eq!(second_reply.message_id, "op-2");
        assert_eq!(second_reply.body, r#"{"success":true,"path":"b.txt"}"#);
        join_server(server)?;
        Ok(())
    }

    #[test]
    fn ppc_client_rejects_bad_callback_reply_message_id() -> TestResult {
        let (client_stream, mut server_stream) = UnixStream::pair()?;
        let server = thread::spawn(move || -> TestResult {
            let _request = PpcEnvelope::decode(&read_frame(&mut server_stream)?)?;
            let callback = PpcEnvelope {
                message_id: "callback-1".to_owned(),
                direction: PpcDirection::Request,
                op: "daemon.occ.apply_changeset".to_owned(),
                body: "{}".to_owned(),
            };
            server_stream.write_all(&callback.encode()?)?;
            Ok(())
        });

        let client = PpcClient::new(client_stream)?;
        let Err(err) = client.round_trip_with_callbacks(
            &PpcEnvelope {
                message_id: "msg-1".to_owned(),
                direction: PpcDirection::Request,
                op: "plugin.lsp.apply".to_owned(),
                body: "{}".to_owned(),
            },
            Duration::from_secs(1),
            |_callback| {
                Ok(PpcEnvelope {
                    message_id: "wrong".to_owned(),
                    direction: PpcDirection::Reply,
                    op: "reply".to_owned(),
                    body: "{}".to_owned(),
                })
            },
        ) else {
            return Err("bad callback reply id unexpectedly succeeded".into());
        };

        assert!(err
            .to_string()
            .contains("did not match callback callback-1"));
        join_server(server)?;
        Ok(())
    }

    fn join_server(server: thread::JoinHandle<TestResult>) -> TestResult {
        server
            .join()
            .unwrap_or_else(|_| Err(std::io::Error::other("server thread panicked").into()))
    }
}
