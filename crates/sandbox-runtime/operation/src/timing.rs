use std::fs::OpenOptions;
use std::io::Write;
use std::time::Instant;

pub(crate) fn duration(label: &'static str, started: Instant) {
    let Some(mut file) = timing_file() else {
        return;
    };
    let _ = writeln!(
        file,
        "{}",
        serde_json::json!({
            "component": "sandbox-runtime",
            "pid": std::process::id(),
            "checkpoint": label,
            "duration_ms": started.elapsed().as_secs_f64() * 1000.0,
        })
    );
}

fn timing_file() -> Option<std::fs::File> {
    let path = std::env::var_os("EOS_RUNTIME_TIMING_LOG")?;
    OpenOptions::new().create(true).append(true).open(path).ok()
}
