use std::collections::{BTreeMap, BTreeSet};

use eos_types::{
    AgentRegistry, AgentType, Attempt, ExecutionNode, PlannerOutcome, WorkItemId, WorkItemSpec,
};

use crate::{Result, WorkflowError};

use super::AttemptResources;

pub(crate) fn validate_work_items(
    work_items: &[WorkItemSpec],
    agents: &AgentRegistry,
) -> Result<()> {
    if work_items.is_empty() {
        return Err(WorkflowError::invariant(
            "planner outcome must contain at least one work item",
        ));
    }
    let mut ids = BTreeSet::new();
    for item in work_items {
        if !ids.insert(item.id.clone()) {
            return Err(WorkflowError::invariant(format!(
                "plan contains duplicate work item id {:?}",
                item.id.as_str()
            )));
        }
        if item.work_spec.trim().is_empty() {
            return Err(WorkflowError::invariant(format!(
                "work item {:?} has blank work_spec",
                item.id.as_str()
            )));
        }
        let Some(agent) = agents.get(&item.agent_name) else {
            return Err(WorkflowError::AgentDefinition(format!(
                "work item {:?} references unknown agent {:?}",
                item.id.as_str(),
                item.agent_name.as_str()
            )));
        };
        if agent.agent_type != AgentType::Agent {
            return Err(WorkflowError::AgentDefinition(format!(
                "work item {:?} references agent {:?} with type {:?}, expected agent",
                item.id.as_str(),
                item.agent_name.as_str(),
                agent.agent_type
            )));
        }
    }
    for item in work_items {
        for need in &item.needs {
            if !ids.contains(need) {
                return Err(WorkflowError::invariant(format!(
                    "work item {:?} needs unknown work item {:?}",
                    item.id.as_str(),
                    need.as_str()
                )));
            }
        }
    }
    assert_acyclic(work_items)
}

pub(crate) fn execution_nodes(work_items: &[WorkItemSpec]) -> Vec<ExecutionNode> {
    work_items
        .iter()
        .map(|item| ExecutionNode {
            work_item_id: item.id.clone(),
            needs: item.needs.clone(),
            task_id: None,
        })
        .collect()
}

pub(crate) fn work_item_by_id<'a>(
    work_items: &'a [WorkItemSpec],
    work_item_id: &WorkItemId,
) -> Result<&'a WorkItemSpec> {
    work_items
        .iter()
        .find(|item| &item.id == work_item_id)
        .ok_or_else(|| WorkflowError::not_found("work item", work_item_id.as_str()))
}

pub(crate) async fn planner_outcome_for_attempt(
    deps: &AttemptResources,
    attempt: &Attempt,
) -> Result<PlannerOutcome> {
    let planner_task_id = attempt.planner_task_id().ok_or_else(|| {
        WorkflowError::invariant(format!(
            "attempt {:?} has no planner task",
            attempt.id.as_str()
        ))
    })?;
    let task = deps
        .task_store
        .get(planner_task_id)
        .await?
        .ok_or_else(|| WorkflowError::not_found("planner task", planner_task_id.as_str()))?;
    let outcome = task.task_outcome.ok_or_else(|| {
        WorkflowError::invariant(format!(
            "planner task {:?} has no planner outcome",
            planner_task_id.as_str()
        ))
    })?;
    outcome.planner_outcome().ok_or_else(|| {
        WorkflowError::invariant(format!(
            "planner task {:?} did not record a planner outcome",
            planner_task_id.as_str()
        ))
    })
}

fn assert_acyclic(work_items: &[WorkItemSpec]) -> Result<()> {
    let graph = work_items
        .iter()
        .map(|item| (item.id.clone(), item.needs.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    for id in graph.keys() {
        visit(id, &graph, &mut visiting, &mut visited)?;
    }
    Ok(())
}

fn visit(
    id: &WorkItemId,
    graph: &BTreeMap<WorkItemId, Vec<WorkItemId>>,
    visiting: &mut BTreeSet<WorkItemId>,
    visited: &mut BTreeSet<WorkItemId>,
) -> Result<()> {
    if visited.contains(id) {
        return Ok(());
    }
    if !visiting.insert(id.clone()) {
        return Err(WorkflowError::invariant(format!(
            "plan contains a dependency cycle involving {:?}",
            id.as_str()
        )));
    }
    for need in graph.get(id).into_iter().flatten() {
        visit(need, graph, visiting, visited)?;
    }
    visiting.remove(id);
    visited.insert(id.clone());
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use eos_types::{
        AgentDefinition, AgentName, AgentRegistry, AgentType, WorkItemId, WorkItemSpec,
    };

    use super::{execution_nodes, validate_work_items};

    #[test]
    fn validates_acyclic_worker_plan_and_materializes_nodes() {
        let work_items = vec![
            item("w1", "executor", "first", []),
            item("w2", "executor", "second", ["w1"]),
        ];

        validate_work_items(
            &work_items,
            &registry([agent("executor", AgentType::Agent)]),
        )
        .expect("valid work item plan");

        let nodes = execution_nodes(&work_items);
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[1].work_item_id.as_str(), "w2");
        assert_eq!(nodes[1].needs[0].as_str(), "w1");
        assert!(nodes.iter().all(|node| node.task_id.is_none()));
    }

    #[test]
    fn rejects_duplicate_unknown_cycle_and_non_worker_agent() {
        let agents = registry([
            agent("executor", AgentType::Agent),
            agent("reviewer", AgentType::Advisor),
        ]);

        assert!(validate_work_items(
            &[
                item("w1", "executor", "first", []),
                item("w1", "executor", "dupe", []),
            ],
            &agents,
        )
        .is_err());
        assert!(
            validate_work_items(&[item("w1", "executor", "first", ["missing"])], &agents).is_err()
        );
        assert!(validate_work_items(
            &[
                item("w1", "executor", "first", ["w2"]),
                item("w2", "executor", "second", ["w1"]),
            ],
            &agents,
        )
        .is_err());
        assert!(validate_work_items(&[item("w1", "reviewer", "first", [])], &agents).is_err());
    }

    fn registry<const N: usize>(defs: [AgentDefinition; N]) -> AgentRegistry {
        defs.into_iter().collect()
    }

    fn agent(name: &str, agent_type: AgentType) -> AgentDefinition {
        AgentDefinition {
            name: AgentName::new(name).expect("agent name"),
            description: String::new(),
            system_prompt: None,
            model: None,
            tool_call_limit: NonZeroU32::new(8).expect("nonzero"),
            agent_type,
            allowed_tools: Vec::new(),
            terminals: vec!["submit_worker_outcome".to_owned()],
            notification_triggers: Vec::new(),
            skill: None,
            context_recipe: None,
        }
    }

    fn item<const N: usize>(
        id: &str,
        agent_name: &str,
        work_spec: &str,
        needs: [&str; N],
    ) -> WorkItemSpec {
        WorkItemSpec {
            id: WorkItemId::new(id).expect("work item id"),
            agent_name: AgentName::new(agent_name).expect("agent name"),
            work_spec: work_spec.to_owned(),
            needs: needs
                .into_iter()
                .map(|need| WorkItemId::new(need).expect("need id"))
                .collect(),
        }
    }
}
