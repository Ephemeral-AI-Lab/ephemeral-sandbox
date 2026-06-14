use trace::{DetailBudget, TraceRecord};

/// Request records cannot spool, so an oversize record drops subsystem
/// children (oldest first, then resource samples) with an explicit
/// `dropped_children` count; never the root or transport frame events.
pub(super) fn enforce_sidecar_record_budget(record: &mut TraceRecord) {
    let budget = DetailBudget::SidecarRecord.bytes();
    while trace::codec::encoded_trace_record_len(record) > budget {
        if let Some(index) = record
            .events
            .iter()
            .position(|event| !event.module.starts_with("daemon."))
        {
            record.events.remove(index);
        } else if !record.resources.is_empty() {
            record.resources.remove(0);
        } else {
            break;
        }
        record.dropped_children = record.dropped_children.saturating_add(1);
        record.truncated = true;
    }
}
