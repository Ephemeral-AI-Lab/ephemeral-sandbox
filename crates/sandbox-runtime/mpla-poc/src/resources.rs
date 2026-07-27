use std::sync::{Arc, Mutex, MutexGuard};

use serde::{Deserialize, Serialize};

use crate::config::{
    MAX_COORDINATORS, MAX_DATA_WORKERS, MAX_PENDING_DESCRIPTORS, MAX_PENDING_DESCRIPTOR_BYTES,
    NONRESIDENT_CREDIT_BYTES, RESIDENT_POOL_BYTES,
};
use crate::{PocError, PocResult, SCHEMA_VERSION};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AdmissionTier {
    ActiveData,
    Coordinator,
    PendingDescriptor,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AdmissionReceipt {
    pub schema_version: u32,
    pub job_ordinal: u32,
    pub tier: AdmissionTier,
    pub descriptor_bytes: u64,
    pub owns_payload_allocation: bool,
    pub owns_workspace_mount: bool,
    pub owns_staging_allocation: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ResourceSnapshot {
    pub submitted_jobs: u32,
    pub active_data_workers: u16,
    pub coordinators: u16,
    pub pending_descriptors: u16,
    pub pending_descriptor_bytes: u64,
    pub resident_pool_bytes: u64,
    pub nonresident_credit_bytes: u64,
    pub private_allocations: u16,
    pub active_mounts: u16,
    pub staging_allocations: u16,
}

#[derive(Debug, Default)]
struct State {
    snapshot: ResourceSnapshot,
}

#[derive(Clone, Debug, Default)]
pub struct AdmissionController {
    state: Arc<Mutex<State>>,
}

#[derive(Debug)]
pub struct AdmissionGuard {
    state: Arc<Mutex<State>>,
    receipt: AdmissionReceipt,
}

impl AdmissionController {
    #[must_use]
    pub fn new() -> Self {
        let snapshot = ResourceSnapshot {
            resident_pool_bytes: RESIDENT_POOL_BYTES,
            nonresident_credit_bytes: NONRESIDENT_CREDIT_BYTES,
            ..ResourceSnapshot::default()
        };
        Self {
            state: Arc::new(Mutex::new(State { snapshot })),
        }
    }

    pub fn submit(&self, descriptor_bytes: u64) -> PocResult<AdmissionGuard> {
        let mut state = lock_state(&self.state)?;
        let job_ordinal = state
            .snapshot
            .submitted_jobs
            .checked_add(1)
            .ok_or_else(|| PocError::Overloaded("job ordinal overflow".to_owned()))?;
        if job_ordinal > 32 {
            return Err(PocError::Overloaded(format!(
                "job {job_ordinal} rejected before resource ownership; capacity is 32"
            )));
        }

        let (tier, owns_physical) = if state.snapshot.active_data_workers < MAX_DATA_WORKERS {
            state.snapshot.active_data_workers += 1;
            state.snapshot.coordinators += 1;
            state.snapshot.private_allocations += 1;
            state.snapshot.active_mounts += 1;
            (AdmissionTier::ActiveData, true)
        } else if state.snapshot.coordinators < MAX_COORDINATORS {
            state.snapshot.coordinators += 1;
            (AdmissionTier::Coordinator, false)
        } else {
            if descriptor_bytes > MAX_PENDING_DESCRIPTOR_BYTES {
                return Err(PocError::Overloaded(format!(
                    "descriptor is {descriptor_bytes} bytes; aggregate cap is {MAX_PENDING_DESCRIPTOR_BYTES}"
                )));
            }
            if state.snapshot.pending_descriptors >= MAX_PENDING_DESCRIPTORS
                || state
                    .snapshot
                    .pending_descriptor_bytes
                    .checked_add(descriptor_bytes)
                    .is_none_or(|bytes| bytes > MAX_PENDING_DESCRIPTOR_BYTES)
            {
                return Err(PocError::Overloaded(
                    "pending descriptor capacity exhausted before resource ownership".to_owned(),
                ));
            }
            state.snapshot.pending_descriptors += 1;
            state.snapshot.pending_descriptor_bytes += descriptor_bytes;
            (AdmissionTier::PendingDescriptor, false)
        };
        state.snapshot.submitted_jobs = job_ordinal;
        let receipt = AdmissionReceipt {
            schema_version: SCHEMA_VERSION,
            job_ordinal,
            tier,
            descriptor_bytes,
            owns_payload_allocation: owns_physical,
            owns_workspace_mount: owns_physical,
            owns_staging_allocation: false,
        };
        Ok(AdmissionGuard {
            state: Arc::clone(&self.state),
            receipt,
        })
    }

    pub fn snapshot(&self) -> PocResult<ResourceSnapshot> {
        Ok(lock_state(&self.state)?.snapshot.clone())
    }
}

impl AdmissionGuard {
    #[must_use]
    pub const fn receipt(&self) -> &AdmissionReceipt {
        &self.receipt
    }
}

impl Drop for AdmissionGuard {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.lock() {
            match self.receipt.tier {
                AdmissionTier::ActiveData => {
                    state.snapshot.active_data_workers -= 1;
                    state.snapshot.coordinators -= 1;
                    state.snapshot.private_allocations -= 1;
                    state.snapshot.active_mounts -= 1;
                }
                AdmissionTier::Coordinator => state.snapshot.coordinators -= 1,
                AdmissionTier::PendingDescriptor => {
                    state.snapshot.pending_descriptors -= 1;
                    state.snapshot.pending_descriptor_bytes -= self.receipt.descriptor_bytes;
                }
            }
        }
    }
}

fn lock_state(state: &Mutex<State>) -> PocResult<MutexGuard<'_, State>> {
    state
        .lock()
        .map_err(|_| PocError::Integrity("resource admission lock poisoned".to_owned()))
}
