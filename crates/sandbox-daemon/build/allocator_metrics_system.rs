use sandbox_observability_telemetry::collect::process_topology::DaemonAllocatorMetrics;

pub(crate) fn collect_current() -> DaemonAllocatorMetrics {
    DaemonAllocatorMetrics::default()
}
