use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{PocError, PocResult};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FaultPoint {
    BeforeSealing,
    AfterSealingDurable,
    AfterProcessDrain,
    AfterStrictUnmount,
    AfterSyncfs,
    AfterFirstInventory,
    AfterStableAllocation,
    AfterOwnerIntent,
    AfterOwnerAdoption,
}

/// Deterministic one-shot fault injector. It never sleeps or allocates payload
/// data, so fault evidence remains scoped to the requested protocol edge.
#[derive(Clone, Debug, Default)]
pub struct FaultInjector {
    armed: BTreeSet<FaultPoint>,
    fired: BTreeSet<FaultPoint>,
}

impl FaultInjector {
    #[must_use]
    pub fn armed(points: impl IntoIterator<Item = FaultPoint>) -> Self {
        Self {
            armed: points.into_iter().collect(),
            fired: BTreeSet::new(),
        }
    }

    pub fn hit(&mut self, point: FaultPoint, post_sealing: bool) -> PocResult<()> {
        if !self.armed.contains(&point) || !self.fired.insert(point) {
            return Ok(());
        }
        let detail = format!("deterministic fault fired at {point:?}");
        if post_sealing {
            Err(PocError::RecoveryRequired(detail))
        } else {
            Err(PocError::Integrity(detail))
        }
    }

    #[must_use]
    pub fn fired(&self, point: FaultPoint) -> bool {
        self.fired.contains(&point)
    }
}
