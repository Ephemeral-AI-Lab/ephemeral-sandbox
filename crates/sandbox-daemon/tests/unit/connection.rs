use tokio::io::AsyncReadExt as _;
use tokio_util::task::TaskTracker;

use crate::rpc::SandboxDaemonError;
use sandbox_protocol::ProtocolLimits;
use serde_json::json;

fn limits(
    max_request_bytes: usize,
    request_read_timeout_s: f64,
    response_write_timeout_s: f64,
) -> ProtocolLimits {
    ProtocolLimits {
        max_request_bytes,
        request_read_timeout_s,
        response_write_timeout_s,
    }
}

#[tokio::test]
async fn read_request_line_rejects_oversized_payloads() {
    // A lowered injected cap rejects an envelope one byte past it.
    let max_request_bytes = 64 * 1024;
    let mut reader = tokio::io::repeat(b'x').take(
        u64::try_from(max_request_bytes)
            .expect("max request bytes fits u64")
            .saturating_add(1),
    );
    let err = read_request_line_with_limits(&mut reader, limits(max_request_bytes, 0.5, 0.5))
        .await
        .expect_err("oversized request rejected");
    assert!(
        matches!(err, SandboxDaemonError::RequestTooLarge { limit } if limit == max_request_bytes)
    );
}

#[tokio::test]
async fn read_request_line_accepts_within_lowered_cap() {
    // The same lowered cap still accepts a request that fits.
    let (mut writer, mut reader) = tokio::io::duplex(256);
    tokio::io::AsyncWriteExt::write_all(&mut writer, b"{\"op\":\"ping\"}\n")
        .await
        .expect("write request line");
    let line = read_request_line_with_limits(&mut reader, limits(64 * 1024, 0.5, 0.5))
        .await
        .expect("request within cap accepted");
    assert!(line.ends_with(b"\n"));
}

#[tokio::test]
async fn read_request_line_times_out_waiting_for_line() {
    let (_writer, mut reader) = tokio::io::duplex(64);
    let err = read_request_line_with_limits(
        &mut reader,
        limits(ProtocolLimits::DEFAULT_MAX_REQUEST_BYTES, 0.1, 0.1),
    )
    .await
    .expect_err("hanging request times out");
    assert!(
        matches!(err, SandboxDaemonError::Io(ref source) if source.kind() == std::io::ErrorKind::TimedOut),
        "{err:?}"
    );
}

#[tokio::test]
async fn response_write_times_out_when_the_peer_stops_reading() {
    let (_reader, mut writer) = tokio::io::duplex(1);
    let err = write_response_with_limits(&mut writer, b"response", limits(64 * 1024, 0.5, 0.05))
        .await
        .expect_err("a stalled peer must not pin the daemon response task");
    assert!(
        matches!(err, SandboxDaemonError::Io(ref source) if source.kind() == std::io::ErrorKind::TimedOut),
        "{err:?}"
    );
}

#[tokio::test]
async fn drain_connection_tasks_waits_for_tracked_tasks() {
    let tracker = TaskTracker::new();
    let completed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let completed_task = std::sync::Arc::clone(&completed);
    tracker.spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        completed_task.store(true, std::sync::atomic::Ordering::SeqCst);
    });

    drain_connection_tasks(&tracker).await;

    assert!(completed.load(std::sync::atomic::Ordering::SeqCst));
    assert!(tracker.is_closed());
    assert!(tracker.is_empty());
}

#[tokio::test]
async fn rpc_connection_limit_rejects_before_spawn_and_recovers_after_release() {
    let admission = ConnectionAdmission::new(1);
    let held = admission
        .try_acquire()
        .expect("the sole configured connection permit is available");
    assert_eq!(admission.in_use(), 1);

    let (mut client, server) = tokio::io::duplex(4 * 1024);
    let rejected = admit_rpc_connection(server, &admission, 1).await;
    assert!(
        rejected.is_none(),
        "an overloaded connection must not return a stream that can be spawned"
    );
    assert_eq!(
        admission.in_use(),
        1,
        "rejection must not exceed or consume the configured bound"
    );

    let mut framed = Vec::new();
    client
        .read_to_end(&mut framed)
        .await
        .expect("read structured overload response");
    assert_eq!(framed.pop(), Some(b'\n'));
    let response: serde_json::Value =
        serde_json::from_slice(&framed).expect("overload response is JSON");
    assert_eq!(response["error"]["kind"], "server_busy");
    assert_eq!(
        response["error"]["message"],
        "daemon is at connection capacity"
    );
    assert_eq!(
        response["error"]["details"],
        json!({ "fields": { "max_concurrent_connections": 1 } })
    );

    drop(held);
    assert_eq!(admission.in_use(), 0);
    let (_client, server) = tokio::io::duplex(64);
    let (_stream, permit) = admit_rpc_connection(server, &admission, 1)
        .await
        .expect("a released connection permit admits the next stream");
    assert_eq!(admission.in_use(), 1);
    drop(permit);
    assert_eq!(admission.in_use(), 0);
}

#[tokio::test]
async fn closed_rpc_admission_returns_shutdown_instead_of_overload() {
    let admission = ConnectionAdmission::new(1);
    admission.close();
    let (mut client, server) = tokio::io::duplex(4 * 1024);

    assert!(admit_rpc_connection(server, &admission, 1).await.is_none());

    let mut framed = Vec::new();
    client
        .read_to_end(&mut framed)
        .await
        .expect("read structured shutdown response");
    assert_eq!(framed.pop(), Some(b'\n'));
    let response: serde_json::Value = serde_json::from_slice(&framed).expect("shutdown JSON");
    assert_eq!(response["error"]["kind"], "server_shutting_down");
    assert_ne!(response["error"]["kind"], "server_busy");
}
