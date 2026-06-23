use serde_json::{json, Value};

use crate::cli_definition::{CliOperationFamilySpec, CliOperationSpec, CliSpec};
use crate::layerstack::{LayerStackRevision, LayerStackServiceError, SquashLayerStackResult};
use crate::observability::{measure_optional, OperationTrace};
use crate::operation::OperationEntry;
use crate::SandboxRuntimeOperations;
use sandbox_protocol::{Request, Response};

pub(crate) const LAYERSTACK_FAMILY: CliOperationFamilySpec = CliOperationFamilySpec {
    id: "layerstack",
    title: "Layer Stack",
    summary: "Inspect and compact runtime layer stack state.",
    description: "Inspect and compact the sandbox runtime layer stack.",
};

const SQUASH_SPEC: CliOperationSpec = CliOperationSpec {
    name: "squash",
    family: "layerstack",
    summary: "Squash committed layer stack revisions.",
    description: "Compact the runtime layer stack into a single current revision when squashable layers exist.",
    args: &[],
    cli: Some(CliSpec {
        path: &["runtime", "squash"],
        usage: "sandbox-cli runtime squash",
        examples: &["sandbox-cli runtime squash"],
    }),
    related: &[],
};

const SQUASH: OperationEntry = OperationEntry::cli(&SQUASH_SPEC, dispatch_squash);
const OPERATIONS: &[OperationEntry] = &[SQUASH];

pub(crate) fn operation_entries() -> &'static [OperationEntry] {
    OPERATIONS
}

fn dispatch_squash(
    operations: &SandboxRuntimeOperations,
    _request: &Request,
    trace: Option<&OperationTrace>,
) -> Response {
    squash_response(measure_optional(trace, "LayerStackService::squash", || {
        operations.layerstack.squash(trace)
    }))
}

fn squash_response(result: Result<SquashLayerStackResult, LayerStackServiceError>) -> Response {
    match result {
        Ok(result) => Response::ok(squash_result_value(result)),
        Err(error) => Response::fault_with_details(
            "operation_failed",
            error.to_string(),
            json!({ "kind": error.kind() }),
        ),
    }
}

fn squash_result_value(result: SquashLayerStackResult) -> Value {
    json!({
        "squashed": result.squashed,
        "revision": revision_value(result.revision),
        "layer_paths": result
            .layer_paths
            .into_iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
        "lease_release_error": result.lease_release_error,
    })
}

fn revision_value(revision: Option<LayerStackRevision>) -> Value {
    revision.map_or(Value::Null, |revision| {
        json!({
            "manifest_version": revision.manifest_version,
            "root_hash": revision.root_hash,
            "layer_count": revision.layer_count,
        })
    })
}
