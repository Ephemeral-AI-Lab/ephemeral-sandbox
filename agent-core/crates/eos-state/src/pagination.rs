//! Pagination value objects for list-style `Store` queries.
//!
//! These back the read-side store APIs the backend composition root consumes
//! through `RuntimeServices::state_reader()` (the backend never opens a pool
//! against the agent-core DB). [`Page`] is the request window; [`PageResult`] is
//! one page plus the filter's unwindowed total; [`RequestListFilter`] narrows a
//! request listing.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::request::RequestStatus;

/// One page window: at most `limit` rows starting after `offset`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Page {
    /// Maximum number of rows to return.
    pub limit: u32,
    /// Number of rows to skip before collecting the page.
    pub offset: u32,
}

impl Default for Page {
    /// The first page of 50 rows.
    fn default() -> Self {
        Self {
            limit: 50,
            offset: 0,
        }
    }
}

/// One page of `T` plus the total rows matching the filter across all pages, so
/// callers can render pagination controls without a second query.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PageResult<T> {
    /// The rows in this page.
    pub items: Vec<T>,
    /// Total rows matching the filter, ignoring the page window.
    pub total: u64,
}

/// Filter for [`RequestStore::list`](crate::RequestStore::list).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RequestListFilter {
    /// When set, restrict the listing to requests in this lifecycle status.
    pub status: Option<RequestStatus>,
}
