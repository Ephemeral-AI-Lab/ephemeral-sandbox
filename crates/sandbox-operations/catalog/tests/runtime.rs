#![cfg(feature = "runtime")]

use sandbox_operation_catalog::runtime::runtime_catalog;
use sandbox_operation_contract::{catalog_to_value, ArgKind, OperationDomain};

#[test]
fn runtime_catalog_is_the_exact_public_runtime_surface() {
    let catalog = runtime_catalog();

    assert_eq!(catalog.operation_execution_space, OperationDomain::Runtime);
    assert_eq!(
        catalog
            .families
            .iter()
            .map(|family| family.id)
            .collect::<Vec<_>>(),
        [
            "command",
            "file",
            "daemon_http",
            "layerstack_baseline",
            "layerstack_phase1",
            "layerstack-phase1.portable-root",
            "network_isolation",
            "reserved_paths",
            "shell_security",
            "workspace_session",
        ]
    );
    assert_eq!(
        catalog
            .operations
            .iter()
            .map(|operation| operation.name)
            .collect::<Vec<_>>(),
        [
            "exec_command",
            "write_command_stdin",
            "read_command_lines",
            "file_read",
            "file_write",
            "file_edit",
            "file_blame",
            "create_workspace_session",
            "create_mpla_workspace_session",
            "publish_workspace_session",
            "destroy_workspace_session",
            "mpla_storage_admin",
        ]
    );
    let edits = catalog
        .operations
        .iter()
        .find(|operation| operation.name == "file_edit")
        .and_then(|operation| operation.args.iter().find(|arg| arg.name == "edits"))
        .expect("file_edit edits argument");
    assert_eq!(edits.kind, ArgKind::JsonArray);
    let encoded = catalog_to_value(catalog);
    let encoded_edits = encoded["operations"]
        .as_array()
        .and_then(|operations| {
            operations
                .iter()
                .find(|operation| operation["name"] == "file_edit")
        })
        .and_then(|operation| operation["args"].as_array())
        .and_then(|args| args.iter().find(|arg| arg["name"] == "edits"))
        .expect("encoded edits argument");
    assert_eq!(encoded_edits["kind"], "json_array");

    let network_profile = catalog
        .operations
        .iter()
        .find(|operation| operation.name == "create_workspace_session")
        .and_then(|operation| {
            operation
                .args
                .iter()
                .find(|arg| arg.name == "network_profile")
        })
        .expect("create_workspace_session network_profile argument");
    assert_eq!(network_profile.default, Some("shared"));

    let mpla_create = catalog
        .operations
        .iter()
        .find(|operation| operation.name == "create_mpla_workspace_session")
        .expect("create_mpla_workspace_session operation");
    assert_eq!(
        mpla_create
            .args
            .iter()
            .map(|argument| (argument.name, argument.kind, argument.required))
            .collect::<Vec<_>>(),
        [("run_id", ArgKind::String, true)]
    );

    let publish = catalog
        .operations
        .iter()
        .find(|operation| operation.name == "publish_workspace_session")
        .expect("publish_workspace_session operation");
    assert_eq!(
        publish.summary,
        "Publish an explicit workspace session and close it."
    );
    assert_eq!(
        publish
            .args
            .iter()
            .map(|argument| (argument.name, argument.kind, argument.required))
            .collect::<Vec<_>>(),
        [
            ("workspace_session_id", ArgKind::String, true),
            ("grace_s", ArgKind::Float, false),
        ]
    );

    let storage_admin = catalog
        .operations
        .iter()
        .find(|operation| operation.name == "mpla_storage_admin")
        .expect("mpla_storage_admin operation");
    assert_eq!(storage_admin.family, "workspace_session");
    assert_eq!(
        storage_admin
            .args
            .iter()
            .map(|argument| (argument.name, argument.kind, argument.required))
            .collect::<Vec<_>>(),
        [("request_json", ArgKind::String, true)]
    );
}

#[test]
fn internal_runtime_operations_do_not_leak_into_the_public_catalog() {
    let encoded = catalog_to_value(runtime_catalog()).to_string();

    for internal in [
        "file_list",
        "squash_layerstack",
        "export_layerstack",
        "read_export_chunk",
    ] {
        assert!(
            !encoded.contains(internal),
            "internal operation {internal} leaked into the public catalog"
        );
    }
}
