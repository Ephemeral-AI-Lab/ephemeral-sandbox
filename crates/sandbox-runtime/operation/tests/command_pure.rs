mod scratch_route {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/command/scratch_route.rs"
    ));
}

mod terminal_echo {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/command/terminal_echo.rs"
    ));
}

use sandbox_runtime_workspace::ExecutionScratchRoute;

#[test]
fn observed_scratch_route_preserves_retained_legacy_ownership() {
    assert_eq!(
        scratch_route::observed_scratch_route(&[ExecutionScratchRoute::LegacyCompat]),
        "legacy_compat"
    );
    assert_eq!(
        scratch_route::observed_scratch_route(&[
            ExecutionScratchRoute::LegacyCompat,
            ExecutionScratchRoute::WorkspaceScoped,
        ]),
        "mixed"
    );
    assert_eq!(
        scratch_route::observed_scratch_route(&[]),
        "workspace_scoped"
    );
}

#[test]
fn terminal_echo_bound_covers_control_expansion() {
    assert_eq!(terminal_echo::max_terminal_echo_bytes("input\n"), 12);
    assert_eq!(terminal_echo::max_terminal_echo_bytes("\u{7}"), 2);
}
