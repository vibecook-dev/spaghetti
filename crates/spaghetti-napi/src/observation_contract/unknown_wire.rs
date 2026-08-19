//! Sidecar negotiation and bounded preservation for additive RFC 012D events.
//!
//! This contract is deliberately separate from the already-frozen base
//! observation selection. When negotiated, it is selected against that exact
//! caller-held value before source access, and it never upgrades an unknown
//! event into a known semantic family. Runtime attachment, emission, and public
//! transport remain separate gates.

use std::collections::BTreeMap;
use std::fmt;
use std::io::{self, Write};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map as JsonMap, Value as JsonValue};

use crate::adapter::{
    CanonicalSourceInstanceKey, CoverageObjectKey, CoverageStreamKey, SemanticRevisionRef,
};

use super::ObservationContractSelection;

pub(crate) const OBSERVATION_UNKNOWN_WIRE_NEGOTIATION_CONTRACT_VERSION: u32 = 1;
pub(crate) const OBSERVATION_UNKNOWN_WIRE_EVENT_CONTRACT_VERSION: u32 = 1;
const MAX_UNKNOWN_WIRE_PRESERVED_BYTES: u32 = 64 * 1024;
const MAX_UNKNOWN_WIRE_DEPTH: usize = 16;
const MAX_UNKNOWN_WIRE_NODES: usize = 1_024;
const MAX_UNKNOWN_WIRE_TYPE_TAG_BYTES: usize = 128;
const MAX_UNKNOWN_WIRE_OBJECT_KEY_BYTES: usize = 256;
const MAX_JAVASCRIPT_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
const DIGEST_BYTES: usize = 32;
const UNKNOWN_WIRE_FAMILY: &str = "unknown_wire_event";

pub(crate) const KNOWN_EVENT_TYPE_TAGS: &[&str] = &[
    "actor_affiliation",
    "actor_run",
    "artifact_availability",
    "observer_bootstrap_complete",
    "observer_failed",
    "observer_resync_complete",
    "observer_resync_required",
    "observer_resync_started",
    "source_created",
    "source_deleted",
    "source_object_error",
    "source_reset",
    "usage_v2",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ObservationUnknownWireCompatibilityAxis {
    EventContractVersion,
    TypeTagPreservation,
    EncodedValuePreservation,
    EnvelopeProvenancePreservation,
}

impl fmt::Display for ObservationUnknownWireCompatibilityAxis {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::EventContractVersion => "event_contract_version",
            Self::TypeTagPreservation => "type_tag_preservation",
            Self::EncodedValuePreservation => "encoded_value_preservation",
            Self::EnvelopeProvenancePreservation => "envelope_provenance_preservation",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ObservationUnknownWireContractError {
    #[error("invalid observation unknown-wire contract: {message}")]
    Invalid { message: String },
    #[error("IncompatibleObservationUnknownWireContract: {axis}")]
    Incompatible {
        axis: ObservationUnknownWireCompatibilityAxis,
    },
}

impl ObservationUnknownWireContractError {
    fn invalid(error: impl fmt::Display) -> Self {
        Self::Invalid {
            message: error.to_string(),
        }
    }

    fn incompatible(axis: ObservationUnknownWireCompatibilityAxis) -> Self {
        Self::Incompatible { axis }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ObservationUnknownWireCapability {
    unknown_wire_event_contract_version: u32,
    preserves_type_tag: bool,
    preserves_encoded_value: bool,
    preserves_envelope_provenance: bool,
    max_preserved_bytes: u32,
}

impl ObservationUnknownWireCapability {
    pub(crate) fn preserving(
        max_preserved_bytes: u32,
    ) -> Result<Self, ObservationUnknownWireContractError> {
        let value = Self {
            unknown_wire_event_contract_version: OBSERVATION_UNKNOWN_WIRE_EVENT_CONTRACT_VERSION,
            preserves_type_tag: true,
            preserves_encoded_value: true,
            preserves_envelope_provenance: true,
            max_preserved_bytes,
        };
        value.validate_shape()?;
        Ok(value)
    }

    fn validate_shape(&self) -> Result<(), ObservationUnknownWireContractError> {
        if self.unknown_wire_event_contract_version == 0 {
            return Err(ObservationUnknownWireContractError::invalid(
                "unknown-wire event contract version must be greater than zero",
            ));
        }
        if self.max_preserved_bytes == 0
            || self.max_preserved_bytes > MAX_UNKNOWN_WIRE_PRESERVED_BYTES
        {
            return Err(ObservationUnknownWireContractError::invalid(format!(
                "unknown-wire preserved-byte bound must be 1..={MAX_UNKNOWN_WIRE_PRESERVED_BYTES}"
            )));
        }
        Ok(())
    }

    fn validate_selected(&self) -> Result<(), ObservationUnknownWireContractError> {
        self.validate_shape()?;
        if self.unknown_wire_event_contract_version
            != OBSERVATION_UNKNOWN_WIRE_EVENT_CONTRACT_VERSION
            || !self.preserves_type_tag
            || !self.preserves_encoded_value
            || !self.preserves_envelope_provenance
        {
            return Err(ObservationUnknownWireContractError::invalid(
                "unknown-wire selection does not preserve the exact v1 carrier",
            ));
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for ObservationUnknownWireCapability {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            unknown_wire_event_contract_version: u32,
            preserves_type_tag: bool,
            preserves_encoded_value: bool,
            preserves_envelope_provenance: bool,
            max_preserved_bytes: u32,
        }

        let wire = Wire::deserialize(deserializer)?;
        let value = Self {
            unknown_wire_event_contract_version: wire.unknown_wire_event_contract_version,
            preserves_type_tag: wire.preserves_type_tag,
            preserves_encoded_value: wire.preserves_encoded_value,
            preserves_envelope_provenance: wire.preserves_envelope_provenance,
            max_preserved_bytes: wire.max_preserved_bytes,
        };
        value.validate_shape().map_err(D::Error::custom)?;
        Ok(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ObservationUnknownWireContractRequest {
    observation_unknown_wire_negotiation_contract_version: u32,
    capability: ObservationUnknownWireCapability,
}

impl ObservationUnknownWireContractRequest {
    pub(crate) fn new(
        capability: ObservationUnknownWireCapability,
    ) -> Result<Self, ObservationUnknownWireContractError> {
        let value = Self {
            observation_unknown_wire_negotiation_contract_version:
                OBSERVATION_UNKNOWN_WIRE_NEGOTIATION_CONTRACT_VERSION,
            capability,
        };
        value.validate()?;
        Ok(value)
    }

    fn validate(&self) -> Result<(), ObservationUnknownWireContractError> {
        if self.observation_unknown_wire_negotiation_contract_version
            != OBSERVATION_UNKNOWN_WIRE_NEGOTIATION_CONTRACT_VERSION
        {
            return Err(ObservationUnknownWireContractError::invalid(
                "unsupported unknown-wire request version",
            ));
        }
        self.capability.validate_shape()
    }
}

impl<'de> Deserialize<'de> for ObservationUnknownWireContractRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            observation_unknown_wire_negotiation_contract_version: u32,
            capability: ObservationUnknownWireCapability,
        }

        let wire = Wire::deserialize(deserializer)?;
        let value = Self {
            observation_unknown_wire_negotiation_contract_version: wire
                .observation_unknown_wire_negotiation_contract_version,
            capability: wire.capability,
        };
        value.validate().map_err(D::Error::custom)?;
        Ok(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ObservationUnknownWireContractOffer {
    observation_unknown_wire_negotiation_contract_version: u32,
    capability: ObservationUnknownWireCapability,
}

impl ObservationUnknownWireContractOffer {
    pub(crate) fn new(
        capability: ObservationUnknownWireCapability,
    ) -> Result<Self, ObservationUnknownWireContractError> {
        let value = Self {
            observation_unknown_wire_negotiation_contract_version:
                OBSERVATION_UNKNOWN_WIRE_NEGOTIATION_CONTRACT_VERSION,
            capability,
        };
        value.validate()?;
        Ok(value)
    }

    fn validate(&self) -> Result<(), ObservationUnknownWireContractError> {
        if self.observation_unknown_wire_negotiation_contract_version
            != OBSERVATION_UNKNOWN_WIRE_NEGOTIATION_CONTRACT_VERSION
        {
            return Err(ObservationUnknownWireContractError::invalid(
                "unsupported unknown-wire offer version",
            ));
        }
        self.capability.validate_shape()
    }
}

impl<'de> Deserialize<'de> for ObservationUnknownWireContractOffer {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            observation_unknown_wire_negotiation_contract_version: u32,
            capability: ObservationUnknownWireCapability,
        }

        let wire = Wire::deserialize(deserializer)?;
        let value = Self {
            observation_unknown_wire_negotiation_contract_version: wire
                .observation_unknown_wire_negotiation_contract_version,
            capability: wire.capability,
        };
        value.validate().map_err(D::Error::custom)?;
        Ok(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ObservationUnknownWireContractSelection {
    observation_unknown_wire_negotiation_contract_version: u32,
    observation_selection: ObservationContractSelection,
    capability: ObservationUnknownWireCapability,
}

impl ObservationUnknownWireContractSelection {
    pub(crate) fn observation_selection(&self) -> &ObservationContractSelection {
        &self.observation_selection
    }

    pub(crate) fn max_preserved_bytes(&self) -> u32 {
        self.capability.max_preserved_bytes
    }

    fn validate(&self) -> Result<(), ObservationUnknownWireContractError> {
        if self.observation_unknown_wire_negotiation_contract_version
            != OBSERVATION_UNKNOWN_WIRE_NEGOTIATION_CONTRACT_VERSION
        {
            return Err(ObservationUnknownWireContractError::invalid(
                "unsupported unknown-wire selection version",
            ));
        }
        self.observation_selection
            .validate()
            .map_err(ObservationUnknownWireContractError::invalid)?;
        self.capability.validate_selected()
    }

    pub(crate) fn from_wire_value_for_negotiation(
        value: JsonValue,
        request: &ObservationUnknownWireContractRequest,
        offer: &ObservationUnknownWireContractOffer,
        observation_selection: &ObservationContractSelection,
    ) -> Result<Self, ObservationUnknownWireContractError> {
        let parsed = Self::from_wire_value_for_observation_selection(value, observation_selection)?;
        if parsed != negotiate_observation_unknown_wire(request, offer, observation_selection)? {
            return Err(ObservationUnknownWireContractError::invalid(
                "unknown-wire selection does not match the exact negotiated result",
            ));
        }
        Ok(parsed)
    }

    fn from_wire_value_for_observation_selection(
        value: JsonValue,
        expected_observation_selection: &ObservationContractSelection,
    ) -> Result<Self, ObservationUnknownWireContractError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            observation_unknown_wire_negotiation_contract_version: u32,
            observation_selection: JsonValue,
            capability: ObservationUnknownWireCapability,
        }

        let wire = serde_json::from_value::<Wire>(value)
            .map_err(ObservationUnknownWireContractError::invalid)?;
        let observation_selection = ObservationContractSelection::from_wire_value_for_expected(
            wire.observation_selection,
            expected_observation_selection,
        )
        .map_err(ObservationUnknownWireContractError::invalid)?;
        let value = Self {
            observation_unknown_wire_negotiation_contract_version: wire
                .observation_unknown_wire_negotiation_contract_version,
            observation_selection,
            capability: wire.capability,
        };
        value.validate()?;
        Ok(value)
    }

    pub(crate) fn from_wire_value_for_expected(
        value: JsonValue,
        expected: &Self,
    ) -> Result<Self, ObservationUnknownWireContractError> {
        let parsed = Self::from_wire_value_for_observation_selection(
            value,
            &expected.observation_selection,
        )?;
        if &parsed != expected {
            return Err(ObservationUnknownWireContractError::invalid(
                "unknown-wire selection does not match caller-held state",
            ));
        }
        Ok(parsed)
    }
}

pub(crate) fn negotiate_observation_unknown_wire(
    request: &ObservationUnknownWireContractRequest,
    offer: &ObservationUnknownWireContractOffer,
    observation_selection: &ObservationContractSelection,
) -> Result<ObservationUnknownWireContractSelection, ObservationUnknownWireContractError> {
    request.validate()?;
    offer.validate()?;
    observation_selection
        .validate()
        .map_err(ObservationUnknownWireContractError::invalid)?;

    if request.capability.unknown_wire_event_contract_version
        != OBSERVATION_UNKNOWN_WIRE_EVENT_CONTRACT_VERSION
        || offer.capability.unknown_wire_event_contract_version
            != OBSERVATION_UNKNOWN_WIRE_EVENT_CONTRACT_VERSION
    {
        return Err(ObservationUnknownWireContractError::incompatible(
            ObservationUnknownWireCompatibilityAxis::EventContractVersion,
        ));
    }
    for (requested, offered, axis) in [
        (
            request.capability.preserves_type_tag,
            offer.capability.preserves_type_tag,
            ObservationUnknownWireCompatibilityAxis::TypeTagPreservation,
        ),
        (
            request.capability.preserves_encoded_value,
            offer.capability.preserves_encoded_value,
            ObservationUnknownWireCompatibilityAxis::EncodedValuePreservation,
        ),
        (
            request.capability.preserves_envelope_provenance,
            offer.capability.preserves_envelope_provenance,
            ObservationUnknownWireCompatibilityAxis::EnvelopeProvenancePreservation,
        ),
    ] {
        if !requested || !offered {
            return Err(ObservationUnknownWireContractError::incompatible(axis));
        }
    }

    let value = ObservationUnknownWireContractSelection {
        observation_unknown_wire_negotiation_contract_version:
            OBSERVATION_UNKNOWN_WIRE_NEGOTIATION_CONTRACT_VERSION,
        observation_selection: observation_selection.clone(),
        capability: ObservationUnknownWireCapability {
            unknown_wire_event_contract_version: OBSERVATION_UNKNOWN_WIRE_EVENT_CONTRACT_VERSION,
            preserves_type_tag: true,
            preserves_encoded_value: true,
            preserves_envelope_provenance: true,
            max_preserved_bytes: request
                .capability
                .max_preserved_bytes
                .min(offer.capability.max_preserved_bytes),
        },
    };
    value.validate()?;
    Ok(value)
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ObservationUnknownWireProvenance {
    observation_selection: ObservationContractSelection,
    observer_sequence: u64,
    scope_epoch: u64,
    event_id: String,
    semantic_revision_ref: Option<SemanticRevisionRef>,
    source_instance_key: CanonicalSourceInstanceKey,
    stream_key: CoverageStreamKey,
    object_key: CoverageObjectKey,
    generation: u64,
    observed_at: i64,
    phase: String,
    additional_envelope_provenance: BTreeMap<String, JsonValue>,
}

impl fmt::Debug for ObservationUnknownWireProvenance {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ObservationUnknownWireProvenance")
            .field("observer_sequence", &self.observer_sequence)
            .field("scope_epoch", &self.scope_epoch)
            .field("generation", &self.generation)
            .field("phase", &self.phase)
            .field(
                "additional_envelope_provenance",
                &"<redacted-preserved-value>",
            )
            .finish_non_exhaustive()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ObservationUnknownWireEvent {
    type_tag: String,
    encoded_value: JsonValue,
    provenance: ObservationUnknownWireProvenance,
}

impl fmt::Debug for ObservationUnknownWireEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ObservationUnknownWireEvent")
            .field("type_tag", &self.type_tag)
            .field("encoded_value", &"<redacted-preserved-value>")
            .field("provenance", &self.provenance)
            .finish_non_exhaustive()
    }
}

impl ObservationUnknownWireEvent {
    pub(crate) fn from_wire_value(
        value: JsonValue,
        selection: &ObservationUnknownWireContractSelection,
    ) -> Result<Self, ObservationUnknownWireContractError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct SourceWire {
            instance_key: CanonicalSourceInstanceKey,
            stream_key: CoverageStreamKey,
            object_key: CoverageObjectKey,
            generation: u64,
        }

        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct ProvenanceWire {
            observation_selection: JsonValue,
            observer_sequence: u64,
            scope_epoch: u64,
            event_id: String,
            semantic_revision_ref: Option<SemanticRevisionRef>,
            source: SourceWire,
            observed_at: i64,
            phase: String,
            additional_envelope_provenance: JsonValue,
        }

        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            unknown_wire_event_contract_version: u32,
            family: String,
            type_tag: String,
            encoded_value: JsonValue,
            envelope_provenance: ProvenanceWire,
        }

        selection.validate()?;
        let wire = serde_json::from_value::<Wire>(value)
            .map_err(ObservationUnknownWireContractError::invalid)?;
        if wire.unknown_wire_event_contract_version
            != OBSERVATION_UNKNOWN_WIRE_EVENT_CONTRACT_VERSION
            || wire.family != UNKNOWN_WIRE_FAMILY
        {
            return Err(ObservationUnknownWireContractError::invalid(
                "unknown-wire event does not match the exact v1 family",
            ));
        }
        validate_type_tag(&wire.type_tag)?;
        let observation_selection = ObservationContractSelection::from_wire_value_for_expected(
            wire.envelope_provenance.observation_selection,
            &selection.observation_selection,
        )
        .map_err(ObservationUnknownWireContractError::invalid)?;
        validate_positive_portable(
            "unknown-wire observer sequence",
            wire.envelope_provenance.observer_sequence,
        )?;
        validate_positive_portable(
            "unknown-wire scope epoch",
            wire.envelope_provenance.scope_epoch,
        )?;
        validate_opaque_digest("unknown-wire event ID", &wire.envelope_provenance.event_id)?;
        validate_positive_portable(
            "unknown-wire source generation",
            wire.envelope_provenance.source.generation,
        )?;
        validate_safe_i64(
            "unknown-wire observed_at",
            wire.envelope_provenance.observed_at,
        )?;
        if !matches!(
            wire.envelope_provenance.phase.as_str(),
            "bootstrap" | "live" | "correction"
        ) {
            return Err(ObservationUnknownWireContractError::invalid(
                "unknown-wire phase is not canonical",
            ));
        }

        let mut budget = UnknownWireBudget::new(selection.capability.max_preserved_bytes as usize);
        let encoded_value = clone_bounded_json(&wire.encoded_value, 1, &mut budget)?;
        let additional_envelope_provenance = wire
            .envelope_provenance
            .additional_envelope_provenance
            .as_object()
            .ok_or_else(|| {
                ObservationUnknownWireContractError::invalid(
                    "unknown-wire additional envelope provenance must be an object",
                )
            })
            .and_then(|values| clone_bounded_fields(values, &mut budget))?;
        validate_exact_encoded_bound(
            &encoded_value,
            &additional_envelope_provenance,
            selection.capability.max_preserved_bytes as usize,
        )?;
        let value = Self {
            type_tag: wire.type_tag,
            encoded_value,
            provenance: ObservationUnknownWireProvenance {
                observation_selection,
                observer_sequence: wire.envelope_provenance.observer_sequence,
                scope_epoch: wire.envelope_provenance.scope_epoch,
                event_id: wire.envelope_provenance.event_id,
                semantic_revision_ref: wire.envelope_provenance.semantic_revision_ref,
                source_instance_key: wire.envelope_provenance.source.instance_key,
                stream_key: wire.envelope_provenance.source.stream_key,
                object_key: wire.envelope_provenance.source.object_key,
                generation: wire.envelope_provenance.source.generation,
                observed_at: wire.envelope_provenance.observed_at,
                phase: wire.envelope_provenance.phase,
                additional_envelope_provenance,
            },
        };
        Ok(value)
    }

    pub(crate) fn type_tag(&self) -> &str {
        &self.type_tag
    }

    pub(crate) fn provenance(&self) -> &ObservationUnknownWireProvenance {
        &self.provenance
    }

    pub(crate) fn wire_value(&self) -> JsonValue {
        serde_json::json!({
            "unknown_wire_event_contract_version": OBSERVATION_UNKNOWN_WIRE_EVENT_CONTRACT_VERSION,
            "family": UNKNOWN_WIRE_FAMILY,
            "type_tag": self.type_tag,
            "encoded_value": self.encoded_value,
            "envelope_provenance": {
                "observation_selection": self.provenance.observation_selection,
                "observer_sequence": self.provenance.observer_sequence,
                "scope_epoch": self.provenance.scope_epoch,
                "event_id": self.provenance.event_id,
                "semantic_revision_ref": self.provenance.semantic_revision_ref,
                "source": {
                    "instance_key": self.provenance.source_instance_key,
                    "stream_key": self.provenance.stream_key,
                    "object_key": self.provenance.object_key,
                    "generation": self.provenance.generation,
                },
                "observed_at": self.provenance.observed_at,
                "phase": self.provenance.phase,
                "additional_envelope_provenance": self.provenance.additional_envelope_provenance,
            }
        })
    }
}

pub(crate) fn is_known_event_type_tag(value: &str) -> bool {
    KNOWN_EVENT_TYPE_TAGS.contains(&value)
}

fn validate_type_tag(value: &str) -> Result<(), ObservationUnknownWireContractError> {
    if value.is_empty()
        || value.len() > MAX_UNKNOWN_WIRE_TYPE_TAG_BYTES
        || !value.is_ascii()
        || !value.bytes().enumerate().all(|(index, byte)| match byte {
            b'a'..=b'z' => true,
            b'0'..=b'9' | b'.' | b'_' | b'-' => index > 0,
            _ => false,
        })
        || is_known_event_type_tag(value)
    {
        return Err(ObservationUnknownWireContractError::invalid(
            "unknown-wire type tag is noncanonical or shadows a known event",
        ));
    }
    Ok(())
}

fn validate_positive_portable(
    label: &str,
    value: u64,
) -> Result<(), ObservationUnknownWireContractError> {
    if value == 0 || value > MAX_JAVASCRIPT_SAFE_INTEGER {
        return Err(ObservationUnknownWireContractError::invalid(format!(
            "{label} must be a positive JavaScript-safe integer"
        )));
    }
    Ok(())
}

fn validate_safe_i64(label: &str, value: i64) -> Result<(), ObservationUnknownWireContractError> {
    if value.unsigned_abs() > MAX_JAVASCRIPT_SAFE_INTEGER {
        return Err(ObservationUnknownWireContractError::invalid(format!(
            "{label} must be a JavaScript-safe integer"
        )));
    }
    Ok(())
}

fn validate_opaque_digest(
    label: &str,
    value: &str,
) -> Result<(), ObservationUnknownWireContractError> {
    let encoded = value.strip_prefix("v1:").ok_or_else(|| {
        ObservationUnknownWireContractError::invalid(format!(
            "{label} must use canonical v1 encoding"
        ))
    })?;
    let decoded = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| {
        ObservationUnknownWireContractError::invalid(format!(
            "{label} must use canonical v1 encoding"
        ))
    })?;
    if decoded.len() != DIGEST_BYTES
        || decoded.iter().all(|byte| *byte == 0)
        || URL_SAFE_NO_PAD.encode(&decoded) != encoded
    {
        return Err(ObservationUnknownWireContractError::invalid(format!(
            "{label} must be a nonzero canonical 32-byte opaque value"
        )));
    }
    Ok(())
}

#[derive(Debug)]
struct UnknownWireBudget {
    bytes: usize,
    nodes: usize,
    max_bytes: usize,
}

impl UnknownWireBudget {
    fn new(max_bytes: usize) -> Self {
        Self {
            bytes: 0,
            nodes: 0,
            max_bytes,
        }
    }

    fn add_bytes(&mut self, bytes: usize) -> Result<(), ObservationUnknownWireContractError> {
        self.bytes = self.bytes.checked_add(bytes).ok_or_else(|| {
            ObservationUnknownWireContractError::invalid("unknown-wire size overflow")
        })?;
        if self.bytes > self.max_bytes {
            return Err(ObservationUnknownWireContractError::invalid(format!(
                "unknown-wire preserved value exceeds the negotiated {} byte bound",
                self.max_bytes
            )));
        }
        Ok(())
    }

    fn add_node(&mut self) -> Result<(), ObservationUnknownWireContractError> {
        self.nodes = self.nodes.checked_add(1).ok_or_else(|| {
            ObservationUnknownWireContractError::invalid("unknown-wire node overflow")
        })?;
        if self.nodes > MAX_UNKNOWN_WIRE_NODES {
            return Err(ObservationUnknownWireContractError::invalid(format!(
                "unknown-wire preserved value exceeds {MAX_UNKNOWN_WIRE_NODES} nodes"
            )));
        }
        self.add_bytes(1)
    }
}

fn clone_bounded_fields(
    values: &JsonMap<String, JsonValue>,
    budget: &mut UnknownWireBudget,
) -> Result<BTreeMap<String, JsonValue>, ObservationUnknownWireContractError> {
    budget.add_node()?;
    let mut result = BTreeMap::new();
    for (key, value) in values {
        validate_object_key(key)?;
        budget.add_bytes(key.len())?;
        result.insert(key.clone(), clone_bounded_json(value, 1, budget)?);
    }
    Ok(result)
}

fn clone_bounded_json(
    value: &JsonValue,
    depth: usize,
    budget: &mut UnknownWireBudget,
) -> Result<JsonValue, ObservationUnknownWireContractError> {
    if depth > MAX_UNKNOWN_WIRE_DEPTH {
        return Err(ObservationUnknownWireContractError::invalid(format!(
            "unknown-wire preserved value exceeds depth {MAX_UNKNOWN_WIRE_DEPTH}"
        )));
    }
    budget.add_node()?;
    match value {
        JsonValue::Null => Ok(JsonValue::Null),
        JsonValue::Bool(value) => {
            budget.add_bytes(1)?;
            Ok(JsonValue::Bool(*value))
        }
        JsonValue::Number(number) => {
            let portable = number
                .as_i64()
                .is_some_and(|value| value.unsigned_abs() <= MAX_JAVASCRIPT_SAFE_INTEGER)
                || number
                    .as_u64()
                    .is_some_and(|value| value <= MAX_JAVASCRIPT_SAFE_INTEGER);
            if !portable {
                return Err(ObservationUnknownWireContractError::invalid(
                    "unknown-wire numbers must be JavaScript-safe integers",
                ));
            }
            budget.add_bytes(8)?;
            Ok(JsonValue::Number(number.clone()))
        }
        JsonValue::String(value) => {
            budget.add_bytes(value.len())?;
            Ok(JsonValue::String(value.clone()))
        }
        JsonValue::Array(values) => {
            let remaining_nodes = MAX_UNKNOWN_WIRE_NODES.saturating_sub(budget.nodes);
            if values.len() > remaining_nodes {
                return Err(ObservationUnknownWireContractError::invalid(format!(
                    "unknown-wire preserved value exceeds {MAX_UNKNOWN_WIRE_NODES} nodes"
                )));
            }
            let mut result = Vec::new();
            result.try_reserve_exact(values.len()).map_err(|_| {
                ObservationUnknownWireContractError::invalid(
                    "unknown-wire preserved array cannot be retained within its bound",
                )
            })?;
            for value in values {
                result.push(clone_bounded_json(value, depth + 1, budget)?);
            }
            Ok(JsonValue::Array(result))
        }
        JsonValue::Object(values) => {
            let mut result = JsonMap::new();
            for (key, value) in values {
                validate_object_key(key)?;
                budget.add_bytes(key.len())?;
                result.insert(key.clone(), clone_bounded_json(value, depth + 1, budget)?);
            }
            Ok(JsonValue::Object(result))
        }
    }
}

fn validate_object_key(value: &str) -> Result<(), ObservationUnknownWireContractError> {
    if value.is_empty()
        || value.len() > MAX_UNKNOWN_WIRE_OBJECT_KEY_BYTES
        || value.trim() != value
        || value.chars().any(char::is_control)
        || matches!(value, "__proto__" | "prototype" | "constructor")
    {
        return Err(ObservationUnknownWireContractError::invalid(
            "unknown-wire object key is not canonical",
        ));
    }
    Ok(())
}

struct BoundedJsonCounter {
    bytes: usize,
    max_bytes: usize,
}

impl Write for BoundedJsonCounter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let next = self
            .bytes
            .checked_add(buffer.len())
            .ok_or_else(|| io::Error::other("unknown-wire encoded size overflow"))?;
        if next > self.max_bytes {
            return Err(io::Error::other(
                "unknown-wire encoded value exceeds its negotiated bound",
            ));
        }
        self.bytes = next;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn validate_exact_encoded_bound(
    encoded_value: &JsonValue,
    additional_envelope_provenance: &BTreeMap<String, JsonValue>,
    max_bytes: usize,
) -> Result<(), ObservationUnknownWireContractError> {
    let mut counter = BoundedJsonCounter {
        bytes: 0,
        max_bytes,
    };
    serde_json::to_writer(&mut counter, encoded_value).map_err(|_| {
        ObservationUnknownWireContractError::invalid(format!(
            "unknown-wire encoded value exceeds the negotiated {max_bytes} byte bound"
        ))
    })?;
    serde_json::to_writer(&mut counter, additional_envelope_provenance).map_err(|_| {
        ObservationUnknownWireContractError::invalid(format!(
            "unknown-wire encoded value exceeds the negotiated {max_bytes} byte bound"
        ))
    })?;
    Ok(())
}

#[cfg(test)]
mod tests;
