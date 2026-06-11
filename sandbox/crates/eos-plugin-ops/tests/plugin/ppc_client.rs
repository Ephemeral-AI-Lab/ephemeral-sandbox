use std::io::Write;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;

use eos_plugin::{PpcDirection, PpcMessage};
use eos_plugin_ops::{read_message_bytes, PpcClient};

type TestResult = std::result::Result<(), Box<dyn std::error::Error + Send + Sync>>;

#[test]
fn ppc_client_round_trip_requires_matching_reply() -> TestResult {
    let (client_stream, mut server_stream) = UnixStream::pair()?;
    let server = thread::spawn(move || -> TestResult {
        let request = PpcMessage::decode(&read_message_bytes(&mut server_stream)?)?;
        let reply = PpcMessage {
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
        &PpcMessage {
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
fn ppc_client_drops_stray_reply_without_failing_in_flight_request() -> TestResult {
    let (client_stream, mut server_stream) = UnixStream::pair()?;
    let server = thread::spawn(move || -> TestResult {
        let request = PpcMessage::decode(&read_message_bytes(&mut server_stream)?)?;
        assert_eq!(request.message_id, "msg-1");
        // A reply for an id that is not in flight must be dropped, not used to
        // cascade-fail the healthy in-flight request on the same client.
        let stray = PpcMessage {
            message_id: "ghost".to_owned(),
            direction: PpcDirection::Reply,
            op: "reply".to_owned(),
            body: "{}".to_owned(),
        };
        server_stream.write_all(&stray.encode()?)?;
        let reply = PpcMessage {
            message_id: "msg-1".to_owned(),
            direction: PpcDirection::Reply,
            op: "reply".to_owned(),
            body: r#"{"ok":true}"#.to_owned(),
        };
        server_stream.write_all(&reply.encode()?)?;
        Ok(())
    });

    let client = PpcClient::new(client_stream)?;
    let reply = client.round_trip(
        &PpcMessage {
            message_id: "msg-1".to_owned(),
            direction: PpcDirection::Request,
            op: "plugin.echo.ping".to_owned(),
            body: "{}".to_owned(),
        },
        Duration::from_secs(5),
    )?;

    assert_eq!(reply.message_id, "msg-1");
    assert_eq!(reply.body, r#"{"ok":true}"#);
    join_server(server)?;
    Ok(())
}

#[test]
fn ppc_client_matches_out_of_order_replies_by_message_id() -> TestResult {
    let (client_stream, mut server_stream) = UnixStream::pair()?;
    let (first_seen_tx, first_seen_rx) = mpsc::channel();
    let server = thread::spawn(move || -> TestResult {
        let first = PpcMessage::decode(&read_message_bytes(&mut server_stream)?)?;
        assert_eq!(first.message_id, "msg-1");
        first_seen_tx.send(())?;
        let second = PpcMessage::decode(&read_message_bytes(&mut server_stream)?)?;
        assert_eq!(second.message_id, "msg-2");

        server_stream.write_all(
            &PpcMessage {
                message_id: second.message_id,
                direction: PpcDirection::Reply,
                op: "reply".to_owned(),
                body: r#"{"success":true,"seq":2}"#.to_owned(),
            }
            .encode()?,
        )?;
        server_stream.write_all(
            &PpcMessage {
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
            &PpcMessage {
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
            &PpcMessage {
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
        let request = PpcMessage::decode(&read_message_bytes(&mut server_stream)?)?;
        assert_eq!(request.message_id, "msg-1");

        let callback = PpcMessage {
            message_id: "callback-1".to_owned(),
            direction: PpcDirection::Request,
            op: "daemon.occ.apply_changeset".to_owned(),
            body: r#"{"changes":[]}"#.to_owned(),
        };
        server_stream.write_all(&callback.encode()?)?;

        let callback_reply = PpcMessage::decode(&read_message_bytes(&mut server_stream)?)?;
        assert_eq!(callback_reply.message_id, "callback-1");
        assert_eq!(callback_reply.direction, PpcDirection::Reply);
        assert_eq!(callback_reply.body, r#"{"published":[]}"#);

        let reply = PpcMessage {
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
        &PpcMessage {
            message_id: "msg-1".to_owned(),
            direction: PpcDirection::Request,
            op: "plugin.generic.apply".to_owned(),
            body: r#"{"path":"main.py"}"#.to_owned(),
        },
        Duration::from_secs(1),
        |callback| {
            assert_eq!(callback.message_id, "callback-1");
            assert_eq!(callback.op, "daemon.occ.apply_changeset");
            Ok(PpcMessage {
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
        let request = PpcMessage::decode(&read_message_bytes(&mut server_stream)?)?;
        assert_eq!(request.message_id, "msg-1");

        for index in 0..2 {
            let callback = PpcMessage {
                message_id: format!("callback-{index}"),
                direction: PpcDirection::Request,
                op: "daemon.occ.apply_changeset".to_owned(),
                body: format!(r#"{{"changes":[{{"path":"file-{index}.txt"}}]}}"#),
            };
            server_stream.write_all(&callback.encode()?)?;

            let callback_reply = PpcMessage::decode(&read_message_bytes(&mut server_stream)?)?;
            assert_eq!(callback_reply.message_id, format!("callback-{index}"));
            assert_eq!(callback_reply.direction, PpcDirection::Reply);
            assert_eq!(
                callback_reply.body,
                format!(r#"{{"published":["file-{index}.txt"]}}"#)
            );
        }

        let reply = PpcMessage {
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
        &PpcMessage {
            message_id: "msg-1".to_owned(),
            direction: PpcDirection::Request,
            op: "plugin.generic.apply_multi".to_owned(),
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
            Ok(PpcMessage {
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
        let first = PpcMessage::decode(&read_message_bytes(&mut server_stream)?)?;
        assert_eq!(first.message_id, "op-1");
        first_seen_tx.send(())?;
        let second = PpcMessage::decode(&read_message_bytes(&mut server_stream)?)?;
        assert_eq!(second.message_id, "op-2");

        for (message_id, path) in [("op-2", "b.txt"), ("op-1", "a.txt")] {
            let callback_id = format!("{message_id}:occ");
            server_stream.write_all(
                &PpcMessage {
                    message_id: callback_id.clone(),
                    direction: PpcDirection::Request,
                    op: "daemon.occ.apply_changeset".to_owned(),
                    body: format!(
                        r#"{{"parent_message_id":"{message_id}","changes":[{{"path":"{path}"}}]}}"#
                    ),
                }
                .encode()?,
            )?;
            let callback_reply = PpcMessage::decode(&read_message_bytes(&mut server_stream)?)?;
            assert_eq!(callback_reply.message_id, callback_id);
            assert_eq!(
                callback_reply.body,
                format!(r#"{{"published":["{path}"]}}"#)
            );
            server_stream.write_all(
                &PpcMessage {
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
            &PpcMessage {
                message_id: "op-1".to_owned(),
                direction: PpcDirection::Request,
                op: "plugin.generic.apply".to_owned(),
                body: r#"{"path":"a.txt"}"#.to_owned(),
            },
            Duration::from_secs(1),
            |callback| {
                assert_eq!(callback.message_id, "op-1:occ");
                Ok(PpcMessage {
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
            &PpcMessage {
                message_id: "op-2".to_owned(),
                direction: PpcDirection::Request,
                op: "plugin.generic.apply".to_owned(),
                body: r#"{"path":"b.txt"}"#.to_owned(),
            },
            Duration::from_secs(1),
            |callback| {
                assert_eq!(callback.message_id, "op-2:occ");
                Ok(PpcMessage {
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
        let _request = PpcMessage::decode(&read_message_bytes(&mut server_stream)?)?;
        let callback = PpcMessage {
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
        &PpcMessage {
            message_id: "msg-1".to_owned(),
            direction: PpcDirection::Request,
            op: "plugin.generic.apply".to_owned(),
            body: "{}".to_owned(),
        },
        Duration::from_secs(1),
        |_callback| {
            Ok(PpcMessage {
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
