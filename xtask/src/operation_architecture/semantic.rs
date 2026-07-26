use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use ignore::WalkBuilder;

use super::stale::{rust_syntax_tokens, semantic_spellings};
use super::{Domain, DomainOperation, Owner, RouteFact, Scope, SemanticFacts};

const DELEGATED_PUBLIC_HANDLER_KEYS: &[&str] = &["sandbox:cgroup:observability"];

pub fn load_semantic_facts(root: &Path) -> Result<SemanticFacts> {
    let catalog_root = root.join("crates/sandbox-operations/catalog/src");
    let sources = rust_sources(&catalog_root)?;
    validate_catalog_aggregate_identity(&sources, &catalog_root)?;
    let mut facts = SemanticFacts::default();
    let mut specs = BTreeMap::new();
    let mut routed = BTreeMap::new();
    let mut parsed_specs = 0;
    let mut parsed_routes = 0;

    for (path, source) in &sources {
        let blocks = declaration_blocks(source, "OperationSpec = OperationSpec")?;
        parsed_specs += blocks.len();
        if let Some(domain) = catalog_domain(path, &catalog_root) {
            for block in blocks {
                let identifier = declaration_identifier(block)
                    .context("operation spec declaration omitted its identifier")?;
                let operation = quoted_field(block, "name")
                    .context("operation spec declaration omitted name")?;
                let domain_operation = DomainOperation { domain, operation };
                if specs
                    .insert(identifier.clone(), domain_operation.clone())
                    .is_some()
                {
                    bail!("duplicate operation spec identifier {identifier}");
                }
                facts.public_operations.push(domain_operation);
            }
        } else {
            let relative = path
                .strip_prefix(&catalog_root)
                .with_context(|| format!("classify catalog source {}", path.display()))?;
            for block in blocks {
                let identifier = declaration_identifier(block)
                    .context("operation spec declaration omitted its identifier")?;
                facts.unclassified_operation_declarations.push(format!(
                    "{}:{identifier}",
                    relative.to_string_lossy().replace('\\', "/")
                ));
            }
        }
        let route_blocks = declaration_blocks(source, "RoutedOperation = RoutedOperation")?;
        parsed_routes += route_blocks.len();
        for block in route_blocks {
            let identifier = declaration_identifier(block)
                .context("routed operation declaration omitted its identifier")?;
            let Some(domain) = catalog_domain(path, &catalog_root) else {
                facts.unwired_route_declarations.push(format!(
                    "{}:{identifier}",
                    relative_path(path, &catalog_root)?
                ));
                continue;
            };
            let spec =
                identifier_after(block, "spec: &").context("routed operation omitted its spec")?;
            let key = qualified_catalog_identifier(path, &catalog_root, domain, &identifier)?;
            if routed
                .insert(
                    key.clone(),
                    (spec, block.to_owned(), relative_path(path, &catalog_root)?),
                )
                .is_some()
            {
                bail!("duplicate routed operation declaration {key}");
            }
        }
    }
    validate_catalog_constructions(&sources, parsed_specs, parsed_routes)?;

    for (domain, module) in [
        (Domain::Manager, "manager.rs"),
        (Domain::Runtime, "runtime.rs"),
        (Domain::Observability, "observability.rs"),
    ] {
        let source = fs::read_to_string(catalog_root.join(module))
            .with_context(|| format!("read {domain} catalog aggregate"))?;
        let aggregate = const_slice_body(&source, "OPERATIONS")?
            .with_context(|| format!("{domain} catalog omitted OPERATIONS aggregate"))?;
        for key in reference_paths(aggregate) {
            let (spec, block, _) = routed.remove(&key).with_context(|| {
                format!("{domain} catalog aggregate references unknown route {key}")
            })?;
            let operation = specs
                .get(&spec)
                .with_context(|| format!("routed operation references unknown spec {spec}"))?;
            if operation.domain != domain {
                bail!(
                    "{domain} catalog aggregate includes {key} from {}",
                    operation.domain
                );
            }
            facts
                .public_routes
                .extend(expand_routing(&block, &operation.operation)?);
        }
    }
    facts.unwired_route_declarations.extend(
        routed
            .into_iter()
            .map(|(identifier, (_, _, path))| format!("{path}:{identifier}")),
    );

    let (internal_identifiers, internal_constants) =
        load_internal_routes(&catalog_root, &mut facts)?;

    load_manager_handlers(root, &specs, &mut facts)?;
    load_runtime_handlers(
        root,
        &specs,
        &internal_identifiers,
        &internal_constants,
        &mut facts,
    )?;
    load_observability_handlers(root, &specs, &mut facts)?;
    load_projections(root, &mut facts)?;
    Ok(facts)
}

pub fn validate_semantic_facts(facts: &SemanticFacts) -> Vec<String> {
    let mut violations = Vec::new();
    for declaration in &facts.unclassified_operation_declarations {
        violations.push(format!(
            "operation spec declaration is outside a domain module: {declaration}"
        ));
    }
    for declaration in &facts.unwired_route_declarations {
        violations.push(format!(
            "operation route declaration is outside its aggregate: {declaration}"
        ));
    }
    for declaration in &facts.unwired_handler_declarations {
        violations.push(format!(
            "operation handler declaration is outside its aggregate: {declaration}"
        ));
    }
    for declaration in &facts.unwired_projection_declarations {
        violations.push(format!(
            "CLI projection declaration is outside its aggregate: {declaration}"
        ));
    }
    report_duplicates(
        "public operation declaration",
        facts
            .public_operations
            .iter()
            .map(|operation| format!("{}:{}", operation.domain, operation.operation)),
        &mut violations,
    );
    report_duplicates(
        "public operation name",
        facts
            .public_operations
            .iter()
            .map(|operation| operation.operation.clone()),
        &mut violations,
    );
    report_duplicates(
        "public route",
        facts.public_routes.iter().map(scope_operation_key),
        &mut violations,
    );
    report_duplicates(
        "internal route",
        facts.internal_routes.iter().map(scope_operation_key),
        &mut violations,
    );
    report_duplicates(
        "public handler",
        facts.public_handlers.iter().map(route_key),
        &mut violations,
    );
    report_duplicates(
        "internal handler",
        facts.internal_handlers.iter().map(route_key),
        &mut violations,
    );
    report_duplicates(
        "CLI projection",
        facts
            .projections
            .iter()
            .map(|operation| format!("{}:{}", operation.domain, operation.operation)),
        &mut violations,
    );

    let operation_names = facts
        .public_operations
        .iter()
        .map(|operation| operation.operation.as_str())
        .collect::<BTreeSet<_>>();
    for operation in &facts.public_operations {
        if !facts
            .public_routes
            .iter()
            .any(|route| route.operation == operation.operation)
        {
            violations.push(format!(
                "public operation {}:{} has no route",
                operation.domain, operation.operation
            ));
        }
    }
    for route in &facts.public_routes {
        if !operation_names.contains(route.operation.as_str()) {
            violations.push(format!(
                "public route {} has no operation declaration",
                route_key(route)
            ));
        }
    }

    compare_public_handler_sets(facts, &mut violations);
    compare_route_sets(
        "internal handler",
        &facts.internal_routes,
        &facts.internal_handlers,
        &mut violations,
    );

    let public_keys = facts
        .public_routes
        .iter()
        .map(|route| (route.scope, route.operation.as_str()))
        .collect::<BTreeSet<_>>();
    for route in &facts.internal_routes {
        if public_keys.contains(&(route.scope, route.operation.as_str())) {
            violations.push(format!(
                "public/internal route overlap at {}:{}",
                route.scope, route.operation
            ));
        }
    }

    let operations = facts
        .public_operations
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let projections = facts.projections.iter().cloned().collect::<BTreeSet<_>>();
    for missing in operations.difference(&projections) {
        violations.push(format!(
            "missing CLI projection entry {}:{}",
            missing.domain, missing.operation
        ));
    }
    for extra in projections.difference(&operations) {
        violations.push(format!(
            "extra CLI projection entry {}:{}",
            extra.domain, extra.operation
        ));
    }
    violations
}

fn load_internal_routes(
    catalog_root: &Path,
    facts: &mut SemanticFacts,
) -> Result<(BTreeSet<String>, BTreeMap<String, String>)> {
    let internal_root = catalog_root.join("internal");
    let sources = rust_sources(&internal_root)?;
    let mut runtime_identifiers = BTreeSet::new();
    let mut runtime_constants = BTreeMap::new();
    for (path, source) in sources {
        let relative = relative_path(&path, &internal_root)?;
        let domain = match relative.as_str() {
            "runtime.rs" => Some(("runtime", Owner::Runtime)),
            "observability.rs" => {
                for identifier in string_constants(&source).into_keys() {
                    facts
                        .unwired_route_declarations
                        .push(format!("internal/{relative}:{identifier}"));
                }
                None
            }
            "mod.rs" => None,
            _ => {
                for identifier in string_constants(&source).into_keys() {
                    facts
                        .unwired_route_declarations
                        .push(format!("internal/{relative}:{identifier}"));
                }
                continue;
            }
        };
        let Some((domain_name, owner)) = domain else {
            continue;
        };
        let constants = string_constants(&source);
        let aggregate = const_slice_body(&source, "ROUTES")?.unwrap_or_default();
        let marker = format!("internal_{domain_name}_route(");
        let routed = identifiers_in_calls(aggregate, &marker);
        for identifier in &routed {
            let operation = constants.get(identifier).with_context(|| {
                format!("internal route references unknown operation constant {identifier}")
            })?;
            if operation == "file_list" {
                facts.unwired_route_declarations.push(format!(
                    "internal/{relative}:{identifier} is the HTTP-only exception"
                ));
                continue;
            }
            facts.internal_routes.push(RouteFact {
                operation: operation.clone(),
                scope: Scope::Sandbox,
                owner,
            });
            if owner == Owner::Runtime {
                runtime_identifiers.insert(identifier.clone());
            }
        }
        let routed = routed.into_iter().collect::<BTreeSet<_>>();
        for (identifier, operation) in &constants {
            if operation != "file_list" && !routed.contains(identifier) {
                facts
                    .unwired_route_declarations
                    .push(format!("internal/{relative}:{identifier}"));
            }
        }
        if owner == Owner::Runtime {
            runtime_constants = constants;
        }
    }
    Ok((runtime_identifiers, runtime_constants))
}

fn load_manager_handlers(
    root: &Path,
    specs: &BTreeMap<String, DomainOperation>,
    facts: &mut SemanticFacts,
) -> Result<()> {
    let manager_root = root.join("crates/sandbox-manager/src");
    let sources = rust_sources(&manager_root)?;
    let relative = "crates/sandbox-manager/src/operations/registry/management_operations.rs";
    let source =
        fs::read_to_string(root.join(relative)).context("read manager operation registry")?;
    let aggregate = const_slice_body(&source, "OPERATIONS")?
        .context("manager operation registry omitted OPERATIONS aggregate")?;
    let all = call_blocks(&source, "ManagerOperationEntry::new(")?;
    let wired = call_blocks(aggregate, "ManagerOperationEntry::new(")?;
    let constructed = sources
        .iter()
        .map(|(_, source)| semantic_method_call_count(source, "ManagerOperationEntry", "new"))
        .sum::<usize>();
    if constructed != all.len() {
        bail!(
            "unwired manager handler construction in owner tree: parsed {}, found {constructed}",
            all.len()
        );
    }
    let direct_constructions = sources
        .iter()
        .map(|(_, source)| semantic_construction_count(source, "ManagerOperationEntry"))
        .sum::<usize>();
    if direct_constructions != 0 {
        bail!(
            "manager executable entry construction census found {direct_constructions} entries outside its public registry"
        );
    }
    validate_aggregate_locations(
        &sources,
        &manager_root,
        "OPERATIONS",
        &["operations/registry/management_operations.rs"],
        "manager handler",
    )?;
    for index in wired.len()..all.len() {
        facts
            .unwired_handler_declarations
            .push(format!("{relative}:ManagerOperationEntry::new#{index}"));
    }
    for call in wired {
        let spec = identifier_after(call, "&").context("manager handler omitted operation spec")?;
        let operation = specs
            .get(&spec)
            .with_context(|| format!("manager handler references unknown spec {spec}"))?;
        let scope = if call.contains("OperationScopeKind::System") {
            Scope::System
        } else if call.contains("OperationScopeKind::Sandbox") {
            Scope::Sandbox
        } else {
            bail!("manager handler for {spec} omitted its scope");
        };
        facts.public_handlers.push(RouteFact {
            operation: operation.operation.clone(),
            scope,
            owner: Owner::Manager,
        });
    }
    Ok(())
}

fn load_runtime_handlers(
    root: &Path,
    specs: &BTreeMap<String, DomainOperation>,
    internal_identifiers: &BTreeSet<String>,
    internal_constants: &BTreeMap<String, String>,
    facts: &mut SemanticFacts,
) -> Result<()> {
    let runtime_root = root.join("crates/sandbox-runtime/operation/src");
    let sources = rust_sources(&runtime_root)?;
    let mut declarations = BTreeMap::new();
    let mut parsed_declarations = 0;
    for (path, source) in &sources {
        let calls = call_declarations(source, "OperationEntry::public(")?;
        parsed_declarations += calls.len();
        for (identifier, call) in calls {
            let relative = relative_path(path, &runtime_root)?;
            let module = path
                .file_stem()
                .and_then(|value| value.to_str())
                .context("runtime handler source omitted file stem")?;
            let key = format!("{module}::{identifier}");
            if declarations.insert(key.clone(), (call, relative)).is_some() {
                bail!("duplicate runtime handler declaration {key}");
            }
        }
    }
    let constructed = sources
        .iter()
        .map(|(_, source)| semantic_method_call_count(source, "OperationEntry", "public"))
        .sum::<usize>();
    if constructed != parsed_declarations {
        bail!(
            "unwired runtime handler construction in owner tree: parsed {parsed_declarations}, found {constructed}"
        );
    }
    let direct_constructions = sources
        .iter()
        .map(|(_, source)| semantic_construction_count(source, "OperationEntry"))
        .sum::<usize>();
    let expected_direct_constructions = internal_identifiers.len() + 1;
    if direct_constructions != expected_direct_constructions {
        bail!(
            "runtime executable entry construction census found {direct_constructions} direct entries; expected {expected_direct_constructions} canonical internal and HTTP-only entries"
        );
    }
    let registry = fs::read_to_string(runtime_root.join("operations/registry/mod.rs"))
        .context("read runtime operation registry aggregate")?;
    let groups = const_slice_body(&registry, "PUBLIC_OPERATION_ENTRY_GROUPS")?
        .context("runtime registry omitted public operation groups")?;
    for module in group_modules(groups, "public_operation_entries") {
        let source = fs::read_to_string(
            runtime_root
                .join("operations/registry")
                .join(format!("{module}.rs")),
        )
        .with_context(|| format!("read runtime public handler group {module}"))?;
        let aggregate = const_slice_body(&source, "PUBLIC_OPERATIONS")?
            .with_context(|| format!("runtime handler group {module} omitted PUBLIC_OPERATIONS"))?;
        for identifier in reference_paths(aggregate) {
            let key = format!("{module}::{identifier}");
            let (call, _) = declarations.remove(&key).with_context(|| {
                format!("runtime public handler aggregate references unknown entry {key}")
            })?;
            let spec = identifier_after(&call, "&")
                .context("runtime public handler omitted operation spec")?;
            let operation = specs
                .get(&spec)
                .with_context(|| format!("runtime handler references unknown spec {spec}"))?;
            facts.public_handlers.push(RouteFact {
                operation: operation.operation.clone(),
                scope: Scope::Sandbox,
                owner: Owner::Runtime,
            });
        }
    }
    facts.unwired_handler_declarations.extend(
        declarations
            .into_iter()
            .map(|(identifier, (_, path))| format!("{path}:{identifier}")),
    );
    let registry_groups = const_slice_body(&registry, "INTERNAL_OPERATION_ENTRY_GROUPS")?
        .context("runtime registry omitted internal operation groups")?;
    let source_by_path = sources
        .iter()
        .map(|(path, source)| Ok((relative_path(path, &runtime_root)?, source.as_str())))
        .collect::<Result<BTreeMap<_, _>>>()?;
    for identifier in internal_identifiers {
        let Some((relative, aggregate, entry, group)) = internal_handler_wiring(identifier) else {
            facts
                .unwired_handler_declarations
                .push(format!("unknown internal runtime operation {identifier}"));
            continue;
        };
        let source = source_by_path.get(relative).copied().unwrap_or_default();
        let local_aggregate = const_slice_body(source, aggregate)?.unwrap_or_default();
        let marker = format!("name: {identifier},");
        let local_count = source.matches(&marker).count();
        let total_count = sources
            .iter()
            .map(|(_, source)| source.matches(&marker).count())
            .sum::<usize>();
        if local_count == 1
            && total_count == 1
            && local_aggregate.contains(entry)
            && registry_groups.contains(group)
        {
            facts.internal_handlers.push(RouteFact {
                operation: internal_constants[identifier].clone(),
                scope: Scope::Sandbox,
                owner: Owner::Runtime,
            });
        } else {
            facts
                .unwired_handler_declarations
                .push(format!("{relative}:{entry}"));
        }
    }
    Ok(())
}

fn internal_handler_wiring(identifier: &str) -> Option<(&str, &str, &str, &str)> {
    match identifier {
        "CREATE_WORKSPACE_SESSION" => Some((
            "operations/registry/workspace_session_operations.rs",
            "INTERNAL_OPERATIONS",
            "CREATE_WORKSPACE_SESSION_ENTRY",
            "workspace_session_operations::internal_operation_entries()",
        )),
        "DESTROY_WORKSPACE_SESSION" => Some((
            "operations/registry/workspace_session_operations.rs",
            "INTERNAL_OPERATIONS",
            "DESTROY_WORKSPACE_SESSION_ENTRY",
            "workspace_session_operations::internal_operation_entries()",
        )),
        "CREATE_WORKSPACE_SESSION_LEGACY_SCRATCH_ADAPTER" => Some((
            "operations/registry/workspace_session_operations.rs",
            "INTERNAL_OPERATIONS",
            "CREATE_WORKSPACE_SESSION_LEGACY_SCRATCH_ADAPTER_ENTRY",
            "workspace_session_operations::internal_operation_entries()",
        )),
        "SQUASH_LAYERSTACK" => Some((
            "layerstack/service/impls/squash.rs",
            "OPERATIONS",
            "SQUASH_LAYERSTACK_ENTRY",
            "crate::layerstack::squash_operation_entries()",
        )),
        "EXPORT_LAYERSTACK" => Some((
            "layerstack/service/impls/export.rs",
            "OPERATIONS",
            "EXPORT_LAYERSTACK_ENTRY",
            "crate::layerstack::export_operation_entries()",
        )),
        "READ_EXPORT_CHUNK" => Some((
            "layerstack/service/impls/export.rs",
            "OPERATIONS",
            "READ_EXPORT_CHUNK_ENTRY",
            "crate::layerstack::export_operation_entries()",
        )),
        _ => None,
    }
}

fn load_observability_handlers(
    root: &Path,
    specs: &BTreeMap<String, DomainOperation>,
    facts: &mut SemanticFacts,
) -> Result<()> {
    let observability_root = root.join("crates/sandbox-observability/query/src");
    let sources = rust_sources(&observability_root)?;
    let relative = "crates/sandbox-observability/query/src/registry.rs";
    let source =
        fs::read_to_string(root.join(relative)).context("read observability operation registry")?;
    let aggregate = const_slice_body(&source, "OPERATIONS")?
        .context("observability operation registry omitted OPERATIONS aggregate")?;
    let all = call_blocks(&source, "OperationEntry::new(")?;
    let wired = call_blocks(aggregate, "OperationEntry::new(")?;
    let constructed = sources
        .iter()
        .map(|(_, source)| semantic_method_call_count(source, "OperationEntry", "new"))
        .sum::<usize>();
    if constructed != all.len() {
        bail!(
            "unwired observability handler construction in owner tree: parsed {}, found {constructed}",
            all.len()
        );
    }
    let direct_constructions = sources
        .iter()
        .map(|(_, source)| semantic_construction_count(source, "OperationEntry"))
        .sum::<usize>();
    if direct_constructions != 0 {
        bail!(
            "observability executable entry construction census found {direct_constructions} entries outside its public registry"
        );
    }
    validate_aggregate_locations(
        &sources,
        &observability_root,
        "OPERATIONS",
        &["registry.rs"],
        "observability handler",
    )?;
    for index in wired.len()..all.len() {
        facts
            .unwired_handler_declarations
            .push(format!("{relative}:OperationEntry::new#{index}"));
    }
    for call in wired {
        let spec =
            identifier_after(call, "&").context("observability handler omitted operation spec")?;
        let operation = specs
            .get(&spec)
            .with_context(|| format!("observability handler references unknown spec {spec}"))?;
        facts.public_handlers.push(RouteFact {
            operation: operation.operation.clone(),
            scope: Scope::Sandbox,
            owner: Owner::Observability,
        });
    }
    Ok(())
}

fn load_projections(root: &Path, facts: &mut SemanticFacts) -> Result<()> {
    let projection_root = root.join("crates/sandbox-cli/src/projection");
    let sources = rust_sources(&projection_root)?;
    validate_aggregate_locations(
        &sources,
        &projection_root,
        "OPERATIONS",
        &["manager.rs", "runtime.rs", "observability.rs"],
        "CLI projection",
    )?;
    let mut parsed_projections = 0;
    for (domain, file) in [
        (Domain::Manager, "manager.rs"),
        (Domain::Runtime, "runtime.rs"),
        (Domain::Observability, "observability.rs"),
    ] {
        let source = fs::read_to_string(projection_root.join(file))
            .with_context(|| format!("read {domain} CLI projection"))?;
        let aggregate = const_slice_body(&source, "OPERATIONS")?
            .with_context(|| format!("{domain} CLI projection omitted OPERATIONS aggregate"))?;
        let all = struct_blocks(&source, "OperationProjection {")?;
        parsed_projections += all.len();
        let wired = struct_blocks(aggregate, "OperationProjection {")?;
        for index in wired.len()..all.len() {
            facts
                .unwired_projection_declarations
                .push(format!("{file}:OperationProjection#{index}"));
        }
        for block in wired {
            let operation = quoted_field(block, "name")
                .with_context(|| format!("{domain} CLI projection omitted name"))?;
            facts
                .projections
                .push(DomainOperation { domain, operation });
        }
    }
    let constructed = sources
        .iter()
        .map(|(_, source)| semantic_construction_count(source, "OperationProjection"))
        .sum::<usize>();
    if constructed != parsed_projections {
        bail!(
            "unwired CLI projection construction in owner tree: parsed {parsed_projections}, found {constructed}"
        );
    }
    Ok(())
}

fn rust_sources(root: &Path) -> Result<Vec<(PathBuf, String)>> {
    let mut sources = Vec::new();
    for entry in WalkBuilder::new(root).standard_filters(false).build() {
        let entry = entry.with_context(|| format!("walk {}", root.display()))?;
        if !entry.file_type().is_some_and(|kind| kind.is_file())
            || entry.path().extension().and_then(|value| value.to_str()) != Some("rs")
        {
            continue;
        }
        let content = fs::read_to_string(entry.path())
            .with_context(|| format!("read {}", entry.path().display()))?;
        sources.push((entry.into_path(), content));
    }
    sources.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(sources)
}

fn validate_catalog_aggregate_identity(
    sources: &[(PathBuf, String)],
    catalog_root: &Path,
) -> Result<()> {
    for (domain, expected) in [
        (Domain::Manager, "manager.rs"),
        (Domain::Runtime, "runtime.rs"),
        (Domain::Observability, "observability.rs"),
    ] {
        let mut locations = Vec::new();
        for (path, source) in sources
            .iter()
            .filter(|(path, _)| catalog_domain(path, catalog_root) == Some(domain))
        {
            let relative = relative_path(path, catalog_root)?;
            locations.extend(std::iter::repeat_n(
                relative,
                const_declaration_count(source, "OPERATIONS"),
            ));
        }
        if locations.len() > 1 {
            bail!(
                "multiple {domain} catalog OPERATIONS aggregates; multiple const OPERATIONS aggregates are forbidden: {locations:?}"
            );
        }
        if locations.len() != 1 || locations.first().map(String::as_str) != Some(expected) {
            bail!(
                "{domain} catalog OPERATIONS aggregate must be declared exactly once in {expected}, found {locations:?}"
            );
        }
    }
    validate_aggregate_locations(
        sources,
        catalog_root,
        "OPERATIONS",
        &["manager.rs", "runtime.rs", "observability.rs"],
        "catalog OPERATIONS",
    )?;
    Ok(())
}

fn validate_catalog_constructions(
    sources: &[(PathBuf, String)],
    parsed_specs: usize,
    parsed_routes: usize,
) -> Result<()> {
    let constructed_specs = sources
        .iter()
        .map(|(_, source)| semantic_construction_count(source, "OperationSpec"))
        .sum::<usize>();
    if constructed_specs != parsed_specs {
        bail!(
            "unparsed OperationSpec construction in catalog owner tree: parsed {parsed_specs}, found {constructed_specs}"
        );
    }
    let constructed_routes = sources
        .iter()
        .map(|(_, source)| semantic_construction_count(source, "RoutedOperation"))
        .sum::<usize>();
    if constructed_routes != parsed_routes {
        bail!(
            "unparsed RoutedOperation construction in catalog owner tree: parsed {parsed_routes}, found {constructed_routes}"
        );
    }
    Ok(())
}

fn catalog_domain(path: &Path, catalog_root: &Path) -> Option<Domain> {
    let relative = path.strip_prefix(catalog_root).ok()?;
    if relative == Path::new("manager.rs") || in_domain_directory(relative, "manager") {
        Some(Domain::Manager)
    } else if relative == Path::new("runtime.rs") || in_domain_directory(relative, "runtime") {
        Some(Domain::Runtime)
    } else if relative == Path::new("observability.rs")
        || in_domain_directory(relative, "observability")
    {
        Some(Domain::Observability)
    } else {
        None
    }
}

fn in_domain_directory(path: &Path, domain: &str) -> bool {
    let mut components = path.components();
    components
        .next()
        .is_some_and(|component| component.as_os_str() == domain)
        && components.next().is_some()
}

fn qualified_catalog_identifier(
    path: &Path,
    catalog_root: &Path,
    domain: Domain,
    identifier: &str,
) -> Result<String> {
    let relative = path
        .strip_prefix(catalog_root)
        .with_context(|| format!("catalog source escaped root: {}", path.display()))?;
    let root_file = match domain {
        Domain::Manager => "manager.rs",
        Domain::Runtime => "runtime.rs",
        Domain::Observability => "observability.rs",
    };
    if relative == Path::new(root_file) {
        return Ok(identifier.to_owned());
    }
    let domain_name = domain.to_string();
    let nested = relative
        .strip_prefix(&domain_name)
        .with_context(|| format!("{relative:?} is not inside {domain_name}"))?;
    let mut modules = nested
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let file = modules.last_mut().context("catalog module path is empty")?;
    *file = file
        .strip_suffix(".rs")
        .context("catalog module is not Rust source")?
        .to_owned();
    modules.push(identifier.to_owned());
    Ok(modules.join("::"))
}

fn relative_path(path: &Path, root: &Path) -> Result<String> {
    Ok(path
        .strip_prefix(root)
        .with_context(|| format!("{} escaped {}", path.display(), root.display()))?
        .to_string_lossy()
        .replace('\\', "/"))
}

fn semantic_construction_count(source: &str, symbol: &str) -> usize {
    let spellings = semantic_spellings(source, symbol);
    let tokens = rust_syntax_tokens(source);
    tokens
        .iter()
        .enumerate()
        .filter(|(index, token)| {
            spellings.contains(token.as_str())
                && tokens.get(index + 1).map(String::as_str) == Some("{")
                && construction_context(&tokens, *index)
        })
        .count()
}

fn semantic_method_call_count(source: &str, symbol: &str, method: &str) -> usize {
    let spellings = semantic_spellings(source, symbol);
    let tokens = rust_syntax_tokens(source);
    tokens
        .windows(4)
        .filter(|tokens| {
            spellings.contains(tokens[0].as_str())
                && tokens[1] == "::"
                && tokens[2] == method
                && tokens[3] == "("
        })
        .count()
}

fn construction_context(tokens: &[String], index: usize) -> bool {
    let mut path_start = index;
    while path_start >= 2
        && tokens[path_start - 1] == "::"
        && tokens[path_start - 2]
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
    {
        path_start -= 2;
    }
    !path_start.checked_sub(1).is_some_and(|context| {
        matches!(
            tokens[context].as_str(),
            "struct" | "enum" | "union" | "trait" | "impl" | "for" | "->"
        )
    })
}

fn const_declaration_count(source: &str, name: &str) -> usize {
    rust_syntax_tokens(source)
        .windows(2)
        .filter(|tokens| tokens[0] == "const" && tokens[1] == name)
        .count()
}

fn validate_aggregate_locations(
    sources: &[(PathBuf, String)],
    root: &Path,
    name: &str,
    expected: &[&str],
    label: &str,
) -> Result<()> {
    let mut locations = Vec::new();
    for (path, source) in sources {
        let relative = relative_path(path, root)?;
        locations.extend(std::iter::repeat_n(
            relative,
            const_declaration_count(source, name),
        ));
    }
    locations.sort();
    let mut expected = expected
        .iter()
        .map(|path| (*path).to_owned())
        .collect::<Vec<_>>();
    expected.sort();
    if locations != expected {
        bail!("{label} aggregate locations are {locations:?}; expected {expected:?}");
    }
    Ok(())
}

fn const_slice_body<'a>(source: &'a str, name: &str) -> Result<Option<&'a str>> {
    let mut bodies = Vec::new();
    for (start, _) in source.match_indices("const") {
        let after_keyword = start + 5;
        let identifier = |character: char| character.is_ascii_alphanumeric() || character == '_';
        if source[..start].chars().next_back().is_some_and(identifier)
            || source[after_keyword..].trim_start().len() == source[after_keyword..].len()
        {
            continue;
        }
        let declaration = source[after_keyword..].trim_start();
        let actual = declaration.split(|character| !identifier(character)).next();
        if actual != Some(name) {
            continue;
        }
        let after_name = &declaration[name.len()..];
        let equals = after_name.find('=').context("missing initializer")?;
        if after_name[..equals].contains(';') {
            bail!("const {name} omitted initializer");
        }
        let initializer = &after_name[equals + 1..];
        let open = initializer.find('[').context("initializer is not slice")?;
        let open = source.len() - initializer.len() + open;
        let close = matching_delimiter(source, open, '[', ']').context("unterminated slice")?;
        bodies.push(&source[open + 1..close]);
    }
    match bodies.as_slice() {
        [] => Ok(None),
        [body] => Ok(Some(body)),
        _ => bail!("multiple const {name} aggregates"),
    }
}

fn reference_paths(source: &str) -> Vec<String> {
    source
        .lines()
        .map(|line| line.split("//").next().unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n")
        .split(',')
        .filter_map(|item| {
            let identifier = item.trim().trim_start_matches('&').trim();
            (!identifier.is_empty()
                && identifier
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || "_:".contains(character)))
            .then(|| identifier.to_owned())
        })
        .collect()
}

fn group_modules(source: &str, function: &str) -> Vec<String> {
    let suffix = format!("::{function}()");
    source
        .split(',')
        .filter_map(|item| item.trim().strip_suffix(&suffix).map(str::to_owned))
        .collect()
}

fn call_declarations(source: &str, marker: &str) -> Result<Vec<(String, String)>> {
    let mut declarations = Vec::new();
    let mut cursor = 0;
    while let Some((start, after_marker)) = find_ignoring_whitespace(source, cursor, marker) {
        let declaration = source[..start]
            .rfind("const ")
            .context("operation entry constructor is not a const declaration")?;
        let boundary = source[..start]
            .rfind([';', '}'])
            .map_or(0, |position| position + 1);
        if declaration < boundary {
            bail!("operation entry constructor is outside a const declaration");
        }
        let identifier = source[declaration + "const ".len()..]
            .split_once(':')
            .map(|(identifier, _)| identifier.trim())
            .filter(|identifier| !identifier.is_empty())
            .context("operation entry const omitted identifier")?
            .to_owned();
        let open = source[..after_marker]
            .rfind('(')
            .context("operation entry constructor omitted arguments")?;
        let end = matching_delimiter(source, open, '(', ')')
            .with_context(|| format!("{marker} has unclosed arguments"))?;
        declarations.push((identifier, source[start..=end].to_owned()));
        cursor = end + 1;
    }
    Ok(declarations)
}

fn declaration_blocks<'a>(source: &'a str, marker: &str) -> Result<Vec<&'a str>> {
    let mut blocks = Vec::new();
    let mut cursor = 0;
    while let Some((marker_start, after_marker)) = find_ignoring_whitespace(source, cursor, marker)
    {
        let start = source[..marker_start]
            .rfind('\n')
            .map_or(0, |index| index + 1);
        let open = source[after_marker..]
            .find('{')
            .map(|index| after_marker + index)
            .with_context(|| format!("{marker} declaration omitted body"))?;
        let end = matching_delimiter(source, open, '{', '}')
            .with_context(|| format!("{marker} declaration has unclosed body"))?;
        blocks.push(&source[start..=end]);
        cursor = end + 1;
    }
    Ok(blocks)
}

fn struct_blocks<'a>(source: &'a str, marker: &str) -> Result<Vec<&'a str>> {
    let mut blocks = Vec::new();
    let mut cursor = 0;
    while let Some((_, after_marker)) = find_ignoring_whitespace(source, cursor, marker) {
        let open = source[..after_marker]
            .rfind('{')
            .with_context(|| format!("{marker} omitted body"))?;
        let end = matching_delimiter(source, open, '{', '}')
            .with_context(|| format!("{marker} has unclosed body"))?;
        blocks.push(&source[open..=end]);
        cursor = end + 1;
    }
    Ok(blocks)
}

fn call_blocks<'a>(source: &'a str, marker: &str) -> Result<Vec<&'a str>> {
    let mut blocks = Vec::new();
    let mut cursor = 0;
    while let Some((start, after_marker)) = find_ignoring_whitespace(source, cursor, marker) {
        let open = source[..after_marker]
            .rfind('(')
            .with_context(|| format!("{marker} omitted arguments"))?;
        let end = matching_delimiter(source, open, '(', ')')
            .with_context(|| format!("{marker} has unclosed arguments"))?;
        blocks.push(&source[start..=end]);
        cursor = end + 1;
    }
    Ok(blocks)
}

fn find_ignoring_whitespace(source: &str, cursor: usize, marker: &str) -> Option<(usize, usize)> {
    let pattern = marker
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<Vec<_>>();
    let first = *pattern.first()?;
    for (offset, candidate) in source[cursor..].char_indices() {
        if candidate != first {
            continue;
        }
        let start = cursor + offset;
        let mut matched = 0;
        for (inner, character) in source[start..].char_indices() {
            if character.is_whitespace() {
                continue;
            }
            if pattern.get(matched) != Some(&character) {
                break;
            }
            matched += 1;
            if matched == pattern.len() {
                return Some((start, start + inner + character.len_utf8()));
            }
        }
    }
    None
}

fn matching_delimiter(source: &str, open: usize, opening: char, closing: char) -> Option<usize> {
    let mut depth = 0;
    let mut quoted = false;
    let mut escaped = false;
    for (offset, character) in source[open..].char_indices() {
        if quoted {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                quoted = false;
            }
            continue;
        }
        if character == '"' {
            quoted = true;
        } else if character == opening {
            depth += 1;
        } else if character == closing {
            depth -= 1;
            if depth == 0 {
                return Some(open + offset);
            }
        }
    }
    None
}

fn declaration_identifier(block: &str) -> Option<String> {
    block
        .split(':')
        .next()?
        .split_whitespace()
        .last()
        .map(str::to_owned)
}

fn quoted_field(block: &str, field: &str) -> Option<String> {
    let marker = format!("{field}:");
    let rest = block.split_once(&marker)?.1;
    let start = rest.find('"')? + 1;
    let end = rest[start..].find('"')? + start;
    Some(rest[start..end].to_owned())
}

fn identifier_after(block: &str, marker: &str) -> Option<String> {
    let rest = block.split_once(marker)?.1.trim_start();
    let length = rest
        .chars()
        .take_while(|character| character.is_ascii_alphanumeric() || *character == '_')
        .map(char::len_utf8)
        .sum();
    (length > 0).then(|| rest[..length].to_owned())
}

fn expand_routing(block: &str, operation: &str) -> Result<Vec<RouteFact>> {
    if block.contains("routing: MANAGER_OWNED") {
        return Ok(vec![RouteFact {
            operation: operation.to_owned(),
            scope: Scope::System,
            owner: Owner::Manager,
        }]);
    }
    if block.contains("routing: RUNTIME_OWNED") {
        return Ok(vec![RouteFact {
            operation: operation.to_owned(),
            scope: Scope::Sandbox,
            owner: Owner::Runtime,
        }]);
    }
    if block.contains("Routing::SystemOrSandbox") {
        let system = owner_after(block, "system: OperationExecutionOwner::")
            .context("system-or-sandbox route omitted system owner")?;
        let sandbox = owner_after(block, "sandbox: OperationExecutionOwner::")
            .context("system-or-sandbox route omitted sandbox owner")?;
        return Ok(vec![
            RouteFact {
                operation: operation.to_owned(),
                scope: Scope::System,
                owner: system,
            },
            RouteFact {
                operation: operation.to_owned(),
                scope: Scope::Sandbox,
                owner: sandbox,
            },
        ]);
    }
    if block.contains("Routing::System(") {
        let owner = owner_after(block, "Routing::System(OperationExecutionOwner::")
            .context("system route omitted owner")?;
        return Ok(vec![RouteFact {
            operation: operation.to_owned(),
            scope: Scope::System,
            owner,
        }]);
    }
    if block.contains("Routing::Sandbox(") {
        let owner = owner_after(block, "Routing::Sandbox(OperationExecutionOwner::")
            .context("sandbox route omitted owner")?;
        return Ok(vec![RouteFact {
            operation: operation.to_owned(),
            scope: Scope::Sandbox,
            owner,
        }]);
    }
    bail!("operation {operation} has unsupported routing declaration")
}

fn owner_after(block: &str, marker: &str) -> Option<Owner> {
    let owner = identifier_after(block, marker)?;
    match owner.as_str() {
        "Manager" => Some(Owner::Manager),
        "Runtime" => Some(Owner::Runtime),
        "Observability" => Some(Owner::Observability),
        _ => None,
    }
}

fn string_constants(source: &str) -> BTreeMap<String, String> {
    let mut constants = BTreeMap::new();
    for (start, declaration) in source.match_indices("pub const ") {
        let rest = &source[start + declaration.len()..];
        let identifier_len = rest
            .chars()
            .take_while(|character| character.is_ascii_alphanumeric() || *character == '_')
            .map(char::len_utf8)
            .sum::<usize>();
        if identifier_len == 0 {
            continue;
        }
        let identifier = &rest[..identifier_len];
        let Some(after_type) = rest[identifier_len..]
            .trim_start()
            .strip_prefix(':')
            .map(str::trim_start)
            .and_then(|rest| rest.strip_prefix("&str"))
        else {
            continue;
        };
        let Some(value) = after_type
            .trim_start()
            .strip_prefix('=')
            .map(str::trim_start)
            .and_then(|rest| rest.strip_prefix('"'))
        else {
            continue;
        };
        let Some(end) = value.find('"') else {
            continue;
        };
        let value = &value[..end];
        constants.insert(identifier.to_owned(), value.to_owned());
    }
    constants
}

fn identifiers_in_calls(source: &str, marker: &str) -> Vec<String> {
    source
        .match_indices(marker)
        .filter_map(|(index, _)| identifier_after(&source[index..], marker))
        .collect()
}

fn route_key(route: &RouteFact) -> String {
    format!("{}:{}:{}", route.scope, route.operation, route.owner)
}

fn scope_operation_key(route: &RouteFact) -> String {
    format!("{}:{}", route.scope, route.operation)
}

fn report_duplicates(
    label: &str,
    values: impl Iterator<Item = String>,
    violations: &mut Vec<String>,
) {
    let mut counts = BTreeMap::new();
    for value in values {
        *counts.entry(value).or_insert(0) += 1;
    }
    for (value, count) in counts {
        if count > 1 {
            violations.push(format!("duplicate {label} {value} ({count} declarations)"));
        }
    }
}

fn compare_route_sets(
    label: &str,
    expected: &[RouteFact],
    actual: &[RouteFact],
    violations: &mut Vec<String>,
) {
    let expected = expected.iter().map(route_key).collect::<BTreeSet<_>>();
    let actual = actual.iter().map(route_key).collect::<BTreeSet<_>>();
    for missing in expected.difference(&actual) {
        violations.push(format!("missing {label} for {missing}"));
    }
    for extra in actual.difference(&expected) {
        violations.push(format!("extra {label} for {extra}"));
    }
}

fn compare_public_handler_sets(facts: &SemanticFacts, violations: &mut Vec<String>) {
    let mut expected = facts
        .public_routes
        .iter()
        .map(route_key)
        .collect::<BTreeSet<_>>();
    expected.extend(
        DELEGATED_PUBLIC_HANDLER_KEYS
            .iter()
            .map(|key| (*key).to_owned()),
    );
    let actual = facts
        .public_handlers
        .iter()
        .map(route_key)
        .collect::<BTreeSet<_>>();
    for missing in expected.difference(&actual) {
        violations.push(format!("missing public handler for {missing}"));
    }
    for extra in actual.difference(&expected) {
        violations.push(format!("extra public handler for {extra}"));
    }
}
