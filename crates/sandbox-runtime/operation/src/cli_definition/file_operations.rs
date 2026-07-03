use serde_json::{json, Value};

use crate::file::{
    BlameRange, EditInput, EditOp, EditOutput, FileError, FileOperationError, ReadInput,
    ReadOutput, WriteInput, WriteOutput,
};
use crate::operation::OperationEntry;
use crate::workspace_crate::WorkspaceSessionId;
use crate::SandboxRuntimeOperations;
use sandbox_protocol::{error_kind, Request, Response};
use sandbox_runtime_layerstack::LayerPath;
use sandbox_runtime_operations::{
    FILE_BLAME_SPEC, FILE_EDIT_SPEC, FILE_READ_SPEC, FILE_WRITE_SPEC,
};

const FILE_NOT_FOUND: &str = "not_found";
const READ_LIMIT_MAX: u64 = 2000;

const FILE_BLAME: OperationEntry = OperationEntry::cli(&FILE_BLAME_SPEC, dispatch_file_blame);
const FILE_READ: OperationEntry = OperationEntry::cli(&FILE_READ_SPEC, dispatch_file_read);
const FILE_WRITE: OperationEntry = OperationEntry::cli(&FILE_WRITE_SPEC, dispatch_file_write);
const FILE_EDIT: OperationEntry = OperationEntry::cli(&FILE_EDIT_SPEC, dispatch_file_edit);

const OPERATIONS: &[OperationEntry] = &[FILE_BLAME, FILE_READ, FILE_WRITE, FILE_EDIT];

pub(crate) const fn operation_entries() -> &'static [OperationEntry] {
    OPERATIONS
}

fn dispatch_file_blame(operations: &SandboxRuntimeOperations, request: &Request) -> Response {
    let path = match request.required_string("path") {
        Ok(path) => path,
        Err(response) => return response,
    };
    let canonical_path = match LayerPath::parse(&path) {
        Ok(path) => path,
        Err(_) => {
            return Response::fault_with_details(
                FILE_NOT_FOUND,
                format!("no auditability record for path: {path}"),
                json!({ "path": path }),
            )
        }
    };
    match operations.file.blame(canonical_path.as_str()) {
        Ok(ranges) => Response::ok(file_blame_value(canonical_path.as_str(), &ranges)),
        Err(FileError::NotFound(missing)) => Response::fault_with_details(
            FILE_NOT_FOUND,
            format!("no auditability record for path: {missing}"),
            json!({ "path": missing }),
        ),
    }
}

fn dispatch_file_read(operations: &SandboxRuntimeOperations, request: &Request) -> Response {
    let input = match parse_read_input(request) {
        Ok(input) => input,
        Err(response) => return response,
    };
    match operations.file.read(
        operations.layerstack.as_ref(),
        operations.workspace_session.as_ref(),
        input,
    ) {
        Ok(output) => Response::ok(file_read_value(&output)),
        Err(error) => file_operation_error_response(error),
    }
}

fn dispatch_file_write(operations: &SandboxRuntimeOperations, request: &Request) -> Response {
    let input = match parse_write_input(request) {
        Ok(input) => input,
        Err(response) => return response,
    };
    match operations.file.write(
        operations.layerstack.as_ref(),
        operations.workspace_session.as_ref(),
        input,
    ) {
        Ok(output) => Response::ok(file_write_value(&output)),
        Err(error) => file_operation_error_response(error),
    }
}

fn dispatch_file_edit(operations: &SandboxRuntimeOperations, request: &Request) -> Response {
    let input = match parse_edit_input(request) {
        Ok(input) => input,
        Err(response) => return response,
    };
    match operations.file.edit(
        operations.layerstack.as_ref(),
        operations.workspace_session.as_ref(),
        input,
    ) {
        Ok(output) => Response::ok(file_edit_value(&output)),
        Err(error) => file_operation_error_response(error),
    }
}

fn parse_read_input(request: &Request) -> Result<ReadInput, Response> {
    let limit = request.optional_usize("limit")?;
    if matches!(limit, Some(value) if value < 1 || value as u64 > READ_LIMIT_MAX) {
        return Err(request.invalid_argument("limit must be between 1 and 2000"));
    }
    Ok(ReadInput {
        path: request.required_string("path")?,
        offset: request.optional_u64("offset")?,
        limit,
        workspace_session_id: parse_workspace_session_id(request)?,
    })
}

fn parse_write_input(request: &Request) -> Result<WriteInput, Response> {
    Ok(WriteInput {
        path: request.required_string("path")?,
        content: request.optional_string("content")?.unwrap_or_default(),
        request_id: request.request_id.clone(),
        workspace_session_id: parse_workspace_session_id(request)?,
    })
}

fn parse_edit_input(request: &Request) -> Result<EditInput, Response> {
    Ok(EditInput {
        path: request.required_string("path")?,
        edits: parse_edits(request)?,
        request_id: request.request_id.clone(),
        workspace_session_id: parse_workspace_session_id(request)?,
    })
}

fn parse_workspace_session_id(request: &Request) -> Result<Option<WorkspaceSessionId>, Response> {
    Ok(request
        .optional_string("workspace_session_id")?
        .filter(|workspace_session_id| !workspace_session_id.is_empty())
        .map(WorkspaceSessionId))
}

fn parse_edits(request: &Request) -> Result<Vec<EditOp>, Response> {
    let value = request
        .args
        .get("edits")
        .ok_or_else(|| request.invalid_argument("edits is required for file_edit"))?;
    let items = match value {
        Value::Array(items) => items.clone(),
        Value::String(text) => match serde_json::from_str::<Value>(text) {
            Ok(Value::Array(items)) => items,
            _ => return Err(request.invalid_argument("edits must be a JSON array")),
        },
        _ => {
            return Err(
                request.invalid_argument("edits must be a JSON array or JSON-encoded string")
            )
        }
    };
    items
        .iter()
        .enumerate()
        .map(|(index, item)| parse_edit_op(request, index, item))
        .collect()
}

fn parse_edit_op(request: &Request, index: usize, item: &Value) -> Result<EditOp, Response> {
    let object = item
        .as_object()
        .ok_or_else(|| request.invalid_argument(format!("edits[{index}] must be an object")))?;
    let old_string = object
        .get("old_string")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            request.invalid_argument(format!("edits[{index}].old_string must be a string"))
        })?;
    let new_string = object
        .get("new_string")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            request.invalid_argument(format!("edits[{index}].new_string must be a string"))
        })?;
    let replace_all = match object.get("replace_all") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(value)) => *value,
        Some(_) => {
            return Err(
                request.invalid_argument(format!("edits[{index}].replace_all must be a boolean"))
            )
        }
    };
    Ok(EditOp {
        old_string: old_string.to_owned(),
        new_string: new_string.to_owned(),
        replace_all,
    })
}

fn file_operation_error_response(error: FileOperationError) -> Response {
    let message = error.to_string();
    match error {
        FileOperationError::NotFound(path) => {
            Response::fault_with_details(FILE_NOT_FOUND, message, json!({ "path": path }))
        }
        FileOperationError::WorkspaceSessionNotFound(id) => Response::fault_with_details(
            FILE_NOT_FOUND,
            message,
            json!({ "workspace_session_id": id }),
        ),
        FileOperationError::InvalidPath(_)
        | FileOperationError::NotUtf8(_)
        | FileOperationError::NotRegular { .. }
        | FileOperationError::FileTooLarge { .. }
        | FileOperationError::OutputTooLarge { .. }
        | FileOperationError::EditNotFound { .. }
        | FileOperationError::EditNotUnique { .. }
        | FileOperationError::NoEdits
        | FileOperationError::NoChanges(_) => Response::fault(error_kind::INVALID_REQUEST, message),
        FileOperationError::WorkspaceSession(_)
        | FileOperationError::LayerStack(_)
        | FileOperationError::Io { .. } => Response::fault(error_kind::OPERATION_FAILED, message),
    }
}

fn file_blame_value(path: &str, ranges: &[BlameRange]) -> Value {
    json!({
        "path": path,
        "ranges": ranges
            .iter()
            .map(|range| {
                json!({
                    "start_line": range.start_line,
                    "line_count": range.line_count,
                    "owner": range.owner,
                })
            })
            .collect::<Vec<_>>(),
    })
}

fn file_read_value(output: &ReadOutput) -> Value {
    json!({
        "path": output.path,
        "content": output.content,
        "start_line": output.start_line,
        "num_lines": output.num_lines,
        "total_lines": output.total_lines,
        "bytes_read": output.bytes_read,
        "total_bytes": output.total_bytes,
        "next_offset": output.next_offset,
        "truncated": output.truncated,
    })
}

fn file_write_value(output: &WriteOutput) -> Value {
    json!({
        "type": output.kind.as_str(),
        "path": output.path,
        "bytes_written": output.bytes_written,
    })
}

fn file_edit_value(output: &EditOutput) -> Value {
    json!({
        "type": "edit",
        "path": output.path,
        "edits_applied": output.edits_applied,
        "replacements": output.replacements,
        "bytes_written": output.bytes_written,
    })
}
