use super::*;
use crate::model::{LayerRef, Manifest, MANIFEST_SCHEMA_VERSION};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

fn layer(id: &str) -> LayerRef {
    LayerRef {
        layer_id: id.to_owned(),
        path: format!("layers/{id}"),
    }
}

fn manifest(ids: &[&str]) -> Result<Manifest, crate::model::CasError> {
    Manifest::new(
        i64::try_from(ids.len()).unwrap_or(i64::MAX),
        ids.iter().map(|id| layer(id)).collect(),
        MANIFEST_SCHEMA_VERSION,
    )
}

fn interval_ids(plan: &ReclaimUnpinnedLayersPlan) -> Vec<Vec<&str>> {
    plan.entries
        .iter()
        .filter_map(|entry| {
            if let ReclaimUnpinnedLayersPlanEntry::ReclaimingInterval(interval) = entry {
                Some(
                    interval
                        .layers
                        .iter()
                        .map(|layer| layer.layer_id.as_str())
                        .collect(),
                )
            } else {
                None
            }
        })
        .collect()
}

fn protected_ids(plan: &ReclaimUnpinnedLayersPlan) -> Vec<&str> {
    plan.entries
        .iter()
        .filter_map(|entry| match entry {
            ReclaimUnpinnedLayersPlanEntry::KeepProtected(layer) => Some(layer.layer_id.as_str()),
            ReclaimUnpinnedLayersPlanEntry::KeepUnleased(_)
            | ReclaimUnpinnedLayersPlanEntry::ReclaimingInterval(_) => None,
        })
        .collect()
}

fn checkpoint_modes(plan: &ReclaimUnpinnedLayersPlan) -> Vec<ReclaimUnpinnedLayersCheckpointMode> {
    plan.entries
        .iter()
        .filter_map(|entry| {
            if let ReclaimUnpinnedLayersPlanEntry::ReclaimingInterval(interval) = entry {
                Some(interval.checkpoint_mode)
            } else {
                None
            }
        })
        .collect()
}

#[test]
fn fully_leased_stack_has_no_reclaiming_intervals() -> TestResult {
    let ids = (1..=50)
        .rev()
        .map(|index| format!("P{index}"))
        .collect::<Vec<_>>();
    let id_refs = ids.iter().map(String::as_str).collect::<Vec<_>>();
    let manifest = manifest(&id_refs)?;
    let protected = manifest.layers.clone();

    let plan = plan_reclaim_unpinned_layers(&manifest, &protected, 2)?;

    assert_eq!(plan.active_layer_count, 50);
    assert_eq!(plan.protected_layer_count, 50);
    assert_eq!(plan.reclaiming_interval_count, 0);
    assert_eq!(plan.reclaiming_layer_count, 0);
    assert_eq!(plan.entries.len(), 50);
    assert_eq!(protected_ids(&plan).len(), 50);
    Ok(())
}

#[test]
fn unleased_prefix_compacts_above_protected_suffix() -> TestResult {
    let mut ids = (1..=10)
        .rev()
        .map(|index| format!("N{index}"))
        .collect::<Vec<_>>();
    ids.extend((1..=50).rev().map(|index| format!("P{index}")));
    let id_refs = ids.iter().map(String::as_str).collect::<Vec<_>>();
    let manifest = manifest(&id_refs)?;
    let protected = (1..=50)
        .rev()
        .map(|index| layer(&format!("P{index}")))
        .collect::<Vec<_>>();

    let plan = plan_reclaim_unpinned_layers(&manifest, &protected, 2)?;

    assert_eq!(plan.protected_layer_count, 50);
    assert_eq!(plan.kept_unleased_layer_count, 0);
    assert_eq!(plan.reclaiming_interval_count, 1);
    assert_eq!(plan.reclaiming_layer_count, 10);
    assert_eq!(
        interval_ids(&plan),
        vec![vec![
            "N10", "N9", "N8", "N7", "N6", "N5", "N4", "N3", "N2", "N1"
        ]]
    );
    assert_eq!(
        checkpoint_modes(&plan),
        [ReclaimUnpinnedLayersCheckpointMode::DeltaRequired]
    );
    assert_eq!(plan.entries.len(), 51);
    Ok(())
}

#[test]
fn same_file_gap_plans_around_single_protected_layer() -> TestResult {
    let manifest = manifest(&["n6", "n5", "l4", "n3", "n2", "n1"])?;
    let protected = vec![layer("l4")];

    let plan = plan_reclaim_unpinned_layers(&manifest, &protected, 2)?;

    assert_eq!(plan.protected_layer_count, 1);
    assert_eq!(plan.kept_unleased_layer_count, 0);
    assert_eq!(plan.reclaiming_interval_count, 2);
    assert_eq!(plan.reclaiming_layer_count, 5);
    assert_eq!(
        interval_ids(&plan),
        vec![vec!["n6", "n5"], vec!["n3", "n2", "n1"]]
    );
    assert_eq!(
        checkpoint_modes(&plan),
        [
            ReclaimUnpinnedLayersCheckpointMode::DeltaRequired,
            ReclaimUnpinnedLayersCheckpointMode::View
        ]
    );
    assert_eq!(protected_ids(&plan), ["l4"]);
    assert_eq!(plan.entries.len(), 3);
    Ok(())
}

#[test]
fn mounted_l4_lease_keeps_lower_prefix_until_normalized_or_released() -> TestResult {
    let manifest = manifest(&["n6", "n5", "l4", "n3", "n2", "n1"])?;
    let protected = ["l4", "n3", "n2", "n1"]
        .iter()
        .map(|id| layer(id))
        .collect::<Vec<_>>();

    let plan = plan_reclaim_unpinned_layers(&manifest, &protected, 2)?;

    assert_eq!(plan.protected_layer_count, 4);
    assert_eq!(plan.reclaiming_interval_count, 1);
    assert_eq!(plan.reclaiming_layer_count, 2);
    assert_eq!(interval_ids(&plan), vec![vec!["n6", "n5"]]);
    assert_eq!(protected_ids(&plan), ["l4", "n3", "n2", "n1"]);
    assert_eq!(plan.entries.len(), 5);
    Ok(())
}

#[test]
fn mounted_l4_lease_after_parent_normalization_keeps_compact_parent() -> TestResult {
    let manifest = manifest(&["n6", "n5", "l4", "c_n3_n1"])?;
    let protected = ["l4", "c_n3_n1"]
        .iter()
        .map(|id| layer(id))
        .collect::<Vec<_>>();

    let plan = plan_reclaim_unpinned_layers(&manifest, &protected, 2)?;

    assert_eq!(plan.protected_layer_count, 2);
    assert_eq!(plan.reclaiming_interval_count, 1);
    assert_eq!(plan.reclaiming_layer_count, 2);
    assert_eq!(interval_ids(&plan), vec![vec!["n6", "n5"]]);
    assert_eq!(protected_ids(&plan), ["l4", "c_n3_n1"]);
    assert_eq!(plan.entries.len(), 3);
    Ok(())
}

#[test]
fn alternating_single_unleased_layers_are_kept_by_minimum_interval() -> TestResult {
    let manifest = manifest(&["n6", "p5", "n4", "p3", "n2", "p1"])?;
    let protected = ["p5", "p3", "p1"]
        .iter()
        .map(|id| layer(id))
        .collect::<Vec<_>>();

    let plan = plan_reclaim_unpinned_layers(&manifest, &protected, 2)?;

    assert_eq!(plan.protected_layer_count, 3);
    assert_eq!(plan.kept_unleased_layer_count, 3);
    assert_eq!(plan.reclaiming_interval_count, 0);
    assert_eq!(plan.entries.len(), 6);
    Ok(())
}
