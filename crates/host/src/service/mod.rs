pub(crate) mod forward;
pub(crate) mod registry;

mod args;
mod container_ops;
mod docker_json;
mod forwarding;
mod host_events;
mod image_ops;
mod lifecycle;
mod response;
mod trace_ops;
mod types;
mod utils;

pub use forward::ForwardError;
pub use types::{ForwardTraceContext, HostConfig, HostForwardRequest, SandboxHost, SandboxStatus};

pub(crate) use args::workspace_root_from_args;
pub(crate) use types::ManagedSandboxStart;

const TRACE_SHOW_DEFAULT_SECTION_LIMIT: usize = 1_000;
const TRACE_SHOW_MAX_SECTION_LIMIT: usize = 5_000;
const SANDBOX_SCRATCH_TMPFS: &str = "/eos/scratch:rw,exec,size=2g,mode=1777";
const SANDBOX_OVERLAY_ROOT: &str = "/eos/scratch/overlay";
const DEFAULT_WORKSPACE_ROOT: &str = "/testbed";
const HOST_SANDBOX_ACQUIRE: &str = "host.sandbox.acquire";
const HOST_SANDBOX_RELEASE: &str = "host.sandbox.release";
const HOST_IMAGE_PROFILES_LIST: &str = "host.image_profiles.list";
const HOST_IMAGE_LIST: &str = "host.image.list";
const HOST_IMAGE_PULL: &str = "host.image.pull";
const HOST_CONTAINER_LIST: &str = "host.container.list";
const HOST_CONTAINER_START: &str = "host.container.start";
const HOST_CONTAINER_ADOPT: &str = "host.container.adopt";
const HOST_CONTAINER_STOP: &str = "host.container.stop";
const HOST_CONTAINER_REMOVE: &str = "host.container.remove";
const HOST_TRACE_REQUESTS: &str = "host.trace.requests";
const HOST_TRACE_SHOW: &str = "host.trace.show";
const HOST_TRACE_VERIFY: &str = "host.trace.verify";
