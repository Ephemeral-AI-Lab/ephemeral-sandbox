use serde::{Deserialize, Serialize};

use crate::{PocError, PocResult};

pub const REQUIRED_DOCKER_CPUS: u16 = 4;
pub const REQUIRED_DOCKER_MEMORY_BYTES: u64 = 4 * 1024 * 1024 * 1024;
pub const RESIDENT_POOL_BYTES: u64 = 8 * 1024 * 1024;
pub const NONRESIDENT_CREDIT_BYTES: u64 = 64 * 1024 * 1024;
pub const MEMORY_HIGH_BYTES: u64 = 96 * 1024 * 1024;
pub const MEMORY_MAX_BYTES: u64 = 128 * 1024 * 1024;
pub const MAX_DATA_WORKERS: u16 = 4;
pub const MAX_COORDINATORS: u16 = 16;
pub const MAX_PENDING_DESCRIPTORS: u16 = 16;
pub const MAX_PENDING_DESCRIPTOR_BYTES: u64 = 64 * 1024;
pub const REQUIRED_IMAGE_DIGEST: &str =
    "sha256:4fbb8e6a8395de5a7550b33509421a2bafbc0aab6c06ba2cef9ebffbc7092d90";
pub const REQUIRED_IMAGE_PLATFORM: &str = "linux/arm64";
pub const REQUIRED_IMAGE_RELEASE: &str = "Ubuntu 24.04";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PocConfig {
    pub docker_cpus: u16,
    pub docker_memory_bytes: u64,
    pub resident_pool_bytes: u64,
    pub nonresident_credit_bytes: u64,
    pub memory_high_bytes: u64,
    pub memory_max_bytes: u64,
    pub max_data_workers: u16,
    pub max_coordinators: u16,
    pub max_pending_descriptors: u16,
    pub max_pending_descriptor_bytes: u64,
    pub required_image_digest: String,
    pub required_image_platform: String,
    pub required_image_release: String,
}

impl Default for PocConfig {
    fn default() -> Self {
        Self {
            docker_cpus: REQUIRED_DOCKER_CPUS,
            docker_memory_bytes: REQUIRED_DOCKER_MEMORY_BYTES,
            resident_pool_bytes: RESIDENT_POOL_BYTES,
            nonresident_credit_bytes: NONRESIDENT_CREDIT_BYTES,
            memory_high_bytes: MEMORY_HIGH_BYTES,
            memory_max_bytes: MEMORY_MAX_BYTES,
            max_data_workers: MAX_DATA_WORKERS,
            max_coordinators: MAX_COORDINATORS,
            max_pending_descriptors: MAX_PENDING_DESCRIPTORS,
            max_pending_descriptor_bytes: MAX_PENDING_DESCRIPTOR_BYTES,
            required_image_digest: REQUIRED_IMAGE_DIGEST.to_owned(),
            required_image_platform: REQUIRED_IMAGE_PLATFORM.to_owned(),
            required_image_release: REQUIRED_IMAGE_RELEASE.to_owned(),
        }
    }
}

impl PocConfig {
    pub fn validate(&self) -> PocResult<()> {
        if self != &Self::default() {
            return Err(PocError::InvalidConfig(
                "the Stage 04.6 PoC resource and image envelope is fixed".to_owned(),
            ));
        }
        Ok(())
    }
}
