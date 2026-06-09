use std::path::Path;

use workspace_guard::{
    directories_under, public_declared_identifiers, relative_to, rust_files_under, snake_case,
    Workspace, FORBIDDEN_VOCABULARY, LEGACY_MIGRATION_CRATES,
};

const PORT_CRATE_ALLOWLIST: &[&str] = &["eos-sandbox-port"];
const API_MODULE_ALLOWLIST: &[&str] = &["crates/eos-sandbox-port/src/tool_api"];
const RUNTIME_PATH_ALLOWLIST: &[&str] = &[
    "crates/eos-agent-core/src/runtime.rs",
    "crates/eos-agent-core/src/runtime",
];

#[test]
fn port_crate_names_are_limited_to_sandbox_or_migration_crates() {
    let workspace = Workspace::load();
    let violations = workspace
        .crate_names()
        .into_iter()
        .filter(|crate_name| crate_name.contains("port"))
        .filter(|crate_name| !PORT_CRATE_ALLOWLIST.contains(&crate_name.as_str()))
        .collect::<Vec<_>>();

    assert!(
        violations.is_empty(),
        "naming_rules rule violated: `port` is allowed only for eos-sandbox-port; \
         unexpected port crates: {violations:?}"
    );
}

#[test]
fn no_nonexistent_api_facade_crate_name() {
    let workspace = Workspace::load();
    assert!(
        !workspace.crate_names().contains("eos-agent-api"),
        "naming_rules rule violated: crate `eos-agent-api` never existed; target facade is eos-agent-core"
    );
}

#[test]
fn final_target_uses_no_forbidden_vocabulary_in_modules_or_public_types() {
    let workspace = Workspace::load();
    if !workspace.is_final_crate_map() {
        return;
    }

    let mut violations = Vec::new();
    for crate_info in workspace.crates().values() {
        for path in rust_files_under(&crate_info.src_dir) {
            if let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) {
                for word in FORBIDDEN_VOCABULARY {
                    if stem == *word || stem.contains(&format!("_{word}")) {
                        violations.push(format!(
                            "{}: module name `{stem}` uses forbidden vocabulary `{word}`",
                            relative_to(&path, workspace.root())
                        ));
                    }
                }
            }

            let text = workspace_guard::read_to_string(&path);
            for ident in public_declared_identifiers(&text) {
                let ident_snake = snake_case(&ident);
                for word in FORBIDDEN_VOCABULARY {
                    if ident_snake == *word || ident_snake.contains(&format!("_{word}")) {
                        violations.push(format!(
                            "{}: public type `{ident}` uses forbidden vocabulary `{word}`",
                            relative_to(&path, workspace.root())
                        ));
                    }
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "naming_rules rule violated:\n{}",
        violations.join("\n")
    );
}

#[test]
fn final_target_keeps_api_out_of_crate_and_module_suffixes() {
    let workspace = Workspace::load();
    if !workspace.is_final_crate_map() {
        return;
    }

    let mut violations = Vec::new();
    for crate_name in workspace.crate_names() {
        if crate_name.ends_with("-api") {
            violations.push(format!(
                "crates/{crate_name}: crate name uses banned `api` suffix"
            ));
        }
    }
    for crate_info in workspace.crates().values() {
        for path in directories_under(&crate_info.src_dir)
            .into_iter()
            .chain(rust_files_under(&crate_info.src_dir))
        {
            if is_api_allowlisted(&path, workspace.root()) {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            if stem == "api" || stem.ends_with("_api") || stem.ends_with("-api") {
                violations.push(format!(
                    "{}: module path uses banned `api` suffix",
                    relative_to(&path, workspace.root())
                ));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "naming_rules rule violated:\n{}",
        violations.join("\n")
    );
}

#[test]
fn final_target_scopes_runtime_to_agent_core_runtime() {
    let workspace = Workspace::load();
    if !workspace.is_final_crate_map() {
        return;
    }

    let violations = workspace
        .crates()
        .values()
        .flat_map(|crate_info| {
            directories_under(&crate_info.src_dir)
                .into_iter()
                .chain(rust_files_under(&crate_info.src_dir))
        })
        .filter(|path| path_contains_runtime(path))
        .filter(|path| !runtime_path_allowlisted(path, workspace.root()))
        .map(|path| {
            format!(
                "{}: `runtime` is allowed only under eos-agent-core/src/runtime.rs or runtime/",
                relative_to(&path, workspace.root())
            )
        })
        .collect::<Vec<_>>();

    assert!(
        violations.is_empty(),
        "naming_rules rule violated:\n{}",
        violations.join("\n")
    );
}

#[test]
fn migration_allowlist_contains_only_retired_crates() {
    for crate_name in LEGACY_MIGRATION_CRATES {
        assert!(
            crate_name.starts_with("eos-"),
            "naming_rules rule violated: migration allowlist entry `{crate_name}` is not an eos crate"
        );
    }
}

fn is_api_allowlisted(path: &Path, root: &Path) -> bool {
    let relative = relative_to(path, root);
    API_MODULE_ALLOWLIST
        .iter()
        .any(|allowed| relative == *allowed || relative.starts_with(&format!("{allowed}/")))
}

fn path_contains_runtime(path: &Path) -> bool {
    path.components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .is_some_and(|part| part == "runtime" || part == "runtime.rs")
    })
}

fn runtime_path_allowlisted(path: &Path, root: &Path) -> bool {
    let relative = relative_to(path, root);
    RUNTIME_PATH_ALLOWLIST
        .iter()
        .any(|allowed| relative == *allowed || relative.starts_with(&format!("{allowed}/")))
}
