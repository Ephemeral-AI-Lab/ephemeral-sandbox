use crate::{ArgCliSpec, ArgKind, ArgSpec, OperationSpec};

#[must_use]
pub fn arg_kind_name(kind: ArgKind) -> &'static str {
    match kind {
        ArgKind::String => "string",
        ArgKind::Integer => "integer",
        ArgKind::Float => "float",
        ArgKind::Path => "path",
    }
}

#[must_use]
pub fn cli_arg_name(arg: &ArgSpec) -> &'static str {
    match arg.cli {
        Some(ArgCliSpec {
            flag: Some(flag), ..
        }) => flag,
        Some(ArgCliSpec {
            positional: Some(positional),
            ..
        }) => positional,
        _ => arg.name,
    }
}

#[must_use]
pub fn operation_usage(spec: &OperationSpec) -> Option<&'static str> {
    spec.cli.map(|cli| cli.usage)
}

#[must_use]
pub fn operation_examples(spec: &OperationSpec) -> &'static [&'static str] {
    spec.cli.map_or(&[], |cli| cli.examples)
}
