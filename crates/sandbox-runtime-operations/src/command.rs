use sandbox_protocol::{
    ArgCliSpec, ArgKind, ArgSpec, CliOperationFamilySpec, CliOperationSpec, CliSpec,
};

pub const COMMAND_FAMILY: CliOperationFamilySpec = CliOperationFamilySpec {
    id: "command",
    title: "Command",
    summary: "Run, interact with, and inspect commands.",
    description: "Run, interact with, and inspect commands inside the active sandbox runtime.",
};

pub const EXEC_COMMAND_SPEC: CliOperationSpec = CliOperationSpec {
    name: "exec_command",
    family: "command",
    summary: "Start a command in a workspace session.",
    description: "Start a shell command in a workspace session. With workspace_session_id, run inside that existing session. Without it, exec_command creates a session with finalize policy publish_then_destroy. A session finalizes per its policy when its last running command reaches terminal state: publish_then_destroy captures and publishes the session's changes to the layerstack, then destroys the session; no_op keeps the session alive until destroy_workspace_session. Explicit destroy_workspace_session always discards unpublished changes. File operations and remounts run under the session's admission gate and neither extend nor trigger the session lifecycle. If the command is still running after the initial wait, the response includes a command_session_id usable with read_command_lines or write_command_stdin; a still-running command stays terminable through write_command_stdin (Ctrl-C or Ctrl-D).",
    args: EXEC_COMMAND_ARGS,
    cli: Some(EXEC_COMMAND_CLI),
    related: &["write_command_stdin", "read_command_lines"],
};

const EXEC_COMMAND_ARGS: &[ArgSpec] = &[
    ArgSpec::optional(
        "workspace_session_id",
        ArgKind::String,
        "Existing workspace session id to run inside. Omit to create a session with finalize policy publish_then_destroy.",
        None,
        Some(ArgCliSpec {
            flag: Some("--workspace-session-id"),
            positional: None,
        }),
    ),
    ArgSpec::required(
        "cmd",
        ArgKind::String,
        "Shell command text.",
        Some(ArgCliSpec {
            flag: None,
            positional: Some("COMMAND"),
        }),
    ),
    ArgSpec::optional(
        "timeout_ms",
        ArgKind::Integer,
        "Command timeout in milliseconds.",
        None,
        Some(ArgCliSpec {
            flag: Some("--timeout-ms"),
            positional: None,
        }),
    ),
    ArgSpec::optional(
        "yield_time_ms",
        ArgKind::Integer,
        "Initial output wait in milliseconds.",
        None,
        Some(ArgCliSpec {
            flag: Some("--yield-time-ms"),
            positional: None,
        }),
    ),
];

const EXEC_COMMAND_CLI: CliSpec = CliSpec {
    path: &["runtime", "exec_command"],
    usage: "sandbox-runtime-cli --sandbox-id ID exec_command [--workspace-session-id ID] COMMAND",
    examples: &[
        "sandbox-runtime-cli --sandbox-id ID exec_command pwd",
        "sandbox-runtime-cli --sandbox-id ID exec_command --workspace-session-id ws-1 pwd",
        "sandbox-runtime-cli --sandbox-id ID exec_command --workspace-session-id ws-1 --yield-time-ms 0 \"sleep 30\"",
    ],
};

pub const WRITE_STDIN_SPEC: CliOperationSpec = CliOperationSpec {
    name: "write_command_stdin",
    family: "command",
    summary: "Write text to a running command stdin.",
    description: "Append text to the stdin stream of a running command session and return a bounded output yield.",
    args: WRITE_STDIN_ARGS,
    cli: Some(WRITE_STDIN_CLI),
    related: &["exec_command", "read_command_lines"],
};

const WRITE_STDIN_ARGS: &[ArgSpec] = &[
    ArgSpec::required(
        "command_session_id",
        ArgKind::String,
        "Command session id returned by exec_command.",
        Some(ArgCliSpec {
            flag: Some("--command-session-id"),
            positional: None,
        }),
    ),
    ArgSpec::required(
        "stdin",
        ArgKind::String,
        "Text to write to stdin.",
        Some(ArgCliSpec {
            flag: None,
            positional: Some("TEXT"),
        }),
    ),
    ArgSpec::optional(
        "yield_time_ms",
        ArgKind::Integer,
        "Output wait after writing stdin.",
        None,
        Some(ArgCliSpec {
            flag: Some("--yield-time-ms"),
            positional: None,
        }),
    ),
];

const WRITE_STDIN_CLI: CliSpec = CliSpec {
    path: &["runtime", "write_command_stdin"],
    usage: "sandbox-runtime-cli --sandbox-id ID write_command_stdin --command-session-id ID TEXT",
    examples: &[
        "sandbox-runtime-cli --sandbox-id ID write_command_stdin --command-session-id cmd-1 hello",
    ],
};

pub const READ_LINES_SPEC: CliOperationSpec = CliOperationSpec {
    name: "read_command_lines",
    family: "command",
    summary: "Read command output by line offset.",
    description: "Read rendered command output for a command session using stable line offsets.",
    args: READ_LINES_ARGS,
    cli: Some(READ_LINES_CLI),
    related: &["exec_command", "write_command_stdin"],
};

const READ_LINES_ARGS: &[ArgSpec] = &[
    ArgSpec::required(
        "command_session_id",
        ArgKind::String,
        "Command session id returned by exec_command.",
        Some(ArgCliSpec {
            flag: Some("--command-session-id"),
            positional: None,
        }),
    ),
    ArgSpec::optional(
        "start_offset",
        ArgKind::Integer,
        "First transcript line offset. Defaults to 0.",
        None,
        Some(ArgCliSpec {
            flag: Some("--start-offset"),
            positional: None,
        }),
    ),
    ArgSpec::optional(
        "limit",
        ArgKind::Integer,
        "Maximum transcript rows to return. Defaults to 200; maximum 1000.",
        None,
        Some(ArgCliSpec {
            flag: Some("--limit"),
            positional: None,
        }),
    ),
];

const READ_LINES_CLI: CliSpec = CliSpec {
    path: &["runtime", "read_command_lines"],
    usage: "sandbox-runtime-cli --sandbox-id ID read_command_lines --command-session-id ID [--start-offset N] [--limit N]",
    examples: &[
        "sandbox-runtime-cli --sandbox-id ID read_command_lines --command-session-id cmd-1 --start-offset 0 --limit 100",
    ],
};
