#![forbid(unsafe_code)]

#[allow(
    dead_code,
    reason = "test harness path-includes private CLI modules and exercises selected helpers"
)]
#[path = "../src/runner.rs"]
mod runner_cli;
#[allow(
    dead_code,
    reason = "test harness path-includes private CLI modules and exercises selected helpers"
)]
#[path = "../src/serve.rs"]
mod serve_cli;
#[allow(
    dead_code,
    unused_imports,
    reason = "test harness path-includes server modules and exercises selected private helpers"
)]
#[path = "../src/server/mod.rs"]
pub(crate) mod server;
#[allow(
    dead_code,
    reason = "test harness path-includes daemon telemetry setup and test subscriber helper"
)]
#[path = "../src/telemetry.rs"]
mod telemetry;

pub(crate) use server::MAX_REQUEST_BYTES;

#[path = "unit/dependency_guard.rs"]
mod dependency_guard_tests;

mod connection_tests {
    pub(crate) use crate::server::connection::read_request_line_with_timeout;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/unit/connection.rs"
    ));
}

mod dispatch_tests {
    pub(crate) use crate::server::dispatch::{decode_request, validate_daemon_scope};
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/unit/dispatch.rs"
    ));
}

mod runner_tests {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/unit/runner.rs"));
}

mod serve_tests {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/unit/serve.rs"));
}

mod telemetry_tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/unit/telemetry.rs"
    ));
}
