use sandbox_protocol::{
    catalog_from_value, catalog_to_value, ArgKind, CliOperationSpecDocument, OperationCatalog,
    OperationCatalogDocument, OperationExecutionSpace, OperationScope, Request,
};
use serde_json::{Map, Number, Value};

use crate::cli::config::GatewayConfig;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildRequestInput {
    pub execution_space: OperationExecutionSpace,
    pub operation: String,
    pub operation_argv: Vec<String>,
    pub sandbox_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestBuildError {
    message: String,
}

impl RequestBuildError {
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for RequestBuildError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for RequestBuildError {}

pub fn manager_catalog_document() -> Result<OperationCatalogDocument, RequestBuildError> {
    catalog_document(sandbox_manager::operation_catalog())
}

pub fn runtime_catalog_document() -> Result<OperationCatalogDocument, RequestBuildError> {
    catalog_document(sandbox_runtime::operation_catalog())
}

pub fn build_request_from_catalog(
    input: BuildRequestInput,
    config: &GatewayConfig,
    catalog: &OperationCatalogDocument,
) -> Result<Request, RequestBuildError> {
    build_request_from_catalog_with_id(input, config, catalog, next_request_id())
}

pub fn build_request_from_catalog_with_id(
    input: BuildRequestInput,
    config: &GatewayConfig,
    catalog: &OperationCatalogDocument,
    request_id: impl Into<String>,
) -> Result<Request, RequestBuildError> {
    if input.execution_space != catalog.operation_execution_space {
        return Err(build_error(format!(
            "loaded catalog is for {}, not {}",
            sandbox_protocol::operation_execution_space_name(catalog.operation_execution_space),
            sandbox_protocol::operation_execution_space_name(input.execution_space)
        )));
    }
    if input.operation == "help" {
        return Err(build_error(
            "help is reserved and cannot be used as an operation name",
        ));
    }
    let spec = find_cli_operation_spec(catalog, &input.operation)?;
    let args = build_args(spec, &input.operation_argv)?;
    let scope = match input.execution_space {
        OperationExecutionSpace::Manager => OperationScope::system(),
        OperationExecutionSpace::Runtime => {
            OperationScope::sandbox(resolve_runtime_sandbox_id(input.sandbox_id, config)?)
        }
    };

    Ok(Request::new(&spec.name, request_id, scope, args))
}

pub fn resolve_runtime_sandbox_id(
    sandbox_id: Option<String>,
    config: &GatewayConfig,
) -> Result<String, RequestBuildError> {
    let sandbox_id = sandbox_id
        .or_else(|| config.default_sandbox_id.clone())
        .ok_or_else(|| {
            build_error("runtime operations require --sandbox-id or SANDBOX_DEFAULT_ID")
        })?;
    if sandbox_id.trim().is_empty() {
        Err(build_error("runtime sandbox id must be non-empty"))
    } else {
        Ok(sandbox_id)
    }
}

fn catalog_document(
    catalog: OperationCatalog,
) -> Result<OperationCatalogDocument, RequestBuildError> {
    catalog_from_value(&catalog_to_value(catalog)).map_err(|error| build_error(error.message()))
}

fn build_args(
    spec: &CliOperationSpecDocument,
    argv: &[String],
) -> Result<Value, RequestBuildError> {
    let mut values = Map::new();
    let positional_args = spec
        .args
        .iter()
        .filter(|arg| {
            arg.cli
                .as_ref()
                .and_then(|cli| cli.positional.as_ref())
                .is_some()
        })
        .collect::<Vec<_>>();
    let mut next_positional = 0usize;
    let mut index = 0usize;

    while index < argv.len() {
        let token = &argv[index];
        if token.starts_with("--") {
            let arg = find_flag_arg(spec, token)?;
            index = index.saturating_add(1);
            let value = argv
                .get(index)
                .ok_or_else(|| build_error(format!("{token} requires a value")))?;
            insert_arg_value(&mut values, arg, value)?;
        } else {
            let arg = positional_args.get(next_positional).ok_or_else(|| {
                build_error(format!(
                    "unexpected positional argument for {}: {token}",
                    spec.name
                ))
            })?;
            next_positional = next_positional.saturating_add(1);
            insert_arg_value(&mut values, arg, token)?;
        }
        index = index.saturating_add(1);
    }

    for arg in &spec.args {
        if values.contains_key(&arg.name) {
            continue;
        }
        if let Some(default) = &arg.default {
            values.insert(arg.name.clone(), parse_arg_value(arg, default)?);
        } else if arg.required {
            return Err(build_error(format!(
                "{} is required for {}",
                cli_arg_name(arg),
                spec.name
            )));
        }
    }

    Ok(Value::Object(values))
}

fn insert_arg_value(
    values: &mut Map<String, Value>,
    arg: &sandbox_protocol::ArgSpecDocument,
    value: &str,
) -> Result<(), RequestBuildError> {
    if values.contains_key(&arg.name) {
        return Err(build_error(format!(
            "{} was provided more than once",
            cli_arg_name(arg)
        )));
    }
    values.insert(arg.name.clone(), parse_arg_value(arg, value)?);
    Ok(())
}

fn parse_arg_value(
    arg: &sandbox_protocol::ArgSpecDocument,
    value: &str,
) -> Result<Value, RequestBuildError> {
    match arg.kind {
        ArgKind::String | ArgKind::Path => Ok(Value::String(value.to_owned())),
        ArgKind::Integer => value.parse::<u64>().map_or_else(
            |_| {
                Err(build_error(format!(
                    "{} must be an unsigned integer",
                    cli_arg_name(arg)
                )))
            },
            |number| Ok(Value::Number(Number::from(number))),
        ),
        ArgKind::Float => {
            let parsed = value.parse::<f64>().map_err(|_| {
                build_error(format!("{} must be a finite number", cli_arg_name(arg)))
            })?;
            Number::from_f64(parsed)
                .map(Value::Number)
                .ok_or_else(|| build_error(format!("{} must be finite", cli_arg_name(arg))))
        }
    }
}

fn find_flag_arg<'a>(
    spec: &'a CliOperationSpecDocument,
    flag: &str,
) -> Result<&'a sandbox_protocol::ArgSpecDocument, RequestBuildError> {
    spec.args
        .iter()
        .find(|arg| arg.cli.as_ref().and_then(|cli| cli.flag.as_deref()) == Some(flag))
        .ok_or_else(|| build_error(format!("unknown flag for {}: {flag}", spec.name)))
}

fn find_cli_operation_spec<'a>(
    catalog: &'a OperationCatalogDocument,
    operation: &str,
) -> Result<&'a CliOperationSpecDocument, RequestBuildError> {
    catalog
        .operations
        .iter()
        .find(|spec| spec.name == operation)
        .ok_or_else(|| build_error(format!("unknown operation: {operation}")))
}

fn cli_arg_name(arg: &sandbox_protocol::ArgSpecDocument) -> &str {
    arg.cli
        .as_ref()
        .and_then(|cli| cli.flag.as_deref().or(cli.positional.as_deref()))
        .unwrap_or(&arg.name)
}

fn next_request_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn build_error(message: impl Into<String>) -> RequestBuildError {
    RequestBuildError {
        message: message.into(),
    }
}
