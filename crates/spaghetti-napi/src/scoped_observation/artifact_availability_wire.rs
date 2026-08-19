//! Contextual portable RFC 012D artifact-availability snapshot.
//!
//! This is a serialize-only projection of state already frozen by the scoped
//! artifact reducer and bound into completion-barrier identity. It neither
//! creates source observations nor assigns observer ordering. Consumption
//! requires the exact caller-held negotiation, root session, entries, and
//! Rust-derived semantic digest.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value as JsonValue;

use crate::adapter::CanonicalEntityKey;
use crate::observation_contract::ObservationContractSelection;

use super::artifact_availability::{
    ScopedArtifactAvailabilitySnapshot, ScopedArtifactAvailabilityState,
    SCOPED_ARTIFACT_AVAILABILITY_CONTRACT_VERSION,
};
use super::artifact_evidence::MAX_SCOPED_ARTIFACT_EVIDENCE_ASSERTIONS;

const REFERENCE_PREFIX: &str = "v1:";
const DIGEST_BYTES: usize = 32;
const DIGEST_ENCODED_BYTES: usize = 43;
const MAX_ARTIFACT_KIND_BYTES: usize = 128;
const JS_SAFE_INTEGER_MAX: u64 = 9_007_199_254_740_991;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ScopedArtifactAvailabilityContractError {
    #[error("invalid scoped artifact-availability contract: {message}")]
    Invalid { message: String },
    #[error("scoped artifact-availability snapshot does not match caller-held context")]
    ContextMismatch,
}

impl ScopedArtifactAvailabilityContractError {
    fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid {
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum ScopedArtifactAvailabilityStateWire {
    Available {
        generation: u64,
        provenance_ref: String,
        size_bytes: u64,
    },
    Missing {
        #[serde(deserialize_with = "deserialize_required_option")]
        observed_generation: Option<u64>,
        #[serde(deserialize_with = "deserialize_required_option")]
        provenance_ref: Option<String>,
    },
    OverLimit {
        generation: u64,
        provenance_ref: String,
        observed_bytes: u64,
        request_max_bytes: u64,
    },
    Unstable,
}

impl ScopedArtifactAvailabilityStateWire {
    pub(super) fn from_internal(state: ScopedArtifactAvailabilityState) -> Self {
        match state {
            ScopedArtifactAvailabilityState::Available {
                generation,
                provenance_ref,
                size_bytes,
            } => Self::Available {
                generation,
                provenance_ref: encode_opaque(&provenance_ref),
                size_bytes,
            },
            ScopedArtifactAvailabilityState::Missing {
                observed_generation,
                provenance_ref,
            } => Self::Missing {
                observed_generation,
                provenance_ref: provenance_ref.map(|value| encode_opaque(&value)),
            },
            ScopedArtifactAvailabilityState::OverLimit {
                generation,
                provenance_ref,
                observed_bytes,
                request_max_bytes,
            } => Self::OverLimit {
                generation,
                provenance_ref: encode_opaque(&provenance_ref),
                observed_bytes,
                request_max_bytes,
            },
            ScopedArtifactAvailabilityState::Unstable => Self::Unstable,
        }
    }

    pub(super) fn validate_shape(&self) -> Result<(), ScopedArtifactAvailabilityContractError> {
        match self {
            Self::Available {
                generation,
                provenance_ref,
                size_bytes,
            } => {
                validate_positive_portable("artifact availability generation", *generation)?;
                decode_opaque_exact(provenance_ref, "artifact availability provenance")?;
                validate_portable("artifact availability size", *size_bytes)
            }
            Self::Missing {
                observed_generation,
                provenance_ref,
            } => {
                if observed_generation.is_some() != provenance_ref.is_some() {
                    return Err(ScopedArtifactAvailabilityContractError::invalid(
                        "missing availability generation and provenance must be present together",
                    ));
                }
                if let Some(generation) = observed_generation {
                    validate_positive_portable(
                        "missing artifact observed generation",
                        *generation,
                    )?;
                }
                if let Some(reference) = provenance_ref {
                    decode_opaque_exact(reference, "missing artifact provenance")?;
                }
                Ok(())
            }
            Self::OverLimit {
                generation,
                provenance_ref,
                observed_bytes,
                request_max_bytes,
            } => {
                validate_positive_portable("over-limit artifact generation", *generation)?;
                decode_opaque_exact(provenance_ref, "over-limit artifact provenance")?;
                validate_portable("over-limit artifact observed bytes", *observed_bytes)?;
                validate_positive_portable(
                    "over-limit artifact request maximum",
                    *request_max_bytes,
                )?;
                if observed_bytes <= request_max_bytes {
                    return Err(ScopedArtifactAvailabilityContractError::invalid(
                        "over-limit artifact must exceed its observed request maximum",
                    ));
                }
                Ok(())
            }
            Self::Unstable => Ok(()),
        }
    }

    pub(super) fn matches_source_generation(&self, source_generation: u64) -> bool {
        match self {
            Self::Available { generation, .. } | Self::OverLimit { generation, .. } => {
                *generation == source_generation
            }
            Self::Missing {
                observed_generation,
                ..
            } => observed_generation.unwrap_or(1) == source_generation,
            Self::Unstable => true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ScopedArtifactAvailabilityEntryWire {
    pub(super) artifact_key: CanonicalEntityKey,
    pub(super) artifact_kind: String,
    pub(super) revision: String,
    pub(super) state: ScopedArtifactAvailabilityStateWire,
}

impl ScopedArtifactAvailabilityEntryWire {
    pub(super) fn from_internal(entry: &super::ScopedArtifactAvailabilityEntry) -> Self {
        Self {
            artifact_key: entry.artifact_key(),
            artifact_kind: entry.artifact_kind().to_owned(),
            revision: encode_opaque(entry.revision().as_bytes()),
            state: ScopedArtifactAvailabilityStateWire::from_internal(entry.state()),
        }
    }

    pub(super) fn validate_shape(&self) -> Result<(), ScopedArtifactAvailabilityContractError> {
        validate_identifier(&self.artifact_kind, "artifact availability kind")?;
        decode_opaque_exact(&self.revision, "artifact availability revision")?;
        self.state.validate_shape()
    }
}

/// Caller-held context. It is intentionally non-Serde, and Debug withholds
/// root, artifact identities, kinds, state, revisions, and semantic digest.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ScopedArtifactAvailabilityConsumerContext {
    contract_selection: ObservationContractSelection,
    root_session_key: CanonicalEntityKey,
    expected_entry_count: u64,
    expected_semantic_digest: String,
    expected_entries: Vec<ScopedArtifactAvailabilityEntryWire>,
}

impl std::fmt::Debug for ScopedArtifactAvailabilityConsumerContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ScopedArtifactAvailabilityConsumerContext")
            .field("expected_entry_count", &self.expected_entry_count)
            .finish_non_exhaustive()
    }
}

impl ScopedArtifactAvailabilityConsumerContext {
    pub(crate) fn from_expected(
        contract_selection: &ObservationContractSelection,
        root_session_key: CanonicalEntityKey,
        expected: &ScopedArtifactAvailabilitySnapshot,
    ) -> Result<Self, ScopedArtifactAvailabilityContractError> {
        if !expected.validate_for_root(root_session_key) {
            return Err(ScopedArtifactAvailabilityContractError::invalid(
                "expected artifact availability is not a valid current-root snapshot",
            ));
        }
        let contract_selection = ObservationContractSelection::from_wire_value_for_expected(
            serde_json::to_value(contract_selection).map_err(|error| {
                ScopedArtifactAvailabilityContractError::invalid(error.to_string())
            })?,
            contract_selection,
        )
        .map_err(|error| ScopedArtifactAvailabilityContractError::invalid(error.to_string()))?;
        let expected_entries = expected
            .entries()
            .iter()
            .map(ScopedArtifactAvailabilityEntryWire::from_internal)
            .collect::<Vec<_>>();
        validate_entries(expected.entry_count(), &expected_entries)?;
        Ok(Self {
            contract_selection,
            root_session_key,
            expected_entry_count: expected.entry_count(),
            expected_semantic_digest: encode_opaque(expected.semantic_digest().as_bytes()),
            expected_entries,
        })
    }

    pub(crate) fn wire(&self) -> ScopedArtifactAvailabilityContextWire {
        ScopedArtifactAvailabilityContextWire {
            contract_selection: self.contract_selection.clone(),
            root_session_key: self.root_session_key,
            expected_entry_count: self.expected_entry_count,
            expected_semantic_digest: self.expected_semantic_digest.clone(),
            expected_entries: self.expected_entries.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ScopedArtifactAvailabilityContextWire {
    contract_selection: ObservationContractSelection,
    root_session_key: CanonicalEntityKey,
    expected_entry_count: u64,
    expected_semantic_digest: String,
    expected_entries: Vec<ScopedArtifactAvailabilityEntryWire>,
}

/// Serialize-only current-state snapshot. A received payload cannot establish
/// its own root, selected contract, entry set, revisions, or semantic digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ScopedArtifactAvailabilitySnapshotWire {
    scoped_artifact_availability_contract_version: u32,
    contract_selection: ObservationContractSelection,
    root_session_key: CanonicalEntityKey,
    entry_count: u64,
    semantic_digest: String,
    entries: Vec<ScopedArtifactAvailabilityEntryWire>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopedArtifactAvailabilitySnapshotInput {
    scoped_artifact_availability_contract_version: u32,
    contract_selection: JsonValue,
    root_session_key: CanonicalEntityKey,
    entry_count: u64,
    semantic_digest: String,
    entries: Vec<ScopedArtifactAvailabilityEntryWire>,
}

impl ScopedArtifactAvailabilitySnapshotWire {
    pub(crate) fn from_context(
        context: &ScopedArtifactAvailabilityConsumerContext,
    ) -> Result<Self, ScopedArtifactAvailabilityContractError> {
        let wire = Self {
            scoped_artifact_availability_contract_version:
                SCOPED_ARTIFACT_AVAILABILITY_CONTRACT_VERSION,
            contract_selection: context.contract_selection.clone(),
            root_session_key: context.root_session_key,
            entry_count: context.expected_entry_count,
            semantic_digest: context.expected_semantic_digest.clone(),
            entries: context.expected_entries.clone(),
        };
        wire.validate_against(context)?;
        Ok(wire)
    }

    pub(crate) fn from_wire_value_for_context(
        value: JsonValue,
        context: &ScopedArtifactAvailabilityConsumerContext,
    ) -> Result<Self, ScopedArtifactAvailabilityContractError> {
        preflight_wire_value(&value)?;
        let input: ScopedArtifactAvailabilitySnapshotInput = serde_json::from_value(value)
            .map_err(|error| ScopedArtifactAvailabilityContractError::invalid(error.to_string()))?;
        let contract_selection = ObservationContractSelection::from_wire_value_for_expected(
            input.contract_selection,
            &context.contract_selection,
        )
        .map_err(|error| ScopedArtifactAvailabilityContractError::invalid(error.to_string()))?;
        let wire = Self {
            scoped_artifact_availability_contract_version: input
                .scoped_artifact_availability_contract_version,
            contract_selection,
            root_session_key: input.root_session_key,
            entry_count: input.entry_count,
            semantic_digest: input.semantic_digest,
            entries: input.entries,
        };
        wire.validate_against(context)?;
        Ok(wire)
    }

    fn validate_against(
        &self,
        context: &ScopedArtifactAvailabilityConsumerContext,
    ) -> Result<(), ScopedArtifactAvailabilityContractError> {
        validate_entries(self.entry_count, &self.entries)?;
        decode_opaque_exact(
            &self.semantic_digest,
            "artifact availability semantic digest",
        )?;
        if self.scoped_artifact_availability_contract_version
            != SCOPED_ARTIFACT_AVAILABILITY_CONTRACT_VERSION
            || self.contract_selection != context.contract_selection
            || self.root_session_key != context.root_session_key
            || self.entry_count != context.expected_entry_count
            || self.semantic_digest != context.expected_semantic_digest
            || self.entries != context.expected_entries
        {
            return Err(ScopedArtifactAvailabilityContractError::ContextMismatch);
        }
        Ok(())
    }
}

fn validate_entries(
    entry_count: u64,
    entries: &[ScopedArtifactAvailabilityEntryWire],
) -> Result<(), ScopedArtifactAvailabilityContractError> {
    if u64::try_from(entries.len()) != Ok(entry_count)
        || entries.len() > MAX_SCOPED_ARTIFACT_EVIDENCE_ASSERTIONS
    {
        return Err(ScopedArtifactAvailabilityContractError::invalid(
            "artifact availability entry count is invalid",
        ));
    }
    for entry in entries {
        entry.validate_shape()?;
    }
    if entries.windows(2).any(|window| {
        (&window[0].artifact_key, window[0].artifact_kind.as_str())
            >= (&window[1].artifact_key, window[1].artifact_kind.as_str())
    }) {
        return Err(ScopedArtifactAvailabilityContractError::invalid(
            "artifact availability entries are not canonical and unique",
        ));
    }
    Ok(())
}

fn preflight_wire_value(value: &JsonValue) -> Result<(), ScopedArtifactAvailabilityContractError> {
    let object = value.as_object().ok_or_else(|| {
        ScopedArtifactAvailabilityContractError::invalid(
            "artifact availability snapshot must be an object",
        )
    })?;
    let top_fields = [
        "scoped_artifact_availability_contract_version",
        "contract_selection",
        "root_session_key",
        "entry_count",
        "semantic_digest",
        "entries",
    ];
    if object.len() != top_fields.len()
        || top_fields.iter().any(|field| !object.contains_key(*field))
        || !has_exact_reference_length(object.get("root_session_key"))
        || !has_exact_reference_length(object.get("semantic_digest"))
    {
        return Err(ScopedArtifactAvailabilityContractError::invalid(
            "artifact availability snapshot has a missing, unknown, or oversized field",
        ));
    }
    let entries = object
        .get("entries")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| {
            ScopedArtifactAvailabilityContractError::invalid(
                "artifact availability entries must be an array",
            )
        })?;
    if entries.len() > MAX_SCOPED_ARTIFACT_EVIDENCE_ASSERTIONS {
        return Err(ScopedArtifactAvailabilityContractError::invalid(
            "artifact availability entries exceed the portable bound",
        ));
    }
    for entry in entries {
        preflight_entry_value(entry)?;
    }
    Ok(())
}

fn preflight_state(value: &JsonValue) -> Result<(), ScopedArtifactAvailabilityContractError> {
    let state = value.as_object().ok_or_else(|| {
        ScopedArtifactAvailabilityContractError::invalid(
            "artifact availability state must be an object",
        )
    })?;
    let fields: &[&str] = match state.get("kind").and_then(JsonValue::as_str) {
        Some("available") => &["kind", "generation", "provenance_ref", "size_bytes"],
        Some("missing") => &["kind", "observed_generation", "provenance_ref"],
        Some("over_limit") => &[
            "kind",
            "generation",
            "provenance_ref",
            "observed_bytes",
            "request_max_bytes",
        ],
        Some("unstable") => &["kind"],
        _ => {
            return Err(ScopedArtifactAvailabilityContractError::invalid(
                "artifact availability state kind is unsupported",
            ));
        }
    };
    if state.len() != fields.len() || fields.iter().any(|field| !state.contains_key(*field)) {
        return Err(ScopedArtifactAvailabilityContractError::invalid(
            "artifact availability state has a missing or unknown field",
        ));
    }
    if state
        .get("provenance_ref")
        .is_some_and(|value| !value.is_null() && !has_exact_reference_length(Some(value)))
    {
        return Err(ScopedArtifactAvailabilityContractError::invalid(
            "artifact availability provenance exceeds the pre-decode bound",
        ));
    }
    Ok(())
}

pub(super) fn preflight_entry_value(
    value: &JsonValue,
) -> Result<(), ScopedArtifactAvailabilityContractError> {
    let entry = value.as_object().ok_or_else(|| {
        ScopedArtifactAvailabilityContractError::invalid(
            "artifact availability entry must be an object",
        )
    })?;
    let entry_fields = ["artifact_key", "artifact_kind", "revision", "state"];
    if entry.len() != entry_fields.len()
        || entry_fields.iter().any(|field| !entry.contains_key(*field))
        || !has_exact_reference_length(entry.get("artifact_key"))
        || entry
            .get("artifact_kind")
            .and_then(JsonValue::as_str)
            .is_none_or(|value| value.len() > MAX_ARTIFACT_KIND_BYTES)
        || !has_exact_reference_length(entry.get("revision"))
    {
        return Err(ScopedArtifactAvailabilityContractError::invalid(
            "artifact availability entry exceeds a pre-decode bound",
        ));
    }
    preflight_state(entry.get("state").ok_or_else(|| {
        ScopedArtifactAvailabilityContractError::invalid(
            "artifact availability entry is missing state",
        )
    })?)
}

fn has_exact_reference_length(value: Option<&JsonValue>) -> bool {
    value
        .and_then(JsonValue::as_str)
        .is_some_and(|value| value.len() == REFERENCE_PREFIX.len() + DIGEST_ENCODED_BYTES)
}

fn validate_identifier(
    value: &str,
    label: &str,
) -> Result<(), ScopedArtifactAvailabilityContractError> {
    if value.is_empty()
        || value.len() > MAX_ARTIFACT_KIND_BYTES
        || !value.as_bytes()[0].is_ascii_lowercase()
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte)
        })
    {
        return Err(ScopedArtifactAvailabilityContractError::invalid(format!(
            "{label} is not canonical"
        )));
    }
    Ok(())
}

fn validate_positive_portable(
    label: &str,
    value: u64,
) -> Result<(), ScopedArtifactAvailabilityContractError> {
    if value == 0 {
        return Err(ScopedArtifactAvailabilityContractError::invalid(format!(
            "{label} must be positive"
        )));
    }
    validate_portable(label, value)
}

fn validate_portable(
    label: &str,
    value: u64,
) -> Result<(), ScopedArtifactAvailabilityContractError> {
    if value > JS_SAFE_INTEGER_MAX {
        return Err(ScopedArtifactAvailabilityContractError::invalid(format!(
            "{label} exceeds the portable integer bound"
        )));
    }
    Ok(())
}

fn encode_opaque(value: &[u8; DIGEST_BYTES]) -> String {
    format!("{REFERENCE_PREFIX}{}", URL_SAFE_NO_PAD.encode(value))
}

fn decode_opaque_exact(
    value: &str,
    label: &str,
) -> Result<[u8; DIGEST_BYTES], ScopedArtifactAvailabilityContractError> {
    let encoded = value.strip_prefix(REFERENCE_PREFIX).ok_or_else(|| {
        ScopedArtifactAvailabilityContractError::invalid(format!(
            "{label} must use the v1 opaque-reference prefix"
        ))
    })?;
    if encoded.len() != DIGEST_ENCODED_BYTES || encoded.contains('=') {
        return Err(ScopedArtifactAvailabilityContractError::invalid(format!(
            "{label} has the wrong encoded length"
        )));
    }
    let bytes = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| {
        ScopedArtifactAvailabilityContractError::invalid(format!(
            "{label} is not canonical base64url"
        ))
    })?;
    let bytes: [u8; DIGEST_BYTES] = bytes.try_into().map_err(|_| {
        ScopedArtifactAvailabilityContractError::invalid(format!(
            "{label} has the wrong decoded length"
        ))
    })?;
    if bytes.iter().all(|byte| *byte == 0) || URL_SAFE_NO_PAD.encode(bytes) != encoded {
        return Err(ScopedArtifactAvailabilityContractError::invalid(format!(
            "{label} is zero or noncanonical"
        )));
    }
    Ok(bytes)
}

fn deserialize_required_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

#[cfg(test)]
mod tests;
