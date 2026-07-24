use std::collections::HashMap;

use crate::error::LayerStackError;
use crate::model::manifest_root_hash;
use crate::service::{LayerStatus, StackObservation};
use crate::stack::lease::lock_shared_registry;
use crate::LayerStack;

impl LayerStack {
    /// Per-layer lease breakdown of the active manifest, newest -> base.
    ///
    /// Computed in one pass over the live leases: each layer's
    /// `leased_by_workspaces` is the number of leases whose newest layer is that
    /// layer. The booked-by relation is left to the caller: it is a pure
    /// function of the returned layer order plus the per-layer counts.
    pub fn observe(&self) -> Result<StackObservation, LayerStackError> {
        let _guard = self.writer_lock.shared()?;
        let manifest = self.read_active_manifest_unlocked()?;
        let (active_lease_count, newest_layers) = {
            let leases = lock_shared_registry(&self.leases)?;
            (leases.active_count(), leases.lease_newest_layers())
        };
        let (route, resources) = self.observation.observe(active_lease_count);
        let mut leased_counts: HashMap<&str, usize> = HashMap::new();
        for layer in &newest_layers {
            *leased_counts.entry(layer.layer_id.as_str()).or_insert(0) += 1;
        }
        let layers = manifest
            .layers
            .iter()
            .map(|layer| LayerStatus {
                leased_by_workspaces: leased_counts
                    .get(layer.layer_id.as_str())
                    .copied()
                    .unwrap_or(0),
                layer: layer.clone(),
            })
            .collect();
        Ok(StackObservation {
            manifest_version: manifest.version,
            root_hash: manifest_root_hash(&manifest),
            active_lease_count,
            route,
            resources,
            layers,
        })
    }
}
