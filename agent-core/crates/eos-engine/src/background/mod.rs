//! Engine-local background session accounting for one agent run.

mod notification;
mod background_session_manager;

pub use background_session_manager::BackgroundSessionStatus;
pub(crate) use background_session_manager::BackgroundSessionFinalizer;
pub use background_session_manager::{BackgroundSessionService, BackgroundTeardownPort};
pub use notification::{BackgroundCompletion, BackgroundNotificationEmitter};
