#[cfg(target_os = "linux")]
#[path = "cases/mod.rs"]
mod cases;

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires the qualified Linux MPLA physical profile"]
fn m1_prepare_smoke_fixtures() {
    cases::prepare().expect("M1 smoke fixture preparation must succeed");
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires the qualified Linux MPLA physical profile"]
fn m1_smoke_campaign() {
    cases::run().expect("M1 smoke campaign must pass");
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "spawned only by the SM-12 physical crash campaign"]
fn m1_sm12_child_fault_then_sigkill() {
    cases::sm12_child_fault_then_sigkill().expect("SM-12 child fault must terminate via SIGKILL");
}

#[cfg(not(target_os = "linux"))]
#[test]
fn smoke_campaign_is_linux_gated() {
    assert_eq!(sandbox_runtime_mpla_poc::INTERFACE_VERSION, "m1-iface-v1");
}
