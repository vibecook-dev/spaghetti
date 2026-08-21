//! Contextual portable RFC 012D replacement-family manifest.
//!
//! This freezes only the selected reducer families, their replacement
//! representations, completeness, counts, and semantic digests. It is not a
//! bootstrap/resync barrier, watermark, source-access proof, or portable
//! observer transport. Consumption requires caller-held negotiation, RFC 012A
//! family coverage, and the exact reducer-derived manifest.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::adapter::{CoverageSetCompleteness, SourceCoverageSet};
use crate::observation_contract::ObservationContractSelection;

use super::{
    selected_replacement_coverage_completeness, ScopedReplacementFamilyManifest,
    ScopedReplacementRepresentation, RUNTIME_ACTOR_AFFILIATION_FACT_FAMILY_CONTRACT_VERSION,
    RUNTIME_ACTOR_RUN_FACT_FAMILY_CONTRACT_VERSION,
    RUNTIME_EFFECTIVE_STATE_FACT_FAMILY_CONTRACT_VERSION,
    RUNTIME_MESSAGE_FACT_FAMILY_CONTRACT_VERSION, RUNTIME_PLAN_FACT_FAMILY_CONTRACT_VERSION,
    RUNTIME_TASK_FACT_FAMILY_CONTRACT_VERSION, RUNTIME_TOOL_FACT_FAMILY_CONTRACT_VERSION,
    RUNTIME_USAGE_V2_FACT_FAMILY_CONTRACT_VERSION, RUNTIME_USER_INPUT_FACT_FAMILY_CONTRACT_VERSION,
    SCOPED_OBSERVATION_IMPLEMENTED_FACT_FAMILIES, SCOPED_REPLACEMENT_DIGEST_CONTRACT_VERSION,
};

pub(crate) const SCOPED_REPLACEMENT_MANIFEST_CONTRACT_VERSION: u32 = 1;

const REFERENCE_PREFIX: &str = "v1:";
const DIGEST_BYTES: usize = 32;
const DIGEST_ENCODED_BYTES: usize = 43;
const MAX_CONTEXT_COVERAGE_SETS: usize = 64;
const MAX_MANIFEST_FAMILIES: usize = SCOPED_OBSERVATION_IMPLEMENTED_FACT_FAMILIES.len();
const JS_SAFE_INTEGER_MAX: u64 = 9_007_199_254_740_991;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ScopedReplacementManifestContractError {
    #[error("invalid scoped replacement manifest contract: {message}")]
    Invalid { message: String },
    #[error("scoped replacement manifest does not match caller-held context")]
    ContextMismatch,
}

impl ScopedReplacementManifestContractError {
    fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid {
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ScopedReplacementRepresentationWire {
    RevisionedEntityCurrent,
    UsageLatestContributionPerResponse,
    CorrelatedLifecycleCurrent,
    CurrentGenerationLog,
    OwnedSetSnapshotCurrent,
}

impl From<ScopedReplacementRepresentation> for ScopedReplacementRepresentationWire {
    fn from(value: ScopedReplacementRepresentation) -> Self {
        match value {
            ScopedReplacementRepresentation::RevisionedEntityCurrent => {
                Self::RevisionedEntityCurrent
            }
            ScopedReplacementRepresentation::UsageLatestContributionPerResponse => {
                Self::UsageLatestContributionPerResponse
            }
            ScopedReplacementRepresentation::CorrelatedLifecycleCurrent => {
                Self::CorrelatedLifecycleCurrent
            }
            ScopedReplacementRepresentation::CurrentGenerationLog => Self::CurrentGenerationLog,
            ScopedReplacementRepresentation::OwnedSetSnapshotCurrent => {
                Self::OwnedSetSnapshotCurrent
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopedReplacementFamilyManifestWire {
    fact_family: String,
    contract_version: u32,
    replacement_representation: ScopedReplacementRepresentationWire,
    completeness: CoverageSetCompleteness,
    entity_or_event_count: u64,
    semantic_digest: String,
}

impl ScopedReplacementFamilyManifestWire {
    fn from_internal(
        value: &ScopedReplacementFamilyManifest,
    ) -> Result<Self, ScopedReplacementManifestContractError> {
        let wire = Self {
            fact_family: value.fact_family.clone(),
            contract_version: value.contract_version,
            replacement_representation: value.replacement_representation.into(),
            completeness: value.completeness,
            entity_or_event_count: value.entity_or_event_count,
            semantic_digest: encode_opaque(value.semantic_digest.as_bytes()),
        };
        wire.validate_shape()?;
        Ok(wire)
    }

    fn validate_shape(&self) -> Result<(), ScopedReplacementManifestContractError> {
        let (contract_version, representation) = family_contract(&self.fact_family)?;
        if self.contract_version != contract_version
            || self.replacement_representation != representation
            || self.entity_or_event_count > JS_SAFE_INTEGER_MAX
        {
            return Err(ScopedReplacementManifestContractError::invalid(
                "replacement family does not match its exact v1 contract",
            ));
        }
        let digest = decode_opaque_exact(
            &self.semantic_digest,
            "replacement semantic digest",
            DIGEST_BYTES,
        )?;
        if digest.iter().all(|byte| *byte == 0) {
            return Err(ScopedReplacementManifestContractError::invalid(
                "replacement semantic digest must not be zero",
            ));
        }
        Ok(())
    }
}

/// Caller-held context. This is intentionally non-Serde and its Debug output
/// withholds family digests and source-coverage coordinates.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ScopedReplacementManifestConsumerContext {
    contract_selection: ObservationContractSelection,
    source_coverage: Vec<SourceCoverageSet>,
    expected_families: Vec<ScopedReplacementFamilyManifestWire>,
}

impl std::fmt::Debug for ScopedReplacementManifestConsumerContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ScopedReplacementManifestConsumerContext")
            .field(
                "selected_family_count",
                &self
                    .contract_selection
                    .contract_versions
                    .fact_family_versions
                    .len(),
            )
            .field("source_coverage_set_count", &self.source_coverage.len())
            .field("expected_family_count", &self.expected_families.len())
            .finish_non_exhaustive()
    }
}

impl ScopedReplacementManifestConsumerContext {
    pub(crate) fn from_expected(
        contract_selection: &ObservationContractSelection,
        source_coverage: &[SourceCoverageSet],
        expected_families: &[ScopedReplacementFamilyManifest],
    ) -> Result<Self, ScopedReplacementManifestContractError> {
        if source_coverage.is_empty() || source_coverage.len() > MAX_CONTEXT_COVERAGE_SETS {
            return Err(ScopedReplacementManifestContractError::invalid(
                "replacement manifest context has an invalid source-coverage set count",
            ));
        }
        let selection_wire = serde_json::to_value(contract_selection)
            .map_err(|error| ScopedReplacementManifestContractError::invalid(error.to_string()))?;
        let contract_selection = ObservationContractSelection::from_wire_value_for_expected(
            selection_wire,
            contract_selection,
        )
        .map_err(|error| ScopedReplacementManifestContractError::invalid(error.to_string()))?;
        let expected_families = expected_families
            .iter()
            .map(ScopedReplacementFamilyManifestWire::from_internal)
            .collect::<Result<Vec<_>, _>>()?;
        validate_context(&contract_selection, source_coverage, &expected_families)?;
        Ok(Self {
            contract_selection,
            source_coverage: source_coverage.to_vec(),
            expected_families,
        })
    }

    pub(crate) fn wire(&self) -> ScopedReplacementManifestContextWire {
        ScopedReplacementManifestContextWire {
            contract_selection: self.contract_selection.clone(),
            source_coverage: self.source_coverage.clone(),
            expected_families: self.expected_families.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ScopedReplacementManifestContextWire {
    contract_selection: ObservationContractSelection,
    source_coverage: Vec<SourceCoverageSet>,
    expected_families: Vec<ScopedReplacementFamilyManifestWire>,
}

/// Serialize-only manifest. A wire payload cannot construct its own selection,
/// coverage completeness, counts, or semantic digest expectations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ScopedReplacementManifestWire {
    scoped_replacement_manifest_contract_version: u32,
    replacement_digest_contract_version: u32,
    contract_selection: ObservationContractSelection,
    families: Vec<ScopedReplacementFamilyManifestWire>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopedReplacementManifestInput {
    scoped_replacement_manifest_contract_version: u32,
    replacement_digest_contract_version: u32,
    contract_selection: JsonValue,
    families: Vec<ScopedReplacementFamilyManifestWire>,
}

impl ScopedReplacementManifestWire {
    pub(crate) fn from_context(
        context: &ScopedReplacementManifestConsumerContext,
    ) -> Result<Self, ScopedReplacementManifestContractError> {
        validate_context(
            &context.contract_selection,
            &context.source_coverage,
            &context.expected_families,
        )?;
        Ok(Self {
            scoped_replacement_manifest_contract_version:
                SCOPED_REPLACEMENT_MANIFEST_CONTRACT_VERSION,
            replacement_digest_contract_version: SCOPED_REPLACEMENT_DIGEST_CONTRACT_VERSION,
            contract_selection: context.contract_selection.clone(),
            families: context.expected_families.clone(),
        })
    }

    pub(crate) fn from_wire_value_for_context(
        value: JsonValue,
        context: &ScopedReplacementManifestConsumerContext,
    ) -> Result<Self, ScopedReplacementManifestContractError> {
        validate_raw_shape(&value)?;
        let input: ScopedReplacementManifestInput = serde_json::from_value(value)
            .map_err(|error| ScopedReplacementManifestContractError::invalid(error.to_string()))?;
        let contract_selection = ObservationContractSelection::from_wire_value_for_expected(
            input.contract_selection,
            &context.contract_selection,
        )
        .map_err(|error| ScopedReplacementManifestContractError::invalid(error.to_string()))?;
        let wire = Self {
            scoped_replacement_manifest_contract_version: input
                .scoped_replacement_manifest_contract_version,
            replacement_digest_contract_version: input.replacement_digest_contract_version,
            contract_selection,
            families: input.families,
        };
        wire.validate_against(context)?;
        Ok(wire)
    }

    fn validate_against(
        &self,
        context: &ScopedReplacementManifestConsumerContext,
    ) -> Result<(), ScopedReplacementManifestContractError> {
        if self.scoped_replacement_manifest_contract_version
            != SCOPED_REPLACEMENT_MANIFEST_CONTRACT_VERSION
            || self.replacement_digest_contract_version
                != SCOPED_REPLACEMENT_DIGEST_CONTRACT_VERSION
            || self.contract_selection != context.contract_selection
        {
            return Err(ScopedReplacementManifestContractError::ContextMismatch);
        }
        validate_context(
            &context.contract_selection,
            &context.source_coverage,
            &self.families,
        )?;
        if self.families != context.expected_families {
            return Err(ScopedReplacementManifestContractError::ContextMismatch);
        }
        Ok(())
    }
}

fn validate_context(
    contract_selection: &ObservationContractSelection,
    source_coverage: &[SourceCoverageSet],
    expected_families: &[ScopedReplacementFamilyManifestWire],
) -> Result<(), ScopedReplacementManifestContractError> {
    if expected_families.is_empty()
        || expected_families.len() > MAX_MANIFEST_FAMILIES
        || expected_families.len()
            != contract_selection
                .contract_versions
                .fact_family_versions
                .len()
    {
        return Err(ScopedReplacementManifestContractError::invalid(
            "replacement manifest family count does not match selection",
        ));
    }
    let mut completeness = selected_replacement_coverage_completeness(
        &contract_selection.contract_versions,
        source_coverage,
    )
    .map_err(|_| {
        ScopedReplacementManifestContractError::invalid(
            "source coverage does not match the selected replacement families",
        )
    })?;
    let mut previous = None::<&str>;
    for family in expected_families {
        family.validate_shape()?;
        if previous.is_some_and(|value| value >= family.fact_family.as_str())
            || contract_selection
                .contract_versions
                .fact_family_versions
                .get(&family.fact_family)
                != Some(&family.contract_version)
            || completeness.remove(family.fact_family.as_str()) != Some(family.completeness)
        {
            return Err(ScopedReplacementManifestContractError::invalid(
                "replacement families are not canonical or coverage-bound",
            ));
        }
        previous = Some(family.fact_family.as_str());
    }
    if !completeness.is_empty() {
        return Err(ScopedReplacementManifestContractError::invalid(
            "replacement coverage contains an unconsumed family",
        ));
    }
    Ok(())
}

fn family_contract(
    family: &str,
) -> Result<(u32, ScopedReplacementRepresentationWire), ScopedReplacementManifestContractError> {
    match family {
        "runtime.actor-affiliation" => Ok((
            RUNTIME_ACTOR_AFFILIATION_FACT_FAMILY_CONTRACT_VERSION,
            ScopedReplacementRepresentationWire::RevisionedEntityCurrent,
        )),
        "runtime.actor-run" => Ok((
            RUNTIME_ACTOR_RUN_FACT_FAMILY_CONTRACT_VERSION,
            ScopedReplacementRepresentationWire::RevisionedEntityCurrent,
        )),
        "runtime.effective-state" => Ok((
            RUNTIME_EFFECTIVE_STATE_FACT_FAMILY_CONTRACT_VERSION,
            ScopedReplacementRepresentationWire::RevisionedEntityCurrent,
        )),
        "runtime.message" => Ok((
            RUNTIME_MESSAGE_FACT_FAMILY_CONTRACT_VERSION,
            ScopedReplacementRepresentationWire::CurrentGenerationLog,
        )),
        "runtime.plan" => Ok((
            RUNTIME_PLAN_FACT_FAMILY_CONTRACT_VERSION,
            ScopedReplacementRepresentationWire::RevisionedEntityCurrent,
        )),
        "runtime.task" => Ok((
            RUNTIME_TASK_FACT_FAMILY_CONTRACT_VERSION,
            ScopedReplacementRepresentationWire::OwnedSetSnapshotCurrent,
        )),
        "runtime.tool" => Ok((
            RUNTIME_TOOL_FACT_FAMILY_CONTRACT_VERSION,
            ScopedReplacementRepresentationWire::CorrelatedLifecycleCurrent,
        )),
        "runtime.usage-v2" => Ok((
            RUNTIME_USAGE_V2_FACT_FAMILY_CONTRACT_VERSION,
            ScopedReplacementRepresentationWire::UsageLatestContributionPerResponse,
        )),
        "runtime.user-input-request" => Ok((
            RUNTIME_USER_INPUT_FACT_FAMILY_CONTRACT_VERSION,
            ScopedReplacementRepresentationWire::CorrelatedLifecycleCurrent,
        )),
        _ => Err(ScopedReplacementManifestContractError::invalid(
            "replacement manifest contains an unsupported family",
        )),
    }
}

fn validate_raw_shape(value: &JsonValue) -> Result<(), ScopedReplacementManifestContractError> {
    let input = exact_object(
        value,
        "replacement manifest",
        &[
            "scoped_replacement_manifest_contract_version",
            "replacement_digest_contract_version",
            "contract_selection",
            "families",
        ],
    )?;
    let families = input["families"].as_array().ok_or_else(|| {
        ScopedReplacementManifestContractError::invalid(
            "replacement manifest families must be an array",
        )
    })?;
    if families.is_empty() || families.len() > MAX_MANIFEST_FAMILIES {
        return Err(ScopedReplacementManifestContractError::invalid(
            "replacement manifest family count is out of bounds",
        ));
    }
    for family in families {
        exact_object(
            family,
            "replacement family",
            &[
                "fact_family",
                "contract_version",
                "replacement_representation",
                "completeness",
                "entity_or_event_count",
                "semantic_digest",
            ],
        )?;
    }
    Ok(())
}

fn exact_object<'a>(
    value: &'a JsonValue,
    label: &str,
    fields: &[&str],
) -> Result<&'a serde_json::Map<String, JsonValue>, ScopedReplacementManifestContractError> {
    let object = value.as_object().ok_or_else(|| {
        ScopedReplacementManifestContractError::invalid(format!("{label} must be an object"))
    })?;
    if object.len() != fields.len()
        || fields.iter().any(|field| !object.contains_key(*field))
        || object.keys().any(|field| !fields.contains(&field.as_str()))
    {
        return Err(ScopedReplacementManifestContractError::invalid(format!(
            "{label} fields do not match the exact contract"
        )));
    }
    Ok(object)
}

fn encode_opaque(bytes: &[u8]) -> String {
    format!("{REFERENCE_PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes))
}

fn decode_opaque_exact(
    value: &str,
    label: &str,
    expected_bytes: usize,
) -> Result<Vec<u8>, ScopedReplacementManifestContractError> {
    let encoded = value.strip_prefix(REFERENCE_PREFIX).ok_or_else(|| {
        ScopedReplacementManifestContractError::invalid(format!("{label} is not v1"))
    })?;
    if expected_bytes != DIGEST_BYTES
        || encoded.len() != DIGEST_ENCODED_BYTES
        || encoded.contains('=')
    {
        return Err(ScopedReplacementManifestContractError::invalid(format!(
            "{label} is not canonical base64url"
        )));
    }
    let decoded = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| {
        ScopedReplacementManifestContractError::invalid(format!(
            "{label} is not canonical base64url"
        ))
    })?;
    if decoded.len() != expected_bytes || URL_SAFE_NO_PAD.encode(&decoded) != encoded {
        return Err(ScopedReplacementManifestContractError::invalid(format!(
            "{label} must contain exactly {expected_bytes} canonical bytes"
        )));
    }
    Ok(decoded)
}

#[cfg(test)]
mod tests;
