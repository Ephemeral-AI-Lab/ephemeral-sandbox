use std::fs::OpenOptions;
use std::io::Write;
use std::sync::OnceLock;
use std::time::Instant;

static STARTED: OnceLock<Instant> = OnceLock::new();

pub(crate) fn checkpoint(label: &'static str) {
    if !enabled() {
        return;
    }
    let elapsed_ms = STARTED.get_or_init(Instant::now).elapsed().as_secs_f64() * 1000.0;
    write_line(serde_json::json!({
        "component": component(),
        "pid": std::process::id(),
        "checkpoint": label,
        "elapsed_ms": elapsed_ms,
    }));
}

pub(crate) fn duration(label: &'static str, started: Instant) {
    if !enabled() {
        return;
    }
    write_line(serde_json::json!({
        "component": component(),
        "pid": std::process::id(),
        "checkpoint": label,
        "duration_ms": started.elapsed().as_secs_f64() * 1000.0,
    }));
}

fn component() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "sandbox-gateway".to_owned())
}

fn enabled() -> bool {
    std::env::var_os("EOS_CLI_TIMING_LOG").is_some() || std::env::var_os("EOS_CLI_TIMING").is_some()
}

fn write_line(value: serde_json::Value) {
    let line = value.to_string();
    if let Some(path) = std::env::var_os("EOS_CLI_TIMING_LOG") {
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(file, "{line}");
        }
    } else {
        eprintln!("{line}");
    }
}
