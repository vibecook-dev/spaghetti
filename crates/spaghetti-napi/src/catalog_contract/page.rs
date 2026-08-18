//! RFC 012B portable catalog page, readiness, resolution, and expiration contracts.
//!
//! This module is intentionally crate-private and contract-only. It does not
//! execute catalog queries, retain snapshots, read sources, or expose N-API.
//! `CatalogPolicyView` remains an internal projection input; public transport
//! exposure stays gated until an authorized view is bound to the frozen plan's
//! access-policy digest.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use serde_json::Value as JsonValue;

use crate::adapter::{
    CanonicalSourceInstanceKey, CoverageDeclarationDigest, ExternalEntityRef, NativeIdentity,
    QualifiedUnknownReason, QualifiedValueQuality, SemanticRevisionRef, SourceCoverageSet,
    EXTERNAL_ENTITY_REFERENCE_VERSION, SEMANTIC_REFERENCE_CONTRACT_VERSION,
};

use super::evidence::{
    CatalogAssertionKey, CatalogAssociationCoverage, CatalogAvailability, CatalogEntityKind,
    CatalogEntityRef, CatalogEvidenceOwner, CatalogFieldAuthority, CatalogFieldSelection,
    CatalogLiveRow, CatalogLocatorClaimKey, CatalogPolicyView, CatalogProjectRow,
    CatalogQualifiedValue, CatalogResolvedEntity, CatalogSessionRow, CatalogUnknownReferenceReason,
    ProjectAssociationBasis, SessionProjectAssociationFact,
};
use super::query::{
    validate_typed_unknown_fields, CatalogContinuationRequest, CatalogQueryContractSelection,
    MAX_CONTINUATION_PAGE_SIZE,
};
use super::{
    validate_identifier, CatalogAccessPolicyDigest, CatalogContractError, CatalogCoveragePlan,
    CatalogCoveragePlanId, CatalogCoveragePlanSource, CatalogCoverageScope, CatalogCursor,
    CatalogQueryFingerprint, CatalogQueryKind, CatalogReadinessPhase, CatalogReadinessSnapshot,
    CatalogSnapshotId, CatalogSortKey, CATALOG_COVERAGE_PLAN_CONTRACT_VERSION,
};

pub(crate) const CATALOG_PAGE_CONTRACT_VERSION: u32 = 1;
pub(crate) const CATALOG_PORTABLE_COVERAGE_PLAN_CONTRACT_VERSION: u32 = 1;
pub(crate) const CATALOG_READINESS_RESPONSE_CONTRACT_VERSION: u32 = 1;
pub(crate) const CATALOG_RESOLUTION_CONTRACT_VERSION: u32 = 1;
pub(crate) const CATALOG_SNAPSHOT_EXPIRATION_CONTRACT_VERSION: u32 = 1;

const MAX_JAVASCRIPT_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
const MAX_ROW_EVIDENCE_KEYS: usize = 4_096;
const MAX_ASSOCIATION_EVIDENCE: usize = 4_096;
const MAX_RESOLUTION_TARGETS: usize = 4_096;
const MAX_PROVENANCE_REVISIONS: usize = 64;
const MAX_SOURCE_COVERAGE_MEMBERS: usize = 16_384;
const MAX_PORTABLE_COVERAGE_REASON_BYTES: usize = 1_024;

/// Opaque proof that a portable row projection is bound to one validated
/// Library plan and one exact negotiated selection. The initial retained-page
/// executor can issue only the privacy-preserving WITHHELD view; no LOCAL
/// constructor exists in this slice.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct CatalogPolicyViewBinding {
    coverage_plan_id: CatalogCoveragePlanId,
    scope: CatalogCoverageScope,
    contract_selection: CatalogQueryContractSelection,
    view: CatalogPolicyView,
}

impl CatalogPolicyViewBinding {
    pub(crate) fn withheld(
        plan: &CatalogCoveragePlan,
        contract_selection: &CatalogQueryContractSelection,
    ) -> Result<Self, CatalogContractError> {
        plan.validate()?;
        contract_selection.validate()?;
        if plan.scope != CatalogCoverageScope::Library {
            return Err(CatalogContractError::invalid(
                "initial retained catalog pages require the Library scope",
            ));
        }
        Ok(Self {
            coverage_plan_id: plan.coverage_plan_id,
            scope: plan.scope,
            contract_selection: contract_selection.clone(),
            view: CatalogPolicyView::WITHHELD,
        })
    }

    fn validate_for(
        &self,
        plan: &CatalogCoveragePlan,
        contract_selection: &CatalogQueryContractSelection,
    ) -> Result<CatalogPolicyView, CatalogContractError> {
        plan.validate()?;
        contract_selection.validate()?;
        if self.coverage_plan_id != plan.coverage_plan_id
            || self.scope != plan.scope
            || self.contract_selection != *contract_selection
            || self.view != CatalogPolicyView::WITHHELD
        {
            return Err(CatalogContractError::invalid(
                "catalog policy view does not match the exact plan and negotiated selection",
            ));
        }
        Ok(self.view)
    }
}

fn validate_portable_u64(label: &str, value: u64) -> Result<(), CatalogContractError> {
    if value > MAX_JAVASCRIPT_SAFE_INTEGER {
        return Err(CatalogContractError::invalid(format!(
            "{label} must be a JavaScript-safe integer"
        )));
    }
    Ok(())
}

fn validate_positive_portable_u64(label: &str, value: u64) -> Result<(), CatalogContractError> {
    validate_portable_u64(label, value)?;
    if value == 0 {
        return Err(CatalogContractError::invalid(format!(
            "{label} must be greater than zero"
        )));
    }
    Ok(())
}

fn validate_portable_i64(label: &str, value: i64) -> Result<(), CatalogContractError> {
    if value.unsigned_abs() > MAX_JAVASCRIPT_SAFE_INTEGER {
        return Err(CatalogContractError::invalid(format!(
            "{label} must be a JavaScript-safe integer"
        )));
    }
    Ok(())
}

fn validate_strictly_increasing<T: Ord>(
    label: &str,
    values: &[T],
    max_len: usize,
) -> Result<(), CatalogContractError> {
    if values.len() > max_len {
        return Err(CatalogContractError::invalid(format!(
            "{label} exceeds {max_len} entries"
        )));
    }
    if !values.windows(2).all(|pair| pair[0] < pair[1]) {
        return Err(CatalogContractError::invalid(format!(
            "{label} must be strictly canonical and duplicate-free"
        )));
    }
    Ok(())
}

fn canonicalize_provenance(provenance: &mut Vec<SemanticRevisionRef>) {
    provenance.sort_by_key(|reference| {
        (
            reference.semantic_reference_contract_version,
            reference.fact_revision_id,
        )
    });
    provenance.dedup();
}

fn validate_provenance(
    label: &str,
    provenance: &[SemanticRevisionRef],
) -> Result<(), CatalogContractError> {
    if provenance.is_empty() || provenance.len() > MAX_PROVENANCE_REVISIONS {
        return Err(CatalogContractError::invalid(format!(
            "{label} requires 1..={MAX_PROVENANCE_REVISIONS} semantic revisions"
        )));
    }
    if provenance.iter().any(|reference| {
        reference.semantic_reference_contract_version != SEMANTIC_REFERENCE_CONTRACT_VERSION
    }) || !provenance.windows(2).all(|pair| {
        (
            pair[0].semantic_reference_contract_version,
            pair[0].fact_revision_id,
        ) < (
            pair[1].semantic_reference_contract_version,
            pair[1].fact_revision_id,
        )
    }) {
        return Err(CatalogContractError::invalid(format!(
            "{label} provenance must be compatible, canonical, and duplicate-free"
        )));
    }
    Ok(())
}

fn validate_owner(owner: &CatalogEvidenceOwner) -> Result<(), CatalogContractError> {
    validate_identifier("catalog portable evidence adapter", &owner.adapter_id)?;
    validate_positive_portable_u64("catalog evidence generation", owner.generation)
}

fn validate_authority(authority: &CatalogFieldAuthority) -> Result<(), CatalogContractError> {
    validate_identifier("catalog portable field authority", &authority.class_id)?;
    if authority.precedence == 0 {
        return Err(CatalogContractError::invalid(
            "catalog portable field authority precedence must be greater than zero",
        ));
    }
    Ok(())
}

fn validate_qualified<T>(
    label: &str,
    value: &CatalogQualifiedValue<T>,
    validate_value: impl FnOnce(&T) -> Result<(), CatalogContractError>,
) -> Result<(), CatalogContractError> {
    let unknown = value.quality == QualifiedValueQuality::Unknown;
    if unknown != value.value.is_none() || unknown != value.unknown_reason.is_some() {
        return Err(CatalogContractError::invalid(format!(
            "{label} must carry a value exactly when its quality is concrete"
        )));
    }
    validate_authority(&value.authority)?;
    if let Some(effective_at) = value.effective_at {
        validate_portable_i64("catalog qualified effective time", effective_at)?;
    }
    validate_provenance(label, &value.provenance)?;
    if let Some(value) = &value.value {
        validate_value(value)?;
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CatalogPortableFieldSelection<T> {
    pub selected_assertion_key: CatalogAssertionKey,
    pub field: CatalogQualifiedValue<T>,
    pub conflicting_assertion_keys: Vec<CatalogAssertionKey>,
}

impl<T: Clone> CatalogPortableFieldSelection<T> {
    fn from_evidence(selection: &CatalogFieldSelection<T>, view: CatalogPolicyView) -> Self {
        let mut field = selection.field.for_view(view);
        canonicalize_provenance(&mut field.provenance);
        Self {
            selected_assertion_key: selection.selected_assertion_key,
            field,
            conflicting_assertion_keys: selection.conflicting_assertion_keys.clone(),
        }
    }

    fn validate(
        &self,
        label: &str,
        validate_value: impl FnOnce(&T) -> Result<(), CatalogContractError>,
    ) -> Result<(), CatalogContractError> {
        validate_strictly_increasing(
            "catalog field conflicting assertion keys",
            &self.conflicting_assertion_keys,
            MAX_ROW_EVIDENCE_KEYS,
        )?;
        if self
            .conflicting_assertion_keys
            .contains(&self.selected_assertion_key)
        {
            return Err(CatalogContractError::invalid(
                "selected catalog evidence cannot also be a conflicting assertion",
            ));
        }
        validate_qualified(label, &self.field, validate_value)
    }

    fn append_evidence_keys(&self, keys: &mut Vec<CatalogAssertionKey>) {
        keys.push(self.selected_assertion_key);
        keys.extend(self.conflicting_assertion_keys.iter().copied());
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum CatalogOptionalField<T> {
    Selected {
        selection: Box<CatalogPortableFieldSelection<T>>,
    },
    Unknown {
        reason: QualifiedUnknownReason,
    },
}

impl<T: Clone> CatalogOptionalField<T> {
    fn from_evidence(
        selection: Option<&CatalogFieldSelection<T>>,
        view: CatalogPolicyView,
    ) -> Self {
        match selection {
            Some(selection) => Self::Selected {
                selection: Box::new(CatalogPortableFieldSelection::from_evidence(
                    selection, view,
                )),
            },
            None => Self::Unknown {
                reason: QualifiedUnknownReason::NotYetObserved,
            },
        }
    }

    fn validate(
        &self,
        label: &str,
        validate_value: impl FnOnce(&T) -> Result<(), CatalogContractError>,
    ) -> Result<(), CatalogContractError> {
        match self {
            Self::Selected { selection } => selection.validate(label, validate_value),
            Self::Unknown { .. } => Ok(()),
        }
    }

    fn selected_key(&self) -> Option<CatalogAssertionKey> {
        match self {
            Self::Selected { selection } => Some(selection.selected_assertion_key),
            Self::Unknown { .. } => None,
        }
    }

    fn append_evidence_keys(&self, keys: &mut Vec<CatalogAssertionKey>) {
        if let Self::Selected { selection } = self {
            selection.append_evidence_keys(keys);
        }
    }
}

fn validate_text(label: &str, value: &str) -> Result<(), CatalogContractError> {
    if value.is_empty() || value.trim() != value || value.len() > 16 * 1_024 {
        return Err(CatalogContractError::invalid(format!(
            "{label} must be non-empty canonical text of at most 16384 bytes"
        )));
    }
    Ok(())
}

fn validate_native_identity(value: &NativeIdentity) -> Result<(), CatalogContractError> {
    validate_identifier(
        "catalog portable native identity namespace",
        &value.native_namespace,
    )?;
    validate_text("catalog portable native identity", &value.native_id)
}

fn validate_availability(value: &CatalogAvailability) -> Result<(), CatalogContractError> {
    if let CatalogAvailability::Unavailable { reason } = value {
        validate_identifier("catalog portable unavailable reason", reason)?;
    }
    Ok(())
}

fn canonicalize_association(
    mut fact: SessionProjectAssociationFact,
) -> SessionProjectAssociationFact {
    canonicalize_provenance(&mut fact.provenance);
    fact
}

fn validate_association_fact(
    fact: &SessionProjectAssociationFact,
) -> Result<(), CatalogContractError> {
    validate_owner(&fact.owner)?;
    fact.session_ref.validate()?;
    fact.project_ref.validate()?;
    if fact.session_ref.kind != CatalogEntityKind::Session
        || fact.project_ref.kind != CatalogEntityKind::Project
    {
        return Err(CatalogContractError::invalid(
            "portable catalog association must relate a session to a project",
        ));
    }
    match (fact.basis, &fact.declared_derivation_id) {
        (ProjectAssociationBasis::DeclaredDerivedAncestor, Some(identifier)) => {
            validate_identifier("catalog declared derivation id", identifier)?;
        }
        (ProjectAssociationBasis::DeclaredDerivedAncestor, None) => {
            return Err(CatalogContractError::invalid(
                "derived catalog association requires its declaration id",
            ));
        }
        (_, None) => {}
        (_, Some(_)) => {
            return Err(CatalogContractError::invalid(
                "only a derived catalog association may carry a declaration id",
            ));
        }
    }
    validate_authority(&fact.authority)?;
    if fact.quality == QualifiedValueQuality::Unknown {
        return Err(CatalogContractError::invalid(
            "unknown evidence cannot select a portable catalog association",
        ));
    }
    if let Some(effective_at) = fact.effective_at {
        validate_portable_i64("catalog association effective time", effective_at)?;
    }
    validate_provenance("catalog association", &fact.provenance)
}

fn portable_association(coverage: &CatalogAssociationCoverage) -> CatalogAssociationCoverage {
    match coverage {
        CatalogAssociationCoverage::Unknown => CatalogAssociationCoverage::Unknown,
        CatalogAssociationCoverage::Available { selection } => {
            let mut selection = (**selection).clone();
            selection.association = canonicalize_association(selection.association);
            selection.competing_associations = selection
                .competing_associations
                .into_iter()
                .map(canonicalize_association)
                .collect();
            CatalogAssociationCoverage::Available {
                selection: Box::new(selection),
            }
        }
    }
}

fn validate_association(
    coverage: &CatalogAssociationCoverage,
    session_ref: CatalogEntityRef,
) -> Result<(), CatalogContractError> {
    let CatalogAssociationCoverage::Available { selection } = coverage else {
        return Ok(());
    };
    validate_association_fact(&selection.association)?;
    if selection.association.session_ref != session_ref {
        return Err(CatalogContractError::invalid(
            "selected catalog association belongs to a different session row",
        ));
    }
    if selection.competing_associations.len() > MAX_ASSOCIATION_EVIDENCE
        || !selection
            .competing_associations
            .windows(2)
            .all(|pair| pair[0].association_key < pair[1].association_key)
    {
        return Err(CatalogContractError::invalid(
            "competing catalog associations must be bounded, canonical, and duplicate-free",
        ));
    }
    let mut expected_conflict_keys = Vec::new();
    for competitor in &selection.competing_associations {
        validate_association_fact(competitor)?;
        if competitor.session_ref != session_ref
            || competitor.association_key == selection.association.association_key
        {
            return Err(CatalogContractError::invalid(
                "competing catalog association is not independent evidence for this session",
            ));
        }
        if competitor.authority == selection.association.authority
            && competitor.project_ref != selection.association.project_ref
        {
            expected_conflict_keys.push(competitor.association_key);
        }
    }
    validate_strictly_increasing(
        "catalog conflicting association keys",
        &selection.conflicting_association_keys,
        MAX_ASSOCIATION_EVIDENCE,
    )?;
    if selection.conflicting_association_keys != expected_conflict_keys {
        return Err(CatalogContractError::invalid(
            "catalog association conflicts must exactly identify equal-authority evidence for a different project",
        ));
    }
    Ok(())
}

fn validate_assertion_membership(
    assertion_keys: &[CatalogAssertionKey],
    selected_keys: impl IntoIterator<Item = CatalogAssertionKey>,
) -> Result<(), CatalogContractError> {
    validate_strictly_increasing(
        "catalog row assertion keys",
        assertion_keys,
        MAX_ROW_EVIDENCE_KEYS,
    )?;
    if assertion_keys.is_empty() {
        return Err(CatalogContractError::invalid(
            "portable catalog row requires membership evidence",
        ));
    }
    if selected_keys
        .into_iter()
        .any(|key| assertion_keys.binary_search(&key).is_err())
    {
        return Err(CatalogContractError::invalid(
            "catalog field selection must belong to the row's assertion evidence",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CatalogPortableProjectRow {
    pub project_ref: CatalogEntityRef,
    pub native_identity: CatalogOptionalField<NativeIdentity>,
    pub root_identity: CatalogOptionalField<String>,
    pub display_path: CatalogOptionalField<String>,
    pub display_name: CatalogOptionalField<String>,
    pub native_time: CatalogOptionalField<i64>,
    pub availability: CatalogPortableFieldSelection<CatalogAvailability>,
    pub assertion_keys: Vec<CatalogAssertionKey>,
}

impl CatalogPortableProjectRow {
    fn from_evidence(
        row: &CatalogProjectRow,
        view: CatalogPolicyView,
    ) -> Result<Self, CatalogContractError> {
        let value = Self {
            project_ref: row.project_ref,
            native_identity: CatalogOptionalField::from_evidence(
                row.native_identity.as_ref(),
                view,
            ),
            root_identity: CatalogOptionalField::from_evidence(row.root_identity.as_ref(), view),
            display_path: CatalogOptionalField::from_evidence(row.display_path.as_ref(), view),
            display_name: CatalogOptionalField::from_evidence(row.display_name.as_ref(), view),
            native_time: CatalogOptionalField::from_evidence(row.native_time.as_ref(), view),
            availability: CatalogPortableFieldSelection::from_evidence(&row.availability, view),
            assertion_keys: row.assertion_keys.clone(),
        };
        value.validate()?;
        Ok(value)
    }

    pub(crate) fn from_bound_evidence(
        row: &CatalogProjectRow,
        binding: &CatalogPolicyViewBinding,
        plan: &CatalogCoveragePlan,
        contract_selection: &CatalogQueryContractSelection,
    ) -> Result<Self, CatalogContractError> {
        Self::from_evidence(row, binding.validate_for(plan, contract_selection)?)
    }

    fn validate(&self) -> Result<(), CatalogContractError> {
        self.project_ref.validate()?;
        if self.project_ref.kind != CatalogEntityKind::Project {
            return Err(CatalogContractError::invalid(
                "project page row requires a project reference",
            ));
        }
        self.native_identity
            .validate("catalog project native identity", validate_native_identity)?;
        self.root_identity
            .validate("catalog project root identity", |value| {
                validate_text("catalog project root identity", value)
            })?;
        self.display_path
            .validate("catalog project display path", |value| {
                validate_text("catalog project display path", value)
            })?;
        self.display_name
            .validate("catalog project display name", |value| {
                validate_text("catalog project display name", value)
            })?;
        self.native_time
            .validate("catalog project native time", |value| {
                validate_portable_i64("catalog project native time", *value)
            })?;
        self.availability
            .validate("catalog project availability", validate_availability)?;
        let mut referenced_evidence = Vec::new();
        self.native_identity
            .append_evidence_keys(&mut referenced_evidence);
        self.root_identity
            .append_evidence_keys(&mut referenced_evidence);
        self.display_path
            .append_evidence_keys(&mut referenced_evidence);
        self.display_name
            .append_evidence_keys(&mut referenced_evidence);
        self.native_time
            .append_evidence_keys(&mut referenced_evidence);
        self.availability
            .append_evidence_keys(&mut referenced_evidence);
        validate_assertion_membership(&self.assertion_keys, referenced_evidence)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CatalogPortableSessionRow {
    pub session_ref: CatalogEntityRef,
    pub project_association: CatalogAssociationCoverage,
    pub native_identity: CatalogOptionalField<NativeIdentity>,
    pub title: CatalogOptionalField<String>,
    pub first_user_summary: CatalogOptionalField<String>,
    pub native_created_at: CatalogOptionalField<i64>,
    pub native_updated_at: CatalogOptionalField<i64>,
    pub native_message_count: CatalogOptionalField<u64>,
    pub transcript_locator_claim_keys: Vec<CatalogLocatorClaimKey>,
    pub availability: CatalogPortableFieldSelection<CatalogAvailability>,
    pub assertion_keys: Vec<CatalogAssertionKey>,
}

impl CatalogPortableSessionRow {
    fn from_evidence(
        row: &CatalogSessionRow,
        view: CatalogPolicyView,
    ) -> Result<Self, CatalogContractError> {
        let value = Self {
            session_ref: row.session_ref,
            project_association: portable_association(&row.project_association),
            native_identity: CatalogOptionalField::from_evidence(
                row.native_identity.as_ref(),
                view,
            ),
            title: CatalogOptionalField::from_evidence(row.title.as_ref(), view),
            first_user_summary: CatalogOptionalField::from_evidence(
                row.first_user_summary.as_ref(),
                view,
            ),
            native_created_at: CatalogOptionalField::from_evidence(
                row.native_created_at.as_ref(),
                view,
            ),
            native_updated_at: CatalogOptionalField::from_evidence(
                row.native_updated_at.as_ref(),
                view,
            ),
            native_message_count: CatalogOptionalField::from_evidence(
                row.native_message_count.as_ref(),
                view,
            ),
            transcript_locator_claim_keys: row.transcript_locator_claim_keys.clone(),
            availability: CatalogPortableFieldSelection::from_evidence(&row.availability, view),
            assertion_keys: row.assertion_keys.clone(),
        };
        value.validate()?;
        Ok(value)
    }

    pub(crate) fn from_bound_evidence(
        row: &CatalogSessionRow,
        binding: &CatalogPolicyViewBinding,
        plan: &CatalogCoveragePlan,
        contract_selection: &CatalogQueryContractSelection,
    ) -> Result<Self, CatalogContractError> {
        Self::from_evidence(row, binding.validate_for(plan, contract_selection)?)
    }

    fn validate(&self) -> Result<(), CatalogContractError> {
        self.session_ref.validate()?;
        if self.session_ref.kind != CatalogEntityKind::Session {
            return Err(CatalogContractError::invalid(
                "session page row requires a session reference",
            ));
        }
        validate_association(&self.project_association, self.session_ref)?;
        self.native_identity
            .validate("catalog session native identity", validate_native_identity)?;
        self.title.validate("catalog session title", |value| {
            validate_text("catalog session title", value)
        })?;
        self.first_user_summary
            .validate("catalog session first-user summary", |value| {
                validate_text("catalog session first-user summary", value)
            })?;
        self.native_created_at
            .validate("catalog session native creation time", |value| {
                validate_portable_i64("catalog session native creation time", *value)
            })?;
        self.native_updated_at
            .validate("catalog session native update time", |value| {
                validate_portable_i64("catalog session native update time", *value)
            })?;
        self.native_message_count
            .validate("catalog session native message count", |value| {
                validate_portable_u64("catalog session native message count", *value)
            })?;
        validate_strictly_increasing(
            "catalog transcript locator claim keys",
            &self.transcript_locator_claim_keys,
            MAX_ROW_EVIDENCE_KEYS,
        )?;
        self.availability
            .validate("catalog session availability", validate_availability)?;
        let mut referenced_evidence = Vec::new();
        self.native_identity
            .append_evidence_keys(&mut referenced_evidence);
        self.title.append_evidence_keys(&mut referenced_evidence);
        self.first_user_summary
            .append_evidence_keys(&mut referenced_evidence);
        self.native_created_at
            .append_evidence_keys(&mut referenced_evidence);
        self.native_updated_at
            .append_evidence_keys(&mut referenced_evidence);
        self.native_message_count
            .append_evidence_keys(&mut referenced_evidence);
        self.availability
            .append_evidence_keys(&mut referenced_evidence);
        validate_assertion_membership(&self.assertion_keys, referenced_evidence)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum CatalogCount {
    Known { value: u64 },
    Unknown { reason: QualifiedUnknownReason },
}

impl CatalogCount {
    pub(crate) fn known(value: u64) -> Result<Self, CatalogContractError> {
        validate_portable_u64("catalog count", value)?;
        Ok(Self::Known { value })
    }

    pub(crate) const fn unknown(reason: QualifiedUnknownReason) -> Self {
        Self::Unknown { reason }
    }

    fn validate(&self) -> Result<(), CatalogContractError> {
        match self {
            Self::Known { value } => validate_portable_u64("catalog count", *value),
            Self::Unknown { .. } => Ok(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CatalogPageRequestBinding {
    pub contract_selection: CatalogQueryContractSelection,
    pub snapshot_id: CatalogSnapshotId,
    pub query_kind: CatalogQueryKind,
    pub query_fingerprint: CatalogQueryFingerprint,
    pub sort_spec_version: u32,
    pub page_size: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after_cursor: Option<CatalogCursor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CatalogPortableCoveragePlanSource {
    pub adapter_id: String,
    pub source_instance_key: CanonicalSourceInstanceKey,
    pub support_release_id: String,
    pub catalog_declaration_digest: CoverageDeclarationDigest,
    pub access_policy_digest: CatalogAccessPolicyDigest,
    pub catalog_coverage_binding_digest: CoverageDeclarationDigest,
}

impl From<&CatalogCoveragePlanSource> for CatalogPortableCoveragePlanSource {
    fn from(source: &CatalogCoveragePlanSource) -> Self {
        Self {
            adapter_id: source.adapter_id.clone(),
            source_instance_key: source.source_instance_key,
            support_release_id: source.support_release_id.clone(),
            catalog_declaration_digest: source.catalog_declaration_digest,
            access_policy_digest: source.access_policy_digest,
            catalog_coverage_binding_digest: source.coverage_binding_digest(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CatalogPortableCoveragePlan {
    pub catalog_portable_coverage_plan_contract_version: u32,
    pub coverage_plan_contract_version: u32,
    pub coverage_plan_id: super::CatalogCoveragePlanId,
    pub scope: CatalogCoverageScope,
    pub required_sources: Vec<CatalogPortableCoveragePlanSource>,
    pub optional_sources: Vec<CatalogPortableCoveragePlanSource>,
}

impl CatalogPortableCoveragePlan {
    pub(crate) fn from_plan(plan: &CatalogCoveragePlan) -> Result<Self, CatalogContractError> {
        plan.validate()?;
        Ok(Self {
            catalog_portable_coverage_plan_contract_version:
                CATALOG_PORTABLE_COVERAGE_PLAN_CONTRACT_VERSION,
            coverage_plan_contract_version: CATALOG_COVERAGE_PLAN_CONTRACT_VERSION,
            coverage_plan_id: plan.coverage_plan_id,
            scope: plan.scope,
            required_sources: plan.required_sources.iter().map(Into::into).collect(),
            optional_sources: plan.optional_sources.iter().map(Into::into).collect(),
        })
    }
}

impl CatalogPageRequestBinding {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        contract_selection: CatalogQueryContractSelection,
        snapshot_id: CatalogSnapshotId,
        query_kind: CatalogQueryKind,
        query_fingerprint: CatalogQueryFingerprint,
        sort_spec_version: u32,
        page_size: u32,
        after_cursor: Option<CatalogCursor>,
    ) -> Result<Self, CatalogContractError> {
        let value = Self {
            contract_selection,
            snapshot_id,
            query_kind,
            query_fingerprint,
            sort_spec_version,
            page_size,
            after_cursor,
        };
        value.validate()?;
        Ok(value)
    }

    fn validate(&self) -> Result<(), CatalogContractError> {
        self.contract_selection.validate()?;
        self.snapshot_id.validate()?;
        validate_portable_u64(
            "catalog page readiness epoch",
            self.snapshot_id.readiness_epoch,
        )?;
        validate_portable_u64(
            "catalog page complete commit",
            self.snapshot_id.complete_commit,
        )?;
        if self.contract_selection.contract_versions.query_pack_version
            != Some(self.snapshot_id.pack_contract_version)
        {
            return Err(CatalogContractError::invalid(
                "catalog page snapshot does not match the negotiated query pack",
            ));
        }
        if self.sort_spec_version == 0 {
            return Err(CatalogContractError::invalid(
                "catalog page sort specification version must be greater than zero",
            ));
        }
        if self.page_size == 0 || self.page_size > MAX_CONTINUATION_PAGE_SIZE {
            return Err(CatalogContractError::invalid(format!(
                "catalog page size must be 1..={MAX_CONTINUATION_PAGE_SIZE}"
            )));
        }
        if let Some(cursor) = &self.after_cursor {
            cursor.validate_binding(
                self.snapshot_id,
                self.query_fingerprint,
                self.sort_spec_version,
            )?;
        }
        Ok(())
    }
}

fn validate_source_coverage_portable(
    coverage: &[SourceCoverageSet],
) -> Result<(), CatalogContractError> {
    let mut member_count = 0_usize;
    for set in coverage {
        let set_member_count = set
            .points
            .len()
            .checked_add(set.explicit_absence_or_deletion.len())
            .and_then(|count| count.checked_add(set.explicit_errors.len()))
            .ok_or_else(|| CatalogContractError::invalid("catalog coverage size overflow"))?;
        if set.points.len() > MAX_SOURCE_COVERAGE_MEMBERS
            || set.explicit_absence_or_deletion.len() > MAX_SOURCE_COVERAGE_MEMBERS
            || set.explicit_errors.len() > MAX_SOURCE_COVERAGE_MEMBERS
            || set_member_count > MAX_SOURCE_COVERAGE_MEMBERS
        {
            return Err(CatalogContractError::invalid(format!(
                "catalog source coverage set exceeds {MAX_SOURCE_COVERAGE_MEMBERS} members"
            )));
        }
        member_count = member_count
            .checked_add(set_member_count)
            .ok_or_else(|| CatalogContractError::invalid("catalog coverage size overflow"))?;
        if !set.points.windows(2).all(|pair| {
            (&pair[0].stream_key, &pair[0].object_key, pair[0].generation)
                < (&pair[1].stream_key, &pair[1].object_key, pair[1].generation)
        }) || !set
            .explicit_absence_or_deletion
            .windows(2)
            .all(|pair| pair[0] < pair[1])
            || !set.explicit_errors.windows(2).all(|pair| pair[0] < pair[1])
        {
            return Err(CatalogContractError::invalid(
                "catalog source coverage members must be canonical and duplicate-free",
            ));
        }
        set.validate().map_err(|error| {
            CatalogContractError::invalid(format!(
                "invalid portable catalog source coverage: {error}"
            ))
        })?;
        for point in &set.points {
            validate_positive_portable_u64("catalog coverage generation", point.generation)?;
            if let crate::adapter::CoverageStatus::Unavailable { reason } = &point.status {
                if reason.is_empty()
                    || reason.trim() != reason
                    || reason.len() > MAX_PORTABLE_COVERAGE_REASON_BYTES
                {
                    return Err(CatalogContractError::invalid(format!(
                        "catalog coverage unavailable reason must be canonical and at most {MAX_PORTABLE_COVERAGE_REASON_BYTES} bytes"
                    )));
                }
            }
            if let Some(position) = &point.position {
                if let Some(order) = position.monotonic_order {
                    validate_portable_u64("catalog coverage monotonic order", order)?;
                }
            }
            if let Some(observed_at) = point.provenance.observed_at {
                validate_portable_i64("catalog coverage observation time", observed_at)?;
            }
        }
        for absence in &set.explicit_absence_or_deletion {
            validate_positive_portable_u64(
                "catalog coverage absence generation",
                absence.generation,
            )?;
        }
        for error in &set.explicit_errors {
            validate_identifier("catalog coverage error code", &error.code)?;
        }
    }
    if member_count > MAX_SOURCE_COVERAGE_MEMBERS {
        return Err(CatalogContractError::invalid(format!(
            "catalog page source coverage exceeds {MAX_SOURCE_COVERAGE_MEMBERS} members"
        )));
    }
    Ok(())
}

fn validate_readiness_portable(
    readiness: &CatalogReadinessSnapshot,
    plan: &CatalogCoveragePlan,
) -> Result<(), CatalogContractError> {
    readiness.validate_against(plan)?;
    validate_portable_u64("catalog readiness epoch", readiness.epoch)?;
    validate_portable_u64("catalog readiness attempt", readiness.attempt)?;
    if let Some(commit) = readiness.complete_through_commit {
        validate_portable_u64("catalog complete-through commit", commit)?;
    }
    for snapshot in [
        readiness.last_complete_snapshot,
        readiness.refreshing_from_snapshot,
    ]
    .into_iter()
    .flatten()
    {
        validate_portable_u64("catalog snapshot readiness epoch", snapshot.readiness_epoch)?;
        validate_portable_u64("catalog snapshot complete commit", snapshot.complete_commit)?;
    }
    validate_source_coverage_portable(&readiness.source_coverage)
}

fn validate_published_readiness(
    readiness: &CatalogReadinessSnapshot,
    snapshot_id: CatalogSnapshotId,
    plan: &CatalogCoveragePlan,
) -> Result<(), CatalogContractError> {
    validate_readiness_portable(readiness, plan)?;
    if readiness.state != CatalogReadinessPhase::Ready
        || readiness.last_complete_snapshot != Some(snapshot_id)
        || readiness.complete_through_commit != Some(snapshot_id.complete_commit)
        || readiness.coverage_plan_id != snapshot_id.coverage_plan_id
        || readiness.epoch != snapshot_id.readiness_epoch
        || readiness.completed_contract_version != Some(snapshot_id.pack_contract_version)
        || readiness.reason.is_some()
        || readiness.refreshing_from_snapshot.is_some()
    {
        return Err(CatalogContractError::invalid(
            "catalog page publication readiness must describe exactly its immutable complete snapshot",
        ));
    }
    Ok(())
}

fn validate_additive_fields(
    fields: &BTreeMap<String, JsonValue>,
    selection: &CatalogQueryContractSelection,
    reserved: &[&str],
) -> Result<(), CatalogContractError> {
    if fields
        .keys()
        .any(|key| reserved.iter().any(|reserved| key == reserved))
    {
        return Err(CatalogContractError::invalid(
            "catalog additive fields cannot replace a response contract field",
        ));
    }
    validate_typed_unknown_fields(fields, &selection.typed_unknown)
}

pub(crate) trait CatalogPortableRow: Clone + PartialEq + Eq + Serialize {
    const QUERY_KIND: CatalogQueryKind;

    fn entity_ref(&self) -> CatalogEntityRef;
    fn validate_row(&self) -> Result<(), CatalogContractError>;
}

impl CatalogPortableRow for CatalogPortableProjectRow {
    const QUERY_KIND: CatalogQueryKind = CatalogQueryKind::Projects;

    fn entity_ref(&self) -> CatalogEntityRef {
        self.project_ref
    }

    fn validate_row(&self) -> Result<(), CatalogContractError> {
        self.validate()
    }
}

impl CatalogPortableRow for CatalogPortableSessionRow {
    const QUERY_KIND: CatalogQueryKind = CatalogQueryKind::Sessions;

    fn entity_ref(&self) -> CatalogEntityRef {
        self.session_ref
    }

    fn validate_row(&self) -> Result<(), CatalogContractError> {
        self.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CatalogPageEntry<R> {
    pub sort_key: CatalogSortKey,
    pub row: R,
}

impl<R: CatalogPortableRow> CatalogPageEntry<R> {
    pub(crate) fn new(sort_key: CatalogSortKey, row: R) -> Result<Self, CatalogContractError> {
        let value = Self { sort_key, row };
        value.row.validate_row()?;
        Ok(value)
    }

    fn key(&self) -> (&CatalogSortKey, crate::adapter::CanonicalEntityKey) {
        (
            &self.sort_key,
            self.row.entity_ref().external_ref.entity_key,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CatalogPage<R> {
    pub catalog_page_contract_version: u32,
    pub request: CatalogPageRequestBinding,
    pub published_readiness: CatalogReadinessSnapshot,
    pub total_count: CatalogCount,
    pub has_more: bool,
    pub rows: Vec<CatalogPageEntry<R>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_continuation: Option<CatalogContinuationRequest>,
    #[serde(flatten)]
    pub additive_fields: BTreeMap<String, JsonValue>,
}

pub(crate) type CatalogProjectPage = CatalogPage<CatalogPortableProjectRow>;
pub(crate) type CatalogSessionPage = CatalogPage<CatalogPortableSessionRow>;

impl<R: CatalogPortableRow> CatalogPage<R> {
    #[allow(clippy::too_many_arguments)]
    fn new(
        request: CatalogPageRequestBinding,
        published_readiness: CatalogReadinessSnapshot,
        total_count: CatalogCount,
        has_more: bool,
        rows: Vec<CatalogPageEntry<R>>,
        next_continuation: Option<CatalogContinuationRequest>,
        additive_fields: BTreeMap<String, JsonValue>,
        plan: &CatalogCoveragePlan,
    ) -> Result<Self, CatalogContractError> {
        let value = Self {
            catalog_page_contract_version: CATALOG_PAGE_CONTRACT_VERSION,
            request,
            published_readiness,
            total_count,
            has_more,
            rows,
            next_continuation,
            additive_fields,
        };
        let expected_request = value.request.clone();
        value.validate_for_request(&expected_request, plan)?;
        Ok(value)
    }

    pub(crate) fn validate_for_request(
        &self,
        expected_request: &CatalogPageRequestBinding,
        plan: &CatalogCoveragePlan,
    ) -> Result<(), CatalogContractError> {
        if self.catalog_page_contract_version != CATALOG_PAGE_CONTRACT_VERSION {
            return Err(CatalogContractError::invalid(format!(
                "unsupported catalog page contract version {}",
                self.catalog_page_contract_version
            )));
        }
        expected_request.validate()?;
        self.request.validate()?;
        if self.request != *expected_request {
            return Err(CatalogContractError::invalid(
                "catalog page response does not match the caller-held request binding",
            ));
        }
        if self.request.query_kind != R::QUERY_KIND {
            return Err(CatalogContractError::invalid(
                "catalog page row type does not match the bound query kind",
            ));
        }
        validate_published_readiness(&self.published_readiness, self.request.snapshot_id, plan)?;
        self.total_count.validate()?;
        if self.rows.len() > self.request.page_size as usize
            || self.rows.len() > MAX_CONTINUATION_PAGE_SIZE as usize
        {
            return Err(CatalogContractError::invalid(
                "catalog page row count exceeds its requested bound",
            ));
        }
        for entry in &self.rows {
            entry.row.validate_row()?;
        }
        if !self
            .rows
            .windows(2)
            .all(|pair| pair[0].key() < pair[1].key())
        {
            return Err(CatalogContractError::invalid(
                "catalog page row keys must be strictly increasing and duplicate-free",
            ));
        }
        let mut entity_keys = BTreeSet::new();
        if self
            .rows
            .iter()
            .any(|entry| !entity_keys.insert(entry.row.entity_ref().external_ref.entity_key))
        {
            return Err(CatalogContractError::invalid(
                "catalog page cannot repeat an entity under a different sort key",
            ));
        }
        if let (Some(after), Some(first)) = (&self.request.after_cursor, self.rows.first()) {
            let after_key = (&after.last_sort_key, after.last_entity_key);
            if after_key >= first.key() {
                return Err(CatalogContractError::invalid(
                    "catalog continuation rows must begin strictly after the caller-held cursor",
                ));
            }
        }
        if let CatalogCount::Known { value } = self.total_count {
            let returned = self.rows.len() as u64;
            if value < returned
                || ((self.has_more || self.request.after_cursor.is_some()) && value <= returned)
            {
                return Err(CatalogContractError::invalid(
                    "catalog total count is inconsistent with page progress",
                ));
            }
        }
        if self.has_more != self.next_continuation.is_some() {
            return Err(CatalogContractError::invalid(
                "catalog page next continuation must be present exactly when more rows exist",
            ));
        }
        if let Some(next) = &self.next_continuation {
            let Some(last) = self.rows.last() else {
                return Err(CatalogContractError::invalid(
                    "an empty catalog page cannot issue a continuation cursor",
                ));
            };
            next.validate_for_selection(&self.request.contract_selection)?;
            if next.snapshot_id != self.request.snapshot_id
                || next.query_fingerprint != self.request.query_fingerprint
                || next.sort_spec_version != self.request.sort_spec_version
                || next.page_size != self.request.page_size
                || next.cursor.last_sort_key != last.sort_key
                || next.cursor.last_entity_key != last.row.entity_ref().external_ref.entity_key
            {
                return Err(CatalogContractError::invalid(
                    "catalog next continuation does not bind the exact page and canonical final row",
                ));
            }
        }
        validate_additive_fields(
            &self.additive_fields,
            &self.request.contract_selection,
            &[
                "catalog_page_contract_version",
                "request",
                "published_readiness",
                "total_count",
                "has_more",
                "rows",
                "next_continuation",
            ],
        )
    }

    pub(crate) fn to_wire_value(
        &self,
        expected_request: &CatalogPageRequestBinding,
        plan: &CatalogCoveragePlan,
    ) -> Result<JsonValue, CatalogContractError> {
        self.validate_for_request(expected_request, plan)?;
        serde_json::to_value(self).map_err(|error| CatalogContractError::invalid(error.to_string()))
    }
}

impl CatalogProjectPage {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_projects(
        request: CatalogPageRequestBinding,
        published_readiness: CatalogReadinessSnapshot,
        total_count: CatalogCount,
        has_more: bool,
        rows: Vec<CatalogPageEntry<CatalogPortableProjectRow>>,
        next_continuation: Option<CatalogContinuationRequest>,
        additive_fields: BTreeMap<String, JsonValue>,
        plan: &CatalogCoveragePlan,
    ) -> Result<Self, CatalogContractError> {
        Self::new(
            request,
            published_readiness,
            total_count,
            has_more,
            rows,
            next_continuation,
            additive_fields,
            plan,
        )
    }
}

impl CatalogSessionPage {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_sessions(
        request: CatalogPageRequestBinding,
        published_readiness: CatalogReadinessSnapshot,
        total_count: CatalogCount,
        has_more: bool,
        rows: Vec<CatalogPageEntry<CatalogPortableSessionRow>>,
        next_continuation: Option<CatalogContinuationRequest>,
        additive_fields: BTreeMap<String, JsonValue>,
        plan: &CatalogCoveragePlan,
    ) -> Result<Self, CatalogContractError> {
        Self::new(
            request,
            published_readiness,
            total_count,
            has_more,
            rows,
            next_continuation,
            additive_fields,
            plan,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CatalogReadinessResponse {
    pub catalog_readiness_response_contract_version: u32,
    pub contract_selection: CatalogQueryContractSelection,
    pub readiness: CatalogReadinessSnapshot,
    #[serde(flatten)]
    pub additive_fields: BTreeMap<String, JsonValue>,
}

impl CatalogReadinessResponse {
    pub(crate) fn new(
        contract_selection: CatalogQueryContractSelection,
        readiness: CatalogReadinessSnapshot,
        additive_fields: BTreeMap<String, JsonValue>,
        plan: &CatalogCoveragePlan,
    ) -> Result<Self, CatalogContractError> {
        let value = Self {
            catalog_readiness_response_contract_version:
                CATALOG_READINESS_RESPONSE_CONTRACT_VERSION,
            contract_selection,
            readiness,
            additive_fields,
        };
        let expected_selection = value.contract_selection.clone();
        value.validate_for_request(&expected_selection, plan)?;
        Ok(value)
    }

    pub(crate) fn validate_for_request(
        &self,
        expected_selection: &CatalogQueryContractSelection,
        expected_plan: &CatalogCoveragePlan,
    ) -> Result<(), CatalogContractError> {
        if self.catalog_readiness_response_contract_version
            != CATALOG_READINESS_RESPONSE_CONTRACT_VERSION
        {
            return Err(CatalogContractError::invalid(format!(
                "unsupported catalog readiness response contract version {}",
                self.catalog_readiness_response_contract_version
            )));
        }
        expected_selection.validate()?;
        self.contract_selection.validate()?;
        if self.contract_selection != *expected_selection {
            return Err(CatalogContractError::invalid(
                "catalog readiness response does not match the negotiated contract selection",
            ));
        }
        validate_readiness_portable(&self.readiness, expected_plan)?;
        if self.contract_selection.contract_versions.query_pack_version
            != Some(self.readiness.desired_contract_version)
        {
            return Err(CatalogContractError::invalid(
                "catalog readiness desired pack does not match the negotiated query pack",
            ));
        }
        validate_additive_fields(
            &self.additive_fields,
            &self.contract_selection,
            &[
                "catalog_readiness_response_contract_version",
                "contract_selection",
                "readiness",
            ],
        )
    }

    pub(crate) fn to_wire_value(
        &self,
        expected_selection: &CatalogQueryContractSelection,
        expected_plan: &CatalogCoveragePlan,
    ) -> Result<JsonValue, CatalogContractError> {
        self.validate_for_request(expected_selection, expected_plan)?;
        serde_json::to_value(self).map_err(|error| CatalogContractError::invalid(error.to_string()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "row", rename_all = "snake_case")]
pub(crate) enum CatalogPortableLiveRow {
    Project(CatalogPortableProjectRow),
    Session(CatalogPortableSessionRow),
}

impl CatalogPortableLiveRow {
    fn from_evidence(
        row: &CatalogLiveRow,
        view: CatalogPolicyView,
    ) -> Result<Self, CatalogContractError> {
        match row {
            CatalogLiveRow::Project(row) => {
                CatalogPortableProjectRow::from_evidence(row, view).map(Self::Project)
            }
            CatalogLiveRow::Session(row) => {
                CatalogPortableSessionRow::from_evidence(row, view).map(Self::Session)
            }
        }
    }

    fn entity_ref(&self) -> CatalogEntityRef {
        match self {
            Self::Project(row) => row.project_ref,
            Self::Session(row) => row.session_ref,
        }
    }

    fn validate(&self) -> Result<(), CatalogContractError> {
        match self {
            Self::Project(row) => row.validate(),
            Self::Session(row) => row.validate(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum CatalogEntityResolution {
    Live {
        external_ref: ExternalEntityRef,
        row: Box<CatalogPortableLiveRow>,
    },
    Tombstoned {
        external_ref: ExternalEntityRef,
        provenance: Vec<SemanticRevisionRef>,
    },
    Superseded {
        external_ref: ExternalEntityRef,
        target_refs: Vec<ExternalEntityRef>,
        provenance: Vec<SemanticRevisionRef>,
    },
    Unknown {
        external_ref: ExternalEntityRef,
        reason: CatalogUnknownReferenceReason,
    },
    TypedUnknown {
        external_ref: ExternalEntityRef,
        variant: String,
        payload: BTreeMap<String, JsonValue>,
    },
}

impl CatalogEntityResolution {
    pub(crate) fn from_evidence(
        requested_ref: ExternalEntityRef,
        resolved: &CatalogResolvedEntity,
        view: CatalogPolicyView,
    ) -> Result<Self, CatalogContractError> {
        let value = match resolved {
            CatalogResolvedEntity::Live { entity_ref, row } => {
                if entity_ref.external_ref != requested_ref {
                    return Err(CatalogContractError::invalid(
                        "live catalog resolution does not match the requested external reference",
                    ));
                }
                let portable = CatalogPortableLiveRow::from_evidence(row, view)?;
                if portable.entity_ref() != *entity_ref {
                    return Err(CatalogContractError::invalid(
                        "live catalog resolution row does not match its typed entity reference",
                    ));
                }
                Self::Live {
                    external_ref: requested_ref,
                    row: Box::new(portable),
                }
            }
            CatalogResolvedEntity::Tombstoned { tombstone } => {
                if tombstone.entity_ref.external_ref != requested_ref {
                    return Err(CatalogContractError::invalid(
                        "catalog tombstone does not match the requested external reference",
                    ));
                }
                let mut provenance = tombstone.provenance.clone();
                canonicalize_provenance(&mut provenance);
                Self::Tombstoned {
                    external_ref: requested_ref,
                    provenance,
                }
            }
            CatalogResolvedEntity::Superseded {
                prior_ref,
                replacement_refs,
                provenance,
                ..
            } => {
                if prior_ref.external_ref != requested_ref {
                    return Err(CatalogContractError::invalid(
                        "superseded catalog resolution does not match the requested external reference",
                    ));
                }
                let mut target_refs: Vec<_> = replacement_refs
                    .iter()
                    .map(|reference| reference.external_ref)
                    .collect();
                target_refs.sort_by_key(|reference| {
                    (
                        reference.external_entity_reference_version,
                        reference.entity_key,
                    )
                });
                target_refs.dedup();
                let mut provenance = provenance.clone();
                canonicalize_provenance(&mut provenance);
                Self::Superseded {
                    external_ref: requested_ref,
                    target_refs,
                    provenance,
                }
            }
            CatalogResolvedEntity::Unknown {
                requested_ref: resolved_ref,
                reason,
                ..
            } => {
                if *resolved_ref != requested_ref {
                    return Err(CatalogContractError::invalid(
                        "unknown catalog resolution does not match the requested external reference",
                    ));
                }
                Self::Unknown {
                    external_ref: requested_ref,
                    reason: *reason,
                }
            }
        };
        value.validate_for_request(requested_ref, None)?;
        Ok(value)
    }

    pub(crate) fn typed_unknown(
        external_ref: ExternalEntityRef,
        variant: impl Into<String>,
        payload: BTreeMap<String, JsonValue>,
        selection: &CatalogQueryContractSelection,
    ) -> Result<Self, CatalogContractError> {
        let value = Self::TypedUnknown {
            external_ref,
            variant: variant.into(),
            payload,
        };
        value.validate_for_request(external_ref, Some(selection))?;
        Ok(value)
    }

    fn external_ref(&self) -> ExternalEntityRef {
        match self {
            Self::Live { external_ref, .. }
            | Self::Tombstoned { external_ref, .. }
            | Self::Superseded { external_ref, .. }
            | Self::Unknown { external_ref, .. }
            | Self::TypedUnknown { external_ref, .. } => *external_ref,
        }
    }

    fn validate_for_request(
        &self,
        requested_ref: ExternalEntityRef,
        selection: Option<&CatalogQueryContractSelection>,
    ) -> Result<(), CatalogContractError> {
        if requested_ref.external_entity_reference_version != EXTERNAL_ENTITY_REFERENCE_VERSION
            || self.external_ref() != requested_ref
        {
            return Err(CatalogContractError::invalid(
                "catalog resolution must preserve the exact requested external reference",
            ));
        }
        match self {
            Self::Live { row, .. } => {
                row.validate()?;
                if row.entity_ref().external_ref != requested_ref {
                    return Err(CatalogContractError::invalid(
                        "live catalog resolution row does not match its external reference",
                    ));
                }
            }
            Self::Tombstoned { provenance, .. } => {
                validate_provenance("catalog tombstone resolution", provenance)?;
            }
            Self::Superseded {
                target_refs,
                provenance,
                ..
            } => {
                if target_refs.is_empty() || target_refs.len() > MAX_RESOLUTION_TARGETS {
                    return Err(CatalogContractError::invalid(format!(
                        "superseded catalog resolution requires 1..={MAX_RESOLUTION_TARGETS} targets"
                    )));
                }
                if target_refs.iter().any(|reference| {
                    reference.external_entity_reference_version != EXTERNAL_ENTITY_REFERENCE_VERSION
                        || *reference == requested_ref
                }) || !target_refs.windows(2).all(|pair| {
                    (
                        pair[0].external_entity_reference_version,
                        pair[0].entity_key,
                    ) < (
                        pair[1].external_entity_reference_version,
                        pair[1].entity_key,
                    )
                }) {
                    return Err(CatalogContractError::invalid(
                        "superseded catalog targets must be compatible, canonical, distinct replacements",
                    ));
                }
                validate_provenance("superseded catalog resolution", provenance)?;
            }
            Self::Unknown { .. } => {}
            Self::TypedUnknown {
                variant, payload, ..
            } => {
                validate_identifier("catalog typed-unknown resolution variant", variant)?;
                if matches!(
                    variant.as_str(),
                    "live" | "tombstoned" | "superseded" | "unknown" | "typed_unknown"
                ) {
                    return Err(CatalogContractError::invalid(
                        "catalog typed-unknown resolution cannot shadow a known state",
                    ));
                }
                let Some(selection) = selection else {
                    return Err(CatalogContractError::invalid(
                        "catalog typed-unknown resolution requires its negotiated selection",
                    ));
                };
                validate_typed_unknown_fields(payload, &selection.typed_unknown)?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CatalogResolutionRequestBinding {
    pub contract_selection: CatalogQueryContractSelection,
    pub snapshot_id: CatalogSnapshotId,
    pub external_ref: ExternalEntityRef,
}

impl CatalogResolutionRequestBinding {
    pub(crate) fn new(
        contract_selection: CatalogQueryContractSelection,
        snapshot_id: CatalogSnapshotId,
        external_ref: ExternalEntityRef,
    ) -> Result<Self, CatalogContractError> {
        let value = Self {
            contract_selection,
            snapshot_id,
            external_ref,
        };
        value.validate()?;
        Ok(value)
    }

    fn validate(&self) -> Result<(), CatalogContractError> {
        self.contract_selection.validate()?;
        self.snapshot_id.validate()?;
        validate_portable_u64(
            "catalog resolution snapshot epoch",
            self.snapshot_id.readiness_epoch,
        )?;
        validate_portable_u64(
            "catalog resolution snapshot commit",
            self.snapshot_id.complete_commit,
        )?;
        if self.contract_selection.contract_versions.query_pack_version
            != Some(self.snapshot_id.pack_contract_version)
            || self.external_ref.external_entity_reference_version
                != EXTERNAL_ENTITY_REFERENCE_VERSION
        {
            return Err(CatalogContractError::invalid(
                "catalog resolution request has an incompatible snapshot or external reference",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CatalogEntityResolutionResponse {
    pub catalog_resolution_contract_version: u32,
    pub request: CatalogResolutionRequestBinding,
    pub resolution: CatalogEntityResolution,
    #[serde(flatten)]
    pub additive_fields: BTreeMap<String, JsonValue>,
}

impl CatalogEntityResolutionResponse {
    pub(crate) fn new(
        request: CatalogResolutionRequestBinding,
        resolution: CatalogEntityResolution,
        additive_fields: BTreeMap<String, JsonValue>,
    ) -> Result<Self, CatalogContractError> {
        let value = Self {
            catalog_resolution_contract_version: CATALOG_RESOLUTION_CONTRACT_VERSION,
            request,
            resolution,
            additive_fields,
        };
        let expected_request = value.request.clone();
        value.validate_for_request(&expected_request)?;
        Ok(value)
    }

    pub(crate) fn validate_for_request(
        &self,
        expected_request: &CatalogResolutionRequestBinding,
    ) -> Result<(), CatalogContractError> {
        if self.catalog_resolution_contract_version != CATALOG_RESOLUTION_CONTRACT_VERSION {
            return Err(CatalogContractError::invalid(format!(
                "unsupported catalog resolution contract version {}",
                self.catalog_resolution_contract_version
            )));
        }
        expected_request.validate()?;
        self.request.validate()?;
        if self.request != *expected_request {
            return Err(CatalogContractError::invalid(
                "catalog resolution response does not match the caller-held request",
            ));
        }
        self.resolution.validate_for_request(
            self.request.external_ref,
            Some(&self.request.contract_selection),
        )?;
        validate_additive_fields(
            &self.additive_fields,
            &self.request.contract_selection,
            &[
                "catalog_resolution_contract_version",
                "request",
                "resolution",
            ],
        )
    }

    pub(crate) fn to_wire_value(
        &self,
        expected_request: &CatalogResolutionRequestBinding,
    ) -> Result<JsonValue, CatalogContractError> {
        self.validate_for_request(expected_request)?;
        serde_json::to_value(self).map_err(|error| CatalogContractError::invalid(error.to_string()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CatalogSnapshotRetention {
    Retained,
    Expired { latest_snapshot: CatalogSnapshotId },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CatalogSnapshotExpired {
    pub catalog_snapshot_expiration_contract_version: u32,
    pub contract_selection: CatalogQueryContractSelection,
    pub scope: CatalogCoverageScope,
    pub request: CatalogContinuationRequest,
    pub latest_snapshot: CatalogSnapshotId,
}

impl CatalogSnapshotExpired {
    fn new(
        request: CatalogContinuationRequest,
        scope: CatalogCoverageScope,
        latest_snapshot: CatalogSnapshotId,
    ) -> Result<Self, CatalogContractError> {
        let value = Self {
            catalog_snapshot_expiration_contract_version:
                CATALOG_SNAPSHOT_EXPIRATION_CONTRACT_VERSION,
            contract_selection: request.contract_selection.clone(),
            scope,
            request,
            latest_snapshot,
        };
        value.validate()?;
        Ok(value)
    }

    fn validate(&self) -> Result<(), CatalogContractError> {
        if self.catalog_snapshot_expiration_contract_version
            != CATALOG_SNAPSHOT_EXPIRATION_CONTRACT_VERSION
        {
            return Err(CatalogContractError::invalid(
                "unsupported catalog snapshot-expiration contract version",
            ));
        }
        self.scope.validate()?;
        self.request
            .validate_for_selection(&self.contract_selection)?;
        self.latest_snapshot.validate()?;
        validate_portable_u64(
            "latest catalog readiness epoch",
            self.latest_snapshot.readiness_epoch,
        )?;
        validate_portable_u64(
            "latest catalog complete commit",
            self.latest_snapshot.complete_commit,
        )?;
        let expired = self.request.snapshot_id;
        if self.latest_snapshot.pack_contract_version != expired.pack_contract_version
            || self.latest_snapshot.complete_commit <= expired.complete_commit
            || self.latest_snapshot.readiness_epoch < expired.readiness_epoch
            || (self.latest_snapshot.readiness_epoch == expired.readiness_epoch
                && self.latest_snapshot.coverage_plan_id != expired.coverage_plan_id)
        {
            return Err(CatalogContractError::invalid(
                "latest catalog snapshot must be strictly newer in the same scope and pack lineage",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CatalogContinuationDisposition {
    Continue(CatalogContinuationRequest),
    SnapshotExpired(CatalogSnapshotExpired),
}

pub(crate) fn validate_continuation_retention(
    wire_request: JsonValue,
    expected_request: &CatalogContinuationRequest,
    expected_scope: CatalogCoverageScope,
    retention: CatalogSnapshotRetention,
) -> Result<CatalogContinuationDisposition, CatalogContractError> {
    expected_request.validate_for_selection(&expected_request.contract_selection)?;
    expected_scope.validate()?;
    let parsed = CatalogContinuationRequest::from_wire_value(
        wire_request,
        &expected_request.contract_selection,
    )?;
    if parsed != *expected_request {
        return Err(CatalogContractError::invalid(
            "catalog continuation does not match the exact caller-held request",
        ));
    }
    match retention {
        CatalogSnapshotRetention::Retained => Ok(CatalogContinuationDisposition::Continue(parsed)),
        CatalogSnapshotRetention::Expired { latest_snapshot } => {
            CatalogSnapshotExpired::new(parsed, expected_scope, latest_snapshot)
                .map(CatalogContinuationDisposition::SnapshotExpired)
        }
    }
}

#[cfg(test)]
mod tests;
