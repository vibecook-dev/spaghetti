//! RFC 012D per-family observation capabilities.
//!
//! This contract reports what one already-negotiated attachment can deliver.
//! It carries no source locator, access authority, queue, observer runtime, or
//! N-API surface.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};

use super::{ObservationContractOffer, ObservationContractSelection};
use crate::adapter::{CompatibilityClass, ContractCompleteness};

pub(crate) const OBSERVATION_CAPABILITIES_CONTRACT_VERSION: u32 = 1;

const MAX_CAPABILITY_FAMILIES: usize = 64;
const MAX_FAMILY_IDENTIFIER_BYTES: usize = 128;
const MAX_SUPPORT_RELEASE_ID_BYTES: usize = 256;
const MAX_OFFERED_VERSIONS: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ObservationCapabilityContractError {
    #[error("invalid observation capabilities: {message}")]
    Invalid { message: String },
}

impl ObservationCapabilityContractError {
    fn invalid(message: impl fmt::Display) -> Self {
        Self::Invalid {
            message: message.to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ObservationCapabilityStatus {
    Supported,
    Degraded,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ObservationCapabilityQuality {
    Exact,
    Qualified,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ObservationCapabilityExpectedTiming {
    BootstrapAndLive,
    Never,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ObservationCapabilityLimitation {
    ScopeBound,
    CoverageReportedSeparately,
    RangeSupportedNativeVersion,
    NotNegotiated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ObservationCapabilitySupportEvidence {
    ExactPromotedRelease,
    RangeSupportedRelease,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum ObservationCapabilityEvidence {
    PromotedSupportRelease {
        support_release_id: String,
        support: ObservationCapabilitySupportEvidence,
    },
    HostOfferNotSelected {
        offered_versions: Vec<u32>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ObservationFactFamilyCapability {
    pub fact_family: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_version: Option<u32>,
    pub status: ObservationCapabilityStatus,
    pub evidence: ObservationCapabilityEvidence,
    pub quality: ObservationCapabilityQuality,
    pub expected_timing: ObservationCapabilityExpectedTiming,
    /// Capability-level expectation, not a claim about current source
    /// readiness. Actual completeness remains coverage/barrier-owned.
    pub expected_completeness: ContractCompleteness,
    pub limitations: Vec<ObservationCapabilityLimitation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ObservationCapabilities {
    pub observation_capabilities_contract_version: u32,
    pub selection: ObservationContractSelection,
    pub fact_families: Vec<ObservationFactFamilyCapability>,
}

impl ObservationCapabilities {
    pub(crate) fn from_negotiation(
        selection: ObservationContractSelection,
        offer: &ObservationContractOffer,
        compatibility: CompatibilityClass,
        support_release_id: Option<&str>,
        implemented_fact_families: &[(&str, u32)],
    ) -> Result<Self, ObservationCapabilityContractError> {
        let (status, support, quality, completeness, range_limited) = match compatibility {
            CompatibilityClass::ExactSupported => (
                ObservationCapabilityStatus::Supported,
                ObservationCapabilitySupportEvidence::ExactPromotedRelease,
                ObservationCapabilityQuality::Exact,
                ContractCompleteness::Complete,
                false,
            ),
            CompatibilityClass::RangeSupported => (
                ObservationCapabilityStatus::Degraded,
                ObservationCapabilitySupportEvidence::RangeSupportedRelease,
                ObservationCapabilityQuality::Qualified,
                ContractCompleteness::Partial,
                true,
            ),
            CompatibilityClass::RecognizedUnverified
            | CompatibilityClass::UnknownOrIncompatible => {
                return Err(ObservationCapabilityContractError::invalid(
                    "typed observation capabilities require an authorized support release",
                ));
            }
        };
        let support_release_id = support_release_id.ok_or_else(|| {
            ObservationCapabilityContractError::invalid(
                "authorized observation capabilities require a support release id",
            )
        })?;
        validate_identifier(
            "observation support release id",
            support_release_id,
            MAX_SUPPORT_RELEASE_ID_BYTES,
        )?;
        offer
            .validate()
            .map_err(ObservationCapabilityContractError::invalid)?;
        validate_selection_is_offered(&selection, offer)?;

        let implemented = validate_implemented_families(implemented_fact_families)?;
        let selected = &selection.contract_versions.fact_family_versions;
        for (family, version) in selected {
            if implemented.get(family.as_str()) != Some(version) {
                return Err(ObservationCapabilityContractError::invalid(format!(
                    "selected observation family {family:?} is not implemented by this observer"
                )));
            }
        }

        let mut fact_families =
            Vec::with_capacity(offer.contract_versions.fact_family_versions.len());
        for (family, offered_versions) in &offer.contract_versions.fact_family_versions {
            if let Some(selected_version) = selected.get(family) {
                let mut limitations = vec![
                    ObservationCapabilityLimitation::ScopeBound,
                    ObservationCapabilityLimitation::CoverageReportedSeparately,
                ];
                if range_limited {
                    limitations.push(ObservationCapabilityLimitation::RangeSupportedNativeVersion);
                }
                limitations.sort_unstable();
                fact_families.push(ObservationFactFamilyCapability {
                    fact_family: family.clone(),
                    selected_version: Some(*selected_version),
                    status,
                    evidence: ObservationCapabilityEvidence::PromotedSupportRelease {
                        support_release_id: support_release_id.to_string(),
                        support,
                    },
                    quality,
                    expected_timing: ObservationCapabilityExpectedTiming::BootstrapAndLive,
                    expected_completeness: completeness,
                    limitations,
                });
            } else {
                fact_families.push(ObservationFactFamilyCapability {
                    fact_family: family.clone(),
                    selected_version: None,
                    status: ObservationCapabilityStatus::Unsupported,
                    evidence: ObservationCapabilityEvidence::HostOfferNotSelected {
                        offered_versions: offered_versions.clone(),
                    },
                    quality: ObservationCapabilityQuality::Unavailable,
                    expected_timing: ObservationCapabilityExpectedTiming::Never,
                    expected_completeness: ContractCompleteness::Unknown,
                    limitations: vec![ObservationCapabilityLimitation::NotNegotiated],
                });
            }
        }

        let value = Self {
            observation_capabilities_contract_version: OBSERVATION_CAPABILITIES_CONTRACT_VERSION,
            selection,
            fact_families,
        };
        value.validate()?;
        Ok(value)
    }

    pub(crate) fn from_wire_value_for_context(
        value: serde_json::Value,
        expected_selection: &ObservationContractSelection,
        expected_offer: &ObservationContractOffer,
        expected_compatibility: CompatibilityClass,
        expected_support_release_id: &str,
    ) -> Result<Self, ObservationCapabilityContractError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            observation_capabilities_contract_version: u32,
            selection: serde_json::Value,
            fact_families: Vec<ObservationFactFamilyCapability>,
        }

        let wire: Wire =
            serde_json::from_value(value).map_err(ObservationCapabilityContractError::invalid)?;
        let selection = ObservationContractSelection::from_wire_value_for_expected(
            wire.selection,
            expected_selection,
        )
        .map_err(ObservationCapabilityContractError::invalid)?;
        let value = Self {
            observation_capabilities_contract_version: wire
                .observation_capabilities_contract_version,
            selection,
            fact_families: wire.fact_families,
        };
        value.validate()?;
        let implemented: Vec<_> = expected_selection
            .contract_versions
            .fact_family_versions
            .iter()
            .map(|(family, version)| (family.as_str(), *version))
            .collect();
        let expected = Self::from_negotiation(
            expected_selection.clone(),
            expected_offer,
            expected_compatibility,
            Some(expected_support_release_id),
            &implemented,
        )?;
        if value != expected {
            return Err(ObservationCapabilityContractError::invalid(
                "observation capabilities do not match the caller-held negotiation and support context",
            ));
        }
        Ok(value)
    }

    fn validate(&self) -> Result<(), ObservationCapabilityContractError> {
        self.selection
            .validate()
            .map_err(ObservationCapabilityContractError::invalid)?;
        if self.observation_capabilities_contract_version
            != OBSERVATION_CAPABILITIES_CONTRACT_VERSION
        {
            return Err(ObservationCapabilityContractError::invalid(
                "unsupported observation capabilities contract version",
            ));
        }
        if self.fact_families.is_empty() || self.fact_families.len() > MAX_CAPABILITY_FAMILIES {
            return Err(ObservationCapabilityContractError::invalid(format!(
                "observation capabilities require 1..={MAX_CAPABILITY_FAMILIES} family reports"
            )));
        }

        let selected = &self.selection.contract_versions.fact_family_versions;
        let mut observed_selected = BTreeSet::new();
        let mut attachment_support: Option<(&str, ObservationCapabilitySupportEvidence)> = None;
        let mut previous_family: Option<&str> = None;
        for capability in &self.fact_families {
            validate_family_identifier(&capability.fact_family)?;
            if previous_family.is_some_and(|previous| previous >= capability.fact_family.as_str()) {
                return Err(ObservationCapabilityContractError::invalid(
                    "observation capability families must be strictly sorted and unique",
                ));
            }
            previous_family = Some(&capability.fact_family);
            validate_limitations(&capability.limitations)?;

            match capability.selected_version {
                Some(version) => {
                    if version == 0
                        || selected.get(&capability.fact_family) != Some(&version)
                        || !observed_selected.insert(capability.fact_family.as_str())
                    {
                        return Err(ObservationCapabilityContractError::invalid(
                            "selected capability does not match the negotiated family version",
                        ));
                    }
                    validate_selected_capability(capability)?;
                    let ObservationCapabilityEvidence::PromotedSupportRelease {
                        support_release_id,
                        support,
                    } = &capability.evidence
                    else {
                        unreachable!("selected capability validation requires support evidence");
                    };
                    if attachment_support
                        .is_some_and(|expected| expected != (support_release_id.as_str(), *support))
                    {
                        return Err(ObservationCapabilityContractError::invalid(
                            "selected capabilities must share one support-release evidence source",
                        ));
                    }
                    attachment_support = Some((support_release_id, *support));
                }
                None => validate_unselected_capability(capability)?,
            }
        }
        if observed_selected.len() != selected.len() {
            return Err(ObservationCapabilityContractError::invalid(
                "observation capabilities omit a negotiated fact family",
            ));
        }
        Ok(())
    }
}

fn validate_selection_is_offered(
    selection: &ObservationContractSelection,
    offer: &ObservationContractOffer,
) -> Result<(), ObservationCapabilityContractError> {
    let selected = &selection.contract_versions;
    let offered = &offer.contract_versions;
    let fact_families_are_offered =
        selected
            .fact_family_versions
            .iter()
            .all(|(family, version)| {
                offered
                    .fact_family_versions
                    .get(family)
                    .is_some_and(|versions| versions.contains(version))
            });
    if selected.model_major != offered.model_major
        || !offered
            .external_entity_reference_versions
            .contains(&selected.external_entity_reference_version)
        || !offered
            .semantic_revision_reference_versions
            .contains(&selected.semantic_revision_reference_version)
        || !offered
            .coverage_contract_versions
            .contains(&selected.coverage_contract_version)
        || !fact_families_are_offered
        || selected.query_pack_version.is_some()
        || !selected
            .observation_contract_version
            .is_some_and(|version| offered.observation_contract_versions.contains(&version))
        || !offer
            .envelope_contract_versions
            .contains(&selection.envelope_contract_version)
        || !offer
            .event_contract_versions
            .contains(&selection.event_contract_version)
        || !offer
            .lifecycle_contract_versions
            .contains(&selection.lifecycle_contract_version)
    {
        return Err(ObservationCapabilityContractError::invalid(
            "observation selection is not contained in the caller-held host offer",
        ));
    }
    Ok(())
}

fn validate_selected_capability(
    capability: &ObservationFactFamilyCapability,
) -> Result<(), ObservationCapabilityContractError> {
    let ObservationCapabilityEvidence::PromotedSupportRelease {
        support_release_id,
        support,
    } = &capability.evidence
    else {
        return Err(ObservationCapabilityContractError::invalid(
            "selected capability requires promoted support-release evidence",
        ));
    };
    validate_identifier(
        "observation support release id",
        support_release_id,
        MAX_SUPPORT_RELEASE_ID_BYTES,
    )?;
    let common_limitations = [
        ObservationCapabilityLimitation::ScopeBound,
        ObservationCapabilityLimitation::CoverageReportedSeparately,
    ];
    if !common_limitations
        .iter()
        .all(|limitation| capability.limitations.contains(limitation))
        || capability.expected_timing != ObservationCapabilityExpectedTiming::BootstrapAndLive
    {
        return Err(ObservationCapabilityContractError::invalid(
            "selected capability is missing its scope, coverage, or timing qualification",
        ));
    }
    match support {
        ObservationCapabilitySupportEvidence::ExactPromotedRelease
            if capability.status == ObservationCapabilityStatus::Supported
                && capability.quality == ObservationCapabilityQuality::Exact
                && capability.expected_completeness == ContractCompleteness::Complete
                && !capability
                    .limitations
                    .contains(&ObservationCapabilityLimitation::RangeSupportedNativeVersion) =>
        {
            Ok(())
        }
        ObservationCapabilitySupportEvidence::RangeSupportedRelease
            if capability.status == ObservationCapabilityStatus::Degraded
                && capability.quality == ObservationCapabilityQuality::Qualified
                && capability.expected_completeness == ContractCompleteness::Partial
                && capability
                    .limitations
                    .contains(&ObservationCapabilityLimitation::RangeSupportedNativeVersion) =>
        {
            Ok(())
        }
        _ => Err(ObservationCapabilityContractError::invalid(
            "selected capability status does not match its support evidence",
        )),
    }
}

fn validate_unselected_capability(
    capability: &ObservationFactFamilyCapability,
) -> Result<(), ObservationCapabilityContractError> {
    let ObservationCapabilityEvidence::HostOfferNotSelected { offered_versions } =
        &capability.evidence
    else {
        return Err(ObservationCapabilityContractError::invalid(
            "unselected capability requires host-offer evidence",
        ));
    };
    validate_versions(offered_versions)?;
    if capability.status != ObservationCapabilityStatus::Unsupported
        || capability.quality != ObservationCapabilityQuality::Unavailable
        || capability.expected_timing != ObservationCapabilityExpectedTiming::Never
        || capability.expected_completeness != ContractCompleteness::Unknown
        || capability.limitations != [ObservationCapabilityLimitation::NotNegotiated]
    {
        return Err(ObservationCapabilityContractError::invalid(
            "unselected capability must remain explicitly unsupported",
        ));
    }
    Ok(())
}

fn validate_limitations(
    limitations: &[ObservationCapabilityLimitation],
) -> Result<(), ObservationCapabilityContractError> {
    if limitations.is_empty() || limitations.len() > 4 {
        return Err(ObservationCapabilityContractError::invalid(
            "capability limitations must be nonempty and bounded",
        ));
    }
    if limitations.windows(2).any(|window| window[0] >= window[1]) {
        return Err(ObservationCapabilityContractError::invalid(
            "capability limitations must be strictly sorted and unique",
        ));
    }
    Ok(())
}

fn validate_versions(versions: &[u32]) -> Result<(), ObservationCapabilityContractError> {
    if versions.is_empty() || versions.len() > MAX_OFFERED_VERSIONS {
        return Err(ObservationCapabilityContractError::invalid(
            "offered capability versions must be nonempty and bounded",
        ));
    }
    let mut seen = BTreeSet::new();
    if versions
        .iter()
        .any(|version| *version == 0 || !seen.insert(*version))
    {
        return Err(ObservationCapabilityContractError::invalid(
            "offered capability versions must be positive and unique",
        ));
    }
    Ok(())
}

fn validate_implemented_families<'a>(
    families: &'a [(&'a str, u32)],
) -> Result<BTreeMap<&'a str, u32>, ObservationCapabilityContractError> {
    if families.is_empty() || families.len() > MAX_CAPABILITY_FAMILIES {
        return Err(ObservationCapabilityContractError::invalid(
            "observer implementation family set must be nonempty and bounded",
        ));
    }
    let mut implemented = BTreeMap::new();
    for (family, version) in families {
        validate_family_identifier(family)?;
        if *version == 0 || implemented.insert(*family, *version).is_some() {
            return Err(ObservationCapabilityContractError::invalid(
                "observer implementation family set is invalid or duplicated",
            ));
        }
    }
    Ok(implemented)
}

fn validate_identifier(
    label: &str,
    value: &str,
    max_bytes: usize,
) -> Result<(), ObservationCapabilityContractError> {
    if value.is_empty()
        || value.len() > max_bytes
        || value.trim() != value
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'.' | b'_' | b'-' | b'@')
        })
        || !value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
    {
        return Err(ObservationCapabilityContractError::invalid(format!(
            "{label} is not a canonical bounded identifier"
        )));
    }
    Ok(())
}

fn validate_family_identifier(value: &str) -> Result<(), ObservationCapabilityContractError> {
    if value.is_empty()
        || value.len() > MAX_FAMILY_IDENTIFIER_BYTES
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
        || !value
            .as_bytes()
            .first()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    {
        return Err(ObservationCapabilityContractError::invalid(
            "observation capability family is not a canonical bounded identifier",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
