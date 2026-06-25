#![forbid(unsafe_code)]

#[allow(
    dead_code,
    reason = "test harness path-includes private CLI modules and exercises selected helpers"
)]
#[path = "../src/cgroup_setup.rs"]
pub(crate) mod cgroup_setup;
#[path = "../src/observability/mod.rs"]
pub(crate) mod observability;
#[allow(
    dead_code,
    reason = "test harness path-includes private CLI modules and exercises selected helpers"
)]
#[path = "../src/runner/mod.rs"]
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
pub(crate) use server::MAX_REQUEST_BYTES;
#[allow(
    dead_code,
    reason = "test harness path-includes private modules that depend on crate-level timing helpers"
)]
#[path = "../src/timing.rs"]
pub(crate) mod timing;

#[path = "unit/dependency_guard.rs"]
mod dependency_guard_tests;

mod connection_tests {
    pub(crate) use crate::server::connection::read_request_line_with_timeout;
    pub(crate) use crate::server::lifecycle::drain_connection_tasks;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/unit/connection.rs"
    ));
}

mod dispatch_tests {
    pub(crate) use crate::server::dispatch::{
        decode_request, sandbox_daemon_ready_response, strip_tcp_auth, validate_daemon_scope,
    };
    pub(crate) use crate::server::SandboxDaemonError;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/unit/dispatch.rs"
    ));
}

mod observability_tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/unit/observability.rs"
    ));
}

mod cgroup_tests {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/unit/cgroup.rs"));
}

mod cgroup_setup_tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/unit/cgroup_setup.rs"
    ));
}

mod runner_tests {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/unit/runner.rs"));
}

mod serve_tests {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/unit/serve.rs"));
}
