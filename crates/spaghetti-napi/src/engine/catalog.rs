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
/// Whether one catalog session has any decoded message, as a SQL predicate
/// correlated on `cs.session_key`.
///
/// Written as `EXISTS` rather than a counted join on purpose: presence needs
/// one index seek per catalog row, while `COUNT(*) ... GROUP BY session_key`
/// aggregates every message in the store. On a mid-rebuild corpus (243k
/// messages) that difference measured 1.7 s against 1 ms, and it grows with
/// the message table — the readiness vector is polled while that table is
/// still filling.
macro_rules! session_hydrated_sql {
    () => {
        "EXISTS (SELECT 1 FROM canonical_messages cm WHERE cm.session_key = cs.session_key)"
    };
}

macro_rules! search_ready_sql {
    () => {
        "(SELECT COUNT(*) = 0 FROM schema_meta WHERE key = 'query_bootstrap_state')"
    };
}

/// The hydration predicate for one catalog session row, chosen by whether the
/// deferred query structures exist yet.
///
/// The `EXISTS` probe is one index seek — but the index it seeks
/// (`idx_canonical_messages_session_activity`) is a deferred structure that
/// does not exist while `query_bootstrap_state` is set, and without it the
/// probe scans the whole message store per catalog row: measured 16.6 s per
/// probe on a 1.2 M-row mid-rebuild corpus, which turns one catalog page into
/// hours. While structures are deferred, hydration reports false — the session
/// stays `transcript_backed` ("processing") until finalization restores the
/// seek, which is the degraded state RFC 012B prescribes for an index that is
/// still converging.
pub(crate) fn session_hydrated_predicate(structures_deferred: bool) -> &'static str {
    if structures_deferred {
        "0"
    } else {
        session_hydrated_sql!()
    }
}

/// Whether the deferred query structures (FTS + the bootstrap query indexes)
/// are still absent. Reads the durable marker inside the caller's transaction
/// so the answer is consistent with every other row the query sees.
pub(crate) fn query_structures_deferred(
    connection: &rusqlite::Connection,
) -> Result<bool, rusqlite::Error> {
    connection.query_row(
        "SELECT COUNT(*) > 0 FROM schema_meta WHERE key = 'query_bootstrap_state'",
        [],
        |row| row.get(0),
    )
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
    encode_external_ref, history_project_catalog_cte, read_project_page, read_session_page,
    resolve_catalog_entity, CatalogEntityResolution, CatalogProjectPage, CatalogProjectPageRequest,
    CatalogProjectRow, CatalogSessionPage, CatalogSessionPageRequest, CatalogSessionRow,
    IdentityConflict, HISTORY_PROJECT_CATALOG_COLUMNS, HISTORY_PROJECT_CATALOG_JOINS,
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
