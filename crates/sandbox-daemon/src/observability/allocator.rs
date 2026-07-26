//! On-demand allocator capability for the daemon self-metrics view.

use sandbox_observability_telemetry::collect::process_topology::DaemonAllocatorMetrics;

pub(crate) fn collect_current() -> DaemonAllocatorMetrics {
    crate::allocator_backend::collect_current()
}
