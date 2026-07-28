use std::borrow::Cow;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path};
use std::process::Command;

use anyhow::{bail, Context, Result};
use ignore::WalkBuilder;

use super::metadata::expected_crate_manifests;
use super::{StaleFacts, TrackedSource};

const DIRECT_CRATE_CHILDREN: &[&str] = &[
    "sandbox-cli",
    "sandbox-config",
    "sandbox-daemon",
    "sandbox-gateway",
    "sandbox-manager",
    "sandbox-mcp",
    "sandbox-observability",
    "sandbox-operations",
    "sandbox-protocol",
    "sandbox-provider-docker",
    "sandbox-runtime",
];

const STALE_REFERENCES: &[&str] = &[
    concat!("sandbox_operation_", "core"),
    concat!("sandbox_operation_", "adapters"),
    concat!("sandbox-manager-", "operations"),
    concat!("sandbox-runtime-", "operations"),
    concat!("sandbox-observability-", "operations"),
    concat!("sandbox-manager-operation-", "catalog"),
    concat!("sandbox-runtime-operation-", "catalog"),
    concat!("sandbox-observability-operation-", "catalog"),
    concat!("sandbox_manager_", "operations"),
    concat!("sandbox_runtime_", "operations"),
    concat!("sandbox_observability_", "operations"),
    concat!("sandbox_manager_operation_", "catalog"),
    concat!("sandbox_runtime_operation_", "catalog"),
    concat!("sandbox_observability_operation_", "catalog"),
    concat!("sandbox_cli::", "core"),
    concat!("crates/sandbox-observability/", "Cargo.toml"),
    concat!("crates/sandbox-observability/", "src"),
    concat!("crates/sandbox-runtime/", "Cargo.toml"),
    concat!("crates/sandbox-runtime/", "src"),
    concat!("internal::", "migration"),
    concat!("internal/", "migration"),
    concat!("mod ", "migration;"),
    concat!("migration::", "resolve"),
    concat!("migration_", "resolver"),
    concat!("resolve_", "observability"),
    concat!("get_", "observability"),
    concat!("cli_", "definition"),
    concat!("cli_", "metadata"),
    concat!("Cli", "Operation"),
    concat!("Cli", "Spec"),
    concat!("ArgCli", "Spec"),
    concat!("sandbox-cli ", "manager"),
    concat!("sandbox-cli ", "runtime"),
    concat!("sandbox-cli ", "observability"),
    concat!("view=", "snapshot"),
    concat!("view-", "generic"),
    concat!("private daemon ", "snapshot"),
];

const STALE_PATH_REFERENCES: &[&str] = &[
    concat!("cli-operation-e2e-", "live-test"),
    concat!("crates/sandbox-operation-", "core"),
    concat!("crates/sandbox-operation-", "adapters"),
    concat!("crates/sandbox-cli/src/", "core"),
    concat!("crates/sandbox-manager/src/", "operation"),
    concat!("crates/sandbox-operations/", "manager"),
    concat!("crates/sandbox-operations/", "runtime"),
    concat!("crates/sandbox-operations/", "observability"),
];

const CANONICAL_LITERALS: &[&str] = &[
    "squash_layerstack",
    "export_layerstack",
    "read_export_chunk",
    "file_list",
    "sandbox_daemon_ready",
];

const FROZEN_SCRIPT_MARKER: &str =
    "# FROZEN HISTORICAL ARTIFACT (operation-layout exempt, 2026-07-11).";
const HISTORICAL_HTML_PATH: &str = "docs/observability-rework/cli-observability.html";
const HISTORICAL_HTML_MARKER: &str =
    "<p><strong>Historical rendered artifact (operation-layout exempt, 2026-07-11):</strong>";
const WTUNING_DIRECTORY: &str = "docs/obsidian/ephemeral-os/implementation_plan/squash/experiments/performance-parallelization/perf-20260703-052525/wtuning";
const WTUNING_RESULTS: &str = "docs/obsidian/ephemeral-os/implementation_plan/squash/experiments/performance-parallelization/perf-20260703-052525/RESULTS.md";
const WTUNING_MARKER: &str =
    "> **Historical experiment snapshot (operation-layout exempt, 2026-07-11):**";

pub fn load_stale_facts(root: &Path) -> Result<StaleFacts> {
    let output = Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(root)
        .output()
        .context("list tracked files")?;
    if !output.status.success() {
        bail!(
            "git ls-files failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let mut files = Vec::new();
    for raw_path in output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        let path = String::from_utf8(raw_path.to_vec()).context("tracked path is not UTF-8")?;
        let absolute = root.join(&path);
        let content = fs::read(&absolute).with_context(|| format!("read tracked file {path}"))?;
        files.push(TrackedSource {
            path,
            content: String::from_utf8_lossy(&content).into_owned(),
            executable: executable(&absolute)?,
        });
    }
    Ok(StaleFacts { files })
}

pub fn validate_stale_facts(root: &Path, facts: &StaleFacts) -> Vec<String> {
    let mut violations = validate_tree(root);
    let measurement_exemptions = measurement_exemptions(facts, &mut violations);
    for file in &facts.files {
        for pattern in STALE_PATH_REFERENCES {
            if path_is_at_or_below(&file.path, pattern) {
                violations.push(format!("stale tracked path {}", file.path));
            }
        }
        if generated_path(&file.path) {
            violations.push(format!("tracked generated or stale path {}", file.path));
        }
        let Some(content) =
            reference_scan_content(file, measurement_exemptions.contains(&file.path))
        else {
            continue;
        };
        for pattern in STALE_REFERENCES {
            if content.contains(pattern) {
                violations.push(format!(
                    "stale reference {pattern:?} remains in {}",
                    file.path
                ));
            }
        }
        for pattern in STALE_PATH_REFERENCES {
            if contains_path_reference(content.as_ref(), pattern) {
                violations.push(format!(
                    "stale path reference {pattern:?} remains in {}",
                    file.path
                ));
            }
        }
        for line in content.lines() {
            let lower = line.to_ascii_lowercase();
            let vocabulary = concat!("operation ", "vocabulary");
            let protocol = concat!("sandbox-", "protocol");
            if lower.contains(vocabulary) && lower.contains(protocol) {
                violations.push(format!(
                    "stale protocol operation-vocabulary ownership remains in {}",
                    file.path
                ));
            }
        }
    }
    validate_projection_ownership(facts, &mut violations);
    violations.extend(super::source_boundaries::validate(facts));
    validate_handler_ownership(facts, &mut violations);
    validate_canonical_literals(facts, &mut violations);
    validate_observability_routing(facts, &mut violations);
    validate_visibility_proofs(facts, &mut violations);
    violations
}

fn path_is_at_or_below(path: &str, root: &str) -> bool {
    path == root
        || path
            .strip_prefix(root)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn contains_path_reference(content: &str, pattern: &str) -> bool {
    content.match_indices(pattern).any(|(start, _)| {
        let before = content[..start].chars().next_back();
        let after = content[start + pattern.len()..].chars().next();
        !before.is_some_and(path_identifier_character)
            && !after.is_some_and(path_identifier_character)
    })
}

fn path_identifier_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
}

fn validate_tree(root: &Path) -> Vec<String> {
    let mut violations = Vec::new();
    for forbidden in [
        concat!(
            "crates/sandbox-operations/catalog/src/internal/",
            "migration.rs"
        ),
        concat!(
            "crates/sandbox-operations/catalog/src/internal/",
            "migration"
        ),
    ] {
        if root.join(forbidden).exists() {
            violations.push(format!("forbidden legacy tree exists: {forbidden}"));
        }
    }
    for forbidden in STALE_PATH_REFERENCES {
        if root.join(forbidden).exists() {
            violations.push(format!("forbidden legacy tree exists: {forbidden}"));
        }
    }
    compare_children(root, "crates", DIRECT_CRATE_CHILDREN, &mut violations);
    compare_children(
        root,
        "crates/sandbox-operations",
        &["catalog", "client", "contract"],
        &mut violations,
    );
    compare_children(
        root,
        "crates/sandbox-observability",
        &["README.md", "query", "telemetry"],
        &mut violations,
    );
    match fs::symlink_metadata(root.join("crates/sandbox-observability/README.md")) {
        Ok(metadata) if metadata.file_type().is_file() => {}
        Ok(_) => violations.push(
            "observability namespace README must be a regular file: crates/sandbox-observability/README.md"
                .to_owned(),
        ),
        Err(error) => violations.push(format!(
            "could not inspect observability namespace README: {error}"
        )),
    }
    compare_children(
        root,
        "crates/sandbox-runtime",
        &[
            "layerstack",
            "layerstack-core",
            "mpla-poc",
            "namespace-execution",
            "namespace-process",
            "operation",
            "overlay",
            "workspace",
        ],
        &mut violations,
    );
    for namespace in [
        "crates/sandbox-operations",
        "crates/sandbox-observability",
        "crates/sandbox-runtime",
    ] {
        for forbidden in ["Cargo.toml", "src"] {
            if root.join(namespace).join(forbidden).exists() {
                violations.push(format!(
                    "namespace root {namespace} must not contain {forbidden}"
                ));
            }
        }
    }
    match crate_manifests(root) {
        Ok(actual) => {
            let expected = expected_crate_manifests()
                .into_iter()
                .map(str::to_owned)
                .collect::<BTreeSet<_>>();
            for missing in expected.difference(&actual) {
                violations.push(format!("required package manifest is missing: {missing}"));
            }
            for extra in actual.difference(&expected) {
                violations.push(format!(
                    "unexpected package manifest under crates/: {extra}"
                ));
            }
        }
        Err(error) => violations.push(format!("could not inspect crate manifests: {error:#}")),
    }
    violations
}

fn compare_children(root: &Path, relative: &str, expected: &[&str], violations: &mut Vec<String>) {
    let expected = expected
        .iter()
        .map(|value| (*value).to_owned())
        .collect::<BTreeSet<_>>();
    match visible_children(&root.join(relative)) {
        Ok(actual) => {
            for missing in expected.difference(&actual) {
                violations.push(format!("{relative} is missing child {missing}"));
            }
            for extra in actual.difference(&expected) {
                violations.push(format!("{relative} has forbidden child {extra}"));
            }
            for child in actual.intersection(&expected) {
                match fs::symlink_metadata(root.join(relative).join(child)) {
                    Ok(metadata) if metadata.file_type().is_symlink() => violations.push(format!(
                        "target tree child must not be a compatibility symlink: {relative}/{child}"
                    )),
                    Ok(_) => {}
                    Err(error) => violations.push(format!(
                        "could not inspect target tree child {relative}/{child}: {error}"
                    )),
                }
            }
        }
        Err(error) => violations.push(format!("could not inspect {relative}: {error:#}")),
    }
}

fn visible_children(path: &Path) -> Result<BTreeSet<String>> {
    let mut children = BTreeSet::new();
    for entry in fs::read_dir(path).with_context(|| format!("read {}", path.display()))? {
        let entry = entry.with_context(|| format!("read entry in {}", path.display()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name != ".DS_Store" {
            children.insert(name);
        }
    }
    Ok(children)
}

fn crate_manifests(root: &Path) -> Result<BTreeSet<String>> {
    let mut manifests = BTreeSet::new();
    let crates = root.join("crates");
    for entry in WalkBuilder::new(&crates).standard_filters(false).build() {
        let entry = entry.with_context(|| format!("walk {}", crates.display()))?;
        if entry.file_type().is_some_and(|kind| kind.is_file()) && entry.file_name() == "Cargo.toml"
        {
            let relative = entry
                .path()
                .strip_prefix(root)
                .context("crate manifest escaped repository root")?;
            manifests.insert(path_string(relative));
        }
    }
    Ok(manifests)
}

fn generated_path(path: &str) -> bool {
    let generated_component = Path::new(path).components().any(|component| {
        let Component::Normal(value) = component else {
            return false;
        };
        matches!(
            value.to_str(),
            Some(
                "target"
                    | "dist"
                    | "cache"
                    | ".cache"
                    | "__pycache__"
                    | ".pytest_cache"
                    | "test-reports"
            )
        )
    });
    generated_component
        || path.ends_with(".pyc")
        || path.starts_with(concat!("cli-operation-e2e-", "live-test/"))
}

fn reference_scan_content(file: &TrackedSource, exempt: bool) -> Option<Cow<'_, str>> {
    if exempt || historical_document(file) {
        return None;
    }
    let ranges = historical_section_ranges(file);
    if ranges.is_empty() {
        return Some(Cow::Borrowed(&file.content));
    }
    let mut maintained = String::with_capacity(file.content.len());
    let mut cursor = 0;
    for (start, end) in ranges {
        maintained.push_str(&file.content[cursor..start]);
        maintained.push('\n');
        cursor = end;
    }
    maintained.push_str(&file.content[cursor..]);
    Some(Cow::Owned(maintained))
}

fn measurement_exemptions(facts: &StaleFacts, violations: &mut Vec<String>) -> BTreeSet<String> {
    let marker_valid = facts
        .files
        .iter()
        .find(|file| file.path == WTUNING_RESULTS)
        .is_some_and(|file| file.content.lines().any(|line| line == WTUNING_MARKER));
    let prefix = format!("{WTUNING_DIRECTORY}/");
    let measurements = facts
        .files
        .iter()
        .filter_map(|file| {
            let name = file.path.strip_prefix(&prefix)?;
            (!name.contains('/') && name.ends_with(".json")).then(|| file.path.clone())
        })
        .collect::<BTreeSet<_>>();
    if !marker_valid && !measurements.is_empty() {
        violations.push(format!(
            "historical wtuning measurements require the exact marker in {WTUNING_RESULTS}"
        ));
        return BTreeSet::new();
    }
    measurements
}

fn historical_document(file: &TrackedSource) -> bool {
    if file.executable {
        return false;
    }
    if (file.path.ends_with(".sh") || file.path.ends_with(".py"))
        && file.content.lines().nth(1) == Some(FROZEN_SCRIPT_MARKER)
    {
        return true;
    }
    if file.path == HISTORICAL_HTML_PATH
        && file
            .content
            .lines()
            .any(|line| line == HISTORICAL_HTML_MARKER)
    {
        return true;
    }
    if !file.path.ends_with(".md") {
        return false;
    }
    let header = file
        .content
        .lines()
        .take_while(|line| !line.starts_with("## "))
        .collect::<Vec<_>>();
    header.iter().any(|line| {
        let trimmed = line.trim();
        let lower = trimmed.to_ascii_lowercase();
        lower.starts_with("status: superseded")
            || lower.starts_with("status: archived")
            || lower.starts_with("# archived ")
            || lower.starts_with("# frozen historical artifact (operation-layout exempt,")
            || whole_historical_marker(&lower)
    }) || wrapped_whole_historical_marker(&header)
}

fn whole_historical_marker(line: &str) -> bool {
    let classes = [
        "> **completed implementation record ",
        "> **completed pre-migration implementation record ",
        "> **frozen historical ",
        "> **historical append-only evidence ",
        "> **historical experiment specification ",
        "> **historical execution prompt ",
        "> **historical handoff ",
        "> **historical implementation record ",
        "> **historical implementation specification ",
        "> **historical operation-layout exemption ",
        "> **historical rendered examples ",
        "> **historical review record ",
        "> **historical review specification ",
        "> **landed design record ",
        "> **superseded design record ",
    ];
    classes.iter().any(|class| line.starts_with(class))
        && (line.contains("(operation-layout exempt, 2026-07-11)")
            || line.starts_with("> **historical operation-layout exemption (2026-07-11)"))
}

fn wrapped_whole_historical_marker(lines: &[&str]) -> bool {
    for (index, line) in lines.iter().enumerate() {
        let mut candidate = line.trim().to_ascii_lowercase();
        if !candidate.starts_with("> **") {
            continue;
        }
        for continuation in &lines[index + 1..] {
            let Some(continuation) = continuation.trim().strip_prefix("> ") else {
                break;
            };
            candidate.push(' ');
            candidate.push_str(&continuation.to_ascii_lowercase());
            if whole_historical_marker(&candidate) {
                return true;
            }
        }
    }
    false
}

fn historical_section_ranges(file: &TrackedSource) -> Vec<(usize, usize)> {
    if file.executable || !file.path.ends_with(".md") {
        return Vec::new();
    }
    let lines = file
        .content
        .split_inclusive('\n')
        .scan(0, |offset, line| {
            let start = *offset;
            *offset += line.len();
            Some((start, line))
        })
        .collect::<Vec<_>>();
    let mut ranges = Vec::new();
    let mut index = 0;
    let mut heading = 1;
    while index < lines.len() {
        if let Some(level) = markdown_heading_level(lines[index].1) {
            heading = level;
        }
        let trimmed = lines[index].1.trim();
        if !bounded_historical_marker(&trimmed.to_ascii_lowercase()) {
            index += 1;
            continue;
        }
        let end_index = ((index + 1)..lines.len())
            .find(|candidate| {
                markdown_heading_level(lines[*candidate].1).is_some_and(|level| level <= heading)
            })
            .unwrap_or(lines.len());
        let end = lines
            .get(end_index)
            .map_or(file.content.len(), |(offset, _)| *offset);
        ranges.push((lines[index].0, end));
        index = end_index;
    }
    ranges
}

fn bounded_historical_marker(line: &str) -> bool {
    let classes = [
        "> **historical decision record ",
        "> **historical implementation estimate ",
        "> **historical implementation map ",
        "> **historical measurement record ",
        "> **historical review record ",
        "> **historical sign-off record ",
    ];
    classes.iter().any(|class| line.starts_with(class))
        && (line.contains("(operation-layout exempt, 2026-07-11)")
            || line.starts_with("> **historical decision record (operation-layout exempt):**"))
}

fn markdown_heading_level(line: &str) -> Option<usize> {
    let trimmed = line.trim_start();
    let level = trimmed
        .chars()
        .take_while(|character| *character == '#')
        .count();
    (level > 0 && level <= 6 && trimmed.as_bytes().get(level) == Some(&b' ')).then_some(level)
}

fn validate_projection_ownership(facts: &StaleFacts, violations: &mut Vec<String>) {
    let core_roots = [
        "crates/sandbox-operations/contract/src/",
        "crates/sandbox-operations/catalog/src/",
        "crates/sandbox-manager/src/",
        "crates/sandbox-runtime/operation/src/",
        "crates/sandbox-observability/query/src/",
    ];
    for file in facts
        .files
        .iter()
        .filter(|file| is_production_source(file) && file.path.ends_with(".rs"))
    {
        let compact = compact_rust_code(&file.content);
        for semantic in ["OperationSpec", "RoutedOperation", "OperationProjection"] {
            if semantic_spellings(&file.content, semantic).len() > 1 {
                violations.push(format!(
                    "semantic architecture type {semantic} is aliased in {}",
                    file.path
                ));
            }
        }
        if core_roots.iter().any(|root| file.path.starts_with(root)) {
            for forbidden in [
                concat!("Cli", "Spec"),
                concat!("ArgCli", "Spec"),
                concat!("Cli", "Operation"),
                concat!("cli_", "definition"),
                concat!("cli_", "metadata"),
            ] {
                if compact.contains(forbidden) {
                    violations.push(format!(
                        "CLI metadata identifier {forbidden} escaped into {}",
                        file.path
                    ));
                }
            }
        }
        let projection_owner = file.path.starts_with("crates/sandbox-cli/src/projection/");
        if !projection_owner {
            for projection in [
                "OperationProjection",
                "ArgumentProjection",
                "CatalogProjection",
            ] {
                for spelling in semantic_spellings(&file.content, projection) {
                    if compact.contains(&format!("struct{spelling}{{"))
                        || compact.contains(&format!("{spelling}{{"))
                    {
                        violations.push(format!(
                            "CLI {projection} definition or value escaped into {}",
                            file.path
                        ));
                    }
                }
            }
        }
        let catalog_owner = file
            .path
            .starts_with("crates/sandbox-operations/catalog/src/");
        let contract_owner = file
            .path
            .starts_with("crates/sandbox-operations/contract/src/");
        if !catalog_owner
            && semantic_spellings(&file.content, "OperationSpec")
                .iter()
                .any(|spelling| constructs_type(&file.content, spelling))
        {
            violations.push(format!(
                "semantic public operation declaration escaped into {}",
                file.path
            ));
        }
        if !contract_owner && compact.contains("structOperationSpec{") {
            violations.push(format!(
                "OperationSpec type definition escaped into {}",
                file.path
            ));
        }
        if contract_owner {
            for structure in ["OperationSpec", "ArgSpec"] {
                for body in structure_bodies(&file.content, structure) {
                    for field in ["cli", "flag", "positional", "path", "usage", "examples"] {
                        if body
                            .lines()
                            .any(|line| struct_field_name(line) == Some(field))
                        {
                            violations.push(format!(
                                "contract {structure} retains CLI field {field} in {}",
                                file.path
                            ));
                        }
                    }
                }
            }
        }
    }
}

fn imported_spellings(source: &str, symbol: &str) -> BTreeSet<String> {
    let mut spellings = BTreeSet::from([symbol.to_owned()]);
    let marker = format!("{symbol}as");
    for (start, _) in source.match_indices(&marker) {
        if source[..start]
            .chars()
            .next_back()
            .is_some_and(|character| character.is_ascii_alphanumeric() || character == '_')
        {
            continue;
        }
        let alias = source[start + marker.len()..]
            .chars()
            .take_while(|character| character.is_ascii_alphanumeric() || *character == '_')
            .collect::<String>();
        if !alias.is_empty() {
            spellings.insert(alias);
        }
    }
    spellings
}

pub(super) fn semantic_spellings(source: &str, symbol: &str) -> BTreeSet<String> {
    let source = compact_rust_code(source);
    let mut spellings = imported_spellings(&source, symbol);
    loop {
        let mut discovered = BTreeSet::new();
        for (start, _) in source.match_indices("type") {
            if source[..start]
                .chars()
                .next_back()
                .is_some_and(|character| character.is_ascii_alphanumeric() || character == '_')
                && !source[..start].ends_with("pub")
            {
                continue;
            }
            let alias = source[start + "type".len()..]
                .chars()
                .take_while(|character| character.is_ascii_alphanumeric() || *character == '_')
                .collect::<String>();
            if alias.is_empty() {
                continue;
            }
            let remainder = &source[start + "type".len() + alias.len()..];
            let Some(end) = remainder.find(';') else {
                continue;
            };
            let Some((_, target)) = remainder[..end].split_once('=') else {
                continue;
            };
            if spellings
                .iter()
                .any(|spelling| contains_identifier(target, spelling))
            {
                discovered.insert(alias);
            }
        }
        let previous = spellings.len();
        spellings.extend(discovered);
        if spellings.len() == previous {
            return spellings;
        }
    }
}

fn contains_identifier(source: &str, identifier: &str) -> bool {
    source.match_indices(identifier).any(|(start, _)| {
        let before = source[..start].chars().next_back();
        let after = source[start + identifier.len()..].chars().next();
        !before.is_some_and(|character| character.is_ascii_alphanumeric() || character == '_')
            && !after.is_some_and(|character| character.is_ascii_alphanumeric() || character == '_')
    })
}

fn constructs_type(source: &str, spelling: &str) -> bool {
    let tokens = rust_syntax_tokens(source);
    tokens.iter().enumerate().any(|(index, token)| {
        if token != spelling || tokens.get(index + 1).map(String::as_str) != Some("{") {
            return false;
        }
        let mut path_start = index;
        while path_start >= 2
            && tokens[path_start - 1] == "::"
            && tokens[path_start - 2]
                .bytes()
                .next()
                .is_some_and(identifier_start)
        {
            path_start -= 2;
        }
        !path_start.checked_sub(1).is_some_and(|context| {
            matches!(
                tokens[context].as_str(),
                "struct" | "enum" | "union" | "trait" | "impl" | "for" | "->"
            )
        })
    })
}

pub(super) fn rust_syntax_tokens(source: &str) -> Vec<String> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index..].starts_with(b"//") {
            index = source[index..]
                .find('\n')
                .map_or(bytes.len(), |offset| index + offset + 1);
            continue;
        }
        if bytes[index..].starts_with(b"/*") {
            index = block_comment_end(bytes, index);
            continue;
        }
        if matches!(bytes[index], b'b' | b'c') {
            if bytes.get(index + 1) == Some(&b'\"') {
                index = quoted_literal(source, index + 1).map_or(index + 1, |(end, _)| end);
                continue;
            }
            if bytes.get(index + 1) == Some(&b'r') {
                if let Some((end, _)) = raw_literal(source, index + 1) {
                    index = end;
                    continue;
                }
            }
        }
        if bytes[index] == b'\"' {
            index = quoted_literal(source, index).map_or(index + 1, |(end, _)| end);
            continue;
        }
        if bytes[index] == b'r' {
            if let Some((end, _)) = raw_literal(source, index) {
                index = end;
                continue;
            }
        }
        if identifier_start(bytes[index]) {
            let start = index;
            index += 1;
            while bytes
                .get(index)
                .is_some_and(|byte| identifier_continue(*byte))
            {
                index += 1;
            }
            tokens.push(source[start..index].to_owned());
            continue;
        }
        if bytes[index..].starts_with(b"::") || bytes[index..].starts_with(b"->") {
            tokens.push(source[index..index + 2].to_owned());
            index += 2;
            continue;
        }
        if !bytes[index].is_ascii_whitespace() {
            tokens.push((bytes[index] as char).to_string());
        }
        index += 1;
    }
    tokens
}

pub(super) fn compact_rust_code(source: &str) -> String {
    rust_syntax_tokens(source).concat()
}

fn identifier_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

fn identifier_continue(byte: u8) -> bool {
    identifier_start(byte) || byte.is_ascii_digit()
}

fn validate_handler_ownership(facts: &StaleFacts, violations: &mut Vec<String>) {
    for file in facts.files.iter().filter(|file| is_production_source(file)) {
        if file.content.contains("ManagerOperationEntry")
            && !file.path.starts_with("crates/sandbox-manager/src/")
        {
            violations.push(format!(
                "manager handler ownership escaped into {}",
                file.path
            ));
        }
        if file.content.contains("OperationEntry::public(")
            && !file
                .path
                .starts_with("crates/sandbox-runtime/operation/src/")
        {
            violations.push(format!(
                "runtime handler ownership escaped into {}",
                file.path
            ));
        }
        let compact = file.content.split_whitespace().collect::<String>();
        if compact.contains("OperationEntry::new(")
            && compact.contains("_SPEC")
            && !file
                .path
                .starts_with("crates/sandbox-observability/query/src/")
            && !file.path.starts_with("crates/sandbox-manager/src/")
        {
            violations.push(format!(
                "observability handler ownership escaped into {}",
                file.path
            ));
        }
    }
}

fn validate_canonical_literals(facts: &StaleFacts, violations: &mut Vec<String>) {
    let sources = facts
        .files
        .iter()
        .filter(|file| is_production_source(file) && file.path.ends_with(".rs"))
        .collect::<Vec<_>>();
    for literal in CANONICAL_LITERALS {
        let count = sources
            .iter()
            .map(|file| rust_string_literal_count(&file.content, literal))
            .sum::<usize>();
        if count != 1 {
            violations.push(format!(
                "canonical literal {literal:?} must occur once in production source, found {count}"
            ));
        }
        let owner = if *literal == "sandbox_daemon_ready" {
            "crates/sandbox-protocol/src/handshake.rs"
        } else {
            "crates/sandbox-operations/catalog/src/internal/runtime.rs"
        };
        let owner_count = sources
            .iter()
            .filter(|file| file.path == owner)
            .map(|file| rust_string_literal_count(&file.content, literal))
            .sum::<usize>();
        if owner_count != 1 {
            violations.push(format!(
                "canonical literal {literal:?} must be owned once by {owner}, found {owner_count}"
            ));
        }
    }
}

fn rust_string_literal_count(source: &str, expected: &str) -> usize {
    let bytes = source.as_bytes();
    let mut count = 0;
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index..].starts_with(b"//") {
            index = source[index..]
                .find('\n')
                .map_or(bytes.len(), |offset| index + offset + 1);
            continue;
        }
        if bytes[index..].starts_with(b"/*") {
            index = block_comment_end(bytes, index);
            continue;
        }
        if let Some((end, matches)) = literal_concat_count(source, index, expected) {
            count += matches;
            index = end;
            continue;
        }
        if matches!(bytes[index], b'b' | b'c') {
            if bytes.get(index + 1) == Some(&b'\"') {
                index = quoted_literal(source, index + 1).map_or(index + 1, |(end, _)| end);
                continue;
            }
            if bytes.get(index + 1) == Some(&b'r') {
                if let Some((end, _)) = raw_literal(source, index + 1) {
                    index = end;
                    continue;
                }
            }
        }
        if bytes[index] == b'\"' {
            if let Some((end, content)) = quoted_literal(source, index) {
                count += usize::from(
                    decode_quoted_literal(content).is_some_and(|value| value == expected),
                );
                index = end;
                continue;
            }
        }
        if bytes[index] == b'r' {
            if let Some((end, content)) = raw_literal(source, index) {
                count += usize::from(content == expected);
                index = end;
                continue;
            }
        }
        index += 1;
    }
    count
}

fn literal_concat_count(source: &str, start: usize, expected: &str) -> Option<(usize, usize)> {
    let bytes = source.as_bytes();
    let name = b"concat";
    if !bytes.get(start..)?.starts_with(name)
        || start
            .checked_sub(1)
            .and_then(|index| bytes.get(index))
            .is_some_and(|byte| identifier_continue(*byte))
        || bytes
            .get(start + name.len())
            .is_some_and(|byte| identifier_continue(*byte))
    {
        return None;
    }
    let mut index = start + name.len();
    skip_rust_trivia(source, &mut index);
    if bytes.get(index) != Some(&b'!') {
        return None;
    }
    index += 1;
    skip_rust_trivia(source, &mut index);
    let closing = match bytes.get(index)? {
        b'(' => b')',
        b'[' => b']',
        b'{' => b'}',
        _ => return None,
    };
    index += 1;
    let mut value = String::new();
    let mut direct_matches = 0;
    loop {
        skip_rust_trivia(source, &mut index);
        if bytes.get(index) == Some(&closing) {
            return (index > start + name.len() + 2).then(|| {
                let matches =
                    direct_matches + usize::from(value == expected && direct_matches == 0);
                (index + 1, matches)
            });
        }
        let (end, component) = concat_component(source, index)?;
        direct_matches += usize::from(component == expected);
        value.push_str(&component);
        index = end;
        skip_rust_trivia(source, &mut index);
        match bytes.get(index) {
            Some(byte) if *byte == closing => {
                let matches =
                    direct_matches + usize::from(value == expected && direct_matches == 0);
                return Some((index + 1, matches));
            }
            Some(b',') => index += 1,
            _ => return None,
        }
    }
}

fn concat_component(source: &str, start: usize) -> Option<(usize, String)> {
    match source.as_bytes().get(start)? {
        b'\"' => {
            let (end, content) = quoted_literal(source, start)?;
            Some((end, decode_quoted_literal(content)?))
        }
        b'r' => {
            let (end, content) = raw_literal(source, start)?;
            Some((end, content.to_owned()))
        }
        b'\'' => {
            let (end, content) = quoted_char_literal(source, start)?;
            let value = decode_quoted_literal(content)?;
            (value.chars().count() == 1).then_some((end, value))
        }
        _ => None,
    }
}

fn decode_quoted_literal(content: &str) -> Option<String> {
    let mut decoded = String::with_capacity(content.len());
    let mut characters = content.chars().peekable();
    while let Some(character) = characters.next() {
        if character != '\\' {
            decoded.push(character);
            continue;
        }
        match characters.next()? {
            '\\' => decoded.push('\\'),
            '\"' => decoded.push('\"'),
            '\'' => decoded.push('\''),
            'n' => decoded.push('\n'),
            'r' => decoded.push('\r'),
            't' => decoded.push('\t'),
            '0' => decoded.push('\0'),
            'x' => {
                let high = characters.next()?.to_digit(16)?;
                let low = characters.next()?.to_digit(16)?;
                decoded.push(char::from_u32(high * 16 + low).filter(char::is_ascii)?);
            }
            'u' => {
                if characters.next()? != '{' {
                    return None;
                }
                let mut digits = String::new();
                loop {
                    match characters.next()? {
                        '}' => break,
                        '_' => {}
                        digit if digit.is_ascii_hexdigit() => digits.push(digit),
                        _ => return None,
                    }
                }
                if digits.is_empty() || digits.len() > 6 {
                    return None;
                }
                decoded.push(char::from_u32(u32::from_str_radix(&digits, 16).ok()?)?);
            }
            '\n' => {
                while characters
                    .peek()
                    .is_some_and(|character| character.is_whitespace())
                {
                    characters.next();
                }
            }
            '\r' if characters.next()? == '\n' => {
                while characters
                    .peek()
                    .is_some_and(|character| character.is_whitespace())
                {
                    characters.next();
                }
            }
            _ => return None,
        }
    }
    Some(decoded)
}

fn skip_rust_trivia(source: &str, index: &mut usize) {
    let bytes = source.as_bytes();
    loop {
        while bytes.get(*index).is_some_and(u8::is_ascii_whitespace) {
            *index += 1;
        }
        if bytes
            .get(*index..)
            .is_some_and(|rest| rest.starts_with(b"//"))
        {
            *index = source[*index..]
                .find('\n')
                .map_or(bytes.len(), |offset| *index + offset + 1);
        } else if bytes
            .get(*index..)
            .is_some_and(|rest| rest.starts_with(b"/*"))
        {
            *index = block_comment_end(bytes, *index);
        } else {
            return;
        }
    }
}

fn block_comment_end(source: &[u8], start: usize) -> usize {
    let mut depth = 1;
    let mut index = start + 2;
    while index < source.len() && depth > 0 {
        if source[index..].starts_with(b"/*") {
            depth += 1;
            index += 2;
        } else if source[index..].starts_with(b"*/") {
            depth -= 1;
            index += 2;
        } else {
            index += 1;
        }
    }
    index
}

fn quoted_literal(source: &str, start: usize) -> Option<(usize, &str)> {
    let bytes = source.as_bytes();
    let mut index = start + 1;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => index += 2,
            b'\"' => return Some((index + 1, &source[start + 1..index])),
            _ => index += 1,
        }
    }
    None
}

fn quoted_char_literal(source: &str, start: usize) -> Option<(usize, &str)> {
    let bytes = source.as_bytes();
    let mut index = start + 1;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => index += 2,
            b'\'' => return Some((index + 1, &source[start + 1..index])),
            _ => index += 1,
        }
    }
    None
}

fn raw_literal(source: &str, start: usize) -> Option<(usize, &str)> {
    let bytes = source.as_bytes();
    let mut opening_quote = start + 1;
    while bytes.get(opening_quote) == Some(&b'#') {
        opening_quote += 1;
    }
    if bytes.get(opening_quote) != Some(&b'\"') {
        return None;
    }
    let hashes = opening_quote - start - 1;
    let mut closing_quote = opening_quote + 1;
    while closing_quote < bytes.len() {
        if bytes[closing_quote] == b'\"'
            && bytes
                .get(closing_quote + 1..closing_quote + 1 + hashes)
                .is_some_and(|suffix| suffix.iter().all(|byte| *byte == b'#'))
        {
            return Some((
                closing_quote + 1 + hashes,
                &source[opening_quote + 1..closing_quote],
            ));
        }
        closing_quote += 1;
    }
    None
}

fn validate_observability_routing(facts: &StaleFacts, violations: &mut Vec<String>) {
    let request_roots = [
        "crates/sandbox-cli/src/",
        "crates/sandbox-mcp/src/",
        "crates/sandbox-manager/src/",
        "crates/sandbox-daemon/src/rpc/",
    ];
    for file in facts.files.iter().filter(|file| {
        request_roots.iter().any(|root| file.path.starts_with(root)) && is_production_source(file)
    }) {
        let legacy_multiplexer = concat!("get_", "observability");
        if file.content.contains(legacy_multiplexer) {
            violations.push(format!(
                "legacy observability multiplexer remains in {}",
                file.path
            ));
        }
        let compact = file
            .content
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let generic_view_argument = ["args.insert(\"view\"", "args[\"view\"]", "\"view\":"]
            .iter()
            .any(|pattern| compact.contains(pattern));
        let lower_path = file.path.to_ascii_lowercase();
        let lower_content = file.content.to_ascii_lowercase();
        let observability_context = lower_path.contains("observability")
            || lower_content.contains("observability")
            || lower_content.contains("operationdomain::observability");
        let synthetic = observability_context && generic_view_argument;
        if synthetic {
            violations.push(format!(
                "synthetic observability view routing remains in {}",
                file.path
            ));
        }
    }
}

fn validate_visibility_proofs(facts: &StaleFacts, violations: &mut Vec<String>) {
    let manager = facts
        .files
        .iter()
        .find(|file| file.path == "crates/sandbox-manager/tests/manager_router.rs")
        .map(|file| file.content.as_str())
        .unwrap_or_default();
    let manager_test =
        "manager_router_rejects_internal_routes_while_public_export_uses_direct_daemon_port";
    let manager_body = active_test_body(manager, manager_test);
    for proof in [
        "for route in internal::runtime::ROUTES",
        "EXPORT_CHANGES_SPEC.name",
        "internal::runtime::EXPORT_LAYERSTACK",
        "internal::runtime::READ_EXPORT_CHUNK",
        "std::fs::read(&destination)",
    ] {
        if !manager_body.is_some_and(|body| body.contains(proof)) {
            violations.push(format!("manager visibility proof is missing: {proof}"));
        }
    }
    if manager_body.is_none() {
        violations.push(format!(
            "manager visibility proof is ignored or not an active test: {manager_test}"
        ));
    }
}

fn active_test_body<'a>(source: &'a str, name: &str) -> Option<&'a str> {
    let async_signature = format!("async fn {name}(");
    let sync_signature = format!("fn {name}(");
    let position = source
        .find(&async_signature)
        .or_else(|| source.find(&sync_signature))?;
    let attributes = source[..position]
        .rsplit_once("\n\n")
        .map_or(&source[..position], |(_, block)| block);
    if !(attributes.contains("#[test]") || attributes.contains("#[tokio::test]"))
        || attributes.contains("#[ignore")
    {
        return None;
    }
    let open = source[position..].find('{')? + position;
    let close = matching_brace(source, open)?;
    Some(&source[open + 1..close])
}

fn is_production_source(file: &TrackedSource) -> bool {
    (file.path.starts_with("crates/") && file.path.contains("/src/") && file.path.ends_with(".rs"))
        || (file.path.starts_with("crates/") && file.path.ends_with("/build.rs"))
}

fn path_string(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn structure_bodies<'a>(source: &'a str, name: &str) -> Vec<&'a str> {
    let mut bodies = Vec::new();
    let mut cursor = 0;
    while let Some(relative) = source[cursor..].find("struct ") {
        let start = cursor + relative;
        let identifier_start = start + "struct ".len();
        let identifier_len = source[identifier_start..]
            .chars()
            .take_while(|character| character.is_ascii_alphanumeric() || *character == '_')
            .map(char::len_utf8)
            .sum::<usize>();
        let identifier = &source[identifier_start..identifier_start + identifier_len];
        let Some(relative_open) = source[identifier_start + identifier_len..].find('{') else {
            break;
        };
        let open = identifier_start + identifier_len + relative_open;
        let Some(close) = matching_brace(source, open) else {
            break;
        };
        if identifier == name {
            bodies.push(&source[open + 1..close]);
        }
        cursor = close + 1;
    }
    bodies
}

fn matching_brace(source: &str, open: usize) -> Option<usize> {
    let mut depth = 0;
    for (offset, character) in source[open..].char_indices() {
        if character == '{' {
            depth += 1;
        } else if character == '}' {
            depth -= 1;
            if depth == 0 {
                return Some(open + offset);
            }
        }
    }
    None
}

fn struct_field_name(line: &str) -> Option<&str> {
    let mut field = line.split("//").next()?.trim();
    if let Some(rest) = field.strip_prefix("pub ") {
        field = rest.trim_start();
    } else if let Some(rest) = field.strip_prefix("pub(") {
        field = rest.split_once(')')?.1.trim_start();
    }
    let (name, _) = field.split_once(':')?;
    let name = name.trim();
    (!name.is_empty()
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_'))
    .then_some(name)
}

#[cfg(unix)]
fn executable(path: &Path) -> Result<bool> {
    use std::os::unix::fs::PermissionsExt;

    Ok(fs::metadata(path)
        .with_context(|| format!("read metadata for {}", path.display()))?
        .permissions()
        .mode()
        & 0o111
        != 0)
}

#[cfg(not(unix))]
fn executable(_: &Path) -> Result<bool> {
    Ok(false)
}
