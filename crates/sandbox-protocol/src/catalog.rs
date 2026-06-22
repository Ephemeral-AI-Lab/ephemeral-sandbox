use std::collections::HashSet;

use serde_json::{json, Map, Value};

use crate::{ArgCliSpec, ArgKind, ArgSpec, CliOperationSpec, CliSpec, OperationFamilySpec};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationExecutionSpace {
    Manager,
    Runtime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OperationCatalog {
    pub operation_execution_space: OperationExecutionSpace,
    pub families: &'static [&'static OperationFamilySpec],
    pub operations: &'static [&'static CliOperationSpec],
}

impl OperationCatalog {
    #[must_use]
    pub const fn new(
        operation_execution_space: OperationExecutionSpace,
        families: &'static [&'static OperationFamilySpec],
        operations: &'static [&'static CliOperationSpec],
    ) -> Self {
        Self {
            operation_execution_space,
            families,
            operations,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationCatalogDocument {
    pub operation_execution_space: OperationExecutionSpace,
    pub families: Vec<OperationFamilyDocument>,
    pub operations: Vec<CliOperationSpecDocument>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationFamilyDocument {
    pub id: String,
    pub title: String,
    pub summary: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliOperationSpecDocument {
    pub name: String,
    pub family: String,
    pub summary: String,
    pub description: String,
    pub args: Vec<ArgSpecDocument>,
    pub cli: Option<CliSpecDocument>,
    pub related: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArgSpecDocument {
    pub name: String,
    pub kind: ArgKind,
    pub required: bool,
    pub help: String,
    pub default: Option<String>,
    pub cli: Option<ArgCliSpecDocument>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArgCliSpecDocument {
    pub flag: Option<String>,
    pub positional: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliSpecDocument {
    pub path: Vec<String>,
    pub usage: String,
    pub examples: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogDecodeError {
    message: String,
}

impl CatalogDecodeError {
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for CatalogDecodeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for CatalogDecodeError {}

#[must_use]
pub fn catalog_to_value(catalog: OperationCatalog) -> Value {
    json!({
        "operation_execution_space": operation_execution_space_name(catalog.operation_execution_space),
        "families": catalog
            .families
            .iter()
            .map(|family| operation_family_value(family))
            .collect::<Vec<_>>(),
        "operations": catalog
            .operations
            .iter()
            .map(|spec| cli_operation_spec_value(spec))
            .collect::<Vec<_>>(),
    })
}

pub fn catalog_from_value(value: &Value) -> Result<OperationCatalogDocument, CatalogDecodeError> {
    let object = value
        .as_object()
        .ok_or_else(|| decode_error("operation catalog response must be an object"))?;
    let operation_execution_space =
        operation_execution_space_from_name(required_string(object, "operation_execution_space")?)?;
    let families = required_array(object, "families")?
        .iter()
        .map(operation_family_from_value)
        .collect::<Result<Vec<_>, _>>()?;
    let operations = required_array(object, "operations")?
        .iter()
        .map(cli_operation_spec_from_value)
        .collect::<Result<Vec<_>, _>>()?;
    validate_catalog(&families, &operations)?;
    Ok(OperationCatalogDocument {
        operation_execution_space,
        families,
        operations,
    })
}

#[must_use]
pub const fn operation_execution_space_name(
    operation_execution_space: OperationExecutionSpace,
) -> &'static str {
    match operation_execution_space {
        OperationExecutionSpace::Manager => "manager",
        OperationExecutionSpace::Runtime => "runtime",
    }
}

#[must_use]
pub(crate) const fn catalog_arg_kind_name(kind: ArgKind) -> &'static str {
    match kind {
        ArgKind::String => "string",
        ArgKind::Integer => "integer",
        ArgKind::Float => "float",
        ArgKind::Path => "path",
    }
}

fn cli_operation_spec_value(spec: &CliOperationSpec) -> Value {
    json!({
        "name": spec.name,
        "family": spec.family,
        "summary": spec.summary,
        "description": spec.description,
        "args": spec.args.iter().map(arg_spec_value).collect::<Vec<_>>(),
        "cli": spec.cli.map(cli_spec_value),
        "related": spec.related,
    })
}

fn operation_family_value(spec: &OperationFamilySpec) -> Value {
    json!({
        "id": spec.id,
        "title": spec.title,
        "summary": spec.summary,
        "description": spec.description,
    })
}

fn arg_spec_value(spec: &ArgSpec) -> Value {
    json!({
        "name": spec.name,
        "kind": catalog_arg_kind_name(spec.kind),
        "required": spec.required,
        "help": spec.help,
        "default": spec.default,
        "cli": spec.cli.map(arg_cli_spec_value),
    })
}

fn cli_spec_value(spec: CliSpec) -> Value {
    json!({
        "path": spec.path,
        "usage": spec.usage,
        "examples": spec.examples,
    })
}

fn arg_cli_spec_value(spec: ArgCliSpec) -> Value {
    json!({
        "flag": spec.flag,
        "positional": spec.positional,
    })
}

fn operation_family_from_value(
    value: &Value,
) -> Result<OperationFamilyDocument, CatalogDecodeError> {
    let object = value
        .as_object()
        .ok_or_else(|| decode_error("operation family spec must be an object"))?;
    Ok(OperationFamilyDocument {
        id: required_string(object, "id")?.to_owned(),
        title: required_string(object, "title")?.to_owned(),
        summary: required_string(object, "summary")?.to_owned(),
        description: required_string(object, "description")?.to_owned(),
    })
}

fn cli_operation_spec_from_value(
    value: &Value,
) -> Result<CliOperationSpecDocument, CatalogDecodeError> {
    let object = value
        .as_object()
        .ok_or_else(|| decode_error("operation spec must be an object"))?;
    let args = required_array(object, "args")?
        .iter()
        .map(arg_spec_from_value)
        .collect::<Result<Vec<_>, _>>()?;
    let cli = optional_object_value(object, "cli")?
        .map(cli_spec_from_value)
        .transpose()?;
    let related = required_string_array(object, "related", "related operation entries")?;
    Ok(CliOperationSpecDocument {
        name: required_string(object, "name")?.to_owned(),
        family: required_string(object, "family")?.to_owned(),
        summary: required_string(object, "summary")?.to_owned(),
        description: required_string(object, "description")?.to_owned(),
        args,
        cli,
        related,
    })
}

fn validate_catalog(
    families: &[OperationFamilyDocument],
    operations: &[CliOperationSpecDocument],
) -> Result<(), CatalogDecodeError> {
    let mut family_ids = HashSet::new();
    for family in families {
        if !family_ids.insert(family.id.as_str()) {
            return Err(decode_error(format!(
                "duplicate operation family id: {}",
                family.id
            )));
        }
    }

    let mut operation_names = HashSet::new();
    for operation in operations {
        if !family_ids.contains(operation.family.as_str()) {
            return Err(decode_error(format!(
                "operation {} references unknown family: {}",
                operation.name, operation.family
            )));
        }
        if !operation_names.insert(operation.name.as_str()) {
            return Err(decode_error(format!(
                "duplicate operation name: {}",
                operation.name
            )));
        }
    }

    for operation in operations {
        for related in &operation.related {
            if !operation_names.contains(related.as_str()) {
                return Err(decode_error(format!(
                    "operation {} references unknown related operation: {}",
                    operation.name, related
                )));
            }
        }
    }

    Ok(())
}

fn arg_spec_from_value(value: &Value) -> Result<ArgSpecDocument, CatalogDecodeError> {
    let object = value
        .as_object()
        .ok_or_else(|| decode_error("operation arg spec must be an object"))?;
    let cli = optional_object_value(object, "cli")?
        .map(arg_cli_spec_from_value)
        .transpose()?;
    Ok(ArgSpecDocument {
        name: required_string(object, "name")?.to_owned(),
        kind: arg_kind_from_name(required_string(object, "kind")?)?,
        required: required_bool(object, "required")?,
        help: required_string(object, "help")?.to_owned(),
        default: optional_string(object, "default")?.map(str::to_owned),
        cli,
    })
}

fn cli_spec_from_value(value: &Value) -> Result<CliSpecDocument, CatalogDecodeError> {
    let object = value
        .as_object()
        .ok_or_else(|| decode_error("operation cli spec must be an object"))?;
    let path = required_string_array(object, "path", "operation cli path entries")?;
    let examples = required_string_array(object, "examples", "operation cli examples")?;
    Ok(CliSpecDocument {
        path,
        usage: required_string(object, "usage")?.to_owned(),
        examples,
    })
}

fn arg_cli_spec_from_value(value: &Value) -> Result<ArgCliSpecDocument, CatalogDecodeError> {
    let object = value
        .as_object()
        .ok_or_else(|| decode_error("operation arg cli spec must be an object"))?;
    Ok(ArgCliSpecDocument {
        flag: optional_string(object, "flag")?.map(str::to_owned),
        positional: optional_string(object, "positional")?.map(str::to_owned),
    })
}

fn operation_execution_space_from_name(
    value: &str,
) -> Result<OperationExecutionSpace, CatalogDecodeError> {
    match value {
        "manager" => Ok(OperationExecutionSpace::Manager),
        "runtime" => Ok(OperationExecutionSpace::Runtime),
        other => Err(decode_error(format!(
            "unknown operation_execution_space: {other}"
        ))),
    }
}

fn arg_kind_from_name(value: &str) -> Result<ArgKind, CatalogDecodeError> {
    match value {
        "string" => Ok(ArgKind::String),
        "integer" => Ok(ArgKind::Integer),
        "float" => Ok(ArgKind::Float),
        "path" => Ok(ArgKind::Path),
        other => Err(decode_error(format!("unknown arg kind: {other}"))),
    }
}

fn required_array<'a>(
    object: &'a Map<String, Value>,
    field: &str,
) -> Result<&'a Vec<Value>, CatalogDecodeError> {
    object
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| decode_error(format!("{field} must be an array")))
}

fn required_string_array(
    object: &Map<String, Value>,
    field: &str,
    entry_label: &str,
) -> Result<Vec<String>, CatalogDecodeError> {
    required_array(object, field)?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| decode_error(format!("{entry_label} must be strings")))
        })
        .collect()
}

fn optional_object_value<'a>(
    object: &'a Map<String, Value>,
    field: &str,
) -> Result<Option<&'a Value>, CatalogDecodeError> {
    match object.get(field) {
        Some(Value::Null) | None => Ok(None),
        Some(value) if value.is_object() => Ok(Some(value)),
        Some(_) => Err(decode_error(format!("{field} must be an object or null"))),
    }
}

fn required_string<'a>(
    object: &'a Map<String, Value>,
    field: &str,
) -> Result<&'a str, CatalogDecodeError> {
    object
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| decode_error(format!("{field} must be a string")))
}

fn optional_string<'a>(
    object: &'a Map<String, Value>,
    field: &str,
) -> Result<Option<&'a str>, CatalogDecodeError> {
    match object.get(field) {
        Some(Value::Null) | None => Ok(None),
        Some(value) => value
            .as_str()
            .map(Some)
            .ok_or_else(|| decode_error(format!("{field} must be a string or null"))),
    }
}

fn required_bool(object: &Map<String, Value>, field: &str) -> Result<bool, CatalogDecodeError> {
    object
        .get(field)
        .and_then(Value::as_bool)
        .ok_or_else(|| decode_error(format!("{field} must be a boolean")))
}

fn decode_error(message: impl Into<String>) -> CatalogDecodeError {
    CatalogDecodeError {
        message: message.into(),
    }
}
