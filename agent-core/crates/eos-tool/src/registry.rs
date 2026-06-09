//! Registry, executor, runtime, and tool configuration.

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use eos_sandbox_port::SandboxTransport;
use eos_types::{
    AgentRunApi, AgentRunId, AttemptSubmissionPort, CommandSessionId, RequestStore, SandboxId,
    StartedWorkflow, TaskStore, WorkflowApi,
};

use crate::{SkillRegistry, ToolError};

mod executor {
    //! [`ToolExecutor`] — the object-safe execute seam — and [`RegisteredTool`],
    //! the bundle of an executor with its static registry metadata.

    use std::sync::Arc;

    use async_trait::async_trait;
    use eos_types::JsonObject;
    use eos_types::ToolSpec;

    use crate::hooks::Hook;
    use crate::ExecutionMetadata;
    use crate::ToolError;
    use crate::ToolIntent;
    use crate::ToolKey;
    use crate::{OutputShape, ToolResult};

    /// Execute against already-parsed, hook-validated input.
    ///
    /// Used behind `dyn` in the registry (heterogeneous tool storage), so it carries
    /// `#[async_trait]` (native async-fn-in-trait is not yet `dyn`-safe, anchor §6).
    /// The executor self-parses its typed input from `input` (the framework applies
    /// only the generic `background`-key rejection); a tool-domain failure (bad args,
    /// "tool said no") is an in-band [`ToolResult`]`{is_error:true}` returned as `Ok`,
    /// while a framework fault is [`ToolError`] (`error.rs`).
    #[async_trait]
    pub trait ToolExecutor: Send + Sync {
        /// Run the tool body.
        async fn execute(
            &self,
            input: &JsonObject,
            ctx: &ExecutionMetadata,
        ) -> Result<ToolResult, ToolError>;
    }

    /// An executor bundled with its static metadata. Built once at composition;
    /// stored in the immutable [`ToolRegistry`](crate::ToolRegistry).
    #[derive(Clone)]
    pub struct RegisteredTool {
        /// The typed tool name (the registry key).
        pub name: ToolKey,
        /// Batch-dispatch / sandbox-routing classification.
        pub intent: ToolIntent,
        /// Whether a successful call ends the agent run (stamped by the pipeline).
        pub is_terminal: bool,
        /// The neutral model-facing declaration (owned by `eos-llm-client`, §5a).
        pub spec: ToolSpec,
        /// The pre-hooks run before the body, in order.
        pub hooks: Vec<Hook>,
        /// The declared output shape the pipeline validates against.
        pub(crate) output: OutputShape,
        /// The executor implementation.
        pub(crate) executor: Arc<dyn ToolExecutor>,
    }

    impl std::fmt::Debug for RegisteredTool {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("RegisteredTool")
                .field("name", &self.name)
                .field("intent", &self.intent)
                .field("is_terminal", &self.is_terminal)
                .field("hooks", &self.hooks)
                .field("output", &self.output)
                .finish_non_exhaustive()
        }
    }

    impl RegisteredTool {
        /// Build a registered tool with no hooks.
        #[must_use]
        pub fn new(
            name: impl Into<ToolKey>,
            intent: ToolIntent,
            is_terminal: bool,
            spec: ToolSpec,
            output: OutputShape,
            executor: Arc<dyn ToolExecutor>,
        ) -> Self {
            Self {
                name: name.into(),
                intent,
                is_terminal,
                spec,
                hooks: Vec::new(),
                output,
                executor,
            }
        }

        /// Attach the pre-hooks (builder-style).
        #[must_use]
        pub fn with_hooks(mut self, hooks: Vec<Hook>) -> Self {
            self.hooks = hooks;
            self
        }

        /// The declared output shape.
        #[must_use]
        pub fn output(&self) -> &OutputShape {
            &self.output
        }

        /// The executor implementation.
        #[must_use]
        pub fn executor(&self) -> &dyn ToolExecutor {
            &*self.executor
        }
    }
}

mod tool_registry {
    //! [`ToolRegistry`] — the insertion-ordered, [`ToolKey`]-keyed tool store.
    //!
    //! Ports `_framework/core/registry.py`. Keyed by [`ToolKey`] (not bare `String`)
    //! so built-in tools and plugin tools share one registry surface. Insertion
    //! order is preserved (`Vec` + index map) so [`ToolRegistry::specs`] is
    //! deterministic for the Phase-4 schema-parity snapshot. Built once at
    //! composition and shared immutably as `Arc<ToolRegistry>`; `restrict`/`remove`
    //! run during per-agent construction before sharing.

    use std::collections::HashMap;

    use eos_types::ToolSpec;

    use super::RegisteredTool;
    use crate::ToolKey;

    /// An insertion-ordered registry of [`RegisteredTool`]s.
    #[derive(Debug, Default)]
    pub struct ToolRegistry {
        tools: Vec<RegisteredTool>,
        index: HashMap<ToolKey, usize>,
    }

    impl ToolRegistry {
        /// An empty registry.
        #[must_use]
        pub fn new() -> Self {
            Self::default()
        }

        /// Register a tool. Re-registering a name replaces it **in place** (keeping
        /// its position), mirroring the Rust dict assignment.
        pub fn register(&mut self, tool: RegisteredTool) {
            if let Some(&idx) = self.index.get(&tool.name) {
                self.tools[idx] = tool;
            } else {
                let idx = self.tools.len();
                self.index.insert(tool.name.clone(), idx);
                self.tools.push(tool);
            }
        }

        /// Look up a tool by name.
        #[must_use]
        pub fn get(&self, name: impl Into<ToolKey>) -> Option<&RegisteredTool> {
            let name = name.into();
            self.index.get(&name).map(|&idx| &self.tools[idx])
        }

        /// Look up a tool by provider/model wire name.
        #[must_use]
        pub fn get_wire(&self, name: &str) -> Option<&RegisteredTool> {
            ToolKey::from_wire(name).and_then(|name| self.get(name))
        }

        /// Iterate tools in insertion order.
        pub fn list(&self) -> impl Iterator<Item = &RegisteredTool> {
            self.tools.iter()
        }

        /// Remove the named tools (no-op for absent names).
        pub fn remove(&mut self, names: &[ToolKey]) {
            let drop: std::collections::HashSet<ToolKey> = names.iter().cloned().collect();
            self.tools.retain(|tool| !drop.contains(&tool.name));
            self.reindex();
        }

        /// Keep only the named tools, preserving their current order.
        pub fn restrict(&mut self, names: &[ToolKey]) {
            let keep: std::collections::HashSet<ToolKey> = names.iter().cloned().collect();
            self.tools.retain(|tool| keep.contains(&tool.name));
            self.reindex();
        }

        /// The model-facing specs in insertion order (replaces `to_api_schema`).
        #[must_use]
        pub fn specs(&self) -> Vec<ToolSpec> {
            self.tools.iter().map(|tool| tool.spec.clone()).collect()
        }

        /// The number of registered tools.
        #[must_use]
        pub fn len(&self) -> usize {
            self.tools.len()
        }

        /// Whether the registry is empty.
        #[must_use]
        pub fn is_empty(&self) -> bool {
            self.tools.is_empty()
        }

        fn reindex(&mut self) {
            self.index.clear();
            for (idx, tool) in self.tools.iter().enumerate() {
                self.index.insert(tool.name.clone(), idx);
            }
        }
    }
}

mod config {
    //! Externalized tool configuration loaded from `.eos-agents/tools/<wire>.md`.
    //!
    //! Each model-facing tool's **prose** (the body) and **policy** (the `intent` /
    //! `terminal` / `hooks` frontmatter) live in one markdown file per tool, loaded
    //! and validated here — mirroring how tool-owned skills and the agent profiles
    //! already load from `.eos-agents/`. The Rust crate stays the owner of the *behavior*:
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

    use eos_types::parse_markdown_frontmatter;
    use serde_yaml::{Mapping, Value};
    use thiserror::Error;

    use crate::tools::terminal::TerminalTool;
    use crate::Hook;
    use crate::ToolIntent;
    use crate::ToolName;

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
                let content =
                    fs::read_to_string(&path).map_err(|cause| ToolConfigError::ReadFile {
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

        /// Return a config set whose planner-deferral depth hooks use the workflow
        /// runtime depth bound.
        #[must_use]
        pub fn with_workflow_max_depth(mut self, max_depth: u32) -> Self {
            for config in self.configs.values_mut() {
                for hook in &mut config.hooks {
                    if let Hook::DisallowNestedPlannerDeferral {
                        max_depth: depth, ..
                    } = hook
                    {
                        *depth = max_depth;
                    }
                }
            }
            self
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
        let intent =
            ToolIntent::from_wire(intent_str).ok_or_else(|| ToolConfigError::UnknownIntent {
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
    /// with the tool-owned skill loader).
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
                    "eos-tool-cfg-{tag}-{:?}",
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
            let err = parse_tool_config(ToolName::ReadFile, "---\nterminal: false\n---\nbody\n")
                .unwrap_err();
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
                if name == ToolName::ReadFile {
                    continue; // drop one
                }
                scratch.write(&format!("{name}.md"), &valid_file(name));
            }
            let err = ToolConfigSet::load_from_dir(scratch.path()).unwrap_err();
            assert!(
                matches!(err, ToolConfigError::MissingTool(ToolName::ReadFile)),
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

        #[test]
        fn workflow_max_depth_overrides_deferral_hooks() {
            let cfg = parse_tool_config(
            ToolName::SubmitPlannerOutcome,
            "---\nintent: read_only\nterminal: true\nhooks: [{disallow_nested_planner_deferral: {max_depth: 1}}]\n---\nSubmit plan.\n",
        )
        .unwrap();
            let set = ToolConfigSet {
                configs: HashMap::from([(ToolName::SubmitPlannerOutcome, cfg)]),
            }
            .with_workflow_max_depth(2);

            assert_eq!(
                set.get(ToolName::SubmitPlannerOutcome).hooks,
                vec![Hook::DisallowNestedPlannerDeferral {
                    tool: ToolName::SubmitPlannerOutcome,
                    max_depth: 2,
                }]
            );
        }
    }
}

mod spec {
    //! Spec-building helpers (anchor §10).
    //!
    //! Each model-facing tool's registration site builds **one** [`ToolSpec`] from
    //! its externalized description (loaded from `.eos-agents/tools/*.md` via
    //! [`ToolConfigSet`](crate::registry::ToolConfigSet)) plus `schemars`-derived
    //! input/output schemas (no docstring fallback, GC-tools-02/09). These helpers
    //! convert a `schemars` schema into the `JsonObject` shape `ToolSpec` carries.

    use eos_types::JsonObject;
    use eos_types::ToolSpec;
    use schemars::schema::RootSchema;
    use serde_json::Value;

    use crate::ToolName;

    /// Convert a `schemars` schema into the `ToolSpec` `input_schema`/`output_schema`
    /// object shape.
    #[must_use]
    pub(crate) fn schema_to_object(schema: RootSchema) -> JsonObject {
        match serde_json::to_value(schema) {
            Ok(Value::Object(map)) => map,
            _ => JsonObject::new(),
        }
    }

    /// A spec for a plain-text-output tool (`TextToolOutput`): input schema only.
    #[must_use]
    pub(crate) fn text_spec(name: ToolName, description: &str, input: RootSchema) -> ToolSpec {
        ToolSpec::new(name.as_str(), description, schema_to_object(input), None)
    }

    /// A spec for a structured-output tool: input + output schema.
    #[must_use]
    pub(crate) fn json_spec(
        name: ToolName,
        description: &str,
        input: RootSchema,
        output: RootSchema,
    ) -> ToolSpec {
        ToolSpec::new(
            name.as_str(),
            description,
            schema_to_object(input),
            Some(schema_to_object(output)),
        )
    }

    /// Build a text-output spec whose already-built input-schema object has its
    /// `agent_name` property `enum`-restricted to `allowed` — the per-caller
    /// `RestrictedRunSubagentTool` patch (§6.6). The enum is injected into both the
    /// top-level `properties.agent_name` and (when present) the `$defs`-free inline
    /// schema so the emitted spec reflects the caller-scoped choices.
    #[must_use]
    pub(crate) fn text_spec_with_agent_enum(
        name: ToolName,
        description: &str,
        input: RootSchema,
        allowed: &[String],
    ) -> ToolSpec {
        let mut object = schema_to_object(input);
        patch_agent_enum(&mut object, allowed);
        ToolSpec::new(name.as_str(), description, object, None)
    }

    fn patch_agent_enum(schema: &mut JsonObject, allowed: &[String]) {
        if let Some(Value::Object(props)) = schema.get_mut("properties") {
            if let Some(Value::Object(agent)) = props.get_mut("agent_name") {
                agent.insert(
                    "enum".to_owned(),
                    Value::Array(allowed.iter().map(|a| Value::String(a.clone())).collect()),
                );
            }
        }
    }
}

pub use crate::tools::{build_default_registry, build_registry_schema, CallerScope};
pub use config::{ToolConfig, ToolConfigError, ToolConfigSet};
pub use executor::{RegisteredTool, ToolExecutor};
pub(crate) use spec::{json_spec, text_spec, text_spec_with_agent_enum};
pub use tool_registry::ToolRegistry;

/// Store and attempt-submission resources used by terminal tools.
#[derive(Clone)]
pub struct Submission {
    inner: Arc<SubmissionInner>,
}

struct SubmissionInner {
    task_store: Arc<dyn TaskStore>,
    request_store: Arc<dyn RequestStore>,
    attempt: Arc<dyn AttemptSubmissionPort>,
}

impl Submission {
    /// Build terminal-submission resources.
    #[must_use]
    pub fn new(
        task_store: Arc<dyn TaskStore>,
        request_store: Arc<dyn RequestStore>,
        attempt: Arc<dyn AttemptSubmissionPort>,
    ) -> Self {
        Self {
            inner: Arc::new(SubmissionInner {
                task_store,
                request_store,
                attempt,
            }),
        }
    }

    pub(crate) fn task_store(&self) -> Result<Arc<dyn TaskStore>, ToolError> {
        Ok(self.inner.task_store.clone())
    }

    pub(crate) fn request_store(&self) -> Result<Arc<dyn RequestStore>, ToolError> {
        Ok(self.inner.request_store.clone())
    }

    pub(crate) fn attempt(&self) -> Result<Arc<dyn AttemptSubmissionPort>, ToolError> {
        Ok(self.inner.attempt.clone())
    }
}

impl fmt::Debug for Submission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Submission").finish_non_exhaustive()
    }
}

/// Object-safe background-session registration used by background-producing tools.
#[async_trait]
pub trait BackgroundSessions: Send + Sync {
    /// Register a detached subagent run.
    async fn register_subagent(&self, run: AgentRunId) -> Result<(), ToolError>;
    /// Register a background command session.
    async fn register_command(
        &self,
        id: CommandSessionId,
        sandbox: SandboxId,
    ) -> Result<(), ToolError>;
    /// Register a delegated workflow.
    async fn register_workflow(&self, started: StartedWorkflow) -> Result<(), ToolError>;
    /// Cancel one detached subagent run.
    async fn cancel_subagent(&self, run: AgentRunId, reason: &str) -> Result<bool, ToolError>;
}

/// Run-local isolated-workspace mode update.
#[async_trait]
pub trait WorkspaceMode: Send + Sync {
    /// Update whether the calling agent currently has an isolated workspace.
    async fn set_isolated_workspace_mode(
        &self,
        agent_run_id: AgentRunId,
        is_isolated: bool,
    ) -> Result<(), ToolError>;
}

/// Runtime handles captured by concrete tool executors.
#[derive(Clone)]
pub struct ToolRuntime {
    /// Sandbox transport for file/edit/isolated-workspace tools.
    pub sandbox: Arc<dyn SandboxTransport>,
    /// Delegated-workflow API.
    pub workflow: Arc<dyn WorkflowApi>,
    /// Agent-run launcher API.
    pub launcher: Arc<dyn AgentRunApi>,
    /// Skill registry.
    pub skills: Arc<SkillRegistry>,
    /// Terminal submission resources.
    pub submission: Submission,
    /// Background session registration/cancellation.
    pub background: Arc<dyn BackgroundSessions>,
    /// Isolated-workspace mode update.
    pub workspace_mode: Arc<dyn WorkspaceMode>,
}

impl fmt::Debug for ToolRuntime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ToolRuntime").finish_non_exhaustive()
    }
}
