//! Test-only helpers for the `agent-core` workspace guard tests.
//!
//! This crate is a publish-disabled workspace member. It has no production
//! behavior; the public items below keep the integration-test guard files small
//! and focused on the architecture rules they enforce.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

pub const TARGET_CRATES: &[&str] = &[
    "eos-agent-core",
    "eos-agent-run",
    "eos-engine",
    "eos-tool",
    "eos-workflow",
    "eos-types",
    "eos-db",
    "eos-llm-client",
    "eos-sandbox-port",
    "eos-testkit",
];

pub const RETIRED_CRATES: &[&str] = &[
    "eos-runtime",
    "eos-agent-ports",
    "eos-tool-ports",
    "eos-agent-message-records",
    "eos-tools",
    "eos-agent-runner",
    "eos-skills",
    "eos-plugin-catalog",
    "eos-agent-def",
    "eos-config",
    "eos-audit",
];

pub const FORBIDDEN_VOCABULARY: &[&str] = &["composition", "deps", "runtime_services"];

#[derive(Debug, Clone, Copy)]
pub struct RetiredCrateRule {
    pub retired: &'static str,
    pub successor: &'static str,
    pub target: &'static str,
}

pub const RETIRED_CRATE_RULES: &[RetiredCrateRule] = &[
    RetiredCrateRule {
        retired: "eos-runtime",
        successor: "eos-agent-core",
        target: "fold request runtime into eos-agent-core/src/runtime/",
    },
    RetiredCrateRule {
        retired: "eos-agent-ports",
        successor: "eos-agent-core",
        target: "split shared contracts into eos-types and facade-private wiring into eos-agent-core",
    },
    RetiredCrateRule {
        retired: "eos-tool-ports",
        successor: "eos-tool",
        target: "fold tool contracts into eos-tool",
    },
    RetiredCrateRule {
        retired: "eos-agent-message-records",
        successor: "eos-agent-run",
        target: "fold message records into eos-agent-run",
    },
    RetiredCrateRule {
        retired: "eos-tools",
        successor: "eos-tool",
        target: "rename and consolidate as eos-tool",
    },
    RetiredCrateRule {
        retired: "eos-agent-runner",
        successor: "eos-agent-run",
        target: "rename lifecycle crate to eos-agent-run",
    },
    RetiredCrateRule {
        retired: "eos-skills",
        successor: "eos-tool",
        target: "fold skills into eos-tool",
    },
    RetiredCrateRule {
        retired: "eos-plugin-catalog",
        successor: "eos-agent-core",
        target: "fold plugin catalog into eos-agent-core/runtime/plugins.rs",
    },
    RetiredCrateRule {
        retired: "eos-agent-def",
        successor: "eos-agent-core",
        target: "move agent DTOs to eos-types and loader/validation to eos-agent-core/src/agents.rs",
    },
    RetiredCrateRule {
        retired: "eos-config",
        successor: "eos-agent-core",
        target: "move config structs to owners, pure parser to eos-types, loader to eos-agent-core/runtime/config.rs",
    },
    RetiredCrateRule {
        retired: "eos-audit",
        successor: "eos-agent-core",
        target: "fold audit sink into eos-agent-core/src/runtime/audit.rs",
    },
];

#[derive(Debug, Clone)]
pub struct Workspace {
    root: PathBuf,
    crates: BTreeMap<String, CrateInfo>,
}

#[derive(Debug, Clone)]
pub struct CrateInfo {
    pub name: String,
    pub manifest_path: PathBuf,
    pub root_dir: PathBuf,
    pub src_dir: PathBuf,
    pub lib_path: PathBuf,
    pub normal_internal_deps: BTreeSet<String>,
}

#[derive(Debug, Clone)]
pub struct SourceFile {
    pub crate_name: String,
    pub path: PathBuf,
    pub relative_path: String,
    pub text: String,
}

impl Workspace {
    pub fn load() -> Self {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
        let output = Command::new(cargo)
            .args(["metadata", "--format-version=1", "--no-deps"])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .unwrap_or_else(|err| panic!("run `cargo metadata`: {err}"));
        assert!(
            output.status.success(),
            "`cargo metadata` exited non-zero: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        let meta: Value = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|err| panic!("parse cargo metadata json: {err}"));
        let packages = meta
            .get("packages")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("cargo metadata missing packages array"));
        let eos_packages: BTreeSet<String> = packages
            .iter()
            .filter_map(|package| package.get("name").and_then(Value::as_str))
            .filter(|name| name.starts_with("eos-"))
            .map(ToOwned::to_owned)
            .collect();

        let mut crates = BTreeMap::new();
        for package in packages {
            let Some(name) = package.get("name").and_then(Value::as_str) else {
                continue;
            };
            if !eos_packages.contains(name) {
                continue;
            }

            let manifest_path = package
                .get("manifest_path")
                .and_then(Value::as_str)
                .map(PathBuf::from)
                .unwrap_or_else(|| panic!("package {name} missing manifest_path"));
            let root_dir = manifest_path
                .parent()
                .unwrap_or_else(|| {
                    panic!("manifest path has no parent: {}", manifest_path.display())
                })
                .to_path_buf();
            let src_dir = root_dir.join("src");
            let lib_path = package
                .get("targets")
                .and_then(Value::as_array)
                .and_then(|targets| {
                    targets.iter().find_map(|target| {
                        let is_lib =
                            target
                                .get("kind")
                                .and_then(Value::as_array)
                                .is_some_and(|kinds| {
                                    kinds.iter().any(|kind| kind.as_str() == Some("lib"))
                                });
                        is_lib
                            .then(|| target.get("src_path").and_then(Value::as_str))
                            .flatten()
                            .map(PathBuf::from)
                    })
                })
                .unwrap_or_else(|| root_dir.join("src/lib.rs"));
            let normal_internal_deps = package
                .get("dependencies")
                .and_then(Value::as_array)
                .unwrap_or_else(|| panic!("package {name} missing dependencies array"))
                .iter()
                .filter(|dep| dep.get("kind").and_then(Value::as_str) != Some("dev"))
                .filter_map(|dep| dep.get("name").and_then(Value::as_str))
                .filter(|dep_name| eos_packages.contains(*dep_name))
                .map(ToOwned::to_owned)
                .collect();

            crates.insert(
                name.to_owned(),
                CrateInfo {
                    name: name.to_owned(),
                    manifest_path,
                    root_dir,
                    src_dir,
                    lib_path,
                    normal_internal_deps,
                },
            );
        }

        Self {
            root: workspace_root(),
            crates,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn crates(&self) -> &BTreeMap<String, CrateInfo> {
        &self.crates
    }

    pub fn crate_names(&self) -> BTreeSet<String> {
        self.crates.keys().cloned().collect()
    }

    pub fn internal_dependency_edges(&self) -> BTreeMap<String, BTreeSet<String>> {
        self.crates
            .iter()
            .map(|(name, info)| (name.clone(), info.normal_internal_deps.clone()))
            .collect()
    }

    pub fn source_files(&self) -> Vec<SourceFile> {
        let mut files = Vec::new();
        for (crate_name, crate_info) in &self.crates {
            for path in rust_files_under(&crate_info.src_dir) {
                let relative_path = relative_to(&path, &self.root);
                let text = read_to_string(&path);
                files.push(SourceFile {
                    crate_name: crate_name.clone(),
                    path,
                    relative_path,
                    text,
                });
            }
        }
        files.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
        files
    }
}

pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap_or_else(|| panic!("workspace-guard manifest dir has no parent"))
        .to_path_buf()
}

pub fn str_set(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

pub fn rust_files_under(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if root.exists() {
        collect_rust_files(root, &mut files);
    }
    files.sort();
    files
}

pub fn directories_under(root: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if root.exists() {
        collect_dirs(root, &mut dirs);
    }
    dirs.sort();
    dirs
}

pub fn relative_to(path: &Path, root: &Path) -> String {
    let relative = path.strip_prefix(root).unwrap_or(path);
    relative.to_string_lossy().replace('\\', "/")
}

pub fn read_to_string(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
}

pub fn nonblank_line_count(path: &Path) -> usize {
    read_to_string(path)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count()
}

pub fn public_declared_identifiers(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(public_declared_identifier)
        .collect()
}

pub fn public_declared_identifier(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let without_prefix = trimmed.strip_prefix("pub ")?;
    let mut tokens = without_prefix.split(|ch: char| ch.is_whitespace() || ch == '<' || ch == '(');
    match tokens.next()? {
        "struct" | "enum" | "trait" | "type" => tokens.next().map(trim_identifier),
        _ => None,
    }
}

pub fn snake_case(name: &str) -> String {
    let mut out = String::new();
    let mut previous_was_lower_or_digit = false;
    for ch in name.chars() {
        if ch.is_ascii_uppercase() {
            if previous_was_lower_or_digit {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
            previous_was_lower_or_digit = false;
        } else if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            previous_was_lower_or_digit = ch.is_ascii_lowercase() || ch.is_ascii_digit();
        } else {
            if !out.ends_with('_') {
                out.push('_');
            }
            previous_was_lower_or_digit = false;
        }
    }
    out.trim_matches('_').to_owned()
}

fn trim_identifier(token: &str) -> String {
    token
        .trim_matches(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_')
        .to_owned()
}

fn collect_rust_files(root: &Path, files: &mut Vec<PathBuf>) {
    for entry in read_dir(root) {
        let path = entry.path();
        if path.is_dir() {
            collect_rust_files(&path, files);
        } else if path.extension() == Some(OsStr::new("rs")) {
            files.push(path);
        }
    }
}

fn collect_dirs(root: &Path, dirs: &mut Vec<PathBuf>) {
    for entry in read_dir(root) {
        let path = entry.path();
        if path.is_dir() {
            dirs.push(path.clone());
            collect_dirs(&path, dirs);
        }
    }
}

fn read_dir(root: &Path) -> Vec<fs::DirEntry> {
    fs::read_dir(root)
        .unwrap_or_else(|err| panic!("read directory {}: {err}", root.display()))
        .map(|entry| {
            entry.unwrap_or_else(|err| {
                panic!("read directory entry under {}: {err}", root.display())
            })
        })
        .collect()
}
