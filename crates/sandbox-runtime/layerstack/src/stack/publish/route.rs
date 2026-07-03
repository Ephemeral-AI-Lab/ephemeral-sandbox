use crate::model::LayerPath;

use super::model::PublishRejectReason;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RouteKind {
    Source,
    Ignored,
}

pub(crate) fn forbidden_path(path: &LayerPath) -> Option<PublishRejectReason> {
    is_protected(path.as_str()).then_some(PublishRejectReason::ProtectedPath)
}

fn is_protected(path: &str) -> bool {
    let mut parts = path.split('/');
    let first = parts.next().unwrap_or_default();
    if matches!(
        first,
        "manifest.json" | "workspace.json" | "layers" | "staging" | ".layer-metadata"
    ) {
        return true;
    }
    path.split('/').any(|part| part == ".layer-metadata")
}
