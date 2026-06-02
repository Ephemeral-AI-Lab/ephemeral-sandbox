//! Engine background policy, dispatch, and supervisor.

mod dispatch;
mod policy;
mod supervisor;

pub use dispatch::launch_background_tool;
pub use policy::{is_engine_background_tool, needs_background_manager};
pub use supervisor::{
    BackgroundTaskKind, BackgroundTaskRecord, BackgroundTaskStatus, BackgroundTaskSupervisor,
    SharedSubagentSupervisor, StopMode,
};
