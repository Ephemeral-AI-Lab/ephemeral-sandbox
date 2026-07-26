use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};
use serde_json::Value;

use super::{DependencyFact, Domain, FeatureFacts, PackageFact};

struct PackagePolicy {
    manifest: &'static str,
    name: &'static str,
    layer: Layer,
    allowed: &'static [&'static str],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Layer {
    Contract,
    Catalog,
    Client,
    Application,
    Protocol,
    ProductAdapter,
    CompositionRoot,
    InfrastructureAdapter,
    Configuration,
    Primitive,
    Tooling,
}

impl fmt::Display for Layer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Contract => "contract",
            Self::Catalog => "catalog",
            Self::Client => "client",
            Self::Application => "application",
            Self::Protocol => "protocol",
            Self::ProductAdapter => "product-adapter",
            Self::CompositionRoot => "composition-root",
            Self::InfrastructureAdapter => "infrastructure-adapter",
            Self::Configuration => "configuration",
            Self::Primitive => "primitive",
            Self::Tooling => "tooling",
        })
    }
}

const PACKAGE_POLICIES: &[PackagePolicy] = &[
    PackagePolicy {
        manifest: "crates/sandbox-operations/contract/Cargo.toml",
        name: "sandbox-operation-contract",
        layer: Layer::Contract,
        allowed: &[],
    },
    PackagePolicy {
        manifest: "crates/sandbox-operations/catalog/Cargo.toml",
        name: "sandbox-operation-catalog",
        layer: Layer::Catalog,
        allowed: &["sandbox-operation-contract"],
    },
    PackagePolicy {
        manifest: "crates/sandbox-operations/client/Cargo.toml",
        name: "sandbox-operation-client",
        layer: Layer::Client,
        allowed: &["sandbox-operation-contract", "sandbox-protocol"],
    },
    PackagePolicy {
        manifest: "crates/sandbox-manager/Cargo.toml",
        name: "sandbox-manager",
        layer: Layer::Application,
        allowed: &[
            "sandbox-operation-contract",
            "sandbox-operation-catalog",
            "sandbox-runtime-layerstack",
        ],
    },
    PackagePolicy {
        manifest: "crates/sandbox-runtime/operation/Cargo.toml",
        name: "sandbox-runtime",
        layer: Layer::Application,
        allowed: &[
            "sandbox-operation-contract",
            "sandbox-operation-catalog",
            "sandbox-runtime-workspace",
            "sandbox-runtime-layerstack",
            "sandbox-runtime-namespace-execution",
            "sandbox-runtime-namespace-process",
            "sandbox-observability-telemetry",
        ],
    },
    PackagePolicy {
        manifest: "crates/sandbox-observability/query/Cargo.toml",
        name: "sandbox-observability-query",
        layer: Layer::Application,
        allowed: &[
            "sandbox-operation-contract",
            "sandbox-operation-catalog",
            "sandbox-observability-telemetry",
            "sandbox-runtime-layerstack",
        ],
    },
    PackagePolicy {
        manifest: "crates/sandbox-protocol/Cargo.toml",
        name: "sandbox-protocol",
        layer: Layer::Protocol,
        allowed: &["sandbox-operation-contract"],
    },
    PackagePolicy {
        manifest: "crates/sandbox-cli/Cargo.toml",
        name: "sandbox-cli",
        layer: Layer::ProductAdapter,
        allowed: &[
            "sandbox-operation-client",
            "sandbox-operation-contract",
            "sandbox-operation-catalog",
        ],
    },
    PackagePolicy {
        manifest: "crates/sandbox-mcp/Cargo.toml",
        name: "sandbox-mcp",
        layer: Layer::ProductAdapter,
        allowed: &[
            "sandbox-operation-client",
            "sandbox-operation-contract",
            "sandbox-operation-catalog",
        ],
    },
    PackagePolicy {
        manifest: "crates/sandbox-gateway/Cargo.toml",
        name: "sandbox-gateway",
        layer: Layer::CompositionRoot,
        allowed: &[
            "sandbox-operation-contract",
            "sandbox-operation-catalog",
            "sandbox-protocol",
            "sandbox-manager",
            "sandbox-provider-docker",
            "sandbox-config",
        ],
    },
    PackagePolicy {
        manifest: "crates/sandbox-daemon/Cargo.toml",
        name: "sandbox-daemon",
        layer: Layer::CompositionRoot,
        allowed: &[
            "sandbox-operation-contract",
            "sandbox-operation-catalog",
            "sandbox-protocol",
            "sandbox-runtime",
            "sandbox-observability-query",
            "sandbox-observability-telemetry",
            "sandbox-runtime-namespace-process",
            "sandbox-config",
            "sandbox-runtime-layerstack",
            "sandbox-runtime-workspace",
        ],
    },
    PackagePolicy {
        manifest: "crates/sandbox-provider-docker/Cargo.toml",
        name: "sandbox-provider-docker",
        layer: Layer::InfrastructureAdapter,
        allowed: &[
            "sandbox-operation-contract",
            "sandbox-manager",
            "sandbox-protocol",
            "sandbox-config",
            "sandbox-runtime-layerstack",
        ],
    },
    PackagePolicy {
        manifest: "crates/sandbox-config/Cargo.toml",
        name: "sandbox-config",
        layer: Layer::Configuration,
        allowed: &[],
    },
    PackagePolicy {
        manifest: "crates/sandbox-observability/telemetry/Cargo.toml",
        name: "sandbox-observability-telemetry",
        layer: Layer::Primitive,
        allowed: &[],
    },
    PackagePolicy {
        manifest: "crates/sandbox-runtime/layerstack-core/Cargo.toml",
        name: "sandbox-runtime-layerstack-core",
        layer: Layer::Primitive,
        allowed: &[],
    },
    PackagePolicy {
        manifest: "crates/sandbox-runtime/layerstack/Cargo.toml",
        name: "sandbox-runtime-layerstack",
        layer: Layer::Primitive,
        allowed: &["sandbox-runtime-layerstack-core"],
    },
    PackagePolicy {
        manifest: "crates/sandbox-runtime/overlay/Cargo.toml",
        name: "sandbox-runtime-overlay",
        layer: Layer::Primitive,
        allowed: &[],
    },
    PackagePolicy {
        manifest: "crates/sandbox-runtime/workspace/Cargo.toml",
        name: "sandbox-runtime-workspace",
        layer: Layer::Primitive,
        allowed: &[
            "sandbox-observability-telemetry",
            "sandbox-runtime-layerstack",
            "sandbox-runtime-namespace-execution",
            "sandbox-runtime-namespace-process",
        ],
    },
    PackagePolicy {
        manifest: "crates/sandbox-runtime/namespace-execution/Cargo.toml",
        name: "sandbox-runtime-namespace-execution",
        layer: Layer::Primitive,
        allowed: &[
            "sandbox-observability-telemetry",
            "sandbox-runtime-namespace-process",
        ],
    },
    PackagePolicy {
        manifest: "crates/sandbox-runtime/namespace-process/Cargo.toml",
        name: "sandbox-runtime-namespace-process",
        layer: Layer::Primitive,
        allowed: &["sandbox-observability-telemetry", "sandbox-runtime-overlay"],
    },
    PackagePolicy {
        manifest: "xtask/Cargo.toml",
        name: "xtask",
        layer: Layer::Tooling,
        allowed: &[],
    },
];

const MANAGER_EXTERNAL_DEPENDENCIES: &[&str] = &[
    "base64",
    "rustix",
    "serde",
    "serde_json",
    "tar",
    "thiserror",
    "tokio",
    "zstd",
];

const PROOF_TARGETS: &[(&str, &str, &str)] = &[
    (
        "sandbox-operation-catalog",
        "integrity",
        "crates/sandbox-operations/catalog/tests/integrity.rs",
    ),
    (
        "sandbox-cli",
        "projection_integrity",
        "crates/sandbox-cli/tests/projection_integrity.rs",
    ),
    (
        "sandbox-manager",
        "manager_router",
        "crates/sandbox-manager/tests/manager_router.rs",
    ),
    (
        "sandbox-manager",
        "manager_export",
        "crates/sandbox-manager/tests/manager_export.rs",
    ),
    (
        "sandbox-runtime",
        "operation_registry",
        "crates/sandbox-runtime/operation/tests/operation_registry.rs",
    ),
    (
        "sandbox-observability-query",
        "query",
        "crates/sandbox-observability/query/tests/query.rs",
    ),
];

pub(super) fn expected_crate_manifests() -> BTreeSet<&'static str> {
    PACKAGE_POLICIES
        .iter()
        .map(|policy| policy.manifest)
        .filter(|path| path.starts_with("crates/"))
        .collect()
}

pub fn load_workspace_facts(root: &Path) -> Result<Vec<PackageFact>> {
    let output = Command::new("cargo")
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .current_dir(root)
        .output()
        .context("run cargo metadata")?;
    if !output.status.success() {
        bail!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let document: Value = serde_json::from_slice(&output.stdout).context("parse cargo metadata")?;
    document["packages"]
        .as_array()
        .context("cargo metadata omitted packages")?
        .iter()
        .map(|package| package_fact(root, package))
        .collect()
}

fn package_fact(root: &Path, package: &Value) -> Result<PackageFact> {
    let manifest = relative_path(root, Path::new(required_string(package, "manifest_path")?))?;
    let package_name = required_string(package, "name")?;
    let mut dependencies = Vec::new();
    for dependency in package["dependencies"]
        .as_array()
        .context("package omitted dependencies")?
    {
        let dependency_name = required_string(dependency, "name")?;
        let Some(path) = dependency["path"].as_str() else {
            if package_name == "sandbox-manager" {
                dependencies.push(DependencyFact {
                    package: dependency_name.to_owned(),
                    rename: dependency["rename"].as_str().map(str::to_owned),
                    manifest: String::new(),
                    kind: dependency["kind"].as_str().unwrap_or("normal").to_owned(),
                    optional: dependency["optional"].as_bool().unwrap_or(false),
                    uses_default_features: dependency["uses_default_features"]
                        .as_bool()
                        .unwrap_or(true),
                    features: strings(&dependency["features"]),
                });
            }
            continue;
        };
        dependencies.push(DependencyFact {
            package: dependency_name.to_owned(),
            rename: dependency["rename"].as_str().map(str::to_owned),
            manifest: relative_path(root, &Path::new(path).join("Cargo.toml"))?,
            kind: dependency["kind"].as_str().unwrap_or("normal").to_owned(),
            optional: dependency["optional"].as_bool().unwrap_or(false),
            uses_default_features: dependency["uses_default_features"]
                .as_bool()
                .unwrap_or(true),
            features: strings(&dependency["features"]),
        });
    }
    let mut library_name = None;
    let mut binaries = BTreeSet::new();
    let mut binary_required_features = BTreeMap::new();
    let mut test_targets = BTreeMap::new();
    for target in package["targets"]
        .as_array()
        .context("package omitted targets")?
    {
        let kinds = target["kind"].as_array().context("target omitted kind")?;
        let name = required_string(target, "name")?;
        if kinds.iter().any(|kind| kind.as_str() == Some("lib")) {
            library_name = Some(name.to_owned());
        }
        if kinds.iter().any(|kind| kind.as_str() == Some("bin")) {
            binaries.insert(name.to_owned());
            binary_required_features.insert(name.to_owned(), strings(&target["required-features"]));
        }
        if kinds.iter().any(|kind| kind.as_str() == Some("test")) {
            let source = relative_path(root, Path::new(required_string(target, "src_path")?))?;
            if test_targets.insert(name.to_owned(), source).is_some() {
                bail!("duplicate cargo metadata test target {package_name}:{name}");
            }
        }
    }
    let features = package["features"]
        .as_object()
        .context("package omitted features")?
        .iter()
        .map(|(name, values)| (name.clone(), strings(values)))
        .collect();
    let library_name_override = manifest_has_library_name_override(&root.join(&manifest))?;
    Ok(PackageFact {
        name: package_name.to_owned(),
        manifest,
        dependencies,
        features,
        library_name,
        library_name_override,
        binaries,
        binary_required_features,
        test_targets,
    })
}

pub fn validate_workspace_facts(packages: &[PackageFact]) -> Vec<String> {
    let mut violations = Vec::new();
    report_package_duplicates(packages, &mut violations);
    let by_manifest = packages
        .iter()
        .map(|package| (package.manifest.as_str(), package))
        .collect::<BTreeMap<_, _>>();
    for package in packages {
        let Some(policy) = policy(&package.manifest) else {
            violations.push(format!(
                "unmapped workspace package: {} at {}",
                package.name, package.manifest
            ));
            continue;
        };
        if package.name != policy.name {
            violations.push(format!(
                "package naming violation in {} layer at {}: expected {}, found {}",
                policy.layer, package.manifest, policy.name, package.name
            ));
        }
        validate_library_and_binaries(package, &mut violations);
        validate_declared_features(package, &mut violations);
        validate_proof_targets(package, &mut violations);
        validate_manager_external_dependencies(package, &mut violations);
        for dependency in &package.dependencies {
            validate_dependency(package, policy, dependency, &mut violations);
        }
    }
    for policy in PACKAGE_POLICIES {
        if !by_manifest.contains_key(policy.manifest) {
            violations.push(format!(
                "missing mapped package: {} at {}",
                policy.name, policy.manifest
            ));
        }
    }
    let catalogs = packages
        .iter()
        .filter(|package| package.name == "sandbox-operation-catalog")
        .count();
    if catalogs != 1 {
        violations.push(format!("single catalog invariant failed: found {catalogs}"));
    }
    violations
}

fn validate_proof_targets(package: &PackageFact, violations: &mut Vec<String>) {
    for (_, target, expected) in PROOF_TARGETS
        .iter()
        .filter(|(name, _, _)| *name == package.name.as_str())
    {
        let actual = package.test_targets.get(*target).map(String::as_str);
        if actual != Some(*expected) {
            violations.push(format!(
                "proof target {}:{target} resolves to {}; expected {expected}",
                package.name,
                actual.unwrap_or("<missing>")
            ));
        }
    }
}

fn validate_manager_external_dependencies(package: &PackageFact, violations: &mut Vec<String>) {
    if package.name != "sandbox-manager" {
        return;
    }
    let actual = package
        .dependencies
        .iter()
        .filter(|dependency| dependency.manifest.is_empty())
        .map(|dependency| dependency.package.as_str())
        .collect::<BTreeSet<_>>();
    for dependency in MANAGER_EXTERNAL_DEPENDENCIES {
        if !actual.contains(dependency) {
            violations.push(format!(
                "required manager external dependency is missing: {dependency}"
            ));
        }
    }
}

fn validate_dependency(
    package: &PackageFact,
    package_policy: &PackagePolicy,
    dependency: &DependencyFact,
    violations: &mut Vec<String>,
) {
    if dependency.manifest.is_empty() {
        if package.name == "sandbox-manager" {
            if !MANAGER_EXTERNAL_DEPENDENCIES.contains(&dependency.package.as_str()) {
                violations.push(format!(
                    "unapproved manager external dependency: {} ({}, optional={})",
                    dependency.package, dependency.kind, dependency.optional
                ));
            }
            if dependency.rename.is_some() {
                violations.push(format!(
                    "manager adapter-capable dependency alias is forbidden: {} as {}",
                    dependency.package,
                    dependency.rename.as_deref().unwrap_or_default()
                ));
            }
        }
        return;
    }
    let Some(dependency_policy) = policy(&dependency.manifest) else {
        violations.push(format!(
            "dependency targets unmapped workspace manifest: {} -> {}",
            package.name, dependency.manifest
        ));
        return;
    };
    if dependency.package != dependency_policy.name {
        violations.push(format!(
            "dependency identity mismatch: {} resolves {} as {}",
            package.name, dependency.manifest, dependency.package
        ));
    }
    if let Some(rename) = &dependency.rename {
        violations.push(format!(
            "workspace dependency alias is forbidden: {} names {} as {}",
            package.name, dependency_policy.name, rename
        ));
    }
    if !package_policy.allowed.contains(&dependency_policy.name) {
        violations.push(format!(
            "forbidden dependency edge: {} [{}] -> {} [{}] ({}, optional={})",
            package.name,
            package_policy.layer,
            dependency_policy.name,
            dependency_policy.layer,
            dependency.kind,
            dependency.optional
        ));
    }
    if dependency_policy.name == "xtask" && package.name != "xtask" {
        violations.push(format!(
            "forbidden dependency on xtask: {} -> xtask",
            package.name
        ));
    }
    if package.name == "sandbox-daemon"
        && matches!(
            dependency_policy.name,
            "sandbox-runtime-layerstack" | "sandbox-runtime-workspace"
        )
        && dependency.kind != "dev"
    {
        violations.push(format!(
            "daemon primitive edge must be dev-only: {} ({})",
            dependency_policy.name, dependency.kind
        ));
    }
    validate_catalog_edge(package, dependency, violations);
}

fn validate_catalog_edge(
    package: &PackageFact,
    dependency: &DependencyFact,
    violations: &mut Vec<String>,
) {
    if dependency.manifest != "crates/sandbox-operations/catalog/Cargo.toml" {
        return;
    }
    if dependency.uses_default_features {
        violations.push(format!(
            "catalog default features enabled on {} edge",
            package.name
        ));
    }
    let expected: Option<&[&str]> = match package.name.as_str() {
        "sandbox-manager" => Some(&["manager", "runtime", "observability"]),
        "sandbox-runtime" => Some(&["runtime"]),
        "sandbox-observability-query" => Some(&["observability"]),
        "sandbox-daemon" => Some(&["runtime", "observability"]),
        "sandbox-mcp" => Some(&["manager", "runtime", "observability"]),
        "sandbox-cli" => Some(&[]),
        _ => None,
    };
    if let Some(expected) = expected {
        let expected = expected
            .iter()
            .map(|feature| (*feature).to_owned())
            .collect::<BTreeSet<_>>();
        if dependency.features != expected {
            violations.push(format!(
                "catalog feature set on {} edge is {:?}; expected {:?}",
                package.name, dependency.features, expected
            ));
        }
    }
}

fn validate_declared_features(package: &PackageFact, violations: &mut Vec<String>) {
    let expected = match package.name.as_str() {
        "sandbox-operation-catalog" => Some(BTreeMap::from([
            ("default".to_owned(), BTreeSet::new()),
            ("manager".to_owned(), BTreeSet::new()),
            ("runtime".to_owned(), BTreeSet::new()),
            ("observability".to_owned(), BTreeSet::new()),
        ])),
        "sandbox-cli" => Some(BTreeMap::from([
            ("default".to_owned(), BTreeSet::new()),
            (
                "manager".to_owned(),
                BTreeSet::from([
                    "dep:clap".to_owned(),
                    "sandbox-operation-catalog/manager".to_owned(),
                ]),
            ),
            (
                "runtime".to_owned(),
                BTreeSet::from([
                    "dep:clap".to_owned(),
                    "sandbox-operation-catalog/runtime".to_owned(),
                ]),
            ),
            (
                "observability".to_owned(),
                BTreeSet::from([
                    "dep:clap".to_owned(),
                    "sandbox-operation-catalog/observability".to_owned(),
                ]),
            ),
        ])),
        _ => None,
    };
    if expected
        .as_ref()
        .is_some_and(|expected| package.features != *expected)
    {
        violations.push(format!("feature declarations changed for {}", package.name));
    }
}

fn validate_library_and_binaries(package: &PackageFact, violations: &mut Vec<String>) {
    if package.library_name_override {
        violations.push(format!(
            "library target name override is forbidden in {}",
            package.name
        ));
    }
    let expected_library = package.name.replace('-', "_");
    if package
        .library_name
        .as_deref()
        .is_some_and(|name| name != expected_library)
    {
        violations.push(format!("library target override in {}", package.name));
    }
    let expected: &[(&str, &[&str])] = match package.name.as_str() {
        "sandbox-cli" => &[
            ("sandbox-manager-cli", &["manager"]),
            ("sandbox-runtime-cli", &["runtime"]),
            ("sandbox-observability-cli", &["observability"]),
            (
                "sandbox-catalog-export",
                &["manager", "runtime", "observability"],
            ),
        ],
        "sandbox-mcp" => &[("sandbox-mcp", &[])],
        "sandbox-gateway" => &[("sandbox-gateway", &[])],
        "sandbox-daemon" => &[("sandbox-daemon", &[])],
        "xtask" => &[("xtask", &[])],
        _ => &[],
    };
    let expected_names = expected
        .iter()
        .map(|(name, _)| (*name).to_owned())
        .collect::<BTreeSet<_>>();
    if package.binaries != expected_names {
        violations.push(format!(
            "binary target set changed for {}: {:?}",
            package.name, package.binaries
        ));
    }
    for (name, required) in expected {
        let required = required
            .iter()
            .map(|feature| (*feature).to_owned())
            .collect::<BTreeSet<_>>();
        if package.binary_required_features.get(*name) != Some(&required) {
            violations.push(format!("required feature set changed for binary {name}"));
        }
    }
}

fn report_package_duplicates(packages: &[PackageFact], violations: &mut Vec<String>) {
    let mut manifests = BTreeSet::new();
    let mut names = BTreeSet::new();
    for package in packages {
        if !manifests.insert(&package.manifest) {
            violations.push(format!(
                "duplicate workspace manifest: {}",
                package.manifest
            ));
        }
        if !names.insert(&package.name) {
            violations.push(format!(
                "duplicate workspace package name: {}",
                package.name
            ));
        }
    }
}

pub fn load_feature_facts(root: &Path) -> Result<FeatureFacts> {
    let mut resolved = BTreeMap::new();
    for domain in [Domain::Manager, Domain::Runtime, Domain::Observability] {
        let output = Command::new("cargo")
            .args([
                "tree",
                "-p",
                "sandbox-cli",
                "--no-default-features",
                "--features",
                &domain.to_string(),
                "-e",
                "features",
                "--prefix",
                "none",
                "-f",
                "{p}|{f}",
            ])
            .current_dir(root)
            .output()
            .with_context(|| format!("resolve {domain} CLI feature closure"))?;
        if !output.status.success() {
            bail!(
                "cargo tree failed for {domain}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let mut catalog_features = BTreeSet::new();
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            if let Some((_, features)) = line
                .starts_with("sandbox-operation-catalog ")
                .then(|| line.rsplit_once('|'))
                .flatten()
            {
                catalog_features.extend(
                    features
                        .split(',')
                        .map(str::trim)
                        .filter(|feature| !feature.is_empty())
                        .map(str::to_owned),
                );
            }
        }
        resolved.insert(domain, catalog_features);
    }
    Ok(FeatureFacts { resolved })
}

pub fn validate_feature_facts(facts: &FeatureFacts) -> Vec<String> {
    let mut violations = Vec::new();
    for domain in [Domain::Manager, Domain::Runtime, Domain::Observability] {
        let expected = BTreeSet::from([domain.to_string()]);
        let actual = facts.resolved.get(&domain).cloned().unwrap_or_default();
        for feature in actual.difference(&expected) {
            violations.push(format!(
                "out-of-closure catalog feature for {domain} CLI: {feature}"
            ));
        }
        for feature in expected.difference(&actual) {
            violations.push(format!(
                "missing catalog feature for {domain} CLI: {feature}"
            ));
        }
    }
    violations
}

fn policy(manifest: &str) -> Option<&'static PackagePolicy> {
    PACKAGE_POLICIES
        .iter()
        .find(|policy| policy.manifest == manifest)
}

fn manifest_has_library_name_override(path: &Path) -> Result<bool> {
    let source = fs::read_to_string(path)
        .with_context(|| format!("read manifest source {}", path.display()))?;
    let mut in_library = false;
    let mut at_root = true;
    for raw_line in source.lines() {
        let line = strip_toml_comment(raw_line).trim();
        if let Some(header) = line
            .strip_prefix('[')
            .and_then(|line| line.strip_suffix(']'))
        {
            in_library = toml_key_path(header).as_deref() == Some(&["lib".to_owned()]);
            at_root = false;
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            let key = toml_key_path(key);
            if (in_library && key.as_deref() == Some(&["name".to_owned()]))
                || (at_root
                    && (key.as_deref() == Some(&["lib".to_owned(), "name".to_owned()])
                        || (key.as_deref() == Some(&["lib".to_owned()])
                            && inline_table_has_key(value, "name"))))
            {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn strip_toml_comment(line: &str) -> &str {
    let mut quote = None;
    let mut escaped = false;
    for (index, character) in line.char_indices() {
        if escaped {
            escaped = false;
        } else if quote == Some('"') && character == '\\' {
            escaped = true;
        } else if quote == Some(character) {
            quote = None;
        } else if quote.is_none() && matches!(character, '\'' | '"') {
            quote = Some(character);
        } else if quote.is_none() && character == '#' {
            return &line[..index];
        }
    }
    line
}

fn toml_key_path(raw: &str) -> Option<Vec<String>> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut quote = None;
    let mut escaped = false;
    for (index, character) in raw.char_indices() {
        if escaped {
            escaped = false;
        } else if quote == Some('"') && character == '\\' {
            escaped = true;
        } else if quote == Some(character) {
            quote = None;
        } else if quote.is_none() && matches!(character, '\'' | '"') {
            quote = Some(character);
        } else if quote.is_none() && character == '.' {
            parts.push(toml_key(&raw[start..index])?);
            start = index + 1;
        }
    }
    if quote.is_some() {
        return None;
    }
    parts.push(toml_key(&raw[start..])?);
    Some(parts)
}

fn toml_key(raw: &str) -> Option<String> {
    let key = raw.trim();
    if key.starts_with('"') {
        serde_json::from_str(key).ok()
    } else if key.starts_with('\'') {
        key.strip_prefix('\'')
            .and_then(|key| key.strip_suffix('\''))
            .map(str::to_owned)
    } else {
        Some(key.split_whitespace().collect())
    }
}

fn inline_table_has_key(value: &str, expected: &str) -> bool {
    value
        .trim()
        .strip_prefix('{')
        .and_then(|value| value.rsplit_once('}').map(|(body, _)| body))
        .into_iter()
        .flat_map(|body| body.split(','))
        .filter_map(|entry| entry.split_once('=').map(|(key, _)| key))
        .filter_map(toml_key)
        .any(|key| key == expected)
}

fn strings(value: &Value) -> BTreeSet<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

fn required_string<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .with_context(|| format!("missing string field {key}"))
}

fn relative_path(root: &Path, path: &Path) -> Result<String> {
    let root = root
        .canonicalize()
        .with_context(|| format!("canonicalize {}", root.display()))?;
    let path = path
        .canonicalize()
        .with_context(|| format!("canonicalize {}", path.display()))?;
    Ok(path
        .strip_prefix(&root)
        .with_context(|| format!("{} is outside {}", path.display(), root.display()))?
        .to_string_lossy()
        .replace('\\', "/"))
}
