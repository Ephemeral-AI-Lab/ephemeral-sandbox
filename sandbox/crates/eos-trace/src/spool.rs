use std::collections::VecDeque;

use crate::codec::encoded_trace_record_len;
use crate::record::TraceRecord;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpoolInsertOutcome {
    Stored,
    DroppedNew,
    DroppedOld { count: u64 },
}

#[derive(Debug)]
pub struct TraceSpool {
    max_bytes: usize,
    current_bytes: usize,
    records: VecDeque<(TraceRecord, usize)>,
    dropped_traces: u64,
}

impl TraceSpool {
    #[must_use]
    pub fn new(max_bytes: usize) -> Self {
        Self {
            max_bytes,
            current_bytes: 0,
            records: VecDeque::new(),
            dropped_traces: 0,
        }
    }

    #[must_use]
    pub fn dropped_traces(&self) -> u64 {
        self.dropped_traces
    }

    #[must_use]
    pub fn pending_len(&self) -> usize {
        self.records.len()
    }

    pub fn push(&mut self, record: TraceRecord) -> SpoolInsertOutcome {
        let record_bytes = encoded_trace_record_len(&record);
        if record_bytes > self.max_bytes {
            self.dropped_traces = self.dropped_traces.saturating_add(1);
            return SpoolInsertOutcome::DroppedNew;
        }

        let mut dropped_old = 0_u64;
        while self.current_bytes.saturating_add(record_bytes) > self.max_bytes {
            let Some((_, dropped_bytes)) = self.records.pop_front() else {
                break;
            };
            self.current_bytes = self.current_bytes.saturating_sub(dropped_bytes);
            self.dropped_traces = self.dropped_traces.saturating_add(1);
            dropped_old = dropped_old.saturating_add(1);
        }

        self.current_bytes = self.current_bytes.saturating_add(record_bytes);
        self.records.push_back((record, record_bytes));
        if dropped_old == 0 {
            SpoolInsertOutcome::Stored
        } else {
            SpoolInsertOutcome::DroppedOld { count: dropped_old }
        }
    }

    #[must_use]
    pub fn drain_batch(&mut self, max_records: usize) -> Vec<TraceRecord> {
        let mut batch = Vec::new();
        for _ in 0..max_records {
            let Some((record, bytes)) = self.records.pop_front() else {
                break;
            };
            self.current_bytes = self.current_bytes.saturating_sub(bytes);
            batch.push(record);
        }
        batch
    }
}

impl Default for TraceSpool {
    fn default() -> Self {
        Self::new(4 * 1024 * 1024)
    }
}
