mod create;
mod destroy;
pub(crate) mod leases;
mod persistence;

use std::collections::HashMap;
use std::time::Instant;

pub use destroy::ExitOutcome;
pub(crate) use leases::monotonic_seconds;

pub(crate) fn record_phase_ms(
    phases_ms: &mut HashMap<String, f64>,
    phase: &str,
    started_at: Instant,
) {
    phases_ms.insert(
        phase.to_owned(),
        started_at.elapsed().as_secs_f64() * 1000.0,
    );
}
