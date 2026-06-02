use std::collections::{HashMap, HashSet};

use eos_state::{Task, TaskId, TaskStatus};

use crate::{Result, WorkflowError};

/// Single-pass DAG status summary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DagStatus {
    /// Every task is terminal, or pending but unreachable because an ancestor failed.
    pub all_quiescent: bool,
    /// Every task is done.
    pub all_done: bool,
    /// Any task failed or blocked.
    pub any_failed_or_blocked: bool,
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

pub(crate) fn dag_status(tasks: &[Task]) -> Result<DagStatus> {
    let statuses = statuses_by_id(tasks);
    validate_persisted_needs(tasks, &statuses)?;
    let unreachable = unreachable_pending_ids(tasks, &statuses)?;
    Ok(DagStatus {
        all_quiescent: statuses.iter().all(|(task_id, status)| {
            status.is_terminal_generator()
                || (*status == TaskStatus::Pending && unreachable.contains(task_id))
        }),
        all_done: statuses.values().all(|s| *s == TaskStatus::Done),
        any_failed_or_blocked: statuses
            .values()
            .any(|s| matches!(s, TaskStatus::Failed | TaskStatus::Blocked)),
    })
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
        if matches!(dep_status, TaskStatus::Failed | TaskStatus::Blocked)
            || (dep_status == TaskStatus::Pending
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

#[cfg(test)]
mod tests {
    use super::*;
    use eos_state::{RequestId, TaskRole};

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
        let status = dag_status(&tasks).expect("dag status");
        assert!(!status.all_quiescent);
        assert!(!status.all_done);
        assert!(!status.any_failed_or_blocked);
    }

    #[test]
    fn failed_ancestor_makes_pending_descendant_quiescent() {
        let tasks = vec![
            task("g1", TaskStatus::Failed, &[]),
            task("r1", TaskStatus::Pending, &["g1"]),
        ];
        let status = dag_status(&tasks).expect("dag status");
        assert!(status.all_quiescent);
        assert!(status.any_failed_or_blocked);
        assert!(!status.all_done);
    }

    #[test]
    fn unknown_need_and_cycle_error() {
        let unknown = vec![task("g1", TaskStatus::Pending, &["missing"])];
        assert!(ready_pending_plan_ids(&unknown).is_err());

        let cycle = vec![
            task("a", TaskStatus::Pending, &["b"]),
            task("b", TaskStatus::Pending, &["a"]),
        ];
        assert!(dag_status(&cycle).is_err());
    }
}
