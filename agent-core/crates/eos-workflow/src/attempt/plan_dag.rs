use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

use eos_types::{
    AgentName, AgentRegistry, AgentType, PlanNodeId, PlannerPlan, Task, TaskId, TaskStatus,
};

use crate::{Result, WorkflowError};

/// Closed scheduler resolution for a persisted plan DAG.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DagResolution {
    /// More work may still be runnable or in flight.
    Running,
    /// Every persisted plan task is done.
    Passed,
    /// The DAG is quiescent because at least one task failed or blocked.
    FailedOrBlocked,
}

/// Pending plan tasks whose needs are all done.
pub fn ready_pending_plan_ids(tasks: &[Task]) -> Result<Vec<TaskId>> {
    let statuses = statuses_by_id(tasks);
    validate_persisted_needs(tasks, &statuses)?;
    Ok(tasks
        .iter()
        .filter(|task| task.status == TaskStatus::Pending)
        .filter(|task| {
            task.needs
                .iter()
                .all(|dep| statuses.get(dep).is_some_and(|s| *s == TaskStatus::Done))
        })
        .map(|task| task.id.clone())
        .collect())
}

pub(crate) fn dag_resolution(tasks: &[Task]) -> Result<DagResolution> {
    let statuses = statuses_by_id(tasks);
    validate_persisted_needs(tasks, &statuses)?;
    let unreachable = unreachable_pending_ids(tasks, &statuses)?;
    let all_done = statuses.values().all(|s| *s == TaskStatus::Done);
    if all_done {
        return Ok(DagResolution::Passed);
    }
    let all_quiescent = statuses.iter().all(|(task_id, status)| {
        status.is_terminal_generator()
            || (*status == TaskStatus::Pending && unreachable.contains(task_id))
    });
    let any_failed_or_blocked = statuses.values().any(|s| {
        matches!(
            s,
            TaskStatus::Failed | TaskStatus::Blocked | TaskStatus::Cancelled
        )
    });
    if all_quiescent && any_failed_or_blocked {
        Ok(DagResolution::FailedOrBlocked)
    } else {
        Ok(DagResolution::Running)
    }
}

fn statuses_by_id(tasks: &[Task]) -> HashMap<TaskId, TaskStatus> {
    tasks
        .iter()
        .map(|task| (task.id.clone(), task.status))
        .collect()
}

fn validate_persisted_needs(tasks: &[Task], statuses: &HashMap<TaskId, TaskStatus>) -> Result<()> {
    for task in tasks {
        let missing: Vec<String> = task
            .needs
            .iter()
            .filter(|dep| !statuses.contains_key(*dep))
            .map(ToString::to_string)
            .collect();
        if !missing.is_empty() {
            return Err(WorkflowError::invariant(format!(
                "plan task {:?} has unknown persisted needs: {missing:?}",
                task.id.as_str()
            )));
        }
    }
    Ok(())
}

fn unreachable_pending_ids(
    tasks: &[Task],
    statuses: &HashMap<TaskId, TaskStatus>,
) -> Result<HashSet<TaskId>> {
    let by_id: HashMap<TaskId, &Task> = tasks.iter().map(|task| (task.id.clone(), task)).collect();
    let mut visiting = HashSet::new();
    let mut memo = HashMap::new();
    let mut unreachable = HashSet::new();
    for (task_id, status) in statuses {
        if *status == TaskStatus::Pending
            && is_unreachable(task_id, statuses, &by_id, &mut visiting, &mut memo)?
        {
            unreachable.insert(task_id.clone());
        }
    }
    Ok(unreachable)
}

fn is_unreachable(
    task_id: &TaskId,
    statuses: &HashMap<TaskId, TaskStatus>,
    by_id: &HashMap<TaskId, &Task>,
    visiting: &mut HashSet<TaskId>,
    memo: &mut HashMap<TaskId, bool>,
) -> Result<bool> {
    if let Some(value) = memo.get(task_id) {
        return Ok(*value);
    }
    if !visiting.insert(task_id.clone()) {
        return Err(WorkflowError::invariant(format!(
            "plan task dependency cycle reached persisted task {:?}",
            task_id.as_str()
        )));
    }
    let status = statuses
        .get(task_id)
        .copied()
        .ok_or_else(|| WorkflowError::not_found("task", task_id.as_str()))?;
    if status != TaskStatus::Pending {
        visiting.remove(task_id);
        memo.insert(task_id.clone(), false);
        return Ok(false);
    }
    let task = by_id
        .get(task_id)
        .ok_or_else(|| WorkflowError::not_found("task", task_id.as_str()))?;
    for dep in &task.needs {
        let dep_status = statuses
            .get(dep)
            .copied()
            .ok_or_else(|| WorkflowError::not_found("task", dep.as_str()))?;
        if matches!(
            dep_status,
            TaskStatus::Failed | TaskStatus::Blocked | TaskStatus::Cancelled
        ) || (dep_status == TaskStatus::Pending
            && is_unreachable(dep, statuses, by_id, visiting, memo)?)
        {
            visiting.remove(task_id);
            memo.insert(task_id.clone(), true);
            return Ok(true);
        }
    }
    visiting.remove(task_id);
    memo.insert(task_id.clone(), false);
    Ok(false)
}

pub(crate) fn validate_plan_shape(plan: &PlannerPlan) -> Result<()> {
    if plan.reducers.is_empty() {
        return Err(WorkflowError::invariant(
            "plan must contain at least one reducer",
        ));
    }
    // D1: reject a duplicate id across the union of generators and reducers
    // (mirror `plan_dag.py` union-dedup). The tool layer only checks
    // generator-vs-generator duplicates, so a reducer<->reducer (or
    // generator<->reducer) collision would otherwise push duplicate task
    // rows into `reducer_task_ids`/`generator_task_ids`.
    let mut seen_ids: BTreeSet<&PlanNodeId> = BTreeSet::new();
    for id in plan
        .tasks
        .iter()
        .map(|task| &task.id)
        .chain(plan.reducers.iter().map(|reducer| &reducer.id))
    {
        if !seen_ids.insert(id) {
            return Err(WorkflowError::invariant(format!(
                "plan contains duplicate task id {id:?}"
            )));
        }
    }
    let generator_ids: BTreeSet<&PlanNodeId> = plan.tasks.iter().map(|task| &task.id).collect();
    let reducer_ids: BTreeSet<&PlanNodeId> = plan.reducers.iter().map(|task| &task.id).collect();
    let all_ids: BTreeSet<&PlanNodeId> = generator_ids.union(&reducer_ids).copied().collect();
    for task in &plan.tasks {
        let reducer_needs: Vec<&str> = task
            .needs
            .iter()
            .filter(|need| reducer_ids.contains(*need))
            .map(PlanNodeId::as_str)
            .collect();
        if !reducer_needs.is_empty() {
            return Err(WorkflowError::invariant(format!(
                "generator task {:?} cannot need reducer task(s): {reducer_needs:?}",
                task.id
            )));
        }
        for need in &task.needs {
            if !all_ids.contains(need) {
                return Err(WorkflowError::invariant(format!(
                    "plan task {:?} has unknown needs: {:?}",
                    task.id, need
                )));
            }
        }
    }
    let mut downstream_by_generator: BTreeMap<&PlanNodeId, Vec<&str>> =
        generator_ids.iter().map(|id| (*id, Vec::new())).collect();
    for task in &plan.tasks {
        for need in &task.needs {
            if let Some(downstream) = downstream_by_generator.get_mut(need) {
                downstream.push(task.id.as_str());
            }
        }
    }
    for reducer in &plan.reducers {
        if reducer.needs.is_empty() {
            return Err(WorkflowError::invariant(format!(
                "reducer task {:?} must need at least one generator",
                reducer.id
            )));
        }
        for need in &reducer.needs {
            if reducer_ids.contains(need) {
                return Err(WorkflowError::invariant(format!(
                    "reducer task {:?} cannot need reducer task(s)",
                    reducer.id
                )));
            }
            if !all_ids.contains(need) {
                return Err(WorkflowError::invariant(format!(
                    "plan task {:?} has unknown needs: {:?}",
                    reducer.id, need
                )));
            }
            if let Some(downstream) = downstream_by_generator.get_mut(need) {
                downstream.push(reducer.id.as_str());
            }
        }
    }
    let dangling: Vec<&str> = downstream_by_generator
        .iter()
        .filter_map(|(id, downstream)| downstream.is_empty().then_some(id.as_str()))
        .collect();
    if !dangling.is_empty() {
        return Err(WorkflowError::invariant(format!(
            "plan has generator(s) no downstream task needs: {dangling:?}"
        )));
    }
    assert_acyclic(plan)
}

/// Validate every plan agent before any task row is written: each generator
/// is bound to a registered workflow-launchable profile and has a task spec,
/// and the fixed `reducer` profile is registered. Runs after the pure shape
/// checks and before materialization so a rejected plan leaves no orphan rows.
pub(crate) fn validate_plan_agents(plan: &PlannerPlan, registry: &AgentRegistry) -> Result<()> {
    for task in &plan.tasks {
        let agent_name = AgentName::new(task.agent_name.clone())?;
        let agent_def = registry.get(&agent_name).ok_or_else(|| {
            WorkflowError::AgentDefinition(format!(
                "agent definition {:?} is not registered",
                task.agent_name
            ))
        })?;
        // D6: a generator task must be bound to an agent-type profile. The
        // generator role itself is task lineage, not profile metadata.
        if agent_def.agent_type != AgentType::Agent {
            return Err(WorkflowError::invariant(format!(
                "generator task {:?} is bound to agent {:?} with type {:?}, expected agent",
                task.id, task.agent_name, agent_def.agent_type
            )));
        }
        if !plan.task_specs.contains_key(&task.id) {
            return Err(WorkflowError::not_found("task spec", task.id.as_str()));
        }
    }
    let reducer_name = AgentName::new("reducer")?;
    let reducer = registry.get(&reducer_name).ok_or_else(|| {
        WorkflowError::AgentDefinition("agent definition \"reducer\" is not registered".to_owned())
    })?;
    if reducer.agent_type != AgentType::Agent {
        return Err(WorkflowError::invariant(format!(
            "reducer profile has type {:?}, expected agent",
            reducer.agent_type
        )));
    }
    Ok(())
}

fn assert_acyclic(plan: &PlannerPlan) -> Result<()> {
    let mut by_needs: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for task in &plan.tasks {
        by_needs.insert(
            task.id.as_str(),
            task.needs.iter().map(PlanNodeId::as_str).collect(),
        );
    }
    for reducer in &plan.reducers {
        by_needs.insert(
            reducer.id.as_str(),
            reducer.needs.iter().map(PlanNodeId::as_str).collect(),
        );
    }
    let mut remaining = by_needs
        .iter()
        .map(|(id, needs)| (*id, needs.iter().copied().collect::<BTreeSet<_>>()))
        .collect::<BTreeMap<_, _>>();
    let mut dependents: BTreeMap<&str, Vec<&str>> =
        by_needs.keys().map(|id| (*id, Vec::new())).collect();
    for (id, needs) in &by_needs {
        for need in needs {
            if let Some(entries) = dependents.get_mut(need) {
                entries.push(id);
            }
        }
    }
    let mut ready = remaining
        .iter()
        .filter_map(|(id, needs)| needs.is_empty().then_some(*id))
        .collect::<VecDeque<_>>();
    let mut order = Vec::new();
    while let Some(id) = ready.pop_front() {
        order.push(id);
        for dependent in dependents.get(id).into_iter().flatten() {
            if let Some(needs) = remaining.get_mut(dependent) {
                needs.remove(id);
                if needs.is_empty() {
                    ready.push_back(dependent);
                }
            }
        }
    }
    if order.len() != by_needs.len() {
        let ordered = order.into_iter().collect::<BTreeSet<_>>();
        let cycle = by_needs
            .keys()
            .filter(|id| !ordered.contains(**id))
            .copied()
            .collect::<Vec<_>>();
        return Err(WorkflowError::invariant(format!(
            "plan contains a dependency cycle among: {cycle:?}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use eos_types::{RequestId, TaskRole};

    fn tid(s: &str) -> TaskId {
        s.parse().expect("task id")
    }

    fn task(id: &str, status: TaskStatus, needs: &[&str]) -> Task {
        Task {
            id: tid(id),
            request_id: RequestId::new_v4(),
            role: TaskRole::Generator,
            instruction: "do".to_owned(),
            status,
            workflow_id: None,
            iteration_id: None,
            attempt_id: None,
            agent_name: Some("coder".to_owned()),
            needs: needs.iter().map(|n| tid(n)).collect(),
            outcomes: Vec::new(),
            terminal_tool_result: None,
        }
    }

    #[test]
    fn dag_status_mixed_ready_and_quiescent() {
        let tasks = vec![
            task("g1", TaskStatus::Done, &[]),
            task("g2", TaskStatus::Pending, &["g1"]),
            task("r1", TaskStatus::Pending, &["g2"]),
        ];
        assert_eq!(
            ready_pending_plan_ids(&tasks).expect("ready ids"),
            vec![tid("g2")]
        );
        assert_eq!(
            dag_resolution(&tasks).expect("dag resolution"),
            DagResolution::Running
        );
    }

    #[test]
    fn failed_ancestor_makes_pending_descendant_quiescent() {
        let tasks = vec![
            task("g1", TaskStatus::Failed, &[]),
            task("r1", TaskStatus::Pending, &["g1"]),
        ];
        assert_eq!(
            dag_resolution(&tasks).expect("dag resolution"),
            DagResolution::FailedOrBlocked
        );
    }

    #[test]
    fn unknown_need_and_cycle_error() {
        let unknown = vec![task("g1", TaskStatus::Pending, &["missing"])];
        assert!(ready_pending_plan_ids(&unknown).is_err());

        let cycle = vec![
            task("a", TaskStatus::Pending, &["b"]),
            task("b", TaskStatus::Pending, &["a"]),
        ];
        assert!(dag_resolution(&cycle).is_err());
    }

    // ---- authoring-time plan-shape validation (`validate_plan_shape`) --------
    //
    // The cycle test above exercises `dag_resolution` (the persisted-state
    // scheduler). `validate_plan_shape` is the distinct authoring-time gate that
    // rejects a malformed planner submission before any task row is written; its
    // reject branches (and `assert_acyclic`) were untested. Each case isolates
    // one rejection and pairs it with the accepted baseline, so the test fails if
    // a guard is dropped (malformed -> Ok) or the baseline breaks (valid -> Err).

    fn pnode(id: &str) -> PlanNodeId {
        PlanNodeId::new(id).expect("plan node id")
    }

    fn gen(id: &str, needs: &[&str]) -> eos_types::PlanTask {
        eos_types::PlanTask {
            id: pnode(id),
            agent_name: "coder".to_owned(),
            needs: needs.iter().map(|n| pnode(n)).collect(),
        }
    }

    fn red(id: &str, needs: &[&str]) -> eos_types::PlanReducer {
        eos_types::PlanReducer {
            id: pnode(id),
            needs: needs.iter().map(|n| pnode(n)).collect(),
            prompt: "reduce".to_owned(),
        }
    }

    fn shape_plan(
        tasks: Vec<eos_types::PlanTask>,
        reducers: Vec<eos_types::PlanReducer>,
    ) -> PlannerPlan {
        let task_specs = tasks
            .iter()
            .map(|task| (task.id.clone(), "spec".to_owned()))
            .collect();
        PlannerPlan {
            attempt_id: eos_types::AttemptId::new_v4(),
            planner_task_id: tid("planner"),
            disposition: eos_types::PlanDisposition::Complete,
            tasks,
            task_specs,
            reducers,
        }
    }

    #[test]
    fn validate_plan_shape_accepts_valid_and_rejects_each_malformation() {
        // Baseline: one generator feeding one reducer — a well-formed DAG.
        validate_plan_shape(&shape_plan(vec![gen("g1", &[])], vec![red("r1", &["g1"])]))
            .expect("a well-formed plan validates");

        // No reducer.
        assert!(validate_plan_shape(&shape_plan(vec![gen("g1", &[])], vec![])).is_err());
        // Duplicate id across the generator/reducer union.
        assert!(validate_plan_shape(&shape_plan(
            vec![gen("g1", &[]), gen("g1", &[])],
            vec![red("r1", &["g1"])],
        ))
        .is_err());
        // A generator may not depend on a reducer.
        assert!(validate_plan_shape(&shape_plan(
            vec![gen("g1", &["r1"])],
            vec![red("r1", &["g1"])],
        ))
        .is_err());
        // A generator with an unknown need.
        assert!(validate_plan_shape(&shape_plan(
            vec![gen("g1", &["ghost"])],
            vec![red("r1", &["g1"])],
        ))
        .is_err());
        // A reducer with no needs.
        assert!(
            validate_plan_shape(&shape_plan(vec![gen("g1", &[])], vec![red("r1", &[])])).is_err()
        );
        // A reducer that depends on another reducer.
        assert!(validate_plan_shape(&shape_plan(
            vec![gen("g1", &[])],
            vec![red("r1", &["g1"]), red("r2", &["r1"])],
        ))
        .is_err());
        // A reducer with an unknown need (alongside a valid one).
        assert!(validate_plan_shape(&shape_plan(
            vec![gen("g1", &[])],
            vec![red("r1", &["g1", "ghost"])],
        ))
        .is_err());
        // A dangling generator no downstream task needs.
        assert!(validate_plan_shape(&shape_plan(
            vec![gen("g1", &[]), gen("g2", &[])],
            vec![red("r1", &["g1"])],
        ))
        .is_err());
        // A generator dependency cycle (reaches `assert_acyclic`).
        assert!(validate_plan_shape(&shape_plan(
            vec![gen("g1", &["g2"]), gen("g2", &["g1"])],
            vec![red("r1", &["g1"])],
        ))
        .is_err());
    }
}
