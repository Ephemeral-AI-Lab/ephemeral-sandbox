//! CLI-owned argv projection and request input construction.

use std::fs::File;
use std::io::Read;

use sandbox_operation_client::{
    build_request_from_values, build_request_from_values_with_id, BuildRequestValueInput,
    RequestBuildError, MAX_REQUEST_BYTES,
};
use sandbox_operation_contract::{
    operation_domain_name, ArgKind, ArgSpecDocument, OperationCatalogDocument, OperationDomain,
    OperationRequest, OperationScopePolicy, OperationSpecDocument,
};
use serde_json::{Map, Number, Value};

use crate::projection::document::{operation_projection, CatalogDocument};
use crate::projection::{ArgumentProjection, OperationProjection};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildRequestInput {
    pub execution_space: OperationDomain,
    pub operation: String,
    pub operation_argv: Vec<String>,
    pub sandbox_id: Option<String>,
}

/// Build an operation request for `input` against `catalog`, minting a fresh request id.
///
/// # Errors
/// Returns an error for unknown operations, argument parse failures, or an
/// invalid scope selector.
pub fn build_request_from_catalog(
    input: BuildRequestInput,
    catalog: &CatalogDocument,
) -> Result<OperationRequest, RequestBuildError> {
    let (spec, scope_policy, scope_selector, arguments) = prepare_request(input, catalog)?;
    build_request_from_values(BuildRequestValueInput {
        spec,
        scope_policy,
        scope_selector,
        arguments,
    })
}

/// Build an operation request for `input` against `catalog` with an explicit request id.
///
/// # Errors
/// Returns an error for unknown operations, argument parse failures, or an
/// invalid scope selector.
pub fn build_request_from_catalog_with_id(
    input: BuildRequestInput,
    catalog: &CatalogDocument,
    request_id: impl Into<String>,
) -> Result<OperationRequest, RequestBuildError> {
    let (spec, scope_policy, scope_selector, arguments) = prepare_request(input, catalog)?;
    build_request_from_values_with_id(
        BuildRequestValueInput {
            spec,
            scope_policy,
            scope_selector,
            arguments,
        },
        request_id,
    )
}

fn prepare_request(
    input: BuildRequestInput,
    catalog: &CatalogDocument,
) -> Result<
    (
        &OperationSpecDocument,
        OperationScopePolicy,
        Option<String>,
        Value,
    ),
    RequestBuildError,
> {
    let (spec, projection, scope_policy) =
        projected_operation(input.execution_space, &input.operation, catalog)?;
    let arguments = build_args(spec, projection, &input.operation_argv)?;
    let scope_selector = scope_selector(input.execution_space, input.sandbox_id, &arguments)?;
    Ok((spec, scope_policy, scope_selector, arguments))
}

fn projected_operation<'a>(
    execution_space: OperationDomain,
    operation: &str,
    catalog: &'a CatalogDocument,
) -> Result<
    (
        &'a OperationSpecDocument,
        &'a OperationProjection,
        OperationScopePolicy,
    ),
    RequestBuildError,
> {
    let spec = semantic_operation(execution_space, operation, &catalog.semantic)?;
    let projection = operation_projection(catalog, operation)
        .ok_or_else(|| build_error(format!("unknown operation: {operation}")))?;
    let scope_policy = catalog
        .semantic
        .routes
        .iter()
        .find(|route| route.operation == operation)
        .map(|route| route.scope_policy)
        .ok_or_else(|| build_error(format!("operation has no public route: {operation}")))?;
    Ok((spec, projection, scope_policy))
}

fn semantic_operation<'a>(
    execution_space: OperationDomain,
    operation: &str,
    catalog: &'a OperationCatalogDocument,
) -> Result<&'a OperationSpecDocument, RequestBuildError> {
    if execution_space != catalog.operation_execution_space {
        return Err(build_error(format!(
            "loaded catalog is for {}, not {}",
            operation_domain_name(catalog.operation_execution_space),
            operation_domain_name(execution_space)
        )));
    }
    if operation == "help" {
        return Err(build_error(
            "help is reserved and cannot be used as an operation name",
        ));
    }
    catalog
        .operations
        .iter()
        .find(|spec| spec.name == operation)
        .ok_or_else(|| build_error(format!("unknown operation: {operation}")))
}

fn scope_selector(
    execution_space: OperationDomain,
    runtime_sandbox_id: Option<String>,
    arguments: &Value,
) -> Result<Option<String>, RequestBuildError> {
    match execution_space {
        OperationDomain::Manager => Ok(None),
        OperationDomain::Runtime => resolve_runtime_sandbox_id(runtime_sandbox_id).map(Some),
        OperationDomain::Observability => observability_scope_selector(arguments),
    }
}

fn observability_scope_selector(arguments: &Value) -> Result<Option<String>, RequestBuildError> {
    match arguments.get("sandbox_id") {
        Some(Value::String(sandbox_id)) if !sandbox_id.trim().is_empty() => {
            Ok(Some(sandbox_id.clone()))
        }
        Some(Value::String(_)) => Err(build_error("--sandbox-id must be non-empty")),
        Some(_) => Err(build_error("--sandbox-id must be a string")),
        None => Ok(None),
    }
}

/// Resolve the sandbox id for a runtime operation.
///
/// Runtime operations require an explicit `--sandbox-id`; there is no config or
/// environment fallback.
///
/// # Errors
/// Returns an error when the sandbox id is absent or empty.
pub fn resolve_runtime_sandbox_id(sandbox_id: Option<String>) -> Result<String, RequestBuildError> {
    let sandbox_id =
        sandbox_id.ok_or_else(|| build_error("runtime operations require --sandbox-id"))?;
    if sandbox_id.trim().is_empty() {
        Err(build_error("runtime sandbox id must be non-empty"))
    } else {
        Ok(sandbox_id)
    }
}

fn build_args(
    spec: &OperationSpecDocument,
    projection: &OperationProjection,
    argv: &[String],
) -> Result<Value, RequestBuildError> {
    let mut values = Map::new();
    let positional_args = projection
        .arguments
        .iter()
        .filter(|arg| arg.positional.is_some())
        .collect::<Vec<_>>();
    let mut next_positional = 0usize;
    let mut index = 0usize;

    while index < argv.len() {
        let token = &argv[index];
        if token.starts_with("--") {
            let projected_arg = find_flag_arg(projection, token)?;
            let arg = semantic_argument(spec, projected_arg)?;
            index = index.saturating_add(1);
            let value = argv
                .get(index)
                .ok_or_else(|| build_error(format!("{token} requires a value")))?;
            let value = read_projected_value(projected_arg, token, value)?;
            insert_arg_value(&mut values, arg, projected_arg, &value)?;
        } else {
            let projected_arg = positional_args.get(next_positional).ok_or_else(|| {
                build_error(format!(
                    "unexpected positional argument for {}: {token}",
                    spec.name
                ))
            })?;
            let arg = semantic_argument(spec, projected_arg)?;
            next_positional = next_positional.saturating_add(1);
            insert_arg_value(&mut values, arg, projected_arg, token)?;
        }
        index = index.saturating_add(1);
    }

    require_cli_args(spec, projection, &values)?;
    Ok(Value::Object(values))
}

fn read_projected_value(
    projection: &ArgumentProjection,
    flag: &str,
    value: &str,
) -> Result<String, RequestBuildError> {
    if projection.value_file_flag != Some(flag) {
        return Ok(value.to_owned());
    }
    let file = File::open(value)
        .map_err(|error| build_error(format!("{flag} could not read {value}: {error}")))?;
    let length = file
        .metadata()
        .map_err(|error| build_error(format!("{flag} could not inspect {value}: {error}")))?
        .len();
    if length > MAX_REQUEST_BYTES as u64 {
        return Err(build_error(format!(
            "{flag} exceeds the {MAX_REQUEST_BYTES}-byte gateway request limit"
        )));
    }
    let mut bytes = Vec::new();
    file.take(MAX_REQUEST_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| build_error(format!("{flag} could not read {value}: {error}")))?;
    if bytes.len() > MAX_REQUEST_BYTES {
        return Err(build_error(format!(
            "{flag} exceeds the {MAX_REQUEST_BYTES}-byte gateway request limit"
        )));
    }
    String::from_utf8(bytes).map_err(|_| build_error(format!("{flag} must contain valid UTF-8")))
}

fn require_cli_args(
    spec: &OperationSpecDocument,
    projection: &OperationProjection,
    values: &Map<String, Value>,
) -> Result<(), RequestBuildError> {
    for arg in &spec.args {
        if arg.required && !values.contains_key(&arg.name) {
            let projected_arg = projection
                .arguments
                .iter()
                .find(|candidate| candidate.name == arg.name);
            return Err(build_error(format!(
                "{} is required for {}",
                cli_arg_name(arg, projected_arg),
                spec.name
            )));
        }
    }
    Ok(())
}

fn insert_arg_value(
    values: &mut Map<String, Value>,
    arg: &ArgSpecDocument,
    projection: &ArgumentProjection,
    value: &str,
) -> Result<(), RequestBuildError> {
    if values.contains_key(&arg.name) {
        return Err(build_error(format!(
            "{} was provided more than once",
            cli_arg_name(arg, Some(projection))
        )));
    }
    values.insert(
        arg.name.clone(),
        parse_arg_value(arg, value, cli_arg_name(arg, Some(projection)))?,
    );
    Ok(())
}

fn parse_arg_value(
    arg: &ArgSpecDocument,
    value: &str,
    name: &str,
) -> Result<Value, RequestBuildError> {
    let value = match arg.kind {
        ArgKind::String | ArgKind::Path => Ok(Value::String(value.to_owned())),
        ArgKind::Integer => value.parse::<u64>().map_or_else(
            |_| Err(build_error(format!("{name} must be an unsigned integer"))),
            |number| Ok(Value::Number(Number::from(number))),
        ),
        ArgKind::Float => {
            let parsed = value
                .parse::<f64>()
                .map_err(|_| build_error(format!("{name} must be a finite number")))?;
            Number::from_f64(parsed)
                .map(Value::Number)
                .ok_or_else(|| build_error(format!("{name} must be finite")))
        }
        ArgKind::JsonArray => serde_json::from_str::<Value>(value)
            .map_err(|_| build_error(format!("{name} must be a JSON array"))),
    }?;
    validate_arg_value(arg, value, name)
}

fn validate_arg_value(
    arg: &ArgSpecDocument,
    value: Value,
    name: &str,
) -> Result<Value, RequestBuildError> {
    let valid = match arg.kind {
        ArgKind::String | ArgKind::Path => value.is_string(),
        ArgKind::Integer => value.as_u64().is_some(),
        ArgKind::Float => value.as_f64().is_some_and(f64::is_finite),
        ArgKind::JsonArray => value.is_array(),
    };
    if valid {
        return Ok(value);
    }
    let expected = match arg.kind {
        ArgKind::String | ArgKind::Path => "a string",
        ArgKind::Integer => "an unsigned integer",
        ArgKind::Float => "a finite number",
        ArgKind::JsonArray => "a JSON array",
    };
    Err(build_error(format!("{name} must be {expected}")))
}

fn find_flag_arg<'a>(
    projection: &'a OperationProjection,
    flag: &str,
) -> Result<&'a ArgumentProjection, RequestBuildError> {
    projection
        .arguments
        .iter()
        .find(|arg| arg.accepts_flag(flag))
        .ok_or_else(|| build_error(format!("unknown flag for {}: {flag}", projection.name)))
}

fn semantic_argument<'a>(
    spec: &'a OperationSpecDocument,
    projection: &ArgumentProjection,
) -> Result<&'a ArgSpecDocument, RequestBuildError> {
    spec.args
        .iter()
        .find(|arg| arg.name == projection.name)
        .ok_or_else(|| {
            build_error(format!(
                "unknown argument for {}: {}",
                spec.name, projection.name
            ))
        })
}

fn cli_arg_name<'a>(
    arg: &'a ArgSpecDocument,
    projection: Option<&'a ArgumentProjection>,
) -> &'a str {
    projection
        .and_then(|projection| projection.flag.or(projection.positional))
        .unwrap_or(&arg.name)
}

fn build_error(message: impl Into<String>) -> RequestBuildError {
    RequestBuildError::invalid(message)
}
