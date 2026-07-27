#[cfg(target_os = "linux")]
#[path = "cases/heavy_lead.rs"]
mod cases;

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires the lead-issued M2 PREPARE physical execution lease"]
fn m2_prepare_lead_heavy_fixtures() {
    cases::prepare_heavy().expect("M2 lead heavy fixture preparation must succeed");
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires the lead-issued HV-05 physical execution lease"]
fn hv_05_long_rapid_chain() {
    cases::run_hv05().expect("HV-05 must complete with durable evidence");
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires one exact lead-issued HV-06/HV-07/HV-09 physical execution lease"]
fn m2_heavy_campaign() {
    let selected = std::env::var("MPLA_POC_CASE_FILTER")
        .expect("MPLA_POC_CASE_FILTER must select exactly one heavy campaign case");
    match selected.as_str() {
        "HV-06" => cases::run_hv06().expect("HV-06 must complete with durable evidence"),
        "HV-07" => cases::run_hv07().expect("HV-07 must complete with durable evidence"),
        "HV-09" => cases::run_hv09().expect("HV-09 must complete with durable evidence"),
        other => panic!("unsupported M2 heavy campaign selector {other:?}"),
    }
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "disposable HV-07 process child; invoke only through the physical supervisor"]
fn m2_hv07_child() {
    cases::run_hv07_child().expect("HV-07 physical child must reach its exact stop marker");
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires the lead-issued HV-08 physical execution lease"]
fn hv_08_real_corpus_activation() {
    cases::run_hv08().expect("HV-08 must complete with durable evidence");
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires the lead-issued HV-10 physical execution lease"]
fn hv_10_lifecycle_scale_and_controls() {
    cases::run_hv10().expect("HV-10 must complete with durable evidence");
}

#[cfg(not(target_os = "linux"))]
#[test]
fn heavy_campaign_is_linux_gated() {
    assert_eq!(sandbox_runtime_mpla_poc::INTERFACE_VERSION, "m2-iface-v1");
}
