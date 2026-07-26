use sandbox_observability_telemetry::collect::process_topology::DaemonAllocatorMetrics;
use tikv_jemalloc_ctl::{epoch, stats};

pub(crate) fn collect_current() -> DaemonAllocatorMetrics {
    if epoch::advance().is_err() {
        return DaemonAllocatorMetrics::default();
    }
    let (Ok(allocated), Ok(active), Ok(mapped), Ok(resident)) = (
        stats::allocated::read(),
        stats::active::read(),
        stats::mapped::read(),
        stats::resident::read(),
    ) else {
        return DaemonAllocatorMetrics::default();
    };
    let (Some(allocated_bytes), Some(active_bytes), Some(mapped_bytes), Some(resident_bytes)) = (
        u64::try_from(allocated).ok(),
        u64::try_from(active).ok(),
        u64::try_from(mapped).ok(),
        u64::try_from(resident).ok(),
    ) else {
        return DaemonAllocatorMetrics::default();
    };

    DaemonAllocatorMetrics {
        supported: true,
        allocated_bytes: Some(allocated_bytes),
        active_bytes: Some(active_bytes),
        mapped_bytes: Some(mapped_bytes),
        resident_bytes: Some(resident_bytes),
    }
}
