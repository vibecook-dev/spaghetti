//! RFC 012B catalog: catalog-first startup and one readiness vector.
//!
//! # What a catalog row means
//!
//! Catalog membership is a first-class fact, distinct from decoded history.
//! Four states, each strictly stronger than the last:
//!
//! | state              | meaning                                             |
//! | ------------------ | --------------------------------------------------- |
//! | `discovered`       | native evidence says this exists                    |
//! | `transcript_backed`| a transcript object exists and has a canonical row  |
//! | `hydrated`         | its messages are decoded                            |
//! | `searchable`       | it is in the full-text index                        |
//!
//! Discovery writes the *evidence*; the state is derived at read time from
//! committed RFC 011 rows inside one snapshot, so the two authorities cannot
//! drift and no second write path exists.
//!
//! # Startup
//!
//! On engine open each configured source runs one bounded discovery pass
//! through its `AgentAdapter` and commits its rows in a single transaction.
//! Catalog readiness is published at that point — before history, usage,
//! artifacts, or FTS converge. A warm start serves the last committed rows
//! immediately (SQLite hands the reader a consistent snapshot) and reconciles
//! in the background by size and modification time.

/// Whether full-text structures are finalized, as a SQL scalar subquery.
///
/// The marker is durable — `schema_meta.query_bootstrap_state` exists only
/// while finalization is incomplete — so every surface reads the same row
/// instead of being handed an engine flag. One definition is what keeps the
/// history page and the catalog page from disagreeing about one session.
macro_rules! search_ready_sql {
    () => {
        "(SELECT COUNT(*) = 0 FROM schema_meta WHERE key = 'query_bootstrap_state')"
    };
}

mod discovery;
mod query;
mod readiness;
mod store;
#[cfg(test)]
mod tests;

use std::fmt;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

pub(crate) use discovery::{scan_source, SourceScan};
pub use query::{
    encode_external_ref, read_project_page, read_session_page, resolve_catalog_entity,
    CatalogEntityResolution, CatalogProjectPage, CatalogProjectPageRequest, CatalogProjectRow,
    CatalogSessionPage, CatalogSessionPageRequest, CatalogSessionRow, IdentityConflict,
    HISTORY_PROJECT_CATALOG_COLUMNS, HISTORY_PROJECT_CATALOG_CTE, HISTORY_PROJECT_CATALOG_JOINS,
    HISTORY_SESSION_CATALOG_COLUMNS, HISTORY_SESSION_CATALOG_JOIN,
};
pub use readiness::{read_readiness, Readiness, ReadinessField, ReadinessState};
pub(crate) use store::{commit_source_scan, CatalogScanReceipt};

/// Largest page a catalog query will return.
pub const MAX_CATALOG_PAGE_LIMIT: u32 = 500;
/// Page size used when a caller does not ask for one.
pub const DEFAULT_CATALOG_PAGE_LIMIT: u32 = 100;

/// How much of an entity is available, from bare discoverability up to
/// full-text searchable. Ordering is meaningful: a later variant implies
/// every earlier one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum CatalogState {
    /// Native evidence proves the entity exists. Nothing is decoded.
    Discovered,
    /// A transcript object exists and durable ingestion has a canonical row.
    TranscriptBacked,
    /// Messages are decoded and readable.
    Hydrated,
    /// Present in the full-text index.
    Searchable,
}

impl CatalogState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Discovered => "discovered",
            Self::TranscriptBacked => "transcript_backed",
            Self::Hydrated => "hydrated",
            Self::Searchable => "searchable",
        }
    }
}

impl fmt::Display for CatalogState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Bounded page request shared by the project and session queries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogPageBounds {
    /// Opaque continuation token from a previous page. A cursor is bound to
    /// the commit watermark it was minted at, so a page never mixes snapshots.
    pub cursor: Option<String>,
    pub limit: u32,
}

impl Default for CatalogPageBounds {
    fn default() -> Self {
        Self {
            cursor: None,
            limit: DEFAULT_CATALOG_PAGE_LIMIT,
        }
    }
}
