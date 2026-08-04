use sandbox_operation_contract::OperationDomain;

use super::{ArgumentProjection, CatalogProjection, OperationProjection};

const EXEC_COMMAND_ARGUMENTS: &[ArgumentProjection] = &[
    ArgumentProjection::flag("workspace_session_id", "--workspace-session-id"),
    ArgumentProjection::positional("cmd", "COMMAND"),
    ArgumentProjection::flag("timeout_ms", "--timeout-ms"),
    ArgumentProjection::flag("yield_time_ms", "--yield-time-ms"),
];

const WRITE_STDIN_ARGUMENTS: &[ArgumentProjection] = &[
    ArgumentProjection::flag("command_session_id", "--command-session-id"),
    ArgumentProjection::positional("stdin", "TEXT"),
    ArgumentProjection::flag("yield_time_ms", "--yield-time-ms"),
];

const READ_LINES_ARGUMENTS: &[ArgumentProjection] = &[
    ArgumentProjection::flag("command_session_id", "--command-session-id"),
    ArgumentProjection::flag("start_offset", "--start-offset"),
    ArgumentProjection::flag("limit", "--limit"),
];

const FILE_READ_ARGUMENTS: &[ArgumentProjection] = &[
    ArgumentProjection::flag("path", "--path"),
    ArgumentProjection::flag("offset", "--offset"),
    ArgumentProjection::flag("limit", "--limit"),
    ArgumentProjection::flag("workspace_session_id", "--workspace-session-id"),
];

const FILE_WRITE_ARGUMENTS: &[ArgumentProjection] = &[
    ArgumentProjection::flag("path", "--path"),
    ArgumentProjection::flag_with_value_file("content", "--content", "--content-file"),
    ArgumentProjection::flag("workspace_session_id", "--workspace-session-id"),
];

const FILE_EDIT_ARGUMENTS: &[ArgumentProjection] = &[
    ArgumentProjection::flag("path", "--path"),
    ArgumentProjection::flag("edits", "--edits"),
    ArgumentProjection::flag("workspace_session_id", "--workspace-session-id"),
];

const FILE_BLAME_ARGUMENTS: &[ArgumentProjection] = &[ArgumentProjection::flag("path", "--path")];

const CREATE_WORKSPACE_SESSION_ARGUMENTS: &[ArgumentProjection] = &[ArgumentProjection::flag(
    "network_profile",
    "--network-profile",
)];

const PUBLISH_WORKSPACE_SESSION_ARGUMENTS: &[ArgumentProjection] = &[
    ArgumentProjection::flag("workspace_session_id", "--workspace-session-id"),
    ArgumentProjection::flag("grace_s", "--grace-s"),
];

const DESTROY_WORKSPACE_SESSION_ARGUMENTS: &[ArgumentProjection] = &[
    ArgumentProjection::flag("workspace_session_id", "--workspace-session-id"),
    ArgumentProjection::flag("grace_s", "--grace-s"),
];

const OPERATIONS: &[OperationProjection] = &[
    OperationProjection {
        name: "exec_command",
        path: &["runtime", "exec_command"],
        usage: "sandbox-runtime-cli --sandbox-id ID exec_command [--workspace-session-id ID] [--timeout-ms N] [--yield-time-ms N] COMMAND",
        examples: &[
            "sandbox-runtime-cli --sandbox-id ID exec_command pwd",
            "sandbox-runtime-cli --sandbox-id ID exec_command --workspace-session-id ws-1 pwd",
            "sandbox-runtime-cli --sandbox-id ID exec_command --workspace-session-id ws-1 --yield-time-ms 0 \"sleep 30\"",
        ],
        arguments: EXEC_COMMAND_ARGUMENTS,
    },
    OperationProjection {
        name: "write_command_stdin",
        path: &["runtime", "write_command_stdin"],
        usage: "sandbox-runtime-cli --sandbox-id ID write_command_stdin --command-session-id ID [--yield-time-ms N] TEXT",
        examples: &[
            "sandbox-runtime-cli --sandbox-id ID write_command_stdin --command-session-id cmd-1 hello",
        ],
        arguments: WRITE_STDIN_ARGUMENTS,
    },
    OperationProjection {
        name: "read_command_lines",
        path: &["runtime", "read_command_lines"],
        usage: "sandbox-runtime-cli --sandbox-id ID read_command_lines --command-session-id ID [--start-offset N] [--limit N]",
        examples: &[
            "sandbox-runtime-cli --sandbox-id ID read_command_lines --command-session-id cmd-1 --start-offset 0 --limit 100",
        ],
        arguments: READ_LINES_ARGUMENTS,
    },
    OperationProjection {
        name: "file_read",
        path: &["runtime", "file_read"],
        usage: "sandbox-runtime-cli --sandbox-id ID file_read --path FILE [--offset N] [--limit N] [--workspace-session-id ID]",
        examples: &[
            "sandbox-runtime-cli --sandbox-id ID file_read --path README.md",
            "sandbox-runtime-cli --sandbox-id ID file_read --path src/main.rs --offset 20 --limit 40",
            "sandbox-runtime-cli --sandbox-id ID file_read --path src/main.rs --workspace-session-id ws-1",
        ],
        arguments: FILE_READ_ARGUMENTS,
    },
    OperationProjection {
        name: "file_write",
        path: &["runtime", "file_write"],
        usage: "sandbox-runtime-cli --sandbox-id ID file_write --path FILE --content TEXT [--workspace-session-id ID]",
        examples: &[
            "sandbox-runtime-cli --sandbox-id ID file_write --path notes.txt --content 'hello'",
            "sandbox-runtime-cli --sandbox-id ID file_write --path notes.txt --content 'hello' --workspace-session-id ws-1",
        ],
        arguments: FILE_WRITE_ARGUMENTS,
    },
    OperationProjection {
        name: "file_edit",
        path: &["runtime", "file_edit"],
        usage: "sandbox-runtime-cli --sandbox-id ID file_edit --path FILE --edits JSON [--workspace-session-id ID]",
        examples: &[
            "sandbox-runtime-cli --sandbox-id ID file_edit --path notes.txt --edits '[{\"old_string\":\"a\",\"new_string\":\"b\"}]'",
            "sandbox-runtime-cli --sandbox-id ID file_edit --path notes.txt --edits '[{\"old_string\":\"a\",\"new_string\":\"b\",\"replace_all\":true}]' --workspace-session-id ws-1",
        ],
        arguments: FILE_EDIT_ARGUMENTS,
    },
    OperationProjection {
        name: "file_blame",
        path: &["runtime", "file_blame"],
        usage: "sandbox-runtime-cli --sandbox-id ID file_blame --path FILE",
        examples: &["sandbox-runtime-cli --sandbox-id ID file_blame --path README.md"],
        arguments: FILE_BLAME_ARGUMENTS,
    },
    OperationProjection {
        name: "create_workspace_session",
        path: &["runtime", "create_workspace_session"],
        usage: "sandbox-runtime-cli --sandbox-id ID create_workspace_session [--network-profile PROFILE]",
        examples: &[
            "sandbox-runtime-cli --sandbox-id ID create_workspace_session",
            "sandbox-runtime-cli --sandbox-id ID create_workspace_session --network-profile isolated",
        ],
        arguments: CREATE_WORKSPACE_SESSION_ARGUMENTS,
    },
    OperationProjection {
        name: "publish_workspace_session",
        path: &["runtime", "publish_workspace_session"],
        usage: "sandbox-runtime-cli --sandbox-id ID publish_workspace_session --workspace-session-id ID [--grace-s SECONDS]",
        examples: &[
            "sandbox-runtime-cli --sandbox-id ID publish_workspace_session --workspace-session-id ws-1",
            "sandbox-runtime-cli --sandbox-id ID publish_workspace_session --workspace-session-id ws-1 --grace-s 1",
        ],
        arguments: PUBLISH_WORKSPACE_SESSION_ARGUMENTS,
    },
    OperationProjection {
        name: "destroy_workspace_session",
        path: &["runtime", "destroy_workspace_session"],
        usage: "sandbox-runtime-cli --sandbox-id ID destroy_workspace_session --workspace-session-id ID [--grace-s SECONDS]",
        examples: &[
            "sandbox-runtime-cli --sandbox-id ID destroy_workspace_session --workspace-session-id ws-1",
        ],
        arguments: DESTROY_WORKSPACE_SESSION_ARGUMENTS,
    },
];

#[must_use]
pub const fn catalog_projection() -> CatalogProjection {
    CatalogProjection {
        operation_execution_space: OperationDomain::Runtime,
        operations: OPERATIONS,
    }
}
