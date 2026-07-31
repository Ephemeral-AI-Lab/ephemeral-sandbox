use serde_json::Value;

use super::mpla_speed_scorecard::{self, LifecyclePhase};

type ScorecardResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

pub fn run(run_id: &str, candidate_sandbox_id: &str, build_commit: &str) -> ScorecardResult<Value> {
    mpla_speed_scorecard::run(
        LifecyclePhase::Fork,
        run_id,
        candidate_sandbox_id,
        build_commit,
    )
}
