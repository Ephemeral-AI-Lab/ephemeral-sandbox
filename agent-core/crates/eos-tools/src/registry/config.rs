//! Externalized tool configuration loaded from `.eos-agents/tools/<wire>.md`.
//!
//! Each model-facing tool's **prose** (the body) and **policy** (the `intent` /
//! `terminal` / `hooks` frontmatter) live in one markdown file per tool, loaded
//! and validated here — mirroring how `eos-skills` and the agent profiles already
//! load from `.eos-agents/`. The Rust crate stays the owner of the *behavior*:
//! the executor, the `schemars`-derived schema, the sealed [`Hook`] match logic,
//! and the [`TerminalTool`] descriptor catalog. Only the configuration moves to
//! markdown; this module is the loader + validator.
//!
//! Validation is total and fail-fast (it replaces the compile-time guarantee the
//! old `include_str!` / `meta.rs` tables gave): every [`ToolName`] must have
//! exactly one well-formed file, every file must map to a known tool, the
//! `intent` and each `hooks` token must resolve to the sealed enums, and the
//! declared `terminal` flag must agree with the [`TerminalTool`] catalog.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use eos_config::parse_markdown_frontmatter;
use serde_yaml::{Mapping, Value};
use thiserror::Error;

use crate::core::intent::ToolIntent;
use crate::core::name::ToolName;
use crate::hooks::Hook;
use crate::tools::terminal::TerminalTool;

/// One tool's loaded configuration: the model-facing description plus the policy
/// the registry stamps onto its `RegisteredTool`.
#[derive(Debug, Clone)]
pub struct ToolConfig {
    /// The model-facing description (the markdown body, end-trimmed).
    pub description: String,
    /// Batch-dispatch / sandbox-routing classification.
    pub intent: ToolIntent,
    /// Whether a successful call ends the agent run (validated against the
    /// [`TerminalTool`] catalog at load).
    pub terminal: bool,
    /// The ordered pre-hook chain (order is load-bearing).
    pub hooks: Vec<Hook>,
}

/// The [`ToolName`]-keyed set of tool configs, built once at composition from a
/// `.eos-agents/tools` root and shared immutably.
#[derive(Debug, Clone)]
pub struct ToolConfigSet {
    configs: HashMap<ToolName, ToolConfig>,
}

impl ToolConfigSet {
    /// Load and validate every `<wire>.md` under `root`.
    ///
    /// # Errors
    /// [`ToolConfigError`] for a non-directory root, an I/O failure, a file whose
    /// stem is not a known tool, a missing tool, or any malformed frontmatter
    /// (bad/absent intent, unknown hook token, terminal-flag mismatch, empty
    /// description).
    pub fn load_from_dir(root: &Path) -> Result<Self, ToolConfigError> {
        if !root.is_dir() {
            return Err(ToolConfigError::RootNotDir(root.to_owned()));
        }
        let mut configs = HashMap::new();
        for path in read_dir_sorted(root)? {
            if !path.is_file() || extension(&path) != Some("md") {
                continue;
            }
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            let name = ToolName::from_wire(stem)
                .ok_or_else(|| ToolConfigError::UnknownToolFile(path.clone()))?;
            let content = fs::read_to_string(&path).map_err(|cause| ToolConfigError::ReadFile {
                path: path.clone(),
                cause,
            })?;
            configs.insert(name, parse_tool_config(name, &content)?);
        }
        for name in ToolName::ALL {
            if !configs.contains_key(&name) {
                return Err(ToolConfigError::MissingTool(name));
            }
        }
        Ok(Self { configs })
    }

    /// The config for `name`. Infallible: `load_from_dir` validated totality over
    /// [`ToolName::ALL`], so every name is present.
    #[must_use]
    pub fn get(&self, name: ToolName) -> &ToolConfig {
        self.configs
            .get(&name)
            .expect("ToolConfigSet validated at load: every ToolName is present")
    }
}

/// A failure loading or validating the tool config tree.
#[derive(Debug, Error)]
pub enum ToolConfigError {
    /// The configured root is not a directory.
    #[error("tool config root is not a directory: {0}")]
    RootNotDir(PathBuf),
    /// A directory could not be read.
    #[error("reading tool config dir {path}: {cause}")]
    ReadDir {
        /// The directory.
        path: PathBuf,
        /// The underlying I/O error.
        cause: std::io::Error,
    },
    /// A file could not be read.
    #[error("reading tool config file {path}: {cause}")]
    ReadFile {
        /// The file.
        path: PathBuf,
        /// The underlying I/O error.
        cause: std::io::Error,
    },
    /// A `*.md` file's stem is not a known tool name.
    #[error("tool config file {0} does not name a known tool")]
    UnknownToolFile(PathBuf),
    /// A known tool has no config file.
    #[error("no tool config file for {0}")]
    MissingTool(ToolName),
    /// A tool file omits the `intent` frontmatter key.
    #[error("tool {0} config is missing the `intent` field")]
    MissingIntent(ToolName),
    /// A tool file declares an `intent` that is not a known [`ToolIntent`].
    #[error("tool {tool} config has unknown intent {value:?}")]
    UnknownIntent {
        /// The tool.
        tool: ToolName,
        /// The bad intent string.
        value: String,
    },
    /// A tool file lists a hook token that is not a known [`Hook`].
    #[error("tool {tool} config has unknown hook token {token:?}")]
    UnknownHook {
        /// The tool.
        tool: ToolName,
        /// The bad hook token.
        token: String,
    },
    /// A tool file's `terminal` flag disagrees with the [`TerminalTool`] catalog.
    #[error("tool {tool} config declares terminal={declared} but the catalog says {expected}")]
    TerminalMismatch {
        /// The tool.
        tool: ToolName,
        /// The flag the file declared.
        declared: bool,
        /// The flag the catalog requires.
        expected: bool,
    },
    /// A tool file has an empty description body.
    #[error("tool {0} config has an empty description")]
    EmptyDescription(ToolName),
}

/// Parse one tool's markdown into a validated [`ToolConfig`].
fn parse_tool_config(name: ToolName, content: &str) -> Result<ToolConfig, ToolConfigError> {
    let (frontmatter, description) = parse_markdown_frontmatter(content);
    if description.is_empty() {
        return Err(ToolConfigError::EmptyDescription(name));
    }

    let intent_str = frontmatter
        .get("intent")
        .and_then(Value::as_str)
        .ok_or(ToolConfigError::MissingIntent(name))?;
    let intent = parse_intent(intent_str).ok_or_else(|| ToolConfigError::UnknownIntent {
        tool: name,
        value: intent_str.to_owned(),
    })?;

    let declared_terminal = frontmatter
        .get("terminal")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let expected_terminal = TerminalTool::from_tool_name(name).is_some();
    if declared_terminal != expected_terminal {
        return Err(ToolConfigError::TerminalMismatch {
            tool: name,
            declared: declared_terminal,
            expected: expected_terminal,
        });
    }

    let hooks = parse_hooks(name, &frontmatter)?;
    Ok(ToolConfig {
        description,
        intent,
        terminal: expected_terminal,
        hooks,
    })
}

/// Resolve an `intent` string to a [`ToolIntent`], reusing [`ToolIntent::as_str`]
/// as the single source of the wire spelling.
fn parse_intent(value: &str) -> Option<ToolIntent> {
    [
        ToolIntent::ReadOnly,
        ToolIntent::WriteAllowed,
        ToolIntent::Lifecycle,
    ]
    .into_iter()
    .find(|intent| intent.as_str() == value)
}

/// Parse the `hooks` list (absent ⇒ empty) into ordered [`Hook`]s.
fn parse_hooks(name: ToolName, frontmatter: &Mapping) -> Result<Vec<Hook>, ToolConfigError> {
    let Some(value) = frontmatter.get("hooks") else {
        return Ok(Vec::new());
    };
    let Some(items) = value.as_sequence() else {
        return Err(ToolConfigError::UnknownHook {
            tool: name,
            token: format!("{value:?}"),
        });
    };
    items
        .iter()
        .map(|item| parse_hook_item(name, item))
        .collect()
}

/// Parse one `hooks` entry: a bare `token` string, or a single-key
/// `{token: {params}}` mapping (currently only `max_depth` for
/// `disallow_nested_planner_deferral`).
fn parse_hook_item(name: ToolName, item: &Value) -> Result<Hook, ToolConfigError> {
    let unknown = || ToolConfigError::UnknownHook {
        tool: name,
        token: format!("{item:?}"),
    };
    if let Some(token) = item.as_str() {
        return hook_from_token(name, token, None).ok_or_else(unknown);
    }
    let one_entry = item
        .as_mapping()
        .filter(|map| map.len() == 1)
        .and_then(|map| map.iter().next());
    if let Some((key, params)) = one_entry {
        if let Some(token) = key.as_str() {
            return hook_from_token(name, token, Some(params)).ok_or_else(unknown);
        }
    }
    Err(unknown())
}

/// Default deepest workflow depth still allowed to defer when a
/// `disallow_nested_planner_deferral` entry omits `max_depth` (depth > 1 ⇒
/// nested ⇒ denied — the historical emergent cap).
const DEFAULT_MAX_WORKFLOW_DEPTH: u32 = 1;

/// Map a config token (see [`Hook::config_token`]) to its [`Hook`], filling the
/// `{ tool }` field from the owning tool and any per-hook `params`.
fn hook_from_token(tool: ToolName, token: &str, params: Option<&Value>) -> Option<Hook> {
    Some(match token {
        "no_background_sessions" => Hook::RequireNoBackgroundSessions { tool },
        "advisor_approval" => Hook::AdvisorApproval { tool },
        "disallow_nested_planner_deferral" => Hook::DisallowNestedPlannerDeferral {
            tool,
            max_depth: params
                .and_then(|p| p.get("max_depth"))
                .and_then(Value::as_u64)
                .map_or(DEFAULT_MAX_WORKFLOW_DEPTH, |d| d as u32),
        },
        "destructive_git_shell" => Hook::DestructiveGitShell { tool },
        "destructive_shell" => Hook::DestructiveShell { tool },
        "block_in_isolated_mode" => Hook::BlockInIsolatedMode { tool },
        _ => return None,
    })
}

fn extension(path: &Path) -> Option<&str> {
    path.extension().and_then(|e| e.to_str())
}

/// List a directory's entries as paths, sorted (deterministic load order, parity
/// with `eos-skills`).
fn read_dir_sorted(dir: &Path) -> Result<Vec<PathBuf>, ToolConfigError> {
    let mut paths = Vec::new();
    let entries = fs::read_dir(dir).map_err(|cause| ToolConfigError::ReadDir {
        path: dir.to_owned(),
        cause,
    })?;
    for entry in entries {
        let entry = entry.map_err(|cause| ToolConfigError::ReadDir {
            path: dir.to_owned(),
            cause,
        })?;
        paths.push(entry.path());
    }
    paths.sort();
    Ok(paths)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // unwrap permitted in tests (err-no-unwrap-prod)
    use super::*;

    /// A throwaway temp dir, removed on drop.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "eos-tools-cfg-{tag}-{:?}",
                std::thread::current().id()
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn write(&self, name: &str, content: &str) {
            fs::write(self.0.join(name), content).unwrap();
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Write a minimal valid file body for `name` (terminal flag matches the
    /// catalog, intent valid, non-empty body, no hooks).
    fn valid_file(name: ToolName) -> String {
        let terminal = TerminalTool::from_tool_name(name).is_some();
        format!(
            "---\nintent: read_only\nterminal: {terminal}\nhooks: []\n---\n{name} description\n"
        )
    }

    #[test]
    fn parses_a_well_formed_file() {
        let cfg = parse_tool_config(
            ToolName::ExecCommand,
            "---\nintent: write_allowed\nterminal: false\nhooks: [destructive_git_shell, destructive_shell]\n---\nRun a command.\n",
        )
        .unwrap();
        assert_eq!(cfg.intent, ToolIntent::WriteAllowed);
        assert!(!cfg.terminal);
        assert_eq!(cfg.description, "Run a command.");
        assert_eq!(
            cfg.hooks,
            vec![
                Hook::DestructiveGitShell {
                    tool: ToolName::ExecCommand
                },
                Hook::DestructiveShell {
                    tool: ToolName::ExecCommand
                },
            ]
        );
    }

    #[test]
    fn absent_hooks_is_empty() {
        let cfg = parse_tool_config(
            ToolName::ReadFile,
            "---\nintent: read_only\nterminal: false\n---\nRead a file.\n",
        )
        .unwrap();
        assert!(cfg.hooks.is_empty());
    }

    #[test]
    fn rejects_unknown_intent() {
        let err = parse_tool_config(
            ToolName::ReadFile,
            "---\nintent: bogus\nterminal: false\n---\nbody\n",
        )
        .unwrap_err();
        assert!(
            matches!(err, ToolConfigError::UnknownIntent { .. }),
            "{err:?}"
        );
    }

    #[test]
    fn rejects_missing_intent() {
        let err =
            parse_tool_config(ToolName::ReadFile, "---\nterminal: false\n---\nbody\n").unwrap_err();
        assert!(matches!(err, ToolConfigError::MissingIntent(_)), "{err:?}");
    }

    #[test]
    fn rejects_unknown_hook_token() {
        let err = parse_tool_config(
            ToolName::ReadFile,
            "---\nintent: read_only\nterminal: false\nhooks: [made_up]\n---\nbody\n",
        )
        .unwrap_err();
        assert!(
            matches!(err, ToolConfigError::UnknownHook { .. }),
            "{err:?}"
        );
    }

    #[test]
    fn rejects_terminal_mismatch() {
        // read_file is not in the TerminalTool catalog, so terminal:true is wrong.
        let err = parse_tool_config(
            ToolName::ReadFile,
            "---\nintent: read_only\nterminal: true\n---\nbody\n",
        )
        .unwrap_err();
        assert!(
            matches!(err, ToolConfigError::TerminalMismatch { .. }),
            "{err:?}"
        );
    }

    #[test]
    fn rejects_empty_description() {
        let err = parse_tool_config(
            ToolName::ReadFile,
            "---\nintent: read_only\nterminal: false\n---\n   \n",
        )
        .unwrap_err();
        assert!(
            matches!(err, ToolConfigError::EmptyDescription(_)),
            "{err:?}"
        );
    }

    #[test]
    fn load_rejects_non_directory_root() {
        let err = ToolConfigSet::load_from_dir(Path::new("/no/such/tools/root")).unwrap_err();
        assert!(matches!(err, ToolConfigError::RootNotDir(_)), "{err:?}");
    }

    #[test]
    fn load_rejects_unknown_tool_file() {
        let scratch = Scratch::new("unknown-file");
        for name in ToolName::ALL {
            scratch.write(&format!("{name}.md"), &valid_file(name));
        }
        scratch.write("not_a_tool.md", &valid_file(ToolName::ReadFile));
        let err = ToolConfigSet::load_from_dir(scratch.path()).unwrap_err();
        assert!(
            matches!(err, ToolConfigError::UnknownToolFile(_)),
            "{err:?}"
        );
    }

    #[test]
    fn load_rejects_missing_tool() {
        let scratch = Scratch::new("missing-tool");
        for name in ToolName::ALL {
            if name == ToolName::Glob {
                continue; // drop one
            }
            scratch.write(&format!("{name}.md"), &valid_file(name));
        }
        let err = ToolConfigSet::load_from_dir(scratch.path()).unwrap_err();
        assert!(
            matches!(err, ToolConfigError::MissingTool(ToolName::Glob)),
            "{err:?}"
        );
    }

    #[test]
    fn load_full_valid_set_is_total() {
        let scratch = Scratch::new("full-valid");
        for name in ToolName::ALL {
            scratch.write(&format!("{name}.md"), &valid_file(name));
        }
        let set = ToolConfigSet::load_from_dir(scratch.path()).unwrap();
        for name in ToolName::ALL {
            assert_eq!(set.get(name).description, format!("{name} description"));
        }
    }

    /// Every [`Hook`] variant's `config_token` round-trips through
    /// `hook_from_token` (keeps the forward/inverse maps in sync).
    #[test]
    fn hook_tokens_round_trip() {
        let tool = ToolName::ExecCommand;
        for hook in [
            Hook::RequireNoBackgroundSessions { tool },
            Hook::AdvisorApproval { tool },
            Hook::DisallowNestedPlannerDeferral { tool, max_depth: 1 },
            Hook::DestructiveGitShell { tool },
            Hook::DestructiveShell { tool },
            Hook::BlockInIsolatedMode { tool },
        ] {
            assert_eq!(hook_from_token(tool, hook.config_token(), None), Some(hook));
        }
    }

    /// The parameterized `{disallow_nested_planner_deferral: {max_depth: N}}`
    /// hooks entry parses the configured depth; the bare token defaults.
    #[test]
    fn disallow_nested_max_depth_parses_from_entry() {
        let name = ToolName::SubmitPlannerOutcome;
        let configured: Value =
            serde_yaml::from_str("{disallow_nested_planner_deferral: {max_depth: 3}}").unwrap();
        assert_eq!(
            parse_hook_item(name, &configured).unwrap(),
            Hook::DisallowNestedPlannerDeferral {
                tool: name,
                max_depth: 3,
            }
        );

        let bare: Value = serde_yaml::from_str("disallow_nested_planner_deferral").unwrap();
        assert_eq!(
            parse_hook_item(name, &bare).unwrap(),
            Hook::DisallowNestedPlannerDeferral {
                tool: name,
                max_depth: DEFAULT_MAX_WORKFLOW_DEPTH,
            }
        );
    }
}
