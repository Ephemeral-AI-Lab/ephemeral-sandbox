//! Engine-local background session accounting for one agent run.

mod factory;
mod notification;
mod session_managers;
mod session_runtime;

pub use factory::BackgroundSessionFactory;
pub use notification::{BackgroundCompletion, BackgroundNotificationEmitter};
pub use session_managers::BackgroundSessionStatus;
pub(crate) use session_runtime::BackgroundRunFinalizer;
pub use session_runtime::BackgroundSessionService;
