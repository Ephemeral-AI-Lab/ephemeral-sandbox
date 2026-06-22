mod service;

use crate::operation::OperationFamilySpec;

pub use service::{
    CgroupMonitorOperationService, CgroupMonitorServiceError, InspectCgroupMonitorInput,
    InspectCgroupMonitorOutput, ReadCgroupMonitorSamplesInput, ReadCgroupMonitorSamplesOutput,
};

pub(crate) const CGROUP_MONITOR_FAMILY: OperationFamilySpec = OperationFamilySpec {
    id: "cgroup_monitor",
    title: "Cgroup Monitor",
    summary: "Inspect cgroup resource usage and retained samples.",
    description:
        "Inspect session and command cgroup CPU, memory, IO, pressure, PID, disk, and cleanup state.",
};

const FAMILIES: &[&OperationFamilySpec] = &[&CGROUP_MONITOR_FAMILY];

pub(crate) fn operation_entries() -> &'static [crate::operation::OperationEntry] {
    service::operation_entries()
}

pub(crate) const fn operation_families() -> &'static [&'static OperationFamilySpec] {
    FAMILIES
}

pub(crate) fn operation_specs() -> &'static [&'static crate::operation::CliOperationSpec] {
    service::operation_specs()
}
