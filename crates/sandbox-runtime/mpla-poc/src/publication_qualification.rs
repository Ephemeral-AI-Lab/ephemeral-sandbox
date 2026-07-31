use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    ControlIntent, ControlOperationReceipt, ControlVerdict, MonotonicSpan, PocError, PocResult,
    MATCHED_PUBLICATION_START_BOUNDARY, MATCHED_PUBLICATION_STOP_BOUNDARY,
};

pub const REQUIRED_PUBLICATION_NS: u64 = 100_000_000;
pub const PREFERRED_PUBLICATION_NS: u64 = 20_000_000;
pub const MATCHED_PUBLICATION_TIMING_BASIS: &str = "matched_publication.span.elapsed_ns";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MatchedPublicationReceipt {
    pub start_boundary: String,
    pub stop_boundary: String,
    pub admission_gate_included: bool,
    pub durable_root_committed: bool,
    pub session_closed: bool,
    pub span: MonotonicSpan,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicationCandidateTiming {
    pub outer_elapsed_ns: u64,
    pub service_elapsed_ns: u64,
    pub matched_publication: MatchedPublicationReceipt,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PublicationTimingQualification {
    pub candidate_ns: Vec<u64>,
    pub matched_candidate_ns: Vec<u64>,
    pub control_ns: Vec<u64>,
    pub candidate_median_ns: u64,
    pub candidate_max_ns: u64,
    pub matched_candidate_median_ns: u64,
    pub control_median_ns: u64,
    pub median_ratio_numerator: u64,
    pub median_ratio_denominator: u64,
    pub required: bool,
    pub preferred: bool,
}

pub fn qualify_publication_timings(
    matched_depth_one: &[PublicationCandidateTiming],
    controls: &[ControlOperationReceipt],
    depth_five: &PublicationCandidateTiming,
    maximum_depth: &PublicationCandidateTiming,
) -> PocResult<PublicationTimingQualification> {
    if matched_depth_one.len() != 3 || controls.len() != 3 {
        return Err(PocError::Integrity(
            "publication timing qualification requires exactly three matched depth-one pairs"
                .to_owned(),
        ));
    }

    for sample in matched_depth_one.iter().chain([depth_five, maximum_depth]) {
        validate_candidate_matched_boundary(&sample.matched_publication)?;
    }
    for (sample, control) in matched_depth_one.iter().zip(controls) {
        validate_matched_control_boundary(control)?;
        if sample.matched_publication.span.clock != control.span.clock {
            return Err(PocError::Integrity(
                "publication candidate/control clocks do not match".to_owned(),
            ));
        }
    }

    let matched_candidate_ns = matched_depth_one
        .iter()
        .map(|sample| sample.matched_publication.span.elapsed_ns)
        .collect::<Vec<_>>();
    let control_ns = controls
        .iter()
        .map(|control| control.span.elapsed_ns)
        .collect::<Vec<_>>();
    let candidate_ns = matched_candidate_ns
        .iter()
        .copied()
        .chain([
            depth_five.matched_publication.span.elapsed_ns,
            maximum_depth.matched_publication.span.elapsed_ns,
        ])
        .collect::<Vec<_>>();
    let candidate_median_ns = median(&candidate_ns);
    let candidate_max_ns = candidate_ns.iter().copied().max().unwrap_or(u64::MAX);
    let matched_candidate_median_ns = median(&matched_candidate_ns);
    let control_median_ns = median(&control_ns);
    let required_ratio = ratio_at_least(control_median_ns, matched_candidate_median_ns, 100);
    let preferred_ratio = ratio_at_least(control_median_ns, matched_candidate_median_ns, 500);

    Ok(PublicationTimingQualification {
        required: samples_within_ceiling(&candidate_ns, REQUIRED_PUBLICATION_NS) && required_ratio,
        preferred: samples_within_ceiling(&candidate_ns, PREFERRED_PUBLICATION_NS)
            && preferred_ratio,
        candidate_ns,
        matched_candidate_ns,
        control_ns,
        candidate_median_ns,
        candidate_max_ns,
        matched_candidate_median_ns,
        control_median_ns,
        median_ratio_numerator: control_median_ns,
        median_ratio_denominator: matched_candidate_median_ns,
    })
}

pub fn validate_candidate_matched_boundary(receipt: &MatchedPublicationReceipt) -> PocResult<()> {
    if receipt.start_boundary != MATCHED_PUBLICATION_START_BOUNDARY
        || receipt.stop_boundary != MATCHED_PUBLICATION_STOP_BOUNDARY
        || !receipt.admission_gate_included
        || !receipt.durable_root_committed
        || !receipt.session_closed
        || !valid_nonzero_span(&receipt.span)
    {
        return Err(PocError::Integrity(
            "candidate matched-publication boundary receipt is invalid".to_owned(),
        ));
    }
    Ok(())
}

pub fn validate_matched_control_boundary(control: &ControlOperationReceipt) -> PocResult<()> {
    if control.intent != ControlIntent::ClosingPublication
        || control.verdict != ControlVerdict::Matched
        || control.boundary.verdict()? != ControlVerdict::Matched
        || control.boundary.candidate_start != MATCHED_PUBLICATION_START_BOUNDARY
        || control.boundary.current_i2_start != MATCHED_PUBLICATION_START_BOUNDARY
        || control.boundary.candidate_stop != MATCHED_PUBLICATION_STOP_BOUNDARY
        || control.boundary.current_i2_stop != MATCHED_PUBLICATION_STOP_BOUNDARY
        || control
            .publication
            .as_ref()
            .is_none_or(|publication| !publication.matched)
        || !valid_nonzero_span(&control.span)
    {
        return Err(PocError::Integrity(
            "current-I2 publication control boundary is not matched".to_owned(),
        ));
    }
    Ok(())
}

#[must_use]
pub fn publication_is_fresh(response: &Value) -> bool {
    response
        .pointer("/lifecycle/idempotent_replay")
        .and_then(Value::as_bool)
        == Some(false)
}

fn valid_nonzero_span(span: &MonotonicSpan) -> bool {
    span.elapsed_ns > 0 && span.finished_ns.checked_sub(span.started_ns) == Some(span.elapsed_ns)
}

fn samples_within_ceiling(samples: &[u64], ceiling_ns: u64) -> bool {
    !samples.is_empty() && samples.iter().all(|sample| *sample <= ceiling_ns)
}

fn ratio_at_least(numerator: u64, denominator: u64, minimum: u64) -> bool {
    denominator != 0 && (numerator as u128) >= (denominator as u128) * (minimum as u128)
}

fn median(values: &[u64]) -> u64 {
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    sorted[sorted.len() / 2]
}
