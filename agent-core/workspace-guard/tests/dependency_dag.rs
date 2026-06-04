// Guards the agent-core internal dependency topology. A stray internal edge
// fails this test, which catches both unused edges and inverted layering.

use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;

type Edges = BTreeMap<String, BTreeSet<String>>;

fn expected_edges() -> Edges {
    let rows: &[(&str, &[&str])] = &[
        ("eos-obs-contract", &[]),
        ("eos-types", &[]),
        ("eos-config", &[]),
        ("eos-state", &["eos-types"]),
        ("eos-db", &["eos-state", "eos-config"]),
        ("eos-audit", &["eos-obs-contract", "eos-types"]),
        ("eos-llm-client", &["eos-types", "eos-config"]),
        ("eos-agent-def", &[]),
        ("eos-sandbox-api", &["eos-types"]),
        ("eos-skills", &["eos-config"]),
        (
            "eos-tools",
            &[
                "eos-types",
                "eos-state",
                "eos-sandbox-api",
                "eos-skills",
                "eos-llm-client",
                "eos-config",
            ],
        ),
        (
            "eos-engine",
            &[
                "eos-types",
                "eos-llm-client",
                "eos-tools",
                "eos-sandbox-api",
                "eos-audit",
                "eos-obs-contract",
                "eos-agent-def",
                "eos-state",
            ],
        ),
        (
            "eos-workflow",
            &["eos-types", "eos-state", "eos-tools", "eos-agent-def"],
        ),
        (
            "eos-sandbox-host",
            &["eos-sandbox-api", "eos-config", "eos-types"],
        ),
        ("eos-plugin-catalog", &["eos-types", "eos-sandbox-api"]),
        (
            "eos-runtime",
            &[
                "eos-db",
                "eos-engine",
                "eos-workflow",
                "eos-sandbox-host",
                "eos-plugin-catalog",
                "eos-skills",
                "eos-config",
                "eos-agent-def",
                "eos-sandbox-api",
                "eos-state",
                "eos-types",
                "eos-llm-client",
                "eos-tools",
                "eos-audit",
            ],
        ),
    ];
    rows.iter()
        .map(|(name, deps)| {
            (
                (*name).to_owned(),
                deps.iter().map(|dep| (*dep).to_owned()).collect(),
            )
        })
        .collect()
}

fn actual_edges() -> Edges {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let output = Command::new(cargo)
        .args(["metadata", "--format-version=1", "--no-deps"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("run `cargo metadata`");
    assert!(output.status.success(), "`cargo metadata` exited non-zero");

    let meta: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("parse cargo metadata json");
    let packages = meta["packages"].as_array().expect("packages array");
    let eos_packages: BTreeSet<&str> = packages
        .iter()
        .filter_map(|package| package["name"].as_str())
        .filter(|name| name.starts_with("eos-"))
        .collect();

    let mut edges = Edges::new();
    for package in packages {
        let name = package["name"].as_str().expect("package name");
        if !eos_packages.contains(name) {
            continue;
        }

        let deps = package["dependencies"]
            .as_array()
            .expect("dependencies array")
            .iter()
            .filter(|dep| dep["kind"].as_str() != Some("dev"))
            .filter_map(|dep| dep["name"].as_str())
            .filter(|dep_name| eos_packages.contains(dep_name))
            .map(ToOwned::to_owned)
            .collect();
        edges.insert(name.to_owned(), deps);
    }
    edges
}

#[test]
fn internal_edges_match_expected_set() {
    assert_eq!(actual_edges(), expected_edges());
}

#[test]
fn internal_dependency_graph_is_acyclic() {
    let edges = actual_edges();
    let mut indegree: BTreeMap<&str, usize> = edges.keys().map(|name| (name.as_str(), 0)).collect();
    for deps in edges.values() {
        for dep in deps {
            *indegree.entry(dep.as_str()).or_insert(0) += 1;
        }
    }

    let mut queue: Vec<&str> = indegree
        .iter()
        .filter(|(_, degree)| **degree == 0)
        .map(|(name, _)| *name)
        .collect();
    let mut visited = 0usize;
    while let Some(node) = queue.pop() {
        visited += 1;
        for dep in edges.get(node).into_iter().flatten() {
            let degree = indegree.get_mut(dep.as_str()).expect("known node");
            *degree -= 1;
            if *degree == 0 {
                queue.push(dep);
            }
        }
    }

    assert_eq!(visited, edges.len(), "dependency graph contains a cycle");
}
