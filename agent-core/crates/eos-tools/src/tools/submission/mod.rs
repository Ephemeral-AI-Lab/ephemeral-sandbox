//! Submission terminal tools.

mod advisor;
mod explorer;
mod generator;
mod lib;
mod planner;
mod reducer;
mod root;

pub(crate) fn register(
    registry: &mut crate::registry::ToolRegistry,
    config: &crate::registry::config::ToolConfigSet,
    root_submission: Option<super::RootSubmissionService>,
    attempt_submission: Option<super::AttemptSubmissionService>,
) {
    planner::register(registry, config, attempt_submission.clone());
    root::register(registry, config, root_submission);
    generator::register(registry, config, attempt_submission.clone());
    reducer::register(registry, config, attempt_submission);
    advisor::register(registry, config);
    explorer::register(registry, config);
}
