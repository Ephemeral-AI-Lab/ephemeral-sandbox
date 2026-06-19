use std::collections::BTreeMap;
use std::time::Instant;

use serde_json::json;
use trace::usize_to_f64_saturating;

use crate::commit::OccTraceEvent;
use crate::LayerStack;

pub(crate) struct AutoSquashTrace {
    pub(crate) timings: BTreeMap<String, f64>,
    pub(crate) events: Vec<OccTraceEvent>,
}

pub(crate) fn run_auto_squash(stack: &mut LayerStack, max_depth: usize) -> AutoSquashTrace {
    let mut timings = BTreeMap::new();
    let max_depth = max_depth.max(1);
    let (depth_before, decision) = match stack.squash_plan_decision(max_depth, 2) {
        Ok(decision) => decision,
        Err(err) => {
            return AutoSquashTrace {
                timings,
                events: vec![auto_squash_event(
                    "auto_squash_skipped",
                    json!({
                        "reason": "plan_failed",
                        "error": err.to_string(),
                        "max_depth": max_depth,
                    }),
                )],
            };
        }
    };
    if let Some(reason) = decision.skip_reason {
        return AutoSquashTrace {
            timings,
            events: vec![auto_squash_event(
                "auto_squash_skipped",
                json!({
                    "reason": reason.as_str(),
                    "max_depth": max_depth,
                    "depth_before": depth_before,
                }),
            )],
        };
    }

    let mut events = vec![auto_squash_event(
        "auto_squash_started",
        json!({
            "max_depth": max_depth,
            "depth_before": depth_before,
        }),
    )];
    let squash_start = Instant::now();
    let squashed = stack.squash(max_depth);
    let squash_elapsed_s = squash_start.elapsed().as_secs_f64();
    timings.insert(
        "layer_stack.auto_squash.total_s".to_owned(),
        squash_elapsed_s,
    );
    timings.insert(
        "layer_stack.auto_squash.max_depth".to_owned(),
        usize_to_f64_saturating(max_depth),
    );
    timings.insert(
        "layer_stack.auto_squash.depth_before".to_owned(),
        usize_to_f64_saturating(depth_before),
    );
    match squashed {
        Ok(outcome) => {
            let Some(manifest) = outcome.manifest else {
                timings.insert("layer_stack.auto_squash.raced".to_owned(), 1.0);
                events.push(auto_squash_event(
                    "auto_squash_skipped",
                    json!({
                        "reason": "live_prefix_race",
                        "max_depth": max_depth,
                        "depth_before": depth_before,
                        "duration_s": squash_elapsed_s,
                    }),
                ));
                return AutoSquashTrace { timings, events };
            };
            timings.insert(
                "layer_stack.auto_squash.depth_after".to_owned(),
                usize_to_f64_saturating(manifest.depth()),
            );
            timings.insert(
                "layer_stack.auto_squash.manifest_version".to_owned(),
                i64_to_f64_saturating(manifest.version),
            );
            events.push(auto_squash_event(
                "auto_squash_finished",
                json!({
                    "success": true,
                    "max_depth": max_depth,
                    "depth_before": depth_before,
                    "depth_after": manifest.depth(),
                    "manifest_version": manifest.version,
                    "duration_s": squash_elapsed_s,
                    "lease_release_error": outcome
                        .lease_release_error
                        .as_ref()
                        .map(ToString::to_string),
                }),
            ));
            if let Some(release_error) = outcome.lease_release_error {
                events.push(auto_squash_event(
                    "lease_release_failed",
                    json!({
                        "lease_owner": "auto_squash",
                        "reason": "post_commit_release_failed",
                        "error": release_error.to_string(),
                        "manifest_version": manifest.version,
                    }),
                ));
            }
            AutoSquashTrace { timings, events }
        }
        Err(err) => {
            events.push(auto_squash_event(
                "auto_squash_finished",
                json!({
                    "success": false,
                    "error": err.to_string(),
                    "max_depth": max_depth,
                    "depth_before": depth_before,
                    "duration_s": squash_elapsed_s,
                }),
            ));
            AutoSquashTrace { timings, events }
        }
    }
}

fn auto_squash_event(name: &'static str, details: serde_json::Value) -> OccTraceEvent {
    OccTraceEvent::new("layer_stack", name, details)
}

fn i64_to_f64_saturating(value: i64) -> f64 {
    u64::try_from(value).map_or(0.0, |value| {
        u32::try_from(value).map_or_else(|_| f64::from(u32::MAX), f64::from)
    })
}
