#[path = "cases/hv07_cgroup_binding.rs"]
mod hv07_cgroup_binding;

#[cfg(target_os = "linux")]
#[path = "cases/heavy_lead.rs"]
mod cases;

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires the qualified live Docker environment"]
fn hv_07_fresh_crash_sweep() {
    cases::prepare_hv07().expect("prepare dedicated HV-07 fixtures");
    cases::run_hv07_fresh_sweep().expect("complete the physical HV-07 crash sweep");
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "invoked by the HV-07 parent process"]
fn m2_hv07_child() {
    cases::run_hv07_child().expect("execute the physical HV-07 child operation");
}
