use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const STATISTICS_SCHEMA_VERSION: u32 = 1;
pub const BOOTSTRAP_RESAMPLES: usize = 10_000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SampleStatistics {
    pub schema_version: u32,
    pub count: usize,
    pub minimum: Option<f64>,
    pub maximum: Option<f64>,
    pub mean: Option<f64>,
    pub sample_standard_deviation: Option<f64>,
    pub median: Option<f64>,
    pub median_absolute_deviation: Option<f64>,
    pub p25: Option<f64>,
    pub p75: Option<f64>,
    pub p95: Option<f64>,
    pub coefficient_of_variation: Option<f64>,
    pub median_confidence_interval: Option<ConfidenceInterval>,
    pub confidence_interval_omission: Option<ConfidenceIntervalOmission>,
    pub p95_exploratory: bool,
    pub outlier_indices: Vec<usize>,
    pub distribution: DistributionProjection,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ConfidenceInterval {
    pub level: f64,
    pub lower: f64,
    pub upper: f64,
    pub method: ConfidenceMethod,
    pub resamples: usize,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConfidenceMethod {
    PercentileBootstrapMedian,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConfidenceIntervalOmission {
    InsufficientN,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PearsonConfidenceInterval {
    pub level: f64,
    pub lower: f64,
    pub upper: f64,
    pub method: PearsonConfidenceMethod,
    pub resamples: usize,
    pub valid_resamples: usize,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PearsonConfidenceMethod {
    PercentileBootstrapPearson,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PearsonConfidenceIntervalOmission {
    InsufficientN,
    ZeroVariance,
    InsufficientValidResamples,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PearsonConfidenceEstimate {
    pub interval: Option<PearsonConfidenceInterval>,
    pub omission: Option<PearsonConfidenceIntervalOmission>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DistributionProjection {
    Empty,
    RawPoints {
        values: Vec<f64>,
    },
    HistogramEcdf {
        histogram: Histogram,
        ecdf: Vec<EcdfPoint>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Histogram {
    pub method: HistogramMethod,
    pub edges: Vec<f64>,
    pub counts: Vec<usize>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HistogramMethod {
    FreedmanDiaconis,
    Sturges,
    SingleValue,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EcdfPoint {
    pub value: f64,
    pub cumulative_probability: f64,
}

#[derive(Debug, Error, PartialEq)]
pub enum StatisticsError {
    #[error("sample at index {index} is not finite")]
    NonFinite { index: usize },
}

pub fn summarize(
    samples: &[f64],
    bootstrap_seed: u64,
) -> Result<SampleStatistics, StatisticsError> {
    if let Some((index, _)) = samples
        .iter()
        .enumerate()
        .find(|(_, sample)| !sample.is_finite())
    {
        return Err(StatisticsError::NonFinite { index });
    }
    if samples.is_empty() {
        return Ok(SampleStatistics {
            schema_version: STATISTICS_SCHEMA_VERSION,
            count: 0,
            minimum: None,
            maximum: None,
            mean: None,
            sample_standard_deviation: None,
            median: None,
            median_absolute_deviation: None,
            p25: None,
            p75: None,
            p95: None,
            coefficient_of_variation: None,
            median_confidence_interval: None,
            confidence_interval_omission: Some(ConfidenceIntervalOmission::InsufficientN),
            p95_exploratory: true,
            outlier_indices: Vec::new(),
            distribution: DistributionProjection::Empty,
        });
    }

    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    let count = sorted.len();
    let mean = sorted.iter().sum::<f64>() / count as f64;
    let sample_standard_deviation = (count > 1).then(|| {
        let squared_sum = sorted
            .iter()
            .map(|sample| (sample - mean).powi(2))
            .sum::<f64>();
        (squared_sum / (count - 1) as f64).sqrt()
    });
    let median = quantile(&sorted, 0.5);
    let mut deviations = sorted
        .iter()
        .map(|sample| (sample - median).abs())
        .collect::<Vec<_>>();
    deviations.sort_by(f64::total_cmp);
    let p25 = quantile(&sorted, 0.25);
    let p75 = quantile(&sorted, 0.75);
    let lower_fence = p25 - 1.5 * (p75 - p25);
    let upper_fence = p75 + 1.5 * (p75 - p25);
    let outlier_indices = samples
        .iter()
        .enumerate()
        .filter_map(|(index, sample)| {
            (*sample < lower_fence || *sample > upper_fence).then_some(index)
        })
        .collect();
    let median_confidence_interval =
        (count >= 5).then(|| bootstrap_median_interval(&sorted, bootstrap_seed));
    let distribution = if count < 30 {
        DistributionProjection::RawPoints {
            values: samples.to_vec(),
        }
    } else {
        DistributionProjection::HistogramEcdf {
            histogram: histogram(&sorted, p25, p75),
            ecdf: sorted
                .iter()
                .enumerate()
                .map(|(index, value)| EcdfPoint {
                    value: *value,
                    cumulative_probability: (index + 1) as f64 / count as f64,
                })
                .collect(),
        }
    };

    Ok(SampleStatistics {
        schema_version: STATISTICS_SCHEMA_VERSION,
        count,
        minimum: sorted.first().copied(),
        maximum: sorted.last().copied(),
        mean: Some(mean),
        sample_standard_deviation,
        median: Some(median),
        median_absolute_deviation: Some(quantile(&deviations, 0.5)),
        p25: Some(p25),
        p75: Some(p75),
        p95: Some(quantile(&sorted, 0.95)),
        coefficient_of_variation: (mean != 0.0)
            .then(|| sample_standard_deviation.map(|deviation| deviation / mean))
            .flatten(),
        median_confidence_interval,
        confidence_interval_omission: (count < 5)
            .then_some(ConfidenceIntervalOmission::InsufficientN),
        p95_exploratory: count < 20,
        outlier_indices,
        distribution,
    })
}

pub fn bootstrap_median_difference_interval(
    reference: &[f64],
    candidate: &[f64],
    seed: u64,
) -> Result<Option<ConfidenceInterval>, StatisticsError> {
    validate_finite(reference)?;
    validate_finite(candidate)?;
    if reference.len() < 5 || candidate.len() < 5 {
        return Ok(None);
    }
    let mut rng = SplitMix64::new(seed);
    let mut reference_resample = vec![0.0; reference.len()];
    let mut candidate_resample = vec![0.0; candidate.len()];
    let mut differences = Vec::with_capacity(BOOTSTRAP_RESAMPLES);
    for _ in 0..BOOTSTRAP_RESAMPLES {
        resample(reference, &mut reference_resample, &mut rng);
        resample(candidate, &mut candidate_resample, &mut rng);
        reference_resample.sort_by(f64::total_cmp);
        candidate_resample.sort_by(f64::total_cmp);
        differences.push(quantile(&candidate_resample, 0.5) - quantile(&reference_resample, 0.5));
    }
    differences.sort_by(f64::total_cmp);
    Ok(Some(ConfidenceInterval {
        level: 0.95,
        lower: quantile(&differences, 0.025),
        upper: quantile(&differences, 0.975),
        method: ConfidenceMethod::PercentileBootstrapMedian,
        resamples: BOOTSTRAP_RESAMPLES,
    }))
}

pub fn bootstrap_pearson_interval(
    pairs: &[(f64, f64)],
    seed: u64,
) -> Result<PearsonConfidenceEstimate, StatisticsError> {
    for (index, (left, right)) in pairs.iter().copied().enumerate() {
        if !left.is_finite() {
            return Err(StatisticsError::NonFinite { index });
        }
        if !right.is_finite() {
            return Err(StatisticsError::NonFinite { index });
        }
    }
    if pairs.len() < 5 {
        return Ok(PearsonConfidenceEstimate {
            interval: None,
            omission: Some(PearsonConfidenceIntervalOmission::InsufficientN),
        });
    }
    if pearson_pairs(pairs).is_none() {
        return Ok(PearsonConfidenceEstimate {
            interval: None,
            omission: Some(PearsonConfidenceIntervalOmission::ZeroVariance),
        });
    }

    let mut rng = SplitMix64::new(seed);
    let mut coefficients = Vec::with_capacity(BOOTSTRAP_RESAMPLES);
    let mut resampled = vec![(0.0, 0.0); pairs.len()];
    for _ in 0..BOOTSTRAP_RESAMPLES {
        for value in &mut resampled {
            *value = pairs[rng.index(pairs.len())];
        }
        if let Some(coefficient) = pearson_pairs(&resampled) {
            coefficients.push(coefficient);
        }
    }
    // A percentile interval with fewer than half of the requested resamples is
    // too sensitive to degenerate (zero-variance) bootstrap draws to publish.
    if coefficients.len() < BOOTSTRAP_RESAMPLES / 2 {
        return Ok(PearsonConfidenceEstimate {
            interval: None,
            omission: Some(PearsonConfidenceIntervalOmission::InsufficientValidResamples),
        });
    }
    coefficients.sort_by(f64::total_cmp);
    Ok(PearsonConfidenceEstimate {
        interval: Some(PearsonConfidenceInterval {
            level: 0.95,
            lower: quantile(&coefficients, 0.025),
            upper: quantile(&coefficients, 0.975),
            method: PearsonConfidenceMethod::PercentileBootstrapPearson,
            resamples: BOOTSTRAP_RESAMPLES,
            valid_resamples: coefficients.len(),
        }),
        omission: None,
    })
}

fn pearson_pairs(pairs: &[(f64, f64)]) -> Option<f64> {
    if pairs.len() < 2 {
        return None;
    }
    let count = pairs.len() as f64;
    let mean_left = pairs.iter().map(|(left, _)| left).sum::<f64>() / count;
    let mean_right = pairs.iter().map(|(_, right)| right).sum::<f64>() / count;
    let numerator = pairs
        .iter()
        .map(|(left, right)| (left - mean_left) * (right - mean_right))
        .sum::<f64>();
    let left_variance = pairs
        .iter()
        .map(|(left, _)| (left - mean_left).powi(2))
        .sum::<f64>();
    let right_variance = pairs
        .iter()
        .map(|(_, right)| (right - mean_right).powi(2))
        .sum::<f64>();
    let denominator = (left_variance * right_variance).sqrt();
    (denominator > 0.0).then_some(numerator / denominator)
}

fn bootstrap_median_interval(samples: &[f64], seed: u64) -> ConfidenceInterval {
    let mut rng = SplitMix64::new(seed);
    let mut resampled = vec![0.0; samples.len()];
    let mut medians = Vec::with_capacity(BOOTSTRAP_RESAMPLES);
    for _ in 0..BOOTSTRAP_RESAMPLES {
        resample(samples, &mut resampled, &mut rng);
        resampled.sort_by(f64::total_cmp);
        medians.push(quantile(&resampled, 0.5));
    }
    medians.sort_by(f64::total_cmp);
    ConfidenceInterval {
        level: 0.95,
        lower: quantile(&medians, 0.025),
        upper: quantile(&medians, 0.975),
        method: ConfidenceMethod::PercentileBootstrapMedian,
        resamples: BOOTSTRAP_RESAMPLES,
    }
}

fn histogram(sorted: &[f64], p25: f64, p75: f64) -> Histogram {
    let minimum = sorted[0];
    let maximum = sorted[sorted.len() - 1];
    if minimum == maximum {
        return Histogram {
            method: HistogramMethod::SingleValue,
            edges: vec![minimum, maximum],
            counts: vec![sorted.len()],
        };
    }

    let iqr = p75 - p25;
    let fd_width = 2.0 * iqr / (sorted.len() as f64).cbrt();
    let (method, bin_count) = if iqr > 0.0 && fd_width.is_finite() && fd_width > 0.0 {
        (
            HistogramMethod::FreedmanDiaconis,
            ((maximum - minimum) / fd_width).ceil().max(1.0) as usize,
        )
    } else {
        (
            HistogramMethod::Sturges,
            (sorted.len() as f64).log2().ceil() as usize + 1,
        )
    };
    let width = (maximum - minimum) / bin_count as f64;
    let edges = (0..=bin_count)
        .map(|index| {
            if index == bin_count {
                maximum
            } else {
                minimum + width * index as f64
            }
        })
        .collect::<Vec<_>>();
    let mut counts = vec![0; bin_count];
    for sample in sorted {
        let index = if *sample == maximum {
            bin_count - 1
        } else {
            ((*sample - minimum) / width).floor() as usize
        };
        counts[index.min(bin_count - 1)] += 1;
    }
    Histogram {
        method,
        edges,
        counts,
    }
}

fn validate_finite(samples: &[f64]) -> Result<(), StatisticsError> {
    match samples
        .iter()
        .enumerate()
        .find(|(_, sample)| !sample.is_finite())
    {
        Some((index, _)) => Err(StatisticsError::NonFinite { index }),
        None => Ok(()),
    }
}

fn resample(samples: &[f64], output: &mut [f64], rng: &mut SplitMix64) {
    for value in output {
        *value = samples[rng.index(samples.len())];
    }
}

fn quantile(sorted: &[f64], probability: f64) -> f64 {
    let position = (sorted.len() - 1) as f64 * probability;
    let lower = position.floor() as usize;
    let upper = position.ceil() as usize;
    if lower == upper {
        sorted[lower]
    } else {
        let fraction = position - lower as f64;
        sorted[lower] + (sorted[upper] - sorted[lower]) * fraction
    }
}

struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn index(&mut self, length: usize) -> usize {
        (self.next() % length as u64) as usize
    }

    fn next(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut value = self.state;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^ (value >> 31)
    }
}
