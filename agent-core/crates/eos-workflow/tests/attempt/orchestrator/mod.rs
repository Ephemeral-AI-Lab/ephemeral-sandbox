#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use eos_state::{
    AttemptBudget, AttemptStatus, GeneratorSubmission, IterationStatus, PlanNodeId,
    ReducerSubmission, TaskOutcomeStatus, TaskStatus, WorkflowStatus,
};

use crate::ids::{generator_task_id, reducer_task_id};
use crate::support::{
    one_step_plan, root_task, terminal_result, wait_for_workflow_status, MemoryStores, QueueRunner,
    ScriptedSubmission,
};
use crate::WorkflowStarter;

fn budget(value: u32) -> AttemptBudget {
    AttemptBudget::try_from_u32(value).unwrap()
}

fn node(id: &str) -> PlanNodeId {
    PlanNodeId::new(id).unwrap()
}

// AC-eos-workflow-06 (reducer exit gate): all generators DONE + all reducers
// DONE -> attempt PASSED, iteration SUCCEEDED, workflow SUCCEEDED, parent
// still running.
#[tokio::test]
async fn reducer_is_exit_gate() {
    let stores = Arc::new(MemoryStores::default());
    let runner = Arc::new(QueueRunner::default());
    let mut deps = stores.deps(runner.clone());
    deps.lifecycle_config.default_attempt_budget = budget(1);
    runner.bind(&deps.orchestrator_registry);
    let parent = root_task("parent", TaskStatus::Running);
    stores.seed_task(parent.clone());
    let started = WorkflowStarter::new(deps)
        .start("delegated goal", &parent.id)
        .await
        .unwrap();
    let generator_id = generator_task_id(&started.attempt_id, &node("g1")).unwrap();
    let reducer_id = reducer_task_id(&started.attempt_id, &node("r1")).unwrap();
    runner.push(ScriptedSubmission::Planner(one_step_plan(&started)));
    runner.push(ScriptedSubmission::Generator(GeneratorSubmission {
        attempt_id: started.attempt_id.clone(),
        task_id: generator_id,
        status: TaskOutcomeStatus::Success,
        outcome: "generated".to_owned(),
        terminal_tool_result: terminal_result(),
    }));
    runner.push(ScriptedSubmission::Reducer(ReducerSubmission {
        attempt_id: started.attempt_id.clone(),
        task_id: reducer_id,
        status: TaskOutcomeStatus::Success,
        outcome: "reduced".to_owned(),
        terminal_tool_result: terminal_result(),
    }));
    wait_for_workflow_status(&stores, &started.workflow_id, WorkflowStatus::Succeeded).await;

    assert_eq!(
        stores.attempt(&started.attempt_id).unwrap().status(),
        AttemptStatus::Passed
    );
    assert_eq!(
        stores.iteration(&started.iteration_id).unwrap().status,
        IterationStatus::Succeeded
    );
    assert_eq!(
        stores.workflow(&started.workflow_id).unwrap().status,
        WorkflowStatus::Succeeded
    );
    assert_eq!(stores.task(&parent.id).unwrap().status, TaskStatus::Running);
    assert_eq!(runner.launches().len(), 3);
}

// AC-eos-workflow-06 (reducer exit gate): a FAILED reducer closes the attempt
// FAILED with TASK_FAILED; with no budget the iteration + workflow fail.
#[tokio::test]
async fn failed_reducer_closes_attempt_failed() {
    let stores = Arc::new(MemoryStores::default());
    let runner = Arc::new(QueueRunner::default());
    let mut deps = stores.deps(runner.clone());
    deps.lifecycle_config.default_attempt_budget = budget(1);
    runner.bind(&deps.orchestrator_registry);
    let parent = root_task("parent", TaskStatus::Running);
    stores.seed_task(parent.clone());
    let started = WorkflowStarter::new(deps)
        .start("delegated goal", &parent.id)
        .await
        .unwrap();
    let generator_id = generator_task_id(&started.attempt_id, &node("g1")).unwrap();
    let reducer_id = reducer_task_id(&started.attempt_id, &node("r1")).unwrap();
    runner.push(ScriptedSubmission::Planner(one_step_plan(&started)));
    runner.push(ScriptedSubmission::Generator(GeneratorSubmission {
        attempt_id: started.attempt_id.clone(),
        task_id: generator_id,
        status: TaskOutcomeStatus::Success,
        outcome: "generated".to_owned(),
        terminal_tool_result: terminal_result(),
    }));
    runner.push(ScriptedSubmission::Reducer(ReducerSubmission {
        attempt_id: started.attempt_id.clone(),
        task_id: reducer_id,
        status: TaskOutcomeStatus::Failed,
        outcome: "reduction failed".to_owned(),
        terminal_tool_result: terminal_result(),
    }));
    wait_for_workflow_status(&stores, &started.workflow_id, WorkflowStatus::Failed).await;

    let attempt = stores.attempt(&started.attempt_id).unwrap();
    assert_eq!(attempt.status(), AttemptStatus::Failed);
    assert_eq!(
        attempt.fail_reason(),
        Some(eos_state::AttemptFailReason::TaskFailed)
    );
    assert_eq!(
        stores.iteration(&started.iteration_id).unwrap().status,
        IterationStatus::Failed
    );
}

// D6 (generator role gate) + D1 (reducer-dup id) + the recording parity win:
// the recording port returns the orchestrator's real `Rejected` ack to the
// agent (a model-facing validation error), not a silent accept.
#[tokio::test]
async fn record_plan_rejects_bad_shape_with_real_ack() {
    use eos_state::{PlanDisposition, PlanNodeId};
    use eos_tools::{AttemptSubmissionPort, PlanReducer, PlanTask, PlannerPlan, SubmissionAck};

    use crate::AttemptSubmissionAdapter;

    fn node(id: &str) -> PlanNodeId {
        PlanNodeId::new(id).unwrap()
    }

    let stores = Arc::new(MemoryStores::default());
    let runner = Arc::new(QueueRunner::default());
    let deps = stores.deps(runner);
    let registry = deps.orchestrator_registry.clone();
    let parent = root_task("parent", TaskStatus::Running);
    stores.seed_task(parent.clone());
    let started = WorkflowStarter::new(deps)
        .start("delegated goal", &parent.id)
        .await
        .unwrap();
    let adapter = AttemptSubmissionAdapter::new(registry);
    let planner_task_id = crate::planner_task_id(&started.attempt_id).unwrap();
    let plan = |tasks: Vec<PlanTask>, reducers: Vec<PlanReducer>| PlannerPlan {
        attempt_id: started.attempt_id.clone(),
        planner_task_id: planner_task_id.clone(),
        disposition: PlanDisposition::Complete,
        tasks,
        task_specs: [(node("g1"), "do work".to_owned())].into_iter().collect(),
        reducers,
    };

    // D6: a generator bound to a non-generator profile ("reducer") is rejected.
    let ack = adapter
        .apply_plan(plan(
            vec![PlanTask {
                id: node("g1"),
                agent_name: "reducer".to_owned(),
                needs: Vec::new(),
            }],
            vec![PlanReducer {
                id: node("r1"),
                needs: vec![node("g1")],
                prompt: "reduce".to_owned(),
            }],
        ))
        .await
        .unwrap();
    assert!(
        matches!(ack, SubmissionAck::Rejected(ref m) if m.contains("expected generator")),
        "D6 role gate: {ack:?}"
    );

    // D1: a duplicate reducer id slips past the tool's generator-only dup
    // check but is rejected by the orchestrator's union-dedup.
    let ack = adapter
        .apply_plan(plan(
            vec![PlanTask {
                id: node("g1"),
                agent_name: "coder".to_owned(),
                needs: Vec::new(),
            }],
            vec![
                PlanReducer {
                    id: node("r1"),
                    needs: vec![node("g1")],
                    prompt: "a".to_owned(),
                },
                PlanReducer {
                    id: node("r1"),
                    needs: vec![node("g1")],
                    prompt: "b".to_owned(),
                },
            ],
        ))
        .await
        .unwrap();
    assert!(
        matches!(ack, SubmissionAck::Rejected(ref m) if m.contains("duplicate task id")),
        "D1 union-dedup: {ack:?}"
    );

    // The attempt is untouched by either rejection (still in PLAN, no plan
    // tasks materialized).
    let attempt = stores.attempt(&started.attempt_id).unwrap();
    assert_eq!(attempt.stage(), eos_state::AttemptStage::Plan);
    assert!(attempt.generator_task_ids().is_empty());
}

// The validation hoist (`validate_plan_agents` runs before any upsert): a plan
// whose FIRST generator is valid but a LATER one is bound to an unregistered
// agent is rejected with NO orphan Pending rows persisted. The prior
// interleaved materialize loop would have written the valid `g1` row before
// `g2`'s registry check failed.
#[tokio::test]
async fn record_plan_rejects_late_agent_without_orphan_rows() {
    use eos_state::{PlanDisposition, PlanNodeId};
    use eos_tools::{AttemptSubmissionPort, PlanReducer, PlanTask, PlannerPlan, SubmissionAck};

    use crate::AttemptSubmissionAdapter;

    fn node(id: &str) -> PlanNodeId {
        PlanNodeId::new(id).unwrap()
    }

    let stores = Arc::new(MemoryStores::default());
    let runner = Arc::new(QueueRunner::default());
    let deps = stores.deps(runner);
    let registry = deps.orchestrator_registry.clone();
    let parent = root_task("parent", TaskStatus::Running);
    stores.seed_task(parent.clone());
    let started = WorkflowStarter::new(deps)
        .start("delegated goal", &parent.id)
        .await
        .unwrap();
    let adapter = AttemptSubmissionAdapter::new(registry);

    // g1 -> registered generator "coder"; g2 -> unregistered "ghost". Both
    // feed the reducer, so the DAG shape is valid and validation reaches the
    // per-agent pass (where g2 is rejected).
    let ack = adapter
        .apply_plan(PlannerPlan {
            attempt_id: started.attempt_id.clone(),
            planner_task_id: crate::planner_task_id(&started.attempt_id).unwrap(),
            disposition: PlanDisposition::Complete,
            tasks: vec![
                PlanTask {
                    id: node("g1"),
                    agent_name: "coder".to_owned(),
                    needs: Vec::new(),
                },
                PlanTask {
                    id: node("g2"),
                    agent_name: "ghost".to_owned(),
                    needs: Vec::new(),
                },
            ],
            task_specs: [
                (node("g1"), "do work 1".to_owned()),
                (node("g2"), "do work 2".to_owned()),
            ]
            .into_iter()
            .collect(),
            reducers: vec![PlanReducer {
                id: node("r1"),
                needs: vec![node("g1"), node("g2")],
                prompt: "reduce".to_owned(),
            }],
        })
        .await
        .unwrap();
    assert!(
        matches!(ack, SubmissionAck::Rejected(ref m) if m.contains("not registered")),
        "a late unregistered generator must be rejected: {ack:?}"
    );

    // No orphan rows: neither generator task row was persisted.
    assert!(stores
        .task(&generator_task_id(&started.attempt_id, &node("g1")).unwrap())
        .is_none());
    assert!(stores
        .task(&generator_task_id(&started.attempt_id, &node("g2")).unwrap())
        .is_none());
    let attempt = stores.attempt(&started.attempt_id).unwrap();
    assert_eq!(attempt.stage(), eos_state::AttemptStage::Plan);
    assert!(attempt.generator_task_ids().is_empty());
}
