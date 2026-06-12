//! Static sandbox operation contracts and catalog rendering.
//!
//! Canonical grammar: `sandbox.<verb>` for host ops,
//! `sandbox.<service>.<verb>` for daemon ops, and `plugin.<id>.<op>` for
//! dynamic plugin ops. Dynamic plugin operations are runtime-discovered and do
//! not appear in this static catalog.

use serde::Serialize;

/// Static sandbox operation catalog protocol version.
pub const PROTOCOL_VERSION: i64 = 1;

/// Functional owner for a catalog op.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum OpFamily {
    /// Host-side sandbox lifecycle (acquire/release/status/list).
    Sandbox,
    /// Runtime readiness, heartbeat, cancellation, and in-flight accounting.
    Control,
    /// LayerStack base, metrics, and checkpoint materialization.
    Checkpoint,
    /// Shared workspace file read/write/edit operations.
    Files,
    /// Plugin package, service, and dynamic dispatch operations.
    Plugins,
    /// Isolated workspace lifecycle and status operations.
    IsolatedWorkspace,
    /// Command lifecycle, IO, and completion operations.
    Command,
    /// Caller-keyed or whole-sandbox workspace-run cleanup operations.
    WorkspaceRun,
}

impl OpFamily {
    /// Stable spelling used in `crates/eos-operation/ops.json`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sandbox => "Sandbox",
            Self::Control => "Control",
            Self::Checkpoint => "Checkpoint",
            Self::Files => "Files",
            Self::Plugins => "Plugins",
            Self::IsolatedWorkspace => "IsolatedWorkspace",
            Self::Command => "Command",
            Self::WorkspaceRun => "WorkspaceRun",
        }
    }
}

/// Runtime side that serves a static catalog operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ServedBy {
    /// Host-side gateway/registry operation.
    Host,
    /// Sandbox daemon operation.
    Daemon,
}

impl ServedBy {
    /// Stable spelling used in `crates/eos-operation/ops.json`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::Daemon => "daemon",
        }
    }
}

/// Caller surface allowed to invoke an op; `eos-sandbox-gateway` enforces it at
/// the client socket (`visibility != public` -> `forbidden`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum OpVisibility {
    /// Part of the public client vocabulary.
    Public,
    /// Operator socket only; never the client socket.
    Operator,
    /// Host machinery only (recovery ready-gate).
    Internal,
    /// Daemon-side test hook; test builds only.
    Test,
}

impl OpVisibility {
    /// Stable spelling used in `crates/eos-operation/ops.json`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Operator => "operator",
            Self::Internal => "internal",
            Self::Test => "test",
        }
    }
}

macro_rules! declare_builtin_ops {
    (
        $(
            $variant:ident, $const_name:ident, $name:literal,
            $served_by:ident, $family:ident, $visibility:ident, $mutates_state:literal,
            $summary:literal;
        )+
    ) => {
        /// One built-in operation in the static sandbox catalog.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum BuiltinOp {
            $(
                #[doc = concat!("`", $name, "`: ", $summary)]
                $variant,
            )+
        }

        /// Catalog metadata for one built-in operation.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub struct OpContract {
            /// Typed op identity.
            pub op: BuiltinOp,
            /// Canonical wire spelling.
            pub name: &'static str,
            /// Runtime side that serves the operation.
            pub served_by: ServedBy,
            /// Functional owner.
            pub family: OpFamily,
            /// Caller surface that may invoke the op.
            pub visibility: OpVisibility,
            /// Whether the op may change daemon, workspace, or process state.
            pub mutates_state: bool,
            /// One-line summary rendered into `ops.json`.
            pub summary: &'static str,
        }

        $(
            #[doc = concat!("Canonical wire spelling `", $name, "`: ", $summary)]
            pub const $const_name: &str = $name;
        )+

        /// Built-in operation metadata in `ops.json` order.
        pub const BUILTIN_OPS: &[OpContract] = &[
            $(
                OpContract {
                    op: BuiltinOp::$variant,
                    name: $name,
                    served_by: ServedBy::$served_by,
                    family: OpFamily::$family,
                    visibility: OpVisibility::$visibility,
                    mutates_state: $mutates_state,
                    summary: $summary,
                },
            )+
        ];

        impl BuiltinOp {
            /// Protocol catalog entry for this op.
            #[must_use]
            pub fn contract(self) -> &'static OpContract {
                BUILTIN_OPS
                    .iter()
                    .find(|contract| contract.op == self)
                    .expect("builtin op must be declared in BUILTIN_OPS")
            }

            /// Resolve a canonical operation name into its typed catalog identity.
            #[must_use]
            pub fn from_op_name(name: &str) -> Option<Self> {
                match name {
                    $($name => Some(Self::$variant),)+
                    _ => None,
                }
            }
        }
    };
}

declare_builtin_ops! {
    SandboxAcquire, SANDBOX_ACQUIRE, "sandbox.acquire",
        Host, Sandbox, Public, true, "Provision a sandbox container plus daemon and return its sandbox_id.";
    SandboxRelease, SANDBOX_RELEASE, "sandbox.release",
        Host, Sandbox, Public, true, "Destroy the sandbox container and drop its registry entry.";
    SandboxStatus, SANDBOX_STATUS, "sandbox.status",
        Host, Sandbox, Public, false, "Host view of one sandbox (container/endpoint/recovery state) plus embedded daemon readiness.";
    SandboxList, SANDBOX_LIST, "sandbox.list",
        Host, Sandbox, Public, false, "Enumerate the sandbox registry.";
    RuntimeReady, SANDBOX_RUNTIME_READY, "sandbox.runtime.ready",
        Daemon, Control, Internal, false, "Daemon readiness probe used by the host recovery machine.";
    InvocationHeartbeat, SANDBOX_CALL_HEARTBEAT, "sandbox.call.heartbeat",
        Daemon, Control, Public, true, "Extend the lease on an in-flight invocation.";
    InvocationCancel, SANDBOX_CALL_CANCEL, "sandbox.call.cancel",
        Daemon, Control, Public, true, "Request cooperative cancellation of an in-flight invocation.";
    InflightCount, SANDBOX_CALL_COUNT, "sandbox.call.count",
        Daemon, Control, Public, false, "Count in-flight invocations.";
    TraceExport, SANDBOX_TRACE_EXPORT, "sandbox.trace.export",
        Daemon, Control, Internal, false, "Drain bounded daemon background trace records for host ingest.";
    LayerMetrics, SANDBOX_CHECKPOINT_LAYER_METRICS, "sandbox.checkpoint.layer_metrics",
        Daemon, Checkpoint, Operator, false, "Report LayerStack and storage metrics for the sandbox.";
    EnsureWorkspaceBase, SANDBOX_CHECKPOINT_ENSURE_BASE, "sandbox.checkpoint.ensure_base",
        Daemon, Checkpoint, Operator, true, "Ensure a workspace base binding exists.";
    BuildWorkspaceBase, SANDBOX_CHECKPOINT_BUILD_BASE, "sandbox.checkpoint.build_base",
        Daemon, Checkpoint, Operator, true, "Build or rebuild a workspace base binding.";
    CommitToWorkspace, SANDBOX_CHECKPOINT_COMMIT_TO_WORKSPACE, "sandbox.checkpoint.commit_to_workspace",
        Daemon, Checkpoint, Operator, true, "Materialize LayerStack state into the bound workspace.";
    CommitToGit, SANDBOX_CHECKPOINT_COMMIT_TO_GIT, "sandbox.checkpoint.commit_to_git",
        Daemon, Checkpoint, Operator, true, "Commit a LayerStack snapshot into the bound workspace's durable Git repo.";
    WorkspaceBinding, SANDBOX_CHECKPOINT_BINDING, "sandbox.checkpoint.binding",
        Daemon, Checkpoint, Operator, false, "Inspect the workspace binding for a layer stack root.";
    ReadFile, SANDBOX_FILE_READ, "sandbox.file.read",
        Daemon, Files, Public, false, "Read one file from the layer stack or isolated workspace.";
    WriteFile, SANDBOX_FILE_WRITE, "sandbox.file.write",
        Daemon, Files, Public, true, "Write one file through the OCC gate.";
    EditFile, SANDBOX_FILE_EDIT, "sandbox.file.edit",
        Daemon, Files, Public, true, "Edit one file through the OCC gate.";
    PluginEnsure, SANDBOX_PLUGIN_ENSURE, "sandbox.plugin.ensure",
        Daemon, Plugins, Public, true, "Ensure a plugin service is installed and running.";
    PluginStatus, SANDBOX_PLUGIN_STATUS, "sandbox.plugin.status",
        Daemon, Plugins, Public, false, "Inspect plugin service status.";
    IsolatedWorkspaceEnter, SANDBOX_ISOLATION_ENTER, "sandbox.isolation.enter",
        Daemon, IsolatedWorkspace, Public, true, "Enter isolated workspace mode for a caller.";
    IsolatedWorkspaceExit, SANDBOX_ISOLATION_EXIT, "sandbox.isolation.exit",
        Daemon, IsolatedWorkspace, Public, true, "Exit isolated workspace mode for a caller.";
    IsolatedWorkspaceStatus, SANDBOX_ISOLATION_STATUS, "sandbox.isolation.status",
        Daemon, IsolatedWorkspace, Public, false, "Inspect isolated workspace status.";
    IsolatedWorkspaceListOpen, SANDBOX_ISOLATION_LIST_OPEN, "sandbox.isolation.list_open",
        Daemon, IsolatedWorkspace, Operator, false, "List open isolated workspaces.";
    IsolatedWorkspaceTestReset, SANDBOX_ISOLATION_TEST_RESET, "sandbox.isolation.test_reset",
        Daemon, IsolatedWorkspace, Test, true, "Test-only isolated workspace reset hook.";
    ExecCommand, SANDBOX_COMMAND_EXEC, "sandbox.command.exec",
        Daemon, Command, Public, true, "Run a foreground command or start a background command.";
    WriteStdin, SANDBOX_COMMAND_WRITE_STDIN, "sandbox.command.write_stdin",
        Daemon, Command, Public, true, "Write stdin to a command.";
    CommandReadProgress, SANDBOX_COMMAND_POLL, "sandbox.command.poll",
        Daemon, Command, Public, false, "Poll command progress without writing stdin.";
    CommandCancel, SANDBOX_COMMAND_CANCEL, "sandbox.command.cancel",
        Daemon, Command, Public, true, "Cancel a command.";
    CommandCollectCompleted, SANDBOX_COMMAND_COLLECT_COMPLETED, "sandbox.command.collect_completed",
        Daemon, Command, Public, true, "Collect completed command notifications.";
    CommandCount, SANDBOX_COMMAND_COUNT, "sandbox.command.count",
        Daemon, Command, Public, false, "Count live commands.";
    CancelWorkspaceRunsByCaller, SANDBOX_RUN_END, "sandbox.run.end",
        Daemon, WorkspaceRun, Public, true, "End a run: cancel every workspace run owned by one caller (caller_id == agent_run_id), discarding its commands and exiting its isolated workspace.";
    CancelWorkspaceRuns, SANDBOX_RUN_CANCEL_ALL, "sandbox.run.cancel_all",
        Daemon, WorkspaceRun, Operator, true, "Cancel every workspace run in the sandbox: the whole-sandbox sweep backstop.";
}

#[derive(Serialize)]
struct CatalogOp {
    name: &'static str,
    served_by: &'static str,
    visibility: &'static str,
    family: &'static str,
    mutates_state: bool,
    summary: &'static str,
}

#[derive(Serialize)]
struct CatalogDocument {
    protocol_version: i64,
    ops: Vec<CatalogOp>,
}

/// Render the static `ops.json` document.
///
/// Pretty-printed with a trailing newline so `eosd dump-ops` output can be
/// committed and diffed byte-for-byte.
#[must_use]
pub fn ops_json_document() -> String {
    let document = CatalogDocument {
        protocol_version: PROTOCOL_VERSION,
        ops: BUILTIN_OPS
            .iter()
            .map(|contract| CatalogOp {
                name: contract.name,
                served_by: contract.served_by.as_str(),
                visibility: contract.visibility.as_str(),
                family: contract.family.as_str(),
                mutates_state: contract.mutates_state,
                summary: contract.summary,
            })
            .collect(),
    };
    let mut body =
        serde_json::to_string_pretty(&document).expect("static catalog always serializes");
    body.push('\n');
    body
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn builtin_contracts_are_returned_by_ops() {
        for contract in BUILTIN_OPS {
            assert_eq!(contract, contract.op.contract());
        }
    }

    #[test]
    fn canonical_names_resolve_to_builtin_ops() {
        for contract in BUILTIN_OPS {
            assert_eq!(BuiltinOp::from_op_name(contract.name), Some(contract.op));
        }
        assert_eq!(BuiltinOp::from_op_name("plugin.dynamic.echo"), None);
        assert_eq!(BuiltinOp::from_op_name(""), None);
    }

    #[test]
    fn canonical_names_follow_grammar() {
        for contract in BUILTIN_OPS {
            assert!(
                contract.name.starts_with("sandbox."),
                "static op {} must use the sandbox.* grammar",
                contract.name
            );
            assert!(
                !contract.name.split('.').any(|token| token == "v1"),
                "the v1 token is dead in canonical names: {}",
                contract.name
            );
            if contract.served_by == ServedBy::Host {
                assert_eq!(
                    contract.name.split('.').count(),
                    2,
                    "host op {} must be sandbox.<verb>",
                    contract.name
                );
            }
        }
    }

    #[test]
    fn no_spelling_is_claimed_twice() {
        let mut spellings = BTreeSet::new();
        for contract in BUILTIN_OPS {
            assert!(
                spellings.insert(contract.name),
                "spelling claimed twice in the catalog: {}",
                contract.name
            );
        }
    }

    #[test]
    fn ops_json_document_is_complete_and_stable() {
        let document = ops_json_document();
        let parsed: serde_json::Value =
            serde_json::from_str(&document).expect("document parses back");
        assert_eq!(parsed["protocol_version"], PROTOCOL_VERSION);
        let ops = parsed["ops"].as_array().expect("ops array");
        assert_eq!(ops.len(), BUILTIN_OPS.len());
        assert!(document.ends_with('\n'));
    }
}
