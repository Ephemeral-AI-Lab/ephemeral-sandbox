#![forbid(unsafe_code)]

mod allocator_backend {
    include!(env!("SANDBOX_DAEMON_ALLOCATOR_METRICS"));
}

#[allow(
    dead_code,
    reason = "test harness path-includes private CLI modules and exercises selected helpers"
)]
#[path = "../src/cgroup_setup.rs"]
pub(crate) mod cgroup_setup;
#[allow(
    dead_code,
    reason = "rpc lifecycle references crate::http; the harness includes it to resolve that path"
)]
#[path = "../src/http/mod.rs"]
pub(crate) mod http;
#[path = "../src/observability/mod.rs"]
pub(crate) mod observability;
#[allow(
    dead_code,
    unused_imports,
    reason = "test harness path-includes rpc modules and exercises selected private helpers"
)]
#[path = "../src/rpc/mod.rs"]
pub(crate) mod rpc;
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

#[path = "unit/dependency_guard.rs"]
mod dependency_guard_tests;

mod connection_tests {
    pub(crate) use crate::rpc::connection::read_request_line_with_limits;
    pub(crate) use crate::rpc::lifecycle::{admit_rpc_connection, drain_connection_tasks};
    pub(crate) use crate::rpc::ConnectionAdmission;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/unit/connection.rs"
    ));
}

#[allow(
    clippy::unwrap_used,
    reason = "test fixtures and negative assertions intentionally fail immediately"
)]
mod dispatch_tests {
    pub(crate) use crate::rpc::dispatch::{
        blocking_overload_response, daemon_readiness_response, decode_request,
        server_shutting_down_response, strip_tcp_auth, validate_daemon_scope,
    };
    pub(crate) use crate::rpc::SandboxDaemonError;
    pub(crate) use crate::rpc::{AdmissionError, BlockingAdmission};
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

#[allow(
    dead_code,
    reason = "path-included production module exposes a constructor not called by private-field tests"
)]
mod resource_sampler_tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/observability/resources.rs"
    ));

    mod tests {
        use super::*;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/unit/resources.rs"
        ));
    }
}

mod http_tests {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/unit/http.rs"));
}

#[allow(
    clippy::unwrap_used,
    reason = "test fixture setup intentionally fails immediately on malformed state"
)]
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
