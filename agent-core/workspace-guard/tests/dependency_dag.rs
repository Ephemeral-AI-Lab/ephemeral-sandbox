// Guards the agent-core internal dependency topology. During the destructive
// workspace migration this accepts the current legacy graph until the final
// ten-crate map is present, then switches to the locked target graph.

use std::collections::{BTreeMap, BTreeSet};

use workspace_guard::Workspace;

type Edges = BTreeMap<String, BTreeSet<String>>;

fn legacy_edges() -> Edges {
    let rows: &[(&str, &[&str])] = &[
        ("eos-types", &[]),
        ("eos-config", &[]),
        ("eos-db", &["eos-types", "eos-config"]),
        ("eos-audit", &["eos-types"]),
        ("eos-llm-client", &["eos-types", "eos-config"]),
        (
            "eos-agent-run",
            &["eos-types", "eos-engine", "eos-llm-client", "eos-tool"],
        ),
        ("eos-sandbox-port", &["eos-types"]),
        ("eos-tool", &["eos-types", "eos-sandbox-port"]),
        (
            "eos-engine",
            &[
                "eos-types",
                "eos-llm-client",
                "eos-tool",
                "eos-sandbox-port",
            ],
        ),
        ("eos-workflow", &["eos-types", "eos-tool"]),
        (
            "eos-runtime",
            &[
                "eos-db",
                "eos-engine",
                "eos-workflow",
                "eos-agent-run",
                "eos-config",
                "eos-sandbox-port",
                "eos-types",
                "eos-llm-client",
                "eos-tool",
                "eos-audit",
            ],
        ),
        // Dev-only shared test doubles (TESTING_SPEC). Its reverse edges
        // (eos-engine/eos-runtime -> eos-testkit) are `[dev-dependencies]`, so
        // they are filtered out of this DAG (line ~112) and introduce no cycle;
        // only eos-testkit's own normal deps appear here.
        (
            "eos-testkit",
            &[
                "eos-types",
                "eos-engine",
                "eos-llm-client",
                "eos-sandbox-port",
                "eos-tool",
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

fn target_edges() -> Edges {
    let rows: &[(&str, &[&str])] = &[
        ("eos-types", &[]),
        ("eos-db", &["eos-types"]),
        ("eos-llm-client", &["eos-types"]),
        ("eos-sandbox-port", &["eos-types"]),
        ("eos-tool", &["eos-types", "eos-sandbox-port"]),
        (
            "eos-engine",
            &[
                "eos-types",
                "eos-tool",
                "eos-llm-client",
                "eos-sandbox-port",
            ],
        ),
        ("eos-workflow", &["eos-types", "eos-tool"]),
        ("eos-agent-run", &["eos-types", "eos-engine"]),
        (
            "eos-agent-core",
            &[
                "eos-db",
                "eos-engine",
                "eos-workflow",
                "eos-agent-run",
                "eos-tool",
                "eos-sandbox-port",
                "eos-types",
                "eos-llm-client",
            ],
        ),
        (
            "eos-testkit",
            &[
                "eos-agent-run",
                "eos-engine",
                "eos-tool",
                "eos-types",
                "eos-llm-client",
                "eos-sandbox-port",
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

fn expected_edges(workspace: &Workspace) -> Edges {
    if workspace.is_final_crate_map() {
        target_edges()
    } else {
        legacy_edges()
    }
}

#[test]
fn internal_edges_match_expected_set() {
    let workspace = Workspace::load();
    assert_eq!(
        workspace.internal_dependency_edges(),
        expected_edges(&workspace),
        "dependency_dag rule violated: normal internal dependency edges do not match the {} graph",
        if workspace.is_final_crate_map() {
            "target"
        } else {
            "staged legacy"
        }
    );
}

#[test]
fn internal_dependency_graph_is_acyclic() {
    let edges = Workspace::load().internal_dependency_edges();
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
