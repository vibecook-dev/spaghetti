//! RFC 012B selected-session hydration command and scheduling-receipt contracts.
//!
//! This module is deliberately separate from read-only catalog queries and
//! contains no scheduler, source-access, persistence, or transport authority.

#[cfg(test)]
pub(crate) mod tests;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value as JsonValue;

use crate::adapter::{
    CoverageDeclarationDigest, SemanticRevisionRef, SEMANTIC_REFERENCE_CONTRACT_VERSION,
};

use super::evidence::{
    CatalogAttachTarget, CatalogDisclosureClass, CatalogEntityKind, CatalogEntityRef,
    CatalogLocatorKind, CatalogPolicyView, CatalogReducer, CatalogSessionAttachHandoff,
    ProjectAssociationBasis,
};
use super::query::CatalogQueryContractSelection;
use super::{
    contract_digest, validate_identifier, CatalogAccessPolicyDigest, CatalogContractError,
    CatalogCoveragePlan, CatalogCoveragePlanSource, CatalogHydrationAuthorizationId,
    CatalogHydrationCoalescingKey, CatalogHydrationCommandId, CatalogHydrationRequestKey,
    CatalogSchedulingReceiptId, CatalogSnapshotId,
};

pub(crate) const CATALOG_HYDRATION_COMMAND_CONTRACT_VERSION: u32 = 1;
pub(crate) const CATALOG_HYDRATION_AUTHORIZATION_CONTRACT_VERSION: u32 = 1;
pub(crate) const CATALOG_HYDRATION_SCOPE_CONTRACT_VERSION: u32 = 1;
pub(crate) const CATALOG_SCHEDULING_RECEIPT_CONTRACT_VERSION: u32 = 1;

const MAX_REQUEST_KEY_BYTES: usize = 1_024;
const MAX_SCOPE_FACT_FAMILIES: usize = 64;
const MAX_SCOPE_SOURCE_OBJECTS: u32 = 4_096;
const MAX_SCOPE_RECORDS: u32 = 1_000_000;
const MAX_SCOPE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_PROVENANCE_REVISIONS: usize = 64;
const MAX_RETRY_AFTER_MILLIS: u32 = 5 * 60 * 1_000;
const MAX_FAILURE_CODE_BYTES: usize = 64;
const MAX_JAVASCRIPT_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

fn serialize_digest_material(
    value: &impl Serialize,
    label: &str,
) -> Result<Vec<u8>, CatalogContractError> {
    serde_json::to_vec(value).map_err(|error| {
        CatalogContractError::invalid(format!("failed to encode {label}: {error}"))
    })
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
    let mut seen = BTreeSet::new();
    for reference in provenance {
        if reference.semantic_reference_contract_version != SEMANTIC_REFERENCE_CONTRACT_VERSION {
            return Err(CatalogContractError::invalid(format!(
                "{label} contains an incompatible semantic revision reference"
            )));
        }
        if !seen.insert(reference.fact_revision_id) {
            return Err(CatalogContractError::invalid(format!(
                "{label} contains duplicate semantic revision provenance"
            )));
        }
    }
    if provenance
        .windows(2)
        .any(|pair| pair[0].fact_revision_id >= pair[1].fact_revision_id)
    {
        return Err(CatalogContractError::invalid(format!(
            "{label} semantic revision provenance must be strictly canonical"
        )));
    }
    Ok(())
}

fn canonical_provenance(
    label: &str,
    mut provenance: Vec<SemanticRevisionRef>,
) -> Result<Vec<SemanticRevisionRef>, CatalogContractError> {
    provenance.sort_by_key(|reference| reference.fact_revision_id);
    validate_provenance(label, &provenance)?;
    Ok(provenance)
}

fn parse_portable_snapshot(value: JsonValue) -> Result<CatalogSnapshotId, CatalogContractError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Wire {
        pack_contract_version: u32,
        coverage_plan_id: super::CatalogCoveragePlanId,
        readiness_epoch: u64,
        complete_commit: u64,
    }

    let wire = serde_json::from_value::<Wire>(value)
        .map_err(|error| CatalogContractError::invalid(error.to_string()))?;
    CatalogSnapshotId::new(
        wire.pack_contract_version,
        wire.coverage_plan_id,
        wire.readiness_epoch,
        wire.complete_commit,
    )
}

fn parse_portable_source(
    value: JsonValue,
) -> Result<CatalogCoveragePlanSource, CatalogContractError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Wire {
        adapter_id: String,
        source_instance_key: crate::adapter::CanonicalSourceInstanceKey,
        support_release_id: String,
        catalog_declaration_digest: CoverageDeclarationDigest,
        access_policy_digest: CatalogAccessPolicyDigest,
    }

    let wire = serde_json::from_value::<Wire>(value)
        .map_err(|error| CatalogContractError::invalid(error.to_string()))?;
    CatalogCoveragePlanSource::new(
        wire.adapter_id,
        wire.source_instance_key,
        wire.support_release_id,
        wire.catalog_declaration_digest,
        wire.access_policy_digest,
    )
}

impl CatalogHydrationRequestKey {
    pub(crate) fn derive(stable_request_token: &[u8]) -> Result<Self, CatalogContractError> {
        if stable_request_token.is_empty() || stable_request_token.len() > MAX_REQUEST_KEY_BYTES {
            return Err(CatalogContractError::invalid(format!(
                "catalog hydration request token must contain 1..={MAX_REQUEST_KEY_BYTES} bytes"
            )));
        }
        Ok(Self::from_digest(contract_digest(
            b"catalog-hydration-request-key",
            &[stable_request_token],
        )))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CatalogHydrationReason {
    SelectedSession,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CatalogHydrationRequestedScope {
    pub hydration_scope_contract_version: u32,
    pub fact_family_versions: BTreeMap<String, u32>,
    pub max_source_objects_per_pass: u32,
    pub max_records_per_pass: u32,
    pub max_bytes_per_pass: u64,
}

impl CatalogHydrationRequestedScope {
    pub(crate) fn new(
        fact_family_versions: BTreeMap<String, u32>,
        max_source_objects_per_pass: u32,
        max_records_per_pass: u32,
        max_bytes_per_pass: u64,
        selection: &CatalogQueryContractSelection,
    ) -> Result<Self, CatalogContractError> {
        let value = Self {
            hydration_scope_contract_version: CATALOG_HYDRATION_SCOPE_CONTRACT_VERSION,
            fact_family_versions,
            max_source_objects_per_pass,
            max_records_per_pass,
            max_bytes_per_pass,
        };
        value.validate_for_selection(selection)?;
        Ok(value)
    }

    fn validate_for_selection(
        &self,
        selection: &CatalogQueryContractSelection,
    ) -> Result<(), CatalogContractError> {
        if self.hydration_scope_contract_version != CATALOG_HYDRATION_SCOPE_CONTRACT_VERSION {
            return Err(CatalogContractError::invalid(format!(
                "unsupported catalog hydration scope contract version {}",
                self.hydration_scope_contract_version
            )));
        }
        if self.fact_family_versions.is_empty()
            || self.fact_family_versions.len() > MAX_SCOPE_FACT_FAMILIES
        {
            return Err(CatalogContractError::invalid(format!(
                "catalog hydration scope requires 1..={MAX_SCOPE_FACT_FAMILIES} fact families"
            )));
        }
        for (family, version) in &self.fact_family_versions {
            validate_identifier("catalog hydration fact family", family)?;
            if matches!(family.as_str(), "__proto__" | "prototype" | "constructor") {
                return Err(CatalogContractError::invalid(format!(
                    "catalog hydration fact family {family:?} is reserved"
                )));
            }
            if *version == 0
                || selection.contract_versions.fact_family_versions.get(family) != Some(version)
            {
                return Err(CatalogContractError::invalid(format!(
                    "catalog hydration fact family {family:?} is not selected at the requested version"
                )));
            }
        }
        if self.max_source_objects_per_pass == 0
            || self.max_source_objects_per_pass > MAX_SCOPE_SOURCE_OBJECTS
            || self.max_records_per_pass == 0
            || self.max_records_per_pass > MAX_SCOPE_RECORDS
            || self.max_bytes_per_pass == 0
            || self.max_bytes_per_pass > MAX_SCOPE_BYTES
        {
            return Err(CatalogContractError::invalid(format!(
                "catalog hydration bounds must be within objects=1..={MAX_SCOPE_SOURCE_OBJECTS}, records=1..={MAX_SCOPE_RECORDS}, bytes=1..={MAX_SCOPE_BYTES}"
            )));
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for CatalogHydrationRequestedScope {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            hydration_scope_contract_version: u32,
            fact_family_versions: BTreeMap<String, u32>,
            max_source_objects_per_pass: u32,
            max_records_per_pass: u32,
            max_bytes_per_pass: u64,
        }

        let wire = Wire::deserialize(deserializer)?;
        Ok(Self {
            hydration_scope_contract_version: wire.hydration_scope_contract_version,
            fact_family_versions: wire.fact_family_versions,
            max_source_objects_per_pass: wire.max_source_objects_per_pass,
            max_records_per_pass: wire.max_records_per_pass,
            max_bytes_per_pass: wire.max_bytes_per_pass,
        })
    }
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CatalogHydrationLocatorAuthorization {
    pub hydration_authorization_contract_version: u32,
    pub authorization_id: CatalogHydrationAuthorizationId,
    pub handoff: CatalogSessionAttachHandoff,
    pub adapter_id: String,
    pub source_instance_key: crate::adapter::CanonicalSourceInstanceKey,
    pub support_release_id: String,
    pub catalog_declaration_digest: CoverageDeclarationDigest,
    pub access_policy_digest: CatalogAccessPolicyDigest,
    pub locator_source_generation: u64,
    pub locator_kind: CatalogLocatorKind,
    pub locator_basis: ProjectAssociationBasis,
    pub locator_disclosure: CatalogDisclosureClass,
    pub locator_provenance: Vec<SemanticRevisionRef>,
}

impl fmt::Debug for CatalogHydrationLocatorAuthorization {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CatalogHydrationLocatorAuthorization")
            .field("authorization_id", &self.authorization_id)
            .field(
                "selected_base_session_ref",
                &self.handoff.selected_base_session_ref,
            )
            .field("locator_claim_key", &self.handoff.locator_claim_key)
            .field("adapter_id", &self.adapter_id)
            .field("source_instance_key", &self.source_instance_key)
            .field("support_release_id", &self.support_release_id)
            .field(
                "catalog_declaration_digest",
                &self.catalog_declaration_digest,
            )
            .field("access_policy_digest", &self.access_policy_digest)
            .field("locator_source_generation", &self.locator_source_generation)
            .field("locator_kind", &self.locator_kind)
            .field("locator_basis", &self.locator_basis)
            .field("locator_disclosure", &self.locator_disclosure)
            .finish_non_exhaustive()
    }
}

impl CatalogHydrationLocatorAuthorization {
    pub(crate) fn authorize(
        reducer: &CatalogReducer,
        handoff: CatalogSessionAttachHandoff,
        policy_view: CatalogPolicyView,
        source: &CatalogCoveragePlanSource,
    ) -> Result<Self, CatalogContractError> {
        Ok(CatalogHydrationExecutionAuthorization::authorize(
            reducer,
            handoff,
            policy_view,
            source,
        )?
        .portable)
    }

    fn from_attach_target(
        handoff: CatalogSessionAttachHandoff,
        target: CatalogAttachTarget,
        source: &CatalogCoveragePlanSource,
    ) -> Result<Self, CatalogContractError> {
        source.validate()?;
        if target.session_ref != handoff.selected_base_session_ref
            || target.locator_claim_key != handoff.locator_claim_key
            || target.locator_owner.adapter_id != source.adapter_id
            || target.locator_owner.source_instance_key != source.source_instance_key
        {
            return Err(CatalogContractError::invalid(
                "catalog hydration attach evidence does not match the selected coverage-plan source",
            ));
        }
        let locator_value = target.locator.value.as_ref().ok_or_else(|| {
            CatalogContractError::invalid(
                "catalog hydration requires policy-authorized concrete locator evidence",
            )
        })?;
        let locator_provenance =
            canonical_provenance("catalog hydration locator authorization", target.provenance)?;
        let qualified_locator_provenance = canonical_provenance(
            "catalog hydration qualified locator authorization",
            target.locator.provenance.clone(),
        )?;
        let binding_material = serialize_digest_material(
            &(
                CATALOG_HYDRATION_AUTHORIZATION_CONTRACT_VERSION,
                &handoff,
                &source.adapter_id,
                source.source_instance_key,
                &source.support_release_id,
                source.catalog_declaration_digest,
                source.access_policy_digest,
                target.locator_owner.generation,
                target.locator_kind,
                target.locator_basis,
                target.locator_disclosure,
            ),
            "catalog hydration locator authorization binding",
        )?;
        let locator_material = serialize_digest_material(
            &(
                locator_value,
                target.locator.quality,
                &target.locator.authority,
                target.locator.completeness,
                target.locator.unknown_reason,
                target.locator.effective_at,
                &qualified_locator_provenance,
                &locator_provenance,
            ),
            "catalog hydration locator authorization evidence",
        )?;
        let value = Self {
            hydration_authorization_contract_version:
                CATALOG_HYDRATION_AUTHORIZATION_CONTRACT_VERSION,
            authorization_id: CatalogHydrationAuthorizationId::from_digest(contract_digest(
                b"catalog-hydration-locator-authorization",
                &[&binding_material, &locator_material],
            )),
            handoff,
            adapter_id: source.adapter_id.clone(),
            source_instance_key: source.source_instance_key,
            support_release_id: source.support_release_id.clone(),
            catalog_declaration_digest: source.catalog_declaration_digest,
            access_policy_digest: source.access_policy_digest,
            locator_source_generation: target.locator_owner.generation,
            locator_kind: target.locator_kind,
            locator_basis: target.locator_basis,
            locator_disclosure: target.locator_disclosure,
            locator_provenance,
        };
        value.validate_shape()?;
        Ok(value)
    }

    fn validate_shape(&self) -> Result<(), CatalogContractError> {
        if self.hydration_authorization_contract_version
            != CATALOG_HYDRATION_AUTHORIZATION_CONTRACT_VERSION
        {
            return Err(CatalogContractError::invalid(format!(
                "unsupported catalog hydration authorization contract version {}",
                self.hydration_authorization_contract_version
            )));
        }
        validate_identifier("catalog hydration authorization adapter", &self.adapter_id)?;
        validate_identifier(
            "catalog hydration authorization support release",
            &self.support_release_id,
        )?;
        let canonical_handoff = CatalogSessionAttachHandoff::new(
            self.handoff.presentation_ref,
            self.handoff.member_refs.clone(),
            self.handoff.relation_keys.clone(),
            self.handoff.selected_base_session_ref,
            self.handoff.locator_claim_key,
        )?;
        if canonical_handoff != self.handoff {
            return Err(CatalogContractError::invalid(
                "catalog hydration authorization handoff is not canonical",
            ));
        }
        if self.handoff.selected_base_session_ref.kind != CatalogEntityKind::Session {
            return Err(CatalogContractError::invalid(
                "catalog hydration authorization must select a concrete base session",
            ));
        }
        if self.locator_disclosure == CatalogDisclosureClass::Public {
            return Err(CatalogContractError::invalid(
                "catalog hydration authorization cannot expose a public native locator",
            ));
        }
        validate_provenance(
            "catalog hydration locator authorization",
            &self.locator_provenance,
        )
    }

    fn matches_source(&self, source: &CatalogCoveragePlanSource) -> bool {
        self.adapter_id == source.adapter_id
            && self.source_instance_key == source.source_instance_key
            && self.support_release_id == source.support_release_id
            && self.catalog_declaration_digest == source.catalog_declaration_digest
            && self.access_policy_digest == source.access_policy_digest
    }
}

/// Non-serializable source-access authority retained beside the portable
/// hydration authorization. The portable contract deliberately omits the
/// native locator; only this reducer-produced value can cross into the native
/// observation supervisor.
pub(crate) struct CatalogHydrationExecutionAuthorization {
    portable: CatalogHydrationLocatorAuthorization,
    target: CatalogAttachTarget,
}

impl CatalogHydrationExecutionAuthorization {
    pub(crate) fn authorize(
        reducer: &CatalogReducer,
        handoff: CatalogSessionAttachHandoff,
        policy_view: CatalogPolicyView,
        source: &CatalogCoveragePlanSource,
    ) -> Result<Self, CatalogContractError> {
        let target = reducer.resolve_attach_target(&handoff, policy_view)?;
        let portable = CatalogHydrationLocatorAuthorization::from_attach_target(
            handoff,
            target.clone(),
            source,
        )?;
        Ok(Self { portable, target })
    }

    pub(crate) fn portable(&self) -> &CatalogHydrationLocatorAuthorization {
        &self.portable
    }

    pub(crate) fn attach_target(&self) -> &CatalogAttachTarget {
        &self.target
    }
}

impl fmt::Debug for CatalogHydrationExecutionAuthorization {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CatalogHydrationExecutionAuthorization")
            .field("portable", &self.portable)
            .finish_non_exhaustive()
    }
}

impl CatalogHydrationLocatorAuthorization {
    fn from_wire_value(value: JsonValue) -> Result<Self, CatalogContractError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            hydration_authorization_contract_version: u32,
            authorization_id: CatalogHydrationAuthorizationId,
            handoff: CatalogSessionAttachHandoff,
            adapter_id: String,
            source_instance_key: crate::adapter::CanonicalSourceInstanceKey,
            support_release_id: String,
            catalog_declaration_digest: CoverageDeclarationDigest,
            access_policy_digest: CatalogAccessPolicyDigest,
            locator_source_generation: u64,
            locator_kind: CatalogLocatorKind,
            locator_basis: ProjectAssociationBasis,
            locator_disclosure: CatalogDisclosureClass,
            locator_provenance: Vec<SemanticRevisionRef>,
        }

        let wire = serde_json::from_value::<Wire>(value)
            .map_err(|error| CatalogContractError::invalid(error.to_string()))?;
        let value = Self {
            hydration_authorization_contract_version: wire.hydration_authorization_contract_version,
            authorization_id: wire.authorization_id,
            handoff: wire.handoff,
            adapter_id: wire.adapter_id,
            source_instance_key: wire.source_instance_key,
            support_release_id: wire.support_release_id,
            catalog_declaration_digest: wire.catalog_declaration_digest,
            access_policy_digest: wire.access_policy_digest,
            locator_source_generation: wire.locator_source_generation,
            locator_kind: wire.locator_kind,
            locator_basis: wire.locator_basis,
            locator_disclosure: wire.locator_disclosure,
            locator_provenance: wire.locator_provenance,
        };
        value.validate_shape()?;
        Ok(value)
    }
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CatalogHydrationCommand {
    pub hydration_command_contract_version: u32,
    pub request_key: CatalogHydrationRequestKey,
    pub command_id: CatalogHydrationCommandId,
    pub coalescing_key: CatalogHydrationCoalescingKey,
    pub contract_selection: CatalogQueryContractSelection,
    pub snapshot_id: CatalogSnapshotId,
    pub source: CatalogCoveragePlanSource,
    pub authorization: CatalogHydrationLocatorAuthorization,
    pub requested_scope: CatalogHydrationRequestedScope,
    pub reason: CatalogHydrationReason,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CatalogHydrationCommandBinding {
    pub request_key: CatalogHydrationRequestKey,
    pub command_id: CatalogHydrationCommandId,
    pub coalescing_key: CatalogHydrationCoalescingKey,
    pub selected_base_session_ref: CatalogEntityRef,
    pub snapshot_id: CatalogSnapshotId,
}

impl fmt::Debug for CatalogHydrationCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CatalogHydrationCommand")
            .field("request_key", &self.request_key)
            .field("command_id", &self.command_id)
            .field("coalescing_key", &self.coalescing_key)
            .field("snapshot_id", &self.snapshot_id)
            .field(
                "selected_base_session_ref",
                &self.authorization.handoff.selected_base_session_ref,
            )
            .field("authorization_id", &self.authorization.authorization_id)
            .field("requested_scope", &self.requested_scope)
            .field("reason", &self.reason)
            .finish()
    }
}

impl CatalogHydrationCommand {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        request_key: CatalogHydrationRequestKey,
        contract_selection: CatalogQueryContractSelection,
        snapshot_id: CatalogSnapshotId,
        plan: &CatalogCoveragePlan,
        source: CatalogCoveragePlanSource,
        authorization: CatalogHydrationLocatorAuthorization,
        requested_scope: CatalogHydrationRequestedScope,
        reason: CatalogHydrationReason,
    ) -> Result<Self, CatalogContractError> {
        let mut value = Self {
            hydration_command_contract_version: CATALOG_HYDRATION_COMMAND_CONTRACT_VERSION,
            request_key,
            command_id: CatalogHydrationCommandId::from_digest([0; 32]),
            coalescing_key: CatalogHydrationCoalescingKey::from_digest([0; 32]),
            contract_selection,
            snapshot_id,
            source,
            authorization,
            requested_scope,
            reason,
        };
        value.validate_content()?;
        value.validate_plan_binding(plan)?;
        value.coalescing_key = value.derive_coalescing_key()?;
        value.command_id = value.derive_command_id()?;
        value.validate_shape()?;
        Ok(value)
    }

    pub(crate) fn from_wire_value(
        value: JsonValue,
        expected_plan: &CatalogCoveragePlan,
        expected_selection: &CatalogQueryContractSelection,
        expected_snapshot: CatalogSnapshotId,
        expected_authorization: &CatalogHydrationLocatorAuthorization,
    ) -> Result<Self, CatalogContractError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            hydration_command_contract_version: u32,
            request_key: CatalogHydrationRequestKey,
            command_id: CatalogHydrationCommandId,
            coalescing_key: CatalogHydrationCoalescingKey,
            contract_selection: CatalogQueryContractSelection,
            snapshot_id: JsonValue,
            source: JsonValue,
            authorization: JsonValue,
            requested_scope: CatalogHydrationRequestedScope,
            reason: CatalogHydrationReason,
        }

        let wire = serde_json::from_value::<Wire>(value)
            .map_err(|error| CatalogContractError::invalid(error.to_string()))?;
        let parsed = Self {
            hydration_command_contract_version: wire.hydration_command_contract_version,
            request_key: wire.request_key,
            command_id: wire.command_id,
            coalescing_key: wire.coalescing_key,
            contract_selection: wire.contract_selection,
            snapshot_id: parse_portable_snapshot(wire.snapshot_id)?,
            source: parse_portable_source(wire.source)?,
            authorization: CatalogHydrationLocatorAuthorization::from_wire_value(
                wire.authorization,
            )?,
            requested_scope: wire.requested_scope,
            reason: wire.reason,
        };
        parsed.validate_against(
            expected_plan,
            expected_selection,
            expected_snapshot,
            expected_authorization,
        )?;
        Ok(parsed)
    }

    pub(crate) fn binding(&self) -> CatalogHydrationCommandBinding {
        CatalogHydrationCommandBinding {
            request_key: self.request_key,
            command_id: self.command_id,
            coalescing_key: self.coalescing_key,
            selected_base_session_ref: self.authorization.handoff.selected_base_session_ref,
            snapshot_id: self.snapshot_id,
        }
    }

    fn validate_against(
        &self,
        plan: &CatalogCoveragePlan,
        expected_selection: &CatalogQueryContractSelection,
        expected_snapshot: CatalogSnapshotId,
        expected_authorization: &CatalogHydrationLocatorAuthorization,
    ) -> Result<(), CatalogContractError> {
        plan.validate()?;
        expected_selection.validate()?;
        expected_snapshot.validate()?;
        expected_authorization.validate_shape()?;
        self.validate_shape()?;
        if self.contract_selection != *expected_selection
            || self.snapshot_id != expected_snapshot
            || self.authorization != *expected_authorization
        {
            return Err(CatalogContractError::invalid(
                "catalog hydration command does not match the expected selection, snapshot, or locator authorization",
            ));
        }
        self.validate_plan_binding(plan)
    }

    fn validate_content(&self) -> Result<(), CatalogContractError> {
        if self.hydration_command_contract_version != CATALOG_HYDRATION_COMMAND_CONTRACT_VERSION {
            return Err(CatalogContractError::invalid(format!(
                "unsupported catalog hydration command contract version {}",
                self.hydration_command_contract_version
            )));
        }
        self.contract_selection.validate()?;
        self.snapshot_id.validate()?;
        if self.snapshot_id.readiness_epoch > MAX_JAVASCRIPT_SAFE_INTEGER
            || self.snapshot_id.complete_commit > MAX_JAVASCRIPT_SAFE_INTEGER
        {
            return Err(CatalogContractError::invalid(
                "catalog hydration snapshot lineage exceeds the portable integer range",
            ));
        }
        self.authorization.validate_shape()?;
        if self.authorization.locator_source_generation == 0
            || self.authorization.locator_source_generation > MAX_JAVASCRIPT_SAFE_INTEGER
        {
            return Err(CatalogContractError::invalid(
                "catalog hydration locator generation must be positive and portable",
            ));
        }
        self.source.validate()?;
        self.requested_scope
            .validate_for_selection(&self.contract_selection)?;
        if self.snapshot_id.pack_contract_version
            != self
                .contract_selection
                .contract_versions
                .query_pack_version
                .ok_or_else(|| {
                    CatalogContractError::invalid(
                        "catalog hydration requires a selected catalog query pack",
                    )
                })?
            || !self.authorization.matches_source(&self.source)
        {
            return Err(CatalogContractError::invalid(
                "catalog hydration command has inconsistent contract, source, or authorization bindings",
            ));
        }
        Ok(())
    }

    fn validate_plan_binding(
        &self,
        plan: &CatalogCoveragePlan,
    ) -> Result<(), CatalogContractError> {
        if self.snapshot_id.coverage_plan_id != plan.coverage_plan_id
            || !plan
                .required_sources
                .iter()
                .chain(plan.optional_sources.iter())
                .any(|source| source == &self.source)
        {
            return Err(CatalogContractError::invalid(
                "catalog hydration command source is outside the retained coverage plan",
            ));
        }
        Ok(())
    }

    fn validate_shape(&self) -> Result<(), CatalogContractError> {
        self.validate_content()?;
        if self.coalescing_key != self.derive_coalescing_key()?
            || self.command_id != self.derive_command_id()?
        {
            return Err(CatalogContractError::invalid(
                "catalog hydration command identity does not match its immutable bindings",
            ));
        }
        Ok(())
    }

    fn derive_coalescing_key(&self) -> Result<CatalogHydrationCoalescingKey, CatalogContractError> {
        let material = serialize_digest_material(
            &(
                self.hydration_command_contract_version,
                &self.contract_selection,
                self.snapshot_id,
                &self.source,
                &self.authorization,
                &self.requested_scope,
                self.reason,
            ),
            "catalog hydration coalescing key",
        )?;
        Ok(CatalogHydrationCoalescingKey::from_digest(contract_digest(
            b"catalog-hydration-coalescing-key",
            &[&material],
        )))
    }

    fn derive_command_id(&self) -> Result<CatalogHydrationCommandId, CatalogContractError> {
        let material = serialize_digest_material(
            &(
                self.hydration_command_contract_version,
                self.request_key,
                self.coalescing_key,
            ),
            "catalog hydration command identity",
        )?;
        Ok(CatalogHydrationCommandId::from_digest(contract_digest(
            b"catalog-hydration-command-id",
            &[&material],
        )))
    }

    pub(crate) fn coalesces_with(&self, other: &Self) -> bool {
        self.coalescing_key == other.coalescing_key
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CatalogHydrationReplayObservation {
    New,
    Replay,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct CatalogHydrationReplayRegistry {
    commands: BTreeMap<CatalogHydrationRequestKey, CatalogHydrationCommand>,
}

impl CatalogHydrationReplayRegistry {
    pub(crate) fn observe(
        &mut self,
        command: CatalogHydrationCommand,
    ) -> Result<CatalogHydrationReplayObservation, CatalogContractError> {
        command.validate_shape()?;
        match self.commands.get(&command.request_key) {
            Some(existing) if existing == &command => Ok(CatalogHydrationReplayObservation::Replay),
            Some(_) => Err(CatalogContractError::invalid(
                "catalog hydration request key cannot retarget immutable command bindings",
            )),
            None => {
                self.commands.insert(command.request_key, command);
                Ok(CatalogHydrationReplayObservation::New)
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CatalogHydrationFailureDisposition {
    Retryable,
    Terminal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CatalogHydrationFailure {
    pub disposition: CatalogHydrationFailureDisposition,
    pub code: String,
    pub retry_after_millis: Option<u32>,
}

impl CatalogHydrationFailure {
    pub(crate) fn retryable(
        code: impl Into<String>,
        retry_after_millis: u32,
    ) -> Result<Self, CatalogContractError> {
        let value = Self {
            disposition: CatalogHydrationFailureDisposition::Retryable,
            code: code.into(),
            retry_after_millis: Some(retry_after_millis),
        };
        value.validate()?;
        Ok(value)
    }

    pub(crate) fn terminal(code: impl Into<String>) -> Result<Self, CatalogContractError> {
        let value = Self {
            disposition: CatalogHydrationFailureDisposition::Terminal,
            code: code.into(),
            retry_after_millis: None,
        };
        value.validate()?;
        Ok(value)
    }

    fn validate(&self) -> Result<(), CatalogContractError> {
        let mut bytes = self.code.bytes();
        if self.code.len() > MAX_FAILURE_CODE_BYTES
            || !matches!(bytes.next(), Some(first) if first.is_ascii_lowercase())
            || !bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        {
            return Err(CatalogContractError::invalid(format!(
                "catalog hydration failure code must be a lowercase ASCII machine code of at most {MAX_FAILURE_CODE_BYTES} bytes"
            )));
        }
        match (self.disposition, self.retry_after_millis) {
            (CatalogHydrationFailureDisposition::Retryable, Some(delay))
                if (1..=MAX_RETRY_AFTER_MILLIS).contains(&delay) =>
            {
                Ok(())
            }
            (CatalogHydrationFailureDisposition::Terminal, None) => Ok(()),
            (CatalogHydrationFailureDisposition::Retryable, _) => {
                Err(CatalogContractError::invalid(format!(
                    "retryable catalog hydration failure requires a 1..={MAX_RETRY_AFTER_MILLIS}ms delay"
                )))
            }
            (CatalogHydrationFailureDisposition::Terminal, Some(_)) => {
                Err(CatalogContractError::invalid(
                    "terminal catalog hydration failure cannot advertise a retry delay",
                ))
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum CatalogHydrationSchedulingOutcome {
    Accepted,
    AlreadySatisfied,
    InProgress {
        active_command_id: CatalogHydrationCommandId,
        active_receipt_id: CatalogSchedulingReceiptId,
    },
    Rejected {
        failure: CatalogHydrationFailure,
    },
}

impl CatalogHydrationSchedulingOutcome {
    fn retryable_rejection(&self) -> bool {
        matches!(
            self,
            Self::Rejected {
                failure: CatalogHydrationFailure {
                    disposition: CatalogHydrationFailureDisposition::Retryable,
                    ..
                }
            }
        )
    }

    fn terminal(&self) -> bool {
        matches!(
            self,
            Self::AlreadySatisfied
                | Self::Rejected {
                    failure: CatalogHydrationFailure {
                        disposition: CatalogHydrationFailureDisposition::Terminal,
                        ..
                    }
                }
        )
    }
}

#[derive(Clone, Copy)]
pub(crate) struct CatalogHydrationActiveSchedule<'a> {
    command: &'a CatalogHydrationCommand,
    receipt: &'a CatalogSchedulingReceipt,
}

impl<'a> CatalogHydrationActiveSchedule<'a> {
    pub(crate) fn new(
        command: &'a CatalogHydrationCommand,
        receipt: &'a CatalogSchedulingReceipt,
    ) -> Result<Self, CatalogContractError> {
        command.validate_shape()?;
        receipt.validate_common_for_command(command)?;
        if !matches!(receipt.outcome, CatalogHydrationSchedulingOutcome::Accepted) {
            return Err(CatalogContractError::invalid(
                "active catalog hydration schedule requires an accepted receipt",
            ));
        }
        Ok(Self { command, receipt })
    }
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CatalogSchedulingReceipt {
    pub scheduling_receipt_contract_version: u32,
    pub receipt_id: CatalogSchedulingReceiptId,
    pub request_key: CatalogHydrationRequestKey,
    pub command_id: CatalogHydrationCommandId,
    pub coalescing_key: CatalogHydrationCoalescingKey,
    pub selected_base_session_ref: CatalogEntityRef,
    pub snapshot_id: CatalogSnapshotId,
    pub attempt: u32,
    pub prior_receipt_id: Option<CatalogSchedulingReceiptId>,
    pub emitted_at_commit: u64,
    pub outcome: CatalogHydrationSchedulingOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CatalogHydrationActiveScheduleBinding {
    pub command: CatalogHydrationCommandBinding,
    pub receipt: CatalogSchedulingReceipt,
}

impl CatalogHydrationActiveScheduleBinding {
    pub(crate) fn new(
        command: &CatalogHydrationCommand,
        receipt: &CatalogSchedulingReceipt,
    ) -> Result<Self, CatalogContractError> {
        CatalogHydrationActiveSchedule::new(command, receipt)?;
        Ok(Self {
            command: command.binding(),
            receipt: receipt.clone(),
        })
    }
}

impl fmt::Debug for CatalogSchedulingReceipt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CatalogSchedulingReceipt")
            .field("receipt_id", &self.receipt_id)
            .field("command_id", &self.command_id)
            .field("coalescing_key", &self.coalescing_key)
            .field("selected_base_session_ref", &self.selected_base_session_ref)
            .field("snapshot_id", &self.snapshot_id)
            .field("attempt", &self.attempt)
            .field("prior_receipt_id", &self.prior_receipt_id)
            .field("emitted_at_commit", &self.emitted_at_commit)
            .field("outcome", &self.outcome)
            .finish()
    }
}

impl CatalogSchedulingReceipt {
    pub(crate) fn issue(
        command: &CatalogHydrationCommand,
        prior: Option<&Self>,
        active: Option<CatalogHydrationActiveSchedule<'_>>,
        emitted_at_commit: u64,
        outcome: CatalogHydrationSchedulingOutcome,
    ) -> Result<Self, CatalogContractError> {
        let (attempt, prior_receipt_id) = match prior {
            None => (1, None),
            Some(previous) if previous.outcome.retryable_rejection() => (
                previous.attempt.checked_add(1).ok_or_else(|| {
                    CatalogContractError::invalid("catalog hydration receipt attempt overflow")
                })?,
                Some(previous.receipt_id),
            ),
            Some(previous) => (previous.attempt, Some(previous.receipt_id)),
        };
        let mut value = Self {
            scheduling_receipt_contract_version: CATALOG_SCHEDULING_RECEIPT_CONTRACT_VERSION,
            receipt_id: CatalogSchedulingReceiptId::from_digest([0; 32]),
            request_key: command.request_key,
            command_id: command.command_id,
            coalescing_key: command.coalescing_key,
            selected_base_session_ref: command.authorization.handoff.selected_base_session_ref,
            snapshot_id: command.snapshot_id,
            attempt,
            prior_receipt_id,
            emitted_at_commit,
            outcome,
        };
        value.receipt_id = value.derive_receipt_id()?;
        value.validate_for_context(command, prior, active)?;
        Ok(value)
    }

    pub(crate) fn from_wire_value(
        value: JsonValue,
        expected_command: &CatalogHydrationCommand,
        expected_prior: Option<&Self>,
        expected_active: Option<CatalogHydrationActiveSchedule<'_>>,
    ) -> Result<Self, CatalogContractError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            scheduling_receipt_contract_version: u32,
            receipt_id: CatalogSchedulingReceiptId,
            request_key: CatalogHydrationRequestKey,
            command_id: CatalogHydrationCommandId,
            coalescing_key: CatalogHydrationCoalescingKey,
            selected_base_session_ref: CatalogEntityRef,
            snapshot_id: JsonValue,
            attempt: u32,
            prior_receipt_id: Option<CatalogSchedulingReceiptId>,
            emitted_at_commit: u64,
            outcome: CatalogHydrationSchedulingOutcome,
        }

        let wire = serde_json::from_value::<Wire>(value)
            .map_err(|error| CatalogContractError::invalid(error.to_string()))?;
        let parsed = Self {
            scheduling_receipt_contract_version: wire.scheduling_receipt_contract_version,
            receipt_id: wire.receipt_id,
            request_key: wire.request_key,
            command_id: wire.command_id,
            coalescing_key: wire.coalescing_key,
            selected_base_session_ref: wire.selected_base_session_ref,
            snapshot_id: parse_portable_snapshot(wire.snapshot_id)?,
            attempt: wire.attempt,
            prior_receipt_id: wire.prior_receipt_id,
            emitted_at_commit: wire.emitted_at_commit,
            outcome: wire.outcome,
        };
        parsed.validate_for_context(expected_command, expected_prior, expected_active)?;
        Ok(parsed)
    }

    fn validate_for_context(
        &self,
        command: &CatalogHydrationCommand,
        prior: Option<&Self>,
        active: Option<CatalogHydrationActiveSchedule<'_>>,
    ) -> Result<(), CatalogContractError> {
        command.validate_shape()?;
        if self.scheduling_receipt_contract_version != CATALOG_SCHEDULING_RECEIPT_CONTRACT_VERSION
            || self.request_key != command.request_key
            || self.command_id != command.command_id
            || self.coalescing_key != command.coalescing_key
            || self.selected_base_session_ref
                != command.authorization.handoff.selected_base_session_ref
            || self.snapshot_id != command.snapshot_id
        {
            return Err(CatalogContractError::invalid(
                "catalog scheduling receipt does not match its hydration command",
            ));
        }
        if self.attempt == 0
            || self.emitted_at_commit < self.snapshot_id.complete_commit
            || self.emitted_at_commit > MAX_JAVASCRIPT_SAFE_INTEGER
        {
            return Err(CatalogContractError::invalid(
                "catalog scheduling receipt has an invalid attempt or commit lineage",
            ));
        }
        if let CatalogHydrationSchedulingOutcome::Rejected { failure } = &self.outcome {
            failure.validate()?;
        }
        match prior {
            None => {
                if self.prior_receipt_id.is_some() || self.attempt != 1 {
                    return Err(CatalogContractError::invalid(
                        "initial catalog scheduling receipt cannot claim prior lineage",
                    ));
                }
            }
            Some(previous) => {
                previous.validate_common_for_command(command)?;
                if self.prior_receipt_id != Some(previous.receipt_id)
                    || self.emitted_at_commit < previous.emitted_at_commit
                    || previous.outcome.terminal()
                {
                    return Err(CatalogContractError::invalid(
                        "catalog scheduling receipt has impossible or terminal prior lineage",
                    ));
                }
                let expected_attempt = if previous.outcome.retryable_rejection() {
                    previous.attempt.checked_add(1).ok_or_else(|| {
                        CatalogContractError::invalid("catalog hydration receipt attempt overflow")
                    })?
                } else {
                    previous.attempt
                };
                if self.attempt != expected_attempt {
                    return Err(CatalogContractError::invalid(
                        "catalog scheduling receipt attempt does not follow its prior outcome",
                    ));
                }
                if !previous.outcome.retryable_rejection()
                    && !matches!(
                        (&previous.outcome, &self.outcome),
                        (
                            CatalogHydrationSchedulingOutcome::Accepted
                                | CatalogHydrationSchedulingOutcome::InProgress { .. },
                            CatalogHydrationSchedulingOutcome::AlreadySatisfied
                                | CatalogHydrationSchedulingOutcome::Rejected { .. }
                        )
                    )
                {
                    return Err(CatalogContractError::invalid(
                        "catalog scheduling receipt outcome cannot follow its prior state",
                    ));
                }
            }
        }
        match (&self.outcome, active) {
            (
                CatalogHydrationSchedulingOutcome::InProgress {
                    active_command_id,
                    active_receipt_id,
                },
                Some(active_schedule),
            ) => {
                active_schedule.command.validate_shape()?;
                active_schedule
                    .receipt
                    .validate_common_for_command(active_schedule.command)?;
                if active_schedule.command.command_id == self.command_id
                    || active_schedule.command.coalescing_key != self.coalescing_key
                    || active_schedule.command.snapshot_id != self.snapshot_id
                    || active_schedule
                        .command
                        .authorization
                        .handoff
                        .selected_base_session_ref
                        != self.selected_base_session_ref
                    || !matches!(
                        active_schedule.receipt.outcome,
                        CatalogHydrationSchedulingOutcome::Accepted
                    )
                    || self.emitted_at_commit < active_schedule.receipt.emitted_at_commit
                    || *active_command_id != active_schedule.command.command_id
                    || *active_receipt_id != active_schedule.receipt.receipt_id
                {
                    return Err(CatalogContractError::invalid(
                        "catalog in-progress receipt does not reference an accepted coalesced command",
                    ));
                }
            }
            (CatalogHydrationSchedulingOutcome::InProgress { .. }, None) => {
                return Err(CatalogContractError::invalid(
                    "catalog in-progress receipt requires its accepted active receipt",
                ));
            }
            (_, Some(_)) => {
                return Err(CatalogContractError::invalid(
                    "only an in-progress receipt may carry active coalescing context",
                ));
            }
            (_, None) => {}
        }
        if self.receipt_id != self.derive_receipt_id()? {
            return Err(CatalogContractError::invalid(
                "catalog scheduling receipt id does not match its immutable lineage",
            ));
        }
        Ok(())
    }

    fn validate_common_for_command(
        &self,
        command: &CatalogHydrationCommand,
    ) -> Result<(), CatalogContractError> {
        self.validate_self_consistency()?;
        if self.request_key != command.request_key
            || self.command_id != command.command_id
            || self.coalescing_key != command.coalescing_key
            || self.snapshot_id != command.snapshot_id
            || self.selected_base_session_ref
                != command.authorization.handoff.selected_base_session_ref
        {
            return Err(CatalogContractError::invalid(
                "catalog scheduling receipt prior belongs to another command",
            ));
        }
        Ok(())
    }

    fn validate_self_consistency(&self) -> Result<(), CatalogContractError> {
        self.snapshot_id.validate()?;
        self.selected_base_session_ref.validate()?;
        if self.scheduling_receipt_contract_version != CATALOG_SCHEDULING_RECEIPT_CONTRACT_VERSION
            || self.attempt == 0
            || self.selected_base_session_ref.kind != CatalogEntityKind::Session
            || (self.attempt > 1 && self.prior_receipt_id.is_none())
            || self.emitted_at_commit < self.snapshot_id.complete_commit
            || self.emitted_at_commit > MAX_JAVASCRIPT_SAFE_INTEGER
            || self.receipt_id != self.derive_receipt_id()?
        {
            return Err(CatalogContractError::invalid(
                "catalog scheduling receipt is not self-consistent",
            ));
        }
        if let CatalogHydrationSchedulingOutcome::Rejected { failure } = &self.outcome {
            failure.validate()?;
        }
        Ok(())
    }

    fn derive_receipt_id(&self) -> Result<CatalogSchedulingReceiptId, CatalogContractError> {
        let material = serialize_digest_material(
            &(
                self.scheduling_receipt_contract_version,
                self.request_key,
                self.command_id,
                self.coalescing_key,
                self.selected_base_session_ref,
                self.snapshot_id,
                self.attempt,
                self.prior_receipt_id,
                self.emitted_at_commit,
                &self.outcome,
            ),
            "catalog scheduling receipt identity",
        )?;
        Ok(CatalogSchedulingReceiptId::from_digest(contract_digest(
            b"catalog-scheduling-receipt-id",
            &[&material],
        )))
    }
}
