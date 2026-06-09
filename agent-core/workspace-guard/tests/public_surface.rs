use std::collections::{BTreeMap, BTreeSet};

use workspace_guard::{read_to_string, Workspace};

#[test]
fn public_surface_matches_target_allowlist() {
    let workspace = Workspace::load();
    let actual = public_surface(&workspace);

    assert_eq!(
        actual,
        final_public_surface(),
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

fn final_public_surface() -> BTreeMap<String, BTreeSet<String>> {
    surface_map(&[
        (
            "eos-agent-core-server",
            &["use:dto", "use:error", "use:service"],
        ),
        (
            "eos-agent-run",
            &["use:active_agent_runs", "use:eos_types", "use:service"],
        ),
        (
            "eos-db",
            &[
                "use:config",
                "use:database",
                "use:error",
                "use:model_registry",
            ],
        ),
        (
            "eos-engine",
            &[
                "mod:agent_loop",
                "mod:background",
                "mod:event",
                "mod:provider_stream",
                "mod:records",
                "mod:tool_call",
                "use:agent_loop",
                "use:background",
                "use:event",
                "use:notifications",
                "use:provider_stream",
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
                "use:config",
                "use:error",
                "use:events",
                "use:message",
                "use:types",
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
                "use:tool_dispatch",
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
                "mod:agent_loop",
                "use:agent",
                "use:agent_loop",
                "use:contracts",
                "use:error",
                "use:frontmatter",
                "use:ids",
                "use:llm",
                "use:models",
                "use:state",
                "use:stores",
                "use:time",
            ],
        ),
        (
            "eos-workflow",
            &[
                "use:attempt",
                "use:config",
                "use:context",
                "use:error",
                "use:ids",
                "use:iteration",
                "use:starter",
                "use:submission",
            ],
        ),
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
