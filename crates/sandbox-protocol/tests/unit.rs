use sandbox_protocol::{
    catalog_from_value, catalog_to_value, decode_request_value, render_catalog_help,
    render_operation_help, ArgCliSpec, ArgKind, ArgSpec, CliOperationCatalog,
    CliOperationExecutionSpace, CliOperationFamilySpec, CliOperationScope, CliOperationSpec,
    CliSpec, Response, DAEMON_AUTH_FIELD,
};
use serde_json::{json, Value};

static TEST_ARGS: &[ArgSpec] = &[
    ArgSpec::required(
        "image",
        ArgKind::String,
        "Container image used to create the sandbox.",
        Some(ArgCliSpec {
            flag: Some("--image"),
            positional: None,
        }),
    ),
    ArgSpec::required(
        "workspace_root",
        ArgKind::Path,
        "Absolute workspace root mounted inside this sandbox.",
        Some(ArgCliSpec {
            flag: Some("--workspace-root"),
            positional: None,
        }),
    ),
];

static TEST_FAMILY: CliOperationFamilySpec = CliOperationFamilySpec {
    id: "management",
    title: "Management",
    summary: "Create, destroy, list, and inspect sandbox records.",
    description: "Create, destroy, list, and inspect sandbox records.",
};

static TEST_SPEC: CliOperationSpec = CliOperationSpec {
    name: "create_sandbox",
    family: "management",
    summary: "Create a sandbox.",
    description: "Create a sandbox and start its daemon.",
    args: TEST_ARGS,
    cli: Some(CliSpec {
        path: &["manager"],
        usage: "sandbox-cli manager create_sandbox --image IMAGE --workspace-root PATH",
        examples: &[
            "sandbox-cli manager create_sandbox --image ubuntu:24.04 --workspace-root /testbed",
        ],
    }),
    related: &[],
};

static TEST_FAMILIES: &[&CliOperationFamilySpec] = &[&TEST_FAMILY];
static TEST_SPECS: &[&CliOperationSpec] = &[&TEST_SPEC];

#[test]
fn daemon_auth_field_uses_sandbox_name() {
    assert_eq!(DAEMON_AUTH_FIELD, "_sandbox_daemon_auth_token");
}

#[test]
fn responses_preserve_payload_owned_shape() {
    let ok = Response::ok(json!({
        "status": "ok",
        "output": "command output remains payload-owned",
    }))
    .into_json_value();
    let err = Response::fault("operation_failed", "failed").into_json_value();

    assert_eq!(ok["status"], "ok");
    assert!(ok.get("result").is_none(), "{ok}");
    assert!(ok.get("meta").is_none(), "{ok}");
    assert_eq!(err["error"]["kind"], "operation_failed");
    assert!(err.get("result").is_none(), "{err}");
    assert!(err.get("meta").is_none(), "{err}");
}

#[test]
fn decode_request_requires_object_args_when_present() {
    let value = json!({
        "op": "exec_command",
        "request_id": "req-1",
        "scope": { "kind": "sandbox", "sandbox_id": "sbox-1" },
        "args": "bad",
    });
    let err = decode_request_value(value).expect_err("non-object args rejected");
    assert_eq!(err.kind(), "invalid_request");
    assert_eq!(err.message(), "args must be an object");
}

#[test]
fn decode_request_rejects_missing_scope() {
    let value = json!({
        "op": "list_sandboxes",
        "request_id": "req-1",
        "args": {},
    });
    let err = decode_request_value(value).expect_err("missing scope rejected");

    assert_eq!(err.kind(), "invalid_request");
    assert_eq!(err.message(), "scope is required");
}

#[test]
fn decode_request_accepts_sandbox_scope() {
    let value = json!({
        "op": "exec_command",
        "request_id": "req-1",
        "scope": {
            "kind": "sandbox",
            "sandbox_id": "sbox-1"
        },
        "args": {},
    });
    let request = decode_request_value(value).expect("request should decode");

    assert_eq!(
        request.scope,
        CliOperationScope::Sandbox {
            sandbox_id: "sbox-1".to_owned()
        }
    );
}

#[test]
fn decode_request_rejects_empty_sandbox_scope_id() {
    let value = json!({
        "op": "exec_command",
        "request_id": "req-1",
        "scope": {
            "kind": "sandbox",
            "sandbox_id": ""
        },
        "args": {},
    });
    let err = decode_request_value(value).expect_err("empty sandbox id rejected");

    assert_eq!(err.kind(), "invalid_request");
    assert_eq!(err.message(), "scope sandbox_id must be non-empty");
}

#[test]
fn catalog_to_value_serializes_cli_metadata() {
    let value = catalog_to_value(CliOperationCatalog::new(
        CliOperationExecutionSpace::Manager,
        TEST_FAMILIES,
        TEST_SPECS,
    ));

    assert_eq!(value["operation_execution_space"], "manager");
    assert_eq!(value["families"][0]["id"], "management");
    assert_eq!(value["families"][0]["title"], "Management");
    assert_eq!(value["operations"][0]["name"], "create_sandbox");
    assert_eq!(value["operations"][0]["family"], "management");
    assert_eq!(value["operations"][0]["summary"], "Create a sandbox.");
    assert_eq!(
        value["operations"][0]["description"],
        "Create a sandbox and start its daemon."
    );
    assert!(value["operations"][0]["related"].is_array());
    assert!(value["operations"][0]["args"].is_array());
    assert!(value["operations"][0]["cli"].is_object());
    assert_eq!(value["operations"][0]["args"][0]["name"], "image");
    assert_eq!(value["operations"][0]["args"][0]["kind"], "string");
    assert_eq!(value["operations"][0]["args"][0]["required"], true);
    assert_eq!(value["operations"][0]["args"][0]["default"], Value::Null);
    assert_eq!(value["operations"][0]["args"][0]["cli"]["flag"], "--image");
    assert_eq!(
        value["operations"][0]["cli"]["examples"][0],
        "sandbox-cli manager create_sandbox --image ubuntu:24.04 --workspace-root /testbed"
    );
}

#[test]
fn catalog_from_value_decodes_cli_metadata() {
    let value = json!({
        "operation_execution_space": "runtime",
        "families": [
            {
                "id": "command",
                "title": "Command",
                "summary": "Run commands.",
                "description": "Run commands in the runtime."
            }
        ],
        "operations": [
            {
                "name": "exec_command",
                "family": "command",
                "summary": "Start a command.",
                "description": "Start a shell command.",
                "args": [
                    {
                        "name": "cmd",
                        "kind": "string",
                        "required": true,
                        "help": "Shell command text.",
                        "default": null,
                        "cli": {
                            "flag": null,
                            "positional": "COMMAND"
                        }
                    }
                ],
                "cli": {
                    "path": ["runtime"],
                    "usage": "sandbox-cli runtime exec_command COMMAND",
                    "examples": ["sandbox-cli runtime exec_command pwd"]
                },
                "related": []
            }
        ]
    });

    let catalog = catalog_from_value(&value).expect("catalog decodes");

    assert_eq!(
        catalog.operation_execution_space,
        CliOperationExecutionSpace::Runtime
    );
    assert_eq!(
        catalog.operations[0].args[0]
            .cli
            .as_ref()
            .and_then(|cli| cli.positional.as_deref()),
        Some("COMMAND")
    );
}

#[test]
fn catalog_from_value_rejects_unknown_execution_space() {
    let value = json!({
        "operation_execution_space": "daemon",
        "families": [],
        "operations": []
    });

    let error = catalog_from_value(&value).expect_err("unknown space rejected");

    assert_eq!(error.message(), "unknown operation_execution_space: daemon");
}

#[test]
fn catalog_from_value_rejects_missing_execution_space() {
    let value = json!({
        "families": [],
        "operations": []
    });

    let error = catalog_from_value(&value).expect_err("missing space rejected");

    assert_eq!(
        error.message(),
        "operation_execution_space must be a string"
    );
}

#[test]
fn catalog_from_value_rejects_duplicate_family_ids() {
    let value = json!({
        "operation_execution_space": "runtime",
        "families": [
            family_value("command", "Command"),
            family_value("command", "Command Again")
        ],
        "operations": []
    });

    let error = catalog_from_value(&value).expect_err("duplicate family rejected");

    assert_eq!(
        error.message(),
        "duplicate cli operation family id: command"
    );
}

#[test]
fn catalog_from_value_rejects_missing_cli_operation_family() {
    let value = json!({
        "operation_execution_space": "runtime",
        "families": [family_value("command", "Command")],
        "operations": [
            operation_value("exec_command", "missing", "Start a command.", [])
        ]
    });

    let error = catalog_from_value(&value).expect_err("missing family rejected");

    assert_eq!(
        error.message(),
        "operation exec_command references unknown family: missing"
    );
}

#[test]
fn catalog_from_value_rejects_duplicate_operation_names() {
    let value = json!({
        "operation_execution_space": "runtime",
        "families": [family_value("command", "Command")],
        "operations": [
            operation_value("exec_command", "command", "Start a command.", []),
            operation_value("exec_command", "command", "Start a command again.", [])
        ]
    });

    let error = catalog_from_value(&value).expect_err("duplicate operation rejected");

    assert_eq!(error.message(), "duplicate operation name: exec_command");
}

#[test]
fn catalog_from_value_rejects_missing_related_operation() {
    let value = json!({
        "operation_execution_space": "runtime",
        "families": [family_value("command", "Command")],
        "operations": [
            operation_value("exec_command", "command", "Start a command.", ["missing_command_status"])
        ]
    });

    let error = catalog_from_value(&value).expect_err("missing related operation rejected");

    assert_eq!(
        error.message(),
        "operation exec_command references unknown related operation: missing_command_status"
    );
}

#[test]
fn render_catalog_help_groups_operations_by_family() {
    let catalog = catalog_from_value(&json!({
        "operation_execution_space": "runtime",
        "families": [
            family_value("command", "Command"),
            family_value("file", "File")
        ],
        "operations": [
            operation_value("exec_command", "command", "Start a command.", ["read_command_lines"]),
            operation_value("read_command_lines", "command", "Read command output.", []),
            operation_value("read_file", "file", "Read a file.", [])
        ]
    }))
    .expect("catalog decodes");

    let help = render_catalog_help(&catalog, "sandbox-cli runtime");

    assert!(help.contains("Sandbox Runtime Help"));
    assert!(
        help.find("Command").expect("command family") < help.find("File").expect("file family")
    );
    assert!(
        help.find("exec_command").expect("exec operation")
            < help
                .find("read_command_lines")
                .expect("read command lines operation")
    );
    assert!(help.contains("sandbox-cli runtime OPERATION"));
}

#[test]
fn render_operation_help_renders_detail_page() {
    let catalog = catalog_from_value(&json!({
        "operation_execution_space": "runtime",
        "families": [family_value("command", "Command")],
        "operations": [
            operation_value("exec_command", "command", "Start a command.", [])
        ]
    }))
    .expect("catalog decodes");

    let help = render_operation_help(&catalog, "exec_command", "sandbox-cli runtime")
        .expect("operation renders");

    assert!(help.contains("exec_command"));
    assert!(help.contains("Family\n  Command"));
    assert!(help.contains("Description\n  Start a command. description"));
    assert!(help.contains("Usage\n  sandbox-cli runtime exec_command COMMAND"));
    assert!(help.contains("Arguments\n  COMMAND string required"));
    assert!(help.contains("Examples\n  sandbox-cli runtime exec_command pwd"));
}

#[test]
fn render_operation_help_unknown_returns_suggestions() {
    let catalog = catalog_from_value(&json!({
        "operation_execution_space": "runtime",
        "families": [family_value("command", "Command")],
        "operations": [
            operation_value("exec_command", "command", "Start a command.", [])
        ]
    }))
    .expect("catalog decodes");

    let error = render_operation_help(&catalog, "exec", "sandbox-cli runtime")
        .expect_err("unknown operation rejected");

    assert_eq!(error.operation(), "exec");
    assert_eq!(error.suggestions()[0].name, "exec_command");
    assert!(error
        .to_string()
        .contains("unknown runtime operation for help: exec"));
}

#[test]
fn catalog_to_value_omits_owner_target_fields() {
    let value = catalog_to_value(CliOperationCatalog::new(
        CliOperationExecutionSpace::Manager,
        TEST_FAMILIES,
        TEST_SPECS,
    ));

    assert_no_forbidden_catalog_keys(&value);
}

fn assert_no_forbidden_catalog_keys(value: &serde_json::Value) {
    match value {
        serde_json::Value::Object(object) => {
            for key in [
                "owner",
                "target",
                "route",
                "implementation_owner",
                "operation_target",
            ] {
                assert!(!object.contains_key(key), "catalog emitted forbidden {key}");
            }
            for child in object.values() {
                assert_no_forbidden_catalog_keys(child);
            }
        }
        serde_json::Value::Array(values) => {
            for child in values {
                assert_no_forbidden_catalog_keys(child);
            }
        }
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_) => {}
    }
}

fn family_value(id: &str, title: &str) -> Value {
    json!({
        "id": id,
        "title": title,
        "summary": format!("{title} summary"),
        "description": format!("{title} description"),
    })
}

fn operation_value<const N: usize>(
    name: &str,
    family: &str,
    summary: &str,
    related: [&str; N],
) -> Value {
    json!({
        "name": name,
        "family": family,
        "summary": summary,
        "description": format!("{summary} description"),
        "args": [
            {
                "name": "cmd",
                "kind": "string",
                "required": true,
                "help": "Shell command text.",
                "default": null,
                "cli": {
                    "flag": null,
                    "positional": "COMMAND"
                }
            }
        ],
        "cli": {
            "path": ["runtime", name],
            "usage": format!("sandbox-cli runtime {name} COMMAND"),
            "examples": [format!("sandbox-cli runtime {name} pwd")]
        },
        "related": related.to_vec()
    })
}
