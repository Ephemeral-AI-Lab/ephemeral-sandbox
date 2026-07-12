use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::definitions::definition;
use crate::model::{CheckId, OperationId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckVerdict {
    Pass,
    Fail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckDefinition {
    pub id: CheckId,
    pub semantic_revision: u32,
    pub evidence_limit: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckEvidenceItem {
    pub expected: String,
    pub actual: String,
    pub artifact_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoundedCheckEvidence {
    pub items: Vec<CheckEvidenceItem>,
    pub truncated_count: u64,
    pub truncated_sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckResult {
    pub id: CheckId,
    pub semantic_revision: u32,
    pub operation_id: OperationId,
    pub cell_id: String,
    pub trial_id: String,
    pub request_id: Option<String>,
    pub verdict: CheckVerdict,
    pub duration_ns: u64,
    pub evidence: BoundedCheckEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CorrectnessFold {
    pub product_succeeded: bool,
    pub required_check_count: u64,
    pub attempted_check_count: u64,
    pub passed_check_count: u64,
    pub failed_check_count: u64,
    pub missing_checks: Vec<CheckId>,
    pub unexpected_checks: Vec<CheckId>,
    pub eligible_for_latency: bool,
}

#[must_use]
pub const fn check_definition(id: CheckId) -> CheckDefinition {
    match id {
        CheckId::CommandExitStatus => check(id, 8),
        CheckId::CommandOutput => check(id, 8),
        CheckId::CommandLifecycle => check(id, 8),
        CheckId::FileReadWindow => check(id, 8),
        CheckId::FileContentHash => check(id, 8),
        CheckId::MutationAttribution => check(id, 8),
        CheckId::FileEditReplacementCount => check(id, 8),
        CheckId::BlameRangeCoverage => check(id, 8),
        CheckId::BlameOwnership => check(id, 8),
        CheckId::WorkspaceReady => check(id, 16),
        CheckId::WorkspaceNetworkProfile => check(id, 16),
        CheckId::WorkspaceRegistryBaseline => check(id, 16),
        CheckId::LayerstackContentEquivalence => check(id, 32),
        CheckId::LayerstackManifestReduction => check(id, 32),
        CheckId::LayerstackDispositionAccounting => check(id, 32),
        CheckId::LayerstackSessionUsability => check(id, 32),
        CheckId::LayerstackResidue => check(id, 32),
    }
}

#[must_use]
pub fn bounded_evidence(id: CheckId, mut items: Vec<CheckEvidenceItem>) -> BoundedCheckEvidence {
    let limit = check_definition(id).evidence_limit;
    if items.len() <= limit {
        return BoundedCheckEvidence {
            items,
            truncated_count: 0,
            truncated_sha256: None,
        };
    }
    let omitted = items.split_off(limit);
    let truncated_count = u64::try_from(omitted.len()).unwrap_or(u64::MAX);
    let bytes = serde_json::to_vec(&omitted).unwrap_or_default();
    BoundedCheckEvidence {
        items,
        truncated_count,
        truncated_sha256: Some(format!("sha256:{:x}", Sha256::digest(bytes))),
    }
}

#[must_use]
pub fn fold_correctness(
    operation: OperationId,
    product_succeeded: bool,
    cleanup_baseline_restored: bool,
    results: &[CheckResult],
) -> CorrectnessFold {
    let required = definition(operation)
        .checks
        .iter()
        .map(|reference| reference.id)
        .collect::<BTreeSet<_>>();
    let mut seen = BTreeMap::<CheckId, CheckVerdict>::new();
    let mut unexpected = BTreeSet::new();
    for result in results {
        if result.operation_id != operation || !required.contains(&result.id) {
            unexpected.insert(result.id);
            continue;
        }
        seen.entry(result.id)
            .and_modify(|verdict| {
                if result.verdict == CheckVerdict::Fail {
                    *verdict = CheckVerdict::Fail;
                }
            })
            .or_insert(result.verdict);
    }
    let missing_checks = required
        .difference(&seen.keys().copied().collect())
        .copied()
        .collect::<Vec<_>>();
    let passed_check_count = seen
        .values()
        .filter(|verdict| **verdict == CheckVerdict::Pass)
        .count();
    let failed_check_count = seen.len().saturating_sub(passed_check_count);
    let eligible_for_latency = product_succeeded
        && cleanup_baseline_restored
        && missing_checks.is_empty()
        && unexpected.is_empty()
        && failed_check_count == 0;
    CorrectnessFold {
        product_succeeded,
        required_check_count: u64::try_from(required.len()).unwrap_or(u64::MAX),
        attempted_check_count: u64::try_from(seen.len()).unwrap_or(u64::MAX),
        passed_check_count: u64::try_from(passed_check_count).unwrap_or(u64::MAX),
        failed_check_count: u64::try_from(failed_check_count).unwrap_or(u64::MAX),
        missing_checks,
        unexpected_checks: unexpected.into_iter().collect(),
        eligible_for_latency,
    }
}

const fn check(id: CheckId, evidence_limit: usize) -> CheckDefinition {
    CheckDefinition {
        id,
        semantic_revision: 1,
        evidence_limit,
    }
}
