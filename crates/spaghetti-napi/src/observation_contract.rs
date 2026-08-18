//! Portable RFC 012D observation contract negotiation.
//!
//! This crate-private boundary composes RFC 012A version selection without
//! opening a source, creating an observer, or freezing the still-incomplete
//! observation event union. Version 1 selects exact profile, envelope, event,
//! and lifecycle contracts; additive unknown-event preservation remains gated
//! on the later complete envelope contract.

use std::collections::BTreeSet;
use std::fmt;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};

use crate::adapter::{
    select_contract_versions, ContractVersionOffer, ContractVersionRequest,
    ContractVersionSelection, CONTRACT_VERSION_SELECTION_VERSION,
    EXTERNAL_ENTITY_REFERENCE_VERSION, SEMANTIC_REFERENCE_CONTRACT_VERSION,
    SOURCE_COVERAGE_CONTRACT_VERSION,
};

mod capabilities;
pub(crate) use capabilities::{ObservationCapabilities, ObservationCapabilityContractError};

pub(crate) const OBSERVATION_NEGOTIATION_CONTRACT_VERSION: u32 = 1;
pub(crate) const OBSERVATION_PROFILE_CONTRACT_VERSION: u32 = 1;
pub(crate) const OBSERVATION_ENVELOPE_CONTRACT_VERSION: u32 = 1;
pub(crate) const OBSERVATION_EVENT_CONTRACT_VERSION: u32 = 1;
pub(crate) const OBSERVATION_LIFECYCLE_CONTRACT_VERSION: u32 = 1;
pub(crate) const OBSERVATION_BASE_MODEL_MAJOR: u32 = 1;

const MAX_VERSION_PREFERENCES: usize = 16;
const MAX_FACT_FAMILIES: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ObservationCompatibilityAxis {
    BaseModelMajor,
    ExternalEntityReferenceVersion,
    SemanticRevisionReferenceVersion,
    CoverageContractVersion,
    FactFamilyVersion,
    ObservationProfileVersion,
    EnvelopeContractVersion,
    EventContractVersion,
    LifecycleContractVersion,
}

impl fmt::Display for ObservationCompatibilityAxis {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::BaseModelMajor => "base_model_major",
            Self::ExternalEntityReferenceVersion => "external_entity_reference_version",
            Self::SemanticRevisionReferenceVersion => "semantic_revision_reference_version",
            Self::CoverageContractVersion => "coverage_contract_version",
            Self::FactFamilyVersion => "fact_family_version",
            Self::ObservationProfileVersion => "observation_profile_version",
            Self::EnvelopeContractVersion => "envelope_contract_version",
            Self::EventContractVersion => "event_contract_version",
            Self::LifecycleContractVersion => "lifecycle_contract_version",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ObservationNegotiationError {
    #[error("invalid observation contract: {message}")]
    InvalidObservationContract { message: String },
    #[error("IncompatibleObservationContract: {axis}")]
    IncompatibleObservationContract { axis: ObservationCompatibilityAxis },
}

impl ObservationNegotiationError {
    fn invalid(error: impl fmt::Display) -> Self {
        Self::InvalidObservationContract {
            message: error.to_string(),
        }
    }

    fn incompatible(axis: ObservationCompatibilityAxis) -> Self {
        Self::IncompatibleObservationContract { axis }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ObservationContractRequest {
    pub observation_negotiation_contract_version: u32,
    pub contract_versions: ContractVersionRequest,
    pub envelope_contract_versions: Vec<u32>,
    pub event_contract_versions: Vec<u32>,
    pub lifecycle_contract_versions: Vec<u32>,
}

impl ObservationContractRequest {
    pub(crate) fn new(
        contract_versions: ContractVersionRequest,
        envelope_contract_versions: Vec<u32>,
        event_contract_versions: Vec<u32>,
        lifecycle_contract_versions: Vec<u32>,
    ) -> Result<Self, ObservationNegotiationError> {
        let value = Self {
            observation_negotiation_contract_version: OBSERVATION_NEGOTIATION_CONTRACT_VERSION,
            contract_versions,
            envelope_contract_versions,
            event_contract_versions,
            lifecycle_contract_versions,
        };
        value.validate()?;
        Ok(value)
    }

    fn validate(&self) -> Result<(), ObservationNegotiationError> {
        if self.observation_negotiation_contract_version != OBSERVATION_NEGOTIATION_CONTRACT_VERSION
            || self.contract_versions.selection_contract_version
                != CONTRACT_VERSION_SELECTION_VERSION
        {
            return Err(ObservationNegotiationError::invalid(
                "unsupported observation negotiation request version",
            ));
        }
        self.contract_versions
            .validate()
            .map_err(ObservationNegotiationError::invalid)?;
        validate_request_base(&self.contract_versions)?;
        validate_version_preferences(
            "requested observation envelope versions",
            &self.envelope_contract_versions,
            true,
        )?;
        validate_version_preferences(
            "requested observation event versions",
            &self.event_contract_versions,
            true,
        )?;
        validate_version_preferences(
            "requested observation lifecycle versions",
            &self.lifecycle_contract_versions,
            true,
        )
    }
}

impl<'de> Deserialize<'de> for ObservationContractRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            observation_negotiation_contract_version: u32,
            contract_versions: ContractVersionRequest,
            envelope_contract_versions: Vec<u32>,
            event_contract_versions: Vec<u32>,
            lifecycle_contract_versions: Vec<u32>,
        }

        let wire = Wire::deserialize(deserializer)?;
        let value = Self {
            observation_negotiation_contract_version: wire.observation_negotiation_contract_version,
            contract_versions: wire.contract_versions,
            envelope_contract_versions: wire.envelope_contract_versions,
            event_contract_versions: wire.event_contract_versions,
            lifecycle_contract_versions: wire.lifecycle_contract_versions,
        };
        value.validate().map_err(D::Error::custom)?;
        Ok(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ObservationContractOffer {
    pub observation_negotiation_contract_version: u32,
    pub contract_versions: ContractVersionOffer,
    pub envelope_contract_versions: Vec<u32>,
    pub event_contract_versions: Vec<u32>,
    pub lifecycle_contract_versions: Vec<u32>,
}

impl ObservationContractOffer {
    pub(crate) fn new(
        contract_versions: ContractVersionOffer,
        envelope_contract_versions: Vec<u32>,
        event_contract_versions: Vec<u32>,
        lifecycle_contract_versions: Vec<u32>,
    ) -> Result<Self, ObservationNegotiationError> {
        let value = Self {
            observation_negotiation_contract_version: OBSERVATION_NEGOTIATION_CONTRACT_VERSION,
            contract_versions,
            envelope_contract_versions,
            event_contract_versions,
            lifecycle_contract_versions,
        };
        value.validate()?;
        Ok(value)
    }

    fn validate(&self) -> Result<(), ObservationNegotiationError> {
        if self.observation_negotiation_contract_version != OBSERVATION_NEGOTIATION_CONTRACT_VERSION
        {
            return Err(ObservationNegotiationError::invalid(
                "unsupported observation negotiation offer version",
            ));
        }
        self.contract_versions
            .validate()
            .map_err(ObservationNegotiationError::invalid)?;
        validate_offer_base(&self.contract_versions)?;
        validate_version_preferences(
            "offered observation envelope versions",
            &self.envelope_contract_versions,
            true,
        )?;
        validate_version_preferences(
            "offered observation event versions",
            &self.event_contract_versions,
            true,
        )?;
        validate_version_preferences(
            "offered observation lifecycle versions",
            &self.lifecycle_contract_versions,
            true,
        )
    }
}

impl<'de> Deserialize<'de> for ObservationContractOffer {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            observation_negotiation_contract_version: u32,
            contract_versions: ContractVersionOffer,
            envelope_contract_versions: Vec<u32>,
            event_contract_versions: Vec<u32>,
            lifecycle_contract_versions: Vec<u32>,
        }

        let wire = Wire::deserialize(deserializer)?;
        let value = Self {
            observation_negotiation_contract_version: wire.observation_negotiation_contract_version,
            contract_versions: wire.contract_versions,
            envelope_contract_versions: wire.envelope_contract_versions,
            event_contract_versions: wire.event_contract_versions,
            lifecycle_contract_versions: wire.lifecycle_contract_versions,
        };
        value.validate().map_err(D::Error::custom)?;
        Ok(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ObservationContractSelection {
    pub observation_negotiation_contract_version: u32,
    pub contract_versions: ContractVersionSelection,
    pub envelope_contract_version: u32,
    pub event_contract_version: u32,
    pub lifecycle_contract_version: u32,
}

impl ObservationContractSelection {
    fn validate(&self) -> Result<(), ObservationNegotiationError> {
        if self.observation_negotiation_contract_version != OBSERVATION_NEGOTIATION_CONTRACT_VERSION
            || self.contract_versions.selection_contract_version
                != CONTRACT_VERSION_SELECTION_VERSION
            || self.contract_versions.model_major != OBSERVATION_BASE_MODEL_MAJOR
            || self.contract_versions.external_entity_reference_version
                != EXTERNAL_ENTITY_REFERENCE_VERSION
            || self.contract_versions.semantic_revision_reference_version
                != SEMANTIC_REFERENCE_CONTRACT_VERSION
            || self.contract_versions.coverage_contract_version != SOURCE_COVERAGE_CONTRACT_VERSION
            || self.contract_versions.query_pack_version.is_some()
            || self.contract_versions.observation_contract_version
                != Some(OBSERVATION_PROFILE_CONTRACT_VERSION)
            || self.envelope_contract_version != OBSERVATION_ENVELOPE_CONTRACT_VERSION
            || self.event_contract_version != OBSERVATION_EVENT_CONTRACT_VERSION
            || self.lifecycle_contract_version != OBSERVATION_LIFECYCLE_CONTRACT_VERSION
        {
            return Err(ObservationNegotiationError::invalid(
                "observation selection does not match the exact v1 contract profile",
            ));
        }
        validate_selected_families(&self.contract_versions)
    }

    /// Consumes a selection only when it is exactly the result of the caller's
    /// request and the host's offer. A shape-valid selection is not sufficient:
    /// its fact-family set and preferred versions are negotiation results too.
    pub(crate) fn from_wire_value_for_negotiation(
        value: serde_json::Value,
        request: &ObservationContractRequest,
        offer: &ObservationContractOffer,
    ) -> Result<Self, ObservationNegotiationError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            observation_negotiation_contract_version: u32,
            contract_versions: ContractVersionSelection,
            envelope_contract_version: u32,
            event_contract_version: u32,
            lifecycle_contract_version: u32,
        }

        let wire: Wire =
            serde_json::from_value(value).map_err(ObservationNegotiationError::invalid)?;
        let value = Self {
            observation_negotiation_contract_version: wire.observation_negotiation_contract_version,
            contract_versions: wire.contract_versions,
            envelope_contract_version: wire.envelope_contract_version,
            event_contract_version: wire.event_contract_version,
            lifecycle_contract_version: wire.lifecycle_contract_version,
        };
        value.validate()?;
        if value != negotiate_observation_contract(request, offer)? {
            return Err(ObservationNegotiationError::invalid(
                "observation selection does not match the exact negotiated result",
            ));
        }
        Ok(value)
    }

    pub(crate) fn from_wire_value_for_expected(
        value: serde_json::Value,
        expected: &Self,
    ) -> Result<Self, ObservationNegotiationError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            observation_negotiation_contract_version: u32,
            contract_versions: ContractVersionSelection,
            envelope_contract_version: u32,
            event_contract_version: u32,
            lifecycle_contract_version: u32,
        }

        let wire: Wire =
            serde_json::from_value(value).map_err(ObservationNegotiationError::invalid)?;
        let value = Self {
            observation_negotiation_contract_version: wire.observation_negotiation_contract_version,
            contract_versions: wire.contract_versions,
            envelope_contract_version: wire.envelope_contract_version,
            event_contract_version: wire.event_contract_version,
            lifecycle_contract_version: wire.lifecycle_contract_version,
        };
        value.validate()?;
        if &value != expected {
            return Err(ObservationNegotiationError::invalid(
                "observation selection does not match the caller-held selection",
            ));
        }
        Ok(value)
    }
}

fn validate_request_base(
    request: &ContractVersionRequest,
) -> Result<(), ObservationNegotiationError> {
    if request.query_pack_versions.is_some() {
        return Err(ObservationNegotiationError::invalid(
            "observation negotiation cannot request query-pack authority",
        ));
    }
    validate_version_preferences(
        "requested coverage contract versions",
        &request.coverage_contract_versions,
        true,
    )?;
    let observation_versions = request
        .observation_contract_versions
        .as_deref()
        .ok_or_else(|| {
            ObservationNegotiationError::invalid(
                "observation negotiation requires observation-profile versions",
            )
        })?;
    validate_version_preferences(
        "requested observation profile versions",
        observation_versions,
        true,
    )?;
    validate_fact_family_preferences(&request.fact_family_versions)
}

fn validate_offer_base(offer: &ContractVersionOffer) -> Result<(), ObservationNegotiationError> {
    if !offer.query_pack_versions.is_empty() {
        return Err(ObservationNegotiationError::invalid(
            "observation negotiation offer cannot grant query-pack authority",
        ));
    }
    validate_version_preferences(
        "offered external entity reference versions",
        &offer.external_entity_reference_versions,
        true,
    )?;
    validate_version_preferences(
        "offered semantic revision reference versions",
        &offer.semantic_revision_reference_versions,
        true,
    )?;
    validate_version_preferences(
        "offered coverage contract versions",
        &offer.coverage_contract_versions,
        true,
    )?;
    validate_version_preferences(
        "offered query pack versions",
        &offer.query_pack_versions,
        false,
    )?;
    validate_version_preferences(
        "offered observation profile versions",
        &offer.observation_contract_versions,
        true,
    )?;
    validate_fact_family_preferences(&offer.fact_family_versions)
}

fn validate_fact_family_preferences(
    families: &std::collections::BTreeMap<String, Vec<u32>>,
) -> Result<(), ObservationNegotiationError> {
    if families.is_empty() || families.len() > MAX_FACT_FAMILIES {
        return Err(ObservationNegotiationError::invalid(format!(
            "observation negotiation requires 1..={MAX_FACT_FAMILIES} fact families"
        )));
    }
    for (family, versions) in families {
        validate_family_identifier(family)?;
        validate_version_preferences(
            &format!("observation fact-family versions for {family}"),
            versions,
            true,
        )?;
    }
    Ok(())
}

fn validate_selected_families(
    selection: &ContractVersionSelection,
) -> Result<(), ObservationNegotiationError> {
    if selection.fact_family_versions.is_empty()
        || selection.fact_family_versions.len() > MAX_FACT_FAMILIES
        || selection
            .fact_family_versions
            .values()
            .any(|version| *version == 0)
    {
        return Err(ObservationNegotiationError::invalid(
            "observation selection has an invalid fact-family set",
        ));
    }
    for family in selection.fact_family_versions.keys() {
        validate_family_identifier(family)?;
    }
    Ok(())
}

fn validate_family_identifier(value: &str) -> Result<(), ObservationNegotiationError> {
    let mut bytes = value.bytes();
    let Some(first) = bytes.next() else {
        return Err(ObservationNegotiationError::invalid(
            "observation fact-family identifier must not be empty",
        ));
    };
    if value.len() > 128
        || !first.is_ascii_lowercase() && !first.is_ascii_digit()
        || bytes.any(|byte| {
            !byte.is_ascii_lowercase()
                && !byte.is_ascii_digit()
                && !matches!(byte, b'.' | b'_' | b'-')
        })
    {
        return Err(ObservationNegotiationError::invalid(
            "observation fact-family identifier is not canonical",
        ));
    }
    Ok(())
}

fn validate_version_preferences(
    label: &str,
    versions: &[u32],
    require_nonempty: bool,
) -> Result<(), ObservationNegotiationError> {
    if require_nonempty && versions.is_empty() {
        return Err(ObservationNegotiationError::invalid(format!(
            "{label} must not be empty"
        )));
    }
    if versions.len() > MAX_VERSION_PREFERENCES {
        return Err(ObservationNegotiationError::invalid(format!(
            "{label} exceeds {MAX_VERSION_PREFERENCES} preferences"
        )));
    }
    let mut seen = BTreeSet::new();
    for version in versions {
        if *version == 0 || !seen.insert(*version) {
            return Err(ObservationNegotiationError::invalid(format!(
                "{label} contains a zero or duplicate version"
            )));
        }
    }
    Ok(())
}

fn first_common(requested: &[u32], offered: &[u32]) -> Option<u32> {
    requested
        .iter()
        .copied()
        .find(|version| offered.contains(version))
}

pub(crate) fn negotiate_observation_contract(
    request: &ObservationContractRequest,
    offer: &ObservationContractOffer,
) -> Result<ObservationContractSelection, ObservationNegotiationError> {
    request.validate()?;
    offer.validate()?;
    let requested = &request.contract_versions;
    let offered = &offer.contract_versions;

    if requested.model_major != OBSERVATION_BASE_MODEL_MAJOR
        || offered.model_major != OBSERVATION_BASE_MODEL_MAJOR
        || requested.model_major != offered.model_major
    {
        return Err(ObservationNegotiationError::incompatible(
            ObservationCompatibilityAxis::BaseModelMajor,
        ));
    }
    if requested.external_entity_reference_version != EXTERNAL_ENTITY_REFERENCE_VERSION
        || !offered
            .external_entity_reference_versions
            .contains(&requested.external_entity_reference_version)
    {
        return Err(ObservationNegotiationError::incompatible(
            ObservationCompatibilityAxis::ExternalEntityReferenceVersion,
        ));
    }
    if requested.semantic_revision_reference_version != SEMANTIC_REFERENCE_CONTRACT_VERSION
        || !offered
            .semantic_revision_reference_versions
            .contains(&requested.semantic_revision_reference_version)
    {
        return Err(ObservationNegotiationError::incompatible(
            ObservationCompatibilityAxis::SemanticRevisionReferenceVersion,
        ));
    }
    if first_common(
        &requested.coverage_contract_versions,
        &offered.coverage_contract_versions,
    ) != Some(SOURCE_COVERAGE_CONTRACT_VERSION)
    {
        return Err(ObservationNegotiationError::incompatible(
            ObservationCompatibilityAxis::CoverageContractVersion,
        ));
    }
    for (family, requested_versions) in &requested.fact_family_versions {
        let Some(offered_versions) = offered.fact_family_versions.get(family) else {
            return Err(ObservationNegotiationError::incompatible(
                ObservationCompatibilityAxis::FactFamilyVersion,
            ));
        };
        if first_common(requested_versions, offered_versions).is_none() {
            return Err(ObservationNegotiationError::incompatible(
                ObservationCompatibilityAxis::FactFamilyVersion,
            ));
        }
    }
    let requested_profiles = requested
        .observation_contract_versions
        .as_deref()
        .expect("request validation requires observation profiles");
    if first_common(requested_profiles, &offered.observation_contract_versions)
        != Some(OBSERVATION_PROFILE_CONTRACT_VERSION)
    {
        return Err(ObservationNegotiationError::incompatible(
            ObservationCompatibilityAxis::ObservationProfileVersion,
        ));
    }
    if first_common(
        &request.envelope_contract_versions,
        &offer.envelope_contract_versions,
    ) != Some(OBSERVATION_ENVELOPE_CONTRACT_VERSION)
    {
        return Err(ObservationNegotiationError::incompatible(
            ObservationCompatibilityAxis::EnvelopeContractVersion,
        ));
    }
    if first_common(
        &request.event_contract_versions,
        &offer.event_contract_versions,
    ) != Some(OBSERVATION_EVENT_CONTRACT_VERSION)
    {
        return Err(ObservationNegotiationError::incompatible(
            ObservationCompatibilityAxis::EventContractVersion,
        ));
    }
    if first_common(
        &request.lifecycle_contract_versions,
        &offer.lifecycle_contract_versions,
    ) != Some(OBSERVATION_LIFECYCLE_CONTRACT_VERSION)
    {
        return Err(ObservationNegotiationError::incompatible(
            ObservationCompatibilityAxis::LifecycleContractVersion,
        ));
    }

    let contract_versions = select_contract_versions(requested, offered)
        .map_err(ObservationNegotiationError::invalid)?;
    let selection = ObservationContractSelection {
        observation_negotiation_contract_version: OBSERVATION_NEGOTIATION_CONTRACT_VERSION,
        contract_versions,
        envelope_contract_version: OBSERVATION_ENVELOPE_CONTRACT_VERSION,
        event_contract_version: OBSERVATION_EVENT_CONTRACT_VERSION,
        lifecycle_contract_version: OBSERVATION_LIFECYCLE_CONTRACT_VERSION,
    };
    selection.validate()?;
    Ok(selection)
}

#[cfg(test)]
mod tests;
