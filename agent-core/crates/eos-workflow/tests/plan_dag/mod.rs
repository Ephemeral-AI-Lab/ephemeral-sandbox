//! Plan-DAG materialization + PLAN->RUN stage advance, asserted at a **non-closure
//! park** (`TESTING_SPEC` §5.2 `plan_dag` rows; the Layer-B half of AC6).
//!
//! Layer B: the whole agent run is mocked as a unit by the `QueueRunner` gate
//! (no engine loop, LLM, tools, or sandbox). We push only the planner's plan,
//! then park at the first generator's `run()` — the runner records the launch,
//! then blocks awaiting a submission that never comes — and inspect the
//! materialized RUN-stage rows while the workflow is still `Open` (taxonomy #3,
//! park-and-inspect; never reaches `Closed`).

#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use eos_agent_def::AgentRole;
use eos_state::{AttemptBudget, AttemptStage, PlanNodeId, TaskStatus, WorkflowStatus};

use crate::ids::generator_task_id;
use crate::support::{
    one_step_plan, root_task, wait_until, MemoryStores, QueueRunner, ScriptedSubmission,
};
use crate::WorkflowStarter;

fn node(id: &str) -> PlanNodeId {
    PlanNodeId::new(id).unwrap()
}

#[tokio::test]
async fn plan_dag_materializes_and_parks_at_run_without_closing() {
    let stores = Arc::new(MemoryStores::default());
    let runner = Arc::new(QueueRunner::default());
    let mut deps = stores.deps(runner.clone());
    deps.lifecycle_config.default_attempt_budget = AttemptBudget::try_from_u32(1).unwrap();
    runner.bind(&deps.orchestrator_registry);
    let parent = root_task("parent", TaskStatus::Running);
    stores.seed_task(parent.clone());
    let started = WorkflowStarter::new(deps)
        .start("delegated goal", &parent.id)
        .await
        .unwrap();

    // Record the plan (drives PLAN->RUN), but never push a generator submission:
    // the runner records the generator launch, then blocks (records-then-blocks),
    // so the loop parks here.
    runner.push(ScriptedSubmission::Planner(one_step_plan(&started)));
    wait_until(|| {
        runner
            .launches()
            .iter()
            .any(|launch| launch.role() == AgentRole::Generator)
    })
    .await;

    // RUN-stage rows, observed at the park — the DAG materialized and the attempt
    // advanced PLAN->RUN.
    let attempt = stores.attempt(&started.attempt_id).unwrap();
    assert_eq!(
        attempt.stage(),
        AttemptStage::Run,
        "the attempt advanced PLAN->RUN once the plan was recorded"
    );
    let plan = attempt.materialized_plan().expect("plan materialized into the attempt");
    let generator_id = generator_task_id(&started.attempt_id, &node("g1")).unwrap();
    assert_eq!(
        plan.generator_task_ids,
        vec![generator_id.clone()],
        "the generator task was materialized at RUN"
    );
    assert_eq!(plan.reducer_task_ids.len(), 1, "one reducer materialized");
    assert!(
        stores.task(&generator_id).is_some(),
        "the generator task row was materialized at RUN"
    );

    // It is a PARK, not a closure: the reducer has not launched (it needs g1) and
    // the workflow is still Open — no terminal status was reached (I5/AC6).
    assert!(
        runner
            .launches()
            .iter()
            .all(|launch| launch.role() != AgentRole::Reducer),
        "the reducer must not launch while the generator is parked"
    );
    assert_eq!(
        stores.workflow(&started.workflow_id).unwrap().status,
        WorkflowStatus::Open,
        "the workflow parks at RUN without reaching a terminal status"
    );
    // Dropped here without pushing a generator submission: park-and-inspect, no
    // closure (the spawned attempt task is cancelled when the test runtime ends).
}
