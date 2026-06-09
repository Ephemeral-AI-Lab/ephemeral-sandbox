//! Terminal submission tools.

mod submit_advisor_outcome;
mod submit_plan_outcome;
mod submit_root_task_outcome;
mod submit_subagent_outcome;
mod submit_worker_outcome;
mod support;

pub(crate) fn register(
    registry: &mut crate::ToolRegistry,
    config: &crate::registry::ToolConfigSet,
    root: crate::tools::RootSubmissionHandle,
    attempt: crate::tools::AttemptSubmissionHandle,
) {
    submit_plan_outcome::register(registry, config, attempt.clone());
    submit_root_task_outcome::register(registry, config, root);
    submit_worker_outcome::register(registry, config, attempt);
    submit_advisor_outcome::register(registry, config);
    submit_subagent_outcome::register(registry, config);
}

pub(crate) fn register_schema(
    registry: &mut crate::ToolRegistry,
    config: &crate::registry::ToolConfigSet,
) {
    submit_plan_outcome::register_schema(registry, config);
    submit_root_task_outcome::register_schema(registry, config);
    submit_worker_outcome::register_schema(registry, config);
    submit_advisor_outcome::register_schema(registry, config);
    submit_subagent_outcome::register_schema(registry, config);
}
