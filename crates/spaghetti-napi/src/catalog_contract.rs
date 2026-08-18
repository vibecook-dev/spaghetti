//! RFC 012B library-catalog identity, pagination, and readiness contracts.
//!
//! This module is deliberately crate-private while RFC 012B remains a draft.
//! It contains no persistence, source-runtime, vendor-adapter, or transport
//! dependencies. The durable catalog pack and public request DTOs will compose
//! these checked semantic values after the B1 contract fixtures settle.

use std::collections::BTreeSet;
use std::fmt;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::adapter::{
    CanonicalEntityKey, CanonicalSourceInstanceKey, CoverageDeclarationDigest, CoverageDomain,
    CoverageSetCompleteness, ExternalEntityRef, SourceCoverageSet,
    EXTERNAL_ENTITY_REFERENCE_VERSION,
};

pub(crate) const CATALOG_COVERAGE_PLAN_CONTRACT_VERSION: u32 = 1;
pub(crate) const CATALOG_QUERY_PACK_CONTRACT_VERSION: u32 = 1;
pub(crate) const CATALOG_CURSOR_CONTRACT_VERSION: u32 = 1;
pub(crate) const CATALOG_READINESS_CONTRACT_VERSION: u32 = 1;
pub(crate) const CATALOG_PROJECTION_PACK_ID: &str = "library.catalog";

const DIGEST_BYTES: usize = 32;
const REFERENCE_ENCODING_VERSION: &str = "v1";
const MAX_IDENTIFIER_BYTES: usize = 256;
const MAX_PLAN_SOURCES: usize = 4_096;
const MAX_QUERY_BYTES: usize = 64 * 1024;
const MAX_SORT_KEY_BYTES: usize = 64 * 1024;
const MAX_REASON_CODE_BYTES: usize = 1_024;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub(crate) struct CatalogContractError {
    message: String,
}

impl CatalogContractError {
    fn invalid(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

fn validate_identifier(label: &str, value: &str) -> Result<(), CatalogContractError> {
    if value.is_empty() || value.trim() != value {
        return Err(CatalogContractError::invalid(format!(
            "{label} must be non-empty and canonical"
        )));
    }
    if value.len() > MAX_IDENTIFIER_BYTES {
        return Err(CatalogContractError::invalid(format!(
            "{label} exceeds {MAX_IDENTIFIER_BYTES} bytes"
        )));
    }
    Ok(())
}

fn validate_reason_code(value: &str) -> Result<(), CatalogContractError> {
    if value.is_empty() || value.trim() != value || value.len() > MAX_REASON_CODE_BYTES {
        return Err(CatalogContractError::invalid(format!(
            "catalog readiness reason must be canonical and at most {MAX_REASON_CODE_BYTES} bytes"
        )));
    }
    Ok(())
}

fn contract_digest(domain: &[u8], components: &[&[u8]]) -> [u8; DIGEST_BYTES] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"spaghetti/rfc012b/contract\0");
    hasher.update(&(domain.len() as u64).to_be_bytes());
    hasher.update(domain);
    hasher.update(&(components.len() as u64).to_be_bytes());
    for component in components {
        hasher.update(&(component.len() as u64).to_be_bytes());
        hasher.update(component);
    }
    *hasher.finalize().as_bytes()
}

fn serialize_digest<S>(bytes: &[u8; DIGEST_BYTES], serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(&format!(
        "{REFERENCE_ENCODING_VERSION}:{}",
        URL_SAFE_NO_PAD.encode(bytes)
    ))
}

fn deserialize_digest<'de, D>(deserializer: D) -> Result<[u8; DIGEST_BYTES], D::Error>
where
    D: Deserializer<'de>,
{
    let encoded = String::deserialize(deserializer)?;
    let Some(payload) = encoded.strip_prefix(&format!("{REFERENCE_ENCODING_VERSION}:")) else {
        return Err(D::Error::custom(
            "catalog reference has an unsupported encoding version",
        ));
    };
    let decoded = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|error| D::Error::custom(format!("invalid catalog reference: {error}")))?;
    decoded
        .try_into()
        .map_err(|_| D::Error::custom("catalog reference must contain exactly 32 digest bytes"))
}

macro_rules! opaque_digest_type {
    ($name:ident) => {
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub(crate) struct $name([u8; DIGEST_BYTES]);

        impl $name {
            fn from_digest(value: [u8; DIGEST_BYTES]) -> Self {
                Self(value)
            }

            fn as_bytes(&self) -> &[u8; DIGEST_BYTES] {
                &self.0
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter
                    .debug_tuple(stringify!($name))
                    .field(&format_args!(
                        "{REFERENCE_ENCODING_VERSION}:{}",
                        URL_SAFE_NO_PAD.encode(self.0)
                    ))
                    .finish()
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serialize_digest(&self.0, serializer)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                deserialize_digest(deserializer).map(Self)
            }
        }
    };
}

opaque_digest_type!(CatalogCoveragePlanId);
opaque_digest_type!(CatalogAccessPolicyDigest);
opaque_digest_type!(CatalogQueryFingerprint);
opaque_digest_type!(CatalogHydrationRequestKey);
opaque_digest_type!(CatalogHydrationCommandId);
opaque_digest_type!(CatalogHydrationCoalescingKey);
opaque_digest_type!(CatalogHydrationAuthorizationId);
opaque_digest_type!(CatalogSchedulingReceiptId);

pub(crate) mod evidence;
pub(crate) mod hydration;
pub(crate) mod page;
pub(crate) mod query;

impl CatalogAccessPolicyDigest {
    pub(crate) fn derive(
        policy_contract_version: u32,
        canonical_policy: &[u8],
    ) -> Result<Self, CatalogContractError> {
        if policy_contract_version == 0 {
            return Err(CatalogContractError::invalid(
                "catalog access-policy contract version must be greater than zero",
            ));
        }
        if canonical_policy.is_empty() || canonical_policy.len() > MAX_QUERY_BYTES {
            return Err(CatalogContractError::invalid(format!(
                "canonical catalog access policy must contain 1..={MAX_QUERY_BYTES} bytes"
            )));
        }
        Ok(Self::from_digest(contract_digest(
            b"catalog-access-policy",
            &[&policy_contract_version.to_be_bytes(), canonical_policy],
        )))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum CatalogCoverageScope {
    Library,
    Entity { external_ref: ExternalEntityRef },
}

impl<'de> Deserialize<'de> for CatalogCoverageScope {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
        enum Wire {
            Library {},
            Entity { external_ref: ExternalEntityRef },
        }

        let value = match Wire::deserialize(deserializer)? {
            Wire::Library {} => Self::Library,
            Wire::Entity { external_ref } => Self::Entity { external_ref },
        };
        value.validate().map_err(D::Error::custom)?;
        Ok(value)
    }
}

impl CatalogCoverageScope {
    fn validate(&self) -> Result<(), CatalogContractError> {
        if let Self::Entity { external_ref } = self {
            if external_ref.external_entity_reference_version != EXTERNAL_ENTITY_REFERENCE_VERSION {
                return Err(CatalogContractError::invalid(
                    "catalog scope uses an unsupported external entity reference",
                ));
            }
        }
        Ok(())
    }

    fn encode_into(&self, encoded: &mut Vec<u8>) {
        match self {
            Self::Library => encoded.push(1),
            Self::Entity { external_ref } => {
                encoded.push(2);
                encoded.extend_from_slice(
                    &external_ref.external_entity_reference_version.to_be_bytes(),
                );
                encoded.extend_from_slice(external_ref.entity_key.as_bytes());
            }
        }
    }

    fn root_entity_key(self) -> Option<CanonicalEntityKey> {
        match self {
            Self::Library => None,
            Self::Entity { external_ref } => Some(external_ref.entity_key),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CatalogCoveragePlanSource {
    pub adapter_id: String,
    pub source_instance_key: CanonicalSourceInstanceKey,
    pub support_release_id: String,
    pub catalog_declaration_digest: CoverageDeclarationDigest,
    pub access_policy_digest: CatalogAccessPolicyDigest,
}

impl CatalogCoveragePlanSource {
    pub(crate) fn new(
        adapter_id: impl Into<String>,
        source_instance_key: CanonicalSourceInstanceKey,
        support_release_id: impl Into<String>,
        catalog_declaration_digest: CoverageDeclarationDigest,
        access_policy_digest: CatalogAccessPolicyDigest,
    ) -> Result<Self, CatalogContractError> {
        let value = Self {
            adapter_id: adapter_id.into(),
            source_instance_key,
            support_release_id: support_release_id.into(),
            catalog_declaration_digest,
            access_policy_digest,
        };
        value.validate()?;
        Ok(value)
    }

    fn validate(&self) -> Result<(), CatalogContractError> {
        validate_identifier("catalog adapter id", &self.adapter_id)?;
        validate_identifier("catalog support release id", &self.support_release_id)
    }

    /// Exact catalog-readiness proof carried in the generic coverage scope.
    ///
    /// A catalog declaration alone cannot authorize completeness after an
    /// access-policy change. The scope declaration digest is therefore bound
    /// to both immutable inputs under a catalog-specific derivation version.
    pub(crate) fn coverage_binding_digest(&self) -> CoverageDeclarationDigest {
        let mut encoded = Vec::with_capacity(64 + 35);
        encoded.extend_from_slice(b"catalog-coverage-binding-v1");
        encoded.extend_from_slice(self.catalog_declaration_digest.as_bytes());
        encoded.extend_from_slice(self.access_policy_digest.as_bytes());
        CoverageDeclarationDigest::derive(&encoded)
            .expect("fixed catalog coverage binding material is valid")
    }

    fn coordinate(&self) -> (&str, CanonicalSourceInstanceKey) {
        (&self.adapter_id, self.source_instance_key)
    }

    fn encode_into(&self, encoded: &mut Vec<u8>) {
        encode_bytes(encoded, self.adapter_id.as_bytes());
        encoded.extend_from_slice(self.source_instance_key.as_bytes());
        encode_bytes(encoded, self.support_release_id.as_bytes());
        encoded.extend_from_slice(self.catalog_declaration_digest.as_bytes());
        encoded.extend_from_slice(self.access_policy_digest.as_bytes());
    }

    fn matches_coverage(&self, coverage: &SourceCoverageSet) -> bool {
        self.adapter_id == coverage.scope.adapter_id
            && self.source_instance_key == coverage.scope.source_instance_key
            && self.support_release_id == coverage.scope.support_release_id
            && self.coverage_binding_digest() == coverage.scope.source_or_scope_declaration_digest
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CatalogCoveragePlan {
    pub coverage_plan_contract_version: u32,
    pub coverage_plan_id: CatalogCoveragePlanId,
    pub scope: CatalogCoverageScope,
    pub required_sources: Vec<CatalogCoveragePlanSource>,
    pub optional_sources: Vec<CatalogCoveragePlanSource>,
}

impl CatalogCoveragePlan {
    pub(crate) fn new(
        scope: CatalogCoverageScope,
        mut required_sources: Vec<CatalogCoveragePlanSource>,
        mut optional_sources: Vec<CatalogCoveragePlanSource>,
    ) -> Result<Self, CatalogContractError> {
        required_sources.sort();
        optional_sources.sort();
        let mut value = Self {
            coverage_plan_contract_version: CATALOG_COVERAGE_PLAN_CONTRACT_VERSION,
            coverage_plan_id: CatalogCoveragePlanId::from_digest([0; DIGEST_BYTES]),
            scope,
            required_sources,
            optional_sources,
        };
        value.validate_content()?;
        value.coverage_plan_id = value.derive_id();
        Ok(value)
    }

    pub(crate) fn validate(&self) -> Result<(), CatalogContractError> {
        if self.coverage_plan_contract_version != CATALOG_COVERAGE_PLAN_CONTRACT_VERSION {
            return Err(CatalogContractError::invalid(format!(
                "unsupported catalog coverage-plan contract version {}",
                self.coverage_plan_contract_version
            )));
        }
        self.validate_content()?;
        if self.coverage_plan_id != self.derive_id() {
            return Err(CatalogContractError::invalid(
                "catalog coverage-plan id does not match normalized content",
            ));
        }
        Ok(())
    }

    fn validate_content(&self) -> Result<(), CatalogContractError> {
        self.scope.validate()?;
        let total_sources = self
            .required_sources
            .len()
            .checked_add(self.optional_sources.len())
            .ok_or_else(|| CatalogContractError::invalid("catalog source count overflow"))?;
        if total_sources > MAX_PLAN_SOURCES {
            return Err(CatalogContractError::invalid(format!(
                "catalog coverage plan exceeds {MAX_PLAN_SOURCES} sources"
            )));
        }
        if !self
            .required_sources
            .windows(2)
            .all(|pair| pair[0] < pair[1])
            || !self
                .optional_sources
                .windows(2)
                .all(|pair| pair[0] < pair[1])
        {
            return Err(CatalogContractError::invalid(
                "catalog coverage-plan sources must be strictly normalized",
            ));
        }
        let mut coordinates = BTreeSet::new();
        for source in self
            .required_sources
            .iter()
            .chain(self.optional_sources.iter())
        {
            source.validate()?;
            if !coordinates.insert(source.coordinate()) {
                return Err(CatalogContractError::invalid(
                    "a catalog source cannot be both repeated and assigned multiple optionality roles",
                ));
            }
        }
        Ok(())
    }

    fn derive_id(&self) -> CatalogCoveragePlanId {
        let mut encoded = Vec::new();
        encoded.extend_from_slice(&self.coverage_plan_contract_version.to_be_bytes());
        self.scope.encode_into(&mut encoded);
        encode_sources(&mut encoded, 1, &self.required_sources);
        encode_sources(&mut encoded, 2, &self.optional_sources);
        CatalogCoveragePlanId::from_digest(contract_digest(b"catalog-coverage-plan", &[&encoded]))
    }

    fn source_for_coverage(
        &self,
        coverage: &SourceCoverageSet,
    ) -> Option<&CatalogCoveragePlanSource> {
        self.required_sources
            .iter()
            .chain(self.optional_sources.iter())
            .find(|source| source.matches_coverage(coverage))
    }

    fn required_coverage_complete(&self, coverage: &[SourceCoverageSet]) -> bool {
        self.required_sources.iter().all(|required| {
            coverage.iter().any(|set| {
                required.matches_coverage(set)
                    && set.completeness == CoverageSetCompleteness::Complete
            })
        })
    }

    fn required_coverage_present(&self, coverage: &[SourceCoverageSet]) -> bool {
        self.required_sources
            .iter()
            .all(|required| coverage.iter().any(|set| required.matches_coverage(set)))
    }

    fn any_required_coverage_present(&self, coverage: &[SourceCoverageSet]) -> bool {
        self.required_sources
            .iter()
            .any(|required| coverage.iter().any(|set| required.matches_coverage(set)))
    }
}

impl<'de> Deserialize<'de> for CatalogCoveragePlan {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            coverage_plan_contract_version: u32,
            coverage_plan_id: CatalogCoveragePlanId,
            scope: CatalogCoverageScope,
            required_sources: Vec<CatalogCoveragePlanSource>,
            optional_sources: Vec<CatalogCoveragePlanSource>,
        }

        let wire = Wire::deserialize(deserializer)?;
        let value = Self {
            coverage_plan_contract_version: wire.coverage_plan_contract_version,
            coverage_plan_id: wire.coverage_plan_id,
            scope: wire.scope,
            required_sources: wire.required_sources,
            optional_sources: wire.optional_sources,
        };
        value.validate().map_err(D::Error::custom)?;
        Ok(value)
    }
}

fn encode_bytes(encoded: &mut Vec<u8>, bytes: &[u8]) {
    encoded.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
    encoded.extend_from_slice(bytes);
}

fn encode_sources(encoded: &mut Vec<u8>, role: u8, sources: &[CatalogCoveragePlanSource]) {
    encoded.push(role);
    encoded.extend_from_slice(&(sources.len() as u64).to_be_bytes());
    for source in sources {
        source.encode_into(encoded);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub(crate) struct CatalogSnapshotId {
    pub pack_contract_version: u32,
    pub coverage_plan_id: CatalogCoveragePlanId,
    pub readiness_epoch: u64,
    pub complete_commit: u64,
}

impl CatalogSnapshotId {
    pub(crate) fn new(
        pack_contract_version: u32,
        coverage_plan_id: CatalogCoveragePlanId,
        readiness_epoch: u64,
        complete_commit: u64,
    ) -> Result<Self, CatalogContractError> {
        let value = Self {
            pack_contract_version,
            coverage_plan_id,
            readiness_epoch,
            complete_commit,
        };
        value.validate()?;
        Ok(value)
    }

    fn validate(&self) -> Result<(), CatalogContractError> {
        if self.pack_contract_version == 0 {
            return Err(CatalogContractError::invalid(
                "catalog snapshot pack contract version must be greater than zero",
            ));
        }
        if self.readiness_epoch == 0 || self.complete_commit == 0 {
            return Err(CatalogContractError::invalid(
                "catalog snapshot epoch and complete commit must be greater than zero",
            ));
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for CatalogSnapshotId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            pack_contract_version: u32,
            coverage_plan_id: CatalogCoveragePlanId,
            readiness_epoch: u64,
            complete_commit: u64,
        }

        let wire = Wire::deserialize(deserializer)?;
        let value = Self {
            pack_contract_version: wire.pack_contract_version,
            coverage_plan_id: wire.coverage_plan_id,
            readiness_epoch: wire.readiness_epoch,
            complete_commit: wire.complete_commit,
        };
        value.validate().map_err(D::Error::custom)?;
        Ok(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CatalogQueryKind {
    Projects,
    Sessions,
}

impl CatalogQueryFingerprint {
    pub(crate) fn derive(
        pack_contract_version: u32,
        query_kind: CatalogQueryKind,
        scope: CatalogCoverageScope,
        sort_spec_version: u32,
        normalized_filter: &[u8],
    ) -> Result<Self, CatalogContractError> {
        if pack_contract_version == 0 || sort_spec_version == 0 {
            return Err(CatalogContractError::invalid(
                "catalog query pack and sort versions must be greater than zero",
            ));
        }
        scope.validate()?;
        if normalized_filter.len() > MAX_QUERY_BYTES {
            return Err(CatalogContractError::invalid(format!(
                "normalized catalog query exceeds {MAX_QUERY_BYTES} bytes"
            )));
        }
        let query_tag = match query_kind {
            CatalogQueryKind::Projects => [1],
            CatalogQueryKind::Sessions => [2],
        };
        let mut encoded_scope = Vec::new();
        scope.encode_into(&mut encoded_scope);
        Ok(Self::from_digest(contract_digest(
            b"catalog-query-fingerprint",
            &[
                &pack_contract_version.to_be_bytes(),
                &query_tag,
                &encoded_scope,
                &sort_spec_version.to_be_bytes(),
                normalized_filter,
            ],
        )))
    }
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct CatalogSortKey(Vec<u8>);

impl CatalogSortKey {
    pub(crate) fn new(value: Vec<u8>) -> Result<Self, CatalogContractError> {
        if value.is_empty() || value.len() > MAX_SORT_KEY_BYTES {
            return Err(CatalogContractError::invalid(format!(
                "catalog sort key must contain 1..={MAX_SORT_KEY_BYTES} bytes"
            )));
        }
        Ok(Self(value))
    }
}

impl fmt::Debug for CatalogSortKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("CatalogSortKey")
            .field(&format_args!("opaque:{}-bytes", self.0.len()))
            .finish()
    }
}

impl Serialize for CatalogSortKey {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&format!(
            "{REFERENCE_ENCODING_VERSION}:{}",
            URL_SAFE_NO_PAD.encode(&self.0)
        ))
    }
}

impl<'de> Deserialize<'de> for CatalogSortKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        let Some(payload) = encoded.strip_prefix(&format!("{REFERENCE_ENCODING_VERSION}:")) else {
            return Err(D::Error::custom(
                "catalog sort key has an unsupported encoding version",
            ));
        };
        let bytes = URL_SAFE_NO_PAD
            .decode(payload)
            .map_err(|error| D::Error::custom(format!("invalid catalog sort key: {error}")))?;
        Self::new(bytes).map_err(D::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CatalogCursor {
    pub cursor_contract_version: u32,
    pub snapshot_id: CatalogSnapshotId,
    pub query_fingerprint: CatalogQueryFingerprint,
    pub sort_spec_version: u32,
    pub last_sort_key: CatalogSortKey,
    pub last_entity_key: CanonicalEntityKey,
}

impl CatalogCursor {
    pub(crate) fn new(
        snapshot_id: CatalogSnapshotId,
        query_fingerprint: CatalogQueryFingerprint,
        sort_spec_version: u32,
        last_sort_key: CatalogSortKey,
        last_entity_key: CanonicalEntityKey,
    ) -> Result<Self, CatalogContractError> {
        let value = Self {
            cursor_contract_version: CATALOG_CURSOR_CONTRACT_VERSION,
            snapshot_id,
            query_fingerprint,
            sort_spec_version,
            last_sort_key,
            last_entity_key,
        };
        value.validate()?;
        Ok(value)
    }

    fn validate(&self) -> Result<(), CatalogContractError> {
        if self.cursor_contract_version != CATALOG_CURSOR_CONTRACT_VERSION {
            return Err(CatalogContractError::invalid(format!(
                "unsupported catalog cursor contract version {}",
                self.cursor_contract_version
            )));
        }
        self.snapshot_id.validate()?;
        if self.sort_spec_version == 0 {
            return Err(CatalogContractError::invalid(
                "catalog cursor sort version must be greater than zero",
            ));
        }
        Ok(())
    }

    pub(crate) fn validate_binding(
        &self,
        snapshot_id: CatalogSnapshotId,
        query_fingerprint: CatalogQueryFingerprint,
        sort_spec_version: u32,
    ) -> Result<(), CatalogContractError> {
        self.validate()?;
        if self.snapshot_id != snapshot_id {
            return Err(CatalogContractError::invalid(
                "catalog cursor is bound to a different retained snapshot",
            ));
        }
        if self.query_fingerprint != query_fingerprint {
            return Err(CatalogContractError::invalid(
                "catalog cursor is bound to a different query fingerprint",
            ));
        }
        if self.sort_spec_version != sort_spec_version {
            return Err(CatalogContractError::invalid(
                "catalog cursor is bound to a different sort specification",
            ));
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for CatalogCursor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            cursor_contract_version: u32,
            snapshot_id: CatalogSnapshotId,
            query_fingerprint: CatalogQueryFingerprint,
            sort_spec_version: u32,
            last_sort_key: CatalogSortKey,
            last_entity_key: CanonicalEntityKey,
        }

        let wire = Wire::deserialize(deserializer)?;
        let value = Self {
            cursor_contract_version: wire.cursor_contract_version,
            snapshot_id: wire.snapshot_id,
            query_fingerprint: wire.query_fingerprint,
            sort_spec_version: wire.sort_spec_version,
            last_sort_key: wire.last_sort_key,
            last_entity_key: wire.last_entity_key,
        };
        value.validate().map_err(D::Error::custom)?;
        Ok(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CatalogReadinessPhase {
    Pending,
    Building,
    Partial,
    Ready,
    Degraded,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CatalogIntegritySnapshotDisposition {
    IndependentlySafe,
    Discarded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum CatalogReadinessReason {
    SourceRetrying {
        code: String,
    },
    TerminalSourceUnavailable {
        code: String,
    },
    IntegrityFailure {
        code: String,
        snapshot_disposition: CatalogIntegritySnapshotDisposition,
    },
}

impl CatalogReadinessReason {
    fn validate(&self) -> Result<(), CatalogContractError> {
        let code = match self {
            Self::SourceRetrying { code }
            | Self::TerminalSourceUnavailable { code }
            | Self::IntegrityFailure { code, .. } => code,
        };
        validate_reason_code(code)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CatalogReadinessSnapshot {
    pub readiness_contract_version: u32,
    pub scope: CatalogCoverageScope,
    pub coverage_plan_id: CatalogCoveragePlanId,
    pub desired_contract_version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_contract_version: Option<u32>,
    pub epoch: u64,
    pub attempt: u64,
    pub state: CatalogReadinessPhase,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub complete_through_commit: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_complete_snapshot: Option<CatalogSnapshotId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refreshing_from_snapshot: Option<CatalogSnapshotId>,
    pub source_coverage: Vec<SourceCoverageSet>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<CatalogReadinessReason>,
}

impl CatalogReadinessSnapshot {
    fn validate_against(&self, plan: &CatalogCoveragePlan) -> Result<(), CatalogContractError> {
        plan.validate()?;
        if self.readiness_contract_version != CATALOG_READINESS_CONTRACT_VERSION {
            return Err(CatalogContractError::invalid(format!(
                "unsupported catalog readiness contract version {}",
                self.readiness_contract_version
            )));
        }
        if self.scope != plan.scope || self.coverage_plan_id != plan.coverage_plan_id {
            return Err(CatalogContractError::invalid(
                "catalog readiness is bound to a different scope or coverage plan",
            ));
        }
        if self.desired_contract_version == 0 || self.epoch == 0 || self.attempt == 0 {
            return Err(CatalogContractError::invalid(
                "catalog readiness versions, epoch, and attempt must be greater than zero",
            ));
        }
        if let Some(completed) = self.completed_contract_version {
            if completed == 0 || completed > self.desired_contract_version {
                return Err(CatalogContractError::invalid(
                    "completed catalog contract version must be nonzero and no newer than desired",
                ));
            }
        }
        if let Some(reason) = &self.reason {
            reason.validate()?;
        }
        match (&self.state, &self.reason) {
            (
                CatalogReadinessPhase::Degraded,
                Some(CatalogReadinessReason::TerminalSourceUnavailable { .. }),
            )
            | (
                CatalogReadinessPhase::Error,
                Some(CatalogReadinessReason::IntegrityFailure { .. }),
            ) => {}
            (CatalogReadinessPhase::Degraded, _) => {
                return Err(CatalogContractError::invalid(
                    "degraded catalog readiness requires terminal source-unavailability evidence",
                ));
            }
            (CatalogReadinessPhase::Error, _) => {
                return Err(CatalogContractError::invalid(
                    "catalog readiness error requires integrity-failure evidence",
                ));
            }
            (_, Some(CatalogReadinessReason::TerminalSourceUnavailable { .. }))
            | (_, Some(CatalogReadinessReason::IntegrityFailure { .. })) => {
                return Err(CatalogContractError::invalid(
                    "catalog readiness reason does not match its state",
                ));
            }
            _ => {}
        }

        match (self.completed_contract_version, self.last_complete_snapshot) {
            (Some(completed), Some(snapshot)) if completed == snapshot.pack_contract_version => {}
            (None, None) => {}
            _ => {
                return Err(CatalogContractError::invalid(
                    "completed catalog version and last-complete snapshot must agree",
                ));
            }
        }
        if let Some(CatalogReadinessReason::IntegrityFailure {
            snapshot_disposition,
            ..
        }) = &self.reason
        {
            match snapshot_disposition {
                CatalogIntegritySnapshotDisposition::IndependentlySafe => {
                    let Some(snapshot) = self.last_complete_snapshot else {
                        return Err(CatalogContractError::invalid(
                            "independently safe integrity failure must retain a complete snapshot",
                        ));
                    };
                    if self.completed_contract_version.is_none() {
                        return Err(CatalogContractError::invalid(
                            "independently safe integrity failure must retain its completed contract version",
                        ));
                    }
                    let expected_current_commit = (snapshot.coverage_plan_id
                        == self.coverage_plan_id
                        && snapshot.readiness_epoch == self.epoch)
                        .then_some(snapshot.complete_commit);
                    if self.complete_through_commit != expected_current_commit {
                        return Err(CatalogContractError::invalid(
                            "independently safe integrity failure must retain exactly its current snapshot commit",
                        ));
                    }
                }
                CatalogIntegritySnapshotDisposition::Discarded => {
                    if self.completed_contract_version.is_some()
                        || self.complete_through_commit.is_some()
                        || self.last_complete_snapshot.is_some()
                    {
                        return Err(CatalogContractError::invalid(
                            "discarded integrity failure cannot retain completed snapshot state",
                        ));
                    }
                }
            }
        }
        if let Some(snapshot) = self.last_complete_snapshot {
            snapshot.validate()?;
            if snapshot.readiness_epoch > self.epoch {
                return Err(CatalogContractError::invalid(
                    "last-complete catalog snapshot cannot come from a future readiness epoch",
                ));
            }
        }
        if self.complete_through_commit.is_some()
            && !matches!(
                self.state,
                CatalogReadinessPhase::Ready
                    | CatalogReadinessPhase::Degraded
                    | CatalogReadinessPhase::Error
            )
        {
            return Err(CatalogContractError::invalid(
                "only ready, degraded, or independently safe error state may be complete through a commit",
            ));
        }
        match (self.complete_through_commit, self.last_complete_snapshot) {
            (Some(commit), Some(snapshot))
                if snapshot.coverage_plan_id == self.coverage_plan_id
                    && snapshot.readiness_epoch == self.epoch
                    && snapshot.complete_commit == commit => {}
            (None, _) => {}
            _ => {
                return Err(CatalogContractError::invalid(
                    "current complete commit must identify the current epoch snapshot",
                ));
            }
        }
        if let Some(refreshing) = self.refreshing_from_snapshot {
            if self.state != CatalogReadinessPhase::Ready
                || Some(refreshing) != self.last_complete_snapshot
                || self.complete_through_commit != Some(refreshing.complete_commit)
            {
                return Err(CatalogContractError::invalid(
                    "catalog refresh must retain the exact current ready snapshot",
                ));
            }
        }
        if self.state == CatalogReadinessPhase::Ready {
            let Some(snapshot) = self.last_complete_snapshot else {
                return Err(CatalogContractError::invalid(
                    "ready catalog state requires a complete snapshot",
                ));
            };
            if snapshot.coverage_plan_id != self.coverage_plan_id
                || snapshot.readiness_epoch != self.epoch
                || snapshot.pack_contract_version != self.desired_contract_version
                || self.completed_contract_version != Some(self.desired_contract_version)
                || self.complete_through_commit != Some(snapshot.complete_commit)
            {
                return Err(CatalogContractError::invalid(
                    "ready catalog state must identify its exact plan, epoch, version, and commit",
                ));
            }
            match &self.reason {
                Some(CatalogReadinessReason::SourceRetrying { .. })
                    if plan.required_coverage_present(&self.source_coverage) => {}
                None if plan.required_coverage_complete(&self.source_coverage) => {}
                _ => {
                    return Err(CatalogContractError::invalid(
                        "ready catalog state requires complete snapshot coverage or explicit current retry coverage",
                    ));
                }
            }
        }
        if self.state == CatalogReadinessPhase::Degraded
            && !plan.required_coverage_present(&self.source_coverage)
        {
            return Err(CatalogContractError::invalid(
                "degraded catalog state must report current coverage for every required source",
            ));
        }

        let expected_domain = CoverageDomain::ProjectionPack {
            pack: CATALOG_PROJECTION_PACK_ID.to_string(),
            version: self.desired_contract_version,
        };
        let mut covered_sources = BTreeSet::new();
        let mut previous_coverage_coordinate = None;
        for coverage in &self.source_coverage {
            coverage.validate().map_err(|error| {
                CatalogContractError::invalid(format!("invalid catalog source coverage: {error}"))
            })?;
            if coverage.coverage_domain != expected_domain {
                return Err(CatalogContractError::invalid(
                    "catalog readiness coverage must use the selected library.catalog pack version",
                ));
            }
            if coverage.scope.root_entity_key != self.scope.root_entity_key() {
                return Err(CatalogContractError::invalid(
                    "catalog readiness coverage belongs to a different catalog scope",
                ));
            }
            let Some(source) = plan.source_for_coverage(coverage) else {
                return Err(CatalogContractError::invalid(
                    "catalog readiness contains coverage outside its frozen plan",
                ));
            };
            let coverage_coordinate = (
                coverage.scope.adapter_id.clone(),
                coverage.scope.source_instance_key,
            );
            if previous_coverage_coordinate
                .as_ref()
                .is_some_and(|previous| previous >= &coverage_coordinate)
            {
                return Err(CatalogContractError::invalid(
                    "catalog readiness source coverage must be canonical and duplicate-free",
                ));
            }
            previous_coverage_coordinate = Some(coverage_coordinate);
            if coverage.points.iter().any(|point| point.generation == 0)
                || coverage
                    .explicit_absence_or_deletion
                    .iter()
                    .any(|absence| absence.generation == 0)
            {
                return Err(CatalogContractError::invalid(
                    "catalog readiness coverage generations must be greater than zero",
                ));
            }
            if !covered_sources.insert(source.coordinate()) {
                return Err(CatalogContractError::invalid(
                    "catalog readiness contains duplicate source coverage",
                ));
            }
        }
        if self.state == CatalogReadinessPhase::Partial
            && !plan.any_required_coverage_present(&self.source_coverage)
        {
            return Err(CatalogContractError::invalid(
                "partial catalog state requires coverage for at least one required source",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub(crate) struct CatalogReadinessMachine {
    plan: CatalogCoveragePlan,
    snapshot: CatalogReadinessSnapshot,
}

impl CatalogReadinessMachine {
    pub(crate) fn register(
        plan: CatalogCoveragePlan,
        desired_contract_version: u32,
    ) -> Result<Self, CatalogContractError> {
        plan.validate()?;
        if desired_contract_version == 0 {
            return Err(CatalogContractError::invalid(
                "desired catalog contract version must be greater than zero",
            ));
        }
        let snapshot = CatalogReadinessSnapshot {
            readiness_contract_version: CATALOG_READINESS_CONTRACT_VERSION,
            scope: plan.scope,
            coverage_plan_id: plan.coverage_plan_id,
            desired_contract_version,
            completed_contract_version: None,
            epoch: 1,
            attempt: 1,
            state: CatalogReadinessPhase::Pending,
            complete_through_commit: None,
            last_complete_snapshot: None,
            refreshing_from_snapshot: None,
            source_coverage: Vec::new(),
            reason: None,
        };
        snapshot.validate_against(&plan)?;
        Ok(Self { plan, snapshot })
    }

    pub(crate) fn resume(
        plan: CatalogCoveragePlan,
        snapshot: CatalogReadinessSnapshot,
    ) -> Result<Self, CatalogContractError> {
        snapshot.validate_against(&plan)?;
        Ok(Self { plan, snapshot })
    }

    pub(crate) fn plan(&self) -> &CatalogCoveragePlan {
        &self.plan
    }

    pub(crate) fn snapshot(&self) -> &CatalogReadinessSnapshot {
        &self.snapshot
    }

    pub(crate) fn schedule_build(&mut self) -> Result<(), CatalogContractError> {
        if self.snapshot.state != CatalogReadinessPhase::Pending {
            return Err(CatalogContractError::invalid(
                "only pending catalog readiness may schedule its initial build",
            ));
        }
        let mut candidate = self.snapshot.clone();
        candidate.state = CatalogReadinessPhase::Building;
        candidate.reason = None;
        self.replace_snapshot(candidate)
    }

    pub(crate) fn record_partial(
        &mut self,
        source_coverage: Vec<SourceCoverageSet>,
    ) -> Result<(), CatalogContractError> {
        if !matches!(
            self.snapshot.state,
            CatalogReadinessPhase::Building | CatalogReadinessPhase::Partial
        ) || source_coverage.is_empty()
        {
            return Err(CatalogContractError::invalid(
                "partial catalog progress requires a building/partial state and source coverage",
            ));
        }
        let mut candidate = self.snapshot.clone();
        candidate.state = CatalogReadinessPhase::Partial;
        candidate.source_coverage = source_coverage;
        candidate.reason = None;
        self.replace_snapshot(candidate)
    }

    pub(crate) fn publish_ready(
        &mut self,
        snapshot_id: CatalogSnapshotId,
        source_coverage: Vec<SourceCoverageSet>,
    ) -> Result<(), CatalogContractError> {
        let allowed = matches!(
            self.snapshot.state,
            CatalogReadinessPhase::Building
                | CatalogReadinessPhase::Partial
                | CatalogReadinessPhase::Degraded
        ) || (self.snapshot.state == CatalogReadinessPhase::Ready
            && self.snapshot.refreshing_from_snapshot.is_some());
        if !allowed {
            return Err(CatalogContractError::invalid(
                "catalog readiness can publish only from an active build, recovery, or refresh",
            ));
        }
        if snapshot_id.pack_contract_version != self.snapshot.desired_contract_version
            || snapshot_id.coverage_plan_id != self.plan.coverage_plan_id
            || snapshot_id.readiness_epoch != self.snapshot.epoch
        {
            return Err(CatalogContractError::invalid(
                "published catalog snapshot does not match the active plan, epoch, and contract",
            ));
        }
        if self
            .snapshot
            .last_complete_snapshot
            .is_some_and(|previous| snapshot_id.complete_commit <= previous.complete_commit)
        {
            return Err(CatalogContractError::invalid(
                "a new catalog snapshot must advance the durable commit clock",
            ));
        }
        if !self.plan.required_coverage_complete(&source_coverage) {
            return Err(CatalogContractError::invalid(
                "ready catalog publication requires complete coverage for every required source",
            ));
        }
        let mut candidate = self.snapshot.clone();
        candidate.state = CatalogReadinessPhase::Ready;
        candidate.completed_contract_version = Some(candidate.desired_contract_version);
        candidate.complete_through_commit = Some(snapshot_id.complete_commit);
        candidate.last_complete_snapshot = Some(snapshot_id);
        candidate.refreshing_from_snapshot = None;
        candidate.source_coverage = source_coverage;
        candidate.reason = None;
        self.replace_snapshot(candidate)
    }

    pub(crate) fn begin_refresh(&mut self) -> Result<(), CatalogContractError> {
        if self.snapshot.state != CatalogReadinessPhase::Ready {
            return Err(CatalogContractError::invalid(
                "only a ready catalog snapshot can begin an ordinary refresh",
            ));
        }
        let mut candidate = self.snapshot.clone();
        candidate.refreshing_from_snapshot = candidate.last_complete_snapshot;
        candidate.reason = None;
        self.replace_snapshot(candidate)
    }

    pub(crate) fn source_retrying(
        &mut self,
        reason_code: impl Into<String>,
        source_coverage: Vec<SourceCoverageSet>,
    ) -> Result<(), CatalogContractError> {
        if self.snapshot.state == CatalogReadinessPhase::Error {
            return Err(CatalogContractError::invalid(
                "an error catalog build must enter an explicit retry attempt",
            ));
        }
        let mut candidate = self.snapshot.clone();
        candidate.source_coverage = source_coverage;
        candidate.reason = Some(CatalogReadinessReason::SourceRetrying {
            code: reason_code.into(),
        });
        self.replace_snapshot(candidate)
    }

    pub(crate) fn source_terminally_unavailable(
        &mut self,
        reason_code: impl Into<String>,
        source_coverage: Vec<SourceCoverageSet>,
    ) -> Result<(), CatalogContractError> {
        if !matches!(
            self.snapshot.state,
            CatalogReadinessPhase::Building
                | CatalogReadinessPhase::Partial
                | CatalogReadinessPhase::Ready
        ) {
            return Err(CatalogContractError::invalid(
                "terminal source unavailability requires building, partial, or ready catalog state",
            ));
        }
        let mut candidate = self.snapshot.clone();
        candidate.state = CatalogReadinessPhase::Degraded;
        candidate.refreshing_from_snapshot = None;
        candidate.source_coverage = source_coverage;
        candidate.reason = Some(CatalogReadinessReason::TerminalSourceUnavailable {
            code: reason_code.into(),
        });
        self.replace_snapshot(candidate)
    }

    pub(crate) fn fail_integrity(
        &mut self,
        reason_code: impl Into<String>,
        snapshot_disposition: CatalogIntegritySnapshotDisposition,
    ) -> Result<(), CatalogContractError> {
        if self.snapshot.state == CatalogReadinessPhase::Error {
            return Err(CatalogContractError::invalid(
                "catalog readiness is already in an error state",
            ));
        }
        let mut candidate = self.snapshot.clone();
        candidate.state = CatalogReadinessPhase::Error;
        candidate.refreshing_from_snapshot = None;
        candidate.reason = Some(CatalogReadinessReason::IntegrityFailure {
            code: reason_code.into(),
            snapshot_disposition,
        });
        match snapshot_disposition {
            CatalogIntegritySnapshotDisposition::IndependentlySafe => {
                candidate.complete_through_commit = candidate
                    .last_complete_snapshot
                    .filter(|snapshot| {
                        snapshot.coverage_plan_id == candidate.coverage_plan_id
                            && snapshot.readiness_epoch == candidate.epoch
                    })
                    .map(|snapshot| snapshot.complete_commit);
            }
            CatalogIntegritySnapshotDisposition::Discarded => {
                candidate.completed_contract_version = None;
                candidate.complete_through_commit = None;
                candidate.last_complete_snapshot = None;
            }
        }
        self.replace_snapshot(candidate)
    }

    pub(crate) fn retry(&mut self) -> Result<(), CatalogContractError> {
        if !matches!(
            self.snapshot.state,
            CatalogReadinessPhase::Degraded | CatalogReadinessPhase::Error
        ) {
            return Err(CatalogContractError::invalid(
                "only a degraded or error catalog build can start a retry attempt",
            ));
        }
        let mut candidate = self.snapshot.clone();
        candidate.attempt = candidate
            .attempt
            .checked_add(1)
            .ok_or_else(|| CatalogContractError::invalid("catalog attempt overflow"))?;
        candidate.state = CatalogReadinessPhase::Building;
        candidate.complete_through_commit = None;
        candidate.refreshing_from_snapshot = None;
        candidate.reason = None;
        self.replace_snapshot(candidate)
    }

    pub(crate) fn replace_coverage_plan(
        &mut self,
        new_plan: CatalogCoveragePlan,
    ) -> Result<(), CatalogContractError> {
        new_plan.validate()?;
        if new_plan.scope != self.plan.scope {
            return Err(CatalogContractError::invalid(
                "a catalog coverage-plan lineage cannot change readiness scope",
            ));
        }
        if new_plan.coverage_plan_id == self.plan.coverage_plan_id {
            return Err(CatalogContractError::invalid(
                "catalog coverage-plan replacement must change normalized plan content",
            ));
        }
        let mut candidate = self.snapshot.clone();
        candidate.coverage_plan_id = new_plan.coverage_plan_id;
        candidate.epoch = candidate
            .epoch
            .checked_add(1)
            .ok_or_else(|| CatalogContractError::invalid("catalog epoch overflow"))?;
        candidate.attempt = 1;
        candidate.state = CatalogReadinessPhase::Building;
        candidate.complete_through_commit = None;
        candidate.refreshing_from_snapshot = None;
        candidate.source_coverage.clear();
        candidate.reason = None;
        candidate.validate_against(&new_plan)?;
        self.plan = new_plan;
        self.snapshot = candidate;
        Ok(())
    }

    pub(crate) fn change_contract_version(
        &mut self,
        desired_contract_version: u32,
        prior_snapshot_schema_compatible: bool,
    ) -> Result<(), CatalogContractError> {
        if desired_contract_version == 0
            || desired_contract_version == self.snapshot.desired_contract_version
        {
            return Err(CatalogContractError::invalid(
                "catalog contract change requires a distinct nonzero version",
            ));
        }
        let mut candidate = self.snapshot.clone();
        candidate.desired_contract_version = desired_contract_version;
        candidate.epoch = candidate
            .epoch
            .checked_add(1)
            .ok_or_else(|| CatalogContractError::invalid("catalog epoch overflow"))?;
        candidate.attempt = 1;
        candidate.state = CatalogReadinessPhase::Building;
        candidate.complete_through_commit = None;
        candidate.refreshing_from_snapshot = None;
        candidate.source_coverage.clear();
        candidate.reason = None;
        if !prior_snapshot_schema_compatible {
            candidate.completed_contract_version = None;
            candidate.last_complete_snapshot = None;
        }
        self.replace_snapshot(candidate)
    }

    pub(crate) fn invalidate_source_generation(&mut self) -> Result<(), CatalogContractError> {
        if self.snapshot.state == CatalogReadinessPhase::Error {
            return Err(CatalogContractError::invalid(
                "an errored catalog build must enter retry before source reset",
            ));
        }
        let mut candidate = self.snapshot.clone();
        candidate.epoch = candidate
            .epoch
            .checked_add(1)
            .ok_or_else(|| CatalogContractError::invalid("catalog epoch overflow"))?;
        candidate.attempt = 1;
        candidate.state = CatalogReadinessPhase::Building;
        candidate.complete_through_commit = None;
        candidate.refreshing_from_snapshot = None;
        candidate.source_coverage.clear();
        candidate.reason = None;
        self.replace_snapshot(candidate)
    }

    fn replace_snapshot(
        &mut self,
        candidate: CatalogReadinessSnapshot,
    ) -> Result<(), CatalogContractError> {
        candidate.validate_against(&self.plan)?;
        self.snapshot = candidate;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::adapter::{
        CoverageMembershipRevision, CoverageObjectKey, CoveragePosition, CoveragePositionKind,
        CoverageProvenance, CoverageScope, CoverageStatus, CoverageStreamKey, SourceCoveragePoint,
    };

    const CLAUDE_DECLARATION: &[u8] = b"claude-catalog-declaration-v1";
    const CODEX_DECLARATION: &[u8] = b"codex-catalog-declaration-v1";

    fn source_key(label: &[u8]) -> CanonicalSourceInstanceKey {
        CanonicalSourceInstanceKey::derive(1, label).unwrap()
    }

    fn plan_source(
        adapter_id: &str,
        source_label: &[u8],
        support_release_id: &str,
        declaration: &[u8],
        policy: &[u8],
    ) -> CatalogCoveragePlanSource {
        CatalogCoveragePlanSource::new(
            adapter_id,
            source_key(source_label),
            support_release_id,
            CoverageDeclarationDigest::derive(declaration).unwrap(),
            CatalogAccessPolicyDigest::derive(1, policy).unwrap(),
        )
        .unwrap()
    }

    fn claude_source() -> CatalogCoveragePlanSource {
        plan_source(
            "claude-code",
            b"fixture-device/claude-root",
            "claude-code@fixture-v1",
            CLAUDE_DECLARATION,
            b"local-catalog-view",
        )
    }

    fn codex_source() -> CatalogCoveragePlanSource {
        plan_source(
            "codex",
            b"fixture-device/codex-root",
            "codex@fixture-v1",
            CODEX_DECLARATION,
            b"local-catalog-view",
        )
    }

    fn library_plan() -> CatalogCoveragePlan {
        CatalogCoveragePlan::new(
            CatalogCoverageScope::Library,
            vec![claude_source(), codex_source()],
            Vec::new(),
        )
        .unwrap()
    }

    fn coverage_for(
        source: &CatalogCoveragePlanSource,
        declaration: &[u8],
        version: u32,
        completeness: CoverageSetCompleteness,
        order: u64,
    ) -> SourceCoverageSet {
        let domain = CoverageDomain::ProjectionPack {
            pack: CATALOG_PROJECTION_PACK_ID.to_string(),
            version,
        };
        let (position, status) = match completeness {
            CoverageSetCompleteness::Complete => (
                Some(
                    CoveragePosition::derive(
                        CoveragePositionKind::AppendCursor,
                        &order.to_be_bytes(),
                        Some(order),
                    )
                    .unwrap(),
                ),
                CoverageStatus::CompleteThrough,
            ),
            CoverageSetCompleteness::Partial => (None, CoverageStatus::Partial),
            CoverageSetCompleteness::Unavailable => (
                None,
                CoverageStatus::Unavailable {
                    reason: "fixture_source_unavailable".to_string(),
                },
            ),
        };
        let point = SourceCoveragePoint::new(
            domain.clone(),
            source.adapter_id.clone(),
            source.source_instance_key,
            CoverageStreamKey::derive(&source.adapter_id, b"catalog-stream").unwrap(),
            CoverageObjectKey::derive(&source.adapter_id, b"catalog-object").unwrap(),
            1,
            position,
            status,
            CoverageProvenance::default(),
        )
        .unwrap();
        SourceCoverageSet::new(
            domain,
            CoverageScope {
                adapter_id: source.adapter_id.clone(),
                source_instance_key: source.source_instance_key,
                root_entity_key: None,
                support_release_id: source.support_release_id.clone(),
                source_or_scope_declaration_digest: {
                    assert_eq!(
                        source.catalog_declaration_digest,
                        CoverageDeclarationDigest::derive(declaration).unwrap()
                    );
                    source.coverage_binding_digest()
                },
            },
            CoverageMembershipRevision::derive(
                format!("{}-catalog-membership", source.adapter_id).as_bytes(),
            )
            .unwrap(),
            vec![point],
            Vec::new(),
            Vec::new(),
            completeness,
        )
        .unwrap()
    }

    fn complete_coverage(
        plan: &CatalogCoveragePlan,
        version: u32,
        order: u64,
    ) -> Vec<SourceCoverageSet> {
        vec![
            coverage_for(
                &plan.required_sources[0],
                if plan.required_sources[0].adapter_id == "claude-code" {
                    CLAUDE_DECLARATION
                } else {
                    CODEX_DECLARATION
                },
                version,
                CoverageSetCompleteness::Complete,
                order,
            ),
            coverage_for(
                &plan.required_sources[1],
                if plan.required_sources[1].adapter_id == "claude-code" {
                    CLAUDE_DECLARATION
                } else {
                    CODEX_DECLARATION
                },
                version,
                CoverageSetCompleteness::Complete,
                order,
            ),
        ]
    }

    fn incomplete_coverage(plan: &CatalogCoveragePlan, version: u32) -> Vec<SourceCoverageSet> {
        let source = &plan.required_sources[0];
        vec![coverage_for(
            source,
            if source.adapter_id == "claude-code" {
                CLAUDE_DECLARATION
            } else {
                CODEX_DECLARATION
            },
            version,
            CoverageSetCompleteness::Partial,
            0,
        )]
    }

    fn unavailable_coverage(plan: &CatalogCoveragePlan, version: u32) -> Vec<SourceCoverageSet> {
        plan.required_sources
            .iter()
            .map(|source| {
                coverage_for(
                    source,
                    if source.adapter_id == "claude-code" {
                        CLAUDE_DECLARATION
                    } else {
                        CODEX_DECLARATION
                    },
                    version,
                    CoverageSetCompleteness::Unavailable,
                    0,
                )
            })
            .collect()
    }

    fn snapshot(machine: &CatalogReadinessMachine, commit: u64) -> CatalogSnapshotId {
        CatalogSnapshotId::new(
            machine.snapshot.desired_contract_version,
            machine.plan.coverage_plan_id,
            machine.snapshot.epoch,
            commit,
        )
        .unwrap()
    }

    #[test]
    fn coverage_plan_identity_is_normalized_and_binds_every_semantic_input() {
        let claude = claude_source();
        let codex = codex_source();
        let first = CatalogCoveragePlan::new(
            CatalogCoverageScope::Library,
            vec![claude.clone(), codex.clone()],
            Vec::new(),
        )
        .unwrap();
        let reordered = CatalogCoveragePlan::new(
            CatalogCoverageScope::Library,
            vec![codex.clone(), claude.clone()],
            Vec::new(),
        )
        .unwrap();
        assert_eq!(first, reordered);

        let optionality_changed = CatalogCoveragePlan::new(
            CatalogCoverageScope::Library,
            vec![claude.clone()],
            vec![codex.clone()],
        )
        .unwrap();
        assert_ne!(first.coverage_plan_id, optionality_changed.coverage_plan_id);

        let support_changed = CatalogCoveragePlan::new(
            CatalogCoverageScope::Library,
            vec![
                claude.clone(),
                plan_source(
                    "codex",
                    b"fixture-device/codex-root",
                    "codex@fixture-v2",
                    CODEX_DECLARATION,
                    b"local-catalog-view",
                ),
            ],
            Vec::new(),
        )
        .unwrap();
        assert_ne!(first.coverage_plan_id, support_changed.coverage_plan_id);

        let declaration_changed = CatalogCoveragePlan::new(
            CatalogCoverageScope::Library,
            vec![
                claude.clone(),
                plan_source(
                    "codex",
                    b"fixture-device/codex-root",
                    "codex@fixture-v1",
                    b"codex-catalog-declaration-v2",
                    b"local-catalog-view",
                ),
            ],
            Vec::new(),
        )
        .unwrap();
        assert_ne!(first.coverage_plan_id, declaration_changed.coverage_plan_id);

        let policy_changed = CatalogCoveragePlan::new(
            CatalogCoverageScope::Library,
            vec![
                claude,
                plan_source(
                    "codex",
                    b"fixture-device/codex-root",
                    "codex@fixture-v1",
                    CODEX_DECLARATION,
                    b"remote-withheld-catalog-view",
                ),
            ],
            Vec::new(),
        )
        .unwrap();
        assert_ne!(first.coverage_plan_id, policy_changed.coverage_plan_id);

        let duplicate = CatalogCoveragePlan::new(
            CatalogCoverageScope::Library,
            vec![codex.clone()],
            vec![codex],
        );
        assert!(duplicate.is_err());

        let mut tampered = serde_json::to_value(&first).unwrap();
        tampered["coverage_plan_id"] = json!(policy_changed.coverage_plan_id);
        assert!(serde_json::from_value::<CatalogCoveragePlan>(tampered).is_err());
    }

    #[test]
    fn catalog_cursor_is_bound_to_one_snapshot_query_and_sort() {
        let plan = library_plan();
        let snapshot = CatalogSnapshotId::new(
            CATALOG_QUERY_PACK_CONTRACT_VERSION,
            plan.coverage_plan_id,
            7,
            42,
        )
        .unwrap();
        let fingerprint = CatalogQueryFingerprint::derive(
            CATALOG_QUERY_PACK_CONTRACT_VERSION,
            CatalogQueryKind::Sessions,
            CatalogCoverageScope::Library,
            3,
            br#"{"project":"fixture","availability":"any"}"#,
        )
        .unwrap();
        let entity = CanonicalEntityKey::derive(
            "claude-code",
            &source_key(b"fixture-device/claude-root"),
            "session",
            b"session-1",
        )
        .unwrap();
        let cursor = CatalogCursor::new(
            snapshot,
            fingerprint,
            3,
            CatalogSortKey::new(b"2026-08-17T12:00:00Z".to_vec()).unwrap(),
            entity,
        )
        .unwrap();
        cursor.validate_binding(snapshot, fingerprint, 3).unwrap();

        let newer_snapshot = CatalogSnapshotId::new(
            CATALOG_QUERY_PACK_CONTRACT_VERSION,
            plan.coverage_plan_id,
            7,
            43,
        )
        .unwrap();
        assert!(cursor
            .validate_binding(newer_snapshot, fingerprint, 3)
            .is_err());
        let changed_filter = CatalogQueryFingerprint::derive(
            CATALOG_QUERY_PACK_CONTRACT_VERSION,
            CatalogQueryKind::Sessions,
            CatalogCoverageScope::Library,
            3,
            br#"{"project":"other","availability":"any"}"#,
        )
        .unwrap();
        assert!(cursor
            .validate_binding(snapshot, changed_filter, 3)
            .is_err());
        assert!(cursor.validate_binding(snapshot, fingerprint, 4).is_err());

        let mut invalid = serde_json::to_value(cursor).unwrap();
        invalid["last_sort_key"] = json!("v1:");
        assert!(serde_json::from_value::<CatalogCursor>(invalid).is_err());
    }

    #[test]
    fn readiness_transition_table_retains_truth_across_refresh_loss_and_recovery() {
        let plan = library_plan();
        let mut machine = CatalogReadinessMachine::register(plan, 1).unwrap();
        assert_eq!(machine.snapshot().state, CatalogReadinessPhase::Pending);
        assert_eq!(
            (machine.snapshot().epoch, machine.snapshot().attempt),
            (1, 1)
        );

        machine.schedule_build().unwrap();
        assert_eq!(machine.snapshot().state, CatalogReadinessPhase::Building);

        let partial = incomplete_coverage(machine.plan(), 1);
        machine.record_partial(partial).unwrap();
        assert_eq!(machine.snapshot().state, CatalogReadinessPhase::Partial);

        let first_snapshot = snapshot(&machine, 10);
        machine
            .publish_ready(first_snapshot, complete_coverage(machine.plan(), 1, 10))
            .unwrap();
        assert_eq!(machine.snapshot().state, CatalogReadinessPhase::Ready);
        assert_eq!(machine.snapshot().complete_through_commit, Some(10));

        machine
            .source_retrying(
                "required_source_retrying",
                unavailable_coverage(machine.plan(), 1),
            )
            .unwrap();
        assert_eq!(machine.snapshot().state, CatalogReadinessPhase::Ready);
        assert_eq!(
            machine.snapshot().last_complete_snapshot,
            Some(first_snapshot)
        );
        assert!(matches!(
            machine.snapshot().reason,
            Some(CatalogReadinessReason::SourceRetrying { .. })
        ));

        machine
            .source_terminally_unavailable(
                "required_source_terminal",
                unavailable_coverage(machine.plan(), 1),
            )
            .unwrap();
        assert_eq!(machine.snapshot().state, CatalogReadinessPhase::Degraded);
        assert_eq!(
            machine.snapshot().last_complete_snapshot,
            Some(first_snapshot)
        );

        machine.retry().unwrap();
        assert_eq!(machine.snapshot().state, CatalogReadinessPhase::Building);
        assert_eq!(
            (machine.snapshot().epoch, machine.snapshot().attempt),
            (1, 2)
        );
        assert_eq!(machine.snapshot().complete_through_commit, None);
        assert_eq!(
            machine.snapshot().last_complete_snapshot,
            Some(first_snapshot)
        );

        let recovered_snapshot = snapshot(&machine, 20);
        machine
            .publish_ready(recovered_snapshot, complete_coverage(machine.plan(), 1, 20))
            .unwrap();
        machine.begin_refresh().unwrap();
        assert_eq!(
            machine.snapshot().refreshing_from_snapshot,
            Some(recovered_snapshot)
        );
        let refreshed_snapshot = snapshot(&machine, 21);
        machine
            .publish_ready(refreshed_snapshot, complete_coverage(machine.plan(), 1, 21))
            .unwrap();
        assert_eq!(machine.snapshot().state, CatalogReadinessPhase::Ready);
        assert_eq!(
            machine.snapshot().last_complete_snapshot,
            Some(refreshed_snapshot)
        );
        assert_eq!(machine.snapshot().refreshing_from_snapshot, None);
    }

    #[test]
    fn readiness_lineage_changes_and_failures_preserve_only_explicit_safe_history() {
        let plan = library_plan();
        let mut machine = CatalogReadinessMachine::register(plan, 1).unwrap();
        machine.schedule_build().unwrap();
        let initial_snapshot = snapshot(&machine, 10);
        machine
            .publish_ready(initial_snapshot, complete_coverage(machine.plan(), 1, 10))
            .unwrap();

        let replacement = CatalogCoveragePlan::new(
            CatalogCoverageScope::Library,
            vec![claude_source()],
            vec![codex_source()],
        )
        .unwrap();
        let previous_plan_id = machine.plan().coverage_plan_id;
        machine.replace_coverage_plan(replacement).unwrap();
        assert_eq!(machine.snapshot().state, CatalogReadinessPhase::Building);
        assert_eq!(
            (machine.snapshot().epoch, machine.snapshot().attempt),
            (2, 1)
        );
        assert_ne!(machine.snapshot().coverage_plan_id, previous_plan_id);
        assert_eq!(machine.snapshot().complete_through_commit, None);
        assert_eq!(
            machine.snapshot().last_complete_snapshot,
            Some(initial_snapshot)
        );
        assert_eq!(
            machine
                .snapshot()
                .last_complete_snapshot
                .unwrap()
                .coverage_plan_id,
            previous_plan_id
        );

        machine.change_contract_version(2, true).unwrap();
        assert_eq!(
            (machine.snapshot().epoch, machine.snapshot().attempt),
            (3, 1)
        );
        assert_eq!(machine.snapshot().desired_contract_version, 2);
        assert_eq!(
            machine.snapshot().last_complete_snapshot,
            Some(initial_snapshot)
        );

        machine.invalidate_source_generation().unwrap();
        assert_eq!(
            (machine.snapshot().epoch, machine.snapshot().attempt),
            (4, 1)
        );

        machine
            .fail_integrity(
                "catalog_invariant_failed",
                CatalogIntegritySnapshotDisposition::IndependentlySafe,
            )
            .unwrap();
        assert_eq!(machine.snapshot().state, CatalogReadinessPhase::Error);
        assert_eq!(
            machine.snapshot().last_complete_snapshot,
            Some(initial_snapshot)
        );
        machine.retry().unwrap();
        assert_eq!(
            (machine.snapshot().epoch, machine.snapshot().attempt),
            (4, 2)
        );

        machine
            .fail_integrity(
                "unsafe_catalog_schema",
                CatalogIntegritySnapshotDisposition::Discarded,
            )
            .unwrap();
        assert_eq!(machine.snapshot().last_complete_snapshot, None);
        assert_eq!(machine.snapshot().completed_contract_version, None);
    }

    #[test]
    fn invalid_ready_publication_is_atomic_and_cannot_claim_wrong_coverage() {
        let plan = library_plan();
        let mut machine = CatalogReadinessMachine::register(plan.clone(), 1).unwrap();
        machine.schedule_build().unwrap();
        let before = machine.snapshot().clone();
        let candidate = snapshot(&machine, 10);
        assert!(machine
            .publish_ready(candidate, incomplete_coverage(machine.plan(), 1))
            .is_err());
        assert_eq!(machine.snapshot(), &before);

        let wrong_plan = CatalogCoveragePlan::new(
            CatalogCoverageScope::Library,
            vec![claude_source()],
            Vec::new(),
        )
        .unwrap();
        let wrong_snapshot =
            CatalogSnapshotId::new(1, wrong_plan.coverage_plan_id, machine.snapshot().epoch, 10)
                .unwrap();
        assert!(machine
            .publish_ready(wrong_snapshot, complete_coverage(machine.plan(), 1, 10),)
            .is_err());
        assert_eq!(machine.snapshot(), &before);

        let mut forged_ready = before;
        forged_ready.state = CatalogReadinessPhase::Ready;
        assert!(CatalogReadinessMachine::resume(plan.clone(), forged_ready).is_err());

        let mut valid = CatalogReadinessMachine::register(plan.clone(), 1).unwrap();
        valid.schedule_build().unwrap();
        let valid_snapshot = snapshot(&valid, 10);
        valid
            .publish_ready(valid_snapshot, complete_coverage(valid.plan(), 1, 10))
            .unwrap();
        let mut missing_publication_coverage = valid.snapshot().clone();
        missing_publication_coverage.source_coverage.clear();
        assert!(
            CatalogReadinessMachine::resume(plan.clone(), missing_publication_coverage).is_err()
        );
        let mut missing_retry_coverage = valid.snapshot().clone();
        missing_retry_coverage.source_coverage.clear();
        missing_retry_coverage.reason = Some(CatalogReadinessReason::SourceRetrying {
            code: "required_source_retrying".to_string(),
        });
        assert!(CatalogReadinessMachine::resume(plan, missing_retry_coverage).is_err());
    }

    #[test]
    fn readiness_resume_rejects_empty_partial_future_lineage_and_false_current_commit() {
        let plan = library_plan();
        let mut building = CatalogReadinessMachine::register(plan.clone(), 1).unwrap();
        building.schedule_build().unwrap();

        let mut empty_partial = building.snapshot().clone();
        empty_partial.state = CatalogReadinessPhase::Partial;
        assert!(CatalogReadinessMachine::resume(plan.clone(), empty_partial).is_err());

        let optional_plan = CatalogCoveragePlan::new(
            CatalogCoverageScope::Library,
            vec![claude_source()],
            vec![codex_source()],
        )
        .unwrap();
        let mut optional_only =
            CatalogReadinessMachine::register(optional_plan.clone(), 1).unwrap();
        optional_only.schedule_build().unwrap();
        let mut optional_partial = optional_only.snapshot().clone();
        optional_partial.state = CatalogReadinessPhase::Partial;
        optional_partial.source_coverage = vec![coverage_for(
            &optional_plan.optional_sources[0],
            CODEX_DECLARATION,
            1,
            CoverageSetCompleteness::Partial,
            0,
        )];
        assert!(CatalogReadinessMachine::resume(optional_plan, optional_partial).is_err());

        let mut valid = CatalogReadinessMachine::register(plan.clone(), 1).unwrap();
        valid.schedule_build().unwrap();
        let complete = snapshot(&valid, 10);
        valid
            .publish_ready(complete, complete_coverage(valid.plan(), 1, 10))
            .unwrap();

        let mut future_lineage = valid.snapshot().clone();
        future_lineage.state = CatalogReadinessPhase::Building;
        future_lineage.complete_through_commit = None;
        future_lineage.last_complete_snapshot = Some(
            CatalogSnapshotId::new(1, plan.coverage_plan_id, future_lineage.epoch + 1, 10).unwrap(),
        );
        assert!(CatalogReadinessMachine::resume(plan.clone(), future_lineage).is_err());

        let mut false_current_commit = valid.snapshot().clone();
        false_current_commit.state = CatalogReadinessPhase::Building;
        assert!(CatalogReadinessMachine::resume(plan, false_current_commit).is_err());
    }

    #[test]
    fn readiness_resume_binds_integrity_disposition_to_retained_snapshot_state() {
        let plan = library_plan();
        let mut safe = CatalogReadinessMachine::register(plan.clone(), 1).unwrap();
        safe.schedule_build().unwrap();
        let complete = snapshot(&safe, 10);
        safe.publish_ready(complete, complete_coverage(safe.plan(), 1, 10))
            .unwrap();
        safe.fail_integrity(
            "catalog_invariant_failed",
            CatalogIntegritySnapshotDisposition::IndependentlySafe,
        )
        .unwrap();
        assert_eq!(safe.snapshot().complete_through_commit, Some(10));
        assert!(CatalogReadinessMachine::resume(plan.clone(), safe.snapshot().clone()).is_ok());

        let mut forged_discarded = safe.snapshot().clone();
        forged_discarded.reason = Some(CatalogReadinessReason::IntegrityFailure {
            code: "catalog_invariant_failed".to_owned(),
            snapshot_disposition: CatalogIntegritySnapshotDisposition::Discarded,
        });
        assert!(CatalogReadinessMachine::resume(plan.clone(), forged_discarded).is_err());

        let mut discarded = CatalogReadinessMachine::register(plan.clone(), 1).unwrap();
        discarded.schedule_build().unwrap();
        let complete = snapshot(&discarded, 20);
        discarded
            .publish_ready(complete, complete_coverage(discarded.plan(), 1, 20))
            .unwrap();
        discarded
            .fail_integrity(
                "unsafe_catalog_schema",
                CatalogIntegritySnapshotDisposition::Discarded,
            )
            .unwrap();
        assert_eq!(discarded.snapshot().complete_through_commit, None);
        assert_eq!(discarded.snapshot().last_complete_snapshot, None);

        let mut forged_safe = discarded.snapshot().clone();
        forged_safe.reason = Some(CatalogReadinessReason::IntegrityFailure {
            code: "unsafe_catalog_schema".to_owned(),
            snapshot_disposition: CatalogIntegritySnapshotDisposition::IndependentlySafe,
        });
        assert!(CatalogReadinessMachine::resume(plan, forged_safe).is_err());
    }

    #[test]
    fn catalog_core_fixture_is_stable() {
        let initial_plan = library_plan();
        let initial_snapshot = CatalogSnapshotId::new(
            CATALOG_QUERY_PACK_CONTRACT_VERSION,
            initial_plan.coverage_plan_id,
            1,
            10,
        )
        .unwrap();
        let fingerprint = CatalogQueryFingerprint::derive(
            CATALOG_QUERY_PACK_CONTRACT_VERSION,
            CatalogQueryKind::Sessions,
            CatalogCoverageScope::Library,
            1,
            br#"{"availability":"any"}"#,
        )
        .unwrap();
        let entity = CanonicalEntityKey::derive(
            "claude-code",
            &source_key(b"fixture-device/claude-root"),
            "session",
            b"session-1",
        )
        .unwrap();
        let cursor = CatalogCursor::new(
            initial_snapshot,
            fingerprint,
            1,
            CatalogSortKey::new(b"2026-08-17T12:00:00Z".to_vec()).unwrap(),
            entity,
        )
        .unwrap();

        let mut readiness = CatalogReadinessMachine::register(initial_plan.clone(), 1).unwrap();
        readiness.schedule_build().unwrap();
        readiness
            .publish_ready(initial_snapshot, complete_coverage(readiness.plan(), 1, 10))
            .unwrap();
        let changed_plan = CatalogCoveragePlan::new(
            CatalogCoverageScope::Library,
            vec![claude_source()],
            vec![codex_source()],
        )
        .unwrap();
        readiness
            .replace_coverage_plan(changed_plan.clone())
            .unwrap();

        let fixture = json!({
            "fixture_contract_version": 1,
            "initial_coverage_plan": initial_plan,
            "changed_coverage_plan": changed_plan,
            "complete_snapshot_id": initial_snapshot,
            "session_cursor": cursor,
            "readiness_after_plan_change": readiness.snapshot(),
            "expected": {
                "registration_order_independent": true,
                "continuation_bound_to_snapshot": true,
                "prior_plan_snapshot_is_not_current": true,
                "plan_change_epoch": 2,
                "plan_change_attempt": 1,
                "plan_change_state": "building"
            }
        });
        let expected = serde_json::from_str::<serde_json::Value>(include_str!(
            "../fixtures/contracts/rfc012b-catalog-core-v1.json"
        ))
        .unwrap();
        if fixture != expected {
            eprintln!("{}", serde_json::to_string_pretty(&fixture).unwrap());
        }
        assert_eq!(fixture, expected);
    }
}
