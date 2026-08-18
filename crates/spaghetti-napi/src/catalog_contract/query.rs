//! RFC 012B query-pack negotiation and portable continuation wire contracts.
//!
//! This module deliberately stops before engine execution or public N-API
//! exposure. It composes RFC 012A contract selection, proves the catalog pack
//! and cursor bindings, and preserves bounded additive wire data.

use std::collections::BTreeMap;
use std::fmt;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map as JsonMap, Value as JsonValue};

use crate::adapter::CONTRACT_VERSION_SELECTION_VERSION;
use crate::adapter::{
    select_contract_versions, ContractVersionOffer, ContractVersionRequest,
    ContractVersionSelection, EXTERNAL_ENTITY_REFERENCE_VERSION,
    SEMANTIC_REFERENCE_CONTRACT_VERSION, SOURCE_COVERAGE_CONTRACT_VERSION,
};

use super::{
    validate_identifier, CatalogContractError, CatalogCursor, CatalogQueryFingerprint,
    CatalogSnapshotId, CATALOG_QUERY_PACK_CONTRACT_VERSION,
};

pub(crate) const CATALOG_QUERY_CONTRACT_VERSION: u32 = 1;
pub(crate) const CATALOG_QUERY_RESPONSE_CONTRACT_VERSION: u32 = 1;
pub(crate) const CATALOG_CONTINUATION_REQUEST_CONTRACT_VERSION: u32 = 1;
pub(crate) const CATALOG_TYPED_UNKNOWN_CONTRACT_VERSION: u32 = 1;
pub(crate) const CATALOG_BASE_MODEL_MAJOR: u32 = 1;

const MAX_TYPED_UNKNOWN_PAYLOAD_BYTES: u32 = 64 * 1024;
const MAX_TYPED_UNKNOWN_DEPTH: usize = 16;
const MAX_TYPED_UNKNOWN_NODES: usize = 1_024;
const MAX_CONTINUATION_PAGE_SIZE: u32 = 1_000;
const MAX_JAVASCRIPT_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CatalogQueryCompatibilityAxis {
    BaseModelMajor,
    ExternalEntityReferenceVersion,
    SemanticRevisionReferenceVersion,
    CoverageContractVersion,
    FactFamilyVersion,
    QueryPackVersion,
    ObservationContractVersion,
    TypedUnknownPreservation,
}

impl fmt::Display for CatalogQueryCompatibilityAxis {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::BaseModelMajor => "base_model_major",
            Self::ExternalEntityReferenceVersion => "external_entity_reference_version",
            Self::SemanticRevisionReferenceVersion => "semantic_revision_reference_version",
            Self::CoverageContractVersion => "coverage_contract_version",
            Self::FactFamilyVersion => "fact_family_version",
            Self::QueryPackVersion => "query_pack_version",
            Self::ObservationContractVersion => "observation_contract_version",
            Self::TypedUnknownPreservation => "typed_unknown_preservation",
        };
        formatter.write_str(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum CatalogQueryNegotiationError {
    #[error("invalid catalog query contract: {message}")]
    InvalidCatalogContract { message: String },
    #[error("IncompatibleCatalogContract: {axis}")]
    IncompatibleCatalogContract { axis: CatalogQueryCompatibilityAxis },
}

impl CatalogQueryNegotiationError {
    fn invalid(error: impl fmt::Display) -> Self {
        Self::InvalidCatalogContract {
            message: error.to_string(),
        }
    }

    fn incompatible(axis: CatalogQueryCompatibilityAxis) -> Self {
        Self::IncompatibleCatalogContract { axis }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CatalogTypedUnknownCapability {
    pub typed_unknown_contract_version: u32,
    pub preserves_unknown_fields: bool,
    pub preserves_unknown_variants: bool,
    pub max_payload_bytes: u32,
}

impl CatalogTypedUnknownCapability {
    pub(crate) fn preserving(max_payload_bytes: u32) -> Result<Self, CatalogContractError> {
        let value = Self {
            typed_unknown_contract_version: CATALOG_TYPED_UNKNOWN_CONTRACT_VERSION,
            preserves_unknown_fields: true,
            preserves_unknown_variants: true,
            max_payload_bytes,
        };
        value.validate_shape()?;
        Ok(value)
    }

    fn validate_shape(&self) -> Result<(), CatalogContractError> {
        if self.typed_unknown_contract_version == 0 {
            return Err(CatalogContractError::invalid(
                "catalog typed-unknown contract version must be greater than zero",
            ));
        }
        if self.max_payload_bytes == 0 || self.max_payload_bytes > MAX_TYPED_UNKNOWN_PAYLOAD_BYTES {
            return Err(CatalogContractError::invalid(format!(
                "catalog typed-unknown payload bound must be 1..={MAX_TYPED_UNKNOWN_PAYLOAD_BYTES} bytes"
            )));
        }
        Ok(())
    }

    fn validate_selected(&self) -> Result<(), CatalogContractError> {
        self.validate_shape()?;
        if self.typed_unknown_contract_version != CATALOG_TYPED_UNKNOWN_CONTRACT_VERSION
            || !self.preserves_unknown_fields
            || !self.preserves_unknown_variants
        {
            return Err(CatalogContractError::invalid(
                "selected catalog query contract must preserve bounded unknown fields and variants",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CatalogQueryContractRequest {
    pub catalog_query_contract_version: u32,
    pub contract_versions: ContractVersionRequest,
    pub typed_unknown: CatalogTypedUnknownCapability,
}

impl CatalogQueryContractRequest {
    pub(crate) fn new(
        contract_versions: ContractVersionRequest,
        typed_unknown: CatalogTypedUnknownCapability,
    ) -> Result<Self, CatalogContractError> {
        let value = Self {
            catalog_query_contract_version: CATALOG_QUERY_CONTRACT_VERSION,
            contract_versions,
            typed_unknown,
        };
        value.validate()?;
        Ok(value)
    }

    fn validate(&self) -> Result<(), CatalogContractError> {
        if self.catalog_query_contract_version != CATALOG_QUERY_CONTRACT_VERSION {
            return Err(CatalogContractError::invalid(format!(
                "unsupported catalog query contract version {}",
                self.catalog_query_contract_version
            )));
        }
        self.contract_versions
            .validate()
            .map_err(|error| CatalogContractError::invalid(error.to_string()))?;
        if self.contract_versions.query_pack_versions.is_none() {
            return Err(CatalogContractError::invalid(
                "catalog query negotiation must explicitly request a query-pack version",
            ));
        }
        self.typed_unknown.validate_shape()
    }
}

impl<'de> Deserialize<'de> for CatalogQueryContractRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            catalog_query_contract_version: u32,
            contract_versions: ContractVersionRequest,
            typed_unknown: CatalogTypedUnknownCapability,
        }

        let wire = Wire::deserialize(deserializer)?;
        let value = Self {
            catalog_query_contract_version: wire.catalog_query_contract_version,
            contract_versions: wire.contract_versions,
            typed_unknown: wire.typed_unknown,
        };
        value.validate().map_err(D::Error::custom)?;
        Ok(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CatalogQueryContractOffer {
    pub catalog_query_contract_version: u32,
    pub contract_versions: ContractVersionOffer,
    pub typed_unknown: CatalogTypedUnknownCapability,
}

impl CatalogQueryContractOffer {
    pub(crate) fn new(
        contract_versions: ContractVersionOffer,
        typed_unknown: CatalogTypedUnknownCapability,
    ) -> Result<Self, CatalogContractError> {
        let value = Self {
            catalog_query_contract_version: CATALOG_QUERY_CONTRACT_VERSION,
            contract_versions,
            typed_unknown,
        };
        value.validate()?;
        Ok(value)
    }

    fn validate(&self) -> Result<(), CatalogContractError> {
        if self.catalog_query_contract_version != CATALOG_QUERY_CONTRACT_VERSION {
            return Err(CatalogContractError::invalid(format!(
                "unsupported catalog query contract version {}",
                self.catalog_query_contract_version
            )));
        }
        self.contract_versions
            .validate()
            .map_err(|error| CatalogContractError::invalid(error.to_string()))?;
        self.typed_unknown.validate_shape()
    }
}

impl<'de> Deserialize<'de> for CatalogQueryContractOffer {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            catalog_query_contract_version: u32,
            contract_versions: ContractVersionOffer,
            typed_unknown: CatalogTypedUnknownCapability,
        }

        let wire = Wire::deserialize(deserializer)?;
        let value = Self {
            catalog_query_contract_version: wire.catalog_query_contract_version,
            contract_versions: wire.contract_versions,
            typed_unknown: wire.typed_unknown,
        };
        value.validate().map_err(D::Error::custom)?;
        Ok(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CatalogQueryContractSelection {
    pub catalog_query_contract_version: u32,
    pub contract_versions: ContractVersionSelection,
    pub typed_unknown: CatalogTypedUnknownCapability,
}

impl CatalogQueryContractSelection {
    fn validate(&self) -> Result<(), CatalogContractError> {
        if self.catalog_query_contract_version != CATALOG_QUERY_CONTRACT_VERSION {
            return Err(CatalogContractError::invalid(format!(
                "unsupported selected catalog query contract version {}",
                self.catalog_query_contract_version
            )));
        }
        let contracts = &self.contract_versions;
        if contracts.selection_contract_version != CONTRACT_VERSION_SELECTION_VERSION
            || contracts.model_major != CATALOG_BASE_MODEL_MAJOR
            || contracts.external_entity_reference_version != EXTERNAL_ENTITY_REFERENCE_VERSION
            || contracts.semantic_revision_reference_version != SEMANTIC_REFERENCE_CONTRACT_VERSION
            || contracts.coverage_contract_version != SOURCE_COVERAGE_CONTRACT_VERSION
            || contracts.query_pack_version != Some(CATALOG_QUERY_PACK_CONTRACT_VERSION)
        {
            return Err(CatalogContractError::invalid(
                "selected catalog query contract has an incompatible base or query-pack version",
            ));
        }
        for (family, version) in &contracts.fact_family_versions {
            validate_identifier("selected catalog fact family", family)?;
            if *version == 0 {
                return Err(CatalogContractError::invalid(
                    "selected catalog fact-family version must be greater than zero",
                ));
            }
        }
        if contracts.observation_contract_version == Some(0) {
            return Err(CatalogContractError::invalid(
                "selected observation contract version must be greater than zero",
            ));
        }
        self.typed_unknown.validate_selected()
    }
}

impl<'de> Deserialize<'de> for CatalogQueryContractSelection {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            catalog_query_contract_version: u32,
            contract_versions: ContractVersionSelection,
            typed_unknown: CatalogTypedUnknownCapability,
        }

        let wire = Wire::deserialize(deserializer)?;
        let value = Self {
            catalog_query_contract_version: wire.catalog_query_contract_version,
            contract_versions: wire.contract_versions,
            typed_unknown: wire.typed_unknown,
        };
        value.validate().map_err(D::Error::custom)?;
        Ok(value)
    }
}

fn first_common(requested: &[u32], offered: &[u32]) -> Option<u32> {
    requested
        .iter()
        .copied()
        .find(|version| offered.contains(version))
}

pub(crate) fn negotiate_catalog_query_contract(
    request: &CatalogQueryContractRequest,
    offer: &CatalogQueryContractOffer,
) -> Result<CatalogQueryContractSelection, CatalogQueryNegotiationError> {
    request
        .validate()
        .map_err(CatalogQueryNegotiationError::invalid)?;
    offer
        .validate()
        .map_err(CatalogQueryNegotiationError::invalid)?;
    let requested = &request.contract_versions;
    let offered = &offer.contract_versions;

    if requested.model_major != CATALOG_BASE_MODEL_MAJOR
        || offered.model_major != CATALOG_BASE_MODEL_MAJOR
        || requested.model_major != offered.model_major
    {
        return Err(CatalogQueryNegotiationError::incompatible(
            CatalogQueryCompatibilityAxis::BaseModelMajor,
        ));
    }
    if requested.external_entity_reference_version != EXTERNAL_ENTITY_REFERENCE_VERSION
        || !offered
            .external_entity_reference_versions
            .contains(&requested.external_entity_reference_version)
    {
        return Err(CatalogQueryNegotiationError::incompatible(
            CatalogQueryCompatibilityAxis::ExternalEntityReferenceVersion,
        ));
    }
    if requested.semantic_revision_reference_version != SEMANTIC_REFERENCE_CONTRACT_VERSION
        || !offered
            .semantic_revision_reference_versions
            .contains(&requested.semantic_revision_reference_version)
    {
        return Err(CatalogQueryNegotiationError::incompatible(
            CatalogQueryCompatibilityAxis::SemanticRevisionReferenceVersion,
        ));
    }
    if first_common(
        &requested.coverage_contract_versions,
        &offered.coverage_contract_versions,
    ) != Some(SOURCE_COVERAGE_CONTRACT_VERSION)
    {
        return Err(CatalogQueryNegotiationError::incompatible(
            CatalogQueryCompatibilityAxis::CoverageContractVersion,
        ));
    }
    for (family, versions) in &requested.fact_family_versions {
        let Some(offered_versions) = offered.fact_family_versions.get(family) else {
            return Err(CatalogQueryNegotiationError::incompatible(
                CatalogQueryCompatibilityAxis::FactFamilyVersion,
            ));
        };
        if first_common(versions, offered_versions).is_none() {
            return Err(CatalogQueryNegotiationError::incompatible(
                CatalogQueryCompatibilityAxis::FactFamilyVersion,
            ));
        }
    }
    let Some(requested_query_packs) = requested.query_pack_versions.as_ref() else {
        return Err(CatalogQueryNegotiationError::invalid(
            "catalog query negotiation requires query-pack versions",
        ));
    };
    if first_common(requested_query_packs, &offered.query_pack_versions)
        != Some(CATALOG_QUERY_PACK_CONTRACT_VERSION)
    {
        return Err(CatalogQueryNegotiationError::incompatible(
            CatalogQueryCompatibilityAxis::QueryPackVersion,
        ));
    }
    if let Some(requested_observation) = &requested.observation_contract_versions {
        if first_common(
            requested_observation,
            &offered.observation_contract_versions,
        )
        .is_none()
        {
            return Err(CatalogQueryNegotiationError::incompatible(
                CatalogQueryCompatibilityAxis::ObservationContractVersion,
            ));
        }
    }
    if request.typed_unknown.typed_unknown_contract_version
        != CATALOG_TYPED_UNKNOWN_CONTRACT_VERSION
        || offer.typed_unknown.typed_unknown_contract_version
            != CATALOG_TYPED_UNKNOWN_CONTRACT_VERSION
        || !request.typed_unknown.preserves_unknown_fields
        || !request.typed_unknown.preserves_unknown_variants
        || !offer.typed_unknown.preserves_unknown_fields
        || !offer.typed_unknown.preserves_unknown_variants
    {
        return Err(CatalogQueryNegotiationError::incompatible(
            CatalogQueryCompatibilityAxis::TypedUnknownPreservation,
        ));
    }

    let contract_versions = select_contract_versions(requested, offered)
        .map_err(CatalogQueryNegotiationError::invalid)?;
    let selection = CatalogQueryContractSelection {
        catalog_query_contract_version: CATALOG_QUERY_CONTRACT_VERSION,
        contract_versions,
        typed_unknown: CatalogTypedUnknownCapability {
            typed_unknown_contract_version: CATALOG_TYPED_UNKNOWN_CONTRACT_VERSION,
            preserves_unknown_fields: true,
            preserves_unknown_variants: true,
            max_payload_bytes: request
                .typed_unknown
                .max_payload_bytes
                .min(offer.typed_unknown.max_payload_bytes),
        },
    };
    selection
        .validate()
        .map_err(CatalogQueryNegotiationError::invalid)?;
    Ok(selection)
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum CatalogQueryContractResponse {
    Selected {
        selection: CatalogQueryContractSelection,
        additive_fields: BTreeMap<String, JsonValue>,
    },
    TypedUnknown {
        selection: CatalogQueryContractSelection,
        variant: String,
        payload: BTreeMap<String, JsonValue>,
    },
}

impl CatalogQueryContractResponse {
    pub(crate) fn selected(
        selection: CatalogQueryContractSelection,
        additive_fields: BTreeMap<String, JsonValue>,
    ) -> Result<Self, CatalogContractError> {
        selection.validate()?;
        validate_response_payload_fields(&additive_fields, &selection.typed_unknown)?;
        Ok(Self::Selected {
            selection,
            additive_fields,
        })
    }

    pub(crate) fn from_wire_value(
        value: JsonValue,
        expected_selection: &CatalogQueryContractSelection,
    ) -> Result<Self, CatalogContractError> {
        expected_selection.validate()?;
        let JsonValue::Object(mut object) = value else {
            return Err(CatalogContractError::invalid(
                "catalog query contract response must be an object",
            ));
        };
        let version = object
            .remove("catalog_query_response_contract_version")
            .and_then(|value| value.as_u64())
            .ok_or_else(|| {
                CatalogContractError::invalid(
                    "catalog query contract response requires an integer contract version",
                )
            })?;
        if version != u64::from(CATALOG_QUERY_RESPONSE_CONTRACT_VERSION) {
            return Err(CatalogContractError::invalid(format!(
                "unsupported catalog query response contract version {version}"
            )));
        }
        let selection = serde_json::from_value::<CatalogQueryContractSelection>(
            object.remove("contract_selection").ok_or_else(|| {
                CatalogContractError::invalid(
                    "catalog query contract response requires its contract selection",
                )
            })?,
        )
        .map_err(|error| CatalogContractError::invalid(error.to_string()))?;
        if selection != *expected_selection {
            return Err(CatalogContractError::invalid(
                "catalog query response does not match the negotiated contract selection",
            ));
        }
        let variant = object
            .remove("kind")
            .and_then(|value| value.as_str().map(str::to_owned))
            .ok_or_else(|| {
                CatalogContractError::invalid(
                    "catalog query contract response requires a string variant",
                )
            })?;
        validate_identifier("catalog query response variant", &variant)?;
        let payload: BTreeMap<_, _> = object.into_iter().collect();
        validate_response_payload_fields(&payload, &selection.typed_unknown)?;
        if variant == "selected" {
            Ok(Self::Selected {
                selection,
                additive_fields: payload,
            })
        } else {
            Ok(Self::TypedUnknown {
                selection,
                variant,
                payload,
            })
        }
    }

    pub(crate) fn to_wire_value(&self) -> Result<JsonValue, CatalogContractError> {
        let (selection, variant, payload) = match self {
            Self::Selected {
                selection,
                additive_fields,
            } => (selection, "selected", additive_fields),
            Self::TypedUnknown {
                selection,
                variant,
                payload,
            } => (selection, variant.as_str(), payload),
        };
        selection.validate()?;
        validate_identifier("catalog query response variant", variant)?;
        validate_response_payload_fields(payload, &selection.typed_unknown)?;
        let mut object = JsonMap::new();
        object.insert(
            "catalog_query_response_contract_version".to_owned(),
            JsonValue::from(CATALOG_QUERY_RESPONSE_CONTRACT_VERSION),
        );
        object.insert(
            "contract_selection".to_owned(),
            serde_json::to_value(selection)
                .map_err(|error| CatalogContractError::invalid(error.to_string()))?,
        );
        object.insert("kind".to_owned(), JsonValue::String(variant.to_owned()));
        for (key, value) in payload {
            if object.insert(key.clone(), value.clone()).is_some() {
                return Err(CatalogContractError::invalid(
                    "catalog typed-unknown payload cannot replace a response contract field",
                ));
            }
        }
        Ok(JsonValue::Object(object))
    }
}

fn validate_response_payload_fields(
    fields: &BTreeMap<String, JsonValue>,
    capability: &CatalogTypedUnknownCapability,
) -> Result<(), CatalogContractError> {
    if fields.keys().any(|key| {
        matches!(
            key.as_str(),
            "catalog_query_response_contract_version" | "contract_selection" | "kind"
        )
    }) {
        return Err(CatalogContractError::invalid(
            "catalog additive fields cannot replace a response contract field",
        ));
    }
    validate_typed_unknown_fields(fields, capability)
}

#[derive(Debug, Default)]
struct UnknownPayloadBudget {
    bytes: usize,
    nodes: usize,
}

impl UnknownPayloadBudget {
    fn add_bytes(&mut self, bytes: usize) -> Result<(), CatalogContractError> {
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .ok_or_else(|| CatalogContractError::invalid("catalog typed-unknown size overflow"))?;
        Ok(())
    }

    fn add_node(&mut self) -> Result<(), CatalogContractError> {
        self.nodes = self
            .nodes
            .checked_add(1)
            .ok_or_else(|| CatalogContractError::invalid("catalog typed-unknown node overflow"))?;
        if self.nodes > MAX_TYPED_UNKNOWN_NODES {
            return Err(CatalogContractError::invalid(format!(
                "catalog typed-unknown payload exceeds {MAX_TYPED_UNKNOWN_NODES} nodes"
            )));
        }
        self.add_bytes(1)
    }
}

fn validate_typed_unknown_fields(
    fields: &BTreeMap<String, JsonValue>,
    capability: &CatalogTypedUnknownCapability,
) -> Result<(), CatalogContractError> {
    capability.validate_selected()?;
    let mut budget = UnknownPayloadBudget::default();
    budget.add_node()?;
    for (key, value) in fields {
        validate_identifier("catalog additive field", key)?;
        if matches!(key.as_str(), "__proto__" | "prototype" | "constructor") {
            return Err(CatalogContractError::invalid(
                "catalog additive field uses a reserved object key",
            ));
        }
        budget.add_bytes(key.len())?;
        validate_typed_unknown_value(value, 1, &mut budget)?;
    }
    if budget.bytes > capability.max_payload_bytes as usize {
        return Err(CatalogContractError::invalid(format!(
            "catalog typed-unknown payload exceeds the negotiated {} byte bound",
            capability.max_payload_bytes
        )));
    }
    Ok(())
}

fn validate_typed_unknown_value(
    value: &JsonValue,
    depth: usize,
    budget: &mut UnknownPayloadBudget,
) -> Result<(), CatalogContractError> {
    if depth > MAX_TYPED_UNKNOWN_DEPTH {
        return Err(CatalogContractError::invalid(format!(
            "catalog typed-unknown payload exceeds depth {MAX_TYPED_UNKNOWN_DEPTH}"
        )));
    }
    budget.add_node()?;
    match value {
        JsonValue::Null => Ok(()),
        JsonValue::Bool(_) => budget.add_bytes(1),
        JsonValue::Number(number) => {
            let portable = number
                .as_i64()
                .is_some_and(|value| value.unsigned_abs() <= MAX_JAVASCRIPT_SAFE_INTEGER)
                || number
                    .as_u64()
                    .is_some_and(|value| value <= MAX_JAVASCRIPT_SAFE_INTEGER);
            if !portable {
                return Err(CatalogContractError::invalid(
                    "catalog typed-unknown numbers must be JavaScript-safe integers",
                ));
            }
            budget.add_bytes(8)
        }
        JsonValue::String(value) => budget.add_bytes(value.len()),
        JsonValue::Array(values) => {
            for value in values {
                validate_typed_unknown_value(value, depth + 1, budget)?;
            }
            Ok(())
        }
        JsonValue::Object(values) => {
            for (key, value) in values {
                validate_identifier("catalog typed-unknown object key", key)?;
                if matches!(key.as_str(), "__proto__" | "prototype" | "constructor") {
                    return Err(CatalogContractError::invalid(
                        "catalog typed-unknown object uses a reserved key",
                    ));
                }
                budget.add_bytes(key.len())?;
                validate_typed_unknown_value(value, depth + 1, budget)?;
            }
            Ok(())
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CatalogContinuationRequest {
    pub catalog_continuation_request_contract_version: u32,
    pub contract_selection: CatalogQueryContractSelection,
    pub snapshot_id: CatalogSnapshotId,
    pub query_fingerprint: CatalogQueryFingerprint,
    pub sort_spec_version: u32,
    pub cursor: CatalogCursor,
    pub page_size: u32,
}

impl CatalogContinuationRequest {
    pub(crate) fn new(
        contract_selection: CatalogQueryContractSelection,
        snapshot_id: CatalogSnapshotId,
        query_fingerprint: CatalogQueryFingerprint,
        sort_spec_version: u32,
        cursor: CatalogCursor,
        page_size: u32,
    ) -> Result<Self, CatalogContractError> {
        let value = Self {
            catalog_continuation_request_contract_version:
                CATALOG_CONTINUATION_REQUEST_CONTRACT_VERSION,
            contract_selection,
            snapshot_id,
            query_fingerprint,
            sort_spec_version,
            cursor,
            page_size,
        };
        value.validate()?;
        Ok(value)
    }

    pub(crate) fn from_wire_value(
        value: JsonValue,
        expected_selection: &CatalogQueryContractSelection,
    ) -> Result<Self, CatalogContractError> {
        #[derive(Deserialize)]
        struct Wire {
            catalog_continuation_request_contract_version: u32,
            contract_selection: CatalogQueryContractSelection,
            snapshot_id: CatalogSnapshotId,
            query_fingerprint: CatalogQueryFingerprint,
            sort_spec_version: u32,
            cursor: CatalogCursor,
            page_size: u32,
        }

        let wire = serde_json::from_value::<Wire>(value)
            .map_err(|error| CatalogContractError::invalid(error.to_string()))?;
        let parsed = Self {
            catalog_continuation_request_contract_version: wire
                .catalog_continuation_request_contract_version,
            contract_selection: wire.contract_selection,
            snapshot_id: wire.snapshot_id,
            query_fingerprint: wire.query_fingerprint,
            sort_spec_version: wire.sort_spec_version,
            cursor: wire.cursor,
            page_size: wire.page_size,
        };
        parsed.validate_for_selection(expected_selection)?;
        Ok(parsed)
    }

    fn validate_for_selection(
        &self,
        expected_selection: &CatalogQueryContractSelection,
    ) -> Result<(), CatalogContractError> {
        self.validate()?;
        expected_selection.validate()?;
        if self.contract_selection != *expected_selection {
            return Err(CatalogContractError::invalid(
                "catalog continuation does not match the negotiated contract selection",
            ));
        }
        Ok(())
    }

    fn validate(&self) -> Result<(), CatalogContractError> {
        if self.catalog_continuation_request_contract_version
            != CATALOG_CONTINUATION_REQUEST_CONTRACT_VERSION
        {
            return Err(CatalogContractError::invalid(format!(
                "unsupported catalog continuation request contract version {}",
                self.catalog_continuation_request_contract_version
            )));
        }
        self.contract_selection.validate()?;
        self.snapshot_id.validate()?;
        let Some(selected_pack) = self.contract_selection.contract_versions.query_pack_version
        else {
            return Err(CatalogContractError::invalid(
                "catalog continuation requires a selected query-pack version",
            ));
        };
        if self.snapshot_id.pack_contract_version != selected_pack {
            return Err(CatalogContractError::invalid(
                "catalog continuation snapshot uses a different selected query pack",
            ));
        }
        if self.snapshot_id.readiness_epoch > MAX_JAVASCRIPT_SAFE_INTEGER
            || self.snapshot_id.complete_commit > MAX_JAVASCRIPT_SAFE_INTEGER
        {
            return Err(CatalogContractError::invalid(
                "catalog continuation snapshot counters must be JavaScript-safe integers",
            ));
        }
        if self.page_size == 0 || self.page_size > MAX_CONTINUATION_PAGE_SIZE {
            return Err(CatalogContractError::invalid(format!(
                "catalog continuation page size must be 1..={MAX_CONTINUATION_PAGE_SIZE}"
            )));
        }
        self.cursor.validate_binding(
            self.snapshot_id,
            self.query_fingerprint,
            self.sort_spec_version,
        )
    }
}

#[cfg(test)]
mod tests;
