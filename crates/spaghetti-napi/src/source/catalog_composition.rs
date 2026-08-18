//! Planned RFC 012B catalog-tier source compositions.
//!
//! This crate-private contract describes bounded catalog views in the same
//! vocabulary as an RFC 012A source declaration. It deliberately performs no
//! source access and names no concrete adapter. A promoted adapter declaration
//! must bind these values before the runtime may execute the composition.

use std::fmt;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::{Serialize, Serializer};
#[cfg(test)]
use sha2::{Digest as _, Sha256};

use crate::adapter::{
    AuthorizedCatalogAccess, CanonicalSourceInstanceKey, CompatibilityClass,
    ContractVersionSelection, CoverageAbsence, CoverageAbsenceKind, CoverageDeclarationDigest,
    CoverageDomain, CoverageObjectKey, CoveragePosition, CoveragePositionKind, CoverageProvenance,
    CoverageSetCompleteness, CoverageStatus, CoverageStreamKey, SourceCoveragePoint,
    SourceCoverageSet, SOURCE_COVERAGE_CONTRACT_VERSION,
};
use crate::catalog_contract::publication::{
    CatalogCompleteSourceAssembly, CatalogPublicationMemberRef, CatalogSourceCompletionRevision,
    CatalogSourceMembershipRevision,
};
use crate::catalog_contract::{
    CatalogAccessPolicyDigest, CatalogContractError, CatalogCoveragePlanSource,
    CatalogCoverageScope, CATALOG_PROJECTION_PACK_ID, CATALOG_QUERY_PACK_CONTRACT_VERSION,
};
use crate::coverage_runtime::{
    derive_coverage_membership_revision, source_membership_prefix, CoverageMembershipObject,
};

pub(crate) const CATALOG_SOURCE_COMPOSITION_CONTRACT_VERSION: u32 = 1;
pub(crate) const CATALOG_MEMBERSHIP_CONTRACT_VERSION: u32 = 1;

const DIGEST_BYTES: usize = 32;
const MAX_IDENTIFIER_BYTES: usize = 128;
const MAX_OWNERSHIP_LABEL_BYTES: usize = 256;
const MAX_SELECTOR_BYTES: usize = 1_024;
const MAX_COMPONENTS: usize = 64;
const MAX_SELECTORS_PER_COMPONENT: usize = 64;
const MAX_DISPOSITION_OWNERS_PER_COMPONENT: usize = 64;
const MAX_DISCOVERY_ENTRIES: u32 = 1_000_000;
const MAX_DISCOVERY_DEPTH: u32 = 128;
const MAX_RECORD_BYTES: u64 = 64 * 1024 * 1024;
const MAX_WINDOW_BYTES: u64 = 64 * 1024 * 1024;
const MAX_WINDOW_RECORDS: u32 = 4_096;
const MAX_CONFORMANCE_RECORDS: usize = 1_000_000;
const MAX_DOCUMENT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_MEMBERSHIP_MEMBERS: usize = 1_000_000;
const MAX_SEMANTIC_IDENTITY_BYTES: usize = 64 * 1024;
// These are the RFC 012A portable set caps, not source-performance targets.
const MAX_CATALOG_COVERAGE_POINTS: usize = 250_000;
const MAX_CATALOG_COVERAGE_ABSENCES: usize = 250_000;
const MAX_PORTABLE_GENERATION: u64 = 9_007_199_254_740_991;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub(crate) struct CatalogCompositionError {
    message: String,
}

impl CatalogCompositionError {
    fn invalid(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

fn validate_identifier(label: &str, value: &str) -> Result<(), CatalogCompositionError> {
    let mut bytes = value.bytes();
    let Some(first) = bytes.next() else {
        return Err(CatalogCompositionError::invalid(format!(
            "{label} must not be empty"
        )));
    };
    if value.len() > MAX_IDENTIFIER_BYTES
        || !first.is_ascii_lowercase() && !first.is_ascii_digit()
        || bytes.any(|byte| {
            !byte.is_ascii_lowercase()
                && !byte.is_ascii_digit()
                && !matches!(byte, b'.' | b'_' | b'-')
        })
    {
        return Err(CatalogCompositionError::invalid(format!(
            "{label} must match the RFC 012A identifier grammar and be at most {MAX_IDENTIFIER_BYTES} bytes"
        )));
    }
    Ok(())
}

fn validate_selector(value: &str) -> Result<(), CatalogCompositionError> {
    if value.is_empty()
        || value.trim() != value
        || value.len() > MAX_SELECTOR_BYTES
        || value.starts_with('/')
        || value.contains('\\')
        || value.contains(':')
        || value
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
    {
        return Err(CatalogCompositionError::invalid(
            "relative selector is empty, oversized, absolute, or platform-specific",
        ));
    }
    for component in value.split('/') {
        if component.is_empty() || matches!(component, "." | "..") {
            return Err(CatalogCompositionError::invalid(
                "relative selector contains an empty or traversal component",
            ));
        }
    }
    Ok(())
}

fn validate_ownership_label(value: &str) -> Result<(), CatalogCompositionError> {
    if value.is_empty()
        || value.trim() != value
        || value.len() > MAX_OWNERSHIP_LABEL_BYTES
        || value.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(CatalogCompositionError::invalid(format!(
            "disposition ownership must be canonical and at most {MAX_OWNERSHIP_LABEL_BYTES} bytes"
        )));
    }
    Ok(())
}

fn canonicalize_unique_strings(
    label: &str,
    values: &mut [String],
    max: usize,
    validator: impl Fn(&str) -> Result<(), CatalogCompositionError>,
) -> Result<(), CatalogCompositionError> {
    if values.is_empty() || values.len() > max {
        return Err(CatalogCompositionError::invalid(format!(
            "{label} must contain 1..={max} values"
        )));
    }
    for value in values.iter() {
        validator(value)?;
    }
    values.sort();
    if values.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(CatalogCompositionError::invalid(format!(
            "{label} contains a duplicate value"
        )));
    }
    Ok(())
}

#[cfg(test)]
fn digest_contract(domain: &[u8], components: &[&[u8]]) -> [u8; DIGEST_BYTES] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"spaghetti/rfc012b/catalog-source-contract\0");
    hash_bytes(&mut hasher, domain);
    hasher.update(&(components.len() as u64).to_be_bytes());
    for component in components {
        hash_bytes(&mut hasher, component);
    }
    *hasher.finalize().as_bytes()
}

fn hash_bytes(hasher: &mut blake3::Hasher, value: &[u8]) {
    hasher.update(&(value.len() as u64).to_be_bytes());
    hasher.update(value);
}

fn hash_string(hasher: &mut blake3::Hasher, value: &str) {
    hash_bytes(hasher, value.as_bytes());
}

fn serialize_digest<S>(
    prefix: &str,
    bytes: &[u8; DIGEST_BYTES],
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(&format!("{prefix}:{}", URL_SAFE_NO_PAD.encode(bytes)))
}

macro_rules! opaque_digest_type {
    ($name:ident, $prefix:literal) => {
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub(crate) struct $name([u8; DIGEST_BYTES]);

        impl $name {
            fn from_digest(bytes: [u8; DIGEST_BYTES]) -> Self {
                Self(bytes)
            }

            fn as_bytes(&self) -> &[u8; DIGEST_BYTES] {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(formatter, "{}:{}", $prefix, URL_SAFE_NO_PAD.encode(self.0))
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter
                    .debug_tuple(stringify!($name))
                    .field(&self.to_string())
                    .finish()
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serialize_digest($prefix, &self.0, serializer)
            }
        }
    };
}

opaque_digest_type!(CatalogCompositionId, "catalog-composition-v1");
opaque_digest_type!(CatalogBindingDigest, "catalog-binding-v1");
// Composition-conformance reference only. This is neither a public
// ExternalEntityRef nor an RFC 012A source-coverage object key.
opaque_digest_type!(CatalogMemberRef, "catalog-member-v1");
// Revision of the admitted opaque member set below. It intentionally does not
// replace SourceCoverageSet.membership_revision, which proves native source
// object coverage at a watermark.
opaque_digest_type!(CatalogMembershipRevision, "catalog-membership-v1");
opaque_digest_type!(CatalogAuthorityRevision, "catalog-authority-revision-v1");
opaque_digest_type!(CatalogCoverageProof, "catalog-coverage-proof-v1");
opaque_digest_type!(CatalogConformanceDigest, "catalog-conformance-v1");
opaque_digest_type!(CatalogWindowContinuation, "catalog-window-continuation-v1");

/// Canonical restart identity for the complete set of component-enumeration
/// evidence assembled into one library coverage result. This deliberately is
/// not a portable DTO and is distinct from both catalog membership and RFC
/// 012A native-object membership revisions.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct CatalogComponentCompletionRevision([u8; DIGEST_BYTES]);

impl CatalogComponentCompletionRevision {
    fn from_digest(bytes: [u8; DIGEST_BYTES]) -> Self {
        Self(bytes)
    }

    fn as_bytes(&self) -> &[u8; DIGEST_BYTES] {
        &self.0
    }
}

impl fmt::Display for CatalogComponentCompletionRevision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "catalog-component-completion-v1:{}",
            URL_SAFE_NO_PAD.encode(self.0)
        )
    }
}

impl fmt::Debug for CatalogComponentCompletionRevision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("CatalogComponentCompletionRevision")
            .field(&self.to_string())
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct CatalogPromotedBinding {
    source_declaration_digest: CatalogBindingDigest,
    support_release_digest: CatalogBindingDigest,
}

impl CatalogPromotedBinding {
    pub(crate) fn from_digests(
        source_declaration_digest: [u8; DIGEST_BYTES],
        support_release_digest: [u8; DIGEST_BYTES],
    ) -> Result<Self, CatalogCompositionError> {
        let value = Self {
            source_declaration_digest: CatalogBindingDigest::from_digest(source_declaration_digest),
            support_release_digest: CatalogBindingDigest::from_digest(support_release_digest),
        };
        value.validate()?;
        Ok(value)
    }

    #[cfg(test)]
    fn fixture(source_declaration: &[u8], support_release: &[u8]) -> Self {
        let source_declaration_digest: [u8; DIGEST_BYTES] =
            Sha256::digest(source_declaration).into();
        let support_release_digest: [u8; DIGEST_BYTES] = Sha256::digest(support_release).into();
        Self::from_digests(source_declaration_digest, support_release_digest)
            .expect("fixture binding digests are nonzero")
    }

    fn validate(self) -> Result<(), CatalogCompositionError> {
        if self.source_declaration_digest.as_bytes() == &[0; DIGEST_BYTES]
            || self.support_release_digest.as_bytes() == &[0; DIGEST_BYTES]
        {
            return Err(CatalogCompositionError::invalid(
                "promoted composition bindings require nonzero declaration and support-release digests",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum CatalogCompositionBinding {
    PlannedUnbound {
        planning_evidence_id: String,
    },
    Promoted {
        source_declaration_digest: CatalogBindingDigest,
        support_release_digest: CatalogBindingDigest,
    },
}

impl CatalogCompositionBinding {
    fn planned(planning_evidence_id: impl Into<String>) -> Self {
        Self::PlannedUnbound {
            planning_evidence_id: planning_evidence_id.into(),
        }
    }

    fn promoted(binding: CatalogPromotedBinding) -> Self {
        Self::Promoted {
            source_declaration_digest: binding.source_declaration_digest,
            support_release_digest: binding.support_release_digest,
        }
    }

    fn validate(&self) -> Result<(), CatalogCompositionError> {
        match self {
            Self::PlannedUnbound {
                planning_evidence_id,
            } => validate_identifier("planning_evidence_id", planning_evidence_id),
            Self::Promoted {
                source_declaration_digest,
                support_release_digest,
            } => CatalogPromotedBinding {
                source_declaration_digest: *source_declaration_digest,
                support_release_digest: *support_release_digest,
            }
            .validate(),
        }
    }

    fn hash_into(&self, hasher: &mut blake3::Hasher) {
        match self {
            Self::PlannedUnbound {
                planning_evidence_id,
            } => {
                hash_string(hasher, "planned_unbound");
                hash_string(hasher, planning_evidence_id);
            }
            Self::Promoted {
                source_declaration_digest,
                support_release_digest,
            } => {
                hash_string(hasher, "promoted");
                hasher.update(source_declaration_digest.as_bytes());
                hasher.update(support_release_digest.as_bytes());
            }
        }
    }

    fn promoted_binding(&self) -> Option<CatalogPromotedBinding> {
        match self {
            Self::PlannedUnbound { .. } => None,
            Self::Promoted {
                source_declaration_digest,
                support_release_digest,
            } => Some(CatalogPromotedBinding {
                source_declaration_digest: *source_declaration_digest,
                support_release_digest: *support_release_digest,
            }),
        }
    }
}

impl CatalogMemberRef {
    /// Derives an opaque member reference from an already-canonical semantic
    /// identity. Native locators must not be supplied as the semantic identity.
    #[cfg(test)]
    fn fixture_from_semantic_identity(
        member_identity_contract_id: &str,
        semantic_identity: &[u8],
    ) -> Result<Self, CatalogCompositionError> {
        validate_identifier("member_identity_contract_id", member_identity_contract_id)?;
        if semantic_identity.is_empty() || semantic_identity.len() > MAX_SEMANTIC_IDENTITY_BYTES {
            return Err(CatalogCompositionError::invalid(format!(
                "semantic member identity must contain 1..={MAX_SEMANTIC_IDENTITY_BYTES} bytes"
            )));
        }
        Ok(Self::from_digest(digest_contract(
            b"member-ref-v1",
            &[member_identity_contract_id.as_bytes(), semantic_identity],
        )))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CatalogDecoderStateBoundary {
    ObjectGenerationCursor,
    ObjectGenerationRevision,
    StatelessRecord,
    FullSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum CatalogOverlapStrategy {
    CommitCatalogFacts,
    IdempotentOverlap,
    DisjointCatalogFamily { ownership_contract_id: String },
    FullOnly,
}

impl CatalogOverlapStrategy {
    fn validate(&self) -> Result<(), CatalogCompositionError> {
        match self {
            Self::DisjointCatalogFamily {
                ownership_contract_id,
            } => validate_identifier("ownership_contract_id", ownership_contract_id),
            Self::CommitCatalogFacts | Self::IdempotentOverlap | Self::FullOnly => Ok(()),
        }
    }

    fn hash_into(&self, hasher: &mut blake3::Hasher) {
        match self {
            Self::CommitCatalogFacts => hash_string(hasher, "commit_catalog_facts"),
            Self::IdempotentOverlap => hash_string(hasher, "idempotent_overlap"),
            Self::DisjointCatalogFamily {
                ownership_contract_id,
            } => {
                hash_string(hasher, "disjoint_catalog_family");
                hash_string(hasher, ownership_contract_id);
            }
            Self::FullOnly => hash_string(hasher, "full_only"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct CatalogDiscoveryBounds {
    max_entries: u32,
    max_depth: u32,
}

impl CatalogDiscoveryBounds {
    pub(crate) fn new(max_entries: u32, max_depth: u32) -> Result<Self, CatalogCompositionError> {
        let value = Self {
            max_entries,
            max_depth,
        };
        value.validate()?;
        Ok(value)
    }

    fn validate(&self) -> Result<(), CatalogCompositionError> {
        if self.max_entries == 0 || self.max_entries > MAX_DISCOVERY_ENTRIES {
            return Err(CatalogCompositionError::invalid(format!(
                "max_entries must be within 1..={MAX_DISCOVERY_ENTRIES}"
            )));
        }
        if self.max_depth == 0 || self.max_depth > MAX_DISCOVERY_DEPTH {
            return Err(CatalogCompositionError::invalid(format!(
                "max_depth must be within 1..={MAX_DISCOVERY_DEPTH}"
            )));
        }
        Ok(())
    }

    fn hash_into(&self, hasher: &mut blake3::Hasher) {
        hasher.update(&self.max_entries.to_be_bytes());
        hasher.update(&self.max_depth.to_be_bytes());
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum CatalogSourcePrimitive {
    DirectoryMembership,
    ReplaceDocument {
        max_object_bytes: u64,
    },
    DelimitedHead {
        max_record_bytes: u64,
    },
    DelimitedPrefix {
        max_record_bytes: u64,
        max_window_bytes: u64,
        max_records: u32,
    },
}

impl CatalogSourcePrimitive {
    fn validate(&self) -> Result<(), CatalogCompositionError> {
        match self {
            Self::DirectoryMembership => Ok(()),
            Self::ReplaceDocument { max_object_bytes } => {
                validate_positive_bound("max_object_bytes", *max_object_bytes, MAX_DOCUMENT_BYTES)
            }
            Self::DelimitedHead { max_record_bytes } => {
                validate_positive_bound("max_record_bytes", *max_record_bytes, MAX_RECORD_BYTES)
            }
            Self::DelimitedPrefix {
                max_record_bytes,
                max_window_bytes,
                max_records,
            } => {
                validate_positive_bound("max_record_bytes", *max_record_bytes, MAX_RECORD_BYTES)?;
                validate_positive_bound("max_window_bytes", *max_window_bytes, MAX_WINDOW_BYTES)?;
                if max_window_bytes < max_record_bytes {
                    return Err(CatalogCompositionError::invalid(
                        "max_window_bytes must admit one maximum-sized logical record",
                    ));
                }
                if *max_records == 0 || *max_records > MAX_WINDOW_RECORDS {
                    return Err(CatalogCompositionError::invalid(format!(
                        "max_records must be within 1..={MAX_WINDOW_RECORDS}"
                    )));
                }
                Ok(())
            }
        }
    }

    fn hash_into(&self, hasher: &mut blake3::Hasher) {
        match self {
            Self::DirectoryMembership => hash_string(hasher, "directory_membership"),
            Self::ReplaceDocument { max_object_bytes } => {
                hash_string(hasher, "replace_document");
                hasher.update(&max_object_bytes.to_be_bytes());
            }
            Self::DelimitedHead { max_record_bytes } => {
                hash_string(hasher, "delimited_head");
                hasher.update(&max_record_bytes.to_be_bytes());
            }
            Self::DelimitedPrefix {
                max_record_bytes,
                max_window_bytes,
                max_records,
            } => {
                hash_string(hasher, "delimited_prefix");
                hasher.update(&max_record_bytes.to_be_bytes());
                hasher.update(&max_window_bytes.to_be_bytes());
                hasher.update(&max_records.to_be_bytes());
            }
        }
    }

    pub(crate) fn window_spec(&self) -> Option<CatalogRecordWindowSpec> {
        match self {
            Self::DelimitedHead { max_record_bytes } => Some(CatalogRecordWindowSpec {
                max_record_bytes: *max_record_bytes,
                max_window_bytes: *max_record_bytes,
                max_records: 1,
            }),
            Self::DelimitedPrefix {
                max_record_bytes,
                max_window_bytes,
                max_records,
            } => Some(CatalogRecordWindowSpec {
                max_record_bytes: *max_record_bytes,
                max_window_bytes: *max_window_bytes,
                max_records: *max_records,
            }),
            Self::DirectoryMembership | Self::ReplaceDocument { .. } => None,
        }
    }

    fn complete_coverage_semantics(&self) -> (CoveragePositionKind, CoverageStatus) {
        match self {
            Self::DirectoryMembership => (
                CoveragePositionKind::SnapshotRevision,
                CoverageStatus::ExactSnapshot,
            ),
            Self::ReplaceDocument { .. } => (
                CoveragePositionKind::DocumentRevision,
                CoverageStatus::ExactSnapshot,
            ),
            Self::DelimitedHead { .. } | Self::DelimitedPrefix { .. } => (
                CoveragePositionKind::AppendCursor,
                CoverageStatus::CompleteThrough,
            ),
        }
    }
}

fn validate_positive_bound(
    label: &str,
    value: u64,
    maximum: u64,
) -> Result<(), CatalogCompositionError> {
    if value == 0 || value > maximum {
        return Err(CatalogCompositionError::invalid(format!(
            "{label} must be within 1..={maximum}"
        )));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum CatalogContribution {
    Membership {
        member_identity_contract_id: String,
        admission_contract_id: String,
        provides_metadata: bool,
    },
    MetadataForKnownMember {
        member_identity_contract_id: String,
        metadata_contract_id: String,
    },
}

impl CatalogContribution {
    fn validate(&self) -> Result<(), CatalogCompositionError> {
        validate_identifier(
            "member_identity_contract_id",
            self.member_identity_contract_id(),
        )?;
        match self {
            Self::Membership {
                admission_contract_id,
                ..
            } => validate_identifier("admission_contract_id", admission_contract_id),
            Self::MetadataForKnownMember {
                metadata_contract_id,
                ..
            } => validate_identifier("metadata_contract_id", metadata_contract_id),
        }
    }

    fn member_identity_contract_id(&self) -> &str {
        match self {
            Self::Membership {
                member_identity_contract_id,
                ..
            }
            | Self::MetadataForKnownMember {
                member_identity_contract_id,
                ..
            } => member_identity_contract_id,
        }
    }

    fn can_admit_member(&self) -> bool {
        matches!(self, Self::Membership { .. })
    }

    fn can_supply_metadata(&self) -> bool {
        match self {
            Self::Membership {
                provides_metadata, ..
            } => *provides_metadata,
            Self::MetadataForKnownMember { .. } => true,
        }
    }

    fn hash_into(&self, hasher: &mut blake3::Hasher) {
        match self {
            Self::Membership {
                member_identity_contract_id,
                admission_contract_id,
                provides_metadata,
            } => {
                hash_string(hasher, "membership");
                hash_string(hasher, member_identity_contract_id);
                hash_string(hasher, admission_contract_id);
                hasher.update(&[u8::from(*provides_metadata)]);
            }
            Self::MetadataForKnownMember {
                member_identity_contract_id,
                metadata_contract_id,
            } => {
                hash_string(hasher, "metadata_for_known_member");
                hash_string(hasher, member_identity_contract_id);
                hash_string(hasher, metadata_contract_id);
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CatalogSourceComponent {
    component_id: String,
    stream_id: String,
    root_id: String,
    relative_selectors: Vec<String>,
    discovery_bounds: CatalogDiscoveryBounds,
    primitive: CatalogSourcePrimitive,
    contribution: CatalogContribution,
    overlap_strategy: CatalogOverlapStrategy,
    safe_decoder_state_boundary: CatalogDecoderStateBoundary,
    source_record_contract_version: u32,
    framing_contract_version: u32,
    decoder_contract_id: String,
    decoder_contract_version: u32,
    disposition_ownership: Vec<String>,
}

impl CatalogSourceComponent {
    pub(crate) fn normalize(mut self) -> Result<Self, CatalogCompositionError> {
        canonicalize_unique_strings(
            "relative_selectors",
            &mut self.relative_selectors,
            MAX_SELECTORS_PER_COMPONENT,
            validate_selector,
        )?;
        canonicalize_unique_strings(
            "disposition_ownership",
            &mut self.disposition_ownership,
            MAX_DISPOSITION_OWNERS_PER_COMPONENT,
            validate_ownership_label,
        )?;
        self.validate()?;
        Ok(self)
    }

    fn validate(&self) -> Result<(), CatalogCompositionError> {
        validate_identifier("component_id", &self.component_id)?;
        validate_identifier("stream_id", &self.stream_id)?;
        validate_identifier("root_id", &self.root_id)?;
        validate_identifier("decoder_contract_id", &self.decoder_contract_id)?;
        validate_canonical_strings(
            "relative_selectors",
            &self.relative_selectors,
            MAX_SELECTORS_PER_COMPONENT,
            validate_selector,
        )?;
        validate_canonical_strings(
            "disposition_ownership",
            &self.disposition_ownership,
            MAX_DISPOSITION_OWNERS_PER_COMPONENT,
            validate_ownership_label,
        )?;
        self.discovery_bounds.validate()?;
        self.primitive.validate()?;
        self.contribution.validate()?;
        self.overlap_strategy.validate()?;
        if self.primitive.window_spec().is_some()
            && matches!(self.overlap_strategy, CatalogOverlapStrategy::FullOnly)
        {
            return Err(CatalogCompositionError::invalid(
                "partial head/prefix components require an explicit compositional overlap strategy",
            ));
        }
        if self.source_record_contract_version == 0
            || self.framing_contract_version == 0
            || self.decoder_contract_version == 0
        {
            return Err(CatalogCompositionError::invalid(
                "source-record, framing, and decoder contract versions must be positive",
            ));
        }
        let compatible_boundary = match self.primitive {
            CatalogSourcePrimitive::DirectoryMembership => {
                self.safe_decoder_state_boundary == CatalogDecoderStateBoundary::FullSnapshot
            }
            CatalogSourcePrimitive::ReplaceDocument { .. } => {
                self.safe_decoder_state_boundary
                    == CatalogDecoderStateBoundary::ObjectGenerationRevision
            }
            CatalogSourcePrimitive::DelimitedHead { .. }
            | CatalogSourcePrimitive::DelimitedPrefix { .. } => matches!(
                self.safe_decoder_state_boundary,
                CatalogDecoderStateBoundary::ObjectGenerationCursor
                    | CatalogDecoderStateBoundary::StatelessRecord
            ),
        };
        if !compatible_boundary {
            return Err(CatalogCompositionError::invalid(
                "primitive and safe decoder-state boundary are incompatible",
            ));
        }
        Ok(())
    }

    fn hash_into(&self, hasher: &mut blake3::Hasher) {
        hash_string(hasher, &self.component_id);
        hash_string(hasher, &self.stream_id);
        hash_string(hasher, &self.root_id);
        hasher.update(&(self.relative_selectors.len() as u64).to_be_bytes());
        for selector in &self.relative_selectors {
            hash_string(hasher, selector);
        }
        self.discovery_bounds.hash_into(hasher);
        self.primitive.hash_into(hasher);
        self.contribution.hash_into(hasher);
        self.overlap_strategy.hash_into(hasher);
        hash_string(
            hasher,
            match self.safe_decoder_state_boundary {
                CatalogDecoderStateBoundary::ObjectGenerationCursor => "object_generation_cursor",
                CatalogDecoderStateBoundary::ObjectGenerationRevision => {
                    "object_generation_revision"
                }
                CatalogDecoderStateBoundary::StatelessRecord => "stateless_record",
                CatalogDecoderStateBoundary::FullSnapshot => "full_snapshot",
            },
        );
        hasher.update(&self.source_record_contract_version.to_be_bytes());
        hasher.update(&self.framing_contract_version.to_be_bytes());
        hash_string(hasher, &self.decoder_contract_id);
        hasher.update(&self.decoder_contract_version.to_be_bytes());
        hasher.update(&(self.disposition_ownership.len() as u64).to_be_bytes());
        for ownership in &self.disposition_ownership {
            hash_string(hasher, ownership);
        }
    }
}

fn validate_canonical_strings(
    label: &str,
    values: &[String],
    max: usize,
    validator: impl Fn(&str) -> Result<(), CatalogCompositionError>,
) -> Result<(), CatalogCompositionError> {
    if values.is_empty() || values.len() > max {
        return Err(CatalogCompositionError::invalid(format!(
            "{label} must contain 1..={max} values"
        )));
    }
    for value in values {
        validator(value)?;
    }
    if values.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(CatalogCompositionError::invalid(format!(
            "{label} must be strictly increasing"
        )));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CatalogSourceComposition {
    composition_contract_version: u32,
    composition_id: CatalogCompositionId,
    binding: CatalogCompositionBinding,
    adapter_id: String,
    support_release_id: String,
    source_declaration_id: String,
    member_identity_contract_id: String,
    components: Vec<CatalogSourceComponent>,
}

impl CatalogSourceComposition {
    pub(crate) fn new_planned(
        adapter_id: impl Into<String>,
        support_release_id: impl Into<String>,
        source_declaration_id: impl Into<String>,
        planning_evidence_id: impl Into<String>,
        components: Vec<CatalogSourceComponent>,
    ) -> Result<Self, CatalogCompositionError> {
        Self::new_with_binding(
            adapter_id,
            support_release_id,
            source_declaration_id,
            CatalogCompositionBinding::planned(planning_evidence_id),
            components,
        )
    }

    pub(crate) fn new_promoted(
        adapter_id: impl Into<String>,
        support_release_id: impl Into<String>,
        source_declaration_id: impl Into<String>,
        binding: CatalogPromotedBinding,
        components: Vec<CatalogSourceComponent>,
    ) -> Result<Self, CatalogCompositionError> {
        Self::new_with_binding(
            adapter_id,
            support_release_id,
            source_declaration_id,
            CatalogCompositionBinding::promoted(binding),
            components,
        )
    }

    fn new_with_binding(
        adapter_id: impl Into<String>,
        support_release_id: impl Into<String>,
        source_declaration_id: impl Into<String>,
        binding: CatalogCompositionBinding,
        mut components: Vec<CatalogSourceComponent>,
    ) -> Result<Self, CatalogCompositionError> {
        if components.is_empty() || components.len() > MAX_COMPONENTS {
            return Err(CatalogCompositionError::invalid(format!(
                "catalog composition must contain 1..={MAX_COMPONENTS} components"
            )));
        }
        components = components
            .into_iter()
            .map(CatalogSourceComponent::normalize)
            .collect::<Result<Vec<_>, _>>()?;
        components.sort_by(|left, right| left.component_id.cmp(&right.component_id));
        if components
            .windows(2)
            .any(|pair| pair[0].component_id == pair[1].component_id)
        {
            return Err(CatalogCompositionError::invalid(
                "catalog composition contains duplicate component IDs",
            ));
        }
        let member_identity_contract_id = components
            .first()
            .expect("component count checked")
            .contribution
            .member_identity_contract_id()
            .to_owned();
        let adapter_id = adapter_id.into();
        let support_release_id = support_release_id.into();
        let source_declaration_id = source_declaration_id.into();
        let mut value = Self {
            composition_contract_version: CATALOG_SOURCE_COMPOSITION_CONTRACT_VERSION,
            composition_id: CatalogCompositionId::from_digest([0; DIGEST_BYTES]),
            binding,
            adapter_id,
            support_release_id,
            source_declaration_id,
            member_identity_contract_id,
            components,
        };
        value.composition_id = value.calculate_id();
        value.validate()?;
        Ok(value)
    }

    pub(crate) fn validate(&self) -> Result<(), CatalogCompositionError> {
        if self.composition_contract_version != CATALOG_SOURCE_COMPOSITION_CONTRACT_VERSION {
            return Err(CatalogCompositionError::invalid(
                "unsupported catalog source composition version",
            ));
        }
        validate_identifier("adapter_id", &self.adapter_id)?;
        validate_identifier("support_release_id", &self.support_release_id)?;
        validate_identifier("source_declaration_id", &self.source_declaration_id)?;
        self.binding.validate()?;
        validate_identifier(
            "member_identity_contract_id",
            &self.member_identity_contract_id,
        )?;
        if self.components.is_empty() || self.components.len() > MAX_COMPONENTS {
            return Err(CatalogCompositionError::invalid(format!(
                "catalog composition must contain 1..={MAX_COMPONENTS} components"
            )));
        }
        if self
            .components
            .windows(2)
            .any(|pair| pair[0].component_id >= pair[1].component_id)
        {
            return Err(CatalogCompositionError::invalid(
                "catalog components must be strictly increasing by component ID",
            ));
        }
        let mut has_membership_authority = false;
        for component in &self.components {
            component.validate()?;
            if component.contribution.member_identity_contract_id()
                != self.member_identity_contract_id
            {
                return Err(CatalogCompositionError::invalid(
                    "all catalog components must share one member identity contract",
                ));
            }
            has_membership_authority |= component.contribution.can_admit_member();
        }
        if !has_membership_authority {
            return Err(CatalogCompositionError::invalid(
                "catalog metadata cannot fabricate membership; at least one membership authority is required",
            ));
        }
        if self.calculate_id() != self.composition_id {
            return Err(CatalogCompositionError::invalid(
                "catalog composition ID does not match its normalized content",
            ));
        }
        Ok(())
    }

    fn calculate_id(&self) -> CatalogCompositionId {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"spaghetti/rfc012b/catalog-composition-v1\0");
        hasher.update(&self.composition_contract_version.to_be_bytes());
        self.binding.hash_into(&mut hasher);
        hash_string(&mut hasher, &self.adapter_id);
        hash_string(&mut hasher, &self.support_release_id);
        hash_string(&mut hasher, &self.source_declaration_id);
        hash_string(&mut hasher, &self.member_identity_contract_id);
        hasher.update(&(self.components.len() as u64).to_be_bytes());
        for component in &self.components {
            component.hash_into(&mut hasher);
        }
        CatalogCompositionId::from_digest(*hasher.finalize().as_bytes())
    }

    fn component(&self, component_id: &str) -> Option<&CatalogSourceComponent> {
        self.components
            .binary_search_by(|component| component.component_id.as_str().cmp(component_id))
            .ok()
            .map(|index| &self.components[index])
    }

    pub(crate) fn authorize_execution<'composition, 'authorization>(
        &'composition self,
        authorization: AuthorizedCatalogAccess<'authorization>,
    ) -> Result<CatalogExecutableComposition<'composition, 'authorization>, CatalogCompositionError>
    {
        self.validate()?;
        if self.adapter_id != authorization.adapter_id() {
            return Err(CatalogCompositionError::invalid(
                "catalog authorization belongs to another adapter",
            ));
        }
        if self.support_release_id != authorization.support_release_id() {
            return Err(CatalogCompositionError::invalid(
                "catalog authorization belongs to another support release",
            ));
        }
        let expected_binding = CatalogPromotedBinding::from_digests(
            *authorization.source_declaration_digest().as_bytes(),
            *authorization.support_release_digest().as_bytes(),
        )?;
        expected_binding.validate()?;
        if self.binding.promoted_binding() != Some(expected_binding) {
            return Err(CatalogCompositionError::invalid(
                "catalog composition is planned/unbound or does not match the authorized promoted declaration binding",
            ));
        }
        Ok(CatalogExecutableComposition {
            composition: self,
            authorization,
        })
    }
}

pub(crate) struct CatalogExecutableComposition<'composition, 'authorization> {
    composition: &'composition CatalogSourceComposition,
    authorization: AuthorizedCatalogAccess<'authorization>,
}

impl CatalogExecutableComposition<'_, '_> {
    pub(crate) fn composition_id(&self) -> CatalogCompositionId {
        self.composition.composition_id
    }

    pub(crate) fn contract_selection(&self) -> &ContractVersionSelection {
        self.authorization.contracts()
    }

    fn library_plan_source(
        &self,
        source_instance_key: CanonicalSourceInstanceKey,
        access_policy_digest: CatalogAccessPolicyDigest,
    ) -> Result<CatalogCoveragePlanSource, CatalogCompositionError> {
        if !matches!(
            self.authorization.compatibility_class(),
            CompatibilityClass::ExactSupported | CompatibilityClass::RangeSupported
        ) {
            return Err(CatalogCompositionError::invalid(
                "forward or incompatible catalog authorization cannot publish normal complete coverage",
            ));
        }
        let selection = self.authorization.contracts();
        if selection.coverage_contract_version != SOURCE_COVERAGE_CONTRACT_VERSION
            || selection.query_pack_version != Some(CATALOG_QUERY_PACK_CONTRACT_VERSION)
        {
            return Err(CatalogCompositionError::invalid(
                "catalog coverage requires the exact selected RFC 012A coverage and RFC 012B query-pack v1 contracts",
            ));
        }
        let declaration_digest = CoverageDeclarationDigest::derive(
            self.authorization.source_declaration_digest().as_bytes(),
        )
        .map_err(|error| {
            CatalogCompositionError::invalid(format!(
                "authorized catalog declaration digest is invalid: {error}"
            ))
        })?;
        CatalogCoveragePlanSource::new(
            self.authorization.adapter_id(),
            source_instance_key,
            self.authorization.support_release_id(),
            declaration_digest,
            access_policy_digest,
        )
        .map_err(|error| {
            CatalogCompositionError::invalid(format!(
                "authorized catalog plan source is invalid: {error}"
            ))
        })
    }

    /// Assemble one complete library-scoped catalog coverage set from
    /// explicitly completed native component evidence. This consumes no source
    /// data and cannot widen a planned or forward-compatible authorization.
    pub(crate) fn assemble_library_coverage(
        &self,
        source_instance_key: CanonicalSourceInstanceKey,
        access_policy_digest: CatalogAccessPolicyDigest,
        membership: &CatalogMembershipSnapshot,
        mut completions: Vec<CatalogComponentCoverageCompletion>,
    ) -> Result<CatalogLibraryCoverageAssembly, CatalogCompositionError> {
        let plan_source = self.library_plan_source(source_instance_key, access_policy_digest)?;
        membership.validate_for(self.composition)?;

        completions.sort_by(|left, right| left.component_id.cmp(&right.component_id));
        if completions
            .windows(2)
            .any(|pair| pair[0].component_id == pair[1].component_id)
        {
            return Err(CatalogCompositionError::invalid(
                "catalog coverage contains duplicate component completion",
            ));
        }
        if !self
            .composition
            .components
            .iter()
            .map(|component| component.component_id.as_str())
            .eq(completions
                .iter()
                .map(|completion| completion.component_id.as_str()))
        {
            return Err(CatalogCompositionError::invalid(
                "complete catalog coverage requires exactly one completion for every composition component",
            ));
        }

        let mut point_count = 0_usize;
        let mut absence_count = 0_usize;
        for completion in &completions {
            completion.validate_for(
                self.composition,
                &plan_source,
                self.authorization.contracts(),
            )?;
            point_count = point_count
                .checked_add(completion.point_count())
                .ok_or_else(|| CatalogCompositionError::invalid("coverage point count overflow"))?;
            absence_count = absence_count
                .checked_add(completion.absence_count())
                .ok_or_else(|| {
                    CatalogCompositionError::invalid("coverage absence count overflow")
                })?;
            if point_count > MAX_CATALOG_COVERAGE_POINTS
                || absence_count > MAX_CATALOG_COVERAGE_ABSENCES
            {
                return Err(CatalogCompositionError::invalid(
                    "catalog coverage exceeds the RFC 012A portable set bounds",
                ));
            }

            let component = self
                .composition
                .component(&completion.component_id)
                .expect("completion IDs were matched to the composition");
            if component.contribution.can_admit_member() {
                let authority = membership
                    .authority_evidence
                    .binary_search_by(|evidence| {
                        evidence.component_id.as_str().cmp(&completion.component_id)
                    })
                    .ok()
                    .map(|index| &membership.authority_evidence[index])
                    .ok_or_else(|| {
                        CatalogCompositionError::invalid(
                            "admitting component completion has no membership authority",
                        )
                    })?;
                if authority.coverage_proof
                    != calculate_component_coverage_proof(
                        self.composition,
                        &membership.members,
                        completion,
                    )
                {
                    return Err(CatalogCompositionError::invalid(
                        "component coverage does not match its completed membership authority proof",
                    ));
                }
            }
        }

        let domain = CoverageDomain::ProjectionPack {
            pack: CATALOG_PROJECTION_PACK_ID.to_owned(),
            version: CATALOG_QUERY_PACK_CONTRACT_VERSION,
        };
        let generic_scope = plan_source.coverage_scope(CatalogCoverageScope::Library);
        let mut points = Vec::with_capacity(point_count);
        let mut absences = Vec::with_capacity(absence_count);
        let mut declared_streams = Vec::with_capacity(self.composition.components.len());

        for (component, completion) in self.composition.components.iter().zip(&completions) {
            let stream_key = CoverageStreamKey::derive(
                self.authorization.adapter_id(),
                component.stream_id.as_bytes(),
            )
            .map_err(|error| {
                CatalogCompositionError::invalid(format!(
                    "catalog coverage stream identity is invalid: {error}"
                ))
            })?;
            declared_streams.push(stream_key);
            let (position_kind, status) = component.primitive.complete_coverage_semantics();
            for object in &completion.objects {
                match object {
                    CatalogCompletedCoverageObject::Point {
                        object_key,
                        generation,
                        position,
                        provenance,
                    } => {
                        if position.kind != position_kind {
                            return Err(CatalogCompositionError::invalid(
                                "catalog coverage position kind does not match its source primitive",
                            ));
                        }
                        points.push(
                            SourceCoveragePoint::new(
                                domain.clone(),
                                self.authorization.adapter_id(),
                                source_instance_key,
                                stream_key,
                                *object_key,
                                *generation,
                                Some(position.clone()),
                                status.clone(),
                                provenance.clone(),
                            )
                            .map_err(|error| {
                                CatalogCompositionError::invalid(format!(
                                    "catalog coverage point is invalid: {error}"
                                ))
                            })?,
                        );
                    }
                    CatalogCompletedCoverageObject::ExplicitAbsence {
                        object_key,
                        generation,
                        kind,
                    } => absences.push(CoverageAbsence {
                        stream_key,
                        object_key: *object_key,
                        generation: *generation,
                        kind: *kind,
                    }),
                }
            }
        }

        declared_streams.sort_unstable();
        declared_streams.dedup();
        points.sort_by_key(|point| (point.stream_key, point.object_key, point.generation));
        absences.sort();
        let mut membership_objects = points
            .iter()
            .map(|point| CoverageMembershipObject {
                stream_key: point.stream_key.as_bytes(),
                object_key: point.object_key.as_bytes(),
                generation: point.generation,
                absent: false,
            })
            .chain(absences.iter().map(|absence| CoverageMembershipObject {
                stream_key: absence.stream_key.as_bytes(),
                object_key: absence.object_key.as_bytes(),
                generation: absence.generation,
                absent: true,
            }))
            .collect::<Vec<_>>();
        membership_objects.sort_by(|left, right| {
            (left.stream_key, left.object_key, left.generation).cmp(&(
                right.stream_key,
                right.object_key,
                right.generation,
            ))
        });
        if membership_objects.windows(2).any(|pair| {
            (pair[0].stream_key, pair[0].object_key, pair[0].generation)
                >= (pair[1].stream_key, pair[1].object_key, pair[1].generation)
        }) {
            return Err(CatalogCompositionError::invalid(
                "catalog coverage contains a duplicate native object generation",
            ));
        }
        validate_membership_object_lineage(&membership_objects)?;
        let stream_bytes = declared_streams
            .iter()
            .map(|stream| stream.as_bytes().as_slice())
            .collect::<Vec<_>>();
        let prefix = source_membership_prefix(&domain).map_err(|error| {
            CatalogCompositionError::invalid(format!(
                "catalog projection membership domain is unsupported: {error}"
            ))
        })?;
        let coverage_membership_revision =
            derive_coverage_membership_revision(&prefix, &stream_bytes, &membership_objects)
                .map_err(|error| {
                    CatalogCompositionError::invalid(format!(
                        "catalog coverage membership is invalid: {error}"
                    ))
                })?;
        let source_coverage = SourceCoverageSet::new(
            domain,
            generic_scope,
            coverage_membership_revision,
            points,
            absences,
            Vec::new(),
            CoverageSetCompleteness::Complete,
        )
        .map_err(|error| {
            CatalogCompositionError::invalid(format!("catalog coverage set is invalid: {error}"))
        })?;
        if !plan_source.matches_coverage(&source_coverage) {
            return Err(CatalogCompositionError::invalid(
                "catalog coverage is not bound to its derived coverage-plan source",
            ));
        }

        let component_completion_revision = calculate_component_completion_revision(
            self.composition,
            membership.membership_revision,
            self.authorization.contracts(),
            &plan_source,
            &completions,
        );

        Ok(CatalogLibraryCoverageAssembly {
            catalog_membership_revision: membership.membership_revision,
            component_completion_revision,
            contract_selection: self.authorization.contracts().clone(),
            plan_source,
            source_coverage,
            member_identity_contract_id: membership.member_identity_contract_id.clone(),
            member_refs: membership
                .members
                .iter()
                .map(|member| member.member_ref)
                .collect(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CatalogCompletedCoverageObject {
    Point {
        object_key: CoverageObjectKey,
        generation: u64,
        position: CoveragePosition,
        provenance: CoverageProvenance,
    },
    ExplicitAbsence {
        object_key: CoverageObjectKey,
        generation: u64,
        kind: CoverageAbsenceKind,
    },
}

impl CatalogCompletedCoverageObject {
    pub(crate) fn point(
        object_key: CoverageObjectKey,
        generation: u64,
        position: CoveragePosition,
        provenance: CoverageProvenance,
    ) -> Result<Self, CatalogCompositionError> {
        validate_coverage_generation(generation)?;
        Ok(Self::Point {
            object_key,
            generation,
            position,
            provenance,
        })
    }

    pub(crate) fn explicit_absence(
        object_key: CoverageObjectKey,
        generation: u64,
        kind: CoverageAbsenceKind,
    ) -> Result<Self, CatalogCompositionError> {
        validate_coverage_generation(generation)?;
        Ok(Self::ExplicitAbsence {
            object_key,
            generation,
            kind,
        })
    }

    fn coordinate(&self) -> (CoverageObjectKey, u64) {
        match self {
            Self::Point {
                object_key,
                generation,
                ..
            }
            | Self::ExplicitAbsence {
                object_key,
                generation,
                ..
            } => (*object_key, *generation),
        }
    }

    fn hash_into(&self, hasher: &mut blake3::Hasher) {
        match self {
            Self::Point {
                object_key,
                generation,
                position,
                provenance,
            } => {
                hash_string(hasher, "point");
                hasher.update(object_key.as_bytes());
                hasher.update(&generation.to_be_bytes());
                hash_coverage_position(hasher, position);
                hash_coverage_provenance(hasher, provenance);
            }
            Self::ExplicitAbsence {
                object_key,
                generation,
                kind,
            } => {
                hash_string(hasher, "explicit_absence");
                hasher.update(object_key.as_bytes());
                hasher.update(&generation.to_be_bytes());
                hash_string(
                    hasher,
                    match kind {
                        CoverageAbsenceKind::Absent => "absent",
                        CoverageAbsenceKind::Deleted => "deleted",
                    },
                );
            }
        }
    }
}

/// Complete, already-derived native coverage for one composition component.
/// It carries only opaque RFC 012A identities and is deliberately not a wire
/// DTO or a source-reading interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CatalogComponentCoverageCompletion {
    composition_id: CatalogCompositionId,
    contract_selection: ContractVersionSelection,
    source_instance_key: CanonicalSourceInstanceKey,
    coverage_binding_digest: CoverageDeclarationDigest,
    component_id: String,
    completion_position: CoveragePosition,
    objects: Vec<CatalogCompletedCoverageObject>,
}

impl CatalogComponentCoverageCompletion {
    pub(crate) fn new(
        executable: &CatalogExecutableComposition<'_, '_>,
        source_instance_key: CanonicalSourceInstanceKey,
        access_policy_digest: CatalogAccessPolicyDigest,
        component_id: impl Into<String>,
        completion_position: CoveragePosition,
        mut objects: Vec<CatalogCompletedCoverageObject>,
    ) -> Result<Self, CatalogCompositionError> {
        let plan_source =
            executable.library_plan_source(source_instance_key, access_policy_digest)?;
        if objects.len() > MAX_CATALOG_COVERAGE_POINTS + MAX_CATALOG_COVERAGE_ABSENCES {
            return Err(CatalogCompositionError::invalid(
                "component coverage exceeds the combined RFC 012A portable set bounds",
            ));
        }
        let point_count = objects
            .iter()
            .filter(|object| matches!(object, CatalogCompletedCoverageObject::Point { .. }))
            .count();
        if point_count > MAX_CATALOG_COVERAGE_POINTS
            || objects.len() - point_count > MAX_CATALOG_COVERAGE_ABSENCES
        {
            return Err(CatalogCompositionError::invalid(
                "component coverage exceeds the RFC 012A portable set bounds",
            ));
        }
        objects.sort_by_key(CatalogCompletedCoverageObject::coordinate);
        let value = Self {
            composition_id: executable.composition.composition_id,
            contract_selection: executable.authorization.contracts().clone(),
            source_instance_key,
            coverage_binding_digest: plan_source.coverage_binding_digest(),
            component_id: component_id.into(),
            completion_position,
            objects,
        };
        value.validate_for(
            executable.composition,
            &plan_source,
            executable.authorization.contracts(),
        )?;
        Ok(value)
    }

    fn validate_for(
        &self,
        composition: &CatalogSourceComposition,
        plan_source: &CatalogCoveragePlanSource,
        contract_selection: &ContractVersionSelection,
    ) -> Result<(), CatalogCompositionError> {
        validate_identifier("coverage completion component ID", &self.component_id)?;
        if self.composition_id != composition.composition_id
            || &self.contract_selection != contract_selection
            || self.source_instance_key != plan_source.source_instance_key
            || self.coverage_binding_digest != plan_source.coverage_binding_digest()
        {
            return Err(CatalogCompositionError::invalid(
                "component coverage belongs to another composition, source instance, declaration, or access policy",
            ));
        }
        let component = composition.component(&self.component_id).ok_or_else(|| {
            CatalogCompositionError::invalid("component coverage names an unknown component")
        })?;
        if self.completion_position.kind != CoveragePositionKind::SnapshotRevision
            || self.completion_position.monotonic_order.is_some()
        {
            return Err(CatalogCompositionError::invalid(
                "component completion requires an opaque unordered snapshot revision",
            ));
        }
        let point_count = self.point_count();
        let absence_count = self.absence_count();
        if point_count > MAX_CATALOG_COVERAGE_POINTS
            || absence_count > MAX_CATALOG_COVERAGE_ABSENCES
        {
            return Err(CatalogCompositionError::invalid(
                "component coverage exceeds the RFC 012A portable set bounds",
            ));
        }
        if self
            .objects
            .windows(2)
            .any(|pair| pair[0].coordinate() >= pair[1].coordinate())
        {
            return Err(CatalogCompositionError::invalid(
                "component coverage objects must be strictly increasing and unique",
            ));
        }
        validate_completion_object_lineage(&self.objects)?;
        let expected_position = component.primitive.complete_coverage_semantics().0;
        for object in &self.objects {
            validate_coverage_generation(object.coordinate().1)?;
            if let CatalogCompletedCoverageObject::Point { position, .. } = object {
                if position.kind != expected_position {
                    return Err(CatalogCompositionError::invalid(
                        "catalog coverage position kind does not match its source primitive",
                    ));
                }
            }
        }
        Ok(())
    }

    fn point_count(&self) -> usize {
        self.objects
            .iter()
            .filter(|object| matches!(object, CatalogCompletedCoverageObject::Point { .. }))
            .count()
    }

    fn absence_count(&self) -> usize {
        self.objects
            .iter()
            .filter(|object| {
                matches!(
                    object,
                    CatalogCompletedCoverageObject::ExplicitAbsence { .. }
                )
            })
            .count()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CatalogLibraryCoverageAssembly {
    catalog_membership_revision: CatalogMembershipRevision,
    component_completion_revision: CatalogComponentCompletionRevision,
    contract_selection: ContractVersionSelection,
    plan_source: CatalogCoveragePlanSource,
    source_coverage: SourceCoverageSet,
    member_identity_contract_id: String,
    member_refs: Vec<CatalogMemberRef>,
}

impl CatalogLibraryCoverageAssembly {
    pub(crate) fn catalog_membership_revision(&self) -> CatalogMembershipRevision {
        self.catalog_membership_revision
    }

    pub(crate) fn component_completion_revision(&self) -> CatalogComponentCompletionRevision {
        self.component_completion_revision
    }

    pub(crate) fn contract_selection(&self) -> &ContractVersionSelection {
        &self.contract_selection
    }

    pub(crate) fn plan_source(&self) -> &CatalogCoveragePlanSource {
        &self.plan_source
    }

    pub(crate) fn source_coverage(&self) -> &SourceCoverageSet {
        &self.source_coverage
    }

    /// Project this checked B2 result into the adapter-neutral input consumed
    /// by the B3 initial-publication envelope. The projection retains the
    /// exact member set and identity contract instead of treating coverage
    /// membership as catalog membership.
    pub(crate) fn complete_publication_source(
        &self,
    ) -> Result<CatalogCompleteSourceAssembly, CatalogContractError> {
        CatalogCompleteSourceAssembly::from_complete_library_coverage(
            self.plan_source.clone(),
            self.contract_selection.clone(),
            self.member_identity_contract_id.clone(),
            CatalogSourceMembershipRevision::from_digest(
                *self.catalog_membership_revision.as_bytes(),
            ),
            CatalogSourceCompletionRevision::from_digest(
                *self.component_completion_revision.as_bytes(),
            ),
            self.member_refs
                .iter()
                .map(|member_ref| CatalogPublicationMemberRef::from_digest(*member_ref.as_bytes()))
                .collect(),
            self.source_coverage.clone(),
        )
    }
}

fn validate_coverage_generation(generation: u64) -> Result<(), CatalogCompositionError> {
    if generation == 0 || generation > MAX_PORTABLE_GENERATION {
        return Err(CatalogCompositionError::invalid(
            "catalog coverage generation must be a positive portable integer",
        ));
    }
    Ok(())
}

/// A complete current set may carry one old explicit absence plus one newer
/// live generation to establish a replacement. It cannot carry two live
/// generations, two absence histories, or a non-forward replacement.
fn validate_completion_object_lineage(
    objects: &[CatalogCompletedCoverageObject],
) -> Result<(), CatalogCompositionError> {
    let mut start = 0_usize;
    while start < objects.len() {
        let object_key = objects[start].coordinate().0;
        let mut end = start + 1;
        while end < objects.len() && objects[end].coordinate().0 == object_key {
            end += 1;
        }

        let mut point_generation = None;
        let mut absence_generation = None;
        for object in &objects[start..end] {
            match object {
                CatalogCompletedCoverageObject::Point { generation, .. } => {
                    if point_generation.replace(*generation).is_some() {
                        return Err(CatalogCompositionError::invalid(
                            "component coverage cannot contain two live generations for one object",
                        ));
                    }
                }
                CatalogCompletedCoverageObject::ExplicitAbsence { generation, .. } => {
                    if absence_generation.replace(*generation).is_some() {
                        return Err(CatalogCompositionError::invalid(
                            "component coverage cannot contain two absence generations for one object",
                        ));
                    }
                }
            }
        }
        if matches!(
            (absence_generation, point_generation),
            (Some(absent), Some(present)) if absent >= present
        ) {
            return Err(CatalogCompositionError::invalid(
                "component replacement requires an older explicit absence and a newer live generation",
            ));
        }
        start = end;
    }
    Ok(())
}

fn validate_membership_object_lineage(
    objects: &[CoverageMembershipObject<'_>],
) -> Result<(), CatalogCompositionError> {
    let mut start = 0_usize;
    while start < objects.len() {
        let coordinate = (objects[start].stream_key, objects[start].object_key);
        let mut end = start + 1;
        while end < objects.len()
            && (objects[end].stream_key, objects[end].object_key) == coordinate
        {
            end += 1;
        }
        let mut live_generation = None;
        let mut absent_generation = None;
        for object in &objects[start..end] {
            let generation = if object.absent {
                &mut absent_generation
            } else {
                &mut live_generation
            };
            if generation.replace(object.generation).is_some() {
                return Err(CatalogCompositionError::invalid(
                    "catalog coverage cannot contain multiple live or absent generations for one native object",
                ));
            }
        }
        if matches!(
            (absent_generation, live_generation),
            (Some(absent), Some(live)) if absent >= live
        ) {
            return Err(CatalogCompositionError::invalid(
                "catalog coverage replacement requires an older absence and a newer live generation",
            ));
        }
        start = end;
    }
    Ok(())
}

fn calculate_component_coverage_proof(
    composition: &CatalogSourceComposition,
    members: &[CatalogMembershipEntry],
    completion: &CatalogComponentCoverageCompletion,
) -> CatalogCoverageProof {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"spaghetti/rfc012b/catalog-component-coverage-v1\0");
    hasher.update(composition.composition_id.as_bytes());
    hash_contract_selection(&mut hasher, &completion.contract_selection);
    hasher.update(completion.source_instance_key.as_bytes());
    hasher.update(completion.coverage_binding_digest.as_bytes());
    hash_string(&mut hasher, &completion.component_id);
    hash_coverage_position(&mut hasher, &completion.completion_position);
    hasher.update(&(completion.objects.len() as u64).to_be_bytes());
    for object in &completion.objects {
        object.hash_into(&mut hasher);
    }
    let mut related_members = members
        .iter()
        .filter_map(|member| {
            let admitting = member
                .admitting_component_ids
                .binary_search(&completion.component_id)
                .is_ok();
            let metadata = member
                .metadata_component_ids
                .binary_search(&completion.component_id)
                .is_ok();
            (admitting || metadata).then_some((member.member_ref, admitting, metadata))
        })
        .collect::<Vec<_>>();
    related_members.sort_unstable();
    hasher.update(&(related_members.len() as u64).to_be_bytes());
    for (member_ref, admitting, metadata) in related_members {
        hasher.update(member_ref.as_bytes());
        hasher.update(&[u8::from(admitting), u8::from(metadata)]);
    }
    CatalogCoverageProof::from_digest(*hasher.finalize().as_bytes())
}

fn calculate_component_completion_revision(
    composition: &CatalogSourceComposition,
    membership_revision: CatalogMembershipRevision,
    contract_selection: &ContractVersionSelection,
    plan_source: &CatalogCoveragePlanSource,
    completions: &[CatalogComponentCoverageCompletion],
) -> CatalogComponentCompletionRevision {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"spaghetti/rfc012b/catalog-component-completion-revision-v1\0");
    hasher.update(composition.composition_id.as_bytes());
    hasher.update(membership_revision.as_bytes());
    hash_contract_selection(&mut hasher, contract_selection);
    hash_string(&mut hasher, &plan_source.adapter_id);
    hasher.update(plan_source.source_instance_key.as_bytes());
    hash_string(&mut hasher, &plan_source.support_release_id);
    hasher.update(plan_source.catalog_declaration_digest.as_bytes());
    // This catalog-specific declaration digest is derived over the exact
    // declaration plus access-policy digest, so the restart identity cannot
    // survive policy drift without exposing the policy digest here.
    hasher.update(plan_source.coverage_binding_digest().as_bytes());
    hasher.update(&(completions.len() as u64).to_be_bytes());
    for completion in completions {
        hash_string(&mut hasher, &completion.component_id);
        hash_coverage_position(&mut hasher, &completion.completion_position);
        hasher.update(&(completion.objects.len() as u64).to_be_bytes());
        for object in &completion.objects {
            object.hash_into(&mut hasher);
        }
    }
    CatalogComponentCompletionRevision::from_digest(*hasher.finalize().as_bytes())
}

fn hash_contract_selection(hasher: &mut blake3::Hasher, selection: &ContractVersionSelection) {
    hasher.update(&selection.selection_contract_version.to_be_bytes());
    hasher.update(&selection.model_major.to_be_bytes());
    hasher.update(&selection.external_entity_reference_version.to_be_bytes());
    hasher.update(&selection.semantic_revision_reference_version.to_be_bytes());
    hasher.update(&selection.coverage_contract_version.to_be_bytes());
    hasher.update(&(selection.fact_family_versions.len() as u64).to_be_bytes());
    for (family, version) in &selection.fact_family_versions {
        hash_string(hasher, family);
        hasher.update(&version.to_be_bytes());
    }
    hash_optional_contract_version(hasher, selection.query_pack_version);
    hash_optional_contract_version(hasher, selection.observation_contract_version);
}

fn hash_optional_contract_version(hasher: &mut blake3::Hasher, version: Option<u32>) {
    match version {
        Some(version) => {
            hasher.update(&[1]);
            hasher.update(&version.to_be_bytes());
        }
        None => {
            hasher.update(&[0]);
        }
    }
}

fn hash_coverage_position(hasher: &mut blake3::Hasher, position: &CoveragePosition) {
    hash_string(
        hasher,
        match position.kind {
            CoveragePositionKind::AppendCursor => "append_cursor",
            CoveragePositionKind::DocumentRevision => "document_revision",
            CoveragePositionKind::SnapshotRevision => "snapshot_revision",
            CoveragePositionKind::DatabaseWatermark => "database_watermark",
            CoveragePositionKind::KeyRangeToken => "key_range_token",
        },
    );
    hasher.update(position.opaque.as_bytes());
    match position.monotonic_order {
        Some(order) => {
            hasher.update(&[1]);
            hasher.update(&order.to_be_bytes());
        }
        None => {
            hasher.update(&[0]);
        }
    }
}

fn hash_coverage_provenance(hasher: &mut blake3::Hasher, provenance: &CoverageProvenance) {
    match provenance.source_record_id {
        Some(source_record_id) => {
            hasher.update(&[1]);
            hasher.update(source_record_id.as_bytes());
        }
        None => {
            hasher.update(&[0]);
        }
    }
    match provenance.semantic_revision_ref {
        Some(reference) => {
            hasher.update(&[1]);
            hasher.update(&reference.semantic_reference_contract_version.to_be_bytes());
            hasher.update(reference.fact_revision_id.as_bytes());
        }
        None => {
            hasher.update(&[0]);
        }
    }
    match provenance.observed_at {
        Some(observed_at) => {
            hasher.update(&[1]);
            hasher.update(&observed_at.to_be_bytes());
        }
        None => {
            hasher.update(&[0]);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CatalogMembershipAuthorityCompleteness {
    Complete,
    Partial,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CatalogMembershipAuthorityEvidence {
    component_id: String,
    generation: u64,
    authority_revision: CatalogAuthorityRevision,
    coverage_proof: CatalogCoverageProof,
    completeness: CatalogMembershipAuthorityCompleteness,
}

impl CatalogMembershipAuthorityEvidence {
    #[cfg(test)]
    fn fixture(
        component_id: &str,
        generation: u64,
        completeness: CatalogMembershipAuthorityCompleteness,
    ) -> Self {
        Self {
            component_id: component_id.to_owned(),
            generation,
            authority_revision: CatalogAuthorityRevision::from_digest(
                *blake3::hash(format!("{component_id}/membership-revision").as_bytes()).as_bytes(),
            ),
            coverage_proof: CatalogCoverageProof::from_digest(
                *blake3::hash(format!("{component_id}/coverage-proof").as_bytes()).as_bytes(),
            ),
            completeness,
        }
    }

    fn validate_for(
        &self,
        composition: &CatalogSourceComposition,
    ) -> Result<(), CatalogCompositionError> {
        validate_identifier("membership authority component ID", &self.component_id)?;
        if self.generation == 0 {
            return Err(CatalogCompositionError::invalid(
                "membership authority generation must be greater than zero",
            ));
        }
        if self.authority_revision.as_bytes() == &[0; DIGEST_BYTES]
            || self.coverage_proof.as_bytes() == &[0; DIGEST_BYTES]
        {
            return Err(CatalogCompositionError::invalid(
                "membership authority requires nonzero revision and coverage proof digests",
            ));
        }
        if self.completeness != CatalogMembershipAuthorityCompleteness::Complete {
            return Err(CatalogCompositionError::invalid(
                "only complete membership-authority evidence can publish a membership snapshot",
            ));
        }
        let component = composition.component(&self.component_id).ok_or_else(|| {
            CatalogCompositionError::invalid(
                "membership authority evidence names an unknown component",
            )
        })?;
        if !component.contribution.can_admit_member() {
            return Err(CatalogCompositionError::invalid(
                "membership authority evidence names a metadata-only component",
            ));
        }
        Ok(())
    }

    fn hash_into(&self, hasher: &mut blake3::Hasher) {
        hash_string(hasher, &self.component_id);
        hasher.update(&self.generation.to_be_bytes());
        hasher.update(self.authority_revision.as_bytes());
        hasher.update(self.coverage_proof.as_bytes());
        hash_string(
            hasher,
            match self.completeness {
                CatalogMembershipAuthorityCompleteness::Complete => "complete",
                CatalogMembershipAuthorityCompleteness::Partial => "partial",
                CatalogMembershipAuthorityCompleteness::Unavailable => "unavailable",
            },
        );
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CatalogMembershipEntry {
    member_ref: CatalogMemberRef,
    admitting_component_ids: Vec<String>,
    metadata_component_ids: Vec<String>,
}

impl CatalogMembershipEntry {
    pub(crate) fn new(
        member_ref: CatalogMemberRef,
        mut admitting_component_ids: Vec<String>,
        mut metadata_component_ids: Vec<String>,
    ) -> Result<Self, CatalogCompositionError> {
        canonicalize_unique_strings(
            "admitting_component_ids",
            &mut admitting_component_ids,
            MAX_COMPONENTS,
            |value| validate_identifier("admitting component ID", value),
        )?;
        if metadata_component_ids.len() > MAX_COMPONENTS {
            return Err(CatalogCompositionError::invalid(format!(
                "metadata_component_ids exceeds {MAX_COMPONENTS} values"
            )));
        }
        for value in &metadata_component_ids {
            validate_identifier("metadata component ID", value)?;
        }
        metadata_component_ids.sort();
        if metadata_component_ids
            .windows(2)
            .any(|pair| pair[0] == pair[1])
        {
            return Err(CatalogCompositionError::invalid(
                "metadata_component_ids contains a duplicate value",
            ));
        }
        Ok(Self {
            member_ref,
            admitting_component_ids,
            metadata_component_ids,
        })
    }

    fn validate_for(
        &self,
        composition: &CatalogSourceComposition,
    ) -> Result<(), CatalogCompositionError> {
        validate_canonical_strings(
            "admitting_component_ids",
            &self.admitting_component_ids,
            MAX_COMPONENTS,
            |value| validate_identifier("admitting component ID", value),
        )?;
        validate_optional_canonical_strings(
            "metadata_component_ids",
            &self.metadata_component_ids,
            MAX_COMPONENTS,
            |value| validate_identifier("metadata component ID", value),
        )?;
        for component_id in &self.admitting_component_ids {
            let component = composition.component(component_id).ok_or_else(|| {
                CatalogCompositionError::invalid(
                    "membership entry names an unknown admitting component",
                )
            })?;
            if !component.contribution.can_admit_member() {
                return Err(CatalogCompositionError::invalid(
                    "metadata-only evidence cannot admit a catalog member",
                ));
            }
        }
        for component_id in &self.metadata_component_ids {
            let component = composition.component(component_id).ok_or_else(|| {
                CatalogCompositionError::invalid(
                    "membership entry names an unknown metadata component",
                )
            })?;
            if !component.contribution.can_supply_metadata() {
                return Err(CatalogCompositionError::invalid(
                    "component is not authorized to supply catalog metadata",
                ));
            }
        }
        Ok(())
    }

    fn hash_into(&self, hasher: &mut blake3::Hasher) {
        hasher.update(self.member_ref.as_bytes());
        hasher.update(&(self.admitting_component_ids.len() as u64).to_be_bytes());
        for component_id in &self.admitting_component_ids {
            hash_string(hasher, component_id);
        }
        hasher.update(&(self.metadata_component_ids.len() as u64).to_be_bytes());
        for component_id in &self.metadata_component_ids {
            hash_string(hasher, component_id);
        }
    }
}

fn validate_optional_canonical_strings(
    label: &str,
    values: &[String],
    max: usize,
    validator: impl Fn(&str) -> Result<(), CatalogCompositionError>,
) -> Result<(), CatalogCompositionError> {
    if values.len() > max {
        return Err(CatalogCompositionError::invalid(format!(
            "{label} exceeds {max} values"
        )));
    }
    for value in values {
        validator(value)?;
    }
    if values.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(CatalogCompositionError::invalid(format!(
            "{label} must be strictly increasing"
        )));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
/// Bounded composition-conformance evidence, not a durable catalog projection.
pub(crate) struct CatalogMembershipSnapshot {
    membership_contract_version: u32,
    composition_id: CatalogCompositionId,
    member_identity_contract_id: String,
    membership_revision: CatalogMembershipRevision,
    authority_evidence: Vec<CatalogMembershipAuthorityEvidence>,
    members: Vec<CatalogMembershipEntry>,
}

impl CatalogMembershipSnapshot {
    pub(crate) fn new(
        composition: &CatalogSourceComposition,
        mut authority_evidence: Vec<CatalogMembershipAuthorityEvidence>,
        mut members: Vec<CatalogMembershipEntry>,
    ) -> Result<Self, CatalogCompositionError> {
        composition.validate()?;
        authority_evidence.sort_by(|left, right| left.component_id.cmp(&right.component_id));
        validate_membership_authorities(composition, &authority_evidence)?;
        if members.len() > MAX_MEMBERSHIP_MEMBERS {
            return Err(CatalogCompositionError::invalid(format!(
                "catalog membership exceeds {MAX_MEMBERSHIP_MEMBERS} members"
            )));
        }
        for member in &members {
            member.validate_for(composition)?;
        }
        members.sort_by_key(|member| member.member_ref);
        if members
            .windows(2)
            .any(|pair| pair[0].member_ref == pair[1].member_ref)
        {
            return Err(CatalogCompositionError::invalid(
                "catalog membership contains a duplicate member reference",
            ));
        }
        let mut value = Self {
            membership_contract_version: CATALOG_MEMBERSHIP_CONTRACT_VERSION,
            composition_id: composition.composition_id,
            member_identity_contract_id: composition.member_identity_contract_id.clone(),
            membership_revision: CatalogMembershipRevision::from_digest([0; DIGEST_BYTES]),
            authority_evidence,
            members,
        };
        value.membership_revision = value.calculate_revision();
        value.validate_for(composition)?;
        Ok(value)
    }

    pub(crate) fn validate_for(
        &self,
        composition: &CatalogSourceComposition,
    ) -> Result<(), CatalogCompositionError> {
        composition.validate()?;
        if self.membership_contract_version != CATALOG_MEMBERSHIP_CONTRACT_VERSION
            || self.composition_id != composition.composition_id
            || self.member_identity_contract_id != composition.member_identity_contract_id
        {
            return Err(CatalogCompositionError::invalid(
                "catalog membership is not bound to the expected composition",
            ));
        }
        if self.members.len() > MAX_MEMBERSHIP_MEMBERS {
            return Err(CatalogCompositionError::invalid(format!(
                "catalog membership exceeds {MAX_MEMBERSHIP_MEMBERS} members"
            )));
        }
        validate_membership_authorities(composition, &self.authority_evidence)?;
        let mut previous = None;
        for member in &self.members {
            if previous.is_some_and(|value| value >= member.member_ref) {
                return Err(CatalogCompositionError::invalid(
                    "catalog members must be strictly increasing",
                ));
            }
            member.validate_for(composition)?;
            previous = Some(member.member_ref);
        }
        if self.membership_revision != self.calculate_revision() {
            return Err(CatalogCompositionError::invalid(
                "catalog membership revision does not match its canonical members",
            ));
        }
        Ok(())
    }

    fn calculate_revision(&self) -> CatalogMembershipRevision {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"spaghetti/rfc012b/catalog-membership-v1\0");
        hasher.update(&self.membership_contract_version.to_be_bytes());
        hasher.update(self.composition_id.as_bytes());
        hash_string(&mut hasher, &self.member_identity_contract_id);
        hasher.update(&(self.authority_evidence.len() as u64).to_be_bytes());
        for evidence in &self.authority_evidence {
            evidence.hash_into(&mut hasher);
        }
        hasher.update(&(self.members.len() as u64).to_be_bytes());
        for member in &self.members {
            member.hash_into(&mut hasher);
        }
        CatalogMembershipRevision::from_digest(*hasher.finalize().as_bytes())
    }
}

fn validate_membership_authorities(
    composition: &CatalogSourceComposition,
    authority_evidence: &[CatalogMembershipAuthorityEvidence],
) -> Result<(), CatalogCompositionError> {
    if authority_evidence
        .windows(2)
        .any(|pair| pair[0].component_id >= pair[1].component_id)
    {
        return Err(CatalogCompositionError::invalid(
            "membership authority evidence must be strictly increasing by component ID",
        ));
    }
    for evidence in authority_evidence {
        evidence.validate_for(composition)?;
    }
    let expected = composition
        .components
        .iter()
        .filter(|component| component.contribution.can_admit_member())
        .map(|component| component.component_id.as_str());
    if !expected.eq(authority_evidence
        .iter()
        .map(|evidence| evidence.component_id.as_str()))
    {
        return Err(CatalogCompositionError::invalid(
            "membership snapshot requires exactly one complete authority proof for every admitting component",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CatalogRecordWindowSpec {
    max_record_bytes: u64,
    max_window_bytes: u64,
    max_records: u32,
}

impl CatalogRecordWindowSpec {
    fn plan_from_ordinal(
        self,
        complete_record_bytes: &[u64],
        incomplete_suffix_bytes: u64,
        start_record: u64,
    ) -> Result<CatalogRecordWindow, CatalogCompositionError> {
        validate_positive_bound("max_record_bytes", self.max_record_bytes, MAX_RECORD_BYTES)?;
        validate_positive_bound("max_window_bytes", self.max_window_bytes, MAX_WINDOW_BYTES)?;
        if self.max_window_bytes < self.max_record_bytes
            || self.max_records == 0
            || self.max_records > MAX_WINDOW_RECORDS
        {
            return Err(CatalogCompositionError::invalid(
                "record-window bounds are inconsistent",
            ));
        }
        let start = usize::try_from(start_record).map_err(|_| {
            CatalogCompositionError::invalid("record-window start does not fit this platform")
        })?;
        if start > complete_record_bytes.len() {
            return Err(CatalogCompositionError::invalid(
                "record-window start is beyond the framed record sequence",
            ));
        }

        let mut selected_records = 0_u32;
        let mut selected_bytes = 0_u64;
        let mut cursor = start;
        while cursor < complete_record_bytes.len() && selected_records < self.max_records {
            let record_bytes = complete_record_bytes[cursor];
            if record_bytes > self.max_record_bytes {
                return Ok(CatalogRecordWindow {
                    start_record,
                    selected_records,
                    selected_bytes,
                    remainder: CatalogRecordWindowRemainder::OversizedRecord {
                        record_index: cursor as u64,
                        observed_bytes: record_bytes,
                        complete: true,
                    },
                });
            }
            let next_bytes = selected_bytes.checked_add(record_bytes).ok_or_else(|| {
                CatalogCompositionError::invalid("record-window byte accounting overflowed")
            })?;
            if next_bytes > self.max_window_bytes {
                break;
            }
            selected_records += 1;
            selected_bytes = next_bytes;
            cursor += 1;
        }

        let remainder = if cursor < complete_record_bytes.len() {
            let next_record_bytes = complete_record_bytes[cursor];
            if next_record_bytes > self.max_record_bytes {
                CatalogRecordWindowRemainder::OversizedRecord {
                    record_index: cursor as u64,
                    observed_bytes: next_record_bytes,
                    complete: true,
                }
            } else {
                CatalogRecordWindowRemainder::ContinueAt {
                    next_record: cursor as u64,
                }
            }
        } else if incomplete_suffix_bytes > self.max_record_bytes {
            CatalogRecordWindowRemainder::OversizedRecord {
                record_index: cursor as u64,
                observed_bytes: incomplete_suffix_bytes,
                complete: false,
            }
        } else if incomplete_suffix_bytes > 0 {
            CatalogRecordWindowRemainder::AwaitingCompleteRecord {
                next_record: cursor as u64,
                observed_bytes: incomplete_suffix_bytes,
            }
        } else {
            CatalogRecordWindowRemainder::AtSnapshotBoundary {
                next_record: cursor as u64,
            }
        };

        Ok(CatalogRecordWindow {
            start_record,
            selected_records,
            selected_bytes,
            remainder,
        })
    }

    fn hash_into(self, hasher: &mut blake3::Hasher) {
        hasher.update(&self.max_record_bytes.to_be_bytes());
        hasher.update(&self.max_window_bytes.to_be_bytes());
        hasher.update(&self.max_records.to_be_bytes());
    }
}

/// Stateful conformance planner for one frozen component record sequence.
///
/// The only initial ordinal is zero. Every later window requires the opaque
/// token emitted by the immediately preceding window, so no caller can select
/// an arbitrary in-range ordinal and silently skip evidence.
pub(crate) struct CatalogRecordWindowPlanner {
    composition_id: CatalogCompositionId,
    component_id: String,
    spec: CatalogRecordWindowSpec,
    record_layout_digest: Option<[u8; DIGEST_BYTES]>,
    expected_continuation: Option<CatalogWindowContinuation>,
    expected_next_record: Option<u64>,
    next_token_sequence: u64,
    started: bool,
}

impl CatalogRecordWindowPlanner {
    pub(crate) fn new(
        composition: &CatalogSourceComposition,
        component_id: &str,
    ) -> Result<Self, CatalogCompositionError> {
        composition.validate()?;
        validate_identifier("component_id", component_id)?;
        let component = composition.component(component_id).ok_or_else(|| {
            CatalogCompositionError::invalid(
                "catalog record-window planner names a component outside the composition",
            )
        })?;
        let spec = component.primitive.window_spec().ok_or_else(|| {
            CatalogCompositionError::invalid(
                "catalog record-window planner requires a delimited head or prefix component",
            )
        })?;
        Ok(Self {
            composition_id: composition.composition_id,
            component_id: component_id.to_owned(),
            spec,
            record_layout_digest: None,
            expected_continuation: None,
            expected_next_record: None,
            next_token_sequence: 0,
            started: false,
        })
    }

    pub(crate) fn plan_initial(
        &mut self,
        complete_record_bytes: &[u64],
        incomplete_suffix_bytes: u64,
    ) -> Result<CatalogRecordWindowStep, CatalogCompositionError> {
        if self.started {
            return Err(CatalogCompositionError::invalid(
                "catalog record-window chain has already started",
            ));
        }
        let record_layout_digest =
            record_layout_digest(complete_record_bytes, incomplete_suffix_bytes)?;
        let window =
            self.spec
                .plan_from_ordinal(complete_record_bytes, incomplete_suffix_bytes, 0)?;
        self.started = true;
        self.record_layout_digest = Some(record_layout_digest);
        self.finish_step(window)
    }

    pub(crate) fn plan_continuation(
        &mut self,
        continuation: CatalogWindowContinuation,
        complete_record_bytes: &[u64],
        incomplete_suffix_bytes: u64,
    ) -> Result<CatalogRecordWindowStep, CatalogCompositionError> {
        if !self.started || self.expected_continuation != Some(continuation) {
            return Err(CatalogCompositionError::invalid(
                "catalog window continuation is replayed, forged, or belongs to another chain",
            ));
        }
        let record_layout_digest =
            record_layout_digest(complete_record_bytes, incomplete_suffix_bytes)?;
        if self.record_layout_digest != Some(record_layout_digest) {
            return Err(CatalogCompositionError::invalid(
                "catalog window continuation record layout changed within the frozen chain",
            ));
        }
        let start_record = self.expected_next_record.ok_or_else(|| {
            CatalogCompositionError::invalid("catalog window continuation has no bound next record")
        })?;
        let window = self.spec.plan_from_ordinal(
            complete_record_bytes,
            incomplete_suffix_bytes,
            start_record,
        )?;
        self.expected_continuation = None;
        self.expected_next_record = None;
        self.finish_step(window)
    }

    fn finish_step(
        &mut self,
        window: CatalogRecordWindow,
    ) -> Result<CatalogRecordWindowStep, CatalogCompositionError> {
        let continuation = match window.remainder.continuation_record() {
            Some(next_record) => {
                let record_layout_digest = self
                    .record_layout_digest
                    .expect("a started record-window chain has a frozen layout digest");
                let token = derive_window_continuation(
                    self.composition_id,
                    &self.component_id,
                    self.spec,
                    record_layout_digest,
                    next_record,
                    self.next_token_sequence,
                );
                self.next_token_sequence =
                    self.next_token_sequence.checked_add(1).ok_or_else(|| {
                        CatalogCompositionError::invalid(
                            "catalog window continuation sequence overflowed",
                        )
                    })?;
                self.expected_continuation = Some(token);
                self.expected_next_record = Some(next_record);
                Some(token)
            }
            None => {
                self.expected_continuation = None;
                self.expected_next_record = None;
                None
            }
        };
        Ok(CatalogRecordWindowStep {
            window,
            continuation,
        })
    }
}

fn record_layout_digest(
    complete_record_bytes: &[u64],
    incomplete_suffix_bytes: u64,
) -> Result<[u8; DIGEST_BYTES], CatalogCompositionError> {
    if complete_record_bytes.len() > MAX_CONFORMANCE_RECORDS {
        return Err(CatalogCompositionError::invalid(format!(
            "record-window conformance layout exceeds {MAX_CONFORMANCE_RECORDS} complete records"
        )));
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"spaghetti/rfc012b/catalog-record-layout-v1\0");
    hasher.update(&(complete_record_bytes.len() as u64).to_be_bytes());
    for record_bytes in complete_record_bytes {
        hasher.update(&record_bytes.to_be_bytes());
    }
    hasher.update(&incomplete_suffix_bytes.to_be_bytes());
    Ok(*hasher.finalize().as_bytes())
}

fn derive_window_continuation(
    composition_id: CatalogCompositionId,
    component_id: &str,
    spec: CatalogRecordWindowSpec,
    record_layout_digest: [u8; DIGEST_BYTES],
    next_record: u64,
    token_sequence: u64,
) -> CatalogWindowContinuation {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"spaghetti/rfc012b/catalog-window-continuation-v1\0");
    hasher.update(composition_id.as_bytes());
    hash_string(&mut hasher, component_id);
    spec.hash_into(&mut hasher);
    hasher.update(&record_layout_digest);
    hasher.update(&next_record.to_be_bytes());
    hasher.update(&token_sequence.to_be_bytes());
    CatalogWindowContinuation::from_digest(*hasher.finalize().as_bytes())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum CatalogRecordWindowRemainder {
    ContinueAt {
        next_record: u64,
    },
    AwaitingCompleteRecord {
        next_record: u64,
        observed_bytes: u64,
    },
    OversizedRecord {
        record_index: u64,
        observed_bytes: u64,
        complete: bool,
    },
    AtSnapshotBoundary {
        next_record: u64,
    },
}

impl CatalogRecordWindowRemainder {
    pub(crate) fn continuation_record(self) -> Option<u64> {
        match self {
            Self::ContinueAt { next_record } => Some(next_record),
            Self::AwaitingCompleteRecord { .. }
            | Self::OversizedRecord { .. }
            | Self::AtSnapshotBoundary { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct CatalogRecordWindow {
    start_record: u64,
    selected_records: u32,
    selected_bytes: u64,
    remainder: CatalogRecordWindowRemainder,
}

impl CatalogRecordWindow {
    pub(crate) fn end_record(self) -> u64 {
        self.start_record + u64::from(self.selected_records)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct CatalogRecordWindowStep {
    window: CatalogRecordWindow,
    continuation: Option<CatalogWindowContinuation>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct CatalogDecodeWitness {
    source_record_id: [u8; DIGEST_BYTES],
    disposition: [u8; DIGEST_BYTES],
    fact_revisions: [u8; DIGEST_BYTES],
    semantic_payload: [u8; DIGEST_BYTES],
    qualified_provenance: [u8; DIGEST_BYTES],
    decoder_state_after: [u8; DIGEST_BYTES],
}

impl CatalogDecodeWitness {
    pub(crate) fn from_digests(
        source_record_id: [u8; DIGEST_BYTES],
        disposition: [u8; DIGEST_BYTES],
        fact_revisions: [u8; DIGEST_BYTES],
        semantic_payload: [u8; DIGEST_BYTES],
        qualified_provenance: [u8; DIGEST_BYTES],
        decoder_state_after: [u8; DIGEST_BYTES],
    ) -> Self {
        Self {
            source_record_id,
            disposition,
            fact_revisions,
            semantic_payload,
            qualified_provenance,
            decoder_state_after,
        }
    }
}

pub(crate) struct CatalogDecodeTraceAccumulator {
    hasher: blake3::Hasher,
    record_count: u64,
}

impl CatalogDecodeTraceAccumulator {
    pub(crate) fn new(
        composition: &CatalogSourceComposition,
        component_id: &str,
    ) -> Result<Self, CatalogCompositionError> {
        composition.validate()?;
        validate_identifier("component_id", component_id)?;
        let component = composition.component(component_id).ok_or_else(|| {
            CatalogCompositionError::invalid(
                "catalog decode trace names a component outside the composition",
            )
        })?;
        if component.primitive.window_spec().is_none() {
            return Err(CatalogCompositionError::invalid(
                "catalog decode trace requires a delimited head or prefix component",
            ));
        }
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"spaghetti/rfc012b/catalog-decode-trace-v1\0");
        hasher.update(composition.composition_id.as_bytes());
        hash_string(&mut hasher, component_id);
        Ok(Self {
            hasher,
            record_count: 0,
        })
    }

    pub(crate) fn push(
        &mut self,
        witness: CatalogDecodeWitness,
    ) -> Result<(), CatalogCompositionError> {
        let next_record_count = self.record_count.checked_add(1).ok_or_else(|| {
            CatalogCompositionError::invalid("catalog decode trace record count overflowed")
        })?;
        self.hasher.update(&self.record_count.to_be_bytes());
        self.hasher.update(&witness.source_record_id);
        self.hasher.update(&witness.disposition);
        self.hasher.update(&witness.fact_revisions);
        self.hasher.update(&witness.semantic_payload);
        self.hasher.update(&witness.qualified_provenance);
        self.hasher.update(&witness.decoder_state_after);
        self.record_count = next_record_count;
        Ok(())
    }

    pub(crate) fn finish(
        mut self,
        final_decoder_state: [u8; DIGEST_BYTES],
    ) -> CatalogDecodeTraceSummary {
        self.hasher.update(&self.record_count.to_be_bytes());
        self.hasher.update(&final_decoder_state);
        CatalogDecodeTraceSummary {
            record_count: self.record_count,
            trace_digest: CatalogConformanceDigest::from_digest(*self.hasher.finalize().as_bytes()),
            final_decoder_state: CatalogConformanceDigest::from_digest(final_decoder_state),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct CatalogDecodeTraceSummary {
    record_count: u64,
    trace_digest: CatalogConformanceDigest,
    final_decoder_state: CatalogConformanceDigest,
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod frozen_fixture_tests {
    use serde_json::Value;

    #[test]
    fn rust_catalog_composition_matches_the_frozen_cross_adapter_fixture() {
        let actual = serde_json::to_value(super::tests::frozen_fixture()).unwrap();
        let expected: Value = serde_json::from_str(include_str!(
            "../../fixtures/contracts/rfc012b-catalog-compositions-v1.json"
        ))
        .unwrap();
        eprintln!("{}", serde_json::to_string_pretty(&actual).unwrap());
        assert_eq!(actual, expected);
        let encoded = serde_json::to_string(&actual).unwrap();
        for semantic_identity in [
            "fixture-semantic-session-alpha",
            "fixture-semantic-session-bravo",
            "fixture-semantic-session-charlie",
            "fixture-semantic-session-delta",
            "fixture-semantic-session-echo",
        ] {
            assert!(!encoded.contains(semantic_identity));
        }
    }
}
