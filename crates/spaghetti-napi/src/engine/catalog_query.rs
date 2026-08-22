//! Checked crate-private RFC 012B retained-snapshot page and external-reference
//! reads.
//!
//! The engine-facing requests negotiate against restart-authenticated durable
//! authority on a persistent read worker. This module intentionally exposes no
//! N-API surface. The first executable slice supports only the frozen v1 `All`
//! filter ordered by ascending external entity key and always projects
//! policy-sensitive values as withheld.

use std::collections::BTreeMap;

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::Value as JsonValue;

use crate::adapter::ExternalEntityRef;
use crate::catalog_contract::evidence::{
    decode_durable_project_row, decode_durable_session_row, CatalogEntityKind, CatalogLiveRow,
    CatalogResolvedLifecycle,
};
use crate::catalog_contract::page::{
    validate_continuation_retention, CatalogContinuationDisposition, CatalogCount,
    CatalogEntityResolution, CatalogEntityResolutionResponse, CatalogPageEntry,
    CatalogPageRequestBinding, CatalogPolicyViewBinding, CatalogPortableCoveragePlan,
    CatalogPortableProjectRow, CatalogPortableRow, CatalogPortableSessionRow, CatalogProjectPage,
    CatalogReadinessResponse, CatalogResolutionRequestBinding, CatalogSessionPage,
    CatalogSnapshotExpired, CatalogSnapshotRetention,
};
use crate::catalog_contract::publication::{
    CatalogDurablePublicationEntryKind, MAX_DURABLE_CATALOG_ROW_BYTES,
};
use crate::catalog_contract::query::{
    negotiate_catalog_query_contract_for_selection, CatalogContinuationRequest,
    CatalogQueryContractRequest, CatalogQueryContractSelection, CatalogQueryNegotiationError,
    MAX_CONTINUATION_PAGE_SIZE,
};
use crate::catalog_contract::{
    CatalogContractError, CatalogCoveragePlanId, CatalogCoverageScope, CatalogCursor,
    CatalogQueryFingerprint, CatalogQueryKind, CatalogSnapshotId, CatalogSortKey,
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

const EXACT_RESOLUTION_ROW_SQL: &str = r#"
    SELECT CASE WHEN typeof(payload) = 'blob' AND length(payload) BETWEEN 1 AND ?4
                       THEN length(payload) END,
           CASE WHEN typeof(payload) = 'blob' AND length(payload) BETWEEN 1 AND ?4
                       THEN payload END,
           CASE WHEN typeof(payload_digest) = 'blob' AND length(payload_digest) = 32
                       THEN payload_digest END
    FROM catalog_snapshot_entries
    WHERE snapshot_commit_seq = ?1 AND entry_kind = ?2 AND entry_key = ?3
"#;

#[derive(Clone)]
pub(crate) struct CatalogPageQueryRequest {
    contract_request: CatalogQueryContractRequest,
    expected_coverage_plan_id: CatalogCoveragePlanId,
    expected_snapshot_id: CatalogSnapshotId,
    query_kind: CatalogQueryKind,
    page_size: u32,
    continuation: Option<JsonValue>,
}

impl CatalogPageQueryRequest {
    pub(crate) fn new(
        contract_request: CatalogQueryContractRequest,
        expected_coverage_plan_id: CatalogCoveragePlanId,
        expected_snapshot_id: CatalogSnapshotId,
        query_kind: CatalogQueryKind,
        page_size: u32,
        continuation: Option<CatalogContinuationRequest>,
    ) -> Result<Self, EngineError> {
        let continuation = continuation
            .map(|continuation| {
                serde_json::to_value(continuation)
                    .map_err(|_| invalid_catalog_query_input("continuation is not portable"))
            })
            .transpose()?;
        Self::from_wire(
            contract_request,
            expected_coverage_plan_id,
            expected_snapshot_id,
            query_kind,
            page_size,
            continuation,
        )
    }

    pub(crate) fn from_wire(
        contract_request: CatalogQueryContractRequest,
        expected_coverage_plan_id: CatalogCoveragePlanId,
        expected_snapshot_id: CatalogSnapshotId,
        query_kind: CatalogQueryKind,
        page_size: u32,
        continuation: Option<JsonValue>,
    ) -> Result<Self, EngineError> {
        if page_size == 0 || page_size > MAX_CONTINUATION_PAGE_SIZE {
            return Err(invalid_catalog_query_input(
                "page size is outside the supported bound",
            ));
        }
        Ok(Self {
            contract_request,
            expected_coverage_plan_id,
            expected_snapshot_id,
            query_kind,
            page_size,
            continuation,
        })
    }
}

#[derive(Clone)]
pub(crate) struct CatalogResolutionQueryRequest {
    contract_request: CatalogQueryContractRequest,
    expected_coverage_plan_id: CatalogCoveragePlanId,
    expected_snapshot_id: CatalogSnapshotId,
    external_ref: ExternalEntityRef,
}

impl CatalogResolutionQueryRequest {
    pub(crate) fn new(
        contract_request: CatalogQueryContractRequest,
        expected_coverage_plan_id: CatalogCoveragePlanId,
        expected_snapshot_id: CatalogSnapshotId,
        external_ref: ExternalEntityRef,
    ) -> Self {
        Self {
            contract_request,
            expected_coverage_plan_id,
            expected_snapshot_id,
            external_ref,
        }
    }
}

#[derive(Clone)]
pub(crate) struct CatalogReadinessQueryRequest {
    contract_request: CatalogQueryContractRequest,
}

impl CatalogReadinessQueryRequest {
    pub(crate) const fn new(contract_request: CatalogQueryContractRequest) -> Self {
        Self { contract_request }
    }
}

/// Transport-neutral readiness result. The plan is returned with the response
/// so a caller can retain its opaque identity and bind every later page or
/// resolution request to that exact durable authority.
#[derive(Serialize)]
pub(crate) struct CatalogReadinessQueryResult {
    pub coverage_plan: CatalogPortableCoveragePlan,
    pub readiness: CatalogReadinessResponse,
}

fn invalid_catalog_query_input(detail: &'static str) -> EngineError {
    EngineError::InvalidQuery(format!("invalid catalog query request: {detail}"))
}

fn negotiate_durable_query(
    request: &CatalogQueryContractRequest,
    authority: &CatalogReadyReadAuthority,
) -> Result<CatalogQueryContractSelection, EngineError> {
    negotiate_catalog_query_contract_for_selection(request, authority.contract_selection()).map_err(
        |error| match error {
            CatalogQueryNegotiationError::IncompatibleCatalogContract { axis } => {
                EngineError::InvalidQuery(format!("IncompatibleCatalogContract: {axis}"))
            }
            CatalogQueryNegotiationError::InvalidCatalogContract { .. } => {
                invalid_catalog_query_input("contract negotiation is invalid")
            }
        },
    )
}

fn require_expected_plan(
    state: &catalog_state::DurableCatalogBuildState,
    expected: CatalogCoveragePlanId,
) -> Result<(), EngineError> {
    if state.plan.coverage_plan_id != expected {
        return Err(invalid_catalog_query_input(
            "coverage plan differs from the current durable authority",
        ));
    }
    Ok(())
}

pub(crate) fn execute_catalog_readiness_query(
    connection: &Connection,
    request: &CatalogReadinessQueryRequest,
) -> Result<CatalogReadinessQueryResult, EngineError> {
    let state = catalog_state::load_catalog_build_state(connection)?.ok_or_else(|| {
        EngineError::InvalidQuery("catalog readiness is not available".to_string())
    })?;
    let authority = state.ready_read_authority()?;
    let selection = negotiate_durable_query(&request.contract_request, &authority)?;
    let coverage_plan = CatalogPortableCoveragePlan::from_plan(&state.plan)
        .map_err(catalog_state::catalog_contract_error)?;
    let readiness = CatalogReadinessResponse::new(
        selection,
        state.readiness.clone(),
        BTreeMap::new(),
        &state.plan,
    )
    .map_err(catalog_state::catalog_contract_error)?;
    Ok(CatalogReadinessQueryResult {
        coverage_plan,
        readiness,
    })
}

pub(crate) fn execute_catalog_page_query(
    connection: &Connection,
    request: &CatalogPageQueryRequest,
) -> Result<CatalogRetainedPageOutcome, EngineError> {
    let state = catalog_state::load_catalog_build_state(connection)?.ok_or_else(|| {
        EngineError::InvalidQuery("catalog readiness is not available".to_string())
    })?;
    require_expected_plan(&state, request.expected_coverage_plan_id)?;
    let current_authority = state.ready_read_authority()?;
    let selection = negotiate_durable_query(&request.contract_request, &current_authority)?;
    let continuation = request
        .continuation
        .as_ref()
        .map(|continuation| {
            CatalogContinuationRequest::from_wire_value(continuation.clone(), &selection)
                .map_err(|_| invalid_catalog_query_input("continuation is invalid"))
        })
        .transpose()?;
    if continuation
        .as_ref()
        .is_some_and(|continuation| continuation.page_size != request.page_size)
    {
        return Err(invalid_catalog_query_input(
            "page size differs from the continuation",
        ));
    }
    let snapshot_id = continuation.as_ref().map_or_else(
        || current_authority.snapshot_id(),
        |value| value.snapshot_id,
    );
    if snapshot_id != request.expected_snapshot_id {
        return Err(invalid_catalog_query_input(
            "snapshot differs from the caller-held catalog authority",
        ));
    }
    let retained_request = match request.query_kind {
        CatalogQueryKind::Projects => CatalogRetainedPageRequest::projects_all(
            selection,
            snapshot_id,
            request.page_size,
            continuation,
        ),
        CatalogQueryKind::Sessions => CatalogRetainedPageRequest::sessions_all(
            selection,
            snapshot_id,
            request.page_size,
            continuation,
        ),
    };
    read_catalog_page_with_retirement(connection, &current_authority, &retained_request)
}

pub(crate) fn execute_catalog_resolution_query(
    connection: &Connection,
    request: &CatalogResolutionQueryRequest,
) -> Result<CatalogEntityResolutionResponse, EngineError> {
    let state = catalog_state::load_catalog_build_state(connection)?.ok_or_else(|| {
        EngineError::InvalidQuery("catalog readiness is not available".to_string())
    })?;
    require_expected_plan(&state, request.expected_coverage_plan_id)?;
    let authority = state.ready_read_authority()?;
    if authority.snapshot_id() != request.expected_snapshot_id {
        return Err(invalid_catalog_query_input(
            "snapshot differs from the caller-held catalog authority",
        ));
    }
    let selection = negotiate_durable_query(&request.contract_request, &authority)?;
    let binding = CatalogResolutionRequestBinding::new(
        selection,
        authority.snapshot_id(),
        request.external_ref,
    )
    .map_err(|_| invalid_catalog_query_input("external reference is invalid"))?;
    resolve_retained_catalog_entity(connection, &authority, &binding)
}

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
        self.validate_for_selection(expected_selection)
    }

    fn validate_for_selection(
        &self,
        expected_selection: &crate::adapter::ContractVersionSelection,
    ) -> Result<CatalogPageRequestBinding, EngineError> {
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

pub(crate) enum CatalogRetainedPage {
    Projects(CatalogProjectPage),
    Sessions(CatalogSessionPage),
}

pub(crate) enum CatalogRetainedPageOutcome {
    Page(Box<CatalogRetainedPage>),
    SnapshotExpired(Box<CatalogSnapshotExpired>),
}

impl CatalogRetainedPageOutcome {
    pub(crate) fn to_wire_value(&self) -> Result<JsonValue, EngineError> {
        match self {
            Self::Page(page) => match page.as_ref() {
                CatalogRetainedPage::Projects(page) => serde_json::to_value(page),
                CatalogRetainedPage::Sessions(page) => serde_json::to_value(page),
            },
            Self::SnapshotExpired(expiration) => serde_json::to_value(expiration),
        }
        .map_err(|_| EngineError::InvalidCommit("catalog response is not portable".to_string()))
    }
}

/// Resolve one current or historical page against the exact current Ready
/// authority. Request/cursor context is validated before retirement is
/// consulted; retirement classification and row reads then share one SQLite
/// read transaction.
pub(super) fn read_catalog_page_with_retirement(
    connection: &Connection,
    current_authority: &CatalogReadyReadAuthority,
    request: &CatalogRetainedPageRequest,
) -> Result<CatalogRetainedPageOutcome, EngineError> {
    let binding = request.validate_for_selection(current_authority.contract_selection())?;
    let chain_index = current_authority
        .retained_chain()
        .iter()
        .position(|commitment| commitment.snapshot_id() == request.snapshot_id)
        .ok_or_else(|| {
            EngineError::InvalidCommit(
                "catalog page snapshot is outside the caller-held current ancestry".to_string(),
            )
        })?;

    let transaction = connection
        .unchecked_transaction()
        .map_err(|error| catalog_state::sqlite_error("begin catalog retention read", error))?;
    let fresh = catalog_state::load_catalog_build_state(&transaction)?
        .ok_or_else(|| {
            EngineError::InvalidCommit(
                "catalog retained-page read requires a durable Ready lineage".to_string(),
            )
        })?
        .ready_read_authority()?;
    if fresh != *current_authority {
        return Err(EngineError::InvalidCommit(
            "catalog current read authority became stale before retention classification"
                .to_string(),
        ));
    }
    let retired_prefix =
        super::catalog_retention::load_retired_prefix(&transaction, fresh.retained_chain())?;
    if chain_index < retired_prefix {
        let continuation = request.continuation.as_ref().ok_or_else(|| {
            EngineError::InvalidCommit(
                "an explicitly requested retired page-1 snapshot is not retained".to_string(),
            )
        })?;
        let disposition = validate_continuation_retention(
            serde_json::to_value(continuation).map_err(catalog_state::catalog_contract_error)?,
            continuation,
            CatalogCoverageScope::Library,
            CatalogSnapshotRetention::Expired {
                latest_snapshot: fresh.snapshot_id(),
            },
        )
        .map_err(catalog_state::catalog_contract_error)?;
        let CatalogContinuationDisposition::SnapshotExpired(expiration) = disposition else {
            return Err(EngineError::InvalidCommit(
                "retired catalog continuation did not produce expiration".to_string(),
            ));
        };
        transaction.commit().map_err(|error| {
            catalog_state::sqlite_error("commit catalog expiration read", error)
        })?;
        return Ok(CatalogRetainedPageOutcome::SnapshotExpired(Box::new(
            expiration,
        )));
    }

    let authority =
        fresh.for_historical_snapshot(&transaction, request.snapshot_id, retired_prefix)?;
    let page =
        read_retained_catalog_page_in_transaction(&transaction, &authority, request, binding)?;
    transaction
        .commit()
        .map_err(|error| catalog_state::sqlite_error("commit retained catalog page", error))?;
    Ok(CatalogRetainedPageOutcome::Page(Box::new(page)))
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
    super::catalog_retention::ensure_snapshot_query_retained(
        &transaction,
        authority.snapshot_id(),
    )?;
    let result =
        read_retained_catalog_page_in_transaction(&transaction, authority, request, binding)?;
    transaction
        .commit()
        .map_err(|error| catalog_state::sqlite_error("commit retained catalog page", error))?;
    Ok(result)
}

fn read_retained_catalog_page_in_transaction(
    connection: &Connection,
    authority: &CatalogReadyReadAuthority,
    request: &CatalogRetainedPageRequest,
    binding: CatalogPageRequestBinding,
) -> Result<CatalogRetainedPage, EngineError> {
    let header = load_retained_query_header(
        connection,
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
                connection,
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
                connection,
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
    Ok(result)
}

pub(super) fn resolve_retained_catalog_entity(
    connection: &Connection,
    authority: &CatalogReadyReadAuthority,
    request: &CatalogResolutionRequestBinding,
) -> Result<CatalogEntityResolutionResponse, EngineError> {
    // Validate every caller-held binding before touching retained state. In
    // particular, malformed/foreign references cannot be relabeled as a
    // retention or database failure.
    request
        .validate()
        .map_err(catalog_state::catalog_contract_error)?;
    if request.snapshot_id != authority.snapshot_id()
        || request.contract_selection.contract_versions != *authority.contract_selection()
    {
        return Err(EngineError::InvalidCommit(
            "catalog resolution request differs from its exact Ready snapshot selection"
                .to_string(),
        ));
    }

    let transaction = connection
        .unchecked_transaction()
        .map_err(|error| catalog_state::sqlite_error("begin retained catalog resolution", error))?;
    super::catalog_retention::ensure_snapshot_query_retained(
        &transaction,
        authority.snapshot_id(),
    )?;
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
    let lifecycle = authority
        .publication_identity()
        .resolution_index()
        .resolve(request.external_ref);
    let live_row = match &lifecycle {
        CatalogResolvedLifecycle::Live { entity_ref } => Some(load_resolution_row(
            &transaction,
            &header,
            authority.publication_identity(),
            authority.snapshot_id(),
            *entity_ref,
        )?),
        CatalogResolvedLifecycle::Tombstoned { .. }
        | CatalogResolvedLifecycle::Superseded { .. }
        | CatalogResolvedLifecycle::Unknown { .. } => None,
    };
    let resolution = CatalogEntityResolution::from_bound_lifecycle(
        request.external_ref,
        &lifecycle,
        live_row.as_ref(),
        &policy,
        authority.plan(),
        &request.contract_selection,
    )
    .map_err(catalog_state::catalog_contract_error)?;
    let response =
        CatalogEntityResolutionResponse::new(request.clone(), resolution, BTreeMap::new())
            .map_err(catalog_state::catalog_contract_error)?;
    transaction.commit().map_err(|error| {
        catalog_state::sqlite_error("commit retained catalog resolution", error)
    })?;
    Ok(response)
}

fn load_resolution_row(
    connection: &Connection,
    header: &CatalogRetainedQueryHeader,
    expected_identity: &CatalogReadyPublicationIdentity,
    snapshot_id: crate::catalog_contract::CatalogSnapshotId,
    entity_ref: crate::catalog_contract::evidence::CatalogEntityRef,
) -> Result<CatalogLiveRow, EngineError> {
    let kind = match entity_ref.kind {
        CatalogEntityKind::Project => CatalogDurablePublicationEntryKind::ProjectRow,
        CatalogEntityKind::Session => CatalogDurablePublicationEntryKind::SessionRow,
    };
    let payload_limit = header
        .encoded_bytes
        .min(MAX_DURABLE_CATALOG_ROW_BYTES)
        .min(MAX_RETAINED_PAGE_PAYLOAD_BYTES);
    let stored = connection
        .query_row(
            EXACT_RESOLUTION_ROW_SQL,
            params![
                catalog_state::to_i64(
                    snapshot_id.complete_commit,
                    "catalog resolution snapshot commit",
                )?,
                kind.as_str(),
                entity_ref.external_ref.entity_key.as_bytes().as_slice(),
                catalog_state::to_i64(payload_limit as u64, "catalog resolution row byte limit")?,
            ],
            |row| {
                Ok((
                    row.get::<_, Option<i64>>(0)?,
                    row.get::<_, Option<Vec<u8>>>(1)?,
                    row.get::<_, Option<Vec<u8>>>(2)?,
                ))
            },
        )
        .optional()
        .map_err(|error| {
            catalog_state::sqlite_error("load retained catalog resolution row", error)
        })?
        .ok_or_else(|| {
            catalog_state::corrupt_catalog_state(
                "restart-validated live catalog entity is missing its durable row",
            )
        })?;
    let payload_len = usize::try_from(stored.0.ok_or_else(|| {
        catalog_state::corrupt_catalog_state(
            "retained catalog resolution row is outside its preflight byte bound",
        )
    })?)
    .map_err(|_| {
        catalog_state::corrupt_catalog_state(
            "retained catalog resolution row length is negative or too large",
        )
    })?;
    let payload = stored.1.ok_or_else(|| {
        catalog_state::corrupt_catalog_state(
            "retained catalog resolution row is outside its preflight byte bound",
        )
    })?;
    if payload.len() != payload_len {
        return Err(catalog_state::corrupt_catalog_state(
            "retained catalog resolution row changed after length preflight",
        ));
    }
    let digest = decode_nonzero_digest(
        stored.2.as_deref().ok_or_else(|| {
            catalog_state::corrupt_catalog_state(
                "retained catalog resolution row digest is outside its fixed bound",
            )
        })?,
        "retained catalog resolution row digest",
    )?;
    if blake3::hash(&payload).as_bytes() != &digest
        || !expected_identity.matches_row(
            kind,
            entity_ref.external_ref.entity_key.as_bytes(),
            payload.len(),
            &digest,
        )
    {
        return Err(catalog_state::corrupt_catalog_state(
            "retained catalog resolution row differs from its restart-validated commitment",
        ));
    }
    match entity_ref.kind {
        CatalogEntityKind::Project => decode_durable_project_row(
            &payload,
            entity_ref.external_ref.entity_key.as_bytes(),
            MAX_DURABLE_CATALOG_ROW_BYTES,
        )
        .map(CatalogLiveRow::Project)
        .map_err(catalog_state::catalog_contract_error),
        CatalogEntityKind::Session => decode_durable_session_row(
            &payload,
            entity_ref.external_ref.entity_key.as_bytes(),
            MAX_DURABLE_CATALOG_ROW_BYTES,
        )
        .map(CatalogLiveRow::Session)
        .map_err(catalog_state::catalog_contract_error),
    }
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
