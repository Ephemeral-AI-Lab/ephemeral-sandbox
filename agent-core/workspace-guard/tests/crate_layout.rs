use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use workspace_guard::{
    directories_under, nonblank_line_count, relative_to, rust_files_under, workspace_root,
    Workspace, FORBIDDEN_VOCABULARY,
};

const RULE_FILES: &[&str] = &[
    "crate_inventory.rs",
    "crate_layout.rs",
    "dependency_dag.rs",
    "module_budget.rs",
    "naming_rules.rs",
    "public_surface.rs",
    "service_boundaries.rs",
];

const LIB_RS_LINE_LIMIT: usize = 200;
const ROOT_RS_LINE_LIMIT: usize = 200;
const VAGUE_BUCKET_FOLDERS: &[&str] = &["common", "helpers", "shared", "utils"];
const BANNED_ARCHITECTURE_FOLDERS: &[&str] = &[
    "api",
    "composition",
    "deps",
    "ports",
    "runtime_services",
    "services",
];

#[test]
fn workspace_guard_has_only_documented_rule_files() {
    let tests_dir = workspace_root().join("workspace-guard/tests");
    let actual = fs::read_dir(&tests_dir)
        .unwrap_or_else(|err| panic!("read {}: {err}", tests_dir.display()))
        .map(|entry| {
            let entry = entry.unwrap_or_else(|err| panic!("read tests dir entry: {err}"));
            entry.file_name().to_string_lossy().into_owned()
        })
        .filter(|name| name.ends_with(".rs"))
        .collect::<BTreeSet<_>>();
    let expected = RULE_FILES
        .iter()
        .map(|name| (*name).to_owned())
        .collect::<BTreeSet<_>>();

    assert_eq!(
        actual, expected,
        "crate_layout rule violated: workspace-guard must contain exactly the seven phase-01 rule files"
    );
}

#[test]
fn crate_root_files_stay_thin() {
    let workspace = Workspace::load();
    let violations = workspace
        .crates()
        .values()
        .flat_map(|crate_info| {
            rust_files_under(&crate_info.src_dir)
                .into_iter()
                .filter(|path| is_rust_root_file(path, &crate_info.src_dir))
                .map(|path| {
                    let lines = nonblank_line_count(&path);
                    (path, lines)
                })
        })
        .filter_map(|(path, lines)| {
            (lines > ROOT_RS_LINE_LIMIT).then(|| {
                format!(
                    "{}: crate root file has {lines} nonblank lines; limit is {ROOT_RS_LINE_LIMIT}",
                    relative_to(&path, workspace.root())
                )
            })
        })
        .collect::<Vec<_>>();

    assert!(
        violations.is_empty(),
        "crate_layout rule violated:\n{}",
        violations.join("\n")
    );
}

#[test]
fn crate_lib_files_stay_thin_for_metadata_targets() {
    let workspace = Workspace::load();
    let violations = workspace
        .crates()
        .values()
        .filter_map(|crate_info| {
            let path = crate_info.lib_path.clone();
            let lines = nonblank_line_count(&path);
            (lines > LIB_RS_LINE_LIMIT).then(|| {
                format!(
                    "{}: library root has {lines} nonblank lines; limit is {LIB_RS_LINE_LIMIT}",
                    relative_to(&path, workspace.root())
                )
            })
        })
        .collect::<Vec<_>>();

    assert!(
        violations.is_empty(),
        "crate_layout rule violated:\n{}",
        violations.join("\n")
    );
}

#[test]
fn source_tree_does_not_contain_test_modules() {
    let workspace = Workspace::load();
    let violations = workspace
        .crates()
        .values()
        .flat_map(|crate_info| rust_files_under(&crate_info.src_dir))
        .filter(|path| is_source_test_module(path))
        .map(|path| {
            format!(
                "{}: test modules belong under the crate tests/ tree, not src/",
                relative_to(&path, workspace.root())
            )
        })
        .collect::<Vec<_>>();

    assert!(
        violations.is_empty(),
        "crate_layout rule violated:\n{}",
        violations.join("\n")
    );
}

#[test]
fn final_target_crates_do_not_use_mod_rs_maze() {
    let workspace = Workspace::load();
    let violations = workspace
        .crates()
        .values()
        .flat_map(|crate_info| rust_files_under(&crate_info.src_dir))
        .filter(|path| path.file_name().and_then(|name| name.to_str()) == Some("mod.rs"))
        .map(|path| {
            format!(
                "{}: mod.rs maze is banned in the final target layout",
                relative_to(&path, workspace.root())
            )
        })
        .collect::<Vec<_>>();

    assert!(
        violations.is_empty(),
        "crate_layout rule violated:\n{}",
        violations.join("\n")
    );
}

#[test]
fn final_target_crates_do_not_use_forbidden_folders() {
    let workspace = Workspace::load();
    let violations = workspace
        .crates()
        .values()
        .flat_map(|crate_info| directories_under(&crate_info.src_dir))
        .filter_map(|path| forbidden_folder_violation(&path, workspace.root()))
        .collect::<Vec<_>>();

    assert!(
        violations.is_empty(),
        "crate_layout rule violated:\n{}",
        violations.join("\n")
    );
}

#[test]
fn final_target_crates_do_not_mix_module_file_shapes() {
    let workspace = Workspace::load();
    let violations = workspace
        .crates()
        .values()
        .flat_map(|crate_info| {
            rust_files_under(&crate_info.src_dir)
                .into_iter()
                .filter_map(move |path| {
                    let relative = path.strip_prefix(&crate_info.src_dir).ok()?;
                    (relative.file_name().and_then(|name| name.to_str()) == Some("mod.rs"))
                        .then_some((crate_info, path))
                })
        })
        .filter_map(|(crate_info, path)| {
            let parent_module = path.parent()?.strip_prefix(&crate_info.src_dir).ok()?;
            if parent_module.as_os_str().is_empty() {
                return None;
            }
            let flat_module = crate_info.src_dir.join(parent_module).with_extension("rs");
            flat_module.exists().then(|| {
                format!(
                    "{}: duplicate Rust module layout; use either `{}/mod.rs` or `{}.rs`, not both",
                    relative_to(&path, workspace.root()),
                    parent_module.display(),
                    parent_module.display()
                )
            })
        })
        .collect::<Vec<_>>();

    assert!(
        violations.is_empty(),
        "crate_layout rule violated:\n{}",
        violations.join("\n")
    );
}

fn forbidden_folder_violation(path: &Path, workspace_root: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    if VAGUE_BUCKET_FOLDERS.contains(&name) {
        return Some(format!(
            "{}: folder `{name}` is a vague bucket; use an owner/domain-specific module name",
            relative_to(path, workspace_root)
        ));
    }
    if BANNED_ARCHITECTURE_FOLDERS.contains(&name) || FORBIDDEN_VOCABULARY.contains(&name) {
        return Some(format!(
            "{}: folder `{name}` uses forbidden architecture vocabulary",
            relative_to(path, workspace_root)
        ));
    }
    None
}

fn is_rust_root_file(path: &Path, src_dir: &Path) -> bool {
    path.parent() == Some(src_dir)
        && matches!(
            path.file_name().and_then(|name| name.to_str()),
            Some("lib.rs" | "main.rs" | "mod.rs")
        )
}

fn is_source_test_module(path: &Path) -> bool {
    path.file_name().and_then(|name| name.to_str()) == Some("tests.rs")
        || path
            .components()
            .any(|component| component.as_os_str().to_str() == Some("tests"))
}
