mod submit_root_outcome;

pub(super) fn register(
    registry: &mut crate::registry::ToolRegistry,
    config: &crate::registry::config::ToolConfigSet,
    root_submission: Option<super::super::RootSubmissionService>,
) {
    submit_root_outcome::register(registry, config, root_submission);
}
