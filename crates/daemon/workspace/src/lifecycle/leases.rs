use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub(crate) fn next_handle_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed) & 0x00ff_ffff;
    format!("{counter:06x}{nanos:016x}")
}

pub(crate) fn monotonic_seconds() -> f64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}
