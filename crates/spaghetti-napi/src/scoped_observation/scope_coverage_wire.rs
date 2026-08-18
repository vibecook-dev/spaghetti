//! Strict portable RFC 012D projection of exact known-object scope coverage.
//!
//! This is deliberately not an observation watermark or completion barrier.
//! Consumption requires caller-held program/root context plus the authoritative
//! RFC 012A Decode set. The scope summary cannot replace source positions,
//! membership revisions, errors, dynamic discovery, or family manifests.

use std::sync::Arc;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::adapter::{
    AdapterId, CanonicalEntityKey, CanonicalSourceInstanceKey, CoverageAbsenceKind,
    CoverageObjectKey, CoverageSetCompleteness, CoverageStatus, CoverageStreamKey, Sha256Digest,
    SourceCoverageSet,
};
use crate::source::validate_relation_id;

use super::{
    derive_scoped_scope_coverage_revision, ScopedObservationRootIdentity, ScopedScopeCoverage,
    ScopedScopeCoverageRevision, ScopedScopeRelationCoverage, ScopedScopeRelationState,
    ScopedSourceObjectIdentity, SCOPED_SCOPE_COVERAGE_CONTRACT_VERSION,
};

const DIGEST_BYTES: usize = 32;
const REFERENCE_PREFIX: &str = "v1:";
const JS_SAFE_INTEGER_MAX: u64 = 9_007_199_254_740_991;
/// One relation maps to either a point or an absence in the RFC 012A Decode
/// set. This cap is the sum of that contract's independently bounded arrays.
const MAX_SCOPE_COVERAGE_RELATIONS: usize = 500_000;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ScopedScopeCoverageContractError {
    #[error("invalid scoped scope-coverage contract: {message}")]
    Invalid { message: String },
    #[error("scoped scope coverage does not match caller-held context")]
    ContextMismatch,
}

impl ScopedScopeCoverageContractError {
    fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid {
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopedScopeCoverageRootWire {
    adapter_id: String,
    source_instance_key: CanonicalSourceInstanceKey,
    session_key: CanonicalEntityKey,
}

impl ScopedScopeCoverageRootWire {
    fn from_root(root: &ScopedObservationRootIdentity) -> Self {
        Self {
            adapter_id: root.adapter_id.as_str().to_owned(),
            source_instance_key: root.source_instance_key,
            session_key: root.session_key,
        }
    }
}

/// Context retained separately by the native observer handle. The portable
/// representation is useful for conformance fixtures; it is not source-access
/// authority and cannot be used to construct this Rust value.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ScopedScopeCoverageConsumerContext {
    root: ScopedObservationRootIdentity,
    program_id: String,
    scope_program_digest: Sha256Digest,
    root_relation_id: String,
    declared_relation_ids: Vec<String>,
    expected_scope_revision: ScopedScopeCoverageRevision,
    decode_coverage: SourceCoverageSet,
}

impl std::fmt::Debug for ScopedScopeCoverageConsumerContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ScopedScopeCoverageConsumerContext")
            .field("program_id", &self.program_id)
            .field("scope_program_digest", &self.scope_program_digest)
            .field("root_relation_id", &self.root_relation_id)
            .field("declared_relation_count", &self.declared_relation_ids.len())
            .field(
                "expected_scope_revision",
                &encode_opaque(self.expected_scope_revision.as_bytes()),
            )
            .field(
                "decode_coverage_membership_revision",
                &self.decode_coverage.membership_revision,
            )
            .finish_non_exhaustive()
    }
}

impl ScopedScopeCoverageConsumerContext {
    pub(crate) fn from_expected(
        expected: &ScopedScopeCoverage,
        root: &ScopedObservationRootIdentity,
        source_coverage: &[SourceCoverageSet],
    ) -> Result<Self, ScopedScopeCoverageContractError> {
        if !expected.validate_against(root, source_coverage) {
            return Err(ScopedScopeCoverageContractError::ContextMismatch);
        }
        let mut decode_sets = source_coverage
            .iter()
            .filter(|set| matches!(set.coverage_domain, crate::adapter::CoverageDomain::Decode));
        let decode_coverage = decode_sets
            .next()
            .ok_or(ScopedScopeCoverageContractError::ContextMismatch)?;
        if decode_sets.next().is_some() {
            return Err(ScopedScopeCoverageContractError::ContextMismatch);
        }
        let declared_relation_ids = expected
            .relations()
            .iter()
            .map(|relation| relation.relation_id.to_string())
            .collect::<Vec<_>>();
        if declared_relation_ids.len() > MAX_SCOPE_COVERAGE_RELATIONS {
            return Err(ScopedScopeCoverageContractError::invalid(
                "scope coverage exceeds the portable relation bound",
            ));
        }
        Ok(Self {
            root: root.clone(),
            program_id: expected.program_id().to_owned(),
            scope_program_digest: expected.scope_program_digest(),
            root_relation_id: expected.root_relation_id().to_owned(),
            declared_relation_ids,
            expected_scope_revision: expected.scope_revision(),
            decode_coverage: decode_coverage.clone(),
        })
    }

    pub(crate) fn wire(&self) -> ScopedScopeCoverageContextWire {
        ScopedScopeCoverageContextWire {
            root: ScopedScopeCoverageRootWire::from_root(&self.root),
            program_id: self.program_id.clone(),
            scope_program_digest: self.scope_program_digest.to_string(),
            root_relation_id: self.root_relation_id.clone(),
            declared_relation_ids: self.declared_relation_ids.clone(),
            expected_scope_revision: encode_opaque(self.expected_scope_revision.as_bytes()),
            decode_coverage: self.decode_coverage.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ScopedScopeCoverageContextWire {
    root: ScopedScopeCoverageRootWire,
    program_id: String,
    scope_program_digest: String,
    root_relation_id: String,
    declared_relation_ids: Vec<String>,
    expected_scope_revision: String,
    decode_coverage: SourceCoverageSet,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopedScopeCoverageSourceWire {
    adapter_id: String,
    source_instance_key: CanonicalSourceInstanceKey,
    stream_key: CoverageStreamKey,
    object_key: CoverageObjectKey,
}

impl ScopedScopeCoverageSourceWire {
    fn from_source(source: &ScopedSourceObjectIdentity) -> Self {
        Self {
            adapter_id: source.adapter_id.as_str().to_owned(),
            source_instance_key: source.source_instance_key,
            stream_key: source.stream_key,
            object_key: source.object_key,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum ScopedScopeRelationStateWire {
    Present { status: CoverageStatus },
    Absent { absence_kind: CoverageAbsenceKind },
}

impl ScopedScopeRelationStateWire {
    fn from_state(state: &ScopedScopeRelationState) -> Self {
        match state {
            ScopedScopeRelationState::Present { status } => Self::Present {
                status: status.clone(),
            },
            ScopedScopeRelationState::Absent { kind } => Self::Absent {
                absence_kind: *kind,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopedScopeRelationCoverageWire {
    relation_id: String,
    scope_root: bool,
    source: ScopedScopeCoverageSourceWire,
    generation: u64,
    state: ScopedScopeRelationStateWire,
    completeness: CoverageSetCompleteness,
}

impl ScopedScopeRelationCoverageWire {
    fn from_relation(relation: &ScopedScopeRelationCoverage) -> Self {
        Self {
            relation_id: relation.relation_id.to_string(),
            scope_root: relation.scope_root,
            source: ScopedScopeCoverageSourceWire::from_source(&relation.source),
            generation: relation.generation,
            state: ScopedScopeRelationStateWire::from_state(&relation.state),
            completeness: relation.completeness,
        }
    }
}

/// Strict portable projection. It is serialize-only; wire consumption must use
/// `from_wire_value_for_context` so a payload cannot authorize its own program,
/// root, declared relation set, or Decode evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ScopedScopeCoverageWire {
    scoped_scope_coverage_contract_version: u32,
    program_id: String,
    scope_program_digest: String,
    root_relation_id: String,
    scope_revision: String,
    relations: Vec<ScopedScopeRelationCoverageWire>,
    completeness: CoverageSetCompleteness,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopedScopeCoverageRawWire {
    scoped_scope_coverage_contract_version: u32,
    program_id: String,
    scope_program_digest: String,
    root_relation_id: String,
    scope_revision: String,
    relations: Vec<ScopedScopeRelationCoverageWire>,
    completeness: CoverageSetCompleteness,
}

impl ScopedScopeCoverageWire {
    pub(crate) fn from_expected(
        expected: &ScopedScopeCoverage,
        context: &ScopedScopeCoverageConsumerContext,
    ) -> Result<Self, ScopedScopeCoverageContractError> {
        if !expected.validate_against(
            &context.root,
            std::slice::from_ref(&context.decode_coverage),
        ) || expected.program_id() != context.program_id
            || expected.scope_program_digest() != context.scope_program_digest
            || expected.root_relation_id() != context.root_relation_id
            || expected
                .relations()
                .iter()
                .map(|relation| relation.relation_id.as_ref())
                .ne(context.declared_relation_ids.iter().map(String::as_str))
        {
            return Err(ScopedScopeCoverageContractError::ContextMismatch);
        }
        Ok(Self::from_validated(expected))
    }

    pub(crate) fn from_wire_value_for_context(
        value: JsonValue,
        context: &ScopedScopeCoverageConsumerContext,
    ) -> Result<Self, ScopedScopeCoverageContractError> {
        let relation_count = value
            .as_object()
            .and_then(|object| object.get("relations"))
            .and_then(JsonValue::as_array)
            .map(Vec::len)
            .ok_or_else(|| {
                ScopedScopeCoverageContractError::invalid(
                    "scope coverage relations must be an array",
                )
            })?;
        if relation_count > MAX_SCOPE_COVERAGE_RELATIONS
            || relation_count != context.declared_relation_ids.len()
        {
            return Err(ScopedScopeCoverageContractError::ContextMismatch);
        }
        let raw: ScopedScopeCoverageRawWire = serde_json::from_value(value)
            .map_err(|error| ScopedScopeCoverageContractError::invalid(error.to_string()))?;
        if raw.scoped_scope_coverage_contract_version != SCOPED_SCOPE_COVERAGE_CONTRACT_VERSION
            || raw.program_id != context.program_id
            || parse_sha256(&raw.scope_program_digest)? != context.scope_program_digest
            || raw.root_relation_id != context.root_relation_id
            || raw
                .relations
                .iter()
                .map(|relation| relation.relation_id.as_str())
                .ne(context.declared_relation_ids.iter().map(String::as_str))
        {
            return Err(ScopedScopeCoverageContractError::ContextMismatch);
        }
        let revision = decode_fixed_opaque(&raw.scope_revision, "scope revision")?;
        if revision != *context.expected_scope_revision.as_bytes() {
            return Err(ScopedScopeCoverageContractError::ContextMismatch);
        }
        let relations = raw
            .relations
            .into_iter()
            .map(|relation| relation.into_internal(context))
            .collect::<Result<Vec<_>, _>>()?;
        let scope_revision = derive_scoped_scope_coverage_revision(
            &raw.program_id,
            context.scope_program_digest,
            &raw.root_relation_id,
            &context.root,
            &relations,
            raw.completeness,
        );
        if revision != *scope_revision.as_bytes() {
            return Err(ScopedScopeCoverageContractError::ContextMismatch);
        }
        let reconstructed = ScopedScopeCoverage {
            contract_version: raw.scoped_scope_coverage_contract_version,
            program_id: raw.program_id,
            scope_program_digest: context.scope_program_digest,
            root_relation_id: Arc::from(raw.root_relation_id),
            scope_revision,
            relations,
            completeness: raw.completeness,
        };
        if !reconstructed.validate_against(
            &context.root,
            std::slice::from_ref(&context.decode_coverage),
        ) {
            return Err(ScopedScopeCoverageContractError::ContextMismatch);
        }
        Ok(Self::from_validated(&reconstructed))
    }

    fn from_validated(value: &ScopedScopeCoverage) -> Self {
        Self {
            scoped_scope_coverage_contract_version: value.contract_version(),
            program_id: value.program_id().to_owned(),
            scope_program_digest: value.scope_program_digest().to_string(),
            root_relation_id: value.root_relation_id().to_owned(),
            scope_revision: encode_opaque(value.scope_revision().as_bytes()),
            relations: value
                .relations()
                .iter()
                .map(ScopedScopeRelationCoverageWire::from_relation)
                .collect(),
            completeness: value.completeness(),
        }
    }
}

impl ScopedScopeRelationCoverageWire {
    fn into_internal(
        self,
        context: &ScopedScopeCoverageConsumerContext,
    ) -> Result<ScopedScopeRelationCoverage, ScopedScopeCoverageContractError> {
        if validate_relation_id(&self.relation_id).is_err()
            || self.source.adapter_id != context.root.adapter_id.as_str()
            || self.source.source_instance_key != context.root.source_instance_key
            || self.generation == 0
            || self.generation > JS_SAFE_INTEGER_MAX
            || self.scope_root != (self.relation_id == context.root_relation_id)
        {
            return Err(ScopedScopeCoverageContractError::ContextMismatch);
        }
        let state = match self.state {
            ScopedScopeRelationStateWire::Present { status } => {
                ScopedScopeRelationState::Present { status }
            }
            ScopedScopeRelationStateWire::Absent { absence_kind } => {
                ScopedScopeRelationState::Absent { kind: absence_kind }
            }
        };
        Ok(ScopedScopeRelationCoverage {
            relation_id: Arc::from(self.relation_id),
            scope_root: self.scope_root,
            source: ScopedSourceObjectIdentity {
                adapter_id: AdapterId::new(self.source.adapter_id.as_str())
                    .map_err(|_| ScopedScopeCoverageContractError::ContextMismatch)?,
                source_instance_key: self.source.source_instance_key,
                stream_key: self.source.stream_key,
                object_key: self.source.object_key,
            },
            generation: self.generation,
            state,
            completeness: self.completeness,
        })
    }
}

fn parse_sha256(value: &str) -> Result<Sha256Digest, ScopedScopeCoverageContractError> {
    Sha256Digest::parse(value)
        .map_err(|error| ScopedScopeCoverageContractError::invalid(error.to_string()))
}

fn encode_opaque(bytes: &[u8; DIGEST_BYTES]) -> String {
    format!("{REFERENCE_PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes))
}

fn decode_fixed_opaque(
    value: &str,
    label: &str,
) -> Result<[u8; DIGEST_BYTES], ScopedScopeCoverageContractError> {
    let encoded = value.strip_prefix(REFERENCE_PREFIX).ok_or_else(|| {
        ScopedScopeCoverageContractError::invalid(format!(
            "{label} must use the {REFERENCE_PREFIX} prefix"
        ))
    })?;
    if encoded.is_empty() || encoded.contains('=') {
        return Err(ScopedScopeCoverageContractError::invalid(format!(
            "{label} is not canonical base64url"
        )));
    }
    let decoded = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| {
        ScopedScopeCoverageContractError::invalid(format!("{label} is not canonical base64url"))
    })?;
    let bytes: [u8; DIGEST_BYTES] = decoded.try_into().map_err(|_| {
        ScopedScopeCoverageContractError::invalid(format!(
            "{label} must contain exactly {DIGEST_BYTES} bytes"
        ))
    })?;
    if URL_SAFE_NO_PAD.encode(bytes) != encoded {
        return Err(ScopedScopeCoverageContractError::invalid(format!(
            "{label} is not canonical base64url"
        )));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests;
