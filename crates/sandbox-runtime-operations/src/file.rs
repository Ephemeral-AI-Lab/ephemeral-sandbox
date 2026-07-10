use sandbox_protocol::{
    ArgCliSpec, ArgKind, ArgSpec, CliOperationFamilySpec, CliOperationSpec, CliSpec,
};

pub const FILE_FAMILY: CliOperationFamilySpec = CliOperationFamilySpec {
    id: "file",
    title: "File",
    summary: "Read, write, edit, and inspect workspace files.",
    description: "Read, write, and edit files against the layerstack snapshot or a live workspace session, and query per-line ownership over the publish auditability log.",
};

pub const FILE_BLAME_SPEC: CliOperationSpec = CliOperationSpec {
    name: "file_blame",
    family: "file",
    summary: "Show per-line ownership for a published file.",
    description: "Return each line's owner for a published path, tiling the whole file from the latest auditability event. The owner is an opaque string (workspace_session:<id> | operation:<id> | original | unknown).",
    args: FILE_BLAME_ARGS,
    cli: Some(CliSpec {
        path: &["runtime", "file_blame"],
        usage: "sandbox-runtime-cli --sandbox-id ID file_blame --path FILE",
        examples: &["sandbox-runtime-cli --sandbox-id ID file_blame --path README.md"],
    }),
    related: &["file_read", "file_write", "file_edit"],
};

const FILE_BLAME_ARGS: &[ArgSpec] = &[ArgSpec::required(
    "path",
    ArgKind::String,
    "Repository-relative path to blame.",
    Some(ArgCliSpec {
        flag: Some("--path"),
        positional: None,
    }),
)];

pub const FILE_LIST_SPEC: CliOperationSpec = CliOperationSpec {
    name: "file_list",
    family: "file",
    summary: "List one directory level from the snapshot or a session.",
    description: "List the entries of a repository-relative or workspace-root-absolute directory (name, kind, size). With workspace_session_id the listing reads that live session's mounted workspace; without it the listing projects the latest published snapshot. Omit path to list the workspace root.",
    args: FILE_LIST_ARGS,
    cli: None,
    related: &["file_read", "file_write", "file_blame"],
};

const FILE_LIST_ARGS: &[ArgSpec] = &[
    ArgSpec::optional(
        "path",
        ArgKind::String,
        "Repository-relative or workspace-root-absolute directory to list. Omit for the workspace root.",
        None,
        Some(ArgCliSpec {
            flag: Some("--path"),
            positional: None,
        }),
    ),
    ArgSpec::optional(
        "workspace_session_id",
        ArgKind::String,
        "Existing workspace session id to list inside. Omit to list the snapshot.",
        None,
        Some(ArgCliSpec {
            flag: Some("--workspace-session-id"),
            positional: None,
        }),
    ),
];

pub const FILE_READ_SPEC: CliOperationSpec = CliOperationSpec {
    name: "file_read",
    family: "file",
    summary: "Read a text file from the snapshot or a session.",
    description: "Read a UTF-8 text window from a repository-relative or workspace-root-absolute path. With workspace_session_id the read runs inside that live session's mounted workspace; without it the read projects the latest published snapshot.",
    args: FILE_READ_ARGS,
    cli: Some(CliSpec {
        path: &["runtime", "file_read"],
        usage: "sandbox-runtime-cli --sandbox-id ID file_read --path FILE [--offset N] [--limit N] [--workspace-session-id ID]",
        examples: &[
            "sandbox-runtime-cli --sandbox-id ID file_read --path README.md",
            "sandbox-runtime-cli --sandbox-id ID file_read --path src/main.rs --offset 20 --limit 40",
            "sandbox-runtime-cli --sandbox-id ID file_read --path src/main.rs --workspace-session-id ws-1",
        ],
    }),
    related: &["file_write", "file_edit", "file_blame"],
};

const FILE_READ_ARGS: &[ArgSpec] = &[
    ArgSpec::required(
        "path",
        ArgKind::String,
        "Repository-relative or workspace-root-absolute path to read.",
        Some(ArgCliSpec {
            flag: Some("--path"),
            positional: None,
        }),
    ),
    ArgSpec::optional(
        "offset",
        ArgKind::Integer,
        "1-indexed line number to start reading from. Defaults to 1.",
        Some("1"),
        Some(ArgCliSpec {
            flag: Some("--offset"),
            positional: None,
        }),
    ),
    ArgSpec::optional(
        "limit",
        ArgKind::Integer,
        "Maximum number of lines to read. Defaults to 2000; must be 1..=2000.",
        Some("2000"),
        Some(ArgCliSpec {
            flag: Some("--limit"),
            positional: None,
        }),
    ),
    ArgSpec::optional(
        "workspace_session_id",
        ArgKind::String,
        "Existing workspace session id to read inside. Omit to read the snapshot.",
        None,
        Some(ArgCliSpec {
            flag: Some("--workspace-session-id"),
            positional: None,
        }),
    ),
];

pub const FILE_WRITE_SPEC: CliOperationSpec = CliOperationSpec {
    name: "file_write",
    family: "file",
    summary: "Overwrite a file in the snapshot or a session.",
    description: "Write content to a repository-relative or workspace-root-absolute path. With workspace_session_id the write lands in that live session's mounted workspace and is attributed on capture; without it the write publishes one layer attributed to operation:<request_id>.",
    args: FILE_WRITE_ARGS,
    cli: Some(CliSpec {
        path: &["runtime", "file_write"],
        usage: "sandbox-runtime-cli --sandbox-id ID file_write --path FILE --content TEXT [--workspace-session-id ID]",
        examples: &[
            "sandbox-runtime-cli --sandbox-id ID file_write --path notes.txt --content 'hello'",
            "sandbox-runtime-cli --sandbox-id ID file_write --path notes.txt --content 'hello' --workspace-session-id ws-1",
        ],
    }),
    related: &["file_read", "file_edit", "file_blame"],
};

const FILE_WRITE_ARGS: &[ArgSpec] = &[
    ArgSpec::required(
        "path",
        ArgKind::String,
        "Repository-relative or workspace-root-absolute path to write.",
        Some(ArgCliSpec {
            flag: Some("--path"),
            positional: None,
        }),
    ),
    ArgSpec::required(
        "content",
        ArgKind::String,
        "File content to write.",
        Some(ArgCliSpec {
            flag: Some("--content"),
            positional: None,
        }),
    ),
    ArgSpec::optional(
        "workspace_session_id",
        ArgKind::String,
        "Existing workspace session id to write inside. Omit to publish a layer.",
        None,
        Some(ArgCliSpec {
            flag: Some("--workspace-session-id"),
            positional: None,
        }),
    ),
];

pub const FILE_EDIT_SPEC: CliOperationSpec = CliOperationSpec {
    name: "file_edit",
    family: "file",
    summary: "Apply ordered string edits to a file.",
    description: "Apply an ordered list of exact-string replacements to a repository-relative or workspace-root-absolute path. Each old_string must be found and unique unless replace_all is set. With workspace_session_id the edit runs inside that live session; without it the edit publishes one layer attributed to operation:<request_id>.",
    args: FILE_EDIT_ARGS,
    cli: Some(CliSpec {
        path: &["runtime", "file_edit"],
        usage: "sandbox-runtime-cli --sandbox-id ID file_edit --path FILE --edits JSON [--workspace-session-id ID]",
        examples: &[
            "sandbox-runtime-cli --sandbox-id ID file_edit --path notes.txt --edits '[{\"old_string\":\"a\",\"new_string\":\"b\"}]'",
            "sandbox-runtime-cli --sandbox-id ID file_edit --path notes.txt --edits '[{\"old_string\":\"a\",\"new_string\":\"b\",\"replace_all\":true}]' --workspace-session-id ws-1",
        ],
    }),
    related: &["file_read", "file_write", "file_blame"],
};

const FILE_EDIT_ARGS: &[ArgSpec] = &[
    ArgSpec::required(
        "path",
        ArgKind::String,
        "Repository-relative or workspace-root-absolute path to edit.",
        Some(ArgCliSpec {
            flag: Some("--path"),
            positional: None,
        }),
    ),
    ArgSpec::required(
        "edits",
        ArgKind::String,
        "JSON array of { old_string, new_string, replace_all? } edits, applied in order.",
        Some(ArgCliSpec {
            flag: Some("--edits"),
            positional: None,
        }),
    ),
    ArgSpec::optional(
        "workspace_session_id",
        ArgKind::String,
        "Existing workspace session id to edit inside. Omit to publish a layer.",
        None,
        Some(ArgCliSpec {
            flag: Some("--workspace-session-id"),
            positional: None,
        }),
    ),
];
