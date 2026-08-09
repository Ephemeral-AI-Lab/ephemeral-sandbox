use sandbox_operation_contract::document::arg_kind_name;
use sandbox_operation_contract::{
    operation_domain_name, ArgSpecDocument, OperationDomain, OperationFamilyDocument,
    OperationSpecDocument,
};

use crate::projection::document::{argument_projection, operation_projection, CatalogDocument};
use crate::projection::{ArgumentProjection, OperationProjection};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationSearchResult {
    pub name: String,
    pub family: String,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelpRenderError {
    operation_execution_space: OperationDomain,
    operation: String,
    suggestions: Vec<OperationSearchResult>,
    program: String,
}

impl HelpRenderError {
    #[must_use]
    pub fn operation(&self) -> &str {
        &self.operation
    }

    #[must_use]
    pub fn suggestions(&self) -> &[OperationSearchResult] {
        &self.suggestions
    }
}

impl std::fmt::Display for HelpRenderError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let space = operation_domain_name(self.operation_execution_space);
        writeln!(
            formatter,
            "unknown {space} operation for help: {}",
            self.operation
        )?;
        if !self.suggestions.is_empty() {
            writeln!(formatter)?;
            writeln!(formatter, "Did you mean:")?;
            for suggestion in &self.suggestions {
                writeln!(formatter, "  {}", suggestion.name)?;
                writeln!(formatter, "    {}", suggestion.summary)?;
            }
        }
        writeln!(formatter)?;
        writeln!(formatter, "Use:")?;
        write!(formatter, "  {} OPERATION", self.program)
    }
}

impl std::error::Error for HelpRenderError {}

#[must_use]
pub fn render_catalog_help(catalog: &CatalogDocument, program: &str) -> String {
    let mut output = String::new();
    output.push_str(catalog_title(catalog.semantic.operation_execution_space));
    output.push_str("\n\n");

    for family in &catalog.semantic.families {
        output.push_str(&family.title);
        output.push('\n');
        push_indented_line(&mut output, 2, &family.summary);
        output.push('\n');

        for operation in operations_for_family(catalog, &family.id) {
            push_indented_line(&mut output, 2, &operation.name);
            push_indented_line(&mut output, 4, &operation.summary);
            output.push('\n');
        }
    }

    output.push_str("Use:\n");
    output.push_str("  ");
    output.push_str(program);
    output.push_str(" OPERATION");
    trim_trailing_blank_lines(output)
}

pub fn render_operation_help(
    catalog: &CatalogDocument,
    operation: &str,
    program: &str,
) -> Result<String, HelpRenderError> {
    let spec = catalog
        .semantic
        .operations
        .iter()
        .find(|candidate| candidate.name == operation)
        .ok_or_else(|| help_error(catalog, operation, program))?;
    let cli = operation_projection(catalog, operation)
        .ok_or_else(|| help_error(catalog, operation, program))?;
    let family = catalog
        .semantic
        .families
        .iter()
        .find(|candidate| candidate.id == spec.family);
    Ok(render_operation_page(family, spec, cli))
}

#[must_use]
pub fn search_operation_help(catalog: &CatalogDocument, query: &str) -> Vec<OperationSearchResult> {
    let query = query.trim().to_ascii_lowercase();
    if query.is_empty() {
        return Vec::new();
    }

    catalog
        .semantic
        .operations
        .iter()
        .filter(|operation| operation_matches_query(catalog, operation, &query))
        .map(|operation| OperationSearchResult {
            name: operation.name.clone(),
            family: operation.family.clone(),
            summary: operation.summary.clone(),
        })
        .collect()
}

fn render_operation_page(
    family: Option<&OperationFamilyDocument>,
    spec: &OperationSpecDocument,
    cli: &OperationProjection,
) -> String {
    let mut output = String::new();
    output.push_str(&spec.name);
    output.push_str("\n\n");

    output.push_str("Family\n");
    push_indented_line(
        &mut output,
        2,
        family.map_or(spec.family.as_str(), |family| family.title.as_str()),
    );
    output.push('\n');

    output.push_str("Description\n");
    push_indented_line(&mut output, 2, &spec.description);
    output.push('\n');

    output.push_str("Usage\n");
    push_indented_line(&mut output, 2, cli.usage);
    output.push('\n');

    output.push_str("Arguments\n");
    if spec.args.is_empty() {
        push_indented_line(&mut output, 2, "None");
    } else {
        for arg in &spec.args {
            push_argument(&mut output, arg, argument_projection(cli, &arg.name));
        }
    }
    output.push('\n');

    if !cli.examples.is_empty() {
        output.push_str("Examples\n");
        for example in cli.examples {
            push_indented_line(&mut output, 2, example);
        }
        output.push('\n');
    }

    if !spec.related.is_empty() {
        output.push_str("Related Operations\n");
        for related in &spec.related {
            push_indented_line(&mut output, 2, related);
        }
        output.push('\n');
    }

    trim_trailing_blank_lines(output)
}

fn push_argument(output: &mut String, arg: &ArgSpecDocument, cli: Option<&ArgumentProjection>) {
    push_indented_line(
        output,
        2,
        &format!(
            "{} {} {}",
            cli_arg_name(arg, cli),
            arg_kind_name(arg.kind),
            if arg.required { "required" } else { "optional" }
        ),
    );
    push_indented_line(output, 4, &arg.help);
    if let Some(default) = &arg.default {
        push_indented_line(output, 4, &format!("Default: {default}"));
    }
    if let Some(value_file_flag) = cli.and_then(|cli| cli.value_file_flag) {
        push_indented_line(
            output,
            4,
            &format!("Alternatively, {value_file_flag} FILE reads the value as UTF-8."),
        );
    }
    output.push('\n');
}

fn operation_matches_query(
    catalog: &CatalogDocument,
    operation: &OperationSpecDocument,
    query: &str,
) -> bool {
    contains_query(&operation.name, query)
        || contains_query(&operation.summary, query)
        || contains_query(&operation.description, query)
        || operation
            .args
            .iter()
            .any(|arg| contains_query(&arg.name, query) || contains_query(&arg.help, query))
        || operation_projection(catalog, &operation.name).is_some_and(|cli| {
            cli.examples
                .iter()
                .any(|example| contains_query(example, query))
        })
        || catalog
            .semantic
            .families
            .iter()
            .find(|family| family.id == operation.family)
            .is_some_and(|family| {
                contains_query(&family.title, query)
                    || contains_query(&family.summary, query)
                    || contains_query(&family.description, query)
            })
}

fn operations_for_family<'a>(
    catalog: &'a CatalogDocument,
    family_id: &str,
) -> Vec<&'a OperationSpecDocument> {
    catalog
        .semantic
        .operations
        .iter()
        .filter(|operation| operation.family == family_id)
        .collect()
}

fn catalog_title(operation_execution_space: OperationDomain) -> &'static str {
    match operation_execution_space {
        OperationDomain::Manager => "Sandbox Manager Help",
        OperationDomain::Runtime => "Sandbox Runtime Help",
        OperationDomain::Observability => "Sandbox Observability Help",
    }
}

fn cli_arg_name<'a>(arg: &'a ArgSpecDocument, cli: Option<&'a ArgumentProjection>) -> &'a str {
    cli.and_then(|cli| cli.flag.or(cli.positional))
        .unwrap_or(&arg.name)
}

fn help_error(catalog: &CatalogDocument, operation: &str, program: &str) -> HelpRenderError {
    HelpRenderError {
        operation_execution_space: catalog.semantic.operation_execution_space,
        operation: operation.to_owned(),
        suggestions: search_operation_help(catalog, operation),
        program: program.to_owned(),
    }
}

fn contains_query(value: &str, query: &str) -> bool {
    value.to_ascii_lowercase().contains(query)
}

fn push_indented_line(output: &mut String, spaces: usize, line: &str) {
    output.push_str(&" ".repeat(spaces));
    output.push_str(line);
    output.push('\n');
}

fn trim_trailing_blank_lines(mut value: String) -> String {
    while value.ends_with('\n') {
        value.pop();
    }
    value.push('\n');
    value
}
