//! Checked crate-private RFC 012B retained-snapshot page reads.
//!
//! This module intentionally exposes no public engine or N-API surface. The
//! first executable slice supports only the frozen v1 `All` filter ordered by
//! ascending external entity key and always projects policy-sensitive values
//! as withheld.

use std::collections::BTreeMap;

use rusqlite::{params, Connection};

use crate::catalog_contract::evidence::{decode_durable_project_row, decode_durable_session_row};
use crate::catalog_contract::page::{
    CatalogCount, CatalogPageEntry, CatalogPageRequestBinding, CatalogPolicyViewBinding,
    CatalogPortableProjectRow, CatalogPortableRow, CatalogPortableSessionRow, CatalogProjectPage,
    CatalogSessionPage,
};
use crate::catalog_contract::publication::{
    CatalogDurablePublicationEntryKind, MAX_DURABLE_CATALOG_ROW_BYTES,
};
use crate::catalog_contract::query::{CatalogContinuationRequest, CatalogQueryContractSelection};
use crate::catalog_contract::{
    CatalogContractError, CatalogCoverageScope, CatalogCursor, CatalogQueryFingerprint,
    CatalogQueryKind, CatalogSortKey,
};

use super::catalog_publication::{
    load_retained_query_header, CatalogReadyPublicationIdentity, CatalogRetainedQueryHeader,
};
use super::catalog_state::{self, CatalogReadyReadAuthority};
use super::EngineError;

const CATALOG_ENTITY_KEY_SORT_SPEC_VERSION: u32 = 1;
const CATALOG_ALL_FILTER_V1: &[u8] = b"rfc012b/catalog-filter/all-v1";
const MAX_RETAINED_PAGE_PAYLOAD_BYTES: usize = 64 * 1024 * 1024;

// Keep these as two simple PK ranges. Tests ratchet the planner and prohibit
// OFFSET/temp sorting; do not merge them behind an OR predicate.
const FIRST_PAGE_SQL: &str = r#"
    SELECT CASE WHEN typeof(entry_key) = 'blob' AND length(entry_key) = 32
                       THEN entry_key END,
           CASE WHEN typeof(payload) = 'blob' AND length(payload) BETWEEN 1 AND ?3
                       THEN length(payload) END,
           CASE WHEN typeof(payload) = 'blob' AND length(payload) BETWEEN 1 AND ?3
                       THEN payload END,
           CASE WHEN typeof(payload_digest) = 'blob' AND length(payload_digest) = 32
                       THEN payload_digest END
    FROM catalog_snapshot_entries
    WHERE snapshot_commit_seq = ?1 AND entry_kind = ?2
    ORDER BY entry_key ASC
    LIMIT ?4
"#;

const CONTINUATION_PAGE_SQL: &str = r#"
    SELECT CASE WHEN typeof(entry_key) = 'blob' AND length(entry_key) = 32
                       THEN entry_key END,
           CASE WHEN typeof(payload) = 'blob' AND length(payload) BETWEEN 1 AND ?4
                       THEN length(payload) END,
           CASE WHEN typeof(payload) = 'blob' AND length(payload) BETWEEN 1 AND ?4
                       THEN payload END,
           CASE WHEN typeof(payload_digest) = 'blob' AND length(payload_digest) = 32
                       THEN payload_digest END
    FROM catalog_snapshot_entries
    WHERE snapshot_commit_seq = ?1 AND entry_kind = ?2 AND entry_key > ?3
    ORDER BY entry_key ASC
    LIMIT ?5
"#;

#[derive(Clone, PartialEq, Eq)]
pub(super) struct CatalogRetainedPageRequest {
    contract_selection: CatalogQueryContractSelection,
    snapshot_id: crate::catalog_contract::CatalogSnapshotId,
    query_kind: CatalogQueryKind,
    page_size: u32,
    continuation: Option<CatalogContinuationRequest>,
}

impl CatalogRetainedPageRequest {
    pub(super) fn projects_all(
        contract_selection: CatalogQueryContractSelection,
        snapshot_id: crate::catalog_contract::CatalogSnapshotId,
        page_size: u32,
        continuation: Option<CatalogContinuationRequest>,
    ) -> Self {
        Self {
            contract_selection,
            snapshot_id,
            query_kind: CatalogQueryKind::Projects,
            page_size,
            continuation,
        }
    }

    pub(super) fn sessions_all(
        contract_selection: CatalogQueryContractSelection,
        snapshot_id: crate::catalog_contract::CatalogSnapshotId,
        page_size: u32,
        continuation: Option<CatalogContinuationRequest>,
    ) -> Self {
        Self {
            contract_selection,
            snapshot_id,
            query_kind: CatalogQueryKind::Sessions,
            page_size,
            continuation,
        }
    }

    fn fingerprint(&self) -> Result<CatalogQueryFingerprint, CatalogContractError> {
        CatalogQueryFingerprint::derive(
            self.snapshot_id.pack_contract_version,
            self.query_kind,
            CatalogCoverageScope::Library,
            CATALOG_ENTITY_KEY_SORT_SPEC_VERSION,
            CATALOG_ALL_FILTER_V1,
        )
    }

    fn validate_and_bind(
        &self,
        authority: &CatalogReadyReadAuthority,
        expected_selection: &crate::adapter::ContractVersionSelection,
    ) -> Result<CatalogPageRequestBinding, EngineError> {
        if self.snapshot_id != authority.snapshot_id() {
            return Err(EngineError::InvalidCommit(
                "catalog retained-page request is bound to a different Ready snapshot".to_string(),
            ));
        }
        if &self.contract_selection.contract_versions != expected_selection {
            return Err(EngineError::InvalidCommit(
                "catalog retained-page selection differs from the exact durable selection"
                    .to_string(),
            ));
        }
        let fingerprint = self
            .fingerprint()
            .map_err(catalog_state::catalog_contract_error)?;
        let after_cursor = if let Some(continuation) = &self.continuation {
            // Reparse through the only checked continuation-consumption path so
            // public fields cannot bypass contract-version/selection validation.
            let canonical = CatalogContinuationRequest::from_wire_value(
                serde_json::to_value(continuation)
                    .map_err(catalog_state::catalog_contract_error)?,
                &self.contract_selection,
            )
            .map_err(catalog_state::catalog_contract_error)?;
            if canonical.snapshot_id != self.snapshot_id
                || canonical.query_fingerprint != fingerprint
                || canonical.sort_spec_version != CATALOG_ENTITY_KEY_SORT_SPEC_VERSION
                || canonical.page_size != self.page_size
            {
                return Err(EngineError::InvalidCommit(
                    "catalog continuation differs from the exact retained All-page request"
                        .to_string(),
                ));
            }
            let expected_sort_key =
                CatalogSortKey::new(canonical.cursor.last_entity_key.as_bytes().to_vec())
                    .map_err(catalog_state::catalog_contract_error)?;
            if canonical.cursor.last_sort_key != expected_sort_key {
                return Err(EngineError::InvalidCommit(
                    "catalog entity-key continuation carries a non-semantic sort key".to_string(),
                ));
            }
            Some(canonical.cursor)
        } else {
            None
        };
        CatalogPageRequestBinding::new(
            self.contract_selection.clone(),
            self.snapshot_id,
            self.query_kind,
            fingerprint,
            CATALOG_ENTITY_KEY_SORT_SPEC_VERSION,
            self.page_size,
            after_cursor,
        )
        .map_err(catalog_state::catalog_contract_error)
    }
}

pub(super) enum CatalogRetainedPage {
    Projects(CatalogProjectPage),
    Sessions(CatalogSessionPage),
}

pub(super) fn read_retained_catalog_page(
    connection: &Connection,
    authority: &CatalogReadyReadAuthority,
    request: &CatalogRetainedPageRequest,
) -> Result<CatalogRetainedPage, EngineError> {
    // Cursor/request validation uses only the caller-held restart authority and
    // therefore precedes any not-retained database decision.
    let binding = request.validate_and_bind(authority, authority.contract_selection())?;
    let transaction = connection
        .unchecked_transaction()
        .map_err(|error| catalog_state::sqlite_error("begin retained catalog page", error))?;
    let header = load_retained_query_header(
        &transaction,
        authority.plan(),
        authority.snapshot_id(),
        authority.readiness().attempt,
        authority.publication_identity(),
    )?;
    debug_assert_eq!(
        request.contract_selection.contract_versions,
        header.contract_selection
    );
    let policy = CatalogPolicyViewBinding::withheld(authority.plan(), &request.contract_selection)
        .map_err(catalog_state::catalog_contract_error)?;

    let result = match request.query_kind {
        CatalogQueryKind::Projects => {
            let (rows, has_more) = load_page_rows(
                &transaction,
                &header,
                &binding,
                authority.publication_identity(),
                CatalogDurablePublicationEntryKind::ProjectRow,
                |payload, key| {
                    let row =
                        decode_durable_project_row(payload, key, MAX_DURABLE_CATALOG_ROW_BYTES)?;
                    CatalogPortableProjectRow::from_bound_evidence(
                        &row,
                        &policy,
                        authority.plan(),
                        &request.contract_selection,
                    )
                },
            )?;
            let next = next_continuation(&binding, &rows, has_more)?;
            CatalogProjectPage::new_projects(
                binding,
                authority.readiness().clone(),
                CatalogCount::known(header.project_row_count as u64)
                    .map_err(catalog_state::catalog_contract_error)?,
                has_more,
                rows,
                next,
                BTreeMap::new(),
                authority.plan(),
            )
            .map(CatalogRetainedPage::Projects)
            .map_err(catalog_state::catalog_contract_error)?
        }
        CatalogQueryKind::Sessions => {
            let (rows, has_more) = load_page_rows(
                &transaction,
                &header,
                &binding,
                authority.publication_identity(),
                CatalogDurablePublicationEntryKind::SessionRow,
                |payload, key| {
                    let row =
                        decode_durable_session_row(payload, key, MAX_DURABLE_CATALOG_ROW_BYTES)?;
                    CatalogPortableSessionRow::from_bound_evidence(
                        &row,
                        &policy,
                        authority.plan(),
                        &request.contract_selection,
                    )
                },
            )?;
            let next = next_continuation(&binding, &rows, has_more)?;
            CatalogSessionPage::new_sessions(
                binding,
                authority.readiness().clone(),
                CatalogCount::known(header.session_row_count as u64)
                    .map_err(catalog_state::catalog_contract_error)?,
                has_more,
                rows,
                next,
                BTreeMap::new(),
                authority.plan(),
            )
            .map(CatalogRetainedPage::Sessions)
            .map_err(catalog_state::catalog_contract_error)?
        }
    };
    transaction
        .commit()
        .map_err(|error| catalog_state::sqlite_error("commit retained catalog page", error))?;
    Ok(result)
}

fn load_page_rows<R: CatalogPortableRow>(
    connection: &Connection,
    header: &CatalogRetainedQueryHeader,
    binding: &CatalogPageRequestBinding,
    expected_identity: &CatalogReadyPublicationIdentity,
    entry_kind: CatalogDurablePublicationEntryKind,
    decode: impl Fn(&[u8], &[u8; 32]) -> Result<R, CatalogContractError>,
) -> Result<(Vec<CatalogPageEntry<R>>, bool), EngineError> {
    let scan_limit = binding.page_size.checked_add(1).ok_or_else(|| {
        catalog_state::corrupt_catalog_state("catalog retained-page scan limit overflow")
    })?;
    let expected_keys = expected_identity
        .expected_row_keys(
            entry_kind,
            binding
                .after_cursor
                .as_ref()
                .map(|cursor| cursor.last_entity_key.as_bytes()),
            scan_limit as usize,
        )
        .ok_or_else(|| {
            EngineError::InvalidCommit(
                "catalog continuation does not name a row in the restart-validated publication"
                    .to_string(),
            )
        })?;
    let payload_limit = header
        .encoded_bytes
        .min(MAX_DURABLE_CATALOG_ROW_BYTES)
        .min(MAX_RETAINED_PAGE_PAYLOAD_BYTES);
    let snapshot_commit = catalog_state::to_i64(
        binding.snapshot_id.complete_commit,
        "catalog retained snapshot commit",
    )?;
    let mut statement = connection
        .prepare(if binding.after_cursor.is_some() {
            CONTINUATION_PAGE_SQL
        } else {
            FIRST_PAGE_SQL
        })
        .map_err(|error| catalog_state::sqlite_error("prepare retained catalog range", error))?;
    let mut sql_rows = if let Some(cursor) = &binding.after_cursor {
        statement
            .query(params![
                snapshot_commit,
                entry_kind.as_str(),
                cursor.last_entity_key.as_bytes().as_slice(),
                catalog_state::to_i64(payload_limit as u64, "catalog retained row byte limit")?,
                i64::from(scan_limit),
            ])
            .map_err(|error| catalog_state::sqlite_error("query retained catalog range", error))?
    } else {
        statement
            .query(params![
                snapshot_commit,
                entry_kind.as_str(),
                catalog_state::to_i64(payload_limit as u64, "catalog retained row byte limit")?,
                i64::from(scan_limit),
            ])
            .map_err(|error| catalog_state::sqlite_error("query retained catalog range", error))?
    };

    let mut rows = Vec::with_capacity(binding.page_size as usize);
    let mut retained_payload_bytes = 0_usize;
    let mut has_more = false;
    let mut stopped_for_payload_budget = false;
    let mut scanned_keys = Vec::with_capacity(scan_limit as usize);
    while let Some(row) = sql_rows
        .next()
        .map_err(|error| catalog_state::sqlite_error("read retained catalog row", error))?
    {
        let key = decode_nonzero_digest(
            row.get::<_, Option<Vec<u8>>>(0)
                .map_err(|error| {
                    catalog_state::sqlite_error("decode retained catalog row key", error)
                })?
                .as_deref()
                .ok_or_else(|| {
                    catalog_state::corrupt_catalog_state(
                        "retained catalog row key is outside its fixed bound",
                    )
                })?,
            "retained catalog row key",
        )?;
        if expected_keys.get(scanned_keys.len()) != Some(&key) {
            return Err(catalog_state::corrupt_catalog_state(
                "retained catalog PK range differs from its restart-validated row sequence",
            ));
        }
        scanned_keys.push(key);
        if rows.len() == binding.page_size as usize {
            has_more = true;
            break;
        }
        let payload_length = row
            .get::<_, Option<i64>>(1)
            .map_err(|error| {
                catalog_state::sqlite_error("decode retained catalog row payload length", error)
            })?
            .ok_or_else(|| {
                catalog_state::corrupt_catalog_state(
                    "retained catalog row payload is outside its preflight byte bound",
                )
            })?;
        let payload_length = usize::try_from(payload_length).map_err(|_| {
            catalog_state::corrupt_catalog_state(
                "retained catalog row payload length is negative or too large",
            )
        })?;
        let remaining_page_bytes = MAX_RETAINED_PAGE_PAYLOAD_BYTES
            .checked_sub(retained_payload_bytes)
            .ok_or_else(|| {
                catalog_state::corrupt_catalog_state("catalog page payload byte count overflow")
            })?;
        if payload_length > remaining_page_bytes {
            has_more = true;
            stopped_for_payload_budget = true;
            break;
        }
        let payload = row
            .get::<_, Option<Vec<u8>>>(2)
            .map_err(|error| {
                catalog_state::sqlite_error("decode retained catalog row payload", error)
            })?
            .ok_or_else(|| {
                catalog_state::corrupt_catalog_state(
                    "retained catalog row payload is outside its preflight byte bound",
                )
            })?;
        if payload.len() != payload_length {
            return Err(catalog_state::corrupt_catalog_state(
                "retained catalog row payload changed after its bounded length projection",
            ));
        }
        let payload_digest = decode_nonzero_digest(
            row.get::<_, Option<Vec<u8>>>(3)
                .map_err(|error| {
                    catalog_state::sqlite_error("decode retained catalog row digest", error)
                })?
                .as_deref()
                .ok_or_else(|| {
                    catalog_state::corrupt_catalog_state(
                        "retained catalog row digest is outside its fixed bound",
                    )
                })?,
            "retained catalog row digest",
        )?;
        if blake3::hash(&payload).as_bytes() != &payload_digest {
            return Err(catalog_state::corrupt_catalog_state(
                "retained catalog row payload digest does not match its bytes",
            ));
        }
        if !expected_identity.matches_row(entry_kind, &key, payload.len(), &payload_digest) {
            return Err(catalog_state::corrupt_catalog_state(
                "retained catalog row differs from its restart-validated commitment",
            ));
        }
        let next_payload_bytes = retained_payload_bytes
            .checked_add(payload.len())
            .ok_or_else(|| {
                catalog_state::corrupt_catalog_state("catalog page payload byte count overflow")
            })?;
        debug_assert!(next_payload_bytes <= MAX_RETAINED_PAGE_PAYLOAD_BYTES);
        let decoded = decode(&payload, &key).map_err(catalog_state::catalog_contract_error)?;
        let entity_key = decoded.entity_ref().external_ref.entity_key;
        let sort_key = CatalogSortKey::new(entity_key.as_bytes().to_vec())
            .map_err(catalog_state::catalog_contract_error)?;
        rows.push(
            CatalogPageEntry::new(sort_key, decoded)
                .map_err(catalog_state::catalog_contract_error)?,
        );
        retained_payload_bytes = next_payload_bytes;
    }
    if (!stopped_for_payload_budget && scanned_keys != expected_keys)
        || (stopped_for_payload_budget
            && scanned_keys.as_slice() != &expected_keys[..scanned_keys.len()])
        || (!stopped_for_payload_budget
            && has_more != (expected_keys.len() > binding.page_size as usize))
    {
        return Err(catalog_state::corrupt_catalog_state(
            "retained catalog PK range is incomplete for its restart-validated publication",
        ));
    }
    Ok((rows, has_more))
}

fn next_continuation<R: CatalogPortableRow>(
    binding: &CatalogPageRequestBinding,
    rows: &[CatalogPageEntry<R>],
    has_more: bool,
) -> Result<Option<CatalogContinuationRequest>, EngineError> {
    if !has_more {
        return Ok(None);
    }
    let last = rows.last().ok_or_else(|| {
        catalog_state::corrupt_catalog_state(
            "a bounded catalog page cannot continue before retaining one row",
        )
    })?;
    let cursor = CatalogCursor::new(
        binding.snapshot_id,
        binding.query_fingerprint,
        binding.sort_spec_version,
        last.sort_key.clone(),
        last.row.entity_ref().external_ref.entity_key,
    )
    .map_err(catalog_state::catalog_contract_error)?;
    CatalogContinuationRequest::new(
        binding.contract_selection.clone(),
        binding.snapshot_id,
        binding.query_fingerprint,
        binding.sort_spec_version,
        cursor,
        binding.page_size,
    )
    .map(Some)
    .map_err(catalog_state::catalog_contract_error)
}

fn decode_nonzero_digest(bytes: &[u8], label: &'static str) -> Result<[u8; 32], EngineError> {
    let digest: [u8; 32] = bytes.try_into().map_err(|_| {
        catalog_state::corrupt_catalog_state(format!("{label} is not exactly 32 bytes"))
    })?;
    if digest.iter().all(|byte| *byte == 0) {
        return Err(catalog_state::corrupt_catalog_state(format!(
            "{label} must be nonzero"
        )));
    }
    Ok(digest)
}

#[cfg(test)]
mod tests;
