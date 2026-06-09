//! Model-registry persistence contracts.

use async_trait::async_trait;

use crate::{CoreError, JsonObject, ModelRegistration};

use super::Sealed;

/// Persistence surface for [`ModelRegistration`].
#[async_trait]
pub trait ModelStore: Sealed + Send + Sync {
    /// Create or update a registration.
    async fn register(
        &self,
        model_key: &str,
        label: &str,
        class_path: &str,
        kwargs: &JsonObject,
        activate: bool,
    ) -> Result<ModelRegistration, CoreError>;

    /// Delete by key; `Ok(false)` means no such key.
    async fn delete(&self, model_key: &str) -> Result<bool, CoreError>;

    /// Load a registration by key.
    async fn get(&self, model_key: &str) -> Result<Option<ModelRegistration>, CoreError>;

    /// The single active registration, if any.
    async fn active(&self) -> Result<Option<ModelRegistration>, CoreError>;
}
