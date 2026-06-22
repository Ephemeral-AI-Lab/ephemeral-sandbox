use crate::catalog::catalog_arg_kind_name;
use crate::{
    operation_execution_space_name, ArgSpecDocument, CliOperationSpecDocument,
    OperationCatalogDocument, OperationExecutionSpace, OperationFamilyDocument,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationSearchResult {
    pub name: String,
    pub family: String,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelpRenderError {
    operation_execution_space: OperationExecutionSpace,
    operation: String,
    suggestions: Vec<OperationSearchResult>,
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
        let space = operation_execution_space_name(self.operation_execution_space);
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
        write!(formatter, "  sandbox-cli {space} help")
    }
}

impl std::error::Error for HelpRenderError {}

#[must_use]
pub fn render_catalog_help(catalog: &OperationCatalogDocument) -> String {
    let space = operation_execution_space_name(catalog.operation_execution_space);
    let mut output = String::new();
    output.push_str(catalog_title(catalog.operation_execution_space));
    output.push_str("\n\n");

    for family in &catalog.families {
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
    output.push_str("  sandbox-cli ");
    output.push_str(space);
    output.push_str(" help OPERATION");
    trim_trailing_blank_lines(output)
}

pub fn render_operation_help(
    catalog: &OperationCatalogDocument,
    operation: &str,
) -> Result<String, HelpRenderError> {
    let spec = catalog
        .operations
        .iter()
        .find(|candidate| candidate.name == operation)
        .ok_or_else(|| HelpRenderError {
            operation_execution_space: catalog.operation_execution_space,
            operation: operation.to_owned(),
            suggestions: search_operation_help(catalog, operation),
        })?;
    let family = catalog
        .families
        .iter()
        .find(|candidate| candidate.id == spec.family);
    Ok(render_operation_page(family, spec))
}

#[must_use]
pub fn search_operation_help(
    catalog: &OperationCatalogDocument,
    query: &str,
) -> Vec<OperationSearchResult> {
    let query = query.trim().to_ascii_lowercase();
    if query.is_empty() {
        return Vec::new();
    }

    catalog
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
    spec: &CliOperationSpecDocument,
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

    if let Some(cli) = &spec.cli {
        output.push_str("Usage\n");
        push_indented_line(&mut output, 2, &cli.usage);
        output.push('\n');
    }

    output.push_str("Arguments\n");
    if spec.args.is_empty() {
        push_indented_line(&mut output, 2, "None");
    } else {
        for arg in &spec.args {
            push_argument(&mut output, arg);
        }
    }
    output.push('\n');

    if let Some(cli) = &spec.cli {
        if !cli.examples.is_empty() {
            output.push_str("Examples\n");
            for example in &cli.examples {
                push_indented_line(&mut output, 2, example);
            }
            output.push('\n');
        }
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

fn push_argument(output: &mut String, arg: &ArgSpecDocument) {
    push_indented_line(
        output,
        2,
        &format!(
            "{} {} {}",
            cli_arg_name(arg),
            catalog_arg_kind_name(arg.kind),
            if arg.required { "required" } else { "optional" }
        ),
    );
    push_indented_line(output, 4, &arg.help);
    if let Some(default) = &arg.default {
        push_indented_line(output, 4, &format!("Default: {default}"));
    }
    output.push('\n');
}

fn operation_matches_query(
    catalog: &OperationCatalogDocument,
    operation: &CliOperationSpecDocument,
    query: &str,
) -> bool {
    contains_query(&operation.name, query)
        || contains_query(&operation.summary, query)
        || contains_query(&operation.description, query)
        || operation
            .args
            .iter()
            .any(|arg| contains_query(&arg.name, query) || contains_query(&arg.help, query))
        || operation.cli.as_ref().is_some_and(|cli| {
            cli.examples
                .iter()
                .any(|example| contains_query(example, query))
        })
        || catalog
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
    catalog: &'a OperationCatalogDocument,
    family_id: &str,
) -> Vec<&'a CliOperationSpecDocument> {
    catalog
        .operations
        .iter()
        .filter(|operation| operation.family == family_id)
        .collect()
}

fn catalog_title(operation_execution_space: OperationExecutionSpace) -> &'static str {
    match operation_execution_space {
        OperationExecutionSpace::Manager => "Sandbox Manager Help",
        OperationExecutionSpace::Runtime => "Sandbox Runtime Help",
    }
}

fn cli_arg_name(arg: &ArgSpecDocument) -> &str {
    arg.cli
        .as_ref()
        .and_then(|cli| cli.flag.as_deref().or(cli.positional.as_deref()))
        .unwrap_or(&arg.name)
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
