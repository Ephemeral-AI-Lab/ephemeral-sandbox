//! Pure on-disk collectors. Each reads bytes from a storage path and returns a
//! plain struct; none depend on runtime implementation crates. The daemon calls
//! them and packs the results into `Sample.metrics`.

pub mod cgroup;
pub mod disk;
mod layerstack;
pub mod process_topology;

pub use layerstack::{sample_layerstack, LayerBytes, LayerStackBytes};

/// Node/depth budget for one sampler walk. The daemon injects it from
/// `observability.sampling` — one budget governs both walkers (spec
/// decision 8) — and `Default` preserves the shipped policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WalkBudget {
    pub max_nodes: usize,
    pub max_depth: usize,
}

impl Default for WalkBudget {
    fn default() -> Self {
        Self {
            max_nodes: 1024,
            max_depth: 64,
        }
    }
}
