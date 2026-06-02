// AC-workspace-02: the internal eos-* -> eos-* dependency edge set equals the
// frozen Phase-0 set (impl-workspace.md §5) and is acyclic. A stray edge (e.g.
// an inverted layering edge) fails this test. The set is reconciled with the
// overview.md dependency topology: eos-plugin-catalog -> sandbox-api, audit,
// config (impl-workspace.md §5's audit-only row is corrected to the topology and
// the crate's own impl-eos-plugin-catalog.md §2).
//
// Plain `//` comments are used throughout so clippy::doc_markdown never fires on
// the crate identifiers.

use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;

use pretty_assertions::assert_eq;

type Edges = BTreeMap<String, BTreeSet<String>>;

fn frozen_edges() -> Edges {
    let rows: &[(&str, &[&str])] = &[
        ("eos-types", &[]),
        // Phase 2: the scaffold-only eos-config -> eos-types edge is pruned when
        // eos-config is implemented (overview.md Phase-0 notes; impl-workspace.md
        // §5). eos-config is a DAG-root leaf with no internal upstream edge.
        ("eos-config", &[]),
        ("eos-state", &["eos-types"]),
        ("eos-db", &["eos-state", "eos-config"]),
        ("eos-audit", &["eos-types"]),
        ("eos-llm-client", &["eos-types", "eos-config"]),
        ("eos-agent-def", &["eos-types"]),
        ("eos-sandbox-api", &["eos-types"]),
        ("eos-skills", &["eos-types", "eos-config"]),
        // Phase 4: eos-tools adds a direct eos-types edge when implemented. It
        // names eos_types items (InvocationId / ToolUseId / WorkflowSessionId /
        // CommandSessionId / SubagentSessionId in ExecutionMetadata + the DTOs,
        // JsonObject / CoreError) in its own signatures, and none of its other
        // upstream deps re-export them (eos-state re-exports only a subset) — so
        // the edge is required to compile, per the crate's own impl-eos-tools.md
        // §2. Mirrors the eos-sandbox-host / eos-plugin-catalog precedent.
        (
            "eos-tools",
            &[
                "eos-types",
                "eos-state",
                "eos-sandbox-api",
                "eos-skills",
                "eos-audit",
                "eos-llm-client",
            ],
        ),
        (
            "eos-engine",
            &[
                "eos-types",
                "eos-llm-client",
                "eos-tools",
                "eos-audit",
                "eos-agent-def",
            ],
        ),
        (
            "eos-workflow",
            &[
                "eos-types",
                "eos-state",
                "eos-tools",
                "eos-agent-def",
                "eos-audit",
            ],
        ),
        // Phase 3: eos-sandbox-host adds a direct eos-types edge when implemented.
        // It names eos_types items (SandboxId, JsonObject, Clock, CoreError,
        // RequestId, InvocationId) in its own signatures (e.g. the SandboxTransport
        // impl), and eos-sandbox-api does not re-export them — so the edge is
        // required to compile, per the crate's own impl-eos-sandbox-host.md §2
        // (which outranks the summary topology block that omitted it).
        (
            "eos-sandbox-host",
            &["eos-sandbox-api", "eos-config", "eos-types"],
        ),
        // Phase 3: eos-plugin-catalog adds a direct eos-types edge when implemented.
        // It names eos_types items (Clock in audit_plugin_call, JsonObject in the
        // LSP input structs) in its own signatures, and neither eos-audit nor
        // eos-sandbox-api re-exports them — so the edge is required to compile, per
        // the crate's own impl-eos-plugin-catalog.md §2 (which outranks the summary
        // topology block that omitted it).
        (
            "eos-plugin-catalog",
            &["eos-types", "eos-sandbox-api", "eos-audit", "eos-config"],
        ),
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
        // The test crate itself depends on no workspace crate.
        ("eos-parity", &[]),
    ];
    rows.iter()
        .map(|(name, deps)| {
            (
                (*name).to_owned(),
                deps.iter().map(|d| (*d).to_owned()).collect(),
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
    let eos: BTreeSet<&str> = packages
        .iter()
        .filter_map(|p| p["name"].as_str())
        .filter(|n| n.starts_with("eos-"))
        .collect();
    let mut edges = Edges::new();
    for pkg in packages {
        let name = pkg["name"].as_str().expect("package name");
        if !name.starts_with("eos-") {
            continue;
        }
        let mut deps = BTreeSet::new();
        if let Some(arr) = pkg["dependencies"].as_array() {
            for dep in arr {
                if dep["kind"].as_str() == Some("dev") {
                    continue;
                }
                if let Some(dep_name) = dep["name"].as_str() {
                    if eos.contains(dep_name) {
                        deps.insert(dep_name.to_owned());
                    }
                }
            }
        }
        edges.insert(name.to_owned(), deps);
    }
    edges
}

#[test]
fn internal_edges_match_frozen_set() {
    assert_eq!(actual_edges(), frozen_edges());
}

#[test]
fn tools_depends_on_llm_client() {
    // The anchor §5a edge is required, not optional: ToolSpec is owned by
    // eos-llm-client and authored in eos-tools.
    let edges = actual_edges();
    let tools = edges.get("eos-tools").expect("eos-tools present");
    assert!(
        tools.contains("eos-llm-client"),
        "missing required §5a edge eos-tools -> eos-llm-client"
    );
}

#[test]
fn dag_is_acyclic() {
    // Cargo would already reject a cycle, but assert it explicitly via Kahn's
    // algorithm over the ACTUAL resolved edges (not the frozen set) so this
    // validates the real workspace, not a hardcoded copy.
    let edges = actual_edges();
    let mut indegree: BTreeMap<&str, usize> = edges.keys().map(|k| (k.as_str(), 0)).collect();
    for deps in edges.values() {
        for dep in deps {
            *indegree.entry(dep.as_str()).or_insert(0) += 1;
        }
    }
    let mut queue: Vec<&str> = indegree
        .iter()
        .filter(|(_, d)| **d == 0)
        .map(|(n, _)| *n)
        .collect();
    let mut visited = 0usize;
    while let Some(node) = queue.pop() {
        visited += 1;
        for dep in edges.get(node).into_iter().flatten() {
            let entry = indegree.get_mut(dep.as_str()).expect("known node");
            *entry -= 1;
            if *entry == 0 {
                queue.push(dep.as_str());
            }
        }
    }
    assert_eq!(visited, edges.len(), "dependency graph contains a cycle");
}
