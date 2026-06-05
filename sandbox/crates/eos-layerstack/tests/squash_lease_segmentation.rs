//! The squash "n leased + gaps" formula at the planning boundary.
//!
//! Squash segments the active manifest AROUND the leased layers: each leased
//! layer is kept as a barrier (frozen for that lease's reads), and every maximal
//! run of unleased layers folds into one checkpoint. So the post-squash layer
//! count == (number of lease heads) + (number of unleased gap runs). This is the
//! deterministic home for the formula — over the wire only one lease per caller
//! is reachable, so the multi-lease arithmetic lives here.

use std::path::PathBuf;

use eos_layerstack::{LayerCheckpointSquasher, LayerRef, SquashPlanEntry};
use eos_protocol::{Manifest, MANIFEST_SCHEMA_VERSION};

type TestResult<T = ()> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

fn layer(id: &str) -> LayerRef {
    LayerRef {
        layer_id: id.to_owned(),
        path: format!("layers/{id}"),
    }
}

fn kept_ids(entries: &[SquashPlanEntry]) -> Vec<&str> {
    entries
        .iter()
        .filter_map(|entry| match entry {
            SquashPlanEntry::Keep(layer) => Some(layer.layer_id.as_str()),
            SquashPlanEntry::Segment(_) => None,
        })
        .collect()
}

#[test]
fn squash_keeps_each_lease_head_and_folds_every_gap() -> TestResult {
    // Nine layers L0..L8; two non-adjacent leases pin L3 and L6, leaving three
    // unleased runs: [L0,L1,L2], [L4,L5], [L7,L8].
    let layers: Vec<LayerRef> = (0..9).map(|index| layer(&format!("L{index}"))).collect();
    let manifest = Manifest::new(9, layers.clone(), MANIFEST_SCHEMA_VERSION)?;
    let lease_heads = vec![layers[3].clone(), layers[6].clone()];

    // plan() is pure segmentation — it never touches the storage root.
    let squasher = LayerCheckpointSquasher::new(PathBuf::from("/squash-plan-only"));
    let plan = squasher
        .plan(&manifest, 5, &lease_heads, 1)?
        .expect("a 9-layer manifest over depth 5 must yield a squash plan");

    // result layers == n_leased (2) + n_gap_runs (3).
    assert_eq!(plan.entries.len(), 5, "result == n_leased + n_gaps");
    assert_eq!(
        kept_ids(&plan.entries),
        ["L3", "L6"],
        "each leased head is preserved as a barrier, in order"
    );
    let folded: Vec<Vec<&str>> = plan
        .checkpoint_segments()
        .iter()
        .map(|segment| {
            segment
                .layers
                .iter()
                .map(|layer| layer.layer_id.as_str())
                .collect()
        })
        .collect();
    assert_eq!(
        folded,
        vec![
            vec!["L0", "L1", "L2"],
            vec!["L4", "L5"],
            vec!["L7", "L8"],
        ],
        "exactly the three unleased gap runs fold into checkpoints"
    );
    Ok(())
}

#[test]
fn squash_without_leases_folds_to_a_single_checkpoint() -> TestResult {
    let layers: Vec<LayerRef> = (0..9).map(|index| layer(&format!("L{index}"))).collect();
    let manifest = Manifest::new(9, layers, MANIFEST_SCHEMA_VERSION)?;

    let squasher = LayerCheckpointSquasher::new(PathBuf::from("/squash-plan-only"));
    let plan = squasher
        .plan(&manifest, 5, &[], 1)?
        .expect("an unleased manifest over depth must yield a plan");

    // Zero lease heads + one unleased run == one resulting checkpoint layer.
    assert_eq!(plan.entries.len(), 1);
    assert!(kept_ids(&plan.entries).is_empty());
    assert_eq!(plan.checkpoint_segments().len(), 1);
    Ok(())
}

#[test]
fn squash_keeps_an_adjacent_lease_pair_without_folding_between_them() -> TestResult {
    // Adjacent leases (L4, L5) have NO gap between them: both are kept, and only
    // the outer runs fold. result == 2 leased + 2 gaps.
    let layers: Vec<LayerRef> = (0..9).map(|index| layer(&format!("L{index}"))).collect();
    let manifest = Manifest::new(9, layers.clone(), MANIFEST_SCHEMA_VERSION)?;
    let lease_heads = vec![layers[4].clone(), layers[5].clone()];

    let squasher = LayerCheckpointSquasher::new(PathBuf::from("/squash-plan-only"));
    let plan = squasher
        .plan(&manifest, 5, &lease_heads, 1)?
        .expect("plan");

    assert_eq!(plan.entries.len(), 4, "2 adjacent leases + 2 outer gaps");
    assert_eq!(kept_ids(&plan.entries), ["L4", "L5"]);
    assert_eq!(plan.checkpoint_segments().len(), 2);
    Ok(())
}
