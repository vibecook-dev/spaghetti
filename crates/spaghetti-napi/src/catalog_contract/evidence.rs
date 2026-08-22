//! RFC 012B catalog assertions, reduction, and external-reference lifecycle.
//!
//! These contracts model native evidence without turning presentation aliases,
//! inferred paths, or query rows into a second base identity. The module is
//! crate-private and persistence-free; B2 source compositions will emit these
//! values and B3 will persist their reducer effects transactionally.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::{self, Write};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::{
    contract_digest, deserialize_digest, serialize_digest, CatalogContractError, DIGEST_BYTES,
    MAX_IDENTIFIER_BYTES, MAX_REASON_CODE_BYTES, REFERENCE_ENCODING_VERSION,
};
use crate::adapter::{
    CanonicalEntityKey, CanonicalSourceInstanceKey, ContractCompleteness, CoverageObjectKey,
    CoverageStreamKey, ExternalEntityRef, NativeIdentity, QualifiedUnknownReason, QualifiedValue,
    QualifiedValueQuality, SemanticRevisionRef, EXTERNAL_ENTITY_REFERENCE_VERSION,
    SEMANTIC_REFERENCE_CONTRACT_VERSION,
};

const MAX_PROVENANCE_REVISIONS: usize = 64;
const MAX_PRESENTATION_MEMBERS: usize = 4_096;
const MAX_REPLACEMENT_RELATIONS: usize = 4_096;
const MAX_PUBLICATION_REDUCER_ENTRIES: usize = 1_000_000;
const MAX_PUBLICATION_ROWS: usize = 1_000_000;
const MAX_CATALOG_LOCATOR_VALUE_BYTES: usize = 8 * 1024;

struct BoundedJsonWriter {
    bytes: Vec<u8>,
    max_bytes: usize,
}

impl BoundedJsonWriter {
    fn new(max_bytes: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(max_bytes.min(64 * 1024)),
            max_bytes,
        }
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

impl Write for BoundedJsonWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let next_len = self
            .bytes
            .len()
            .checked_add(bytes.len())
            .ok_or_else(|| io::Error::other("bounded JSON byte count overflow"))?;
        if next_len > self.max_bytes {
            return Err(io::Error::other("bounded JSON byte ceiling exceeded"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(super) fn serialize_private_json_bounded<T: Serialize + ?Sized>(
    value: &T,
    max_bytes: usize,
    label: &'static str,
) -> Result<Vec<u8>, CatalogContractError> {
    if max_bytes == 0 {
        return Err(CatalogContractError::invalid(format!(
            "{label} has no remaining durable byte budget"
        )));
    }
    let mut writer = BoundedJsonWriter::new(max_bytes);
    serde_json::to_writer(&mut writer, value).map_err(|error| {
        CatalogContractError::invalid(format!(
            "{label} could not be encoded within its durable byte budget: {error}"
        ))
    })?;
    Ok(writer.finish())
}

opaque_digest_type!(CatalogAssertionKey);
opaque_digest_type!(CatalogAssociationKey);
opaque_digest_type!(CatalogLocatorClaimKey);
opaque_digest_type!(CatalogIdentityRelationKey);

fn validate_identifier(label: &str, value: &str) -> Result<(), CatalogContractError> {
    if value.is_empty() || value.trim() != value || value.len() > MAX_IDENTIFIER_BYTES {
        return Err(CatalogContractError::invalid(format!(
            "{label} must be canonical and at most {MAX_IDENTIFIER_BYTES} bytes"
        )));
    }
    Ok(())
}

fn validate_optional_text(label: &str, value: &str) -> Result<(), CatalogContractError> {
    if value.is_empty() || value.trim() != value {
        return Err(CatalogContractError::invalid(format!(
            "{label} must not encode absence as empty or padded text"
        )));
    }
    if value.len() > MAX_REASON_CODE_BYTES * 16 {
        return Err(CatalogContractError::invalid(format!(
            "{label} exceeds the bounded catalog value size"
        )));
    }
    Ok(())
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
    let mut unique = BTreeSet::new();
    for reference in provenance {
        if reference.semantic_reference_contract_version != SEMANTIC_REFERENCE_CONTRACT_VERSION {
            return Err(CatalogContractError::invalid(format!(
                "{label} contains an incompatible semantic revision reference"
            )));
        }
        if reference
            .fact_revision_id
            .as_bytes()
            .iter()
            .all(|byte| *byte == 0)
        {
            return Err(CatalogContractError::invalid(format!(
                "{label} contains a zero semantic revision reference"
            )));
        }
        if !unique.insert(reference.fact_revision_id) {
            return Err(CatalogContractError::invalid(format!(
                "{label} contains duplicate semantic revision provenance"
            )));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CatalogEntityKind {
    Project,
    Session,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct CatalogEntityRef {
    pub kind: CatalogEntityKind,
    pub external_ref: ExternalEntityRef,
}

impl<'de> Deserialize<'de> for CatalogEntityRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            kind: CatalogEntityKind,
            external_ref: ExternalEntityRef,
        }

        let wire = Wire::deserialize(deserializer)?;
        let value = Self {
            kind: wire.kind,
            external_ref: wire.external_ref,
        };
        value.validate().map_err(D::Error::custom)?;
        Ok(value)
    }
}

impl PartialOrd for CatalogEntityRef {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for CatalogEntityRef {
    fn cmp(&self, other: &Self) -> Ordering {
        self.kind
            .cmp(&other.kind)
            .then_with(|| {
                self.external_ref
                    .external_entity_reference_version
                    .cmp(&other.external_ref.external_entity_reference_version)
            })
            .then_with(|| {
                self.external_ref
                    .entity_key
                    .cmp(&other.external_ref.entity_key)
            })
    }
}

impl CatalogEntityRef {
    pub(crate) fn project(entity_key: CanonicalEntityKey) -> Self {
        Self {
            kind: CatalogEntityKind::Project,
            external_ref: ExternalEntityRef::new(entity_key),
        }
    }

    pub(crate) fn session(entity_key: CanonicalEntityKey) -> Self {
        Self {
            kind: CatalogEntityKind::Session,
            external_ref: ExternalEntityRef::new(entity_key),
        }
    }

    pub(super) fn validate(self) -> Result<(), CatalogContractError> {
        if self.external_ref.external_entity_reference_version != EXTERNAL_ENTITY_REFERENCE_VERSION
        {
            return Err(CatalogContractError::invalid(
                "catalog entity uses an incompatible external-reference version",
            ));
        }
        if self
            .external_ref
            .entity_key
            .as_bytes()
            .iter()
            .all(|byte| *byte == 0)
        {
            return Err(CatalogContractError::invalid(
                "catalog entity reference requires a nonzero entity key",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct CatalogEvidenceOwner {
    pub adapter_id: String,
    pub source_instance_key: CanonicalSourceInstanceKey,
    pub stream_key: CoverageStreamKey,
    pub object_key: CoverageObjectKey,
    pub generation: u64,
}

impl CatalogEvidenceOwner {
    pub(crate) fn new(
        adapter_id: impl Into<String>,
        source_instance_key: CanonicalSourceInstanceKey,
        stream_key: CoverageStreamKey,
        object_key: CoverageObjectKey,
        generation: u64,
    ) -> Result<Self, CatalogContractError> {
        let value = Self {
            adapter_id: adapter_id.into(),
            source_instance_key,
            stream_key,
            object_key,
            generation,
        };
        value.validate()?;
        Ok(value)
    }

    fn validate(&self) -> Result<(), CatalogContractError> {
        validate_identifier("catalog evidence adapter id", &self.adapter_id)?;
        if self.generation == 0 {
            return Err(CatalogContractError::invalid(
                "catalog evidence generation must be greater than zero",
            ));
        }
        Ok(())
    }

    fn encode(&self) -> Vec<u8> {
        let mut encoded = Vec::new();
        encode_bytes(&mut encoded, self.adapter_id.as_bytes());
        encoded.extend_from_slice(self.source_instance_key.as_bytes());
        encoded.extend_from_slice(self.stream_key.as_bytes());
        encoded.extend_from_slice(self.object_key.as_bytes());
        encoded.extend_from_slice(&self.generation.to_be_bytes());
        encoded
    }
}

fn encode_bytes(encoded: &mut Vec<u8>, value: &[u8]) {
    encoded.extend_from_slice(&(value.len() as u64).to_be_bytes());
    encoded.extend_from_slice(value);
}

fn derive_evidence_key(
    domain: &[u8],
    owner: &CatalogEvidenceOwner,
    stable_native_key: &[u8],
) -> Result<[u8; 32], CatalogContractError> {
    owner.validate()?;
    if stable_native_key.is_empty() || stable_native_key.len() > 64 * 1024 {
        return Err(CatalogContractError::invalid(
            "catalog evidence key must contain 1..=65536 bytes",
        ));
    }
    Ok(contract_digest(
        domain,
        &[&owner.encode(), stable_native_key],
    ))
}

impl CatalogAssertionKey {
    pub(super) fn publication_bytes(&self) -> &[u8; DIGEST_BYTES] {
        self.as_bytes()
    }

    fn derive(
        owner: &CatalogEvidenceOwner,
        entity_kind: CatalogEntityKind,
        stable_native_key: &[u8],
    ) -> Result<Self, CatalogContractError> {
        let kind = match entity_kind {
            CatalogEntityKind::Project => [1],
            CatalogEntityKind::Session => [2],
        };
        Ok(Self::from_digest(contract_digest(
            b"catalog-assertion-key",
            &[
                &owner.encode(),
                &kind,
                &derive_evidence_key(b"catalog-native-assertion", owner, stable_native_key)?,
            ],
        )))
    }
}

impl CatalogAssociationKey {
    fn derive(
        owner: &CatalogEvidenceOwner,
        stable_native_key: &[u8],
    ) -> Result<Self, CatalogContractError> {
        derive_evidence_key(b"catalog-association-key", owner, stable_native_key)
            .map(Self::from_digest)
    }
}

impl CatalogLocatorClaimKey {
    fn derive(
        owner: &CatalogEvidenceOwner,
        stable_native_key: &[u8],
    ) -> Result<Self, CatalogContractError> {
        derive_evidence_key(b"catalog-locator-claim-key", owner, stable_native_key)
            .map(Self::from_digest)
    }
}

impl CatalogIdentityRelationKey {
    fn derive(
        owner: &CatalogEvidenceOwner,
        stable_native_key: &[u8],
    ) -> Result<Self, CatalogContractError> {
        derive_evidence_key(b"catalog-identity-relation-key", owner, stable_native_key)
            .map(Self::from_digest)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CatalogFieldAuthority {
    pub class_id: String,
    pub precedence: u16,
    pub native_times_comparable: bool,
}

impl CatalogFieldAuthority {
    pub(crate) fn new(
        class_id: impl Into<String>,
        precedence: u16,
        native_times_comparable: bool,
    ) -> Result<Self, CatalogContractError> {
        let value = Self {
            class_id: class_id.into(),
            precedence,
            native_times_comparable,
        };
        value.validate()?;
        Ok(value)
    }

    fn validate(&self) -> Result<(), CatalogContractError> {
        validate_identifier("catalog field authority class", &self.class_id)?;
        if self.precedence == 0 {
            return Err(CatalogContractError::invalid(
                "catalog authority precedence must be greater than zero",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CatalogDisclosureClass {
    Public,
    LocalSensitive,
    PolicyShareable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CatalogPolicyView {
    pub disclose_local_sensitive: bool,
    pub disclose_policy_shareable: bool,
}

impl CatalogPolicyView {
    pub(crate) const LOCAL: Self = Self {
        disclose_local_sensitive: true,
        disclose_policy_shareable: true,
    };

    pub(crate) const WITHHELD: Self = Self {
        disclose_local_sensitive: false,
        disclose_policy_shareable: false,
    };

    fn permits(self, disclosure: CatalogDisclosureClass) -> bool {
        match disclosure {
            CatalogDisclosureClass::Public => true,
            CatalogDisclosureClass::LocalSensitive => self.disclose_local_sensitive,
            CatalogDisclosureClass::PolicyShareable => self.disclose_policy_shareable,
        }
    }
}

pub(crate) type CatalogQualifiedValue<T> =
    QualifiedValue<T, CatalogFieldAuthority, Vec<SemanticRevisionRef>>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CatalogQualifiedField<T> {
    pub qualified: CatalogQualifiedValue<T>,
    pub disclosure: CatalogDisclosureClass,
}

impl<T: Clone> CatalogQualifiedField<T> {
    pub(crate) fn for_view(&self, view: CatalogPolicyView) -> CatalogQualifiedValue<T> {
        if view.permits(self.disclosure) {
            return self.qualified.clone();
        }
        QualifiedValue::from_parts(
            None,
            QualifiedValueQuality::Unknown,
            self.qualified.authority.clone(),
            self.qualified.completeness,
            Some(QualifiedUnknownReason::Withheld),
            self.qualified.effective_at,
            self.qualified.provenance.clone(),
        )
        .expect("withheld catalog value is structurally valid")
    }
}

impl<T> CatalogQualifiedField<T> {
    pub(crate) fn new(
        qualified: CatalogQualifiedValue<T>,
        disclosure: CatalogDisclosureClass,
    ) -> Result<Self, CatalogContractError> {
        let value = Self {
            qualified,
            disclosure,
        };
        value.validate()?;
        Ok(value)
    }

    fn validate(&self) -> Result<(), CatalogContractError> {
        self.qualified.authority.validate()?;
        validate_provenance("catalog qualified value", &self.qualified.provenance)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum CatalogAvailability {
    MetadataOnly,
    TranscriptDiscovered,
    Hydrating,
    HistoryReady,
    Unavailable { reason: String },
}

impl CatalogAvailability {
    fn validate(&self) -> Result<(), CatalogContractError> {
        if let Self::Unavailable { reason } = self {
            validate_identifier("catalog availability reason", reason)?;
        }
        Ok(())
    }
}

fn validate_string_field(
    label: &str,
    field: &CatalogQualifiedField<String>,
) -> Result<(), CatalogContractError> {
    field.validate()?;
    if let Some(value) = &field.qualified.value {
        validate_optional_text(label, value)?;
    }
    Ok(())
}

fn validate_native_identity_field(
    field: &CatalogQualifiedField<NativeIdentity>,
) -> Result<(), CatalogContractError> {
    field.validate()?;
    if field.disclosure == CatalogDisclosureClass::Public {
        return Err(CatalogContractError::invalid(
            "native catalog identities cannot be unconditionally public",
        ));
    }
    if let Some(identity) = &field.qualified.value {
        validate_identifier(
            "catalog native identity namespace",
            &identity.native_namespace,
        )?;
        validate_optional_text("catalog native identity", &identity.native_id)?;
    }
    Ok(())
}

fn validate_availability_field(
    field: &CatalogQualifiedField<CatalogAvailability>,
) -> Result<(), CatalogContractError> {
    field.validate()?;
    let Some(availability) = &field.qualified.value else {
        return Err(CatalogContractError::invalid(
            "catalog membership assertion requires known availability",
        ));
    };
    availability.validate()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CatalogProjectAssertion {
    pub assertion_key: CatalogAssertionKey,
    pub owner: CatalogEvidenceOwner,
    pub project_ref: CatalogEntityRef,
    pub native_identity: Option<CatalogQualifiedField<NativeIdentity>>,
    pub root_identity: Option<CatalogQualifiedField<String>>,
    pub display_path: Option<CatalogQualifiedField<String>>,
    pub display_name: Option<CatalogQualifiedField<String>>,
    pub native_time: Option<CatalogQualifiedField<i64>>,
    pub availability: CatalogQualifiedField<CatalogAvailability>,
    pub provenance: Vec<SemanticRevisionRef>,
}

impl CatalogProjectAssertion {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        owner: CatalogEvidenceOwner,
        stable_native_assertion_key: &[u8],
        project_ref: CatalogEntityRef,
        native_identity: Option<CatalogQualifiedField<NativeIdentity>>,
        root_identity: Option<CatalogQualifiedField<String>>,
        display_path: Option<CatalogQualifiedField<String>>,
        display_name: Option<CatalogQualifiedField<String>>,
        native_time: Option<CatalogQualifiedField<i64>>,
        availability: CatalogQualifiedField<CatalogAvailability>,
        provenance: Vec<SemanticRevisionRef>,
    ) -> Result<Self, CatalogContractError> {
        let value = Self {
            assertion_key: CatalogAssertionKey::derive(
                &owner,
                CatalogEntityKind::Project,
                stable_native_assertion_key,
            )?,
            owner,
            project_ref,
            native_identity,
            root_identity,
            display_path,
            display_name,
            native_time,
            availability,
            provenance,
        };
        value.validate()?;
        Ok(value)
    }

    fn validate(&self) -> Result<(), CatalogContractError> {
        self.owner.validate()?;
        self.project_ref.validate()?;
        if self.project_ref.kind != CatalogEntityKind::Project {
            return Err(CatalogContractError::invalid(
                "catalog project assertion requires a base project reference",
            ));
        }
        if let Some(field) = &self.native_identity {
            validate_native_identity_field(field)?;
        }
        for (label, field) in [
            ("catalog project root identity", &self.root_identity),
            ("catalog project display path", &self.display_path),
            ("catalog project display name", &self.display_name),
        ] {
            if let Some(field) = field {
                validate_string_field(label, field)?;
            }
        }
        if let Some(field) = &self.native_time {
            field.validate()?;
        }
        validate_availability_field(&self.availability)?;
        validate_provenance("catalog project assertion", &self.provenance)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CatalogSessionAssertion {
    pub assertion_key: CatalogAssertionKey,
    pub owner: CatalogEvidenceOwner,
    pub session_ref: CatalogEntityRef,
    pub native_identity: Option<CatalogQualifiedField<NativeIdentity>>,
    pub title: Option<CatalogQualifiedField<String>>,
    pub first_user_summary: Option<CatalogQualifiedField<String>>,
    pub native_created_at: Option<CatalogQualifiedField<i64>>,
    pub native_updated_at: Option<CatalogQualifiedField<i64>>,
    pub native_message_count: Option<CatalogQualifiedField<u64>>,
    pub transcript_locator_claim: Option<CatalogLocatorClaimKey>,
    pub availability: CatalogQualifiedField<CatalogAvailability>,
    pub provenance: Vec<SemanticRevisionRef>,
}

impl CatalogSessionAssertion {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        owner: CatalogEvidenceOwner,
        stable_native_assertion_key: &[u8],
        session_ref: CatalogEntityRef,
        native_identity: Option<CatalogQualifiedField<NativeIdentity>>,
        title: Option<CatalogQualifiedField<String>>,
        first_user_summary: Option<CatalogQualifiedField<String>>,
        native_created_at: Option<CatalogQualifiedField<i64>>,
        native_updated_at: Option<CatalogQualifiedField<i64>>,
        native_message_count: Option<CatalogQualifiedField<u64>>,
        transcript_locator_claim: Option<CatalogLocatorClaimKey>,
        availability: CatalogQualifiedField<CatalogAvailability>,
        provenance: Vec<SemanticRevisionRef>,
    ) -> Result<Self, CatalogContractError> {
        let value = Self {
            assertion_key: CatalogAssertionKey::derive(
                &owner,
                CatalogEntityKind::Session,
                stable_native_assertion_key,
            )?,
            owner,
            session_ref,
            native_identity,
            title,
            first_user_summary,
            native_created_at,
            native_updated_at,
            native_message_count,
            transcript_locator_claim,
            availability,
            provenance,
        };
        value.validate()?;
        Ok(value)
    }

    fn validate(&self) -> Result<(), CatalogContractError> {
        self.owner.validate()?;
        self.session_ref.validate()?;
        if self.session_ref.kind != CatalogEntityKind::Session {
            return Err(CatalogContractError::invalid(
                "catalog session assertion requires a base session reference",
            ));
        }
        if let Some(field) = &self.native_identity {
            validate_native_identity_field(field)?;
        }
        if let Some(field) = &self.title {
            validate_string_field("catalog session title", field)?;
        }
        if let Some(field) = &self.first_user_summary {
            validate_string_field("catalog first-user summary", field)?;
        }
        for field in [&self.native_created_at, &self.native_updated_at]
            .into_iter()
            .flatten()
        {
            field.validate()?;
        }
        if let Some(field) = &self.native_message_count {
            field.validate()?;
        }
        validate_availability_field(&self.availability)?;
        validate_provenance("catalog session assertion", &self.provenance)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ProjectAssociationBasis {
    NativeProjectIndex,
    TranscriptCwd,
    SessionDirectory,
    RolloutHeader,
    DeclaredDerivedAncestor,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SessionProjectAssociationFact {
    pub association_key: CatalogAssociationKey,
    pub owner: CatalogEvidenceOwner,
    pub session_ref: CatalogEntityRef,
    pub project_ref: CatalogEntityRef,
    pub basis: ProjectAssociationBasis,
    pub declared_derivation_id: Option<String>,
    pub locator_claim_key: Option<CatalogLocatorClaimKey>,
    pub authority: CatalogFieldAuthority,
    pub quality: QualifiedValueQuality,
    pub completeness: ContractCompleteness,
    pub effective_at: Option<i64>,
    pub provenance: Vec<SemanticRevisionRef>,
}

impl SessionProjectAssociationFact {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        owner: CatalogEvidenceOwner,
        stable_native_association_key: &[u8],
        session_ref: CatalogEntityRef,
        project_ref: CatalogEntityRef,
        basis: ProjectAssociationBasis,
        declared_derivation_id: Option<String>,
        locator_claim_key: Option<CatalogLocatorClaimKey>,
        authority: CatalogFieldAuthority,
        quality: QualifiedValueQuality,
        completeness: ContractCompleteness,
        effective_at: Option<i64>,
        provenance: Vec<SemanticRevisionRef>,
    ) -> Result<Self, CatalogContractError> {
        let value = Self {
            association_key: CatalogAssociationKey::derive(&owner, stable_native_association_key)?,
            owner,
            session_ref,
            project_ref,
            basis,
            declared_derivation_id,
            locator_claim_key,
            authority,
            quality,
            completeness,
            effective_at,
            provenance,
        };
        value.validate()?;
        Ok(value)
    }

    fn validate(&self) -> Result<(), CatalogContractError> {
        self.owner.validate()?;
        if self
            .association_key
            .as_bytes()
            .iter()
            .all(|byte| *byte == 0)
            || self
                .locator_claim_key
                .is_some_and(|key| key.as_bytes().iter().all(|byte| *byte == 0))
        {
            return Err(CatalogContractError::invalid(
                "catalog association evidence keys must be nonzero",
            ));
        }
        self.session_ref.validate()?;
        self.project_ref.validate()?;
        if self.session_ref.kind != CatalogEntityKind::Session
            || self.project_ref.kind != CatalogEntityKind::Project
        {
            return Err(CatalogContractError::invalid(
                "catalog association must relate one base session to one base project",
            ));
        }
        match (self.basis, &self.declared_derivation_id) {
            (ProjectAssociationBasis::DeclaredDerivedAncestor, Some(identifier)) => {
                validate_identifier("declared catalog derivation id", identifier)?;
            }
            (ProjectAssociationBasis::DeclaredDerivedAncestor, None) => {
                return Err(CatalogContractError::invalid(
                    "derived-ancestor association requires an ADS-declared derivation id",
                ));
            }
            (_, None) => {}
            (_, Some(_)) => {
                return Err(CatalogContractError::invalid(
                    "only declared-derived-ancestor evidence may carry a derivation id",
                ));
            }
        }
        self.authority.validate()?;
        if self.quality == QualifiedValueQuality::Unknown {
            return Err(CatalogContractError::invalid(
                "unknown evidence cannot assert a catalog project association",
            ));
        }
        validate_provenance("catalog project association", &self.provenance)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CatalogLocatorKind {
    Filesystem,
    NativeIndex,
    Repository,
    OpaqueNative,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CatalogLocatorValue {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native_value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canonical_local_path: Option<String>,
}

impl CatalogLocatorValue {
    fn validate(&self) -> Result<(), CatalogContractError> {
        if self.native_value.is_none() && self.canonical_local_path.is_none() {
            return Err(CatalogContractError::invalid(
                "catalog locator requires a native value or canonical local path",
            ));
        }
        if let Some(value) = &self.native_value {
            validate_locator_text("catalog native locator", value)?;
        }
        if let Some(value) = &self.canonical_local_path {
            validate_locator_text("catalog canonical local path", value)?;
        }
        Ok(())
    }
}

fn validate_locator_text(label: &str, value: &str) -> Result<(), CatalogContractError> {
    if value.is_empty() || value.trim() != value || value.len() > MAX_CATALOG_LOCATOR_VALUE_BYTES {
        return Err(CatalogContractError::invalid(format!(
            "{label} must be canonical and at most {MAX_CATALOG_LOCATOR_VALUE_BYTES} bytes"
        )));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeLocatorClaim {
    pub locator_claim_key: CatalogLocatorClaimKey,
    pub owner: CatalogEvidenceOwner,
    pub subject_ref: CatalogEntityRef,
    pub kind: CatalogLocatorKind,
    pub locator: CatalogQualifiedField<CatalogLocatorValue>,
    pub basis: ProjectAssociationBasis,
    pub provenance: Vec<SemanticRevisionRef>,
}

impl NativeLocatorClaim {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        owner: CatalogEvidenceOwner,
        stable_native_locator_key: &[u8],
        subject_ref: CatalogEntityRef,
        kind: CatalogLocatorKind,
        locator: CatalogQualifiedField<CatalogLocatorValue>,
        basis: ProjectAssociationBasis,
        provenance: Vec<SemanticRevisionRef>,
    ) -> Result<Self, CatalogContractError> {
        let value = Self {
            locator_claim_key: CatalogLocatorClaimKey::derive(&owner, stable_native_locator_key)?,
            owner,
            subject_ref,
            kind,
            locator,
            basis,
            provenance,
        };
        value.validate()?;
        Ok(value)
    }

    fn validate(&self) -> Result<(), CatalogContractError> {
        self.owner.validate()?;
        self.subject_ref.validate()?;
        self.locator.validate()?;
        if let Some(locator) = &self.locator.qualified.value {
            locator.validate()?;
        }
        if self.locator.disclosure == CatalogDisclosureClass::Public {
            return Err(CatalogContractError::invalid(
                "native catalog locators cannot be unconditionally public",
            ));
        }
        validate_provenance("catalog locator claim", &self.provenance)
    }

    pub(crate) fn for_view(
        &self,
        view: CatalogPolicyView,
    ) -> CatalogQualifiedValue<CatalogLocatorValue> {
        self.locator.for_view(view)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum IdentityRelationKind {
    Alias,
    SameEntity,
    Supersedes,
    ReplacedBy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct IdentityRelationFact {
    pub relation_key: CatalogIdentityRelationKey,
    pub owner: CatalogEvidenceOwner,
    pub relation: IdentityRelationKind,
    pub left_ref: CatalogEntityRef,
    pub right_ref: CatalogEntityRef,
    pub authority: CatalogFieldAuthority,
    pub quality: QualifiedValueQuality,
    pub completeness: ContractCompleteness,
    pub canonical_winner: Option<CatalogEntityRef>,
    pub collision_policy_id: Option<String>,
    pub provenance: Vec<SemanticRevisionRef>,
}

impl IdentityRelationFact {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        owner: CatalogEvidenceOwner,
        stable_native_relation_key: &[u8],
        relation: IdentityRelationKind,
        left_ref: CatalogEntityRef,
        right_ref: CatalogEntityRef,
        authority: CatalogFieldAuthority,
        quality: QualifiedValueQuality,
        completeness: ContractCompleteness,
        canonical_winner: Option<CatalogEntityRef>,
        collision_policy_id: Option<String>,
        provenance: Vec<SemanticRevisionRef>,
    ) -> Result<Self, CatalogContractError> {
        let value = Self {
            relation_key: CatalogIdentityRelationKey::derive(&owner, stable_native_relation_key)?,
            owner,
            relation,
            left_ref,
            right_ref,
            authority,
            quality,
            completeness,
            canonical_winner,
            collision_policy_id,
            provenance,
        };
        value.validate()?;
        Ok(value)
    }

    fn validate(&self) -> Result<(), CatalogContractError> {
        self.owner.validate()?;
        self.left_ref.validate()?;
        self.right_ref.validate()?;
        if self.left_ref == self.right_ref || self.left_ref.kind != self.right_ref.kind {
            return Err(CatalogContractError::invalid(
                "identity relation requires two distinct base entities of the same kind",
            ));
        }
        self.authority.validate()?;
        if self.quality == QualifiedValueQuality::Unknown {
            return Err(CatalogContractError::invalid(
                "unknown evidence cannot assert an identity relation",
            ));
        }
        match self.relation {
            IdentityRelationKind::SameEntity => {
                if !matches!(
                    self.quality,
                    QualifiedValueQuality::Exact | QualifiedValueQuality::NativeClaimed
                ) || self.completeness != ContractCompleteness::Complete
                {
                    return Err(CatalogContractError::invalid(
                        "same-entity reduction requires complete exact or native-claimed evidence",
                    ));
                }
                if !matches!(
                    self.canonical_winner,
                    Some(winner) if winner == self.left_ref || winner == self.right_ref
                ) {
                    return Err(CatalogContractError::invalid(
                        "same-entity evidence requires an explicit member canonical winner",
                    ));
                }
                let Some(policy) = &self.collision_policy_id else {
                    return Err(CatalogContractError::invalid(
                        "same-entity evidence requires a deterministic collision policy",
                    ));
                };
                validate_identifier("same-entity collision policy", policy)?;
            }
            IdentityRelationKind::Supersedes | IdentityRelationKind::ReplacedBy => {
                if !matches!(
                    self.quality,
                    QualifiedValueQuality::Exact | QualifiedValueQuality::NativeClaimed
                ) || self.completeness != ContractCompleteness::Complete
                {
                    return Err(CatalogContractError::invalid(
                        "replacement relation requires complete exact or native-claimed evidence",
                    ));
                }
                if self.canonical_winner.is_some() || self.collision_policy_id.is_some() {
                    return Err(CatalogContractError::invalid(
                        "replacement relation preserves both identities and has no merge winner",
                    ));
                }
            }
            IdentityRelationKind::Alias => {
                if self.canonical_winner.is_some() || self.collision_policy_id.is_some() {
                    return Err(CatalogContractError::invalid(
                        "alias evidence cannot silently declare a canonical merge winner",
                    ));
                }
            }
        }
        validate_provenance("catalog identity relation", &self.provenance)
    }

    fn replacement_edge(&self) -> Option<(CatalogEntityRef, CatalogEntityRef)> {
        match self.relation {
            IdentityRelationKind::ReplacedBy => Some((self.left_ref, self.right_ref)),
            IdentityRelationKind::Supersedes => Some((self.right_ref, self.left_ref)),
            IdentityRelationKind::Alias | IdentityRelationKind::SameEntity => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CatalogSessionAttachHandoff {
    pub presentation_ref: CatalogEntityRef,
    pub member_refs: Vec<CatalogEntityRef>,
    pub relation_keys: Vec<CatalogIdentityRelationKey>,
    pub selected_base_session_ref: CatalogEntityRef,
    pub locator_claim_key: CatalogLocatorClaimKey,
}

impl CatalogSessionAttachHandoff {
    pub(crate) fn new(
        presentation_ref: CatalogEntityRef,
        mut member_refs: Vec<CatalogEntityRef>,
        mut relation_keys: Vec<CatalogIdentityRelationKey>,
        selected_base_session_ref: CatalogEntityRef,
        locator_claim_key: CatalogLocatorClaimKey,
    ) -> Result<Self, CatalogContractError> {
        presentation_ref.validate()?;
        selected_base_session_ref.validate()?;
        if presentation_ref.kind != CatalogEntityKind::Session
            || selected_base_session_ref.kind != CatalogEntityKind::Session
        {
            return Err(CatalogContractError::invalid(
                "catalog attach handoff requires session references",
            ));
        }
        if member_refs.is_empty() || member_refs.len() > MAX_PRESENTATION_MEMBERS {
            return Err(CatalogContractError::invalid(format!(
                "catalog attach handoff requires 1..={MAX_PRESENTATION_MEMBERS} concrete members"
            )));
        }
        member_refs.sort();
        if member_refs
            .windows(2)
            .any(|members| members[0] == members[1])
            || member_refs.iter().any(|member| {
                member.kind != CatalogEntityKind::Session || member.validate().is_err()
            })
            || !member_refs.contains(&presentation_ref)
            || !member_refs.contains(&selected_base_session_ref)
        {
            return Err(CatalogContractError::invalid(
                "catalog attach selection must name exactly one disclosed concrete base-session member",
            ));
        }
        relation_keys.sort();
        if relation_keys.len() > MAX_REPLACEMENT_RELATIONS
            || relation_keys.windows(2).any(|keys| keys[0] == keys[1])
            || (member_refs.len() > 1 && relation_keys.is_empty())
        {
            return Err(CatalogContractError::invalid(
                "multi-member catalog attach handoff requires bounded distinct explicit relation evidence",
            ));
        }
        Ok(Self {
            presentation_ref,
            member_refs,
            relation_keys,
            selected_base_session_ref,
            locator_claim_key,
        })
    }
}

impl<'de> Deserialize<'de> for CatalogSessionAttachHandoff {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            presentation_ref: CatalogEntityRef,
            member_refs: Vec<CatalogEntityRef>,
            relation_keys: Vec<CatalogIdentityRelationKey>,
            selected_base_session_ref: CatalogEntityRef,
            locator_claim_key: CatalogLocatorClaimKey,
        }

        let wire = Wire::deserialize(deserializer)?;
        let member_refs = wire.member_refs.clone();
        let relation_keys = wire.relation_keys.clone();
        let value = Self::new(
            wire.presentation_ref,
            wire.member_refs,
            wire.relation_keys,
            wire.selected_base_session_ref,
            wire.locator_claim_key,
        )
        .map_err(D::Error::custom)?;
        if value.member_refs != member_refs || value.relation_keys != relation_keys {
            return Err(D::Error::custom(
                "catalog attach handoff members and relation keys must be canonical",
            ));
        }
        Ok(value)
    }
}

#[derive(Debug, Clone, Copy)]
struct FieldCandidate<'a, T> {
    assertion_key: CatalogAssertionKey,
    observation_commit: u64,
    field: &'a CatalogQualifiedField<T>,
}

fn quality_rank(quality: QualifiedValueQuality) -> u8 {
    match quality {
        QualifiedValueQuality::Exact => 5,
        QualifiedValueQuality::NativeClaimed => 4,
        QualifiedValueQuality::Derived => 3,
        QualifiedValueQuality::Estimated => 2,
        QualifiedValueQuality::Unknown => 1,
    }
}

fn compare_field_candidates<T>(
    left: &FieldCandidate<'_, T>,
    right: &FieldCandidate<'_, T>,
) -> Ordering {
    let left_value = &left.field.qualified;
    let right_value = &right.field.qualified;
    left_value
        .value
        .is_some()
        .cmp(&right_value.value.is_some())
        .then_with(|| {
            left_value
                .authority
                .precedence
                .cmp(&right_value.authority.precedence)
        })
        .then_with(|| quality_rank(left_value.quality).cmp(&quality_rank(right_value.quality)))
        .then_with(|| {
            if left_value.authority.native_times_comparable
                && right_value.authority.native_times_comparable
            {
                left_value.effective_at.cmp(&right_value.effective_at)
            } else {
                Ordering::Equal
            }
        })
        .then_with(|| left.observation_commit.cmp(&right.observation_commit))
        // A smaller evidence key wins the final deterministic tie.
        .then_with(|| right.assertion_key.cmp(&left.assertion_key))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CatalogFieldSelection<T> {
    pub selected_assertion_key: CatalogAssertionKey,
    pub field: CatalogQualifiedField<T>,
    pub conflicting_assertion_keys: Vec<CatalogAssertionKey>,
}

fn select_field<'a, T: Clone + Eq + 'a>(
    candidates: impl IntoIterator<Item = FieldCandidate<'a, T>>,
) -> Option<CatalogFieldSelection<T>> {
    let candidates: Vec<_> = candidates.into_iter().collect();
    let winner = candidates
        .iter()
        .max_by(|left, right| compare_field_candidates(left, right))?;
    let winner_value = &winner.field.qualified;
    let mut conflicting_assertion_keys: Vec<_> = candidates
        .iter()
        .filter(|candidate| {
            candidate.assertion_key != winner.assertion_key
                && candidate.field.qualified.authority == winner_value.authority
                && candidate.field.qualified.value.is_some()
                && winner_value.value.is_some()
                && candidate.field.qualified.value != winner_value.value
        })
        .map(|candidate| candidate.assertion_key)
        .collect();
    conflicting_assertion_keys.sort();
    Some(CatalogFieldSelection {
        selected_assertion_key: winner.assertion_key,
        field: winner.field.clone(),
        conflicting_assertion_keys,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StoredProjectAssertion {
    fact: CatalogProjectAssertion,
    observation_commit: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StoredSessionAssertion {
    fact: CatalogSessionAssertion,
    observation_commit: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StoredAssociation {
    fact: SessionProjectAssociationFact,
    observation_commit: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StoredLocatorClaim {
    fact: NativeLocatorClaim,
    observation_commit: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StoredIdentityRelation {
    fact: IdentityRelationFact,
    observation_commit: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CatalogProjectRow {
    pub project_ref: CatalogEntityRef,
    pub native_identity: Option<CatalogFieldSelection<NativeIdentity>>,
    pub root_identity: Option<CatalogFieldSelection<String>>,
    pub display_path: Option<CatalogFieldSelection<String>>,
    pub display_name: Option<CatalogFieldSelection<String>>,
    pub native_time: Option<CatalogFieldSelection<i64>>,
    pub availability: CatalogFieldSelection<CatalogAvailability>,
    pub assertion_keys: Vec<CatalogAssertionKey>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CatalogSessionRow {
    pub session_ref: CatalogEntityRef,
    pub project_association: CatalogAssociationCoverage,
    pub native_identity: Option<CatalogFieldSelection<NativeIdentity>>,
    pub title: Option<CatalogFieldSelection<String>>,
    pub first_user_summary: Option<CatalogFieldSelection<String>>,
    pub native_created_at: Option<CatalogFieldSelection<i64>>,
    pub native_updated_at: Option<CatalogFieldSelection<i64>>,
    pub native_message_count: Option<CatalogFieldSelection<u64>>,
    pub transcript_locator_claim_keys: Vec<CatalogLocatorClaimKey>,
    pub availability: CatalogFieldSelection<CatalogAvailability>,
    pub assertion_keys: Vec<CatalogAssertionKey>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CatalogAssociationSelection {
    pub association: SessionProjectAssociationFact,
    pub competing_associations: Vec<SessionProjectAssociationFact>,
    pub conflicting_association_keys: Vec<CatalogAssociationKey>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum CatalogAssociationCoverage {
    Available {
        selection: Box<CatalogAssociationSelection>,
    },
    Unknown,
}

fn validate_nonzero_assertion_key(
    label: &str,
    key: CatalogAssertionKey,
) -> Result<(), CatalogContractError> {
    if key.as_bytes().iter().all(|byte| *byte == 0) {
        return Err(CatalogContractError::invalid(format!(
            "{label} requires nonzero assertion keys"
        )));
    }
    Ok(())
}

fn validate_durable_field_selection<T>(
    label: &str,
    selection: &CatalogFieldSelection<T>,
    validate_field: impl FnOnce(&CatalogQualifiedField<T>) -> Result<(), CatalogContractError>,
) -> Result<Vec<CatalogAssertionKey>, CatalogContractError> {
    validate_nonzero_assertion_key(label, selection.selected_assertion_key)?;
    if selection.conflicting_assertion_keys.len() > MAX_PRESENTATION_MEMBERS
        || !selection
            .conflicting_assertion_keys
            .windows(2)
            .all(|pair| pair[0] < pair[1])
    {
        return Err(CatalogContractError::invalid(format!(
            "{label} conflicting assertion keys must be bounded, canonical, and duplicate-free"
        )));
    }
    for key in &selection.conflicting_assertion_keys {
        validate_nonzero_assertion_key(label, *key)?;
    }
    if selection
        .conflicting_assertion_keys
        .binary_search(&selection.selected_assertion_key)
        .is_ok()
    {
        return Err(CatalogContractError::invalid(format!(
            "{label} selected assertion cannot also be conflicting evidence"
        )));
    }
    validate_field(&selection.field)?;
    let mut referenced = Vec::with_capacity(selection.conflicting_assertion_keys.len() + 1);
    referenced.push(selection.selected_assertion_key);
    referenced.extend(selection.conflicting_assertion_keys.iter().copied());
    Ok(referenced)
}

fn validate_durable_row_assertions(
    assertion_keys: &[CatalogAssertionKey],
    referenced: &[CatalogAssertionKey],
) -> Result<(), CatalogContractError> {
    if assertion_keys.is_empty()
        || assertion_keys.len() > MAX_PRESENTATION_MEMBERS
        || !assertion_keys.windows(2).all(|pair| pair[0] < pair[1])
    {
        return Err(CatalogContractError::invalid(
            "durable catalog row assertion keys must be nonempty, bounded, canonical, and duplicate-free",
        ));
    }
    for key in assertion_keys {
        validate_nonzero_assertion_key("durable catalog row", *key)?;
    }
    if referenced
        .iter()
        .any(|key| assertion_keys.binary_search(key).is_err())
    {
        return Err(CatalogContractError::invalid(
            "durable catalog field evidence must belong to the row assertion set",
        ));
    }
    Ok(())
}

fn validate_durable_association(
    coverage: &CatalogAssociationCoverage,
    session_ref: CatalogEntityRef,
) -> Result<(), CatalogContractError> {
    let CatalogAssociationCoverage::Available { selection } = coverage else {
        return Ok(());
    };
    selection.association.validate()?;
    if selection.association.session_ref != session_ref {
        return Err(CatalogContractError::invalid(
            "durable catalog association belongs to a different session row",
        ));
    }
    if selection.competing_associations.len() > MAX_REPLACEMENT_RELATIONS
        || !selection
            .competing_associations
            .windows(2)
            .all(|pair| pair[0].association_key < pair[1].association_key)
    {
        return Err(CatalogContractError::invalid(
            "durable competing associations must be bounded, canonical, and duplicate-free",
        ));
    }
    let mut expected_conflicts = Vec::new();
    for competitor in &selection.competing_associations {
        competitor.validate()?;
        if competitor.session_ref != session_ref
            || competitor.association_key == selection.association.association_key
        {
            return Err(CatalogContractError::invalid(
                "durable competing association is not independent evidence for this session",
            ));
        }
        if competitor.authority == selection.association.authority
            && competitor.project_ref != selection.association.project_ref
        {
            expected_conflicts.push(competitor.association_key);
        }
    }
    if selection.conflicting_association_keys.len() > MAX_REPLACEMENT_RELATIONS
        || !selection
            .conflicting_association_keys
            .windows(2)
            .all(|pair| pair[0] < pair[1])
        || selection
            .conflicting_association_keys
            .iter()
            .any(|key| key.as_bytes().iter().all(|byte| *byte == 0))
        || selection.conflicting_association_keys != expected_conflicts
    {
        return Err(CatalogContractError::invalid(
            "durable association conflicts must exactly name canonical equal-authority project conflicts",
        ));
    }
    Ok(())
}

impl CatalogProjectRow {
    pub(super) fn validate_for_durable(&self) -> Result<(), CatalogContractError> {
        self.project_ref.validate()?;
        if self.project_ref.kind != CatalogEntityKind::Project {
            return Err(CatalogContractError::invalid(
                "durable project row requires a project reference",
            ));
        }
        let mut referenced = Vec::new();
        if let Some(selection) = &self.native_identity {
            referenced.extend(validate_durable_field_selection(
                "catalog project native identity",
                selection,
                validate_native_identity_field,
            )?);
        }
        for (label, selection) in [
            ("catalog project root identity", &self.root_identity),
            ("catalog project display path", &self.display_path),
            ("catalog project display name", &self.display_name),
        ] {
            if let Some(selection) = selection {
                referenced.extend(validate_durable_field_selection(
                    label,
                    selection,
                    |field| validate_string_field(label, field),
                )?);
            }
        }
        if let Some(selection) = &self.native_time {
            referenced.extend(validate_durable_field_selection(
                "catalog project native time",
                selection,
                CatalogQualifiedField::validate,
            )?);
        }
        referenced.extend(validate_durable_field_selection(
            "catalog project availability",
            &self.availability,
            validate_availability_field,
        )?);
        validate_durable_row_assertions(&self.assertion_keys, &referenced)
    }
}

impl CatalogSessionRow {
    pub(super) fn validate_for_durable(&self) -> Result<(), CatalogContractError> {
        self.session_ref.validate()?;
        if self.session_ref.kind != CatalogEntityKind::Session {
            return Err(CatalogContractError::invalid(
                "durable session row requires a session reference",
            ));
        }
        validate_durable_association(&self.project_association, self.session_ref)?;
        let mut referenced = Vec::new();
        if let Some(selection) = &self.native_identity {
            referenced.extend(validate_durable_field_selection(
                "catalog session native identity",
                selection,
                validate_native_identity_field,
            )?);
        }
        for (label, selection) in [
            ("catalog session title", &self.title),
            (
                "catalog session first-user summary",
                &self.first_user_summary,
            ),
        ] {
            if let Some(selection) = selection {
                referenced.extend(validate_durable_field_selection(
                    label,
                    selection,
                    |field| validate_string_field(label, field),
                )?);
            }
        }
        for (label, selection) in [
            (
                "catalog session native creation time",
                &self.native_created_at,
            ),
            (
                "catalog session native update time",
                &self.native_updated_at,
            ),
        ] {
            if let Some(selection) = selection {
                referenced.extend(validate_durable_field_selection(
                    label,
                    selection,
                    CatalogQualifiedField::validate,
                )?);
            }
        }
        if let Some(selection) = &self.native_message_count {
            referenced.extend(validate_durable_field_selection(
                "catalog session native message count",
                selection,
                CatalogQualifiedField::validate,
            )?);
        }
        if self.transcript_locator_claim_keys.len() > MAX_PRESENTATION_MEMBERS
            || !self
                .transcript_locator_claim_keys
                .windows(2)
                .all(|pair| pair[0] < pair[1])
            || self
                .transcript_locator_claim_keys
                .iter()
                .any(|key| key.as_bytes().iter().all(|byte| *byte == 0))
        {
            return Err(CatalogContractError::invalid(
                "durable transcript locator claims must be bounded, canonical, nonzero, and duplicate-free",
            ));
        }
        referenced.extend(validate_durable_field_selection(
            "catalog session availability",
            &self.availability,
            validate_availability_field,
        )?);
        validate_durable_row_assertions(&self.assertion_keys, &referenced)
    }
}

fn validate_canonical_durable_row<T: Serialize>(
    payload: &[u8],
    value: &T,
    label: &'static str,
) -> Result<(), CatalogContractError> {
    let canonical = serialize_private_json_bounded(value, payload.len(), label)?;
    if canonical != payload {
        return Err(CatalogContractError::invalid(format!(
            "{label} is not the exact canonical private JSON encoding"
        )));
    }
    Ok(())
}

pub(crate) fn decode_durable_project_row(
    payload: &[u8],
    expected_key: &[u8; DIGEST_BYTES],
    max_payload_bytes: usize,
) -> Result<CatalogProjectRow, CatalogContractError> {
    if payload.is_empty() || payload.len() > max_payload_bytes {
        return Err(CatalogContractError::invalid(
            "durable project row payload is outside its byte bound",
        ));
    }
    let row: CatalogProjectRow = serde_json::from_slice(payload).map_err(|error| {
        CatalogContractError::invalid(format!("durable project row is invalid: {error}"))
    })?;
    row.validate_for_durable()?;
    if row.project_ref.external_ref.entity_key.as_bytes() != expected_key {
        return Err(CatalogContractError::invalid(
            "durable project row does not match its frame key",
        ));
    }
    validate_canonical_durable_row(payload, &row, "durable catalog project row")?;
    Ok(row)
}

pub(crate) fn decode_durable_session_row(
    payload: &[u8],
    expected_key: &[u8; DIGEST_BYTES],
    max_payload_bytes: usize,
) -> Result<CatalogSessionRow, CatalogContractError> {
    if payload.is_empty() || payload.len() > max_payload_bytes {
        return Err(CatalogContractError::invalid(
            "durable session row payload is outside its byte bound",
        ));
    }
    let row: CatalogSessionRow = serde_json::from_slice(payload).map_err(|error| {
        CatalogContractError::invalid(format!("durable session row is invalid: {error}"))
    })?;
    row.validate_for_durable()?;
    if row.session_ref.external_ref.entity_key.as_bytes() != expected_key {
        return Err(CatalogContractError::invalid(
            "durable session row does not match its frame key",
        ));
    }
    validate_canonical_durable_row(payload, &row, "durable catalog session row")?;
    Ok(row)
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CatalogTombstoneSourceGeneration {
    pub source_instance_key: CanonicalSourceInstanceKey,
    pub max_generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CatalogTombstone {
    pub entity_ref: CatalogEntityRef,
    pub absence_evidence: Vec<CatalogRetractionEvidence>,
    pub confirmed_absent_at_commit: u64,
    pub prior_assertion_keys: Vec<CatalogAssertionKey>,
    pub prior_source_generations: Vec<CatalogTombstoneSourceGeneration>,
    pub provenance: Vec<SemanticRevisionRef>,
}

impl CatalogTombstone {
    fn validate_shape(&self) -> Result<(), CatalogContractError> {
        self.entity_ref.validate()?;
        if self.absence_evidence.is_empty()
            || self.confirmed_absent_at_commit == 0
            || self.prior_assertion_keys.is_empty()
            || self.prior_source_generations.is_empty()
        {
            return Err(CatalogContractError::invalid(
                "catalog tombstone requires confirmed absence and prior membership evidence",
            ));
        }
        for evidence in &self.absence_evidence {
            evidence.validate()?;
        }
        if !self
            .absence_evidence
            .windows(2)
            .all(|pair| pair[0].owner < pair[1].owner)
            || !self
                .prior_assertion_keys
                .windows(2)
                .all(|pair| pair[0] < pair[1])
            || self
                .prior_assertion_keys
                .iter()
                .any(|key| key.as_bytes().iter().all(|byte| *byte == 0))
            || !self
                .prior_source_generations
                .windows(2)
                .all(|pair| pair[0] < pair[1])
            || self
                .prior_source_generations
                .iter()
                .any(|source| source.max_generation == 0)
        {
            return Err(CatalogContractError::invalid(
                "catalog tombstone membership evidence must be nonzero, canonical, and duplicate-free",
            ));
        }
        validate_provenance("catalog tombstone", &self.provenance)
    }
}

pub(crate) fn decode_durable_tombstone(
    payload: &[u8],
    expected_key: &[u8; DIGEST_BYTES],
    max_payload_bytes: usize,
) -> Result<CatalogTombstone, CatalogContractError> {
    if payload.is_empty() || payload.len() > max_payload_bytes {
        return Err(CatalogContractError::invalid(
            "durable catalog tombstone payload is outside its byte bound",
        ));
    }
    let tombstone: CatalogTombstone = serde_json::from_slice(payload).map_err(|error| {
        CatalogContractError::invalid(format!("durable catalog tombstone is invalid: {error}"))
    })?;
    tombstone.validate_shape()?;
    if tombstone.entity_ref.external_ref.entity_key.as_bytes() != expected_key {
        return Err(CatalogContractError::invalid(
            "durable catalog tombstone does not match its frame key",
        ));
    }
    let canonical =
        serialize_private_json_bounded(&tombstone, payload.len(), "durable catalog tombstone")?;
    if canonical != payload {
        return Err(CatalogContractError::invalid(
            "durable catalog tombstone is not canonical",
        ));
    }
    Ok(tombstone)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CatalogUnknownReferenceReason {
    NeverObserved,
    RetractedPendingPublication,
    RelatedIdentityOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "row", rename_all = "snake_case")]
pub(crate) enum CatalogLiveRow {
    Project(CatalogProjectRow),
    Session(CatalogSessionRow),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum CatalogResolvedEntity {
    Live {
        entity_ref: CatalogEntityRef,
        row: Box<CatalogLiveRow>,
    },
    Tombstoned {
        tombstone: CatalogTombstone,
    },
    Superseded {
        prior_ref: CatalogEntityRef,
        replacement_refs: Vec<CatalogEntityRef>,
        relation_keys: Vec<CatalogIdentityRelationKey>,
        provenance: Vec<SemanticRevisionRef>,
    },
    Unknown {
        requested_ref: ExternalEntityRef,
        reason: CatalogUnknownReferenceReason,
        related_refs: Vec<CatalogEntityRef>,
        relation_keys: Vec<CatalogIdentityRelationKey>,
    },
}

/// Privacy-minimal restart-validated lifecycle state. It deliberately omits
/// native locators, native identities, relation keys, and materialized rows;
/// a live row must still be loaded through its authenticated durable frame.
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum CatalogResolvedLifecycle {
    Live {
        entity_ref: CatalogEntityRef,
    },
    Tombstoned {
        entity_ref: CatalogEntityRef,
        provenance: Vec<SemanticRevisionRef>,
    },
    Superseded {
        prior_ref: CatalogEntityRef,
        replacement_refs: Vec<CatalogEntityRef>,
        provenance: Vec<SemanticRevisionRef>,
    },
    Unknown {
        requested_ref: ExternalEntityRef,
        reason: CatalogUnknownReferenceReason,
    },
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct CatalogResolutionIndex {
    entries: BTreeMap<CanonicalEntityKey, CatalogResolvedLifecycle>,
}

impl fmt::Debug for CatalogResolutionIndex {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut live = 0_usize;
        let mut tombstoned = 0_usize;
        let mut superseded = 0_usize;
        let mut unknown = 0_usize;
        for entry in self.entries.values() {
            match entry {
                CatalogResolvedLifecycle::Live { .. } => live += 1,
                CatalogResolvedLifecycle::Tombstoned { .. } => tombstoned += 1,
                CatalogResolvedLifecycle::Superseded { .. } => superseded += 1,
                CatalogResolvedLifecycle::Unknown { .. } => unknown += 1,
            }
        }
        formatter
            .debug_struct("CatalogResolutionIndex")
            .field("entry_count", &self.entries.len())
            .field("live_count", &live)
            .field("tombstoned_count", &tombstoned)
            .field("superseded_count", &superseded)
            .field("unknown_count", &unknown)
            .finish_non_exhaustive()
    }
}

impl CatalogResolutionIndex {
    pub(crate) fn resolve(&self, external_ref: ExternalEntityRef) -> CatalogResolvedLifecycle {
        self.entries
            .get(&external_ref.entity_key)
            .cloned()
            .unwrap_or(CatalogResolvedLifecycle::Unknown {
                requested_ref: external_ref,
                reason: CatalogUnknownReferenceReason::NeverObserved,
            })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CatalogMutation {
    Inserted,
    Updated,
    Noop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CatalogRetractionCause {
    ConfirmedDeletion,
    ConfirmedReplacement,
    TemporarilyUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CatalogRetractionEvidence {
    pub owner: CatalogEvidenceOwner,
    pub cause: CatalogRetractionCause,
    pub completeness: ContractCompleteness,
    pub provenance: Vec<SemanticRevisionRef>,
}

impl CatalogRetractionEvidence {
    pub(crate) fn new(
        owner: CatalogEvidenceOwner,
        cause: CatalogRetractionCause,
        completeness: ContractCompleteness,
        provenance: Vec<SemanticRevisionRef>,
    ) -> Result<Self, CatalogContractError> {
        let value = Self {
            owner,
            cause,
            completeness,
            provenance,
        };
        value.validate()?;
        Ok(value)
    }

    fn validate(&self) -> Result<(), CatalogContractError> {
        self.owner.validate()?;
        if self.completeness != ContractCompleteness::Complete
            || self.cause == CatalogRetractionCause::TemporarilyUnavailable
        {
            return Err(CatalogContractError::invalid(
                "catalog retraction requires complete confirmed deletion or replacement evidence",
            ));
        }
        validate_provenance("catalog retraction evidence", &self.provenance)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RetractedAssertion {
    evidence: CatalogRetractionEvidence,
    retracted_at_commit: u64,
    provenance: Vec<SemanticRevisionRef>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DurableFrozenFact<T> {
    fact: T,
    observation_commit: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DurableAssertionHistory {
    assertion_key: CatalogAssertionKey,
    owner: CatalogEvidenceOwner,
    entity_ref: CatalogEntityRef,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DurableAssociationHistory {
    association_key: CatalogAssociationKey,
    owner: CatalogEvidenceOwner,
    session_ref: CatalogEntityRef,
    project_ref: CatalogEntityRef,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DurableLocatorHistory {
    locator_claim_key: CatalogLocatorClaimKey,
    owner: CatalogEvidenceOwner,
    subject_ref: CatalogEntityRef,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DurableIdentityRelationHistory {
    relation_key: CatalogIdentityRelationKey,
    owner: CatalogEvidenceOwner,
    relation: IdentityRelationKind,
    left_ref: CatalogEntityRef,
    right_ref: CatalogEntityRef,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DurableEntityKindEntry {
    entity_key: CanonicalEntityKey,
    kind: CatalogEntityKind,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DurableRetractedAssertionEntry {
    entity_ref: CatalogEntityRef,
    assertion_key: CatalogAssertionKey,
    evidence: CatalogRetractionEvidence,
    retracted_at_commit: u64,
    provenance: Vec<SemanticRevisionRef>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DurableReducerStateWire {
    durable_reducer_state_contract_version: u32,
    projects: Vec<DurableFrozenFact<CatalogProjectAssertion>>,
    sessions: Vec<DurableFrozenFact<CatalogSessionAssertion>>,
    associations: Vec<DurableFrozenFact<SessionProjectAssociationFact>>,
    locators: Vec<DurableFrozenFact<NativeLocatorClaim>>,
    identity_relations: Vec<DurableFrozenFact<IdentityRelationFact>>,
    retracted_owners: Vec<CatalogRetractionEvidence>,
    assertion_history: Vec<DurableAssertionHistory>,
    association_history: Vec<DurableAssociationHistory>,
    locator_history: Vec<DurableLocatorHistory>,
    identity_relation_history: Vec<DurableIdentityRelationHistory>,
    entity_kinds: Vec<DurableEntityKindEntry>,
    retracted_assertions: Vec<DurableRetractedAssertionEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AssertionCoordinates {
    owner: CatalogEvidenceOwner,
    entity_ref: CatalogEntityRef,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AssociationCoordinates {
    owner: CatalogEvidenceOwner,
    session_ref: CatalogEntityRef,
    project_ref: CatalogEntityRef,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LocatorCoordinates {
    owner: CatalogEvidenceOwner,
    subject_ref: CatalogEntityRef,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct IdentityRelationCoordinates {
    owner: CatalogEvidenceOwner,
    relation: IdentityRelationKind,
    left_ref: CatalogEntityRef,
    right_ref: CatalogEntityRef,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CatalogRetraction {
    pub assertion_count: usize,
    pub association_count: usize,
    pub locator_count: usize,
    pub identity_relation_count: usize,
    pub orphaned_entities: Vec<CatalogEntityRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CatalogAttachTarget {
    pub session_ref: CatalogEntityRef,
    pub locator_claim_key: CatalogLocatorClaimKey,
    pub locator_owner: CatalogEvidenceOwner,
    pub locator_kind: CatalogLocatorKind,
    pub locator_basis: ProjectAssociationBasis,
    pub locator_disclosure: CatalogDisclosureClass,
    pub locator: CatalogQualifiedValue<CatalogLocatorValue>,
    pub provenance: Vec<SemanticRevisionRef>,
}

/// Caller-selected ceilings for one immutable reducer publication freeze.
/// These are safety caps for the contract assembly, not performance targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CatalogReducerPublicationLimits {
    pub max_reducer_entries: usize,
    pub max_rows: usize,
}

impl CatalogReducerPublicationLimits {
    pub(crate) fn new(
        max_reducer_entries: usize,
        max_rows: usize,
    ) -> Result<Self, CatalogContractError> {
        if max_reducer_entries == 0
            || max_reducer_entries > MAX_PUBLICATION_REDUCER_ENTRIES
            || max_rows == 0
            || max_rows > MAX_PUBLICATION_ROWS
        {
            return Err(CatalogContractError::invalid(format!(
                "catalog publication limits must be within entries=1..={MAX_PUBLICATION_REDUCER_ENTRIES}, rows=1..={MAX_PUBLICATION_ROWS}"
            )));
        }
        Ok(Self {
            max_reducer_entries,
            max_rows,
        })
    }
}

impl Default for CatalogReducerPublicationLimits {
    fn default() -> Self {
        Self {
            max_reducer_entries: MAX_PUBLICATION_REDUCER_ENTRIES,
            max_rows: MAX_PUBLICATION_ROWS,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct CatalogReducerPublicationRevision([u8; DIGEST_BYTES]);

impl CatalogReducerPublicationRevision {
    pub(crate) fn from_storage_bytes(bytes: [u8; DIGEST_BYTES]) -> Self {
        Self(bytes)
    }

    pub(super) fn as_bytes(&self) -> &[u8; DIGEST_BYTES] {
        &self.0
    }

    pub(crate) fn storage_bytes(&self) -> &[u8; DIGEST_BYTES] {
        &self.0
    }
}

impl fmt::Debug for CatalogReducerPublicationRevision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("CatalogReducerPublicationRevision")
            .field(&format_args!(
                "{REFERENCE_ENCODING_VERSION}:{}",
                URL_SAFE_NO_PAD.encode(self.0)
            ))
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct FrozenProjectAssertion {
    pub(super) fact: CatalogProjectAssertion,
    pub(super) observation_commit: u64,
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct FrozenSessionAssertion {
    pub(super) fact: CatalogSessionAssertion,
    pub(super) observation_commit: u64,
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct FrozenAssociation {
    pub(super) fact: SessionProjectAssociationFact,
    pub(super) observation_commit: u64,
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct FrozenLocatorClaim {
    pub(super) fact: NativeLocatorClaim,
    pub(super) observation_commit: u64,
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct FrozenIdentityRelation {
    pub(super) fact: IdentityRelationFact,
    pub(super) observation_commit: u64,
}

/// Immutable, canonical reducer state consumed by the store-free B3
/// publication envelope. Debug deliberately exposes only counts and the
/// revision because facts and rows may retain local paths or native IDs.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct CatalogReducerPublication {
    pub(super) projects: Vec<FrozenProjectAssertion>,
    pub(super) sessions: Vec<FrozenSessionAssertion>,
    pub(super) associations: Vec<FrozenAssociation>,
    pub(super) locators: Vec<FrozenLocatorClaim>,
    pub(super) identity_relations: Vec<FrozenIdentityRelation>,
    pub(super) project_rows: Vec<CatalogProjectRow>,
    pub(super) session_rows: Vec<CatalogSessionRow>,
    pub(super) tombstones: Vec<CatalogTombstone>,
    pub(super) retracted_owners: Vec<CatalogRetractionEvidence>,
    assertion_history: BTreeMap<CatalogAssertionKey, AssertionCoordinates>,
    association_history: BTreeMap<CatalogAssociationKey, AssociationCoordinates>,
    locator_history: BTreeMap<CatalogLocatorClaimKey, LocatorCoordinates>,
    identity_relation_history: BTreeMap<CatalogIdentityRelationKey, IdentityRelationCoordinates>,
    entity_kinds: BTreeMap<CanonicalEntityKey, CatalogEntityKind>,
    retracted_assertions:
        BTreeMap<CatalogEntityRef, BTreeMap<CatalogAssertionKey, RetractedAssertion>>,
    revision: CatalogReducerPublicationRevision,
}

impl CatalogReducerPublication {
    pub(crate) fn revision(&self) -> CatalogReducerPublicationRevision {
        self.revision
    }

    pub(crate) fn project_row_count(&self) -> usize {
        self.project_rows.len()
    }

    pub(crate) fn session_row_count(&self) -> usize {
        self.session_rows.len()
    }

    pub(crate) fn tombstone_count(&self) -> usize {
        self.tombstones.len()
    }

    pub(super) fn project_rows(&self) -> &[CatalogProjectRow] {
        &self.project_rows
    }

    pub(super) fn session_rows(&self) -> &[CatalogSessionRow] {
        &self.session_rows
    }

    pub(super) fn tombstones(&self) -> &[CatalogTombstone] {
        &self.tombstones
    }

    /// Canonical private restart/audit payload for the reducer state that is
    /// not already represented by the immutable materialized rows and
    /// tombstones. This is deliberately not a public wire contract and has no
    /// unchecked decoder. B3 persistence stores the bytes with their digest;
    /// a later refresh slice must introduce a checked decoder before it can
    /// resume mutation from this history.
    pub(super) fn durable_state_json(
        &self,
        max_encoded_bytes: usize,
    ) -> Result<Vec<u8>, CatalogContractError> {
        #[derive(Serialize)]
        struct FrozenFact<'a, T> {
            fact: &'a T,
            observation_commit: u64,
        }

        #[derive(Serialize)]
        struct AssertionHistory<'a> {
            assertion_key: &'a CatalogAssertionKey,
            owner: &'a CatalogEvidenceOwner,
            entity_ref: CatalogEntityRef,
        }

        #[derive(Serialize)]
        struct AssociationHistory<'a> {
            association_key: &'a CatalogAssociationKey,
            owner: &'a CatalogEvidenceOwner,
            session_ref: CatalogEntityRef,
            project_ref: CatalogEntityRef,
        }

        #[derive(Serialize)]
        struct LocatorHistory<'a> {
            locator_claim_key: &'a CatalogLocatorClaimKey,
            owner: &'a CatalogEvidenceOwner,
            subject_ref: CatalogEntityRef,
        }

        #[derive(Serialize)]
        struct IdentityRelationHistory<'a> {
            relation_key: &'a CatalogIdentityRelationKey,
            owner: &'a CatalogEvidenceOwner,
            relation: IdentityRelationKind,
            left_ref: CatalogEntityRef,
            right_ref: CatalogEntityRef,
        }

        #[derive(Serialize)]
        struct EntityKindEntry<'a> {
            entity_key: &'a CanonicalEntityKey,
            kind: CatalogEntityKind,
        }

        #[derive(Serialize)]
        struct RetractedAssertionEntry<'a> {
            entity_ref: CatalogEntityRef,
            assertion_key: &'a CatalogAssertionKey,
            evidence: &'a CatalogRetractionEvidence,
            retracted_at_commit: u64,
            provenance: &'a [SemanticRevisionRef],
        }

        #[derive(Serialize)]
        struct DurableReducerState<'a> {
            durable_reducer_state_contract_version: u32,
            projects: Vec<FrozenFact<'a, CatalogProjectAssertion>>,
            sessions: Vec<FrozenFact<'a, CatalogSessionAssertion>>,
            associations: Vec<FrozenFact<'a, SessionProjectAssociationFact>>,
            locators: Vec<FrozenFact<'a, NativeLocatorClaim>>,
            identity_relations: Vec<FrozenFact<'a, IdentityRelationFact>>,
            retracted_owners: &'a [CatalogRetractionEvidence],
            assertion_history: Vec<AssertionHistory<'a>>,
            association_history: Vec<AssociationHistory<'a>>,
            locator_history: Vec<LocatorHistory<'a>>,
            identity_relation_history: Vec<IdentityRelationHistory<'a>>,
            entity_kinds: Vec<EntityKindEntry<'a>>,
            retracted_assertions: Vec<RetractedAssertionEntry<'a>>,
        }

        let projects = self
            .projects
            .iter()
            .map(|stored| FrozenFact {
                fact: &stored.fact,
                observation_commit: stored.observation_commit,
            })
            .collect();
        let sessions = self
            .sessions
            .iter()
            .map(|stored| FrozenFact {
                fact: &stored.fact,
                observation_commit: stored.observation_commit,
            })
            .collect();
        let associations = self
            .associations
            .iter()
            .map(|stored| FrozenFact {
                fact: &stored.fact,
                observation_commit: stored.observation_commit,
            })
            .collect();
        let locators = self
            .locators
            .iter()
            .map(|stored| FrozenFact {
                fact: &stored.fact,
                observation_commit: stored.observation_commit,
            })
            .collect();
        let identity_relations = self
            .identity_relations
            .iter()
            .map(|stored| FrozenFact {
                fact: &stored.fact,
                observation_commit: stored.observation_commit,
            })
            .collect();
        let assertion_history = self
            .assertion_history
            .iter()
            .map(|(assertion_key, coordinates)| AssertionHistory {
                assertion_key,
                owner: &coordinates.owner,
                entity_ref: coordinates.entity_ref,
            })
            .collect();
        let association_history = self
            .association_history
            .iter()
            .map(|(association_key, coordinates)| AssociationHistory {
                association_key,
                owner: &coordinates.owner,
                session_ref: coordinates.session_ref,
                project_ref: coordinates.project_ref,
            })
            .collect();
        let locator_history = self
            .locator_history
            .iter()
            .map(|(locator_claim_key, coordinates)| LocatorHistory {
                locator_claim_key,
                owner: &coordinates.owner,
                subject_ref: coordinates.subject_ref,
            })
            .collect();
        let identity_relation_history = self
            .identity_relation_history
            .iter()
            .map(|(relation_key, coordinates)| IdentityRelationHistory {
                relation_key,
                owner: &coordinates.owner,
                relation: coordinates.relation,
                left_ref: coordinates.left_ref,
                right_ref: coordinates.right_ref,
            })
            .collect();
        let entity_kinds = self
            .entity_kinds
            .iter()
            .map(|(entity_key, kind)| EntityKindEntry {
                entity_key,
                kind: *kind,
            })
            .collect();
        let retracted_assertions = self
            .retracted_assertions
            .iter()
            .flat_map(|(entity_ref, assertions)| {
                assertions
                    .iter()
                    .map(move |(assertion_key, retracted)| RetractedAssertionEntry {
                        entity_ref: *entity_ref,
                        assertion_key,
                        evidence: &retracted.evidence,
                        retracted_at_commit: retracted.retracted_at_commit,
                        provenance: &retracted.provenance,
                    })
            })
            .collect();
        serialize_private_json_bounded(
            &DurableReducerState {
                durable_reducer_state_contract_version: 1,
                projects,
                sessions,
                associations,
                locators,
                identity_relations,
                retracted_owners: &self.retracted_owners,
                assertion_history,
                association_history,
                locator_history,
                identity_relation_history,
                entity_kinds,
                retracted_assertions,
            },
            max_encoded_bytes,
            "catalog reducer durable state",
        )
    }
}

/// Checked, non-serializable intermediate reconstructed from the private
/// durable reducer frame. Tombstones are separate frames and must be supplied
/// before the immutable publication can be re-frozen.
pub(crate) struct CatalogDurableReducerRestore {
    reducer: CatalogReducer,
}

pub(crate) fn decode_durable_reducer_state(
    payload: &[u8],
    max_payload_bytes: usize,
) -> Result<CatalogDurableReducerRestore, CatalogContractError> {
    if payload.is_empty() || payload.len() > max_payload_bytes {
        return Err(CatalogContractError::invalid(
            "durable catalog reducer payload is outside its byte bound",
        ));
    }
    let wire: DurableReducerStateWire = serde_json::from_slice(payload).map_err(|error| {
        CatalogContractError::invalid(format!("durable catalog reducer is invalid: {error}"))
    })?;
    if wire.durable_reducer_state_contract_version != 1 {
        return Err(CatalogContractError::invalid(
            "durable catalog reducer has an unsupported contract version",
        ));
    }
    let canonical =
        serialize_private_json_bounded(&wire, payload.len(), "durable catalog reducer")?;
    if canonical != payload {
        return Err(CatalogContractError::invalid(
            "durable catalog reducer is not canonical",
        ));
    }

    if !wire
        .projects
        .windows(2)
        .all(|pair| pair[0].fact.assertion_key < pair[1].fact.assertion_key)
        || !wire
            .sessions
            .windows(2)
            .all(|pair| pair[0].fact.assertion_key < pair[1].fact.assertion_key)
        || !wire
            .associations
            .windows(2)
            .all(|pair| pair[0].fact.association_key < pair[1].fact.association_key)
        || !wire
            .locators
            .windows(2)
            .all(|pair| pair[0].fact.locator_claim_key < pair[1].fact.locator_claim_key)
        || !wire
            .identity_relations
            .windows(2)
            .all(|pair| pair[0].fact.relation_key < pair[1].fact.relation_key)
        || !wire
            .retracted_owners
            .windows(2)
            .all(|pair| pair[0].owner < pair[1].owner)
        || !wire
            .assertion_history
            .windows(2)
            .all(|pair| pair[0].assertion_key < pair[1].assertion_key)
        || !wire
            .association_history
            .windows(2)
            .all(|pair| pair[0].association_key < pair[1].association_key)
        || !wire
            .locator_history
            .windows(2)
            .all(|pair| pair[0].locator_claim_key < pair[1].locator_claim_key)
        || !wire
            .identity_relation_history
            .windows(2)
            .all(|pair| pair[0].relation_key < pair[1].relation_key)
        || !wire
            .entity_kinds
            .windows(2)
            .all(|pair| pair[0].entity_key < pair[1].entity_key)
        || !wire.retracted_assertions.windows(2).all(|pair| {
            (pair[0].entity_ref, pair[0].assertion_key)
                < (pair[1].entity_ref, pair[1].assertion_key)
        })
    {
        return Err(CatalogContractError::invalid(
            "durable catalog reducer members must be canonical and duplicate-free",
        ));
    }

    let mut reducer = CatalogReducer::default();
    for stored in wire.projects {
        if stored
            .fact
            .assertion_key
            .as_bytes()
            .iter()
            .all(|byte| *byte == 0)
        {
            return Err(CatalogContractError::invalid(
                "durable catalog project assertion key must be nonzero",
            ));
        }
        reducer.upsert_project_assertion(stored.fact, stored.observation_commit)?;
    }
    for stored in wire.sessions {
        if stored
            .fact
            .assertion_key
            .as_bytes()
            .iter()
            .all(|byte| *byte == 0)
        {
            return Err(CatalogContractError::invalid(
                "durable catalog session assertion key must be nonzero",
            ));
        }
        reducer.upsert_session_assertion(stored.fact, stored.observation_commit)?;
    }
    for stored in wire.associations {
        reducer.upsert_association(stored.fact, stored.observation_commit)?;
    }
    for stored in wire.locators {
        if stored
            .fact
            .locator_claim_key
            .as_bytes()
            .iter()
            .all(|byte| *byte == 0)
        {
            return Err(CatalogContractError::invalid(
                "durable catalog locator key must be nonzero",
            ));
        }
        reducer.upsert_locator_claim(stored.fact, stored.observation_commit)?;
    }
    for stored in wire.identity_relations {
        if stored
            .fact
            .relation_key
            .as_bytes()
            .iter()
            .all(|byte| *byte == 0)
        {
            return Err(CatalogContractError::invalid(
                "durable catalog identity-relation key must be nonzero",
            ));
        }
        reducer.upsert_identity_relation(stored.fact, stored.observation_commit)?;
    }

    let mut retracted_owners = BTreeMap::new();
    for evidence in wire.retracted_owners {
        evidence.validate()?;
        if reducer
            .projects
            .values()
            .any(|stored| stored.fact.owner == evidence.owner)
            || reducer
                .sessions
                .values()
                .any(|stored| stored.fact.owner == evidence.owner)
            || reducer
                .associations
                .values()
                .any(|stored| stored.fact.owner == evidence.owner)
            || reducer
                .locators
                .values()
                .any(|stored| stored.fact.owner == evidence.owner)
            || reducer
                .identity_relations
                .values()
                .any(|stored| stored.fact.owner == evidence.owner)
        {
            return Err(CatalogContractError::invalid(
                "durable catalog reducer cannot retain live evidence for a retracted owner",
            ));
        }
        retracted_owners.insert(evidence.owner.clone(), evidence);
    }

    let mut assertion_history = BTreeMap::new();
    for entry in wire.assertion_history {
        entry.owner.validate()?;
        entry.entity_ref.validate()?;
        if entry.assertion_key.as_bytes().iter().all(|byte| *byte == 0) {
            return Err(CatalogContractError::invalid(
                "durable catalog assertion history key must be nonzero",
            ));
        }
        assertion_history.insert(
            entry.assertion_key,
            AssertionCoordinates {
                owner: entry.owner,
                entity_ref: entry.entity_ref,
            },
        );
    }
    let mut association_history = BTreeMap::new();
    for entry in wire.association_history {
        entry.owner.validate()?;
        entry.session_ref.validate()?;
        entry.project_ref.validate()?;
        if entry
            .association_key
            .as_bytes()
            .iter()
            .all(|byte| *byte == 0)
        {
            return Err(CatalogContractError::invalid(
                "durable catalog association history key must be nonzero",
            ));
        }
        association_history.insert(
            entry.association_key,
            AssociationCoordinates {
                owner: entry.owner,
                session_ref: entry.session_ref,
                project_ref: entry.project_ref,
            },
        );
    }
    let mut locator_history = BTreeMap::new();
    for entry in wire.locator_history {
        entry.owner.validate()?;
        entry.subject_ref.validate()?;
        if entry
            .locator_claim_key
            .as_bytes()
            .iter()
            .all(|byte| *byte == 0)
        {
            return Err(CatalogContractError::invalid(
                "durable catalog locator history key must be nonzero",
            ));
        }
        locator_history.insert(
            entry.locator_claim_key,
            LocatorCoordinates {
                owner: entry.owner,
                subject_ref: entry.subject_ref,
            },
        );
    }
    let mut identity_relation_history = BTreeMap::new();
    for entry in wire.identity_relation_history {
        entry.owner.validate()?;
        entry.left_ref.validate()?;
        entry.right_ref.validate()?;
        if entry.relation_key.as_bytes().iter().all(|byte| *byte == 0) {
            return Err(CatalogContractError::invalid(
                "durable catalog identity-relation history key must be nonzero",
            ));
        }
        identity_relation_history.insert(
            entry.relation_key,
            IdentityRelationCoordinates {
                owner: entry.owner,
                relation: entry.relation,
                left_ref: entry.left_ref,
                right_ref: entry.right_ref,
            },
        );
    }

    if reducer
        .assertion_history
        .iter()
        .any(|(key, coordinates)| assertion_history.get(key) != Some(coordinates))
        || reducer
            .association_history
            .iter()
            .any(|(key, coordinates)| association_history.get(key) != Some(coordinates))
        || reducer
            .locator_history
            .iter()
            .any(|(key, coordinates)| locator_history.get(key) != Some(coordinates))
        || reducer
            .identity_relation_history
            .iter()
            .any(|(key, coordinates)| identity_relation_history.get(key) != Some(coordinates))
    {
        return Err(CatalogContractError::invalid(
            "durable catalog reducer history does not match its live evidence coordinates",
        ));
    }

    let live_assertions = reducer
        .projects
        .keys()
        .chain(reducer.sessions.keys())
        .copied()
        .collect::<BTreeSet<_>>();
    let live_associations = reducer
        .associations
        .keys()
        .copied()
        .collect::<BTreeSet<_>>();
    let live_locators = reducer.locators.keys().copied().collect::<BTreeSet<_>>();
    let live_relations = reducer
        .identity_relations
        .keys()
        .copied()
        .collect::<BTreeSet<_>>();
    if association_history.iter().any(|(key, coordinates)| {
        !live_associations.contains(key) && !retracted_owners.contains_key(&coordinates.owner)
    }) || locator_history.iter().any(|(key, coordinates)| {
        !live_locators.contains(key) && !retracted_owners.contains_key(&coordinates.owner)
    }) || identity_relation_history.iter().any(|(key, coordinates)| {
        !live_relations.contains(key) && !retracted_owners.contains_key(&coordinates.owner)
    }) {
        return Err(CatalogContractError::invalid(
            "durable catalog history without live evidence requires an exact owner retraction",
        ));
    }

    let mut retracted_assertions: BTreeMap<
        CatalogEntityRef,
        BTreeMap<CatalogAssertionKey, RetractedAssertion>,
    > = BTreeMap::new();
    for entry in wire.retracted_assertions {
        entry.entity_ref.validate()?;
        entry.evidence.validate()?;
        validate_observation_commit(entry.retracted_at_commit)?;
        validate_provenance("durable retracted catalog assertion", &entry.provenance)?;
        let coordinates = assertion_history.get(&entry.assertion_key).ok_or_else(|| {
            CatalogContractError::invalid(
                "durable retracted assertion is missing immutable assertion history",
            )
        })?;
        if live_assertions.contains(&entry.assertion_key)
            || coordinates.entity_ref != entry.entity_ref
            || coordinates.owner != entry.evidence.owner
            || retracted_owners.get(&entry.evidence.owner) != Some(&entry.evidence)
        {
            return Err(CatalogContractError::invalid(
                "durable retracted assertion does not match its owner and immutable coordinates",
            ));
        }
        retracted_assertions
            .entry(entry.entity_ref)
            .or_default()
            .insert(
                entry.assertion_key,
                RetractedAssertion {
                    evidence: entry.evidence,
                    retracted_at_commit: entry.retracted_at_commit,
                    provenance: entry.provenance,
                },
            );
    }
    if assertion_history.iter().any(|(key, coordinates)| {
        !live_assertions.contains(key)
            && retracted_assertions
                .get(&coordinates.entity_ref)
                .and_then(|assertions| assertions.get(key))
                .is_none()
    }) {
        return Err(CatalogContractError::invalid(
            "durable catalog assertion history is neither live nor explicitly retracted",
        ));
    }

    let mut derived_entity_kinds = BTreeMap::new();
    let mut register = |entity_ref: CatalogEntityRef| -> Result<(), CatalogContractError> {
        entity_ref.validate()?;
        if derived_entity_kinds
            .insert(entity_ref.external_ref.entity_key, entity_ref.kind)
            .is_some_and(|kind| kind != entity_ref.kind)
        {
            return Err(CatalogContractError::invalid(
                "durable catalog reducer strengthens one entity key into two kinds",
            ));
        }
        Ok(())
    };
    for coordinates in assertion_history.values() {
        register(coordinates.entity_ref)?;
    }
    for coordinates in association_history.values() {
        register(coordinates.session_ref)?;
        register(coordinates.project_ref)?;
    }
    for coordinates in locator_history.values() {
        register(coordinates.subject_ref)?;
    }
    for coordinates in identity_relation_history.values() {
        register(coordinates.left_ref)?;
        register(coordinates.right_ref)?;
    }
    let entity_kinds = wire
        .entity_kinds
        .into_iter()
        .map(|entry| (entry.entity_key, entry.kind))
        .collect::<BTreeMap<_, _>>();
    if entity_kinds != derived_entity_kinds {
        return Err(CatalogContractError::invalid(
            "durable catalog entity-kind history is not exactly derived from evidence coordinates",
        ));
    }

    reducer.assertion_history = assertion_history;
    reducer.association_history = association_history;
    reducer.locator_history = locator_history;
    reducer.identity_relation_history = identity_relation_history;
    reducer.retracted_owners = retracted_owners;
    reducer.entity_kinds = entity_kinds;
    reducer.retracted_assertions = retracted_assertions;
    Ok(CatalogDurableReducerRestore { reducer })
}

impl CatalogDurableReducerRestore {
    pub(crate) fn finish(
        mut self,
        mut tombstones: Vec<CatalogTombstone>,
        expected_revision: CatalogReducerPublicationRevision,
        limits: CatalogReducerPublicationLimits,
    ) -> Result<CatalogReducerPublication, CatalogContractError> {
        // The durable table orders tombstone frames by opaque entity key;
        // semantic reducer order also includes the entity kind.
        tombstones.sort_by_key(|tombstone| tombstone.entity_ref);
        if !tombstones
            .windows(2)
            .all(|pair| pair[0].entity_ref < pair[1].entity_ref)
        {
            return Err(CatalogContractError::invalid(
                "durable catalog tombstones must be canonical and duplicate-free",
            ));
        }
        for tombstone in tombstones {
            tombstone.validate_shape()?;
            if self.reducer.entity_has_assertion(tombstone.entity_ref) {
                return Err(CatalogContractError::invalid(
                    "durable catalog tombstone cannot coexist with live membership",
                ));
            }
            let history = self
                .reducer
                .retracted_assertions
                .get(&tombstone.entity_ref)
                .ok_or_else(|| {
                    CatalogContractError::invalid(
                        "durable catalog tombstone is missing retracted membership history",
                    )
                })?;
            let expected_keys = history.keys().copied().collect::<Vec<_>>();
            let expected_evidence = history
                .values()
                .map(|retracted| (retracted.evidence.owner.clone(), retracted.evidence.clone()))
                .collect::<BTreeMap<_, _>>()
                .into_values()
                .collect::<Vec<_>>();
            let mut generations = BTreeMap::new();
            for retracted in history.values() {
                generations
                    .entry(retracted.evidence.owner.source_instance_key)
                    .and_modify(|generation: &mut u64| {
                        *generation = (*generation).max(retracted.evidence.owner.generation);
                    })
                    .or_insert(retracted.evidence.owner.generation);
            }
            let expected_generations = generations
                .into_iter()
                .map(
                    |(source_instance_key, max_generation)| CatalogTombstoneSourceGeneration {
                        source_instance_key,
                        max_generation,
                    },
                )
                .collect::<Vec<_>>();
            let expected_provenance = sorted_provenance(history.values().flat_map(|retracted| {
                retracted
                    .provenance
                    .iter()
                    .chain(retracted.evidence.provenance.iter())
                    .copied()
            }));
            let newest_retraction = history
                .values()
                .map(|retracted| retracted.retracted_at_commit)
                .max()
                .expect("retracted tombstone history is nonempty");
            if tombstone.prior_assertion_keys != expected_keys
                || tombstone.absence_evidence != expected_evidence
                || tombstone.prior_source_generations != expected_generations
                || tombstone.provenance != expected_provenance
                || tombstone.confirmed_absent_at_commit <= newest_retraction
            {
                return Err(CatalogContractError::invalid(
                    "durable catalog tombstone is not exactly derived from retracted membership history",
                ));
            }
            self.reducer
                .tombstones
                .insert(tombstone.entity_ref, tombstone);
        }
        let publication = self.reducer.freeze_for_initial_publication(limits)?;
        if publication.revision() != expected_revision {
            return Err(CatalogContractError::invalid(
                "durable catalog reducer does not reproduce its declared revision",
            ));
        }
        Ok(publication)
    }
}

impl CatalogReducerPublication {
    /// Canonical set of exact source-object generations that still own live
    /// reducer evidence. Refresh reconciliation uses this only with complete
    /// source-coverage absences; the values contain no native paths.
    pub(crate) fn live_owners(&self) -> BTreeSet<CatalogEvidenceOwner> {
        self.projects
            .iter()
            .map(|stored| stored.fact.owner.clone())
            .chain(self.sessions.iter().map(|stored| stored.fact.owner.clone()))
            .chain(
                self.associations
                    .iter()
                    .map(|stored| stored.fact.owner.clone()),
            )
            .chain(self.locators.iter().map(|stored| stored.fact.owner.clone()))
            .chain(
                self.identity_relations
                    .iter()
                    .map(|stored| stored.fact.owner.clone()),
            )
            .collect()
    }

    pub(crate) fn validate_durable_row_commitments(
        &self,
        max_row_bytes: usize,
        expected_project_row_count: usize,
        expected_session_row_count: usize,
        mut matches: impl FnMut(
            CatalogEntityKind,
            &[u8; DIGEST_BYTES],
            usize,
            &[u8; DIGEST_BYTES],
        ) -> bool,
    ) -> Result<(), CatalogContractError> {
        if self.project_rows.len() != expected_project_row_count
            || self.session_rows.len() != expected_session_row_count
        {
            return Err(CatalogContractError::invalid(
                "durable catalog row counts differ from the reconstructed reducer",
            ));
        }
        for row in &self.project_rows {
            row.validate_for_durable()?;
            let payload = serialize_private_json_bounded(
                row,
                max_row_bytes,
                "restart-validated catalog project row",
            )?;
            let digest = *blake3::hash(&payload).as_bytes();
            if !matches(
                CatalogEntityKind::Project,
                row.project_ref.external_ref.entity_key.as_bytes(),
                payload.len(),
                &digest,
            ) {
                return Err(CatalogContractError::invalid(
                    "durable catalog project rows differ from the reconstructed reducer",
                ));
            }
        }
        for row in &self.session_rows {
            row.validate_for_durable()?;
            let payload = serialize_private_json_bounded(
                row,
                max_row_bytes,
                "restart-validated catalog session row",
            )?;
            let digest = *blake3::hash(&payload).as_bytes();
            if !matches(
                CatalogEntityKind::Session,
                row.session_ref.external_ref.entity_key.as_bytes(),
                payload.len(),
                &digest,
            ) {
                return Err(CatalogContractError::invalid(
                    "durable catalog session rows differ from the reconstructed reducer",
                ));
            }
        }
        Ok(())
    }

    pub(crate) fn resolution_index(&self) -> Result<CatalogResolutionIndex, CatalogContractError> {
        let live = self
            .project_rows
            .iter()
            .map(|row| row.project_ref)
            .chain(self.session_rows.iter().map(|row| row.session_ref))
            .collect::<BTreeSet<_>>();
        let tombstones = self
            .tombstones
            .iter()
            .map(|tombstone| (tombstone.entity_ref, tombstone.provenance.clone()))
            .collect::<BTreeMap<_, _>>();
        let mut replacements: BTreeMap<
            CatalogEntityRef,
            BTreeMap<CatalogEntityRef, Vec<SemanticRevisionRef>>,
        > = BTreeMap::new();
        let mut related = BTreeSet::new();
        for stored in &self.identity_relations {
            if let Some((prior_ref, replacement_ref)) = stored.fact.replacement_edge() {
                replacements
                    .entry(prior_ref)
                    .or_default()
                    .entry(replacement_ref)
                    .or_default()
                    .extend(stored.fact.provenance.iter().copied());
            } else if matches!(
                stored.fact.relation,
                IdentityRelationKind::Alias | IdentityRelationKind::SameEntity
            ) {
                related.insert(stored.fact.left_ref);
                related.insert(stored.fact.right_ref);
            }
        }

        let mut entries = BTreeMap::new();
        for (entity_key, kind) in &self.entity_kinds {
            let entity_ref = CatalogEntityRef {
                kind: *kind,
                external_ref: ExternalEntityRef::new(*entity_key),
            };
            let lifecycle = if let Some(targets) = replacements.get(&entity_ref) {
                let replacement_refs = targets.keys().copied().collect::<Vec<_>>();
                let provenance = sorted_provenance(
                    targets
                        .values()
                        .flat_map(|revisions| revisions.iter().copied()),
                );
                validate_provenance("catalog supersession resolution", &provenance)?;
                CatalogResolvedLifecycle::Superseded {
                    prior_ref: entity_ref,
                    replacement_refs,
                    provenance,
                }
            } else if live.contains(&entity_ref) {
                CatalogResolvedLifecycle::Live { entity_ref }
            } else if let Some(provenance) = tombstones.get(&entity_ref) {
                CatalogResolvedLifecycle::Tombstoned {
                    entity_ref,
                    provenance: provenance.clone(),
                }
            } else {
                CatalogResolvedLifecycle::Unknown {
                    requested_ref: entity_ref.external_ref,
                    reason: if related.contains(&entity_ref) {
                        CatalogUnknownReferenceReason::RelatedIdentityOnly
                    } else if self.retracted_assertions.contains_key(&entity_ref) {
                        CatalogUnknownReferenceReason::RetractedPendingPublication
                    } else {
                        CatalogUnknownReferenceReason::NeverObserved
                    },
                }
            };
            entries.insert(*entity_key, lifecycle);
        }
        Ok(CatalogResolutionIndex { entries })
    }

    /// Rehydrate the exact checked reducer state behind one immutable
    /// publication so an ordinary refresh can apply observations to that
    /// state rather than rebuilding a fresh, retargetable reducer.
    pub(crate) fn resume_for_refresh(&self) -> CatalogReducer {
        CatalogReducer {
            projects: self
                .projects
                .iter()
                .map(|stored| {
                    (
                        stored.fact.assertion_key,
                        StoredProjectAssertion {
                            fact: stored.fact.clone(),
                            observation_commit: stored.observation_commit,
                        },
                    )
                })
                .collect(),
            sessions: self
                .sessions
                .iter()
                .map(|stored| {
                    (
                        stored.fact.assertion_key,
                        StoredSessionAssertion {
                            fact: stored.fact.clone(),
                            observation_commit: stored.observation_commit,
                        },
                    )
                })
                .collect(),
            associations: self
                .associations
                .iter()
                .map(|stored| {
                    (
                        stored.fact.association_key,
                        StoredAssociation {
                            fact: stored.fact.clone(),
                            observation_commit: stored.observation_commit,
                        },
                    )
                })
                .collect(),
            locators: self
                .locators
                .iter()
                .map(|stored| {
                    (
                        stored.fact.locator_claim_key,
                        StoredLocatorClaim {
                            fact: stored.fact.clone(),
                            observation_commit: stored.observation_commit,
                        },
                    )
                })
                .collect(),
            identity_relations: self
                .identity_relations
                .iter()
                .map(|stored| {
                    (
                        stored.fact.relation_key,
                        StoredIdentityRelation {
                            fact: stored.fact.clone(),
                            observation_commit: stored.observation_commit,
                        },
                    )
                })
                .collect(),
            assertion_history: self.assertion_history.clone(),
            association_history: self.association_history.clone(),
            locator_history: self.locator_history.clone(),
            identity_relation_history: self.identity_relation_history.clone(),
            retracted_owners: self
                .retracted_owners
                .iter()
                .cloned()
                .map(|evidence| (evidence.owner.clone(), evidence))
                .collect(),
            entity_kinds: self.entity_kinds.clone(),
            retracted_assertions: self.retracted_assertions.clone(),
            tombstones: self
                .tombstones
                .iter()
                .cloned()
                .map(|tombstone| (tombstone.entity_ref, tombstone))
                .collect(),
        }
    }

    /// Validate that a newly frozen reducer is a monotonic successor of this
    /// publication. Immutable evidence coordinates and retraction history can
    /// only grow, and a previously live fact may disappear only after its
    /// exact owner generation has acquired retained retraction evidence.
    pub(crate) fn validate_refresh_successor(
        &self,
        next: &Self,
    ) -> Result<(), CatalogContractError> {
        fn require_history_subset<K: Ord + fmt::Debug, V: PartialEq>(
            prior: &BTreeMap<K, V>,
            next: &BTreeMap<K, V>,
            label: &'static str,
        ) -> Result<(), CatalogContractError> {
            if prior
                .iter()
                .any(|(key, value)| next.get(key) != Some(value))
            {
                return Err(CatalogContractError::invalid(format!(
                    "catalog refresh cannot remove or retarget {label} history"
                )));
            }
            Ok(())
        }

        require_history_subset(
            &self.assertion_history,
            &next.assertion_history,
            "assertion-coordinate",
        )?;
        require_history_subset(
            &self.association_history,
            &next.association_history,
            "association-coordinate",
        )?;
        require_history_subset(
            &self.locator_history,
            &next.locator_history,
            "locator-coordinate",
        )?;
        require_history_subset(
            &self.identity_relation_history,
            &next.identity_relation_history,
            "identity-relation-coordinate",
        )?;
        require_history_subset(&self.entity_kinds, &next.entity_kinds, "entity-kind")?;
        for (entity_ref, assertions) in &self.retracted_assertions {
            let next_assertions = next.retracted_assertions.get(entity_ref).ok_or_else(|| {
                CatalogContractError::invalid(
                    "catalog refresh cannot discard retracted assertion history",
                )
            })?;
            require_history_subset(assertions, next_assertions, "retracted-assertion")?;
        }
        let next_retractions = next
            .retracted_owners
            .iter()
            .map(|evidence| (&evidence.owner, evidence))
            .collect::<BTreeMap<_, _>>();
        if self
            .retracted_owners
            .iter()
            .any(|evidence| next_retractions.get(&evidence.owner) != Some(&evidence))
        {
            return Err(CatalogContractError::invalid(
                "catalog refresh cannot discard or rewrite owner retraction history",
            ));
        }

        macro_rules! require_live_successor {
            ($prior:expr, $next:expr, $key:expr, $owner:expr, $label:literal) => {
                for stored in $prior {
                    match $next
                        .iter()
                        .find(|candidate| $key(candidate) == $key(stored))
                    {
                        Some(candidate)
                            if candidate.observation_commit < stored.observation_commit =>
                        {
                            return Err(CatalogContractError::invalid(concat!(
                                "catalog refresh cannot move a live ",
                                $label,
                                " observation commit backwards"
                            )));
                        }
                        Some(candidate)
                            if candidate.observation_commit == stored.observation_commit
                                && candidate.fact != stored.fact =>
                        {
                            return Err(CatalogContractError::invalid(concat!(
                                "catalog refresh cannot rewrite a live ",
                                $label,
                                " at the same observation commit"
                            )));
                        }
                        Some(_) => {}
                        None if next_retractions.contains_key(&$owner(stored)) => {}
                        None => {
                            return Err(CatalogContractError::invalid(concat!(
                                "catalog refresh cannot remove a live ",
                                $label,
                                " without exact owner retraction evidence"
                            )));
                        }
                    }
                }
            };
        }
        require_live_successor!(
            &self.projects,
            &next.projects,
            |stored: &FrozenProjectAssertion| stored.fact.assertion_key,
            |stored: &FrozenProjectAssertion| stored.fact.owner.clone(),
            "project assertion"
        );
        require_live_successor!(
            &self.sessions,
            &next.sessions,
            |stored: &FrozenSessionAssertion| stored.fact.assertion_key,
            |stored: &FrozenSessionAssertion| stored.fact.owner.clone(),
            "session assertion"
        );
        require_live_successor!(
            &self.associations,
            &next.associations,
            |stored: &FrozenAssociation| stored.fact.association_key,
            |stored: &FrozenAssociation| stored.fact.owner.clone(),
            "association"
        );
        require_live_successor!(
            &self.locators,
            &next.locators,
            |stored: &FrozenLocatorClaim| stored.fact.locator_claim_key,
            |stored: &FrozenLocatorClaim| stored.fact.owner.clone(),
            "locator"
        );
        require_live_successor!(
            &self.identity_relations,
            &next.identity_relations,
            |stored: &FrozenIdentityRelation| stored.fact.relation_key,
            |stored: &FrozenIdentityRelation| stored.fact.owner.clone(),
            "identity relation"
        );

        for prior in &self.tombstones {
            match next
                .tombstones
                .iter()
                .find(|candidate| candidate.entity_ref == prior.entity_ref)
            {
                Some(candidate) => {
                    let assertion_history_retained = prior
                        .prior_assertion_keys
                        .iter()
                        .all(|key| candidate.prior_assertion_keys.binary_search(key).is_ok());
                    let absence_history_retained = prior.absence_evidence.iter().all(|evidence| {
                        candidate
                            .absence_evidence
                            .iter()
                            .any(|candidate| candidate == evidence)
                    });
                    let source_history_retained =
                        prior.prior_source_generations.iter().all(|source| {
                            candidate
                                .prior_source_generations
                                .iter()
                                .any(|next_source| {
                                    next_source.source_instance_key == source.source_instance_key
                                        && next_source.max_generation >= source.max_generation
                                })
                        });
                    let provenance_retained = prior.provenance.iter().all(|revision| {
                        candidate
                            .provenance
                            .binary_search_by_key(&revision.fact_revision_id, |candidate| {
                                candidate.fact_revision_id
                            })
                            .is_ok()
                    });
                    if candidate.confirmed_absent_at_commit < prior.confirmed_absent_at_commit
                        || !assertion_history_retained
                        || !absence_history_retained
                        || !source_history_retained
                        || !provenance_retained
                    {
                        return Err(CatalogContractError::invalid(
                            "catalog refresh cannot regress or rewrite retained tombstone evidence",
                        ));
                    }
                }
                None => {
                    let revived = next
                        .projects
                        .iter()
                        .map(|stored| {
                            (
                                stored.fact.project_ref,
                                &stored.fact.owner,
                                stored.observation_commit,
                            )
                        })
                        .chain(next.sessions.iter().map(|stored| {
                            (
                                stored.fact.session_ref,
                                &stored.fact.owner,
                                stored.observation_commit,
                            )
                        }))
                        .any(|(entity_ref, owner, observation_commit)| {
                            entity_ref == prior.entity_ref
                                && observation_commit > prior.confirmed_absent_at_commit
                                && prior.prior_source_generations.iter().any(|source| {
                                    source.source_instance_key == owner.source_instance_key
                                        && owner.generation > source.max_generation
                                })
                        });
                    if !revived {
                        return Err(CatalogContractError::invalid(
                            "catalog refresh cannot omit a tombstone without an exact newer-generation live revival",
                        ));
                    }
                }
            }
        }
        Ok(())
    }
}

impl fmt::Debug for CatalogReducerPublication {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CatalogReducerPublication")
            .field("revision", &self.revision)
            .field("project_assertions", &self.projects.len())
            .field("session_assertions", &self.sessions.len())
            .field("associations", &self.associations.len())
            .field("locators", &self.locators.len())
            .field("identity_relations", &self.identity_relations.len())
            .field("project_rows", &self.project_rows.len())
            .field("session_rows", &self.session_rows.len())
            .field("tombstones", &self.tombstones.len())
            .field("retracted_owners", &self.retracted_owners.len())
            .field("assertion_history", &self.assertion_history.len())
            .field("association_history", &self.association_history.len())
            .field("locator_history", &self.locator_history.len())
            .field(
                "identity_relation_history",
                &self.identity_relation_history.len(),
            )
            .field("entity_kinds", &self.entity_kinds.len())
            .field("retracted_entities", &self.retracted_assertions.len())
            .finish()
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct CatalogReducer {
    projects: BTreeMap<CatalogAssertionKey, StoredProjectAssertion>,
    sessions: BTreeMap<CatalogAssertionKey, StoredSessionAssertion>,
    associations: BTreeMap<CatalogAssociationKey, StoredAssociation>,
    locators: BTreeMap<CatalogLocatorClaimKey, StoredLocatorClaim>,
    identity_relations: BTreeMap<CatalogIdentityRelationKey, StoredIdentityRelation>,
    assertion_history: BTreeMap<CatalogAssertionKey, AssertionCoordinates>,
    association_history: BTreeMap<CatalogAssociationKey, AssociationCoordinates>,
    locator_history: BTreeMap<CatalogLocatorClaimKey, LocatorCoordinates>,
    identity_relation_history: BTreeMap<CatalogIdentityRelationKey, IdentityRelationCoordinates>,
    retracted_owners: BTreeMap<CatalogEvidenceOwner, CatalogRetractionEvidence>,
    entity_kinds: BTreeMap<CanonicalEntityKey, CatalogEntityKind>,
    retracted_assertions:
        BTreeMap<CatalogEntityRef, BTreeMap<CatalogAssertionKey, RetractedAssertion>>,
    tombstones: BTreeMap<CatalogEntityRef, CatalogTombstone>,
}

fn validate_observation_commit(observation_commit: u64) -> Result<(), CatalogContractError> {
    if observation_commit == 0 {
        return Err(CatalogContractError::invalid(
            "catalog evidence observation commit must be greater than zero",
        ));
    }
    Ok(())
}

fn sorted_provenance(
    revisions: impl IntoIterator<Item = SemanticRevisionRef>,
) -> Vec<SemanticRevisionRef> {
    let mut by_revision = BTreeMap::new();
    for revision in revisions {
        by_revision.insert(revision.fact_revision_id, revision);
    }
    by_revision.into_values().collect()
}

fn compare_associations(left: &StoredAssociation, right: &StoredAssociation) -> Ordering {
    left.fact
        .authority
        .precedence
        .cmp(&right.fact.authority.precedence)
        .then_with(|| quality_rank(left.fact.quality).cmp(&quality_rank(right.fact.quality)))
        .then_with(|| {
            if left.fact.authority.native_times_comparable
                && right.fact.authority.native_times_comparable
            {
                left.fact.effective_at.cmp(&right.fact.effective_at)
            } else {
                Ordering::Equal
            }
        })
        .then_with(|| left.observation_commit.cmp(&right.observation_commit))
        .then_with(|| right.fact.association_key.cmp(&left.fact.association_key))
}

fn project_field_candidate<'a, T>(
    stored: &'a StoredProjectAssertion,
    field: &'a CatalogQualifiedField<T>,
) -> FieldCandidate<'a, T> {
    FieldCandidate {
        assertion_key: stored.fact.assertion_key,
        observation_commit: stored.observation_commit,
        field,
    }
}

fn session_field_candidate<'a, T>(
    stored: &'a StoredSessionAssertion,
    field: &'a CatalogQualifiedField<T>,
) -> FieldCandidate<'a, T> {
    FieldCandidate {
        assertion_key: stored.fact.assertion_key,
        observation_commit: stored.observation_commit,
        field,
    }
}

impl CatalogReducer {
    fn ensure_entity_kind(&self, entity_ref: CatalogEntityRef) -> Result<(), CatalogContractError> {
        if let Some(existing_kind) = self.entity_kinds.get(&entity_ref.external_ref.entity_key) {
            if *existing_kind != entity_ref.kind {
                return Err(CatalogContractError::invalid(
                    "one external entity key cannot be strengthened into two catalog entity kinds",
                ));
            }
        }
        Ok(())
    }

    fn register_entity_kind(&mut self, entity_ref: CatalogEntityRef) {
        self.entity_kinds
            .entry(entity_ref.external_ref.entity_key)
            .or_insert(entity_ref.kind);
    }

    fn validate_tombstone_revival(
        &self,
        entity_ref: CatalogEntityRef,
        owner: &CatalogEvidenceOwner,
        observation_commit: u64,
    ) -> Result<(), CatalogContractError> {
        let Some(tombstone) = self.tombstones.get(&entity_ref) else {
            return Ok(());
        };
        if tombstone.confirmed_absent_at_commit >= observation_commit {
            return Err(CatalogContractError::invalid(
                "stale catalog evidence cannot resurrect a confirmed-absent entity",
            ));
        }
        let Some(prior_source) = tombstone
            .prior_source_generations
            .iter()
            .find(|prior| prior.source_instance_key == owner.source_instance_key)
        else {
            return Err(CatalogContractError::invalid(
                "catalog tombstone revival requires a previously evidenced source identity",
            ));
        };
        if owner.generation <= prior_source.max_generation {
            return Err(CatalogContractError::invalid(
                "catalog tombstone revival requires an explicitly newer source generation",
            ));
        }
        Ok(())
    }

    fn validate_retracted_revival(
        &self,
        entity_ref: CatalogEntityRef,
        owner: &CatalogEvidenceOwner,
    ) -> Result<(), CatalogContractError> {
        let newest_retracted_generation = self
            .retracted_assertions
            .get(&entity_ref)
            .into_iter()
            .flat_map(|history| history.values())
            .filter(|retracted| {
                retracted.evidence.owner.source_instance_key == owner.source_instance_key
            })
            .map(|retracted| retracted.evidence.owner.generation)
            .max();
        if newest_retracted_generation.is_some_and(|generation| owner.generation <= generation) {
            return Err(CatalogContractError::invalid(
                "confirmed retraction revival requires an explicitly newer source generation",
            ));
        }
        Ok(())
    }

    fn clear_validated_tombstone(&mut self, entity_ref: CatalogEntityRef) {
        self.tombstones.remove(&entity_ref);
    }

    fn ensure_assertion_coordinates(
        &self,
        key: CatalogAssertionKey,
        coordinates: &AssertionCoordinates,
    ) -> Result<(), CatalogContractError> {
        if self
            .assertion_history
            .get(&key)
            .is_some_and(|existing| existing != coordinates)
        {
            return Err(CatalogContractError::invalid(
                "a retracted catalog assertion key cannot retarget its owner or entity",
            ));
        }
        Ok(())
    }

    fn ensure_association_coordinates(
        &self,
        key: CatalogAssociationKey,
        coordinates: &AssociationCoordinates,
    ) -> Result<(), CatalogContractError> {
        if self
            .association_history
            .get(&key)
            .is_some_and(|existing| existing != coordinates)
        {
            return Err(CatalogContractError::invalid(
                "a retracted catalog association key cannot retarget its owner or endpoints",
            ));
        }
        Ok(())
    }

    fn ensure_locator_coordinates(
        &self,
        key: CatalogLocatorClaimKey,
        coordinates: &LocatorCoordinates,
    ) -> Result<(), CatalogContractError> {
        if self
            .locator_history
            .get(&key)
            .is_some_and(|existing| existing != coordinates)
        {
            return Err(CatalogContractError::invalid(
                "a retracted catalog locator key cannot retarget its owner or subject",
            ));
        }
        Ok(())
    }

    fn ensure_identity_relation_coordinates(
        &self,
        key: CatalogIdentityRelationKey,
        coordinates: &IdentityRelationCoordinates,
    ) -> Result<(), CatalogContractError> {
        if self
            .identity_relation_history
            .get(&key)
            .is_some_and(|existing| existing != coordinates)
        {
            return Err(CatalogContractError::invalid(
                "a retracted identity-relation key cannot retarget its owner, endpoints, or kind",
            ));
        }
        Ok(())
    }

    fn ensure_owner_generation_not_retracted(
        &self,
        owner: &CatalogEvidenceOwner,
    ) -> Result<(), CatalogContractError> {
        if self.retracted_owners.contains_key(owner) {
            return Err(CatalogContractError::invalid(
                "confirmed catalog owner retraction requires a newer source generation",
            ));
        }
        Ok(())
    }

    fn entity_has_assertion(&self, entity_ref: CatalogEntityRef) -> bool {
        match entity_ref.kind {
            CatalogEntityKind::Project => self
                .projects
                .values()
                .any(|stored| stored.fact.project_ref == entity_ref),
            CatalogEntityKind::Session => self
                .sessions
                .values()
                .any(|stored| stored.fact.session_ref == entity_ref),
        }
    }

    pub(crate) fn upsert_project_assertion(
        &mut self,
        fact: CatalogProjectAssertion,
        observation_commit: u64,
    ) -> Result<CatalogMutation, CatalogContractError> {
        fact.validate()?;
        validate_observation_commit(observation_commit)?;
        self.ensure_owner_generation_not_retracted(&fact.owner)?;
        self.ensure_entity_kind(fact.project_ref)?;
        let coordinates = AssertionCoordinates {
            owner: fact.owner.clone(),
            entity_ref: fact.project_ref,
        };
        self.ensure_assertion_coordinates(fact.assertion_key, &coordinates)?;
        self.validate_retracted_revival(fact.project_ref, &fact.owner)?;
        self.validate_tombstone_revival(fact.project_ref, &fact.owner, observation_commit)?;

        let mutation = match self.projects.get(&fact.assertion_key) {
            Some(existing) if existing.fact == fact => {
                if observation_commit <= existing.observation_commit {
                    return Ok(CatalogMutation::Noop);
                }
                CatalogMutation::Updated
            }
            Some(existing) => {
                if existing.fact.owner != fact.owner
                    || existing.fact.project_ref != fact.project_ref
                {
                    return Err(CatalogContractError::invalid(
                        "a catalog project assertion key cannot retarget its owner or entity",
                    ));
                }
                if observation_commit <= existing.observation_commit {
                    return Err(CatalogContractError::invalid(
                        "changed catalog project evidence requires a newer observation commit",
                    ));
                }
                CatalogMutation::Updated
            }
            None => CatalogMutation::Inserted,
        };

        let assertion_key = fact.assertion_key;
        let project_ref = fact.project_ref;
        self.projects.insert(
            assertion_key,
            StoredProjectAssertion {
                fact,
                observation_commit,
            },
        );
        self.assertion_history
            .entry(assertion_key)
            .or_insert(coordinates);
        self.register_entity_kind(project_ref);
        self.clear_validated_tombstone(project_ref);
        Ok(mutation)
    }

    pub(crate) fn upsert_session_assertion(
        &mut self,
        fact: CatalogSessionAssertion,
        observation_commit: u64,
    ) -> Result<CatalogMutation, CatalogContractError> {
        fact.validate()?;
        validate_observation_commit(observation_commit)?;
        self.ensure_owner_generation_not_retracted(&fact.owner)?;
        self.ensure_entity_kind(fact.session_ref)?;
        let coordinates = AssertionCoordinates {
            owner: fact.owner.clone(),
            entity_ref: fact.session_ref,
        };
        self.ensure_assertion_coordinates(fact.assertion_key, &coordinates)?;
        self.validate_retracted_revival(fact.session_ref, &fact.owner)?;
        self.validate_tombstone_revival(fact.session_ref, &fact.owner, observation_commit)?;

        let mutation = match self.sessions.get(&fact.assertion_key) {
            Some(existing) if existing.fact == fact => {
                if observation_commit <= existing.observation_commit {
                    return Ok(CatalogMutation::Noop);
                }
                CatalogMutation::Updated
            }
            Some(existing) => {
                if existing.fact.owner != fact.owner
                    || existing.fact.session_ref != fact.session_ref
                {
                    return Err(CatalogContractError::invalid(
                        "a catalog session assertion key cannot retarget its owner or entity",
                    ));
                }
                if observation_commit <= existing.observation_commit {
                    return Err(CatalogContractError::invalid(
                        "changed catalog session evidence requires a newer observation commit",
                    ));
                }
                CatalogMutation::Updated
            }
            None => CatalogMutation::Inserted,
        };

        let assertion_key = fact.assertion_key;
        let session_ref = fact.session_ref;
        self.sessions.insert(
            assertion_key,
            StoredSessionAssertion {
                fact,
                observation_commit,
            },
        );
        self.assertion_history
            .entry(assertion_key)
            .or_insert(coordinates);
        self.register_entity_kind(session_ref);
        self.clear_validated_tombstone(session_ref);
        Ok(mutation)
    }

    pub(crate) fn upsert_association(
        &mut self,
        fact: SessionProjectAssociationFact,
        observation_commit: u64,
    ) -> Result<CatalogMutation, CatalogContractError> {
        fact.validate()?;
        validate_observation_commit(observation_commit)?;
        self.ensure_owner_generation_not_retracted(&fact.owner)?;
        self.ensure_entity_kind(fact.session_ref)?;
        self.ensure_entity_kind(fact.project_ref)?;
        let coordinates = AssociationCoordinates {
            owner: fact.owner.clone(),
            session_ref: fact.session_ref,
            project_ref: fact.project_ref,
        };
        self.ensure_association_coordinates(fact.association_key, &coordinates)?;

        let mutation = match self.associations.get(&fact.association_key) {
            Some(existing) if existing.fact == fact => {
                if observation_commit <= existing.observation_commit {
                    return Ok(CatalogMutation::Noop);
                }
                CatalogMutation::Updated
            }
            Some(existing) => {
                if existing.fact.owner != fact.owner
                    || existing.fact.session_ref != fact.session_ref
                    || existing.fact.project_ref != fact.project_ref
                {
                    return Err(CatalogContractError::invalid(
                        "a catalog association key cannot retarget its owner or endpoints",
                    ));
                }
                if observation_commit <= existing.observation_commit {
                    return Err(CatalogContractError::invalid(
                        "changed catalog association evidence requires a newer observation commit",
                    ));
                }
                CatalogMutation::Updated
            }
            None => CatalogMutation::Inserted,
        };

        let association_key = fact.association_key;
        let session_ref = fact.session_ref;
        let project_ref = fact.project_ref;
        self.associations.insert(
            association_key,
            StoredAssociation {
                fact,
                observation_commit,
            },
        );
        self.association_history
            .entry(association_key)
            .or_insert(coordinates);
        self.register_entity_kind(session_ref);
        self.register_entity_kind(project_ref);
        Ok(mutation)
    }

    pub(crate) fn upsert_locator_claim(
        &mut self,
        fact: NativeLocatorClaim,
        observation_commit: u64,
    ) -> Result<CatalogMutation, CatalogContractError> {
        fact.validate()?;
        validate_observation_commit(observation_commit)?;
        self.ensure_owner_generation_not_retracted(&fact.owner)?;
        self.ensure_entity_kind(fact.subject_ref)?;
        let coordinates = LocatorCoordinates {
            owner: fact.owner.clone(),
            subject_ref: fact.subject_ref,
        };
        self.ensure_locator_coordinates(fact.locator_claim_key, &coordinates)?;

        let mutation = match self.locators.get(&fact.locator_claim_key) {
            Some(existing) if existing.fact == fact => {
                if observation_commit <= existing.observation_commit {
                    return Ok(CatalogMutation::Noop);
                }
                CatalogMutation::Updated
            }
            Some(existing) => {
                if existing.fact.owner != fact.owner
                    || existing.fact.subject_ref != fact.subject_ref
                {
                    return Err(CatalogContractError::invalid(
                        "a catalog locator key cannot retarget its owner or subject",
                    ));
                }
                if observation_commit <= existing.observation_commit {
                    return Err(CatalogContractError::invalid(
                        "changed catalog locator evidence requires a newer observation commit",
                    ));
                }
                CatalogMutation::Updated
            }
            None => CatalogMutation::Inserted,
        };

        let locator_claim_key = fact.locator_claim_key;
        let subject_ref = fact.subject_ref;
        self.locators.insert(
            locator_claim_key,
            StoredLocatorClaim {
                fact,
                observation_commit,
            },
        );
        self.locator_history
            .entry(locator_claim_key)
            .or_insert(coordinates);
        self.register_entity_kind(subject_ref);
        Ok(mutation)
    }

    pub(crate) fn upsert_identity_relation(
        &mut self,
        fact: IdentityRelationFact,
        observation_commit: u64,
    ) -> Result<CatalogMutation, CatalogContractError> {
        fact.validate()?;
        validate_observation_commit(observation_commit)?;
        self.ensure_owner_generation_not_retracted(&fact.owner)?;
        self.ensure_entity_kind(fact.left_ref)?;
        self.ensure_entity_kind(fact.right_ref)?;
        let coordinates = IdentityRelationCoordinates {
            owner: fact.owner.clone(),
            relation: fact.relation,
            left_ref: fact.left_ref,
            right_ref: fact.right_ref,
        };
        self.ensure_identity_relation_coordinates(fact.relation_key, &coordinates)?;

        let mutation = match self.identity_relations.get(&fact.relation_key) {
            Some(existing) if existing.fact == fact => {
                if observation_commit <= existing.observation_commit {
                    return Ok(CatalogMutation::Noop);
                }
                CatalogMutation::Updated
            }
            Some(existing) => {
                if existing.fact.owner != fact.owner
                    || existing.fact.left_ref != fact.left_ref
                    || existing.fact.right_ref != fact.right_ref
                    || existing.fact.relation != fact.relation
                {
                    return Err(CatalogContractError::invalid(
                        "an identity-relation key cannot retarget its owner, endpoints, or kind",
                    ));
                }
                if observation_commit <= existing.observation_commit {
                    return Err(CatalogContractError::invalid(
                        "changed identity-relation evidence requires a newer observation commit",
                    ));
                }
                CatalogMutation::Updated
            }
            None => CatalogMutation::Inserted,
        };

        let mut candidate_relations = self.identity_relations.clone();
        candidate_relations.insert(
            fact.relation_key,
            StoredIdentityRelation {
                fact: fact.clone(),
                observation_commit,
            },
        );
        validate_identity_relation_graph(&candidate_relations)?;

        self.identity_relations = candidate_relations;
        self.identity_relation_history
            .entry(fact.relation_key)
            .or_insert(coordinates);
        self.register_entity_kind(fact.left_ref);
        self.register_entity_kind(fact.right_ref);
        Ok(mutation)
    }

    pub(crate) fn project_row(&self, project_ref: CatalogEntityRef) -> Option<CatalogProjectRow> {
        if project_ref.kind != CatalogEntityKind::Project {
            return None;
        }
        let assertions: Vec<_> = self
            .projects
            .values()
            .filter(|stored| stored.fact.project_ref == project_ref)
            .collect();
        if assertions.is_empty() {
            return None;
        }
        Some(materialize_project_row(project_ref, &assertions))
    }

    pub(crate) fn session_row(&self, session_ref: CatalogEntityRef) -> Option<CatalogSessionRow> {
        if session_ref.kind != CatalogEntityKind::Session {
            return None;
        }
        let assertions: Vec<_> = self
            .sessions
            .values()
            .filter(|stored| stored.fact.session_ref == session_ref)
            .collect();
        if assertions.is_empty() {
            return None;
        }
        let associations = self
            .associations
            .values()
            .filter(|stored| stored.fact.session_ref == session_ref)
            .collect::<Vec<_>>();
        Some(materialize_session_row(
            session_ref,
            &assertions,
            &associations,
        ))
    }

    pub(crate) fn association_for_session(
        &self,
        session_ref: CatalogEntityRef,
    ) -> CatalogAssociationCoverage {
        let candidates: Vec<_> = self
            .associations
            .values()
            .filter(|stored| stored.fact.session_ref == session_ref)
            .collect();
        materialize_association_coverage(&candidates)
    }

    /// Freeze every live reducer input and materialized row under explicit
    /// bounded ceilings. The returned value is not a wire DTO; it is the exact
    /// immutable semantic input to the atomic B3 publication framing step.
    pub(crate) fn freeze_for_initial_publication(
        &self,
        limits: CatalogReducerPublicationLimits,
    ) -> Result<CatalogReducerPublication, CatalogContractError> {
        CatalogReducerPublicationLimits::new(limits.max_reducer_entries, limits.max_rows)?;
        let retracted_assertion_count =
            self.retracted_assertions
                .values()
                .try_fold(0_usize, |count, assertions| {
                    count.checked_add(assertions.len()).ok_or_else(|| {
                        CatalogContractError::invalid(
                            "catalog retracted-assertion count overflow during publication freeze",
                        )
                    })
                })?;
        let base_reducer_entry_counts = [
            self.projects.len(),
            self.sessions.len(),
            self.associations.len(),
            self.locators.len(),
            self.identity_relations.len(),
            self.assertion_history.len(),
            self.association_history.len(),
            self.locator_history.len(),
            self.identity_relation_history.len(),
            self.retracted_owners.len(),
            self.entity_kinds.len(),
            retracted_assertion_count,
            self.tombstones.len(),
        ];
        let base_reducer_entry_count =
            base_reducer_entry_counts
                .into_iter()
                .try_fold(0_usize, |count, next| {
                    count.checked_add(next).ok_or_else(|| {
                        CatalogContractError::invalid(
                            "catalog reducer entry count overflow during publication freeze",
                        )
                    })
                })?;
        if base_reducer_entry_count > limits.max_reducer_entries {
            return Err(CatalogContractError::invalid(
                "catalog reducer publication exceeds its bounded evidence ceiling",
            ));
        }

        let mut project_assertions = BTreeMap::new();
        for stored in self.projects.values() {
            project_assertions
                .entry(stored.fact.project_ref)
                .or_insert_with(Vec::new)
                .push(stored);
        }
        let mut session_assertions = BTreeMap::new();
        for stored in self.sessions.values() {
            session_assertions
                .entry(stored.fact.session_ref)
                .or_insert_with(Vec::new)
                .push(stored);
        }
        let row_count = project_assertions
            .len()
            .checked_add(session_assertions.len())
            .ok_or_else(|| {
                CatalogContractError::invalid("catalog publication row count overflow")
            })?;
        if row_count > limits.max_rows {
            return Err(CatalogContractError::invalid(
                "catalog reducer publication exceeds its bounded row ceiling",
            ));
        }
        let reducer_entry_count =
            base_reducer_entry_count
                .checked_add(row_count)
                .ok_or_else(|| {
                    CatalogContractError::invalid(
                        "catalog reducer entry count overflow during publication freeze",
                    )
                })?;
        if reducer_entry_count > limits.max_reducer_entries {
            return Err(CatalogContractError::invalid(
                "catalog reducer publication exceeds its bounded evidence ceiling",
            ));
        }

        validate_publication_endpoints(self)?;
        validate_identity_relation_graph(&self.identity_relations)?;

        let mut associations_by_session = BTreeMap::new();
        for stored in self.associations.values() {
            associations_by_session
                .entry(stored.fact.session_ref)
                .or_insert_with(Vec::new)
                .push(stored);
        }
        let project_rows = project_assertions
            .into_iter()
            .map(|(project_ref, assertions)| materialize_project_row(project_ref, &assertions))
            .collect::<Vec<_>>();
        let session_rows = session_assertions
            .into_iter()
            .map(|(session_ref, assertions)| {
                let associations = associations_by_session
                    .remove(&session_ref)
                    .unwrap_or_default();
                materialize_session_row(session_ref, &assertions, &associations)
            })
            .collect::<Vec<_>>();

        let projects = self
            .projects
            .values()
            .map(|stored| FrozenProjectAssertion {
                fact: stored.fact.clone(),
                observation_commit: stored.observation_commit,
            })
            .collect();
        let sessions = self
            .sessions
            .values()
            .map(|stored| FrozenSessionAssertion {
                fact: stored.fact.clone(),
                observation_commit: stored.observation_commit,
            })
            .collect();
        let associations = self
            .associations
            .values()
            .map(|stored| FrozenAssociation {
                fact: stored.fact.clone(),
                observation_commit: stored.observation_commit,
            })
            .collect();
        let locators = self
            .locators
            .values()
            .map(|stored| FrozenLocatorClaim {
                fact: stored.fact.clone(),
                observation_commit: stored.observation_commit,
            })
            .collect();
        let identity_relations = self
            .identity_relations
            .values()
            .map(|stored| FrozenIdentityRelation {
                fact: stored.fact.clone(),
                observation_commit: stored.observation_commit,
            })
            .collect();
        let tombstones = self.tombstones.values().cloned().collect();
        let retracted_owners = self.retracted_owners.values().cloned().collect();
        let revision = derive_reducer_publication_revision(self, &project_rows, &session_rows)?;

        Ok(CatalogReducerPublication {
            projects,
            sessions,
            associations,
            locators,
            identity_relations,
            project_rows,
            session_rows,
            tombstones,
            retracted_owners,
            assertion_history: self.assertion_history.clone(),
            association_history: self.association_history.clone(),
            locator_history: self.locator_history.clone(),
            identity_relation_history: self.identity_relation_history.clone(),
            entity_kinds: self.entity_kinds.clone(),
            retracted_assertions: self.retracted_assertions.clone(),
            revision,
        })
    }

    pub(crate) fn retract_owner(
        &mut self,
        evidence: &CatalogRetractionEvidence,
        observation_commit: u64,
    ) -> Result<CatalogRetraction, CatalogContractError> {
        evidence.validate()?;
        validate_observation_commit(observation_commit)?;
        let owner = &evidence.owner;
        if let Some(existing) = self.retracted_owners.get(owner) {
            if existing != evidence {
                return Err(CatalogContractError::invalid(
                    "one catalog owner generation cannot acquire conflicting retraction evidence",
                ));
            }
            return Ok(CatalogRetraction {
                assertion_count: 0,
                association_count: 0,
                locator_count: 0,
                identity_relation_count: 0,
                orphaned_entities: Vec::new(),
            });
        }

        let project_keys: Vec<_> = self
            .projects
            .iter()
            .filter(|(_, stored)| &stored.fact.owner == owner)
            .map(|(key, _)| *key)
            .collect();
        let session_keys: Vec<_> = self
            .sessions
            .iter()
            .filter(|(_, stored)| &stored.fact.owner == owner)
            .map(|(key, _)| *key)
            .collect();
        let association_keys: Vec<_> = self
            .associations
            .iter()
            .filter(|(_, stored)| &stored.fact.owner == owner)
            .map(|(key, _)| *key)
            .collect();
        let locator_keys: Vec<_> = self
            .locators
            .iter()
            .filter(|(_, stored)| &stored.fact.owner == owner)
            .map(|(key, _)| *key)
            .collect();
        let relation_keys: Vec<_> = self
            .identity_relations
            .iter()
            .filter(|(_, stored)| &stored.fact.owner == owner)
            .map(|(key, _)| *key)
            .collect();

        let stale_retraction = project_keys.iter().any(|key| {
            self.projects
                .get(key)
                .is_some_and(|stored| stored.observation_commit >= observation_commit)
        }) || session_keys.iter().any(|key| {
            self.sessions
                .get(key)
                .is_some_and(|stored| stored.observation_commit >= observation_commit)
        }) || association_keys.iter().any(|key| {
            self.associations
                .get(key)
                .is_some_and(|stored| stored.observation_commit >= observation_commit)
        }) || locator_keys.iter().any(|key| {
            self.locators
                .get(key)
                .is_some_and(|stored| stored.observation_commit >= observation_commit)
        }) || relation_keys.iter().any(|key| {
            self.identity_relations
                .get(key)
                .is_some_and(|stored| stored.observation_commit >= observation_commit)
        });
        if stale_retraction {
            return Err(CatalogContractError::invalid(
                "catalog owner retraction must follow every owned evidence observation",
            ));
        }

        let mut affected_entities = BTreeSet::new();
        for key in &project_keys {
            let stored = self
                .projects
                .remove(key)
                .expect("project retraction key came from the same map");
            affected_entities.insert(stored.fact.project_ref);
            self.retracted_assertions
                .entry(stored.fact.project_ref)
                .or_default()
                .insert(
                    *key,
                    RetractedAssertion {
                        evidence: evidence.clone(),
                        retracted_at_commit: observation_commit,
                        provenance: stored.fact.provenance,
                    },
                );
        }
        for key in &session_keys {
            let stored = self
                .sessions
                .remove(key)
                .expect("session retraction key came from the same map");
            affected_entities.insert(stored.fact.session_ref);
            self.retracted_assertions
                .entry(stored.fact.session_ref)
                .or_default()
                .insert(
                    *key,
                    RetractedAssertion {
                        evidence: evidence.clone(),
                        retracted_at_commit: observation_commit,
                        provenance: stored.fact.provenance,
                    },
                );
        }
        for key in &association_keys {
            self.associations.remove(key);
        }
        for key in &locator_keys {
            self.locators.remove(key);
        }
        for key in &relation_keys {
            self.identity_relations.remove(key);
        }

        let orphaned_entities = affected_entities
            .into_iter()
            .filter(|entity_ref| !self.entity_has_assertion(*entity_ref))
            .collect();
        self.retracted_owners
            .insert(evidence.owner.clone(), evidence.clone());
        Ok(CatalogRetraction {
            assertion_count: project_keys.len() + session_keys.len(),
            association_count: association_keys.len(),
            locator_count: locator_keys.len(),
            identity_relation_count: relation_keys.len(),
            orphaned_entities,
        })
    }

    pub(crate) fn confirm_absent(
        &mut self,
        entity_ref: CatalogEntityRef,
        evidence: &CatalogRetractionEvidence,
        observation_commit: u64,
    ) -> Result<CatalogMutation, CatalogContractError> {
        entity_ref.validate()?;
        evidence.validate()?;
        validate_observation_commit(observation_commit)?;
        self.ensure_entity_kind(entity_ref)?;
        if self.entity_has_assertion(entity_ref) {
            return Err(CatalogContractError::invalid(
                "catalog absence cannot tombstone an entity with live membership evidence",
            ));
        }
        let history = self.retracted_assertions.get(&entity_ref).ok_or_else(|| {
            CatalogContractError::invalid(
                "catalog absence requires prior retracted membership evidence",
            )
        })?;
        let evidence_owns_retraction = history
            .values()
            .any(|retracted| retracted.evidence == *evidence);
        if !evidence_owns_retraction {
            return Err(CatalogContractError::invalid(
                "catalog absence evidence did not own the prior membership retraction",
            ));
        }
        let newest_retraction = history
            .values()
            .map(|retracted| retracted.retracted_at_commit)
            .max()
            .ok_or_else(|| {
                CatalogContractError::invalid(
                    "catalog absence requires prior retracted membership evidence",
                )
            })?;
        if observation_commit <= newest_retraction {
            return Err(CatalogContractError::invalid(
                "catalog absence confirmation must follow membership retraction",
            ));
        }

        let mutation = match self.tombstones.get(&entity_ref) {
            Some(existing) if existing.confirmed_absent_at_commit >= observation_commit => {
                return Ok(CatalogMutation::Noop);
            }
            Some(_) => CatalogMutation::Updated,
            None => CatalogMutation::Inserted,
        };
        let mut prior_assertion_keys: Vec<_> = history.keys().copied().collect();
        prior_assertion_keys.sort();
        let mut source_generations = BTreeMap::new();
        for retracted in history.values() {
            source_generations
                .entry(retracted.evidence.owner.source_instance_key)
                .and_modify(|generation: &mut u64| {
                    *generation = (*generation).max(retracted.evidence.owner.generation);
                })
                .or_insert(retracted.evidence.owner.generation);
        }
        let prior_source_generations = source_generations
            .into_iter()
            .map(
                |(source_instance_key, max_generation)| CatalogTombstoneSourceGeneration {
                    source_instance_key,
                    max_generation,
                },
            )
            .collect();
        let absence_evidence = history
            .values()
            .map(|retracted| (retracted.evidence.owner.clone(), retracted.evidence.clone()))
            .collect::<BTreeMap<_, _>>()
            .into_values()
            .collect();
        let provenance = sorted_provenance(
            history
                .values()
                .flat_map(|retracted| {
                    retracted
                        .provenance
                        .iter()
                        .chain(retracted.evidence.provenance.iter())
                })
                .copied(),
        );
        let tombstone = CatalogTombstone {
            entity_ref,
            absence_evidence,
            confirmed_absent_at_commit: observation_commit,
            prior_assertion_keys,
            prior_source_generations,
            provenance,
        };
        tombstone.validate_shape()?;
        self.tombstones.insert(entity_ref, tombstone);
        self.register_entity_kind(entity_ref);
        Ok(mutation)
    }

    pub(crate) fn resolve_external_ref(
        &self,
        external_ref: ExternalEntityRef,
    ) -> CatalogResolvedEntity {
        let Some(kind) = self.entity_kinds.get(&external_ref.entity_key).copied() else {
            return CatalogResolvedEntity::Unknown {
                requested_ref: external_ref,
                reason: CatalogUnknownReferenceReason::NeverObserved,
                related_refs: Vec::new(),
                relation_keys: Vec::new(),
            };
        };
        let entity_ref = CatalogEntityRef { kind, external_ref };

        let mut replacements: BTreeMap<CatalogEntityRef, Vec<CatalogIdentityRelationKey>> =
            BTreeMap::new();
        let mut replacement_provenance = Vec::new();
        for stored in self.identity_relations.values() {
            if let Some((prior_ref, replacement_ref)) = stored.fact.replacement_edge() {
                if prior_ref == entity_ref {
                    replacements
                        .entry(replacement_ref)
                        .or_default()
                        .push(stored.fact.relation_key);
                    replacement_provenance.extend(stored.fact.provenance.iter().copied());
                }
            }
        }
        if !replacements.is_empty() {
            let replacement_refs = replacements.keys().copied().collect();
            let mut relation_keys: Vec<_> = replacements.into_values().flatten().collect();
            relation_keys.sort();
            return CatalogResolvedEntity::Superseded {
                prior_ref: entity_ref,
                replacement_refs,
                relation_keys,
                provenance: sorted_provenance(replacement_provenance),
            };
        }

        let live_row = match entity_ref.kind {
            CatalogEntityKind::Project => self.project_row(entity_ref).map(CatalogLiveRow::Project),
            CatalogEntityKind::Session => self.session_row(entity_ref).map(CatalogLiveRow::Session),
        };
        if let Some(row) = live_row {
            return CatalogResolvedEntity::Live {
                entity_ref,
                row: Box::new(row),
            };
        }
        if let Some(tombstone) = self.tombstones.get(&entity_ref) {
            return CatalogResolvedEntity::Tombstoned {
                tombstone: tombstone.clone(),
            };
        }

        let mut related: BTreeMap<CatalogEntityRef, Vec<CatalogIdentityRelationKey>> =
            BTreeMap::new();
        for stored in self.identity_relations.values() {
            if !matches!(
                stored.fact.relation,
                IdentityRelationKind::Alias | IdentityRelationKind::SameEntity
            ) {
                continue;
            }
            let counterpart = if stored.fact.left_ref == entity_ref {
                Some(stored.fact.right_ref)
            } else if stored.fact.right_ref == entity_ref {
                Some(stored.fact.left_ref)
            } else {
                None
            };
            if let Some(counterpart) = counterpart {
                related
                    .entry(counterpart)
                    .or_default()
                    .push(stored.fact.relation_key);
            }
        }
        if related.is_empty() {
            CatalogResolvedEntity::Unknown {
                requested_ref: external_ref,
                reason: if self.retracted_assertions.contains_key(&entity_ref) {
                    CatalogUnknownReferenceReason::RetractedPendingPublication
                } else {
                    CatalogUnknownReferenceReason::NeverObserved
                },
                related_refs: Vec::new(),
                relation_keys: Vec::new(),
            }
        } else {
            let related_refs = related.keys().copied().collect();
            let mut relation_keys: Vec<_> = related.into_values().flatten().collect();
            relation_keys.sort();
            CatalogResolvedEntity::Unknown {
                requested_ref: external_ref,
                reason: CatalogUnknownReferenceReason::RelatedIdentityOnly,
                related_refs,
                relation_keys,
            }
        }
    }

    pub(crate) fn resolve_attach_target(
        &self,
        handoff: &CatalogSessionAttachHandoff,
        view: CatalogPolicyView,
    ) -> Result<CatalogAttachTarget, CatalogContractError> {
        let canonical_handoff = CatalogSessionAttachHandoff::new(
            handoff.presentation_ref,
            handoff.member_refs.clone(),
            handoff.relation_keys.clone(),
            handoff.selected_base_session_ref,
            handoff.locator_claim_key,
        )?;
        if canonical_handoff != *handoff {
            return Err(CatalogContractError::invalid(
                "catalog attach handoff members and relation keys must be canonical",
            ));
        }
        let members: BTreeSet<_> = handoff.member_refs.iter().copied().collect();
        let mut relation_adjacency: BTreeMap<CatalogEntityRef, BTreeSet<CatalogEntityRef>> =
            BTreeMap::new();
        for relation_key in &handoff.relation_keys {
            let stored = self.identity_relations.get(relation_key).ok_or_else(|| {
                CatalogContractError::invalid("catalog attach relation evidence is unknown")
            })?;
            if stored.fact.relation != IdentityRelationKind::SameEntity
                || stored.fact.canonical_winner != Some(handoff.presentation_ref)
                || !members.contains(&stored.fact.left_ref)
                || !members.contains(&stored.fact.right_ref)
            {
                return Err(CatalogContractError::invalid(
                    "catalog attach grouping requires accepted same-entity evidence with the disclosed representative",
                ));
            }
            relation_adjacency
                .entry(stored.fact.left_ref)
                .or_default()
                .insert(stored.fact.right_ref);
            relation_adjacency
                .entry(stored.fact.right_ref)
                .or_default()
                .insert(stored.fact.left_ref);
        }
        let mut proven_members = BTreeSet::from([handoff.presentation_ref]);
        let mut pending = vec![handoff.presentation_ref];
        while let Some(member) = pending.pop() {
            if let Some(related) = relation_adjacency.get(&member) {
                for related_member in related {
                    if proven_members.insert(*related_member) {
                        pending.push(*related_member);
                    }
                }
            }
        }
        if proven_members != members {
            return Err(CatalogContractError::invalid(
                "catalog attach grouping contains a member without explicit same-entity proof",
            ));
        }

        let selected = handoff.selected_base_session_ref;
        if !matches!(
            self.resolve_external_ref(selected.external_ref),
            CatalogResolvedEntity::Live { entity_ref, .. } if entity_ref == selected
        ) {
            return Err(CatalogContractError::invalid(
                "catalog attach requires a live concrete base session",
            ));
        }
        let locator = self
            .locators
            .get(&handoff.locator_claim_key)
            .ok_or_else(|| CatalogContractError::invalid("catalog attach locator is unknown"))?;
        if locator.fact.subject_ref != selected {
            return Err(CatalogContractError::invalid(
                "catalog attach locator does not belong to the selected base session",
            ));
        }
        let locator_value = locator.fact.for_view(view);
        if locator_value.value.is_none() {
            return Err(CatalogContractError::invalid(
                "catalog attach locator is withheld by the current policy view",
            ));
        }
        Ok(CatalogAttachTarget {
            session_ref: selected,
            locator_claim_key: locator.fact.locator_claim_key,
            locator_owner: locator.fact.owner.clone(),
            locator_kind: locator.fact.kind,
            locator_basis: locator.fact.basis,
            locator_disclosure: locator.fact.locator.disclosure,
            locator: locator_value,
            provenance: locator.fact.provenance.clone(),
        })
    }
}

fn materialize_project_row(
    project_ref: CatalogEntityRef,
    assertions: &[&StoredProjectAssertion],
) -> CatalogProjectRow {
    let native_identity = select_field(assertions.iter().filter_map(|stored| {
        stored
            .fact
            .native_identity
            .as_ref()
            .map(|value| project_field_candidate(stored, value))
    }));
    let root_identity = select_field(assertions.iter().filter_map(|stored| {
        stored
            .fact
            .root_identity
            .as_ref()
            .map(|value| project_field_candidate(stored, value))
    }));
    let display_path = select_field(assertions.iter().filter_map(|stored| {
        stored
            .fact
            .display_path
            .as_ref()
            .map(|value| project_field_candidate(stored, value))
    }));
    let display_name = select_field(assertions.iter().filter_map(|stored| {
        stored
            .fact
            .display_name
            .as_ref()
            .map(|value| project_field_candidate(stored, value))
    }));
    let native_time = select_field(assertions.iter().filter_map(|stored| {
        stored
            .fact
            .native_time
            .as_ref()
            .map(|value| project_field_candidate(stored, value))
    }));
    let availability = select_field(assertions.iter().map(|stored| FieldCandidate {
        assertion_key: stored.fact.assertion_key,
        observation_commit: stored.observation_commit,
        field: &stored.fact.availability,
    }))
    .expect("a project membership assertion always carries availability");
    let mut assertion_keys = assertions
        .iter()
        .map(|stored| stored.fact.assertion_key)
        .collect::<Vec<_>>();
    assertion_keys.sort();
    CatalogProjectRow {
        project_ref,
        native_identity,
        root_identity,
        display_path,
        display_name,
        native_time,
        availability,
        assertion_keys,
    }
}

fn materialize_session_row(
    session_ref: CatalogEntityRef,
    assertions: &[&StoredSessionAssertion],
    associations: &[&StoredAssociation],
) -> CatalogSessionRow {
    let native_identity = select_field(assertions.iter().filter_map(|stored| {
        stored
            .fact
            .native_identity
            .as_ref()
            .map(|value| session_field_candidate(stored, value))
    }));
    let title = select_field(assertions.iter().filter_map(|stored| {
        stored
            .fact
            .title
            .as_ref()
            .map(|value| session_field_candidate(stored, value))
    }));
    let first_user_summary = select_field(assertions.iter().filter_map(|stored| {
        stored
            .fact
            .first_user_summary
            .as_ref()
            .map(|value| session_field_candidate(stored, value))
    }));
    let native_created_at = select_field(assertions.iter().filter_map(|stored| {
        stored
            .fact
            .native_created_at
            .as_ref()
            .map(|value| session_field_candidate(stored, value))
    }));
    let native_updated_at = select_field(assertions.iter().filter_map(|stored| {
        stored
            .fact
            .native_updated_at
            .as_ref()
            .map(|value| session_field_candidate(stored, value))
    }));
    let native_message_count = select_field(assertions.iter().filter_map(|stored| {
        stored
            .fact
            .native_message_count
            .as_ref()
            .map(|value| session_field_candidate(stored, value))
    }));
    let transcript_locator_claim_keys = assertions
        .iter()
        .filter_map(|stored| stored.fact.transcript_locator_claim)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let availability = select_field(assertions.iter().map(|stored| FieldCandidate {
        assertion_key: stored.fact.assertion_key,
        observation_commit: stored.observation_commit,
        field: &stored.fact.availability,
    }))
    .expect("a session membership assertion always carries availability");
    let mut assertion_keys = assertions
        .iter()
        .map(|stored| stored.fact.assertion_key)
        .collect::<Vec<_>>();
    assertion_keys.sort();
    CatalogSessionRow {
        session_ref,
        project_association: materialize_association_coverage(associations),
        native_identity,
        title,
        first_user_summary,
        native_created_at,
        native_updated_at,
        native_message_count,
        transcript_locator_claim_keys,
        availability,
        assertion_keys,
    }
}

fn materialize_association_coverage(
    candidates: &[&StoredAssociation],
) -> CatalogAssociationCoverage {
    let Some(winner) = candidates
        .iter()
        .copied()
        .max_by(|left, right| compare_associations(left, right))
    else {
        return CatalogAssociationCoverage::Unknown;
    };
    let mut conflicting_association_keys = candidates
        .iter()
        .filter(|candidate| {
            candidate.fact.association_key != winner.fact.association_key
                && candidate.fact.authority == winner.fact.authority
                && candidate.fact.project_ref != winner.fact.project_ref
        })
        .map(|candidate| candidate.fact.association_key)
        .collect::<Vec<_>>();
    conflicting_association_keys.sort();
    let mut competing_associations = candidates
        .iter()
        .filter(|candidate| candidate.fact.association_key != winner.fact.association_key)
        .map(|candidate| candidate.fact.clone())
        .collect::<Vec<_>>();
    competing_associations.sort_by_key(|fact| fact.association_key);
    CatalogAssociationCoverage::Available {
        selection: Box::new(CatalogAssociationSelection {
            association: winner.fact.clone(),
            competing_associations,
            conflicting_association_keys,
        }),
    }
}

fn validate_publication_endpoints(reducer: &CatalogReducer) -> Result<(), CatalogContractError> {
    let live_projects = reducer
        .projects
        .values()
        .map(|stored| stored.fact.project_ref)
        .collect::<BTreeSet<_>>();
    let live_sessions = reducer
        .sessions
        .values()
        .map(|stored| stored.fact.session_ref)
        .collect::<BTreeSet<_>>();
    let live_entities = live_projects
        .iter()
        .chain(live_sessions.iter())
        .copied()
        .collect::<BTreeSet<_>>();

    for stored in reducer.associations.values() {
        if !live_sessions.contains(&stored.fact.session_ref)
            || !live_projects.contains(&stored.fact.project_ref)
        {
            return Err(CatalogContractError::invalid(
                "catalog publication association endpoints require live assertion evidence",
            ));
        }
        if let Some(locator_key) = stored.fact.locator_claim_key {
            let locator = reducer.locators.get(&locator_key).ok_or_else(|| {
                CatalogContractError::invalid(
                    "catalog publication association references unknown locator evidence",
                )
            })?;
            if locator.fact.subject_ref != stored.fact.session_ref
                && locator.fact.subject_ref != stored.fact.project_ref
            {
                return Err(CatalogContractError::invalid(
                    "catalog publication association locator belongs to neither endpoint",
                ));
            }
        }
    }
    for stored in reducer.locators.values() {
        if !live_entities.contains(&stored.fact.subject_ref) {
            return Err(CatalogContractError::invalid(
                "catalog publication locator subject requires live assertion evidence",
            ));
        }
    }
    for stored in reducer.sessions.values() {
        if let Some(locator_key) = stored.fact.transcript_locator_claim {
            let locator = reducer.locators.get(&locator_key).ok_or_else(|| {
                CatalogContractError::invalid(
                    "catalog publication session references unknown transcript locator evidence",
                )
            })?;
            if locator.fact.subject_ref != stored.fact.session_ref {
                return Err(CatalogContractError::invalid(
                    "catalog publication transcript locator belongs to another entity",
                ));
            }
        }
    }

    let historically_evidenced = live_entities
        .iter()
        .chain(reducer.retracted_assertions.keys())
        .chain(reducer.tombstones.keys())
        .copied()
        .collect::<BTreeSet<_>>();
    for stored in reducer.identity_relations.values() {
        let left_known = historically_evidenced.contains(&stored.fact.left_ref);
        let right_known = historically_evidenced.contains(&stored.fact.right_ref);
        if !(left_known && right_known) {
            return Err(CatalogContractError::invalid(
                "catalog publication identity relation has an unevidenced endpoint",
            ));
        }
    }
    for tombstone in reducer.tombstones.values() {
        if live_entities.contains(&tombstone.entity_ref)
            || !reducer
                .retracted_assertions
                .contains_key(&tombstone.entity_ref)
        {
            return Err(CatalogContractError::invalid(
                "catalog publication tombstone must retain non-live retracted assertion evidence",
            ));
        }
    }
    Ok(())
}

fn hash_publication_value<T: Serialize>(
    hasher: &mut blake3::Hasher,
    label: &[u8],
    value: &T,
) -> Result<(), CatalogContractError> {
    let encoded = serde_json::to_vec(value).map_err(|error| {
        CatalogContractError::invalid(format!(
            "catalog reducer publication could not canonically encode evidence: {error}"
        ))
    })?;
    hasher.update(&(label.len() as u64).to_be_bytes());
    hasher.update(label);
    hasher.update(&(encoded.len() as u64).to_be_bytes());
    hasher.update(&encoded);
    Ok(())
}

fn hash_publication_map_len(hasher: &mut blake3::Hasher, label: &[u8], len: usize) {
    hasher.update(&(label.len() as u64).to_be_bytes());
    hasher.update(label);
    hasher.update(&(len as u64).to_be_bytes());
}

fn derive_reducer_publication_revision(
    reducer: &CatalogReducer,
    project_rows: &[CatalogProjectRow],
    session_rows: &[CatalogSessionRow],
) -> Result<CatalogReducerPublicationRevision, CatalogContractError> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"spaghetti/rfc012b/catalog-reducer-publication-v1\0");

    hash_publication_map_len(&mut hasher, b"projects", reducer.projects.len());
    for (key, stored) in &reducer.projects {
        validate_observation_commit(stored.observation_commit)?;
        hasher.update(key.as_bytes());
        hasher.update(&stored.observation_commit.to_be_bytes());
        hash_publication_value(&mut hasher, b"project", &stored.fact)?;
    }
    hash_publication_map_len(&mut hasher, b"sessions", reducer.sessions.len());
    for (key, stored) in &reducer.sessions {
        validate_observation_commit(stored.observation_commit)?;
        hasher.update(key.as_bytes());
        hasher.update(&stored.observation_commit.to_be_bytes());
        hash_publication_value(&mut hasher, b"session", &stored.fact)?;
    }
    hash_publication_map_len(&mut hasher, b"associations", reducer.associations.len());
    for (key, stored) in &reducer.associations {
        validate_observation_commit(stored.observation_commit)?;
        hasher.update(key.as_bytes());
        hasher.update(&stored.observation_commit.to_be_bytes());
        hash_publication_value(&mut hasher, b"association", &stored.fact)?;
    }
    hash_publication_map_len(&mut hasher, b"locators", reducer.locators.len());
    for (key, stored) in &reducer.locators {
        validate_observation_commit(stored.observation_commit)?;
        hasher.update(key.as_bytes());
        hasher.update(&stored.observation_commit.to_be_bytes());
        hash_publication_value(&mut hasher, b"locator", &stored.fact)?;
    }
    hash_publication_map_len(
        &mut hasher,
        b"identity-relations",
        reducer.identity_relations.len(),
    );
    for (key, stored) in &reducer.identity_relations {
        validate_observation_commit(stored.observation_commit)?;
        hasher.update(key.as_bytes());
        hasher.update(&stored.observation_commit.to_be_bytes());
        hash_publication_value(&mut hasher, b"identity-relation", &stored.fact)?;
    }

    hash_publication_map_len(
        &mut hasher,
        b"assertion-history",
        reducer.assertion_history.len(),
    );
    for (key, coordinates) in &reducer.assertion_history {
        hasher.update(key.as_bytes());
        hash_publication_value(&mut hasher, b"owner", &coordinates.owner)?;
        hash_publication_value(&mut hasher, b"entity", &coordinates.entity_ref)?;
    }
    hash_publication_map_len(
        &mut hasher,
        b"association-history",
        reducer.association_history.len(),
    );
    for (key, coordinates) in &reducer.association_history {
        hasher.update(key.as_bytes());
        hash_publication_value(&mut hasher, b"owner", &coordinates.owner)?;
        hash_publication_value(&mut hasher, b"session", &coordinates.session_ref)?;
        hash_publication_value(&mut hasher, b"project", &coordinates.project_ref)?;
    }
    hash_publication_map_len(
        &mut hasher,
        b"locator-history",
        reducer.locator_history.len(),
    );
    for (key, coordinates) in &reducer.locator_history {
        hasher.update(key.as_bytes());
        hash_publication_value(&mut hasher, b"owner", &coordinates.owner)?;
        hash_publication_value(&mut hasher, b"subject", &coordinates.subject_ref)?;
    }
    hash_publication_map_len(
        &mut hasher,
        b"identity-relation-history",
        reducer.identity_relation_history.len(),
    );
    for (key, coordinates) in &reducer.identity_relation_history {
        hasher.update(key.as_bytes());
        hash_publication_value(&mut hasher, b"owner", &coordinates.owner)?;
        hash_publication_value(&mut hasher, b"relation-kind", &coordinates.relation)?;
        hash_publication_value(&mut hasher, b"left", &coordinates.left_ref)?;
        hash_publication_value(&mut hasher, b"right", &coordinates.right_ref)?;
    }
    hash_publication_map_len(
        &mut hasher,
        b"retracted-owners",
        reducer.retracted_owners.len(),
    );
    for (owner, evidence) in &reducer.retracted_owners {
        hash_publication_value(&mut hasher, b"owner", owner)?;
        hash_publication_value(&mut hasher, b"retraction", evidence)?;
    }
    hash_publication_map_len(&mut hasher, b"entity-kinds", reducer.entity_kinds.len());
    for (key, kind) in &reducer.entity_kinds {
        hasher.update(key.as_bytes());
        hash_publication_value(&mut hasher, b"entity-kind", kind)?;
    }
    hash_publication_map_len(
        &mut hasher,
        b"retracted-assertion-entities",
        reducer.retracted_assertions.len(),
    );
    for (entity_ref, assertions) in &reducer.retracted_assertions {
        hash_publication_value(&mut hasher, b"retracted-entity", entity_ref)?;
        hash_publication_map_len(&mut hasher, b"retracted-assertions", assertions.len());
        for (key, retracted) in assertions {
            hasher.update(key.as_bytes());
            hash_publication_value(&mut hasher, b"retraction", &retracted.evidence)?;
            hasher.update(&retracted.retracted_at_commit.to_be_bytes());
            hash_publication_value(&mut hasher, b"provenance", &retracted.provenance)?;
        }
    }
    hash_publication_map_len(&mut hasher, b"tombstones", reducer.tombstones.len());
    for (entity_ref, tombstone) in &reducer.tombstones {
        hash_publication_value(&mut hasher, b"tombstone-entity", entity_ref)?;
        hash_publication_value(&mut hasher, b"tombstone", tombstone)?;
    }
    hash_publication_map_len(&mut hasher, b"project-rows", project_rows.len());
    for row in project_rows {
        hash_publication_value(&mut hasher, b"project-row", row)?;
    }
    hash_publication_map_len(&mut hasher, b"session-rows", session_rows.len());
    for row in session_rows {
        hash_publication_value(&mut hasher, b"session-row", row)?;
    }

    Ok(CatalogReducerPublicationRevision(
        *hasher.finalize().as_bytes(),
    ))
}

fn validate_identity_relation_graph(
    relations: &BTreeMap<CatalogIdentityRelationKey, StoredIdentityRelation>,
) -> Result<(), CatalogContractError> {
    if relations.len() > MAX_REPLACEMENT_RELATIONS {
        return Err(CatalogContractError::invalid(format!(
            "catalog identity graph exceeds {MAX_REPLACEMENT_RELATIONS} relations"
        )));
    }
    validate_replacement_graph(relations)?;

    let mut replacement_provenance = BTreeMap::<CatalogEntityRef, BTreeSet<_>>::new();
    for stored in relations.values() {
        if let Some((prior_ref, _)) = stored.fact.replacement_edge() {
            replacement_provenance.entry(prior_ref).or_default().extend(
                stored
                    .fact
                    .provenance
                    .iter()
                    .map(|reference| reference.fact_revision_id),
            );
        }
    }
    if replacement_provenance
        .values()
        .any(|provenance| provenance.len() > MAX_PROVENANCE_REVISIONS)
    {
        return Err(CatalogContractError::invalid(format!(
            "catalog supersession resolution exceeds {MAX_PROVENANCE_REVISIONS} provenance revisions"
        )));
    }

    let same_relations: Vec<_> = relations
        .values()
        .filter(|stored| stored.fact.relation == IdentityRelationKind::SameEntity)
        .collect();
    let mut adjacency: BTreeMap<CatalogEntityRef, BTreeSet<CatalogEntityRef>> = BTreeMap::new();
    for stored in &same_relations {
        adjacency
            .entry(stored.fact.left_ref)
            .or_default()
            .insert(stored.fact.right_ref);
        adjacency
            .entry(stored.fact.right_ref)
            .or_default()
            .insert(stored.fact.left_ref);
    }
    let mut pending: BTreeSet<_> = adjacency.keys().copied().collect();
    while let Some(seed) = pending.pop_first() {
        let mut component = BTreeSet::from([seed]);
        let mut stack = vec![seed];
        while let Some(node) = stack.pop() {
            if let Some(neighbors) = adjacency.get(&node) {
                for neighbor in neighbors {
                    if component.insert(*neighbor) {
                        pending.remove(neighbor);
                        stack.push(*neighbor);
                    }
                }
            }
        }
        let component_relations: Vec<_> = same_relations
            .iter()
            .filter(|stored| component.contains(&stored.fact.left_ref))
            .collect();
        let winners: BTreeSet<_> = component_relations
            .iter()
            .filter_map(|stored| stored.fact.canonical_winner)
            .collect();
        let policies: BTreeSet<_> = component_relations
            .iter()
            .filter_map(|stored| stored.fact.collision_policy_id.as_deref())
            .collect();
        if winners.len() != 1 || policies.len() != 1 {
            return Err(CatalogContractError::invalid(
                "same-entity evidence forms an ambiguous canonical winner or collision policy",
            ));
        }
        if relations.values().any(|stored| {
            stored
                .fact
                .replacement_edge()
                .is_some_and(|(prior, replacement)| {
                    component.contains(&prior) && component.contains(&replacement)
                })
        }) {
            return Err(CatalogContractError::invalid(
                "the same identity component cannot also declare an internal replacement",
            ));
        }
    }
    Ok(())
}

fn validate_replacement_graph(
    relations: &BTreeMap<CatalogIdentityRelationKey, StoredIdentityRelation>,
) -> Result<(), CatalogContractError> {
    let replacement_relations: Vec<_> = relations
        .values()
        .filter_map(|stored| stored.fact.replacement_edge())
        .collect();
    let mut nodes = BTreeSet::new();
    let mut edges: BTreeMap<CatalogEntityRef, BTreeSet<CatalogEntityRef>> = BTreeMap::new();
    let mut incoming: BTreeMap<CatalogEntityRef, usize> = BTreeMap::new();
    for (prior_ref, replacement_ref) in replacement_relations {
        nodes.insert(prior_ref);
        nodes.insert(replacement_ref);
        incoming.entry(prior_ref).or_insert(0);
        incoming.entry(replacement_ref).or_insert(0);
        if edges.entry(prior_ref).or_default().insert(replacement_ref) {
            *incoming.entry(replacement_ref).or_insert(0) += 1;
        }
    }
    let mut ready: BTreeSet<_> = nodes
        .iter()
        .filter(|node| incoming.get(node).copied().unwrap_or(0) == 0)
        .copied()
        .collect();
    let mut visited = 0;
    while let Some(node) = ready.pop_first() {
        visited += 1;
        if let Some(successors) = edges.get(&node) {
            for successor in successors {
                let count = incoming
                    .get_mut(successor)
                    .expect("replacement successor is registered");
                *count -= 1;
                if *count == 0 {
                    ready.insert(*successor);
                }
            }
        }
    }
    if visited != nodes.len() {
        return Err(CatalogContractError::invalid(
            "catalog replacement relations must form an acyclic graph",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
