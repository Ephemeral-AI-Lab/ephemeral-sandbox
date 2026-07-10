use sandbox_operation_contract::{
    OperationExecutionOwner, OperationScopeKind, OperationVisibility,
};

type HandlerKey = (OperationScopeKind, &'static str);

#[test]
fn public_runtime_routes_and_handlers_are_bijective() {
    let handlers = sandbox_runtime::runtime_public_handler_keys().collect::<Vec<_>>();
    let routes = sandbox_operation_catalog::routes::runtime_routes()
        .iter()
        .filter(|route| {
            route.execution_owner == OperationExecutionOwner::Runtime
                && route.visibility == OperationVisibility::Public
        })
        .map(|route| (route.scope_kind, route.operation))
        .collect::<Vec<_>>();

    assert_bijective(&handlers, &routes);
}

#[test]
fn canonical_internal_routes_and_handlers_are_bijective() {
    let handlers = sandbox_runtime::runtime_internal_handler_keys().collect::<Vec<_>>();
    let routes = sandbox_operation_catalog::internal::runtime::ROUTES
        .iter()
        .map(|route| (route.scope_kind, route.operation))
        .collect::<Vec<_>>();

    assert_bijective(&handlers, &routes);
}

#[test]
fn runtime_registry_partitions_are_unique_and_disjoint() {
    let public = sandbox_runtime::runtime_public_handler_keys().collect::<Vec<_>>();
    let internal = sandbox_runtime::runtime_internal_handler_keys().collect::<Vec<_>>();
    let http_only = sandbox_runtime::runtime_http_only_handler_keys().collect::<Vec<_>>();

    assert_unique(&public);
    assert_unique(&internal);
    assert_unique(&http_only);
    assert_eq!(
        http_only,
        [(
            OperationScopeKind::Sandbox,
            sandbox_operation_catalog::internal::runtime::FILE_LIST,
        )]
    );
    assert_disjoint(&public, &internal);
    assert_disjoint(&public, &http_only);
    assert_disjoint(&internal, &http_only);
}

fn assert_bijective(actual: &[HandlerKey], expected: &[HandlerKey]) {
    assert_eq!(actual.len(), expected.len());
    assert_unique(actual);
    assert_unique(expected);
    for key in actual {
        assert!(expected.contains(key), "unexpected handler: {key:?}");
    }
    for key in expected {
        assert!(actual.contains(key), "missing handler: {key:?}");
    }
}

fn assert_unique(keys: &[HandlerKey]) {
    for key in keys {
        assert_eq!(
            keys.iter().filter(|candidate| *candidate == key).count(),
            1,
            "duplicate key: {key:?}"
        );
    }
}

fn assert_disjoint(left: &[HandlerKey], right: &[HandlerKey]) {
    for key in left {
        assert!(!right.contains(key), "registry overlap: {key:?}");
    }
}
