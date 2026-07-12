#[test]
fn operation_modules_do_not_depend_on_transport_or_reporting_layers() {
    let modules = [
        ("command", include_str!("../../src/executors/command.rs")),
        ("files", include_str!("../../src/executors/files.rs")),
        (
            "workspace",
            include_str!("../../src/executors/workspace.rs"),
        ),
        (
            "layerstack",
            include_str!("../../src/executors/layerstack.rs"),
        ),
    ];
    let forbidden = [
        "crate::api",
        "crate::app",
        "crate::artifacts",
        "crate::statistics",
        "crate::report",
        "hyper::",
        "http::",
    ];

    for (name, source) in modules {
        for dependency in forbidden {
            assert!(
                !source.contains(dependency),
                "{name} operation module imports forbidden layer {dependency}"
            );
        }
    }
}

#[test]
fn statistics_reports_and_compare_do_not_depend_on_execution_or_transport_modules() {
    let modules = [
        ("statistics", include_str!("../../src/statistics.rs")),
        ("report", include_str!("../../src/report.rs")),
        ("compare", include_str!("../../src/compare.rs")),
    ];
    let forbidden = [
        "crate::executors",
        "executors::command",
        "executors::files",
        "executors::workspace",
        "executors::layerstack",
        "crate::gateway",
        "crate::daemon_session",
        "hyper::",
        "sandbox_provider",
        "OperationPlan::",
        "ExpandedOperationCell::",
    ];

    for (name, source) in modules {
        for dependency in forbidden {
            assert!(
                !source.contains(dependency),
                "{name} module imports forbidden execution or transport layer {dependency}"
            );
        }
    }
}

#[test]
fn closed_dispatch_has_no_type_erased_plugin_or_service_locator_surface() {
    let dispatch = include_str!("../../src/executors/mod.rs");
    let definitions = include_str!("../../src/definitions.rs");
    for forbidden in [
        "std::any::Any",
        "Box<dyn OperationLifecycle",
        "HashMap<OperationId",
        "ServiceLocator",
        "load_plugin",
    ] {
        assert!(!dispatch.contains(forbidden));
        assert!(!definitions.contains(forbidden));
    }

    assert!(dispatch.contains("match plan {"));
    assert!(dispatch.contains("match operation {"));
    assert!(definitions.contains("pub const fn definition(id: OperationId)"));
}

#[test]
fn http_surface_does_not_expose_the_internal_workspace_action_type() {
    let api = include_str!("../../src/api.rs");
    let app = include_str!("../../src/app.rs");
    for source in [api, app] {
        assert!(!source.contains("WorkspaceAction"));
        assert!(!source.contains("create_no_op_session"));
        assert!(!source.contains("destroy_session"));
    }
}
