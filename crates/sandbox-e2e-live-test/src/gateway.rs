use std::os::unix::net::UnixStream;
use std::path::Path;
use std::thread::sleep;
use std::time::{Duration, Instant};

/// Default readiness timeout when no orchestrator override is supplied (the
/// test-side `Harness` uses this). The orchestrator passes its resolved
/// `RunConfig.gateway_ready_timeout` instead.
pub const DEFAULT_READY_TIMEOUT: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Poll until the gateway socket exists and accepts a connection, or `timeout`
/// elapses. Attach mode only — never spawns a gateway. Returns `Err` naming the
/// socket if it never becomes ready.
pub fn await_ready(socket: &Path, timeout: Duration) -> anyhow::Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        if socket.exists() && UnixStream::connect(socket).is_ok() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            anyhow::bail!(
                "gateway socket {} did not become ready within {timeout:?}",
                socket.display()
            );
        }
        sleep(POLL_INTERVAL);
    }
}
