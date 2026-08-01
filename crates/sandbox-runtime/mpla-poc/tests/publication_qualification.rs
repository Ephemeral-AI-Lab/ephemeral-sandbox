use sandbox_runtime_mpla_poc::publication_qualification::{
    publication_is_fresh, qualify_publication_timings, validate_candidate_matched_boundary,
    validate_matched_control_boundary, MatchedPublicationReceipt, PublicationCandidateTiming,
    MATCHED_CONTROL_IMPLEMENTATION, MATCHED_CONTROL_OPERATION, REQUIRED_PUBLICATION_NS,
};
use sandbox_runtime_mpla_poc::{
    CatalogCoverageReceipt, ControlApiCoverage, ControlBoundary, ControlCacheMatch, ControlIntent,
    ControlOperationReceipt, ControlPublicationOutcome, ControlVerdict, MonotonicClock,
    MonotonicSpan, MATCHED_PUBLICATION_START_BOUNDARY, MATCHED_PUBLICATION_STOP_BOUNDARY,
};
use serde_json::json;

fn span(elapsed_ns: u64) -> MonotonicSpan {
    MonotonicSpan {
        clock: MonotonicClock::Monotonic,
        started_ns: 10,
        finished_ns: 10 + elapsed_ns,
        elapsed_ns,
    }
}

fn matched_receipt(elapsed_ns: u64) -> MatchedPublicationReceipt {
    MatchedPublicationReceipt {
        start_boundary: MATCHED_PUBLICATION_START_BOUNDARY.to_owned(),
        stop_boundary: MATCHED_PUBLICATION_STOP_BOUNDARY.to_owned(),
        admission_gate_included: true,
        durable_root_committed: true,
        session_closed: true,
        span: span(elapsed_ns),
    }
}

fn candidate(
    matched_elapsed_ns: u64,
    outer_elapsed_ns: u64,
    service_elapsed_ns: u64,
) -> PublicationCandidateTiming {
    PublicationCandidateTiming {
        outer_elapsed_ns,
        service_elapsed_ns,
        matched_publication: matched_receipt(matched_elapsed_ns),
    }
}

fn matched_control(elapsed_ns: u64) -> ControlOperationReceipt {
    ControlOperationReceipt {
        schema_version: 1,
        implementation: MATCHED_CONTROL_IMPLEMENTATION.to_owned(),
        intent: ControlIntent::ClosingPublication,
        catalog_binding_id: "catalog-binding".to_owned(),
        coverage: CatalogCoverageReceipt {
            classification: ControlApiCoverage::PublicIntentProgrammaticCurrentI2,
            product_operation: MATCHED_CONTROL_OPERATION.to_owned(),
            product_operation_present: true,
            direct_control_api: MATCHED_CONTROL_OPERATION.to_owned(),
        },
        boundary: ControlBoundary {
            candidate_start: MATCHED_PUBLICATION_START_BOUNDARY.to_owned(),
            candidate_stop: MATCHED_PUBLICATION_STOP_BOUNDARY.to_owned(),
            current_i2_start: MATCHED_PUBLICATION_START_BOUNDARY.to_owned(),
            current_i2_stop: MATCHED_PUBLICATION_STOP_BOUNDARY.to_owned(),
            same_fixture: true,
            same_intent: true,
            same_durability: true,
            same_readiness: true,
            cache_state: ControlCacheMatch::NotApplicable,
            unknown_reason: None,
        },
        verdict: ControlVerdict::Matched,
        started_unix_ms: 1,
        span: span(elapsed_ns),
        source: None,
        publication: Some(ControlPublicationOutcome {
            correlation_id: "correlation".to_owned(),
            candidate_generation: 1,
            matched: true,
        }),
        materialization: None,
        readiness: None,
    }
}

#[test]
fn timing_qualification_uses_five_matched_spans_and_only_three_paired_medians() {
    let depth_one = [
        candidate(10_000_000, 900_000_000, 800_000_000),
        candidate(20_000_000, 700_000_000, 600_000_000),
        candidate(30_000_000, 500_000_000, 400_000_000),
    ];
    let controls = [
        matched_control(10_000_000_000),
        matched_control(20_000_000_000),
        matched_control(30_000_000_000),
    ];
    let depth_five = candidate(40_000_000, 300_000_000, 200_000_000);
    let maximum_depth = candidate(REQUIRED_PUBLICATION_NS, 1_000_000_000, 900_000_000);

    let qualified = qualify_publication_timings(&depth_one, &controls, &depth_five, &maximum_depth)
        .expect("valid timing receipts must qualify");

    assert_eq!(
        qualified.candidate_ns,
        [
            10_000_000,
            20_000_000,
            30_000_000,
            40_000_000,
            REQUIRED_PUBLICATION_NS
        ]
    );
    assert_eq!(
        qualified.matched_candidate_ns,
        [10_000_000, 20_000_000, 30_000_000]
    );
    assert_eq!(
        qualified.control_ns,
        [10_000_000_000, 20_000_000_000, 30_000_000_000]
    );
    assert_eq!(qualified.matched_candidate_median_ns, 20_000_000);
    assert_eq!(qualified.control_median_ns, 20_000_000_000);
    assert_eq!(qualified.median_ratio_numerator, 20_000_000_000);
    assert_eq!(qualified.median_ratio_denominator, 20_000_000);
    assert!(qualified.required);
    assert!(!qualified.preferred);
}

#[test]
fn timing_qualification_ignores_outer_and_service_diagnostics() {
    let controls = [
        matched_control(10_000_000_000),
        matched_control(20_000_000_000),
        matched_control(30_000_000_000),
    ];
    let fast_diagnostics = [
        candidate(10_000_000, 1, 1),
        candidate(20_000_000, 1, 1),
        candidate(30_000_000, 1, 1),
    ];
    let slow_diagnostics = [
        candidate(10_000_000, u64::MAX, u64::MAX),
        candidate(20_000_000, u64::MAX, u64::MAX),
        candidate(30_000_000, u64::MAX, u64::MAX),
    ];
    let fast_depth_five = candidate(15_000_000, 1, 1);
    let slow_depth_five = candidate(15_000_000, u64::MAX, u64::MAX);
    let fast_maximum = candidate(16_000_000, 1, 1);
    let slow_maximum = candidate(16_000_000, u64::MAX, u64::MAX);

    let fast = qualify_publication_timings(
        &fast_diagnostics,
        &controls,
        &fast_depth_five,
        &fast_maximum,
    )
    .expect("fast diagnostics");
    let slow = qualify_publication_timings(
        &slow_diagnostics,
        &controls,
        &slow_depth_five,
        &slow_maximum,
    )
    .expect("slow diagnostics");

    assert_eq!(fast, slow);
}

#[test]
fn required_absolute_ceiling_is_inclusive_and_applies_to_every_sample() {
    let depth_one = [
        candidate(10_000_000, 0, 0),
        candidate(20_000_000, 0, 0),
        candidate(30_000_000, 0, 0),
    ];
    let controls = [
        matched_control(10_000_000_000),
        matched_control(20_000_000_000),
        matched_control(30_000_000_000),
    ];
    let depth_five = candidate(40_000_000, 0, 0);
    let exact = candidate(REQUIRED_PUBLICATION_NS, 0, 0);
    let over = candidate(REQUIRED_PUBLICATION_NS + 1, 0, 0);

    assert!(
        qualify_publication_timings(&depth_one, &controls, &depth_five, &exact)
            .expect("exact boundary")
            .required
    );
    assert!(
        !qualify_publication_timings(&depth_one, &controls, &depth_five, &over)
            .expect("over boundary")
            .required
    );
}

#[test]
fn publication_freshness_fails_closed() {
    assert!(publication_is_fresh(
        &json!({"lifecycle": {"idempotent_replay": false}})
    ));
    assert!(!publication_is_fresh(
        &json!({"lifecycle": {"idempotent_replay": true}})
    ));
    assert!(!publication_is_fresh(&json!({"lifecycle": {}})));
    assert!(!publication_is_fresh(&json!({})));
}

#[test]
fn candidate_boundary_receipt_rejects_each_invalid_dimension() {
    let valid = matched_receipt(10);
    assert!(validate_candidate_matched_boundary(&valid).is_ok());

    let mut wrong_start = valid.clone();
    wrong_start.start_boundary = "wrong".to_owned();
    assert!(validate_candidate_matched_boundary(&wrong_start).is_err());

    let mut wrong_stop = valid.clone();
    wrong_stop.stop_boundary = "wrong".to_owned();
    assert!(validate_candidate_matched_boundary(&wrong_stop).is_err());

    let mut no_admission = valid.clone();
    no_admission.admission_gate_included = false;
    assert!(validate_candidate_matched_boundary(&no_admission).is_err());

    let mut no_durable_root = valid.clone();
    no_durable_root.durable_root_committed = false;
    assert!(validate_candidate_matched_boundary(&no_durable_root).is_err());

    let mut open_session = valid.clone();
    open_session.session_closed = false;
    assert!(validate_candidate_matched_boundary(&open_session).is_err());

    let mut zero = valid.clone();
    zero.span = span(0);
    assert!(validate_candidate_matched_boundary(&zero).is_err());

    let mut wrong_arithmetic = valid;
    wrong_arithmetic.span.elapsed_ns = 9;
    assert!(validate_candidate_matched_boundary(&wrong_arithmetic).is_err());
}

#[test]
fn matched_control_receipt_rejects_unknown_or_nonpublication_outcomes() {
    let valid = matched_control(10);
    assert!(validate_matched_control_boundary(&valid).is_ok());

    let mut unknown = valid.clone();
    unknown.verdict = ControlVerdict::Unknown;
    assert!(validate_matched_control_boundary(&unknown).is_err());

    let mut missing = valid.clone();
    missing.publication = None;
    assert!(validate_matched_control_boundary(&missing).is_err());

    let mut unmatched = valid;
    unmatched.publication.as_mut().expect("publication").matched = false;
    assert!(validate_matched_control_boundary(&unmatched).is_err());
}

#[test]
fn matched_control_receipt_requires_the_public_product_operation() {
    let valid = matched_control(10);

    let mut wrong_implementation = valid.clone();
    wrong_implementation.implementation = "current_i2_layerstack".to_owned();
    assert!(validate_matched_control_boundary(&wrong_implementation).is_err());

    let mut hidden_api = valid.clone();
    hidden_api.coverage.direct_control_api = "LayerStack::publish_hidden_validation".to_owned();
    assert!(validate_matched_control_boundary(&hidden_api).is_err());

    let mut missing_operation = valid.clone();
    missing_operation.coverage.product_operation_present = false;
    assert!(validate_matched_control_boundary(&missing_operation).is_err());

    let mut wrong_classification = valid.clone();
    wrong_classification.coverage.classification = ControlApiCoverage::ProgrammaticCurrentControl;
    assert!(validate_matched_control_boundary(&wrong_classification).is_err());

    let mut empty_correlation = valid.clone();
    empty_correlation
        .publication
        .as_mut()
        .expect("publication")
        .correlation_id
        .clear();
    assert!(validate_matched_control_boundary(&empty_correlation).is_err());

    let mut generation_zero = valid;
    generation_zero
        .publication
        .as_mut()
        .expect("publication")
        .candidate_generation = 0;
    assert!(validate_matched_control_boundary(&generation_zero).is_err());
}

#[test]
fn matched_control_receipt_rejects_wrong_boundaries_and_spans() {
    let valid = matched_control(10);

    for field in 0..4 {
        let mut wrong = valid.clone();
        match field {
            0 => wrong.boundary.candidate_start = "wrong".to_owned(),
            1 => wrong.boundary.current_i2_start = "wrong".to_owned(),
            2 => wrong.boundary.candidate_stop = "wrong".to_owned(),
            3 => wrong.boundary.current_i2_stop = "wrong".to_owned(),
            _ => unreachable!(),
        }
        assert!(validate_matched_control_boundary(&wrong).is_err());
    }

    let mut zero = valid.clone();
    zero.span = span(0);
    assert!(validate_matched_control_boundary(&zero).is_err());

    let mut wrong_arithmetic = valid;
    wrong_arithmetic.span.elapsed_ns = 9;
    assert!(validate_matched_control_boundary(&wrong_arithmetic).is_err());
}
