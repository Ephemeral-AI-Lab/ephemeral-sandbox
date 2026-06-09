use std::collections::{BTreeMap, BTreeSet};

use workspace_guard::{read_to_string, Workspace};

#[test]
fn public_surface_matches_staged_or_final_allowlist() {
    let workspace = Workspace::load();
    let expected = if workspace.is_final_crate_map() {
        final_public_surface()
    } else {
        legacy_public_surface()
    };
    let actual = public_surface(&workspace);

    assert_eq!(
        actual,
        expected,
        "public_surface rule violated: lib.rs public exports drifted; update the guard only with the owning phase spec"
    );
}

fn public_surface(workspace: &Workspace) -> BTreeMap<String, BTreeSet<String>> {
    workspace
        .crates()
        .iter()
        .map(|(crate_name, crate_info)| {
            let path = crate_info.lib_path.clone();
            let entries = read_to_string(&path)
                .lines()
                .filter_map(public_surface_entry)
                .collect::<BTreeSet<_>>();
            (crate_name.clone(), entries)
        })
        .collect()
}

fn public_surface_entry(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    if let Some(rest) = trimmed.strip_prefix("pub mod ") {
        return Some(format!("mod:{}", trim_entry(rest)));
    }
    trimmed
        .strip_prefix("pub use ")
        .map(|rest| format!("use:{}", trim_entry(rest)))
}

fn trim_entry(rest: &str) -> String {
    rest.split(|ch: char| ch == ':' || ch == ';' || ch == '{' || ch == ',' || ch.is_whitespace())
        .next()
        .unwrap_or("")
        .to_owned()
}

fn legacy_public_surface() -> BTreeMap<String, BTreeSet<String>> {
    surface_map(&[
        (
            "eos-agent-run",
            &[
                "use:active_agent_runs",
                "use:agent_run_records",
                "use:agent_run_service",
                "use:eos_engine",
                "use:eos_types",
            ],
        ),
        (
            "eos-audit",
            &[
                "use:error",
                "use:event",
                "use:jsonl",
                "use:node",
                "use:obs",
                "use:sink",
            ],
        ),
        (
            "eos-config",
            &[
                "use:configs",
                "use:document",
                "use:error",
                "use:loader",
                "use:markdown",
            ],
        ),
        (
            "eos-db",
            &["use:composition", "use:error", "use:model_registry"],
        ),
        (
            "eos-engine",
            &[
                "mod:agent_loop",
                "mod:background",
                "mod:query",
                "mod:records",
                "mod:tool_call",
                "use:agent_loop",
                "use:background",
                "use:notifications",
                "use:query",
                "use:support",
                "use:telemetry",
            ],
        ),
        (
            "eos-llm-client",
            &[
                "use:auth",
                "use:client",
                "use:clients",
                "use:error",
                "use:events",
                "use:message",
                "use:types",
            ],
        ),
        (
            "eos-runtime",
            &[
                "mod:observability",
                "use:cancel",
                "use:entry",
                "use:eos_sandbox_port",
                "use:request_input",
                "use:runtime_services",
            ],
        ),
        (
            "eos-sandbox-port",
            &[
                "use:command_service",
                "use:error",
                "use:gateway",
                "use:models",
                "use:ops",
                "use:provision",
                "use:timeouts",
                "use:tool_api",
                "use:transport",
            ],
        ),
        (
            "eos-testkit",
            &[
                "use:agents",
                "use:engine",
                "use:llm",
                "use:meta",
                "use:sandbox",
            ],
        ),
        (
            "eos-tool",
            &[
                "use:error",
                "use:hooks",
                "use:model",
                "use:registry",
                "use:tools",
            ],
        ),
        (
            "eos-types",
            &[
                "mod:ports",
                "mod:state",
                "use:agent",
                "use:contracts",
                "use:error",
                "use:frontmatter",
                "use:ids",
                "use:json",
                "use:llm",
                "use:ports",
                "use:state",
                "use:time",
            ],
        ),
        (
            "eos-workflow",
            &[
                "use:attempt",
                "use:context",
                "use:error",
                "use:ids",
                "use:iteration",
                "use:service",
                "use:starter",
                "use:submission",
            ],
        ),
    ])
}

fn final_public_surface() -> BTreeMap<String, BTreeSet<String>> {
    surface_map(&[
        ("eos-agent-core", &[]),
        ("eos-agent-run", &[]),
        ("eos-engine", &[]),
        ("eos-tool", &[]),
        ("eos-workflow", &[]),
        ("eos-types", &[]),
        ("eos-db", &[]),
        ("eos-llm-client", &[]),
        ("eos-sandbox-port", &[]),
        ("eos-testkit", &[]),
    ])
}

fn surface_map(rows: &[(&str, &[&str])]) -> BTreeMap<String, BTreeSet<String>> {
    rows.iter()
        .map(|(crate_name, entries)| {
            (
                (*crate_name).to_owned(),
                entries.iter().map(|entry| (*entry).to_owned()).collect(),
            )
        })
        .collect()
}
