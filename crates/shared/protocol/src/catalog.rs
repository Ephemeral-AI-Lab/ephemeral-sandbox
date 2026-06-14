//! Static gateway/sandbox operation contracts and catalog rendering.
//!
//! Canonical grammar: `host.<service>.<verb>` for host/fleet ops and
//! `sandbox.<service>.<verb>` for daemon ops.

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
    /// Host-side trace query and audit verification.
    Trace,
    /// Host-side image/profile policy and Docker image operations.
    Image,
    /// Host-side Docker container lifecycle operations.
    Container,
    /// LayerStack base, metrics, and checkpoint materialization.
    Checkpoint,
    /// Shared workspace file read/write/edit operations.
    Files,
    /// Static first-party plugin provider operations.
    Plugins,
    /// Isolated workspace lifecycle and status operations.
    IsolatedWorkspace,
    /// Command lifecycle, IO, and completion operations.
    Command,
    /// Caller-keyed or whole-sandbox workspace-run cleanup operations.
    WorkspaceRun,
}

impl OpFamily {
    /// Stable spelling used in `crates/daemon/operation/ops.json`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sandbox => "Sandbox",
            Self::Control => "Control",
            Self::Trace => "Trace",
            Self::Image => "Image",
            Self::Container => "Container",
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
    /// Stable spelling used in `crates/daemon/operation/ops.json`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::Daemon => "daemon",
        }
    }
}

/// Host-side operation implementation selected by the gateway for host-served
/// catalog entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum HostVerb {
    Acquire,
    Release,
    Status,
    List,
    TraceRequests,
    TraceShow,
    TraceVerify,
    ImageProfilesList,
    ImageList,
    ImagePull,
    ContainerList,
    ContainerStart,
    ContainerAdopt,
    ContainerStop,
    ContainerRemove,
}

impl HostVerb {
    /// Stable spelling rendered into `ops.json`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Acquire => "acquire",
            Self::Release => "release",
            Self::Status => "status",
            Self::List => "list",
            Self::TraceRequests => "trace_requests",
            Self::TraceShow => "trace_show",
            Self::TraceVerify => "trace_verify",
            Self::ImageProfilesList => "image_profiles_list",
            Self::ImageList => "image_list",
            Self::ImagePull => "image_pull",
            Self::ContainerList => "container_list",
            Self::ContainerStart => "container_start",
            Self::ContainerAdopt => "container_adopt",
            Self::ContainerStop => "container_stop",
            Self::ContainerRemove => "container_remove",
        }
    }
}

/// Caller surface allowed to invoke an op; `gateway` enforces it at
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
    /// Stable spelling used in `crates/daemon/operation/ops.json`.
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
            $host_verb:expr, $args_schema:literal, $response_schema:literal, $summary:literal;
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
            /// Host implementation selected by the gateway for host-served ops.
            pub host_verb: Option<HostVerb>,
            /// Stable args DTO/schema descriptor.
            pub args_schema: &'static str,
            /// Stable response DTO/schema descriptor.
            pub response_schema: &'static str,
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
                    host_verb: $host_verb,
                    args_schema: $args_schema,
                    response_schema: $response_schema,
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
    HostSandboxAcquire, HOST_SANDBOX_ACQUIRE, "host.sandbox.acquire",
        Host, Sandbox, Public, true, Some(HostVerb::Acquire), "host.sandbox.AcquireArgs", "host.sandbox.AcquireResponse", "Provision a sandbox container plus daemon and return its sandbox_id.";
    HostSandboxRelease, HOST_SANDBOX_RELEASE, "host.sandbox.release",
        Host, Sandbox, Public, true, Some(HostVerb::Release), "host.sandbox.ReleaseArgs", "host.sandbox.ReleaseResponse", "Destroy the sandbox container and drop its registry entry.";
    HostSandboxStatus, HOST_SANDBOX_STATUS, "host.sandbox.status",
        Host, Sandbox, Public, false, Some(HostVerb::Status), "host.sandbox.StatusArgs", "host.sandbox.StatusResponse", "Host view of one sandbox (container/endpoint/recovery state) plus embedded daemon readiness.";
    HostSandboxList, HOST_SANDBOX_LIST, "host.sandbox.list",
        Host, Sandbox, Public, false, Some(HostVerb::List), "host.sandbox.ListArgs", "host.sandbox.ListResponse", "Enumerate the sandbox registry.";
    HostTraceRequests, HOST_TRACE_REQUESTS, "host.trace.requests",
        Host, Trace, Operator, false, Some(HostVerb::TraceRequests), "host.trace.TraceRequestsArgs", "host.trace.TraceRequestsResponse", "List recent trace requests from the host audit store.";
    HostTraceShow, HOST_TRACE_SHOW, "host.trace.show",
        Host, Trace, Operator, false, Some(HostVerb::TraceShow), "host.trace.TraceShowArgs", "host.trace.TraceShowResponse", "Show one trace from the host audit projections.";
    HostTraceVerify, HOST_TRACE_VERIFY, "host.trace.verify",
        Host, Trace, Operator, false, Some(HostVerb::TraceVerify), "host.trace.TraceVerifyArgs", "host.trace.TraceVerifyReport", "Verify host audit hash chains and projection joinability.";
    HostImageProfilesList, HOST_IMAGE_PROFILES_LIST, "host.image_profiles.list",
        Host, Image, Public, false, Some(HostVerb::ImageProfilesList), "host.image_profiles.ListArgs", "host.image_profiles.ListResponse", "List operator-approved image profiles that public clients may request.";
    HostImageList, HOST_IMAGE_LIST, "host.image.list",
        Host, Image, Operator, false, Some(HostVerb::ImageList), "host.image.ListArgs", "host.image.ListResponse", "List locally available Docker images visible to the gateway host.";
    HostImagePull, HOST_IMAGE_PULL, "host.image.pull",
        Host, Image, Operator, true, Some(HostVerb::ImagePull), "host.image.PullArgs", "host.image.PullResponse", "Pull or refresh an operator-approved image reference.";
    HostContainerList, HOST_CONTAINER_LIST, "host.container.list",
        Host, Container, Operator, false, Some(HostVerb::ContainerList), "host.container.ListArgs", "host.container.ListResponse", "List Docker containers relevant to the gateway host.";
    HostContainerStart, HOST_CONTAINER_START, "host.container.start",
        Host, Container, Operator, true, Some(HostVerb::ContainerStart), "host.container.StartArgs", "host.container.StartResponse", "Start a container from an explicit image reference and optional name.";
    HostContainerAdopt, HOST_CONTAINER_ADOPT, "host.container.adopt",
        Host, Container, Operator, true, Some(HostVerb::ContainerAdopt), "host.container.AdoptArgs", "host.container.AdoptResponse", "Register an existing compatible container as a managed sandbox.";
    HostContainerStop, HOST_CONTAINER_STOP, "host.container.stop",
        Host, Container, Operator, true, Some(HostVerb::ContainerStop), "host.container.StopArgs", "host.container.StopResponse", "Stop a host container by container name/id or managed sandbox_id.";
    HostContainerRemove, HOST_CONTAINER_REMOVE, "host.container.remove",
        Host, Container, Operator, true, Some(HostVerb::ContainerRemove), "host.container.RemoveArgs", "host.container.RemoveResponse", "Remove a host container by container name/id or managed sandbox_id.";
    RuntimeReady, SANDBOX_RUNTIME_READY, "sandbox.runtime.ready",
        Daemon, Control, Internal, false, None, "operation.control.RuntimeReadyInput", "operation.control.RuntimeReadyOutput", "Daemon readiness probe used by the host recovery machine.";
    InvocationHeartbeat, SANDBOX_CALL_HEARTBEAT, "sandbox.call.heartbeat",
        Daemon, Control, Public, true, None, "operation.control.HeartbeatInput", "operation.control.HeartbeatOutput", "Extend the lease on an in-flight invocation.";
    InvocationCancel, SANDBOX_CALL_CANCEL, "sandbox.call.cancel",
        Daemon, Control, Public, true, None, "operation.control.CancelInvocationInput", "operation.control.CancelInvocationOutput", "Request cooperative cancellation of an in-flight invocation.";
    InflightCount, SANDBOX_CALL_COUNT, "sandbox.call.count",
        Daemon, Control, Public, false, None, "operation.control.CallerCountInput", "operation.control.InflightCountOutput", "Count in-flight invocations.";
    TraceExport, SANDBOX_TRACE_EXPORT, "sandbox.trace.export",
        Daemon, Control, Internal, false, None, "operation.control.TraceExportInput", "operation.control.TraceExportOutput", "Lease bounded daemon background trace records for host ingest.";
    TraceExportAck, SANDBOX_TRACE_EXPORT_ACK, "sandbox.trace.export_ack",
        Daemon, Control, Internal, true, None, "operation.control.TraceExportAckInput", "operation.control.TraceExportAckOutput", "Ack a durably ingested daemon trace export lease.";
    LayerMetrics, SANDBOX_CHECKPOINT_LAYER_METRICS, "sandbox.checkpoint.layer_metrics",
        Daemon, Checkpoint, Operator, false, None, "operation.checkpoint.LayerMetricsInput", "operation.checkpoint.LayerMetricsOutput", "Report LayerStack and storage metrics for the sandbox.";
    EnsureWorkspaceBase, SANDBOX_CHECKPOINT_ENSURE_BASE, "sandbox.checkpoint.ensure_base",
        Daemon, Checkpoint, Operator, true, None, "operation.checkpoint.EnsureBaseInput", "operation.checkpoint.WorkspaceBaseOutput", "Ensure a workspace base binding exists.";
    BuildWorkspaceBase, SANDBOX_CHECKPOINT_BUILD_BASE, "sandbox.checkpoint.build_base",
        Daemon, Checkpoint, Operator, true, None, "operation.checkpoint.BuildBaseInput", "operation.checkpoint.WorkspaceBaseOutput", "Build or rebuild a workspace base binding.";
    CommitToWorkspace, SANDBOX_CHECKPOINT_COMMIT_TO_WORKSPACE, "sandbox.checkpoint.commit_to_workspace",
        Daemon, Checkpoint, Operator, true, None, "operation.checkpoint.CommitToWorkspaceInput", "operation.checkpoint.CommitToWorkspaceOutput", "Materialize LayerStack state into the bound workspace.";
    CommitToGit, SANDBOX_CHECKPOINT_COMMIT_TO_GIT, "sandbox.checkpoint.commit_to_git",
        Daemon, Checkpoint, Operator, true, None, "operation.checkpoint.CommitInput", "operation.checkpoint.CommitOutput", "Commit a LayerStack snapshot into the bound workspace's durable Git repo.";
    WorkspaceBinding, SANDBOX_CHECKPOINT_BINDING, "sandbox.checkpoint.binding",
        Daemon, Checkpoint, Operator, false, None, "operation.checkpoint.BindingInput", "operation.checkpoint.BindingOutput", "Inspect the workspace binding for a layer stack root.";
    ReadFile, SANDBOX_FILE_READ, "sandbox.file.read",
        Daemon, Files, Public, false, None, "operation.file.ReadFileInput", "operation.file.ReadFileResponse", "Read one file from the layer stack or isolated workspace.";
    WriteFile, SANDBOX_FILE_WRITE, "sandbox.file.write",
        Daemon, Files, Public, true, None, "operation.file.WriteFileInput", "operation.file.WriteFileResponse", "Write one file through the OCC gate.";
    EditFile, SANDBOX_FILE_EDIT, "sandbox.file.edit",
        Daemon, Files, Public, true, None, "operation.file.EditFileInput", "operation.file.EditFileResponse", "Edit one file through the OCC gate.";
    PluginList, SANDBOX_PLUGIN_LIST, "sandbox.plugin.list",
        Daemon, Plugins, Public, false, None, "operation.plugin.PluginListInput", "operation.plugin.PluginListOutput", "List configured first-party plugin providers without probing them.";
    PluginHealth, SANDBOX_PLUGIN_HEALTH, "sandbox.plugin.health",
        Daemon, Plugins, Public, false, None, "operation.plugin.PluginHealthInput", "operation.plugin.PluginHealthOutput", "Actively probe enabled first-party plugin providers.";
    PyrightLspQuerySymbols, SANDBOX_PLUGIN_PYRIGHT_LSP_QUERY_SYMBOLS, "sandbox.plugin.pyright_lsp.query_symbols",
        Daemon, Plugins, Public, false, None, "operation.plugin.PyrightLspQuerySymbolsInput", "operation.plugin.PyrightLspQuerySymbolsOutput", "Return Pyright document symbols for a Python file.";
    PyrightLspDefinition, SANDBOX_PLUGIN_PYRIGHT_LSP_DEFINITION, "sandbox.plugin.pyright_lsp.definition",
        Daemon, Plugins, Public, false, None, "operation.plugin.PyrightLspDefinitionInput", "operation.plugin.PyrightLspLocationsOutput", "Resolve a Pyright definition location.";
    PyrightLspReferences, SANDBOX_PLUGIN_PYRIGHT_LSP_REFERENCES, "sandbox.plugin.pyright_lsp.references",
        Daemon, Plugins, Public, false, None, "operation.plugin.PyrightLspReferencesInput", "operation.plugin.PyrightLspLocationsOutput", "Resolve Pyright reference locations.";
    PyrightLspDiagnostics, SANDBOX_PLUGIN_PYRIGHT_LSP_DIAGNOSTICS, "sandbox.plugin.pyright_lsp.diagnostics",
        Daemon, Plugins, Public, false, None, "operation.plugin.PyrightLspDiagnosticsInput", "operation.plugin.PyrightLspDiagnosticsOutput", "Return current Pyright diagnostics for a Python file.";
    IsolatedWorkspaceEnter, SANDBOX_ISOLATION_ENTER, "sandbox.isolation.enter",
        Daemon, IsolatedWorkspace, Public, true, None, "operation.isolation.IsolationEnterInput", "operation.isolation.IsolationEnterOutput", "Enter isolated workspace mode for a caller.";
    IsolatedWorkspaceExit, SANDBOX_ISOLATION_EXIT, "sandbox.isolation.exit",
        Daemon, IsolatedWorkspace, Public, true, None, "operation.isolation.IsolationExitInput", "operation.isolation.IsolationExitOutput", "Exit isolated workspace mode for a caller.";
    IsolatedWorkspaceStatus, SANDBOX_ISOLATION_STATUS, "sandbox.isolation.status",
        Daemon, IsolatedWorkspace, Public, false, None, "operation.isolation.IsolationStatusInput", "operation.isolation.IsolationStatusOutput", "Inspect isolated workspace status.";
    IsolatedWorkspaceListOpen, SANDBOX_ISOLATION_LIST_OPEN, "sandbox.isolation.list_open",
        Daemon, IsolatedWorkspace, Operator, false, None, "operation.core.NoArgs", "operation.isolation.ListOpenOutput", "List open isolated workspaces.";
    IsolatedWorkspaceTestReset, SANDBOX_ISOLATION_TEST_RESET, "sandbox.isolation.test_reset",
        Daemon, IsolatedWorkspace, Test, true, None, "operation.core.NoArgs", "operation.isolation.TestResetOutput", "Test-only isolated workspace reset hook.";
    ExecCommand, SANDBOX_COMMAND_EXEC, "sandbox.command.exec",
        Daemon, Command, Public, true, None, "operation.command.ExecCommandInput", "operation.command.CommandResponse", "Run a foreground command or start a background command.";
    WriteStdin, SANDBOX_COMMAND_WRITE_STDIN, "sandbox.command.write_stdin",
        Daemon, Command, Public, true, None, "operation.command.WriteStdinInput", "operation.command.CommandResponse", "Write stdin to a command.";
    CommandReadProgress, SANDBOX_COMMAND_POLL, "sandbox.command.poll",
        Daemon, Command, Public, true, None, "operation.command.ReadProgressInput", "operation.command.CommandResponse", "Poll command progress without writing stdin and finalize completed commands.";
    CommandCancel, SANDBOX_COMMAND_CANCEL, "sandbox.command.cancel",
        Daemon, Command, Public, true, None, "operation.command.CancelCommandInput", "operation.command.CommandResponse", "Cancel a command.";
    CommandCollectCompleted, SANDBOX_COMMAND_COLLECT_COMPLETED, "sandbox.command.collect_completed",
        Daemon, Command, Public, true, None, "operation.command.CollectCompletedInput", "operation.command.CollectCompletedOutput", "Collect completed command notifications.";
    CommandCount, SANDBOX_COMMAND_COUNT, "sandbox.command.count",
        Daemon, Command, Public, false, None, "operation.control.CallerCountInput", "operation.command.CommandCountOutput", "Count live commands.";
    CancelWorkspaceRunsByCaller, SANDBOX_RUN_END, "sandbox.run.end",
        Daemon, WorkspaceRun, Public, true, None, "operation.workspace_run.RunEndInput", "operation.workspace_run.RunEndOutput", "End a run: cancel every workspace run owned by one caller (caller_id == agent_run_id), discarding its commands and exiting its isolated workspace.";
    CancelWorkspaceRuns, SANDBOX_RUN_CANCEL_ALL, "sandbox.run.cancel_all",
        Daemon, WorkspaceRun, Operator, true, None, "operation.workspace_run.RunCancelAllInput", "operation.workspace_run.RunCancelAllOutput", "Cancel every workspace run in the sandbox: the whole-sandbox sweep backstop.";
}

#[derive(Serialize)]
struct CatalogOp {
    name: &'static str,
    served_by: &'static str,
    host_verb: Option<&'static str>,
    visibility: &'static str,
    family: &'static str,
    mutates_state: bool,
    args_schema: &'static str,
    response_schema: &'static str,
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
                host_verb: contract.host_verb.map(HostVerb::as_str),
                visibility: contract.visibility.as_str(),
                family: contract.family.as_str(),
                mutates_state: contract.mutates_state,
                args_schema: contract.args_schema,
                response_schema: contract.response_schema,
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
        assert_eq!(BuiltinOp::from_op_name("plugin.lsp.query_symbols"), None);
        assert_eq!(
            BuiltinOp::from_op_name("sandbox.plugin.lsp.query_symbols"),
            None
        );
        assert_eq!(BuiltinOp::from_op_name(""), None);
    }

    #[test]
    fn canonical_names_follow_grammar() {
        for contract in BUILTIN_OPS {
            assert!(
                !contract.name.split('.').any(|token| token == "v1"),
                "the v1 token is dead in canonical names: {}",
                contract.name
            );
            match contract.served_by {
                ServedBy::Host => assert!(
                    contract.name.starts_with("host."),
                    "host op {} must use host.*",
                    contract.name
                ),
                ServedBy::Daemon => assert!(
                    contract.name.starts_with("sandbox."),
                    "daemon op {} must use sandbox.*",
                    contract.name
                ),
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
    fn host_verbs_are_declared_only_for_host_served_ops() {
        for contract in BUILTIN_OPS {
            match contract.served_by {
                ServedBy::Host => assert!(
                    contract.host_verb.is_some(),
                    "host op {} must declare host_verb",
                    contract.name
                ),
                ServedBy::Daemon => assert!(
                    contract.host_verb.is_none(),
                    "daemon op {} must not declare host_verb",
                    contract.name
                ),
            }
        }
    }

    #[test]
    fn schema_descriptors_are_declared_for_every_builtin_op() {
        for contract in BUILTIN_OPS {
            assert!(
                !contract.args_schema.trim().is_empty(),
                "op {} must declare args_schema",
                contract.name
            );
            assert!(
                !contract.response_schema.trim().is_empty(),
                "op {} must declare response_schema",
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
        for op in ops {
            assert!(
                op.get("args_schema")
                    .and_then(serde_json::Value::as_str)
                    .is_some(),
                "ops.json entry must include args_schema: {op}"
            );
            assert!(
                op.get("response_schema")
                    .and_then(serde_json::Value::as_str)
                    .is_some(),
                "ops.json entry must include response_schema: {op}"
            );
        }
        assert!(document.ends_with('\n'));
    }
}
