use sandbox_benchmark::statistics::{
    bootstrap_median_difference_interval, bootstrap_pearson_interval, summarize,
    ConfidenceIntervalOmission, DistributionProjection, HistogramMethod,
    PearsonConfidenceIntervalOmission, PearsonConfidenceMethod, StatisticsError,
    BOOTSTRAP_RESAMPLES,
};

#[test]
fn descriptive_statistics_follow_the_documented_quantile_and_small_sample_rules() {
    let samples = [4.0, 1.0, 3.0, 2.0];
    let summary = summarize(&samples, 7).expect("summarize finite samples");

    assert_eq!(summary.count, 4);
    assert_close(summary.minimum, 1.0);
    assert_close(summary.maximum, 4.0);
    assert_close(summary.mean, 2.5);
    assert_close(summary.sample_standard_deviation, (5.0_f64 / 3.0).sqrt());
    assert_close(summary.median, 2.5);
    assert_close(summary.median_absolute_deviation, 1.0);
    assert_close(summary.p25, 1.75);
    assert_close(summary.p75, 3.25);
    assert_close(summary.p95, 3.85);
    assert!(summary.median_confidence_interval.is_none());
    assert_eq!(
        summary.confidence_interval_omission,
        Some(ConfidenceIntervalOmission::InsufficientN)
    );
    assert!(summary.p95_exploratory);
    assert!(summary.outlier_indices.is_empty());
    assert_eq!(
        summary.distribution,
        DistributionProjection::RawPoints {
            values: samples.to_vec(),
        }
    );
}

#[test]
fn bootstrap_intervals_are_seeded_and_deterministic() {
    let samples = [1.0, 2.0, 3.0, 4.0, 5.0, 8.0];
    let first = summarize(&samples, 0x5eed).expect("first summary");
    let second = summarize(&samples, 0x5eed).expect("second summary");
    assert_eq!(first, second);
    let interval = first
        .median_confidence_interval
        .expect("confidence interval for n >= 5");
    assert_eq!(interval.resamples, BOOTSTRAP_RESAMPLES);
    assert_eq!(first.confidence_interval_omission, None);

    let difference = bootstrap_median_difference_interval(
        &[1.0, 1.0, 1.0, 1.0, 1.0],
        &[3.0, 3.0, 3.0, 3.0, 3.0],
        42,
    )
    .expect("bootstrap difference")
    .expect("difference interval for n >= 5");
    assert_eq!(difference.lower, 2.0);
    assert_eq!(difference.upper, 2.0);
    assert_eq!(difference.resamples, BOOTSTRAP_RESAMPLES);
}

#[test]
fn large_single_value_samples_use_histogram_and_ecdf_projection() {
    let samples = vec![7.0; 30];
    let summary = summarize(&samples, 19).expect("summarize single-value sample");

    assert!(!summary.p95_exploratory);
    match summary.distribution {
        DistributionProjection::HistogramEcdf { histogram, ecdf } => {
            assert_eq!(histogram.method, HistogramMethod::SingleValue);
            assert_eq!(histogram.edges, vec![7.0, 7.0]);
            assert_eq!(histogram.counts, vec![30]);
            assert_eq!(ecdf.len(), 30);
            assert_eq!(ecdf.last().expect("last ECDF point").value, 7.0);
            assert_eq!(
                ecdf.last().expect("last ECDF point").cumulative_probability,
                1.0
            );
        }
        other => panic!("expected histogram/ECDF projection, got {other:?}"),
    }
}

#[test]
fn empty_and_non_finite_samples_are_handled_without_inventing_values() {
    let empty = summarize(&[], 0).expect("summarize empty sample");
    assert_eq!(empty.count, 0);
    assert_eq!(empty.minimum, None);
    assert_eq!(empty.mean, None);
    assert_eq!(empty.distribution, DistributionProjection::Empty);

    assert_eq!(
        summarize(&[1.0, f64::NAN, 2.0], 0),
        Err(StatisticsError::NonFinite { index: 1 })
    );
}

#[test]
fn pearson_interval_is_seeded_and_reports_why_it_is_unavailable() {
    let pairs = [(1.0, 2.0), (2.0, 4.0), (3.0, 6.0), (4.0, 8.0), (5.0, 10.0)];
    let first = bootstrap_pearson_interval(&pairs, 0x5eed).expect("Pearson interval");
    let second = bootstrap_pearson_interval(&pairs, 0x5eed).expect("repeat Pearson interval");
    assert_eq!(first, second);
    let interval = first.interval.expect("interval for five correlated pairs");
    assert_eq!(
        interval.method,
        PearsonConfidenceMethod::PercentileBootstrapPearson
    );
    assert_eq!(interval.resamples, BOOTSTRAP_RESAMPLES);
    assert!(interval.valid_resamples <= BOOTSTRAP_RESAMPLES);
    assert!((interval.lower - 1.0).abs() < 1e-12);
    assert!((interval.upper - 1.0).abs() < 1e-12);
    assert_eq!(first.omission, None);

    let too_small = bootstrap_pearson_interval(&pairs[..4], 1).expect("small Pearson sample");
    assert_eq!(
        too_small.omission,
        Some(PearsonConfidenceIntervalOmission::InsufficientN)
    );
    let constant = bootstrap_pearson_interval(
        &[(1.0, 2.0), (1.0, 3.0), (1.0, 4.0), (1.0, 5.0), (1.0, 6.0)],
        1,
    )
    .expect("constant Pearson sample");
    assert_eq!(
        constant.omission,
        Some(PearsonConfidenceIntervalOmission::ZeroVariance)
    );
}

fn assert_close(actual: Option<f64>, expected: f64) {
    let actual = actual.expect("statistic is present");
    assert!(
        (actual - expected).abs() <= f64::EPSILON * 8.0,
        "expected {expected}, got {actual}"
    );
}
