use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{AllocationId, CanonicalRootPair, PocError, PocResult, SCHEMA_VERSION};

pub const MAX_RECENT_DELTAS: usize = 8;

/// A kernel-facing recipe contains one immutable base, an optional carrier
/// containing only net historical deltas, and at most eight recent adopted
/// delta allocations. It never describes a reconstructed copy of the base.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProjectionRecipe {
    pub schema_version: u32,
    pub roots: CanonicalRootPair,
    pub base_allocation_id: AllocationId,
    pub net_delta_carrier_id: Option<AllocationId>,
    /// Newest delta first, matching OverlayFS lowerdir precedence.
    pub recent_delta_ids: Vec<AllocationId>,
}

impl ProjectionRecipe {
    pub fn validate(&self) -> PocResult<()> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(PocError::Integrity(format!(
                "projection schema {} differs from {}",
                self.schema_version, SCHEMA_VERSION
            )));
        }
        if self.recent_delta_ids.len() > MAX_RECENT_DELTAS {
            return Err(PocError::Overloaded(format!(
                "projection has {} recent deltas; maximum is {MAX_RECENT_DELTAS}",
                self.recent_delta_ids.len()
            )));
        }
        let mut unique = BTreeSet::new();
        for allocation_id in self.lower_allocation_ids_newest_first() {
            if !unique.insert(allocation_id) {
                return Err(PocError::Integrity(format!(
                    "projection repeats allocation {allocation_id}"
                )));
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn lower_allocation_ids_newest_first(&self) -> Vec<&AllocationId> {
        let mut allocation_ids = Vec::with_capacity(
            self.recent_delta_ids.len() + usize::from(self.net_delta_carrier_id.is_some()) + 1,
        );
        allocation_ids.extend(self.recent_delta_ids.iter());
        if let Some(carrier) = &self.net_delta_carrier_id {
            allocation_ids.push(carrier);
        }
        allocation_ids.push(&self.base_allocation_id);
        allocation_ids
    }

    #[must_use]
    pub fn kernel_lower_count(&self) -> usize {
        self.recent_delta_ids.len() + usize::from(self.net_delta_carrier_id.is_some()) + 1
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExactProjectionReceipt {
    pub schema_version: u32,
    pub roots: CanonicalRootPair,
    pub lower_allocation_ids_newest_first: Vec<AllocationId>,
    pub kernel_lower_count: u16,
    pub reconstructed_payload_bytes: u64,
    pub hydrated_payload_bytes: u64,
    pub base_bytes_copied: u64,
    pub projection_built_during_activation: bool,
}

pub fn select_exact(recipe: &ProjectionRecipe) -> PocResult<ExactProjectionReceipt> {
    recipe.validate()?;
    let kernel_lower_count = u16::try_from(recipe.kernel_lower_count())
        .map_err(|_| PocError::Integrity("kernel lower count does not fit u16".to_owned()))?;
    Ok(ExactProjectionReceipt {
        schema_version: SCHEMA_VERSION,
        roots: recipe.roots.clone(),
        lower_allocation_ids_newest_first: recipe
            .lower_allocation_ids_newest_first()
            .into_iter()
            .cloned()
            .collect(),
        kernel_lower_count,
        reconstructed_payload_bytes: 0,
        hydrated_payload_bytes: 0,
        base_bytes_copied: 0,
        projection_built_during_activation: false,
    })
}
