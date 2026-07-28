mod feature_gates;
mod metadata;
mod semantic;
mod source_boundaries;
mod stale;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;
use std::process::{Command, Output};

use anyhow::{bail, Context, Result};

pub use feature_gates::validate_feature_gates;
pub use metadata::{
    load_feature_facts, load_workspace_facts, validate_feature_facts, validate_workspace_facts,
};
pub use semantic::{load_semantic_facts, validate_semantic_facts};
pub use stale::{load_stale_facts, validate_stale_facts};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DependencyFact {
    pub package: String,
    pub rename: Option<String>,
    pub manifest: String,
    pub kind: String,
    pub optional: bool,
    pub uses_default_features: bool,
    pub features: BTreeSet<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageFact {
    pub name: String,
    pub manifest: String,
    pub dependencies: Vec<DependencyFact>,
    pub features: BTreeMap<String, BTreeSet<String>>,
    pub library_name: Option<String>,
    pub library_name_override: bool,
    pub binaries: BTreeSet<String>,
    pub binary_required_features: BTreeMap<String, BTreeSet<String>>,
    pub test_targets: BTreeMap<String, String>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Domain {
    Manager,
    Runtime,
    Observability,
}

impl fmt::Display for Domain {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Manager => "manager",
            Self::Runtime => "runtime",
            Self::Observability => "observability",
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Owner {
    Manager,
    Runtime,
    Observability,
}

impl fmt::Display for Owner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Manager => "manager",
            Self::Runtime => "runtime",
            Self::Observability => "observability",
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Scope {
    System,
    Sandbox,
}

impl fmt::Display for Scope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::System => "system",
            Self::Sandbox => "sandbox",
        })
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct DomainOperation {
    pub domain: Domain,
    pub operation: String,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct RouteFact {
    pub operation: String,
    pub scope: Scope,
    pub owner: Owner,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SemanticFacts {
    pub public_operations: Vec<DomainOperation>,
    pub public_routes: Vec<RouteFact>,
    pub internal_routes: Vec<RouteFact>,
    pub public_handlers: Vec<RouteFact>,
    pub internal_handlers: Vec<RouteFact>,
    pub projections: Vec<DomainOperation>,
    pub unclassified_operation_declarations: Vec<String>,
    pub unwired_route_declarations: Vec<String>,
    pub unwired_handler_declarations: Vec<String>,
    pub unwired_projection_declarations: Vec<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FeatureFacts {
    pub resolved: BTreeMap<Domain, BTreeSet<String>>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TrackedSource {
    pub path: String,
    pub content: String,
    pub executable: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StaleFacts {
    pub files: Vec<TrackedSource>,
}

pub fn check(root: &Path) -> Result<()> {
    let packages = load_workspace_facts(root)?;
    let semantics = load_semantic_facts(root)?;
    let features = load_feature_facts(root)?;
    let stale = load_stale_facts(root)?;
    let mut violations = validate_workspace_facts(&packages);
    violations.extend(validate_semantic_facts(&semantics));
    violations.extend(validate_feature_facts(&features));
    violations.extend(validate_feature_gates(&stale));
    violations.extend(validate_stale_facts(root, &stale));
    if !violations.is_empty() {
        violations.sort();
        bail!(
            "operation architecture violations:\n- {}",
            violations.join("\n- ")
        );
    }
    run_behavior_proofs(root)?;
    println!("operation architecture check passed");
    Ok(())
}

fn run_behavior_proofs(root: &Path) -> Result<()> {
    for (binary, feature) in [
        ("sandbox-manager-cli", "manager"),
        ("sandbox-runtime-cli", "runtime"),
        ("sandbox-observability-cli", "observability"),
    ] {
        let arguments = [
            "check",
            "-p",
            "sandbox-cli",
            "--bin",
            binary,
            "--no-default-features",
            "--features",
            feature,
        ];
        run_cargo(root, &arguments)?;
        println!("cargo {} passed", arguments.join(" "));
    }
    let proofs: &[(&[&str], Option<&str>)] = &[
        (&[
            "test",
            "-p",
            "sandbox-operation-catalog",
            "--all-features",
            "--test",
            "integrity",
        ], None),
        (&[
            "test",
            "-p",
            "sandbox-operation-catalog",
            "--all-features",
            "--test",
            "integrity",
            "public_catalogs_are_route_complete",
            "--",
            "--exact",
        ], Some("public_catalogs_are_route_complete")),
        (&[
            "test",
            "-p",
            "sandbox-operation-catalog",
            "--all-features",
            "--test",
            "integrity",
            "public_route_manifest_is_exact_and_policy_consistent",
            "--",
            "--exact",
        ], Some("public_route_manifest_is_exact_and_policy_consistent")),
        (&[
            "test",
            "-p",
            "sandbox-operation-catalog",
            "--all-features",
            "--test",
            "integrity",
            "internal_routes_never_leak_into_public_documents",
            "--",
            "--exact",
        ], Some("internal_routes_never_leak_into_public_documents")),
        (&[
            "test",
            "-p",
            "sandbox-operation-catalog",
            "--all-features",
            "--test",
            "integrity",
            "internal_route_sets_are_exact",
            "--",
            "--exact",
        ], Some("internal_route_sets_are_exact")),
        (&[
            "test",
            "-p",
            "sandbox-cli",
            "--all-features",
            "--test",
            "projection_integrity",
        ], None),
        (&[
            "test",
            "-p",
            "sandbox-cli",
            "--all-features",
            "--test",
            "projection_integrity",
            "cli_projection_is_bidirectional_with_public_routes",
            "--",
            "--exact",
        ], Some("cli_projection_is_bidirectional_with_public_routes")),
        (&["test", "-p", "sandbox-manager", "--test", "manager_router"], None),
        (&[
            "test",
            "-p",
            "sandbox-manager",
            "--test",
            "manager_router",
            "manager_public_routes_and_handler_keys_are_bijective",
            "--",
            "--exact",
        ], Some("manager_public_routes_and_handler_keys_are_bijective")),
        (&[
            "test",
            "-p",
            "sandbox-manager",
            "--test",
            "manager_router",
            "manager_router_rejects_internal_routes_while_public_export_uses_direct_daemon_port",
            "--",
            "--exact",
        ], Some("manager_router_rejects_internal_routes_while_public_export_uses_direct_daemon_port")),
        (&["test", "-p", "sandbox-manager", "--test", "manager_export"], None),
        (&[
            "test",
            "-p",
            "sandbox-runtime",
            "--test",
            "operation_registry",
        ], None),
        (&[
            "test",
            "-p",
            "sandbox-runtime",
            "--test",
            "operation_registry",
            "public_runtime_routes_and_handlers_are_bijective",
            "--",
            "--exact",
        ], Some("public_runtime_routes_and_handlers_are_bijective")),
        (&[
            "test",
            "-p",
            "sandbox-runtime",
            "--test",
            "operation_registry",
            "canonical_internal_routes_and_handlers_are_bijective",
            "--",
            "--exact",
        ], Some("canonical_internal_routes_and_handlers_are_bijective")),
        (&[
            "test",
            "-p",
            "sandbox-runtime",
            "--test",
            "operation_registry",
            "runtime_registry_partitions_are_unique_and_disjoint",
            "--",
            "--exact",
        ], Some("runtime_registry_partitions_are_unique_and_disjoint")),
        (&[
            "test",
            "-p",
            "sandbox-observability-query",
            "--test",
            "query",
        ], None),
        (&[
            "test",
            "-p",
            "sandbox-observability-query",
            "--test",
            "query",
            "sandbox_registry_is_bijective_with_observability_domain_routes",
            "--",
            "--exact",
        ], Some("sandbox_registry_is_bijective_with_observability_domain_routes")),
    ];
    for (arguments, expected) in proofs {
        let output = run_cargo(root, arguments)?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let passed = stdout
            .lines()
            .filter_map(test_result_passed_count)
            .sum::<usize>();
        if passed == 0 {
            bail!(
                "cargo {} completed without executing a test",
                arguments.join(" ")
            );
        }
        if let Some(expected) = expected {
            if passed != 1 {
                bail!(
                    "cargo {} executed {passed} tests; expected 1",
                    arguments.join(" ")
                );
            }
            if !stdout.lines().any(|line| named_test_passed(line, expected)) {
                bail!(
                    "cargo {} did not report named test {expected} as passed",
                    arguments.join(" ")
                );
            }
        }
        println!("cargo {} passed ({passed} tests)", arguments.join(" "));
    }
    Ok(())
}

fn run_cargo(root: &Path, arguments: &[&str]) -> Result<Output> {
    let output = Command::new("cargo")
        .args(arguments)
        .current_dir(root)
        .output()
        .with_context(|| format!("run cargo {}", arguments.join(" ")))?;
    if !output.status.success() {
        bail!(
            "cargo {} failed:\n{}{}",
            arguments.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(output)
}

fn test_result_passed_count(line: &str) -> Option<usize> {
    let fields = line.split_whitespace().collect::<Vec<_>>();
    let passed = fields.iter().position(|field| *field == "passed;")?;
    fields.get(passed.checked_sub(1)?)?.parse().ok()
}

fn named_test_passed(line: &str, expected: &str) -> bool {
    line.strip_prefix("test ")
        .and_then(|line| line.strip_suffix(" ... ok"))
        == Some(expected)
}
