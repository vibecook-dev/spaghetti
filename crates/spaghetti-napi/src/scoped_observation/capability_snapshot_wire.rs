//! Contextual portable RFC 012D observation-capability snapshot.
//!
//! This freezes one attachment's already-validated, phase-independent
//! capability report behind a deterministic semantic digest. It carries no
//! source coverage, current readiness, root, artifact state, barrier sequence,
//! source-access authority, or portable observer transport. Consumption
//! requires the exact caller-held negotiation and promoted-support context.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::adapter::CompatibilityClass;
use crate::observation_contract::{
    ObservationCapabilities, ObservationContractOffer, ObservationContractSelection,
};

pub(crate) const SCOPED_CAPABILITY_SNAPSHOT_CONTRACT_VERSION: u32 = 1;
pub(crate) const SCOPED_CAPABILITY_DIGEST_CONTRACT_VERSION: u32 = 1;

const CAPABILITY_DIGEST_DOMAIN: &[u8] = b"spaghetti.rfc012d.scoped-capability-snapshot.v1";
const REFERENCE_PREFIX: &str = "v1:";
const DIGEST_BYTES: usize = 32;
const DIGEST_ENCODED_BYTES: usize = 43;
const MAX_CAPABILITY_FAMILIES: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ScopedCapabilitySnapshotContractError {
    #[error("invalid scoped capability snapshot contract: {message}")]
    Invalid { message: String },
    #[error("scoped capability snapshot does not match caller-held context")]
    ContextMismatch,
}

impl ScopedCapabilitySnapshotContractError {
    fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid {
            message: message.into(),
        }
    }
}

/// Caller-held support and negotiation context. This is intentionally
/// non-Serde. Debug output withholds the support-release identity, full offer,
/// family details, and semantic digest.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ScopedCapabilitySnapshotConsumerContext {
    contract_selection: ObservationContractSelection,
    contract_offer: ObservationContractOffer,
    compatibility_class: CompatibilityClass,
    support_release_id: String,
    expected_capabilities: ObservationCapabilities,
    expected_semantic_digest: [u8; DIGEST_BYTES],
}

impl std::fmt::Debug for ScopedCapabilitySnapshotConsumerContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ScopedCapabilitySnapshotConsumerContext")
            .field(
                "selected_family_count",
                &self
                    .contract_selection
                    .contract_versions
                    .fact_family_versions
                    .len(),
            )
            .field(
                "offered_family_count",
                &self
                    .contract_offer
                    .contract_versions
                    .fact_family_versions
                    .len(),
            )
            .field("compatibility_class", &self.compatibility_class)
            .finish_non_exhaustive()
    }
}

impl ScopedCapabilitySnapshotConsumerContext {
    pub(crate) fn from_expected(
        contract_selection: &ObservationContractSelection,
        contract_offer: &ObservationContractOffer,
        compatibility_class: CompatibilityClass,
        support_release_id: &str,
        expected_capabilities: &ObservationCapabilities,
    ) -> Result<Self, ScopedCapabilitySnapshotContractError> {
        let expected_capabilities = ObservationCapabilities::from_wire_value_for_context(
            serde_json::to_value(expected_capabilities).map_err(|error| {
                ScopedCapabilitySnapshotContractError::invalid(error.to_string())
            })?,
            contract_selection,
            contract_offer,
            compatibility_class,
            support_release_id,
        )
        .map_err(|error| ScopedCapabilitySnapshotContractError::invalid(error.to_string()))?;
        let expected_semantic_digest = derive_capability_digest(&expected_capabilities)?;
        Ok(Self {
            contract_selection: contract_selection.clone(),
            contract_offer: contract_offer.clone(),
            compatibility_class,
            support_release_id: support_release_id.to_owned(),
            expected_capabilities,
            expected_semantic_digest,
        })
    }

    pub(crate) fn wire(&self) -> ScopedCapabilitySnapshotContextWire {
        ScopedCapabilitySnapshotContextWire {
            contract_selection: self.contract_selection.clone(),
            contract_offer: self.contract_offer.clone(),
            compatibility_class: self.compatibility_class,
            support_release_id: self.support_release_id.clone(),
            expected_capabilities: self.expected_capabilities.clone(),
            expected_semantic_digest: encode_opaque(&self.expected_semantic_digest),
        }
    }

    pub(crate) fn support_release_id(&self) -> &str {
        &self.support_release_id
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ScopedCapabilitySnapshotContextWire {
    contract_selection: ObservationContractSelection,
    contract_offer: ObservationContractOffer,
    compatibility_class: CompatibilityClass,
    support_release_id: String,
    expected_capabilities: ObservationCapabilities,
    expected_semantic_digest: String,
}

/// Serialize-only phase-independent capability snapshot. A wire payload cannot
/// construct its own support context or semantic-digest expectation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ScopedCapabilitySnapshotWire {
    scoped_capability_snapshot_contract_version: u32,
    capability_digest_contract_version: u32,
    observation_capabilities: ObservationCapabilities,
    semantic_digest: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopedCapabilitySnapshotInput {
    scoped_capability_snapshot_contract_version: u32,
    capability_digest_contract_version: u32,
    observation_capabilities: JsonValue,
    semantic_digest: String,
}

impl ScopedCapabilitySnapshotWire {
    pub(crate) fn from_capabilities(
        observation_capabilities: &ObservationCapabilities,
    ) -> Result<Self, ScopedCapabilitySnapshotContractError> {
        let semantic_digest = derive_capability_digest(observation_capabilities)?;
        Ok(Self {
            scoped_capability_snapshot_contract_version:
                SCOPED_CAPABILITY_SNAPSHOT_CONTRACT_VERSION,
            capability_digest_contract_version: SCOPED_CAPABILITY_DIGEST_CONTRACT_VERSION,
            observation_capabilities: observation_capabilities.clone(),
            semantic_digest: encode_opaque(&semantic_digest),
        })
    }

    pub(crate) fn from_context(
        context: &ScopedCapabilitySnapshotConsumerContext,
    ) -> Result<Self, ScopedCapabilitySnapshotContractError> {
        let wire = Self::from_capabilities(&context.expected_capabilities)?;
        wire.validate_against(context)?;
        Ok(wire)
    }

    pub(crate) fn from_wire_value_for_context(
        value: JsonValue,
        context: &ScopedCapabilitySnapshotConsumerContext,
    ) -> Result<Self, ScopedCapabilitySnapshotContractError> {
        validate_raw_shape(&value)?;
        let input: ScopedCapabilitySnapshotInput = serde_json::from_value(value)
            .map_err(|error| ScopedCapabilitySnapshotContractError::invalid(error.to_string()))?;
        let observation_capabilities = ObservationCapabilities::from_wire_value_for_context(
            input.observation_capabilities,
            &context.contract_selection,
            &context.contract_offer,
            context.compatibility_class,
            &context.support_release_id,
        )
        .map_err(|error| ScopedCapabilitySnapshotContractError::invalid(error.to_string()))?;
        let wire = Self {
            scoped_capability_snapshot_contract_version: input
                .scoped_capability_snapshot_contract_version,
            capability_digest_contract_version: input.capability_digest_contract_version,
            observation_capabilities,
            semantic_digest: input.semantic_digest,
        };
        wire.validate_against(context)?;
        Ok(wire)
    }

    fn validate_against(
        &self,
        context: &ScopedCapabilitySnapshotConsumerContext,
    ) -> Result<(), ScopedCapabilitySnapshotContractError> {
        if self.scoped_capability_snapshot_contract_version
            != SCOPED_CAPABILITY_SNAPSHOT_CONTRACT_VERSION
            || self.capability_digest_contract_version != SCOPED_CAPABILITY_DIGEST_CONTRACT_VERSION
        {
            return Err(ScopedCapabilitySnapshotContractError::invalid(
                "unsupported scoped capability snapshot contract version",
            ));
        }
        let semantic_digest =
            decode_opaque_exact(&self.semantic_digest, "capability semantic digest")?;
        let derived = derive_capability_digest(&self.observation_capabilities)?;
        if self.observation_capabilities != context.expected_capabilities
            || semantic_digest != derived
            || semantic_digest != context.expected_semantic_digest
        {
            return Err(ScopedCapabilitySnapshotContractError::ContextMismatch);
        }
        Ok(())
    }
}

pub(super) fn derive_capability_digest(
    capabilities: &ObservationCapabilities,
) -> Result<[u8; DIGEST_BYTES], ScopedCapabilitySnapshotContractError> {
    capabilities
        .validate()
        .map_err(|error| ScopedCapabilitySnapshotContractError::invalid(error.to_string()))?;
    let canonical = serde_json::to_vec(capabilities)
        .map_err(|error| ScopedCapabilitySnapshotContractError::invalid(error.to_string()))?;
    let mut hasher = blake3::Hasher::new();
    hash_part(&mut hasher, CAPABILITY_DIGEST_DOMAIN);
    hash_part(
        &mut hasher,
        &SCOPED_CAPABILITY_DIGEST_CONTRACT_VERSION.to_be_bytes(),
    );
    hash_part(&mut hasher, &canonical);
    let digest = *hasher.finalize().as_bytes();
    if digest.iter().all(|byte| *byte == 0) {
        return Err(ScopedCapabilitySnapshotContractError::invalid(
            "derived capability semantic digest must not be zero",
        ));
    }
    Ok(digest)
}

fn hash_part(hasher: &mut blake3::Hasher, bytes: &[u8]) {
    hasher.update(&(bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
}

fn validate_raw_shape(value: &JsonValue) -> Result<(), ScopedCapabilitySnapshotContractError> {
    let input = exact_object(
        value,
        "capability snapshot",
        &[
            "scoped_capability_snapshot_contract_version",
            "capability_digest_contract_version",
            "observation_capabilities",
            "semantic_digest",
        ],
    )?;
    let capabilities = exact_object(
        &input["observation_capabilities"],
        "observation capabilities",
        &[
            "observation_capabilities_contract_version",
            "selection",
            "fact_families",
        ],
    )?;
    let families = capabilities["fact_families"].as_array().ok_or_else(|| {
        ScopedCapabilitySnapshotContractError::invalid(
            "observation capability families must be an array",
        )
    })?;
    if families.is_empty() || families.len() > MAX_CAPABILITY_FAMILIES {
        return Err(ScopedCapabilitySnapshotContractError::invalid(
            "observation capability family count is out of bounds",
        ));
    }
    Ok(())
}

fn exact_object<'a>(
    value: &'a JsonValue,
    label: &str,
    fields: &[&str],
) -> Result<&'a serde_json::Map<String, JsonValue>, ScopedCapabilitySnapshotContractError> {
    let object = value.as_object().ok_or_else(|| {
        ScopedCapabilitySnapshotContractError::invalid(format!("{label} must be an object"))
    })?;
    if object.len() != fields.len()
        || fields.iter().any(|field| !object.contains_key(*field))
        || object.keys().any(|field| !fields.contains(&field.as_str()))
    {
        return Err(ScopedCapabilitySnapshotContractError::invalid(format!(
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
) -> Result<[u8; DIGEST_BYTES], ScopedCapabilitySnapshotContractError> {
    let encoded = value.strip_prefix(REFERENCE_PREFIX).ok_or_else(|| {
        ScopedCapabilitySnapshotContractError::invalid(format!("{label} is not v1"))
    })?;
    if encoded.len() != DIGEST_ENCODED_BYTES || encoded.contains('=') {
        return Err(ScopedCapabilitySnapshotContractError::invalid(format!(
            "{label} is not canonical base64url"
        )));
    }
    let decoded = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| {
        ScopedCapabilitySnapshotContractError::invalid(format!(
            "{label} is not canonical base64url"
        ))
    })?;
    if decoded.len() != DIGEST_BYTES || URL_SAFE_NO_PAD.encode(&decoded) != encoded {
        return Err(ScopedCapabilitySnapshotContractError::invalid(format!(
            "{label} must contain exactly {DIGEST_BYTES} canonical bytes"
        )));
    }
    let decoded: [u8; DIGEST_BYTES] = decoded.try_into().map_err(|_| {
        ScopedCapabilitySnapshotContractError::invalid(format!(
            "{label} must contain exactly {DIGEST_BYTES} canonical bytes"
        ))
    })?;
    if decoded.iter().all(|byte| *byte == 0) {
        return Err(ScopedCapabilitySnapshotContractError::invalid(format!(
            "{label} must not be zero"
        )));
    }
    Ok(decoded)
}

#[cfg(test)]
mod tests;
