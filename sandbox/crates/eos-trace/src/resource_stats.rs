use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::budget::{BoundedJson, DetailBudget};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceStatsKind {
    CgroupProcess,
    Tree,
    Host,
    MountCost,
}

impl ResourceStatsKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CgroupProcess => "cgroup_process",
            Self::Tree => "tree",
            Self::Host => "host",
            Self::MountCost => "mount_cost",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceStatsMeta {
    pub stats_kind: ResourceStatsKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    pub source: String,
    pub source_available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parse_error: Option<String>,
    pub sampler_duration_us: u64,
    pub inflight_requests: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResourceStats {
    pub meta: ResourceStatsMeta,
    pub payload: BoundedJson,
}

impl ResourceStats {
    #[must_use]
    pub fn available(
        stats_kind: ResourceStatsKind,
        phase: Option<String>,
        source: impl Into<String>,
        sampler_duration_us: u64,
        inflight_requests: u64,
        payload: Value,
    ) -> Self {
        Self {
            meta: ResourceStatsMeta {
                stats_kind,
                phase,
                source: source.into(),
                source_available: true,
                read_error: None,
                parse_error: None,
                sampler_duration_us,
                inflight_requests,
            },
            payload: BoundedJson::capture(payload, DetailBudget::EventDetails),
        }
    }

    #[must_use]
    pub fn unavailable(
        stats_kind: ResourceStatsKind,
        phase: Option<String>,
        source: impl Into<String>,
        read_error: Option<String>,
        parse_error: Option<String>,
        sampler_duration_us: u64,
        inflight_requests: u64,
    ) -> Self {
        Self {
            meta: ResourceStatsMeta {
                stats_kind,
                phase,
                source: source.into(),
                source_available: false,
                read_error,
                parse_error,
                sampler_duration_us,
                inflight_requests,
            },
            payload: BoundedJson::empty_object(),
        }
    }
}
