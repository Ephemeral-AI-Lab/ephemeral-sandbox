use sandbox_runtime_workspace::ExecutionScratchRoute;

pub(crate) fn observed_scratch_route(routes: &[ExecutionScratchRoute]) -> &'static str {
    match routes.first().copied() {
        None => ExecutionScratchRoute::WorkspaceScoped.as_str(),
        Some(first) if routes.iter().all(|candidate| *candidate == first) => first.as_str(),
        Some(_) => "mixed",
    }
}
