//! RFC 012A agent-neutral support selection and public contract negotiation.
//!
//! This module is deliberately independent of concrete adapters, source
//! drivers, persistence, and delivery. A runtime host may probe native markers
//! first, but typed catalog, durable, or scoped access requires an opaque
//! [`TypedAccessAuthorization`] produced here from both a promoted support
//! release and an explicit compatible public-contract selection.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::scope::{
    ScopeProgramDeclaration, ScopeProgramManifest, ScopeProgramStatus, ScopeRelationPrimitive,
    ScopeRelationSourcePrimitive,
};

pub const SUPPORT_SELECTION_CONTRACT_VERSION: u32 = 1;
pub const CONTRACT_VERSION_SELECTION_VERSION: u32 = 1;
pub const SUPPORT_RELEASE_SCHEMA_VERSION: u32 = 1;
const ACCESS_REQUEST_CONTRACT_VERSION: u32 = 1;
const ACCESS_REPORT_RETRIEVAL_CONTRACT_VERSION: u32 = 1;
const MAX_ACCESS_REQUEST_GRANTS: usize = 256;
const MAX_ACCESS_REQUEST_IDENTITY_INPUTS: usize = 32;
const MAX_ACCESS_REQUEST_MARKERS: usize = 64;
const MAX_ACCESS_REQUEST_FACT_FAMILIES: usize = 64;
const MAX_ACCESS_REQUEST_ENCODED_BYTES: usize = 64 * 1024;

const MAX_IDENTIFIER_BYTES: usize = 128;
const MAX_VERSION_BYTES: usize = 128;
const MAX_VERSION_COMPONENTS: usize = 16;
const MAX_SUPPORT_RELEASE_BYTES: usize = 1024 * 1024;
const MAX_SUPPORT_DOCUMENT_BYTES: usize = 16 * 1024 * 1024;
const MAX_SUPPORT_PATH_BYTES: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct SupportContractError {
    message: String,
}

impl SupportContractError {
    fn invalid(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

fn validate_identifier(label: &str, value: &str) -> Result<(), SupportContractError> {
    if value.is_empty() || value.trim() != value {
        return Err(SupportContractError::invalid(format!(
            "{label} must be non-empty and canonical"
        )));
    }
    if value.len() > MAX_IDENTIFIER_BYTES {
        return Err(SupportContractError::invalid(format!(
            "{label} exceeds {MAX_IDENTIFIER_BYTES} bytes"
        )));
    }
    Ok(())
}

fn validate_version(label: &str, value: &str) -> Result<(), SupportContractError> {
    if value.is_empty() || value.trim() != value {
        return Err(SupportContractError::invalid(format!(
            "{label} must be non-empty and canonical"
        )));
    }
    if value.len() > MAX_VERSION_BYTES {
        return Err(SupportContractError::invalid(format!(
            "{label} exceeds {MAX_VERSION_BYTES} bytes"
        )));
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Sha256Digest([u8; 32]);

impl std::fmt::Debug for Sha256Digest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("Sha256Digest")
            .field(&self.to_string())
            .finish()
    }
}

impl std::fmt::Display for Sha256Digest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("sha256:")?;
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl Sha256Digest {
    pub fn parse(value: &str) -> Result<Self, SupportContractError> {
        let hexadecimal = value.strip_prefix("sha256:").ok_or_else(|| {
            SupportContractError::invalid("support digest must use the sha256: prefix")
        })?;
        if hexadecimal.len() != 64
            || !hexadecimal
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(SupportContractError::invalid(
                "support digest must contain 64 lowercase hexadecimal characters",
            ));
        }
        let mut bytes = [0_u8; 32];
        for (index, byte) in bytes.iter_mut().enumerate() {
            let offset = index * 2;
            *byte = u8::from_str_radix(&hexadecimal[offset..offset + 2], 16).map_err(|_| {
                SupportContractError::invalid("support digest contains invalid hexadecimal")
            })?;
        }
        Ok(Self(bytes))
    }

    pub fn of(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into())
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    fn is_zero(self) -> bool {
        self.0.iter().all(|byte| *byte == 0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterSupportBinding {
    support_release_id: String,
    adapter_package_version: String,
    decoder_contract_version: u32,
    ads_digest: Sha256Digest,
    source_declaration_digest: Sha256Digest,
    scope_program_digest: Sha256Digest,
}

impl AdapterSupportBinding {
    pub fn new(
        support_release_id: impl Into<String>,
        adapter_package_version: impl Into<String>,
        decoder_contract_version: u32,
        ads_digest: &str,
        source_declaration_digest: &str,
        scope_program_digest: &str,
    ) -> Result<Self, SupportContractError> {
        let support_release_id = support_release_id.into();
        validate_identifier("support release id", &support_release_id)?;
        let adapter_package_version = adapter_package_version.into();
        validate_version("adapter package version", &adapter_package_version)?;
        if decoder_contract_version == 0 {
            return Err(SupportContractError::invalid(
                "decoder contract version must be greater than zero",
            ));
        }
        Ok(Self {
            support_release_id,
            adapter_package_version,
            decoder_contract_version,
            ads_digest: Sha256Digest::parse(ads_digest)?,
            source_declaration_digest: Sha256Digest::parse(source_declaration_digest)?,
            scope_program_digest: Sha256Digest::parse(scope_program_digest)?,
        })
    }

    pub fn support_release_id(&self) -> &str {
        &self.support_release_id
    }

    pub fn adapter_package_version(&self) -> &str {
        &self.adapter_package_version
    }

    pub fn decoder_contract_version(&self) -> u32 {
        self.decoder_contract_version
    }

    pub fn ads_digest(&self) -> Sha256Digest {
        self.ads_digest
    }

    pub fn source_declaration_digest(&self) -> Sha256Digest {
        self.source_declaration_digest
    }

    pub fn scope_program_digest(&self) -> Sha256Digest {
        self.scope_program_digest
    }
}

/// Borrowed compiled declarations presented together for strict registration
/// and typed authorization. This is an input bundle, not an access capability.
#[derive(Debug, Clone, Copy)]
pub struct AdapterSupportRegistration<'a> {
    adapter_id: &'a str,
    binding: &'a AdapterSupportBinding,
    scope_programs: &'a ScopeProgramManifest,
}

impl<'a> AdapterSupportRegistration<'a> {
    pub fn new(
        adapter_id: &'a str,
        binding: &'a AdapterSupportBinding,
        scope_programs: &'a ScopeProgramManifest,
    ) -> Self {
        Self {
            adapter_id,
            binding,
            scope_programs,
        }
    }
}

fn validate_unique_nonzero_versions(
    label: &str,
    versions: &[u32],
    require_nonempty: bool,
) -> Result<(), SupportContractError> {
    if require_nonempty && versions.is_empty() {
        return Err(SupportContractError::invalid(format!(
            "{label} must not be empty"
        )));
    }
    let mut seen = BTreeSet::new();
    for version in versions {
        if *version == 0 {
            return Err(SupportContractError::invalid(format!(
                "{label} contains zero"
            )));
        }
        if !seen.insert(*version) {
            return Err(SupportContractError::invalid(format!(
                "{label} contains duplicate version {version}"
            )));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SupportReleaseStatus {
    Candidate,
    Promoted,
    Retired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SupportCapabilityTopology {
    Catalog,
    Durable,
    Scoped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SupportCapabilityLevel {
    Supported,
    Degraded,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupportCapabilityDeclaration {
    pub capability_id: String,
    pub topology: SupportCapabilityTopology,
    pub level: SupportCapabilityLevel,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactVersionRange {
    pub minimum: String,
    pub minimum_inclusive: bool,
    pub maximum: String,
    pub maximum_inclusive: bool,
}

impl ArtifactVersionRange {
    pub fn validate(&self) -> Result<(), SupportContractError> {
        let minimum = parse_dotted_version(&self.minimum)?;
        let maximum = parse_dotted_version(&self.maximum)?;
        match compare_dotted_versions(&minimum, &maximum) {
            Ordering::Greater => Err(SupportContractError::invalid(
                "artifact version range minimum exceeds maximum",
            )),
            Ordering::Equal if !(self.minimum_inclusive && self.maximum_inclusive) => Err(
                SupportContractError::invalid("artifact version range is empty"),
            ),
            Ordering::Equal | Ordering::Less => Ok(()),
        }
    }

    fn contains(&self, version: &str) -> bool {
        let Ok(candidate) = parse_dotted_version(version) else {
            return false;
        };
        let Ok(minimum) = parse_dotted_version(&self.minimum) else {
            return false;
        };
        let Ok(maximum) = parse_dotted_version(&self.maximum) else {
            return false;
        };
        let lower = compare_dotted_versions(&candidate, &minimum);
        let upper = compare_dotted_versions(&candidate, &maximum);
        let lower_matches = match lower {
            Ordering::Greater => true,
            Ordering::Equal => self.minimum_inclusive,
            Ordering::Less => false,
        };
        let upper_matches = match upper {
            Ordering::Less => true,
            Ordering::Equal => self.maximum_inclusive,
            Ordering::Greater => false,
        };
        lower_matches && upper_matches
    }
}

fn parse_dotted_version(value: &str) -> Result<Vec<u64>, SupportContractError> {
    validate_version("dotted artifact version", value)?;
    let parts = value.split('.').collect::<Vec<_>>();
    if parts.len() > MAX_VERSION_COMPONENTS {
        return Err(SupportContractError::invalid(format!(
            "dotted artifact version exceeds {MAX_VERSION_COMPONENTS} components"
        )));
    }
    parts
        .into_iter()
        .map(|part| {
            if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(SupportContractError::invalid(format!(
                    "artifact range version {value:?} is not dotted numeric"
                )));
            }
            part.parse::<u64>().map_err(|_| {
                SupportContractError::invalid(format!(
                    "artifact range version {value:?} contains an oversized component"
                ))
            })
        })
        .collect()
}

fn compare_dotted_versions(left: &[u64], right: &[u64]) -> Ordering {
    let count = left.len().max(right.len());
    for index in 0..count {
        let ordering = left
            .get(index)
            .copied()
            .unwrap_or_default()
            .cmp(&right.get(index).copied().unwrap_or_default());
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    Ordering::Equal
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactCompatibilityDeclaration {
    pub family: String,
    pub platforms: Vec<String>,
    pub exact_versions: Vec<String>,
    pub ranges: Vec<ArtifactVersionRange>,
    pub required_markers: Vec<String>,
    pub forward_catalog_only: bool,
}

impl ArtifactCompatibilityDeclaration {
    pub fn validate(&self) -> Result<(), SupportContractError> {
        validate_identifier("artifact family", &self.family)?;
        validate_identifier_list("artifact platforms", &self.platforms, true)?;
        validate_identifier_list("required native markers", &self.required_markers, false)?;
        let mut exact_versions = BTreeSet::new();
        for version in &self.exact_versions {
            validate_version("exact artifact version", version)?;
            if !exact_versions.insert(version) {
                return Err(SupportContractError::invalid(format!(
                    "duplicate exact artifact version {version:?}"
                )));
            }
        }
        for version_range in &self.ranges {
            version_range.validate()?;
        }
        Ok(())
    }
}

fn validate_identifier_list(
    label: &str,
    values: &[String],
    require_nonempty: bool,
) -> Result<(), SupportContractError> {
    if require_nonempty && values.is_empty() {
        return Err(SupportContractError::invalid(format!(
            "{label} must not be empty"
        )));
    }
    let mut seen = BTreeSet::new();
    for value in values {
        validate_identifier(label, value)?;
        if !seen.insert(value) {
            return Err(SupportContractError::invalid(format!(
                "{label} contains duplicate value {value:?}"
            )));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupportReleaseDescriptor {
    pub support_release_id: String,
    pub status: SupportReleaseStatus,
    pub capabilities: Vec<SupportCapabilityDeclaration>,
    pub artifact_compatibility: ArtifactCompatibilityDeclaration,
}

impl SupportReleaseDescriptor {
    pub fn validate(&self) -> Result<(), SupportContractError> {
        validate_identifier("support release id", &self.support_release_id)?;
        if self.capabilities.is_empty() {
            return Err(SupportContractError::invalid(
                "support release capabilities must not be empty",
            ));
        }
        let mut capability_ids = BTreeSet::new();
        for capability in &self.capabilities {
            validate_identifier("support capability id", &capability.capability_id)?;
            if !capability_ids.insert(&capability.capability_id) {
                return Err(SupportContractError::invalid(format!(
                    "duplicate support capability id {:?}",
                    capability.capability_id
                )));
            }
        }
        self.artifact_compatibility.validate()
    }

    fn declared_operation_permissions(&self) -> OperationPermissions {
        let topology_is_fully_supported = |topology| {
            let mut declared = false;
            for capability in &self.capabilities {
                if capability.topology != topology {
                    continue;
                }
                declared = true;
                if capability.level != SupportCapabilityLevel::Supported {
                    return false;
                }
            }
            declared
        };
        OperationPermissions {
            version_probe: true,
            catalog: topology_is_fully_supported(SupportCapabilityTopology::Catalog),
            durable: topology_is_fully_supported(SupportCapabilityTopology::Durable),
            scoped_observation: topology_is_fully_supported(SupportCapabilityTopology::Scoped),
            bounded_drift: true,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SupportBundleDocument<'a> {
    path: &'a str,
    bytes: &'a [u8],
}

impl<'a> SupportBundleDocument<'a> {
    pub fn new(path: &'a str, bytes: &'a [u8]) -> Self {
        Self { path, bytes }
    }
}

#[derive(Debug, Deserialize)]
struct SupportDocumentReferenceWire {
    path: String,
    sha256: String,
}

#[derive(Debug, Deserialize)]
struct SupportReferenceSetWire {
    ads: SupportDocumentReferenceWire,
    source_declaration: SupportDocumentReferenceWire,
    scope_program: SupportDocumentReferenceWire,
    evidence: SupportDocumentReferenceWire,
    conformance: SupportDocumentReferenceWire,
}

impl SupportReferenceSetWire {
    fn entries(&self) -> [(&'static str, &SupportDocumentReferenceWire); 5] {
        [
            ("ads", &self.ads),
            ("source_declaration", &self.source_declaration),
            ("scope_program", &self.scope_program),
            ("evidence", &self.evidence),
            ("conformance", &self.conformance),
        ]
    }
}

#[derive(Debug, Deserialize)]
struct SupportVersionsWire {
    adapter_package: String,
    decoder_contract: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SupportCapabilityWire {
    capability_id: String,
    topology: SupportCapabilityTopology,
    level: SupportCapabilityLevel,
    notes: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct SupportReleaseWire {
    schema_version: u32,
    support_release_id: String,
    adapter_id: String,
    status: SupportReleaseStatus,
    artifact_compatibility: ArtifactCompatibilityDeclaration,
    references: SupportReferenceSetWire,
    versions: SupportVersionsWire,
    capabilities: Vec<SupportCapabilityWire>,
}

#[derive(Debug, Deserialize)]
struct SupportDocumentIdentityWire {
    adapter_id: String,
    #[serde(default)]
    ads_id: Option<String>,
    #[serde(default)]
    support_release_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SupportSourceDeclarationWire {
    #[serde(default)]
    streams: Vec<SupportSourceStreamWire>,
}

#[derive(Debug, Deserialize)]
struct SupportSourceStreamWire {
    stream_id: String,
    root_id: String,
    #[serde(default)]
    relative_patterns: Vec<String>,
    #[serde(default)]
    decoder_id: Option<String>,
    #[serde(default)]
    authority: Option<String>,
    primitive: String,
    topologies: Vec<String>,
    implementation_state: String,
    bounds: SupportSourceBoundsWire,
    lifecycle: Vec<String>,
    safe_decoder_state_boundary: String,
}

#[derive(Debug, Deserialize)]
struct SupportSourceBoundsWire {
    #[serde(default)]
    max_entries: Option<u64>,
    #[serde(default)]
    max_depth: Option<u64>,
    #[serde(default)]
    max_object_bytes: Option<u64>,
    #[serde(default)]
    max_record_bytes: Option<u64>,
    #[serde(default)]
    max_batch_bytes: Option<u64>,
    #[serde(default)]
    max_records_per_batch: Option<u64>,
}

/// Closed common-driver contract retained from one digest-verified scoped
/// source stream. It is deliberately non-serializable and has no public
/// constructor: a scope reservation may carry it only after support-bundle
/// verification and typed access authorization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AuthorizedObservationSourceDriver {
    DirectoryMembership {
        max_entries: u64,
        max_depth: u64,
    },
    AppendDelimited {
        max_record_bytes: u64,
        max_batch_bytes: u64,
        max_records_per_batch: u64,
    },
    ReplaceDocument {
        max_object_bytes: u64,
    },
    PresenceObject {
        max_object_bytes: u64,
    },
}

/// Closed fact-authority coordinate retained from the reviewed source
/// declaration. Keeping this independent of the runtime adapter type prevents
/// support selection from acquiring decoder or source execution authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AuthorizedObservationSourceAuthority {
    Canonical,
    Supplemental,
    Diagnostic,
    IgnoredDerived,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AuthorizedObservationSourceContract {
    stream_id: String,
    root_id: String,
    relative_patterns: Vec<String>,
    decoder_id: String,
    authority: AuthorizedObservationSourceAuthority,
    driver: AuthorizedObservationSourceDriver,
    max_entries: Option<u64>,
    max_depth: Option<u64>,
}

impl AuthorizedObservationSourceContract {
    pub(crate) fn stream_id(&self) -> &str {
        &self.stream_id
    }

    pub(crate) fn root_id(&self) -> &str {
        &self.root_id
    }

    pub(crate) fn relative_patterns(&self) -> &[String] {
        &self.relative_patterns
    }

    pub(crate) fn decoder_id(&self) -> &str {
        &self.decoder_id
    }

    pub(crate) fn authority(&self) -> AuthorizedObservationSourceAuthority {
        self.authority
    }

    pub(crate) fn driver(&self) -> AuthorizedObservationSourceDriver {
        self.driver
    }

    pub(crate) fn max_entries(&self) -> Option<u64> {
        self.max_entries
    }

    pub(crate) fn max_depth(&self) -> Option<u64> {
        self.max_depth
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedSupportRelease {
    release_digest: Sha256Digest,
    descriptor: SupportReleaseDescriptor,
    adapter_id: String,
    adapter_binding: AdapterSupportBinding,
    scope_programs: ScopeProgramManifest,
    observation_source_contracts: BTreeMap<String, AuthorizedObservationSourceContract>,
}

impl VerifiedSupportRelease {
    pub fn release_digest(&self) -> Sha256Digest {
        self.release_digest
    }

    pub fn descriptor(&self) -> &SupportReleaseDescriptor {
        &self.descriptor
    }

    pub fn adapter_id(&self) -> &str {
        &self.adapter_id
    }

    pub fn adapter_binding(&self) -> &AdapterSupportBinding {
        &self.adapter_binding
    }

    pub fn scope_programs(&self) -> &ScopeProgramManifest {
        &self.scope_programs
    }

    /// Return one digest-verified source declaration entry. This describes
    /// contract content only; a Candidate release still cannot authorize I/O.
    pub(crate) fn source_contract(
        &self,
        stream_id: &str,
    ) -> Option<&AuthorizedObservationSourceContract> {
        self.observation_source_contracts.get(stream_id)
    }

    pub(crate) fn source_contracts(
        &self,
    ) -> &BTreeMap<String, AuthorizedObservationSourceContract> {
        &self.observation_source_contracts
    }

    pub fn verify_adapter_binding(
        &self,
        adapter_id: &str,
        binding: &AdapterSupportBinding,
    ) -> Result<(), SupportContractError> {
        validate_identifier("adapter id", adapter_id)?;
        if self.adapter_id != adapter_id {
            return Err(SupportContractError::invalid(format!(
                "support release {} belongs to adapter {}, not {adapter_id}",
                self.descriptor.support_release_id, self.adapter_id
            )));
        }
        if &self.adapter_binding != binding {
            return Err(SupportContractError::invalid(format!(
                "adapter {adapter_id} does not match the digest-bound package, decoder, or declaration versions in support release {}",
                self.descriptor.support_release_id
            )));
        }
        Ok(())
    }

    pub fn verify_scope_programs(
        &self,
        scope_programs: &ScopeProgramManifest,
    ) -> Result<(), SupportContractError> {
        if &self.scope_programs != scope_programs {
            return Err(SupportContractError::invalid(format!(
                "compiled scope declarations differ from support release {}",
                self.descriptor.support_release_id
            )));
        }
        Ok(())
    }

    /// Candidate declarations may be compared with runtime stream coordinates
    /// in conformance tests, but this accessor is deliberately unavailable to
    /// production source access. A verified candidate is still not an access
    /// authorization.
    #[cfg(test)]
    pub(crate) fn source_contract_for_test(
        &self,
        stream_id: &str,
    ) -> Option<&AuthorizedObservationSourceContract> {
        self.observation_source_contracts.get(stream_id)
    }
}

pub fn verify_support_release_bundle(
    release_json: &[u8],
    documents: &[SupportBundleDocument<'_>],
) -> Result<VerifiedSupportRelease, SupportContractError> {
    if release_json.is_empty() || release_json.len() > MAX_SUPPORT_RELEASE_BYTES {
        return Err(SupportContractError::invalid(format!(
            "support release must contain between 1 and {MAX_SUPPORT_RELEASE_BYTES} bytes"
        )));
    }
    let release: SupportReleaseWire = serde_json::from_slice(release_json).map_err(|error| {
        SupportContractError::invalid(format!("support release JSON is invalid: {error}"))
    })?;
    if release.schema_version != SUPPORT_RELEASE_SCHEMA_VERSION {
        return Err(SupportContractError::invalid(format!(
            "unsupported support-release schema version {}",
            release.schema_version
        )));
    }
    validate_identifier("support release adapter id", &release.adapter_id)?;
    validate_version(
        "support release adapter package version",
        &release.versions.adapter_package,
    )?;
    if release.versions.decoder_contract == 0 {
        return Err(SupportContractError::invalid(
            "support release decoder contract version must be greater than zero",
        ));
    }
    let capabilities = release
        .capabilities
        .into_iter()
        .map(|capability| {
            let SupportCapabilityWire {
                capability_id,
                topology,
                level,
                notes,
            } = capability;
            if !matches!(
                notes,
                serde_json::Value::Null | serde_json::Value::String(_)
            ) {
                return Err(SupportContractError::invalid(
                    "support capability notes must be a string or null",
                ));
            }
            Ok(SupportCapabilityDeclaration {
                capability_id,
                topology,
                level,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let descriptor = SupportReleaseDescriptor {
        support_release_id: release.support_release_id,
        status: release.status,
        capabilities,
        artifact_compatibility: release.artifact_compatibility,
    };
    descriptor.validate()?;

    let mut supplied = BTreeMap::new();
    for document in documents {
        validate_support_path(document.path)?;
        if document.bytes.is_empty() || document.bytes.len() > MAX_SUPPORT_DOCUMENT_BYTES {
            return Err(SupportContractError::invalid(format!(
                "support document {:?} must contain between 1 and {MAX_SUPPORT_DOCUMENT_BYTES} bytes",
                document.path
            )));
        }
        if supplied.insert(document.path, document.bytes).is_some() {
            return Err(SupportContractError::invalid(format!(
                "support bundle supplies document {:?} more than once",
                document.path
            )));
        }
    }

    let mut referenced = BTreeSet::new();
    let mut ads_digest = None;
    let mut ads_id = None;
    let mut ads_references = Vec::new();
    let mut source_declaration_digest = None;
    let mut source_declaration = None;
    let mut scope_program_digest = None;
    let mut scope_programs = None;
    for (kind, reference) in release.references.entries() {
        validate_support_path(&reference.path)?;
        if !referenced.insert(reference.path.as_str()) {
            return Err(SupportContractError::invalid(format!(
                "support release references {:?} more than once",
                reference.path
            )));
        }
        let expected_digest = Sha256Digest::parse(&reference.sha256)?;
        let bytes = supplied.get(reference.path.as_str()).ok_or_else(|| {
            SupportContractError::invalid(format!(
                "support bundle omits referenced {kind} document {:?}",
                reference.path
            ))
        })?;
        let actual_digest = Sha256Digest::of(bytes);
        if actual_digest != expected_digest {
            return Err(SupportContractError::invalid(format!(
                "support bundle {kind} document digest does not match {:?}",
                reference.path
            )));
        }
        let identity: SupportDocumentIdentityWire =
            serde_json::from_slice(bytes).map_err(|error| {
                SupportContractError::invalid(format!(
                    "support bundle {kind} document is invalid JSON: {error}"
                ))
            })?;
        if identity.adapter_id != release.adapter_id {
            return Err(SupportContractError::invalid(format!(
                "support bundle {kind} document belongs to adapter {}, not {}",
                identity.adapter_id, release.adapter_id
            )));
        }
        match kind {
            "ads" => {
                let document_ads_id = identity.ads_id.as_deref().ok_or_else(|| {
                    SupportContractError::invalid("support ADS document has no ads_id")
                })?;
                validate_identifier("support ADS id", document_ads_id)?;
                ads_id = Some(document_ads_id.to_string());
            }
            "source_declaration" | "scope_program" | "evidence" => {
                let referenced_ads_id = identity.ads_id.as_deref().ok_or_else(|| {
                    SupportContractError::invalid(format!("support {kind} document has no ads_id"))
                })?;
                validate_identifier("support document ADS reference", referenced_ads_id)?;
                ads_references.push((kind, referenced_ads_id.to_string()));
            }
            "conformance" => {}
            _ => unreachable!(),
        }
        if kind == "conformance"
            && identity.support_release_id.as_deref()
                != Some(descriptor.support_release_id.as_str())
        {
            return Err(SupportContractError::invalid(
                "conformance document names a different support release",
            ));
        }
        match kind {
            "ads" => ads_digest = Some(actual_digest),
            "source_declaration" => {
                let parsed = serde_json::from_slice::<SupportSourceDeclarationWire>(bytes)
                    .map_err(|error| {
                        SupportContractError::invalid(format!(
                            "support bundle source declaration is invalid: {error}"
                        ))
                    })?;
                source_declaration_digest = Some(actual_digest);
                source_declaration = Some(parsed);
            }
            "scope_program" => {
                let parsed = ScopeProgramManifest::from_json(bytes).map_err(|error| {
                    SupportContractError::invalid(format!(
                        "support bundle scope program is invalid: {error}"
                    ))
                })?;
                scope_program_digest = Some(actual_digest);
                scope_programs = Some(parsed);
            }
            "evidence" | "conformance" => {}
            _ => unreachable!(),
        }
    }
    if supplied.len() != referenced.len() {
        return Err(SupportContractError::invalid(
            "support bundle contains an unreferenced document",
        ));
    }

    let ads_id = ads_id.expect("support reference set always includes ADS");
    for (kind, referenced_ads_id) in ads_references {
        if referenced_ads_id != ads_id {
            return Err(SupportContractError::invalid(format!(
                "support {kind} document references ADS {referenced_ads_id}, not {ads_id}"
            )));
        }
    }

    let scope_programs =
        scope_programs.expect("support reference set always includes scope program");
    let source_declaration =
        source_declaration.expect("support reference set always includes source declaration");
    let durable_access_supported = descriptor.status == SupportReleaseStatus::Promoted
        && descriptor.declared_operation_permissions().durable;
    let observation_source_contracts = validate_scope_source_bindings(
        &scope_programs,
        &source_declaration,
        durable_access_supported,
    )?;
    let scope_status_matches = match descriptor.status {
        SupportReleaseStatus::Candidate => matches!(
            scope_programs.status,
            ScopeProgramStatus::Incomplete | ScopeProgramStatus::Candidate
        ),
        SupportReleaseStatus::Promoted => scope_programs.status == ScopeProgramStatus::Promoted,
        // Retirement removes selection authority and may apply to a withdrawn
        // candidate or a formerly promoted package without rewriting its
        // digest-bound declaration documents.
        SupportReleaseStatus::Retired => true,
    };
    if !scope_status_matches {
        return Err(SupportContractError::invalid(
            "scope-program status is incompatible with the support-release status",
        ));
    }

    let adapter_binding = AdapterSupportBinding {
        support_release_id: descriptor.support_release_id.clone(),
        adapter_package_version: release.versions.adapter_package,
        decoder_contract_version: release.versions.decoder_contract,
        ads_digest: ads_digest.expect("support reference set always includes ADS"),
        source_declaration_digest: source_declaration_digest
            .expect("support reference set always includes source declaration"),
        scope_program_digest: scope_program_digest
            .expect("support reference set always includes scope program"),
    };
    Ok(VerifiedSupportRelease {
        release_digest: Sha256Digest::of(release_json),
        descriptor,
        adapter_id: release.adapter_id,
        adapter_binding,
        scope_programs,
        observation_source_contracts,
    })
}

fn validate_scope_source_bindings(
    scope_programs: &ScopeProgramManifest,
    source_declaration: &SupportSourceDeclarationWire,
    require_complete_durable_contracts: bool,
) -> Result<BTreeMap<String, AuthorizedObservationSourceContract>, SupportContractError> {
    let mut streams = BTreeMap::new();
    let mut observation_source_contracts = BTreeMap::new();
    for stream in &source_declaration.streams {
        validate_identifier("support source stream id", &stream.stream_id)?;
        if let Some(decoder_id) = &stream.decoder_id {
            validate_identifier("support source decoder id", decoder_id)?;
        }
        if streams.insert(stream.stream_id.as_str(), stream).is_some() {
            return Err(SupportContractError::invalid(format!(
                "support source declaration repeats stream {:?}",
                stream.stream_id
            )));
        }
        if stream
            .topologies
            .iter()
            .any(|topology| topology == "durable")
            && stream.implementation_state == "existing"
        {
            match authorized_observation_source_contract(stream) {
                Some(contract)
                    if !require_complete_durable_contracts
                        || observation_stream_has_complete_lifecycle(stream, u64::MAX) =>
                {
                    observation_source_contracts.insert(stream.stream_id.clone(), contract);
                }
                Some(_) | None if require_complete_durable_contracts => {
                    return Err(SupportContractError::invalid(format!(
                        "supported durable source stream {:?} has no closed common-driver contract",
                        stream.stream_id
                    )));
                }
                Some(_) | None => {}
            }
        }
    }
    for relation in scope_programs.relations() {
        if let Some(binding) = relation.source_binding.as_ref() {
            let stream = streams.get(binding.stream_id.as_str()).ok_or_else(|| {
                SupportContractError::invalid(format!(
                    "scope relation {:?} binds unknown source stream {:?}",
                    relation.relation_id, binding.stream_id
                ))
            })?;
            let lifecycle = stream
                .lifecycle
                .iter()
                .map(String::as_str)
                .collect::<BTreeSet<_>>();
            if stream.root_id != relation.access_root
                || stream.primitive
                    != match binding.primitive {
                        ScopeRelationSourcePrimitive::ReplaceDocument => "ReplaceDocument",
                    }
                || stream.bounds.max_object_bytes != Some(binding.max_object_bytes)
                || !stream
                    .topologies
                    .iter()
                    .any(|topology| topology == "scoped")
                || stream.implementation_state != "existing"
                || stream.safe_decoder_state_boundary != "object_generation_revision"
                || !["replace", "delete", "recreate"]
                    .into_iter()
                    .all(|required| lifecycle.contains(required))
            {
                return Err(SupportContractError::invalid(format!(
                    "scope relation {:?} source binding does not match an existing scoped ReplaceDocument stream",
                    relation.relation_id
                )));
            }
        }
        if let Some(binding) = relation.observation_binding.as_ref() {
            let stream = streams.get(binding.stream_id.as_str()).ok_or_else(|| {
                SupportContractError::invalid(format!(
                    "scope relation {:?} binds unknown observation stream {:?}",
                    relation.relation_id, binding.stream_id
                ))
            })?;
            if stream.root_id != relation.access_root
                || stream
                    .relative_patterns
                    .iter()
                    .filter(|pattern| *pattern == &binding.source_pattern)
                    .count()
                    != 1
                || !stream
                    .topologies
                    .iter()
                    .any(|topology| topology == "scoped")
                || stream.implementation_state != "existing"
                || !observation_stream_has_complete_lifecycle(stream, relation.bounds.max_bytes)
            {
                return Err(SupportContractError::invalid(format!(
                    "scope relation {:?} observation binding does not match an existing scoped source stream",
                    relation.relation_id
                )));
            }
            let contract = authorized_observation_source_contract(stream).ok_or_else(|| {
                SupportContractError::invalid(format!(
                    "scope relation {:?} observation binding has no closed common-driver contract",
                    relation.relation_id
                ))
            })?;
            if observation_source_contracts
                .insert(binding.stream_id.clone(), contract.clone())
                .is_some_and(|existing| existing != contract)
            {
                return Err(SupportContractError::invalid(format!(
                    "observation source stream {:?} has inconsistent verified contracts",
                    binding.stream_id
                )));
            }
        }
    }
    Ok(observation_source_contracts)
}

fn authorized_observation_source_contract(
    stream: &SupportSourceStreamWire,
) -> Option<AuthorizedObservationSourceContract> {
    let decoder_id = stream.decoder_id.as_ref()?.clone();
    let authority = match stream.authority.as_deref()? {
        "canonical" => AuthorizedObservationSourceAuthority::Canonical,
        "supplemental" => AuthorizedObservationSourceAuthority::Supplemental,
        "diagnostic" => AuthorizedObservationSourceAuthority::Diagnostic,
        "ignored_derived" => AuthorizedObservationSourceAuthority::IgnoredDerived,
        _ => return None,
    };
    let driver = match stream.primitive.as_str() {
        "DirectoryMembership" => AuthorizedObservationSourceDriver::DirectoryMembership {
            max_entries: stream.bounds.max_entries?,
            max_depth: stream.bounds.max_depth?,
        },
        "AppendDelimited" => AuthorizedObservationSourceDriver::AppendDelimited {
            max_record_bytes: stream.bounds.max_record_bytes?,
            max_batch_bytes: stream.bounds.max_batch_bytes?,
            max_records_per_batch: stream.bounds.max_records_per_batch?,
        },
        "ReplaceDocument" => AuthorizedObservationSourceDriver::ReplaceDocument {
            max_object_bytes: stream.bounds.max_object_bytes?,
        },
        "PresenceObject" => AuthorizedObservationSourceDriver::PresenceObject {
            max_object_bytes: stream.bounds.max_object_bytes?,
        },
        _ => return None,
    };
    Some(AuthorizedObservationSourceContract {
        stream_id: stream.stream_id.clone(),
        root_id: stream.root_id.clone(),
        relative_patterns: stream.relative_patterns.clone(),
        decoder_id,
        authority,
        driver,
        max_entries: stream.bounds.max_entries,
        max_depth: stream.bounds.max_depth,
    })
}

fn observation_stream_has_complete_lifecycle(
    stream: &SupportSourceStreamWire,
    relation_max_bytes: u64,
) -> bool {
    let lifecycle = stream
        .lifecycle
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    match stream.primitive.as_str() {
        "DirectoryMembership" => {
            stream.safe_decoder_state_boundary == "full_snapshot"
                && stream.bounds.max_entries.is_some_and(|bound| bound > 0)
                && stream.bounds.max_depth.is_some_and(|bound| bound > 0)
                && ["membership_change", "identity_change", "delete", "recreate"]
                    .into_iter()
                    .all(|required| lifecycle.contains(required))
        }
        "ReplaceDocument" | "PresenceObject" => {
            stream.safe_decoder_state_boundary == "object_generation_revision"
                && stream
                    .bounds
                    .max_object_bytes
                    .is_some_and(|bound| bound > 0 && bound <= relation_max_bytes)
                && ["replace", "delete", "recreate"]
                    .into_iter()
                    .all(|required| lifecycle.contains(required))
        }
        "AppendDelimited" => {
            let Some(max_record_bytes) = stream.bounds.max_record_bytes else {
                return false;
            };
            let Some(max_batch_bytes) = stream.bounds.max_batch_bytes else {
                return false;
            };
            let Some(max_records_per_batch) = stream.bounds.max_records_per_batch else {
                return false;
            };
            stream.safe_decoder_state_boundary == "object_generation_cursor"
                && max_record_bytes > 0
                && max_record_bytes <= max_batch_bytes
                && max_batch_bytes <= relation_max_bytes
                && (1..=u64::from(u32::MAX)).contains(&max_records_per_batch)
                && [
                    "append",
                    "partial_write",
                    "truncate",
                    "identity_change",
                    "delete",
                    "recreate",
                ]
                .into_iter()
                .all(|required| lifecycle.contains(required))
        }
        _ => false,
    }
}

fn validate_support_path(path: &str) -> Result<(), SupportContractError> {
    if path.is_empty()
        || path.len() > MAX_SUPPORT_PATH_BYTES
        || path.starts_with('/')
        || path.contains('\\')
        || path
            .split('/')
            .any(|component| component.is_empty() || component == "." || component == "..")
    {
        return Err(SupportContractError::invalid(format!(
            "support document path {path:?} is not a canonical confined relative path"
        )));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeArtifactProbe {
    pub family: String,
    pub platform: String,
    pub version: Option<String>,
    pub markers: Vec<String>,
    pub contradictory_markers: bool,
}

impl NativeArtifactProbe {
    pub fn validate(&self) -> Result<(), SupportContractError> {
        validate_identifier("probed artifact family", &self.family)?;
        validate_identifier("probed artifact platform", &self.platform)?;
        if let Some(version) = &self.version {
            validate_version("probed artifact version", version)?;
        }
        validate_identifier_list("probed native markers", &self.markers, false)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CompatibilityClass {
    ExactSupported,
    RangeSupported,
    RecognizedUnverified,
    UnknownOrIncompatible,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompatibilityReason {
    ExactPromotedVersion,
    FixtureBackedRange,
    PromotedForwardCatalogOnly,
    NoMatchingPromotedRelease,
    RequiredNativeMarkerAbsent,
    PlatformNotDeclared,
    UnrecognizedArtifactFamily,
    ContradictoryNativeMarkers,
    AmbiguousPromotedRelease,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationPermissions {
    pub version_probe: bool,
    pub catalog: bool,
    pub durable: bool,
    pub scoped_observation: bool,
    pub bounded_drift: bool,
}

impl OperationPermissions {
    const RECOGNIZED: Self = Self {
        version_probe: true,
        catalog: false,
        durable: false,
        scoped_observation: false,
        bounded_drift: true,
    };

    const INCOMPATIBLE: Self = Self {
        version_probe: true,
        catalog: false,
        durable: false,
        scoped_observation: false,
        bounded_drift: false,
    };

    fn permits(self, operation: SupportOperation) -> bool {
        match operation {
            SupportOperation::BoundedVersionProbe => self.version_probe,
            SupportOperation::CatalogDiscovery => self.catalog,
            SupportOperation::DurableHistoryRuntime => self.durable,
            SupportOperation::ScopedTypedObservation => self.scoped_observation,
            SupportOperation::BoundedDriftEvidence => self.bounded_drift,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompatibilityDecision {
    support_selection_contract_version: u32,
    compatibility_class: CompatibilityClass,
    support_release_id: Option<String>,
    reason: CompatibilityReason,
    permissions: OperationPermissions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SupportOperation {
    BoundedVersionProbe,
    CatalogDiscovery,
    DurableHistoryRuntime,
    ScopedTypedObservation,
    BoundedDriftEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationAuthorization {
    operation: SupportOperation,
    compatibility_class: CompatibilityClass,
    support_release_id: Option<String>,
}

impl OperationAuthorization {
    pub fn operation(&self) -> SupportOperation {
        self.operation
    }

    pub fn compatibility_class(&self) -> CompatibilityClass {
        self.compatibility_class
    }

    pub fn support_release_id(&self) -> Option<&str> {
        self.support_release_id.as_deref()
    }
}

impl CompatibilityDecision {
    pub fn support_selection_contract_version(&self) -> u32 {
        self.support_selection_contract_version
    }

    pub fn compatibility_class(&self) -> CompatibilityClass {
        self.compatibility_class
    }

    pub fn support_release_id(&self) -> Option<&str> {
        self.support_release_id.as_deref()
    }

    pub fn reason(&self) -> CompatibilityReason {
        self.reason
    }

    pub fn permissions(&self) -> OperationPermissions {
        self.permissions
    }

    fn authorize(
        &self,
        operation: SupportOperation,
    ) -> Result<OperationAuthorization, SupportContractError> {
        if self.support_selection_contract_version != SUPPORT_SELECTION_CONTRACT_VERSION {
            return Err(SupportContractError::invalid(
                "compatibility decision has an unsupported contract version",
            ));
        }
        if !self.permissions.permits(operation) {
            return Err(SupportContractError::invalid(format!(
                "{operation:?} is forbidden for {:?}",
                self.compatibility_class
            )));
        }
        if matches!(
            operation,
            SupportOperation::CatalogDiscovery
                | SupportOperation::DurableHistoryRuntime
                | SupportOperation::ScopedTypedObservation
        ) && self.support_release_id.is_none()
        {
            return Err(SupportContractError::invalid(
                "typed source access requires a selected promoted support release",
            ));
        }
        Ok(OperationAuthorization {
            operation,
            compatibility_class: self.compatibility_class,
            support_release_id: self.support_release_id.clone(),
        })
    }
}

pub fn classify_runtime_support(
    probe: &NativeArtifactProbe,
    releases: &[SupportReleaseDescriptor],
) -> Result<CompatibilityDecision, SupportContractError> {
    probe.validate()?;
    let mut release_ids = BTreeSet::new();
    for release in releases {
        release.validate()?;
        if !release_ids.insert(&release.support_release_id) {
            return Err(SupportContractError::invalid(format!(
                "duplicate support release id {:?}",
                release.support_release_id
            )));
        }
    }

    let family_entries = releases
        .iter()
        .filter(|release| release.artifact_compatibility.family == probe.family)
        .collect::<Vec<_>>();
    if family_entries.is_empty() {
        return Ok(incompatible_decision(
            CompatibilityReason::UnrecognizedArtifactFamily,
        ));
    }
    if probe.contradictory_markers {
        return Ok(incompatible_decision(
            CompatibilityReason::ContradictoryNativeMarkers,
        ));
    }
    if !family_entries.iter().any(|release| {
        release
            .artifact_compatibility
            .platforms
            .iter()
            .any(|platform| platform == &probe.platform)
    }) {
        return Ok(incompatible_decision(
            CompatibilityReason::PlatformNotDeclared,
        ));
    }

    let probe_markers = probe
        .markers
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let promoted_on_platform = family_entries
        .iter()
        .copied()
        .filter(|release| {
            release.status == SupportReleaseStatus::Promoted
                && release
                    .artifact_compatibility
                    .platforms
                    .iter()
                    .any(|platform| platform == &probe.platform)
        })
        .collect::<Vec<_>>();
    let marker_compatible = promoted_on_platform
        .iter()
        .copied()
        .filter(|release| {
            release
                .artifact_compatibility
                .required_markers
                .iter()
                .all(|marker| probe_markers.contains(marker.as_str()))
        })
        .collect::<Vec<_>>();

    let mut matches = Vec::new();
    if let Some(version) = &probe.version {
        for release in &marker_compatible {
            let compatibility = &release.artifact_compatibility;
            let compatibility_class = if compatibility
                .exact_versions
                .iter()
                .any(|exact| exact == version)
            {
                Some(CompatibilityClass::ExactSupported)
            } else if compatibility
                .ranges
                .iter()
                .any(|version_range| version_range.contains(version))
            {
                Some(CompatibilityClass::RangeSupported)
            } else {
                None
            };
            if let Some(compatibility_class) = compatibility_class {
                matches.push((*release, compatibility_class));
            }
        }
    }
    if matches.len() > 1 {
        return Ok(incompatible_decision(
            CompatibilityReason::AmbiguousPromotedRelease,
        ));
    }
    if let Some((release, compatibility_class)) = matches.first() {
        return Ok(CompatibilityDecision {
            support_selection_contract_version: SUPPORT_SELECTION_CONTRACT_VERSION,
            compatibility_class: *compatibility_class,
            support_release_id: Some(release.support_release_id.clone()),
            reason: match compatibility_class {
                CompatibilityClass::ExactSupported => CompatibilityReason::ExactPromotedVersion,
                CompatibilityClass::RangeSupported => CompatibilityReason::FixtureBackedRange,
                CompatibilityClass::RecognizedUnverified
                | CompatibilityClass::UnknownOrIncompatible => unreachable!(),
            },
            permissions: release.declared_operation_permissions(),
        });
    }

    if !promoted_on_platform.is_empty() && marker_compatible.is_empty() {
        return Ok(incompatible_decision(
            CompatibilityReason::RequiredNativeMarkerAbsent,
        ));
    }

    let forward_catalog = marker_compatible
        .iter()
        .copied()
        .filter(|release| {
            release.artifact_compatibility.forward_catalog_only
                && release.declared_operation_permissions().catalog
        })
        .collect::<Vec<_>>();
    if forward_catalog.len() > 1 {
        return Ok(incompatible_decision(
            CompatibilityReason::AmbiguousPromotedRelease,
        ));
    }
    let mut permissions = OperationPermissions::RECOGNIZED;
    let (support_release_id, reason) = if let Some(release) = forward_catalog.first() {
        permissions.catalog = true;
        (
            Some(release.support_release_id.clone()),
            CompatibilityReason::PromotedForwardCatalogOnly,
        )
    } else {
        (None, CompatibilityReason::NoMatchingPromotedRelease)
    };
    Ok(CompatibilityDecision {
        support_selection_contract_version: SUPPORT_SELECTION_CONTRACT_VERSION,
        compatibility_class: CompatibilityClass::RecognizedUnverified,
        support_release_id,
        reason,
        permissions,
    })
}

fn incompatible_decision(reason: CompatibilityReason) -> CompatibilityDecision {
    CompatibilityDecision {
        support_selection_contract_version: SUPPORT_SELECTION_CONTRACT_VERSION,
        compatibility_class: CompatibilityClass::UnknownOrIncompatible,
        support_release_id: None,
        reason,
        permissions: OperationPermissions::INCOMPATIBLE,
    }
}

#[derive(Debug, Clone)]
pub struct SupportCatalog {
    releases: BTreeMap<String, VerifiedSupportRelease>,
}

impl SupportCatalog {
    pub fn new(
        releases: impl IntoIterator<Item = VerifiedSupportRelease>,
    ) -> Result<Self, SupportContractError> {
        let mut indexed = BTreeMap::new();
        for release in releases {
            let release_id = release.descriptor.support_release_id.clone();
            if indexed.insert(release_id.clone(), release).is_some() {
                return Err(SupportContractError::invalid(format!(
                    "duplicate verified support release id {release_id:?}"
                )));
            }
        }
        Ok(Self { releases: indexed })
    }

    pub fn len(&self) -> usize {
        self.releases.len()
    }

    pub fn is_empty(&self) -> bool {
        self.releases.is_empty()
    }

    pub fn classify(
        &self,
        probe: &NativeArtifactProbe,
    ) -> Result<CompatibilityDecision, SupportContractError> {
        let descriptors = self
            .releases
            .values()
            .map(|release| release.descriptor.clone())
            .collect::<Vec<_>>();
        classify_runtime_support(probe, &descriptors)
    }

    pub fn verify_adapter_binding(
        &self,
        adapter_id: &str,
        binding: &AdapterSupportBinding,
        require_promoted: bool,
    ) -> Result<(), SupportContractError> {
        let mut matching_status = false;
        for release in self.releases.values().filter(|release| {
            release.adapter_id == adapter_id
                && (!require_promoted
                    || release.descriptor.status == SupportReleaseStatus::Promoted)
        }) {
            matching_status = true;
            if release.verify_adapter_binding(adapter_id, binding).is_ok() {
                return Ok(());
            }
        }
        let qualifier = if require_promoted { "promoted " } else { "" };
        if matching_status {
            Err(SupportContractError::invalid(format!(
                "adapter {adapter_id} does not match any digest-bound {qualifier}support release"
            )))
        } else {
            Err(SupportContractError::invalid(format!(
                "adapter {adapter_id} has no digest-bound {qualifier}support release"
            )))
        }
    }

    pub fn verify_adapter_registration(
        &self,
        registration: AdapterSupportRegistration<'_>,
        require_promoted: bool,
    ) -> Result<(), SupportContractError> {
        let AdapterSupportRegistration {
            adapter_id,
            binding,
            scope_programs,
        } = registration;
        let mut matching_status = false;
        for release in self.releases.values().filter(|release| {
            release.adapter_id == adapter_id
                && (!require_promoted
                    || release.descriptor.status == SupportReleaseStatus::Promoted)
        }) {
            matching_status = true;
            if release.verify_adapter_binding(adapter_id, binding).is_ok()
                && release.verify_scope_programs(scope_programs).is_ok()
            {
                return Ok(());
            }
        }
        let qualifier = if require_promoted { "promoted " } else { "" };
        if matching_status {
            Err(SupportContractError::invalid(format!(
                "adapter {adapter_id} does not match any digest-bound {qualifier}support release and its compiled scope declarations"
            )))
        } else {
            Err(SupportContractError::invalid(format!(
                "adapter {adapter_id} has no digest-bound {qualifier}support release"
            )))
        }
    }

    pub(crate) fn authorize_typed_access(
        &self,
        registration: AdapterSupportRegistration<'_>,
        probe: &NativeArtifactProbe,
        operation: SupportOperation,
        request: &ContractVersionRequest,
        offer: &ContractVersionOffer,
    ) -> Result<(CompatibilityDecision, TypedAccessAuthorization), SupportContractError> {
        let AdapterSupportRegistration {
            adapter_id,
            binding,
            scope_programs,
        } = registration;
        if !matches!(
            operation,
            SupportOperation::CatalogDiscovery
                | SupportOperation::DurableHistoryRuntime
                | SupportOperation::ScopedTypedObservation
        ) {
            return Err(SupportContractError::invalid(
                "typed access authorization requires a catalog, durable, or scoped operation",
            ));
        }

        let decision = self.classify(probe)?;
        // Support authorization intentionally runs before public negotiation.
        // A candidate or incompatible artifact therefore cannot use a malformed
        // request to probe the host's offered semantic surface.
        let operation_authorization = decision.authorize(operation)?;
        let release_id = operation_authorization
            .support_release_id()
            .ok_or_else(|| {
                SupportContractError::invalid(
                    "typed source access requires a selected promoted support release",
                )
            })?;
        let release = self.releases.get(release_id).ok_or_else(|| {
            SupportContractError::invalid(
                "compatibility decision selected an unknown verified support release",
            )
        })?;
        release.verify_adapter_binding(adapter_id, binding)?;
        release.verify_scope_programs(scope_programs)?;
        let contracts = select_contract_versions(request, offer)?;
        if operation == SupportOperation::ScopedTypedObservation
            && contracts.observation_contract_version.is_none()
        {
            return Err(SupportContractError::invalid(
                "scoped typed observation requires a negotiated observation contract",
            ));
        }
        let probe = request_bound_probe(probe)?;
        Ok((
            decision,
            TypedAccessAuthorization {
                operation: operation_authorization,
                contracts,
                adapter_id: adapter_id.to_string(),
                support_release_digest: release.release_digest,
                source_declaration_digest: release.adapter_binding.source_declaration_digest,
                scope_program_digest: release.adapter_binding.scope_program_digest,
                scope_programs: release.scope_programs.clone(),
                observation_source_contracts: release.observation_source_contracts.clone(),
                probe,
            },
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContractVersionRequest {
    pub selection_contract_version: u32,
    pub model_major: u32,
    pub external_entity_reference_version: u32,
    pub semantic_revision_reference_version: u32,
    pub coverage_contract_versions: Vec<u32>,
    pub fact_family_versions: BTreeMap<String, Vec<u32>>,
    pub query_pack_versions: Option<Vec<u32>>,
    pub observation_contract_versions: Option<Vec<u32>>,
}

impl ContractVersionRequest {
    pub fn validate(&self) -> Result<(), SupportContractError> {
        if self.selection_contract_version != CONTRACT_VERSION_SELECTION_VERSION {
            return Err(SupportContractError::invalid(
                "unsupported contract-version selection request version",
            ));
        }
        if self.model_major == 0
            || self.external_entity_reference_version == 0
            || self.semantic_revision_reference_version == 0
        {
            return Err(SupportContractError::invalid(
                "requested base contract versions must be greater than zero",
            ));
        }
        validate_unique_nonzero_versions(
            "requested coverage contract versions",
            &self.coverage_contract_versions,
            true,
        )?;
        validate_fact_family_versions("requested", &self.fact_family_versions)?;
        if let Some(versions) = &self.query_pack_versions {
            validate_unique_nonzero_versions("requested query pack versions", versions, true)?;
        }
        if let Some(versions) = &self.observation_contract_versions {
            validate_unique_nonzero_versions(
                "requested observation contract versions",
                versions,
                true,
            )?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContractVersionOffer {
    pub selection_contract_version: u32,
    pub model_major: u32,
    pub external_entity_reference_versions: Vec<u32>,
    pub semantic_revision_reference_versions: Vec<u32>,
    pub coverage_contract_versions: Vec<u32>,
    pub fact_family_versions: BTreeMap<String, Vec<u32>>,
    pub query_pack_versions: Vec<u32>,
    pub observation_contract_versions: Vec<u32>,
}

impl ContractVersionOffer {
    pub fn validate(&self) -> Result<(), SupportContractError> {
        if self.selection_contract_version != CONTRACT_VERSION_SELECTION_VERSION {
            return Err(SupportContractError::invalid(
                "unsupported contract-version offer version",
            ));
        }
        if self.model_major == 0 {
            return Err(SupportContractError::invalid(
                "offered model major must be greater than zero",
            ));
        }
        validate_unique_nonzero_versions(
            "offered external entity reference versions",
            &self.external_entity_reference_versions,
            true,
        )?;
        validate_unique_nonzero_versions(
            "offered semantic revision reference versions",
            &self.semantic_revision_reference_versions,
            true,
        )?;
        validate_unique_nonzero_versions(
            "offered coverage contract versions",
            &self.coverage_contract_versions,
            true,
        )?;
        validate_fact_family_versions("offered", &self.fact_family_versions)?;
        validate_unique_nonzero_versions(
            "offered query pack versions",
            &self.query_pack_versions,
            false,
        )?;
        validate_unique_nonzero_versions(
            "offered observation contract versions",
            &self.observation_contract_versions,
            false,
        )
    }
}

fn validate_fact_family_versions(
    label: &str,
    families: &BTreeMap<String, Vec<u32>>,
) -> Result<(), SupportContractError> {
    for (family, versions) in families {
        validate_identifier(&format!("{label} fact family"), family)?;
        validate_unique_nonzero_versions(
            &format!("{label} fact-family versions for {family}"),
            versions,
            true,
        )?;
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContractVersionSelection {
    pub selection_contract_version: u32,
    pub model_major: u32,
    pub external_entity_reference_version: u32,
    pub semantic_revision_reference_version: u32,
    pub coverage_contract_version: u32,
    pub fact_family_versions: BTreeMap<String, u32>,
    pub query_pack_version: Option<u32>,
    pub observation_contract_version: Option<u32>,
}

impl ContractVersionSelection {
    fn view(&self) -> RequestSelectionView<'_> {
        RequestSelectionView {
            selection_contract_version: self.selection_contract_version,
            model_major: self.model_major,
            external_entity_reference_version: self.external_entity_reference_version,
            semantic_revision_reference_version: self.semantic_revision_reference_version,
            coverage_contract_version: self.coverage_contract_version,
            fact_family_versions: &self.fact_family_versions,
            query_pack_version: self.query_pack_version,
            observation_contract_version: self.observation_contract_version,
        }
    }
}

pub fn select_contract_versions(
    request: &ContractVersionRequest,
    offer: &ContractVersionOffer,
) -> Result<ContractVersionSelection, SupportContractError> {
    request.validate()?;
    offer.validate()?;
    if request.model_major != offer.model_major {
        return Err(SupportContractError::invalid(
            "incompatible base model major",
        ));
    }
    if !offer
        .external_entity_reference_versions
        .contains(&request.external_entity_reference_version)
    {
        return Err(SupportContractError::invalid(format!(
            "unsupported external entity reference version {}",
            request.external_entity_reference_version
        )));
    }
    if !offer
        .semantic_revision_reference_versions
        .contains(&request.semantic_revision_reference_version)
    {
        return Err(SupportContractError::invalid(format!(
            "unsupported semantic revision reference version {}",
            request.semantic_revision_reference_version
        )));
    }
    let coverage_contract_version = select_preferred(
        "coverage contract",
        &request.coverage_contract_versions,
        &offer.coverage_contract_versions,
    )?;
    let query_pack_version = request
        .query_pack_versions
        .as_ref()
        .map(|requested| select_preferred("query pack", requested, &offer.query_pack_versions))
        .transpose()?;
    let observation_contract_version = request
        .observation_contract_versions
        .as_ref()
        .map(|requested| {
            select_preferred(
                "observation contract",
                requested,
                &offer.observation_contract_versions,
            )
        })
        .transpose()?;
    let mut fact_family_versions = BTreeMap::new();
    for (family, requested) in &request.fact_family_versions {
        let offered = offer.fact_family_versions.get(family).ok_or_else(|| {
            SupportContractError::invalid(format!("required fact family is absent: {family}"))
        })?;
        fact_family_versions.insert(
            family.clone(),
            select_preferred(&format!("fact family {family}"), requested, offered)?,
        );
    }
    Ok(ContractVersionSelection {
        selection_contract_version: CONTRACT_VERSION_SELECTION_VERSION,
        model_major: request.model_major,
        external_entity_reference_version: request.external_entity_reference_version,
        semantic_revision_reference_version: request.semantic_revision_reference_version,
        coverage_contract_version,
        fact_family_versions,
        query_pack_version,
        observation_contract_version,
    })
}

fn select_preferred(
    label: &str,
    requested: &[u32],
    offered: &[u32],
) -> Result<u32, SupportContractError> {
    requested
        .iter()
        .find(|version| offered.contains(version))
        .copied()
        .ok_or_else(|| SupportContractError::invalid(format!("no compatible {label} version")))
}

#[derive(Clone, PartialEq, Eq)]
pub struct TypedAccessAuthorization {
    operation: OperationAuthorization,
    contracts: ContractVersionSelection,
    adapter_id: String,
    support_release_digest: Sha256Digest,
    source_declaration_digest: Sha256Digest,
    scope_program_digest: Sha256Digest,
    scope_programs: ScopeProgramManifest,
    observation_source_contracts: BTreeMap<String, AuthorizedObservationSourceContract>,
    probe: NativeArtifactProbe,
}

impl std::fmt::Debug for TypedAccessAuthorization {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TypedAccessAuthorization")
            .field("adapter_id", &self.adapter_id)
            .field("operation", &self.operation.operation)
            .field("compatibility_class", &self.operation.compatibility_class)
            .field("support_release_id", &self.operation.support_release_id)
            .field(
                "selection_contract_version",
                &self.contracts.selection_contract_version,
            )
            .field(
                "has_query_pack",
                &self.contracts.query_pack_version.is_some(),
            )
            .field(
                "has_observation_contract",
                &self.contracts.observation_contract_version.is_some(),
            )
            .field("probe_has_version", &self.probe.version.is_some())
            .field("probe_marker_count", &self.probe.markers.len())
            .finish_non_exhaustive()
    }
}

impl TypedAccessAuthorization {
    pub fn operation(&self) -> &OperationAuthorization {
        &self.operation
    }

    pub fn contracts(&self) -> &ContractVersionSelection {
        &self.contracts
    }

    /// Select the exact verified catalog authorization carried by this typed
    /// access token. The returned proof is borrowed and cannot be serialized
    /// or reconstructed from digest strings by a source runtime.
    pub fn select_catalog_access(
        &self,
    ) -> Result<AuthorizedCatalogAccess<'_>, SupportContractError> {
        if self.operation.operation != SupportOperation::CatalogDiscovery {
            return Err(SupportContractError::invalid(
                "typed authorization does not permit catalog discovery",
            ));
        }
        if self.contracts.query_pack_version.is_none() {
            return Err(SupportContractError::invalid(
                "catalog discovery requires a negotiated query-pack contract",
            ));
        }
        let support_release_id = self.operation.support_release_id().ok_or_else(|| {
            SupportContractError::invalid(
                "catalog authorization does not select a promoted support release",
            )
        })?;
        Ok(AuthorizedCatalogAccess {
            adapter_id: &self.adapter_id,
            support_release_id,
            support_release_digest: self.support_release_digest,
            source_declaration_digest: self.source_declaration_digest,
            contracts: &self.contracts,
            compatibility_class: self.operation.compatibility_class,
            source_contracts: Some(self.observation_source_contracts.clone()),
        })
    }

    /// Select the exact verified durable-source authorization carried by this
    /// token. Unlike the portable compatibility decision, this borrowed proof
    /// retains the digest-verified source contracts used to gate native I/O.
    pub(crate) fn select_durable_access(
        &self,
    ) -> Result<AuthorizedDurableAccess<'_>, SupportContractError> {
        if self.operation.operation != SupportOperation::DurableHistoryRuntime {
            return Err(SupportContractError::invalid(
                "typed authorization does not permit durable history/runtime access",
            ));
        }
        let support_release_id = self.operation.support_release_id().ok_or_else(|| {
            SupportContractError::invalid(
                "durable authorization does not select a promoted support release",
            )
        })?;
        if self.observation_source_contracts.is_empty() {
            return Err(SupportContractError::invalid(
                "durable authorization contains no verified source contracts",
            ));
        }
        Ok(AuthorizedDurableAccess {
            authorization: self,
            support_release_id,
        })
    }

    pub fn select_scope_program(
        &self,
        program_id: &str,
    ) -> Result<AuthorizedScopeProgram<'_>, SupportContractError> {
        if self.operation.operation != SupportOperation::ScopedTypedObservation
            || self.contracts.observation_contract_version.is_none()
        {
            return Err(SupportContractError::invalid(
                "typed authorization does not permit scoped observation",
            ));
        }
        if self.scope_programs.status != ScopeProgramStatus::Promoted {
            return Err(SupportContractError::invalid(
                "scoped authorization does not contain a promoted scope declaration",
            ));
        }
        let program = self.scope_programs.program(program_id).ok_or_else(|| {
            SupportContractError::invalid(format!(
                "scope program {program_id:?} is not present in the authorized support release"
            ))
        })?;
        Ok(AuthorizedScopeProgram {
            authorization: self,
            program,
        })
    }
}

/// Borrowed, non-serializable proof for one promoted durable runtime. Source
/// drivers can only obtain stream contracts through this value after native
/// probe classification and bundle verification have both succeeded.
pub(crate) struct AuthorizedDurableAccess<'a> {
    authorization: &'a TypedAccessAuthorization,
    support_release_id: &'a str,
}

impl std::fmt::Debug for AuthorizedDurableAccess<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthorizedDurableAccess")
            .field("adapter_id", &self.adapter_id())
            .field("support_release_id", &self.support_release_id)
            .finish_non_exhaustive()
    }
}

impl AuthorizedDurableAccess<'_> {
    pub(crate) fn adapter_id(&self) -> &str {
        &self.authorization.adapter_id
    }

    pub(crate) fn support_release_id(&self) -> &str {
        self.support_release_id
    }

    pub(crate) fn support_release_digest(&self) -> Sha256Digest {
        self.authorization.support_release_digest
    }

    pub(crate) fn source_declaration_digest(&self) -> Sha256Digest {
        self.authorization.source_declaration_digest
    }

    pub(crate) fn contracts(&self) -> &ContractVersionSelection {
        &self.authorization.contracts
    }

    pub(crate) fn source_contract(
        &self,
        stream_id: &str,
    ) -> Option<&AuthorizedObservationSourceContract> {
        self.authorization
            .observation_source_contracts
            .get(stream_id)
    }
}

/// Borrowed proof that Rust support selection authorized catalog discovery
/// against one exact promoted release and negotiated contract selection.
///
/// This type deliberately has no serde implementation or public constructor.
/// It is an in-process source-access capability, not a transferable digest or
/// portable decision object.
pub struct AuthorizedCatalogAccess<'a> {
    adapter_id: &'a str,
    support_release_id: &'a str,
    support_release_digest: Sha256Digest,
    source_declaration_digest: Sha256Digest,
    contracts: &'a ContractVersionSelection,
    compatibility_class: CompatibilityClass,
    source_contracts: Option<BTreeMap<String, AuthorizedObservationSourceContract>>,
}

impl std::fmt::Debug for AuthorizedCatalogAccess<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthorizedCatalogAccess")
            .field("adapter_id", &self.adapter_id)
            .field("support_release_id", &self.support_release_id)
            .field("support_release_digest", &self.support_release_digest)
            .field("source_declaration_digest", &self.source_declaration_digest)
            .field("contracts", &self.contracts)
            .finish_non_exhaustive()
    }
}

impl<'a> AuthorizedCatalogAccess<'a> {
    pub fn adapter_id(&self) -> &str {
        self.adapter_id
    }

    pub fn support_release_id(&self) -> &str {
        self.support_release_id
    }

    pub fn support_release_digest(&self) -> Sha256Digest {
        self.support_release_digest
    }

    pub fn source_declaration_digest(&self) -> Sha256Digest {
        self.source_declaration_digest
    }

    pub fn contracts(&self) -> &ContractVersionSelection {
        self.contracts
    }

    /// Private execution lineage used to prevent a forward-compatible catalog
    /// authorization from being strengthened into normal complete coverage.
    pub(crate) fn compatibility_class(&self) -> CompatibilityClass {
        self.compatibility_class
    }

    pub(crate) fn source_contracts(
        &self,
    ) -> Option<&BTreeMap<String, AuthorizedObservationSourceContract>> {
        self.source_contracts.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn fixture(
        adapter_id: &'a str,
        support_release_id: &'a str,
        support_release_digest: Sha256Digest,
        source_declaration_digest: Sha256Digest,
        contracts: &'a ContractVersionSelection,
    ) -> Self {
        Self::fixture_with_compatibility(
            adapter_id,
            support_release_id,
            support_release_digest,
            source_declaration_digest,
            contracts,
            CompatibilityClass::ExactSupported,
        )
    }

    #[cfg(test)]
    pub(crate) fn fixture_with_compatibility(
        adapter_id: &'a str,
        support_release_id: &'a str,
        support_release_digest: Sha256Digest,
        source_declaration_digest: Sha256Digest,
        contracts: &'a ContractVersionSelection,
        compatibility_class: CompatibilityClass,
    ) -> Self {
        Self {
            adapter_id,
            support_release_id,
            support_release_digest,
            source_declaration_digest,
            contracts,
            compatibility_class,
            source_contracts: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn fixture_with_source_contracts(
        adapter_id: &'a str,
        support_release_id: &'a str,
        support_release_digest: Sha256Digest,
        source_declaration_digest: Sha256Digest,
        contracts: &'a ContractVersionSelection,
        compatibility_class: CompatibilityClass,
        source_contracts: BTreeMap<String, AuthorizedObservationSourceContract>,
    ) -> Self {
        Self {
            adapter_id,
            support_release_id,
            support_release_digest,
            source_declaration_digest,
            contracts,
            compatibility_class,
            source_contracts: Some(source_contracts),
        }
    }
}

/// A program selected from the exact promoted declaration embedded in a
/// Rust-issued scoped-observation authorization. It borrows the authorization
/// and cannot be serialized or constructed by an adapter.
pub struct AuthorizedScopeProgram<'a> {
    authorization: &'a TypedAccessAuthorization,
    program: &'a ScopeProgramDeclaration,
}

impl std::fmt::Debug for AuthorizedScopeProgram<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthorizedScopeProgram")
            .field("adapter_id", &self.adapter_id())
            .field("declaration_id", &self.declaration_id())
            .field("program_id", &self.program_id())
            .finish_non_exhaustive()
    }
}

impl AuthorizedScopeProgram<'_> {
    pub fn adapter_id(&self) -> &str {
        &self.authorization.adapter_id
    }

    pub fn support_release_id(&self) -> &str {
        self.authorization
            .operation
            .support_release_id()
            .expect("scoped typed authorization always selects a support release")
    }

    pub fn support_release_digest(&self) -> Sha256Digest {
        self.authorization.support_release_digest
    }

    pub(crate) fn source_declaration_digest(&self) -> Sha256Digest {
        self.authorization.source_declaration_digest
    }

    pub fn scope_program_digest(&self) -> Sha256Digest {
        self.authorization.scope_program_digest
    }

    pub fn selection_contract_version(&self) -> u32 {
        self.authorization.contracts.selection_contract_version
    }

    pub fn observation_contract_version(&self) -> u32 {
        self.authorization
            .contracts
            .observation_contract_version
            .expect("scoped typed authorization always negotiates observation semantics")
    }

    pub fn declaration_id(&self) -> &str {
        &self.authorization.scope_programs.declaration_id
    }

    pub fn program_id(&self) -> &str {
        &self.program.program_id
    }

    pub(crate) fn scope_programs(&self) -> &ScopeProgramManifest {
        &self.authorization.scope_programs
    }

    pub(crate) fn observation_source_contract(
        &self,
        relation_id: &str,
    ) -> Option<&AuthorizedObservationSourceContract> {
        let relation = self
            .program
            .relations
            .iter()
            .find(|relation| relation.relation_id == relation_id)?;
        let binding = relation.observation_binding.as_ref()?;
        let contract = self
            .authorization
            .observation_source_contracts
            .get(&binding.stream_id)?;
        (contract.stream_id == binding.stream_id && contract.root_id == relation.access_root)
            .then_some(contract)
    }
}

impl TypedAccessAuthorization {
    /// Mint a path-free native-probe/grant request from this private
    /// authorization. The classify-time probe is bound and canonicalized after
    /// support-operation authorization succeeds and cannot be substituted at
    /// mint time. Catalog and durable operations require `program_id = None`
    /// and emit no grants. Scoped observation requires an exact program and
    /// emits that program's KnownObject relations only.
    fn native_probe_grant_request(
        &self,
        access_policy_digest: Sha256Digest,
        program_id: Option<&str>,
    ) -> Result<TrustedNativeProbeGrantRequest, SupportContractError> {
        TrustedNativeProbeGrantRequest::from_authorization(self, access_policy_digest, program_id)
    }

    /// Mint a scoped access-report retrieval request bound to one caller-held
    /// report digest. The portable projection cannot reserve access or become
    /// this authorization.
    fn access_report_retrieval(
        &self,
        program_id: &str,
        access_policy_digest: Sha256Digest,
        expected_report_digest: Sha256Digest,
    ) -> Result<ScopeAccessReportRetrieval, SupportContractError> {
        ScopeAccessReportRetrieval::from_authorization(
            self,
            program_id,
            access_policy_digest,
            expected_report_digest,
        )
    }
}

fn require_nonzero_digest(
    digest: Sha256Digest,
    label: &str,
) -> Result<Sha256Digest, SupportContractError> {
    if digest.is_zero() {
        return Err(SupportContractError::invalid(format!(
            "{label} must be a nonzero 32-byte digest"
        )));
    }
    Ok(digest)
}

fn hash_u8(hasher: &mut Sha256, value: u8) {
    hasher.update([value]);
}

fn hash_u32(hasher: &mut Sha256, value: u32) {
    hasher.update(value.to_be_bytes());
}

fn hash_u64(hasher: &mut Sha256, value: u64) {
    hasher.update(value.to_be_bytes());
}

fn hash_component(hasher: &mut Sha256, value: &[u8]) {
    hash_u64(hasher, value.len() as u64);
    hasher.update(value);
}

fn hash_optional_u32(hasher: &mut Sha256, value: Option<u32>) {
    match value {
        Some(version) => {
            hash_u8(hasher, 1);
            hash_u32(hasher, version);
        }
        None => hash_u8(hasher, 0),
    }
}

fn topology_code(topology: SupportCapabilityTopology) -> u8 {
    match topology {
        SupportCapabilityTopology::Catalog => 1,
        SupportCapabilityTopology::Durable => 2,
        SupportCapabilityTopology::Scoped => 3,
    }
}

fn operation_code(operation: TypedAccessRequestOperation) -> u8 {
    match operation {
        TypedAccessRequestOperation::CatalogDiscovery => 1,
        TypedAccessRequestOperation::DurableHistoryRuntime => 2,
        TypedAccessRequestOperation::ScopedTypedObservation => 3,
    }
}

fn topology_for_operation(operation: TypedAccessRequestOperation) -> SupportCapabilityTopology {
    match operation {
        TypedAccessRequestOperation::CatalogDiscovery => SupportCapabilityTopology::Catalog,
        TypedAccessRequestOperation::DurableHistoryRuntime => SupportCapabilityTopology::Durable,
        TypedAccessRequestOperation::ScopedTypedObservation => SupportCapabilityTopology::Scoped,
    }
}

fn typed_request_operation(
    operation: SupportOperation,
) -> Result<TypedAccessRequestOperation, SupportContractError> {
    match operation {
        SupportOperation::CatalogDiscovery => Ok(TypedAccessRequestOperation::CatalogDiscovery),
        SupportOperation::DurableHistoryRuntime => {
            Ok(TypedAccessRequestOperation::DurableHistoryRuntime)
        }
        SupportOperation::ScopedTypedObservation => {
            Ok(TypedAccessRequestOperation::ScopedTypedObservation)
        }
        SupportOperation::BoundedVersionProbe | SupportOperation::BoundedDriftEvidence => {
            Err(SupportContractError::invalid(
                "native-probe/grant request requires a catalog, durable, or scoped operation",
            ))
        }
    }
}

#[derive(Clone, Copy)]
struct RequestSelectionView<'a> {
    selection_contract_version: u32,
    model_major: u32,
    external_entity_reference_version: u32,
    semantic_revision_reference_version: u32,
    coverage_contract_version: u32,
    fact_family_versions: &'a BTreeMap<String, u32>,
    query_pack_version: Option<u32>,
    observation_contract_version: Option<u32>,
}

fn hash_contract_selection(hasher: &mut Sha256, selection: RequestSelectionView<'_>) {
    hash_u32(hasher, selection.selection_contract_version);
    hash_u32(hasher, selection.model_major);
    hash_u32(hasher, selection.external_entity_reference_version);
    hash_u32(hasher, selection.semantic_revision_reference_version);
    hash_u32(hasher, selection.coverage_contract_version);
    hash_u64(hasher, selection.fact_family_versions.len() as u64);
    for (family, version) in selection.fact_family_versions {
        hash_component(hasher, family.as_bytes());
        hash_u32(hasher, *version);
    }
    hash_optional_u32(hasher, selection.query_pack_version);
    hash_optional_u32(hasher, selection.observation_contract_version);
}

fn hash_probe_fields(
    hasher: &mut Sha256,
    family: &str,
    platform: &str,
    version: Option<&str>,
    markers: &[String],
    contradictory_markers: bool,
) {
    hash_component(hasher, family.as_bytes());
    hash_component(hasher, platform.as_bytes());
    match version {
        Some(version) => {
            hash_u8(hasher, 1);
            hash_component(hasher, version.as_bytes());
        }
        None => hash_u8(hasher, 0),
    }
    let mut markers: Vec<&str> = markers.iter().map(String::as_str).collect();
    markers.sort_unstable();
    markers.dedup();
    hash_u64(hasher, markers.len() as u64);
    for marker in markers {
        hash_component(hasher, marker.as_bytes());
    }
    hash_u8(hasher, u8::from(contradictory_markers));
}

fn hash_probe(hasher: &mut Sha256, probe: &NativeArtifactProbe) {
    hash_probe_fields(
        hasher,
        &probe.family,
        &probe.platform,
        probe.version.as_deref(),
        &probe.markers,
        probe.contradictory_markers,
    );
}

struct AccessRequestDigestCoordinates<'a> {
    contract_version: u32,
    adapter_id: &'a str,
    support_release_id: &'a str,
    support_release_digest: Sha256Digest,
    source_declaration_digest: Sha256Digest,
    scope_program_digest: Sha256Digest,
    declaration_id: &'a str,
    program_id: &'a str,
    topology: SupportCapabilityTopology,
    operation: TypedAccessRequestOperation,
    selection: RequestSelectionView<'a>,
    access_policy_digest: Sha256Digest,
}

impl AccessRequestDigestCoordinates<'_> {
    fn write(&self, hasher: &mut Sha256) {
        hash_u32(hasher, self.contract_version);
        hash_component(hasher, self.adapter_id.as_bytes());
        hash_component(hasher, self.support_release_id.as_bytes());
        hash_component(hasher, self.support_release_digest.as_bytes());
        hash_component(hasher, self.source_declaration_digest.as_bytes());
        hash_component(hasher, self.scope_program_digest.as_bytes());
        hash_component(hasher, self.declaration_id.as_bytes());
        hash_component(hasher, self.program_id.as_bytes());
        hash_u8(hasher, topology_code(self.topology));
        hash_u8(hasher, operation_code(self.operation));
        hash_contract_selection(hasher, self.selection);
        hash_component(hasher, self.access_policy_digest.as_bytes());
    }
}

fn validate_request_machine_id(label: &str, value: &str) -> Result<(), SupportContractError> {
    let valid = !value.is_empty()
        && value.len() <= MAX_IDENTIFIER_BYTES
        && value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        });
    if !valid {
        return Err(SupportContractError::invalid(format!(
            "{label} must be a machine identifier"
        )));
    }
    Ok(())
}

fn canonical_probe_markers(
    markers: &[String],
    encoded_bytes: &mut usize,
) -> Result<Vec<String>, SupportContractError> {
    if markers.len() > MAX_ACCESS_REQUEST_MARKERS {
        return Err(SupportContractError::invalid(
            "native probe exceeds the marker collection limit",
        ));
    }
    let mut canonical = Vec::with_capacity(markers.len());
    for marker in markers {
        validate_request_machine_id("probed native marker", marker)?;
        *encoded_bytes = encoded_bytes.saturating_add(marker.len());
        if *encoded_bytes > MAX_ACCESS_REQUEST_ENCODED_BYTES {
            return Err(SupportContractError::invalid(
                "native probe exceeds the encoded-byte limit",
            ));
        }
        canonical.push(marker.clone());
    }
    canonical.sort();
    canonical.dedup();
    Ok(canonical)
}

fn request_bound_probe(
    probe: &NativeArtifactProbe,
) -> Result<NativeArtifactProbe, SupportContractError> {
    validate_request_machine_id("probed artifact family", &probe.family)?;
    validate_request_machine_id("probed artifact platform", &probe.platform)?;
    if let Some(version) = &probe.version {
        validate_request_machine_id("probed artifact version", version)?;
    }
    let mut encoded_bytes = probe.family.len().saturating_add(probe.platform.len());
    if let Some(version) = &probe.version {
        encoded_bytes = encoded_bytes.saturating_add(version.len());
    }
    if encoded_bytes > MAX_ACCESS_REQUEST_ENCODED_BYTES {
        return Err(SupportContractError::invalid(
            "native probe exceeds the encoded-byte limit",
        ));
    }
    Ok(NativeArtifactProbe {
        family: probe.family.clone(),
        platform: probe.platform.clone(),
        version: probe.version.clone(),
        markers: canonical_probe_markers(&probe.markers, &mut encoded_bytes)?,
        contradictory_markers: probe.contradictory_markers,
    })
}

fn validate_request_selection(
    selection: RequestSelectionView<'_>,
    operation: TypedAccessRequestOperation,
) -> Result<(), SupportContractError> {
    if selection.selection_contract_version != CONTRACT_VERSION_SELECTION_VERSION {
        return Err(SupportContractError::invalid(
            "unsupported contract-version selection version",
        ));
    }
    if selection.model_major == 0
        || selection.external_entity_reference_version == 0
        || selection.semantic_revision_reference_version == 0
        || selection.coverage_contract_version == 0
    {
        return Err(SupportContractError::invalid(
            "selected base contract versions must be greater than zero",
        ));
    }
    if selection.fact_family_versions.len() > MAX_ACCESS_REQUEST_FACT_FAMILIES {
        return Err(SupportContractError::invalid(
            "selected fact families exceed the collection limit",
        ));
    }
    let mut encoded_bytes = 0usize;
    for (family, version) in selection.fact_family_versions {
        validate_request_machine_id("selected fact family", family)?;
        encoded_bytes = encoded_bytes.saturating_add(family.len());
        if encoded_bytes > MAX_ACCESS_REQUEST_ENCODED_BYTES {
            return Err(SupportContractError::invalid(
                "selected fact families exceed the encoded-byte limit",
            ));
        }
        if *version == 0 {
            return Err(SupportContractError::invalid(
                "selected fact-family versions must be greater than zero",
            ));
        }
    }
    if matches!(selection.query_pack_version, Some(0))
        || matches!(selection.observation_contract_version, Some(0))
    {
        return Err(SupportContractError::invalid(
            "selected optional contract versions must be greater than zero when present",
        ));
    }
    if operation == TypedAccessRequestOperation::CatalogDiscovery
        && selection.query_pack_version.is_none()
    {
        return Err(SupportContractError::invalid(
            "catalog discovery requires a negotiated query-pack contract",
        ));
    }
    if operation == TypedAccessRequestOperation::ScopedTypedObservation
        && selection.observation_contract_version.is_none()
    {
        return Err(SupportContractError::invalid(
            "scoped probe/grant request requires a negotiated observation contract",
        ));
    }
    Ok(())
}

fn validate_request_identity_coordinates(
    adapter_id: &str,
    support_release_id: &str,
    declaration_id: &str,
    program_id: &str,
    operation: TypedAccessRequestOperation,
) -> Result<(), SupportContractError> {
    validate_request_machine_id("request adapter id", adapter_id)?;
    validate_request_machine_id("request support release id", support_release_id)?;
    validate_request_machine_id("request declaration id", declaration_id)?;
    match operation {
        TypedAccessRequestOperation::ScopedTypedObservation => {
            validate_request_machine_id("request program id", program_id)?;
        }
        TypedAccessRequestOperation::CatalogDiscovery
        | TypedAccessRequestOperation::DurableHistoryRuntime => {
            if !program_id.is_empty() {
                return Err(SupportContractError::invalid(
                    "catalog and durable probe/grant requests cannot carry grants or a program id",
                ));
            }
        }
    }
    Ok(())
}

fn request_contract_error(message: &'static str) -> SupportContractError {
    SupportContractError::invalid(message)
}

fn json_object<'a>(
    value: &'a serde_json::Value,
    message: &'static str,
) -> Result<&'a serde_json::Map<String, serde_json::Value>, SupportContractError> {
    value
        .as_object()
        .ok_or_else(|| request_contract_error(message))
}

fn json_array<'a>(
    value: &'a serde_json::Value,
    message: &'static str,
) -> Result<&'a Vec<serde_json::Value>, SupportContractError> {
    value
        .as_array()
        .ok_or_else(|| request_contract_error(message))
}

fn reject_unknown_json_fields(
    object: &serde_json::Map<String, serde_json::Value>,
    allowed: &[&str],
    message: &'static str,
) -> Result<(), SupportContractError> {
    if object
        .keys()
        .any(|key| allowed.iter().copied().all(|allowed| allowed != key))
    {
        return Err(request_contract_error(message));
    }
    Ok(())
}

fn require_json_fields(
    object: &serde_json::Map<String, serde_json::Value>,
    required: &[&str],
    message: &'static str,
) -> Result<(), SupportContractError> {
    if required.iter().any(|field| !object.contains_key(*field)) {
        return Err(request_contract_error(message));
    }
    Ok(())
}

fn preflight_json_identifier(
    value: &serde_json::Value,
    message: &'static str,
) -> Result<(), SupportContractError> {
    let Some(text) = value.as_str() else {
        return Err(request_contract_error(message));
    };
    if text.len() > MAX_IDENTIFIER_BYTES {
        return Err(request_contract_error(message));
    }
    Ok(())
}

fn preflight_nullable_json_identifier(
    value: &serde_json::Value,
    message: &'static str,
) -> Result<(), SupportContractError> {
    if value.is_null() {
        return Ok(());
    }
    preflight_json_identifier(value, message)
}

const PROBE_GRANT_REQUEST_FIELDS: &[&str] = &[
    "access_request_contract_version",
    "adapter_id",
    "support_release_id",
    "support_release_digest",
    "source_declaration_digest",
    "scope_program_digest",
    "declaration_id",
    "program_id",
    "capability_topology",
    "operation",
    "selection",
    "access_policy_digest",
    "probe",
    "grants",
    "digest",
];
const RETRIEVAL_REQUEST_FIELDS: &[&str] = &[
    "access_report_retrieval_contract_version",
    "adapter_id",
    "support_release_id",
    "support_release_digest",
    "source_declaration_digest",
    "scope_program_digest",
    "declaration_id",
    "program_id",
    "capability_topology",
    "operation",
    "selection",
    "access_policy_digest",
    "expected_report_digest",
    "digest",
];
const PROBE_FIELDS: &[&str] = &[
    "family",
    "platform",
    "version",
    "markers",
    "contradictory_markers",
];
const SELECTION_FIELDS: &[&str] = &[
    "selection_contract_version",
    "model_major",
    "external_entity_reference_version",
    "semantic_revision_reference_version",
    "coverage_contract_version",
    "fact_family_versions",
    "query_pack_version",
    "observation_contract_version",
];
const GRANT_FIELDS: &[&str] = &[
    "relation_id",
    "scope_root",
    "access_root",
    "identity_input_names",
];

fn preflight_request_selection(value: &serde_json::Value) -> Result<(), SupportContractError> {
    let selection = json_object(value, "contract version selection must be an object")?;
    reject_unknown_json_fields(
        selection,
        SELECTION_FIELDS,
        "contract version selection contains an unknown field",
    )?;
    require_json_fields(
        selection,
        SELECTION_FIELDS,
        "contract version selection is missing a required field",
    )?;
    let families = json_object(
        &selection["fact_family_versions"],
        "selected fact families must be an object",
    )?;
    if families.len() > MAX_ACCESS_REQUEST_FACT_FAMILIES {
        return Err(request_contract_error(
            "selected fact families exceed the collection limit",
        ));
    }
    for family in families.keys() {
        if family.len() > MAX_IDENTIFIER_BYTES {
            return Err(request_contract_error(
                "selected fact family must be a machine identifier",
            ));
        }
    }
    Ok(())
}

fn preflight_probe_grant_request(value: &serde_json::Value) -> Result<(), SupportContractError> {
    let object = json_object(value, "native-probe/grant request is not valid JSON")?;
    reject_unknown_json_fields(
        object,
        PROBE_GRANT_REQUEST_FIELDS,
        "native-probe/grant request contains an unknown field",
    )?;
    require_json_fields(
        object,
        PROBE_GRANT_REQUEST_FIELDS,
        "native-probe/grant request is missing a required field",
    )?;
    preflight_json_identifier(
        &object["adapter_id"],
        "request adapter id must be a machine identifier",
    )?;
    preflight_json_identifier(
        &object["support_release_id"],
        "request support release id must be a machine identifier",
    )?;
    preflight_json_identifier(
        &object["declaration_id"],
        "request declaration id must be a machine identifier",
    )?;
    preflight_json_identifier(
        &object["program_id"],
        "request program id must be a machine identifier",
    )?;
    let probe = json_object(&object["probe"], "native artifact probe must be an object")?;
    reject_unknown_json_fields(
        probe,
        PROBE_FIELDS,
        "native artifact probe contains an unknown field",
    )?;
    require_json_fields(
        probe,
        PROBE_FIELDS,
        "native artifact probe is missing a required field",
    )?;
    preflight_json_identifier(
        &probe["family"],
        "probed artifact family must be a machine identifier",
    )?;
    preflight_json_identifier(
        &probe["platform"],
        "probed artifact platform must be a machine identifier",
    )?;
    preflight_nullable_json_identifier(
        &probe["version"],
        "probed artifact version must be a machine identifier",
    )?;
    let markers = json_array(&probe["markers"], "probed native markers must be an array")?;
    if markers.len() > MAX_ACCESS_REQUEST_MARKERS {
        return Err(request_contract_error(
            "native probe exceeds the marker collection limit",
        ));
    }
    for marker in markers {
        preflight_json_identifier(marker, "probed native marker must be a machine identifier")?;
    }
    preflight_request_selection(&object["selection"])?;
    let grants = json_array(
        &object["grants"],
        "probe/grant request grants must be an array",
    )?;
    if grants.len() > MAX_ACCESS_REQUEST_GRANTS {
        return Err(request_contract_error(
            "probe/grant request exceeds the grant collection limit",
        ));
    }
    let mut encoded_bytes = 0usize;
    for grant in grants {
        let grant = json_object(grant, "declared known-object grant must be an object")?;
        reject_unknown_json_fields(
            grant,
            GRANT_FIELDS,
            "declared known-object grant contains an unknown field",
        )?;
        require_json_fields(
            grant,
            GRANT_FIELDS,
            "declared known-object grant is missing a required field",
        )?;
        preflight_json_identifier(
            &grant["relation_id"],
            "grant relation id must be a machine identifier",
        )?;
        preflight_json_identifier(
            &grant["access_root"],
            "grant access root must be a machine identifier",
        )?;
        let names = json_array(
            &grant["identity_input_names"],
            "grant identity input list must not be empty",
        )?;
        if names.len() > MAX_ACCESS_REQUEST_IDENTITY_INPUTS {
            return Err(request_contract_error(
                "grant identity inputs exceed the collection limit",
            ));
        }
        encoded_bytes = encoded_bytes
            .saturating_add(grant["relation_id"].as_str().map(str::len).unwrap_or(0))
            .saturating_add(grant["access_root"].as_str().map(str::len).unwrap_or(0));
        if encoded_bytes > MAX_ACCESS_REQUEST_ENCODED_BYTES {
            return Err(request_contract_error(
                "access request exceeds the encoded-byte limit",
            ));
        }
        for name in names {
            preflight_json_identifier(name, "grant identity input must be a machine identifier")?;
            encoded_bytes = encoded_bytes.saturating_add(name.as_str().map(str::len).unwrap_or(0));
            if encoded_bytes > MAX_ACCESS_REQUEST_ENCODED_BYTES {
                return Err(request_contract_error(
                    "access request exceeds the encoded-byte limit",
                ));
            }
        }
    }
    Ok(())
}

fn preflight_access_report_retrieval(
    value: &serde_json::Value,
) -> Result<(), SupportContractError> {
    let object = json_object(value, "access-report retrieval request is not valid JSON")?;
    reject_unknown_json_fields(
        object,
        RETRIEVAL_REQUEST_FIELDS,
        "access-report retrieval request contains an unknown field",
    )?;
    require_json_fields(
        object,
        RETRIEVAL_REQUEST_FIELDS,
        "access-report retrieval request is missing a required field",
    )?;
    preflight_json_identifier(
        &object["adapter_id"],
        "retrieval adapter id must be a machine identifier",
    )?;
    preflight_json_identifier(
        &object["support_release_id"],
        "retrieval support release id must be a machine identifier",
    )?;
    preflight_json_identifier(
        &object["declaration_id"],
        "retrieval declaration id must be a machine identifier",
    )?;
    preflight_json_identifier(
        &object["program_id"],
        "retrieval program id must be a machine identifier",
    )?;
    preflight_request_selection(&object["selection"])?;
    Ok(())
}

fn declared_known_object_grants(
    program: &ScopeProgramDeclaration,
) -> Result<Vec<DeclaredKnownObjectGrant>, SupportContractError> {
    let root_id = program.root_relation_id.as_deref().ok_or_else(|| {
        SupportContractError::invalid(
            "scoped probe/grant request requires a declared known-object root",
        )
    })?;
    let mut grants = Vec::new();
    let mut encoded_bytes = 0usize;
    for relation in &program.relations {
        if relation.primitive != ScopeRelationPrimitive::KnownObject {
            continue;
        }
        if grants.len() >= MAX_ACCESS_REQUEST_GRANTS {
            return Err(SupportContractError::invalid(
                "probe/grant request exceeds the grant collection limit",
            ));
        }
        validate_request_machine_id("grant relation id", &relation.relation_id)?;
        validate_request_machine_id("grant access root", &relation.access_root)?;
        if relation.identity_inputs.is_empty() {
            return Err(SupportContractError::invalid(
                "grant identity input list must not be empty",
            ));
        }
        if relation.identity_inputs.len() > MAX_ACCESS_REQUEST_IDENTITY_INPUTS {
            return Err(SupportContractError::invalid(
                "grant identity inputs exceed the collection limit",
            ));
        }
        encoded_bytes = encoded_bytes
            .saturating_add(relation.relation_id.len())
            .saturating_add(relation.access_root.len());
        if encoded_bytes > MAX_ACCESS_REQUEST_ENCODED_BYTES {
            return Err(SupportContractError::invalid(
                "probe/grant request exceeds the encoded-byte limit",
            ));
        }
        let mut seen = BTreeSet::new();
        for name in &relation.identity_inputs {
            validate_request_machine_id("grant identity input", name)?;
            if !seen.insert(name.as_str()) {
                return Err(SupportContractError::invalid(
                    "grant identity input contains duplicate value",
                ));
            }
            encoded_bytes = encoded_bytes.saturating_add(name.len());
            if encoded_bytes > MAX_ACCESS_REQUEST_ENCODED_BYTES {
                return Err(SupportContractError::invalid(
                    "probe/grant request exceeds the encoded-byte limit",
                ));
            }
        }
        grants.push(DeclaredKnownObjectGrant {
            relation_id: relation.relation_id.clone(),
            scope_root: relation.relation_id == root_id,
            access_root: relation.access_root.clone(),
            identity_input_names: relation.identity_inputs.clone(),
        });
    }
    grants.sort_by(|left, right| left.relation_id.cmp(&right.relation_id));
    if grants.is_empty() {
        return Err(SupportContractError::invalid(
            "scoped probe/grant request requires at least one known-object grant",
        ));
    }
    if grants.iter().filter(|grant| grant.scope_root).count() != 1 {
        return Err(SupportContractError::invalid(
            "scoped probe/grant request requires exactly one scope-root known-object grant",
        ));
    }
    Ok(grants)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TypedAccessRequestOperation {
    CatalogDiscovery,
    DurableHistoryRuntime,
    ScopedTypedObservation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeArtifactProbeWire {
    family: String,
    platform: String,
    version: Option<String>,
    markers: Vec<String>,
    contradictory_markers: bool,
}

impl NativeArtifactProbeWire {
    fn from_probe(probe: &NativeArtifactProbe) -> Self {
        let mut markers = probe.markers.clone();
        markers.sort();
        markers.dedup();
        Self {
            family: probe.family.clone(),
            platform: probe.platform.clone(),
            version: probe.version.clone(),
            markers,
            contradictory_markers: probe.contradictory_markers,
        }
    }

    fn validate(&self) -> Result<(), SupportContractError> {
        validate_request_machine_id("probed artifact family", &self.family)?;
        validate_request_machine_id("probed artifact platform", &self.platform)?;
        if let Some(version) = &self.version {
            validate_request_machine_id("probed artifact version", version)?;
        }
        let mut encoded_bytes = self.family.len().saturating_add(self.platform.len());
        if let Some(version) = &self.version {
            encoded_bytes = encoded_bytes.saturating_add(version.len());
        }
        if encoded_bytes > MAX_ACCESS_REQUEST_ENCODED_BYTES {
            return Err(SupportContractError::invalid(
                "native probe exceeds the encoded-byte limit",
            ));
        }
        if self.markers.len() > MAX_ACCESS_REQUEST_MARKERS {
            return Err(SupportContractError::invalid(
                "native probe exceeds the marker collection limit",
            ));
        }
        for marker in &self.markers {
            validate_request_machine_id("probed native marker", marker)?;
            encoded_bytes = encoded_bytes.saturating_add(marker.len());
            if encoded_bytes > MAX_ACCESS_REQUEST_ENCODED_BYTES {
                return Err(SupportContractError::invalid(
                    "native probe exceeds the encoded-byte limit",
                ));
            }
        }
        Ok(())
    }

    fn hash(&self, hasher: &mut Sha256) {
        hash_probe_fields(
            hasher,
            &self.family,
            &self.platform,
            self.version.as_deref(),
            &self.markers,
            self.contradictory_markers,
        );
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContractVersionSelectionWire {
    selection_contract_version: u32,
    model_major: u32,
    external_entity_reference_version: u32,
    semantic_revision_reference_version: u32,
    coverage_contract_version: u32,
    fact_family_versions: BTreeMap<String, u32>,
    query_pack_version: Option<u32>,
    observation_contract_version: Option<u32>,
}

impl ContractVersionSelectionWire {
    fn from_selection(selection: &ContractVersionSelection) -> Self {
        Self {
            selection_contract_version: selection.selection_contract_version,
            model_major: selection.model_major,
            external_entity_reference_version: selection.external_entity_reference_version,
            semantic_revision_reference_version: selection.semantic_revision_reference_version,
            coverage_contract_version: selection.coverage_contract_version,
            fact_family_versions: selection.fact_family_versions.clone(),
            query_pack_version: selection.query_pack_version,
            observation_contract_version: selection.observation_contract_version,
        }
    }

    fn view(&self) -> RequestSelectionView<'_> {
        RequestSelectionView {
            selection_contract_version: self.selection_contract_version,
            model_major: self.model_major,
            external_entity_reference_version: self.external_entity_reference_version,
            semantic_revision_reference_version: self.semantic_revision_reference_version,
            coverage_contract_version: self.coverage_contract_version,
            fact_family_versions: &self.fact_family_versions,
            query_pack_version: self.query_pack_version,
            observation_contract_version: self.observation_contract_version,
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
struct DeclaredKnownObjectGrant {
    relation_id: String,
    scope_root: bool,
    access_root: String,
    identity_input_names: Vec<String>,
}

impl DeclaredKnownObjectGrant {
    fn to_wire(&self) -> DeclaredKnownObjectGrantWire {
        DeclaredKnownObjectGrantWire {
            relation_id: self.relation_id.clone(),
            scope_root: self.scope_root,
            access_root: self.access_root.clone(),
            identity_input_names: self.identity_input_names.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeclaredKnownObjectGrantWire {
    relation_id: String,
    scope_root: bool,
    access_root: String,
    identity_input_names: Vec<String>,
}

impl DeclaredKnownObjectGrantWire {
    fn validate(&self, encoded_bytes: &mut usize) -> Result<(), SupportContractError> {
        validate_request_machine_id("grant relation id", &self.relation_id)?;
        validate_request_machine_id("grant access root", &self.access_root)?;
        if self.identity_input_names.is_empty() {
            return Err(SupportContractError::invalid(
                "grant identity input list must not be empty",
            ));
        }
        if self.identity_input_names.len() > MAX_ACCESS_REQUEST_IDENTITY_INPUTS {
            return Err(SupportContractError::invalid(
                "grant identity inputs exceed the collection limit",
            ));
        }
        *encoded_bytes = encoded_bytes
            .saturating_add(self.relation_id.len())
            .saturating_add(self.access_root.len());
        if *encoded_bytes > MAX_ACCESS_REQUEST_ENCODED_BYTES {
            return Err(SupportContractError::invalid(
                "probe/grant request exceeds the encoded-byte limit",
            ));
        }
        let mut seen = BTreeSet::new();
        for name in &self.identity_input_names {
            validate_request_machine_id("grant identity input", name)?;
            if !seen.insert(name.as_str()) {
                return Err(SupportContractError::invalid(
                    "grant identity input contains duplicate value",
                ));
            }
            *encoded_bytes = encoded_bytes.saturating_add(name.len());
            if *encoded_bytes > MAX_ACCESS_REQUEST_ENCODED_BYTES {
                return Err(SupportContractError::invalid(
                    "probe/grant request exceeds the encoded-byte limit",
                ));
            }
        }
        Ok(())
    }
}

fn hash_grants(hasher: &mut Sha256, grants: &[DeclaredKnownObjectGrantWire]) {
    hash_u64(hasher, grants.len() as u64);
    for grant in grants {
        hash_component(hasher, grant.relation_id.as_bytes());
        hash_u8(hasher, u8::from(grant.scope_root));
        hash_component(hasher, grant.access_root.as_bytes());
        hash_u64(hasher, grant.identity_input_names.len() as u64);
        for name in &grant.identity_input_names {
            hash_component(hasher, name.as_bytes());
        }
    }
}

/// Private native-probe/grant request. It is minted only from a verified
/// typed authorization and never from a portable value.
struct TrustedNativeProbeGrantRequest {
    contract_version: u32,
    adapter_id: String,
    support_release_id: String,
    support_release_digest: Sha256Digest,
    source_declaration_digest: Sha256Digest,
    scope_program_digest: Sha256Digest,
    declaration_id: String,
    program_id: String,
    topology: SupportCapabilityTopology,
    operation: TypedAccessRequestOperation,
    selection: ContractVersionSelection,
    access_policy_digest: Sha256Digest,
    probe: NativeArtifactProbe,
    grants: Vec<DeclaredKnownObjectGrant>,
    digest: Sha256Digest,
}

impl std::fmt::Debug for TrustedNativeProbeGrantRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TrustedNativeProbeGrantRequest")
            .field("adapter_id", &self.adapter_id)
            .field("operation", &self.operation)
            .field("topology", &self.topology)
            .field("program_id", &self.program_id)
            .field("grant_count", &self.grants.len())
            .field("probe_has_version", &self.probe.version.is_some())
            .field("probe_marker_count", &self.probe.markers.len())
            .finish_non_exhaustive()
    }
}

impl TrustedNativeProbeGrantRequest {
    fn from_authorization(
        authorization: &TypedAccessAuthorization,
        access_policy_digest: Sha256Digest,
        program_id: Option<&str>,
    ) -> Result<Self, SupportContractError> {
        let probe = request_bound_probe(&authorization.probe)?;
        let access_policy_digest =
            require_nonzero_digest(access_policy_digest, "access policy digest")?;
        let operation = typed_request_operation(authorization.operation.operation)?;
        validate_request_selection(authorization.contracts.view(), operation)?;
        let topology = topology_for_operation(operation);
        let support_release_id = authorization
            .operation
            .support_release_id()
            .ok_or_else(|| {
                SupportContractError::invalid(
                    "native-probe/grant request requires a selected promoted support release",
                )
            })?
            .to_string();
        let (program_id, grants) = match (operation, program_id) {
            (TypedAccessRequestOperation::ScopedTypedObservation, Some(program_id)) => {
                let program = authorization.select_scope_program(program_id)?;
                (
                    program.program_id().to_string(),
                    declared_known_object_grants(program.program)?,
                )
            }
            (TypedAccessRequestOperation::ScopedTypedObservation, None) => {
                return Err(SupportContractError::invalid(
                    "scoped probe/grant request requires an exact program id",
                ));
            }
            (_, Some(_)) => {
                return Err(SupportContractError::invalid(
                    "catalog and durable probe/grant requests cannot name a scope program",
                ));
            }
            (_, None) => (String::new(), Vec::new()),
        };
        if operation == TypedAccessRequestOperation::CatalogDiscovery {
            authorization.select_catalog_access()?;
        }
        validate_request_identity_coordinates(
            &authorization.adapter_id,
            &support_release_id,
            &authorization.scope_programs.declaration_id,
            &program_id,
            operation,
        )?;
        let mut request = Self {
            contract_version: ACCESS_REQUEST_CONTRACT_VERSION,
            adapter_id: authorization.adapter_id.clone(),
            support_release_id,
            support_release_digest: authorization.support_release_digest,
            source_declaration_digest: authorization.source_declaration_digest,
            scope_program_digest: authorization.scope_program_digest,
            declaration_id: authorization.scope_programs.declaration_id.clone(),
            program_id,
            topology,
            operation,
            selection: authorization.contracts.clone(),
            access_policy_digest,
            probe,
            grants,
            digest: Sha256Digest::from_bytes([0; 32]),
        };
        request.digest = request.calculate_digest();
        Ok(request)
    }

    fn calculate_digest(&self) -> Sha256Digest {
        let mut hasher = Sha256::new();
        hasher.update(b"spaghetti/rfc012a/native-probe-grant-request/v1\0");
        AccessRequestDigestCoordinates {
            contract_version: self.contract_version,
            adapter_id: &self.adapter_id,
            support_release_id: &self.support_release_id,
            support_release_digest: self.support_release_digest,
            source_declaration_digest: self.source_declaration_digest,
            scope_program_digest: self.scope_program_digest,
            declaration_id: &self.declaration_id,
            program_id: &self.program_id,
            topology: self.topology,
            operation: self.operation,
            selection: self.selection.view(),
            access_policy_digest: self.access_policy_digest,
        }
        .write(&mut hasher);
        hash_probe(&mut hasher, &self.probe);
        let grants: Vec<_> = self
            .grants
            .iter()
            .map(DeclaredKnownObjectGrant::to_wire)
            .collect();
        hash_grants(&mut hasher, &grants);
        Sha256Digest::from_bytes(hasher.finalize().into())
    }

    fn project(&self) -> NativeProbeGrantRequestWire {
        NativeProbeGrantRequestWire {
            access_request_contract_version: self.contract_version,
            adapter_id: self.adapter_id.clone(),
            support_release_id: self.support_release_id.clone(),
            support_release_digest: *self.support_release_digest.as_bytes(),
            source_declaration_digest: *self.source_declaration_digest.as_bytes(),
            scope_program_digest: *self.scope_program_digest.as_bytes(),
            declaration_id: self.declaration_id.clone(),
            program_id: self.program_id.clone(),
            capability_topology: self.topology,
            operation: self.operation,
            selection: ContractVersionSelectionWire::from_selection(&self.selection),
            access_policy_digest: *self.access_policy_digest.as_bytes(),
            probe: NativeArtifactProbeWire::from_probe(&self.probe),
            grants: self
                .grants
                .iter()
                .map(DeclaredKnownObjectGrant::to_wire)
                .collect(),
            digest: *self.digest.as_bytes(),
        }
    }

    fn matches_authorization(&self, authorization: &TypedAccessAuthorization) -> bool {
        typed_request_operation(authorization.operation.operation).ok() == Some(self.operation)
            && authorization.adapter_id == self.adapter_id
            && authorization.operation.support_release_id()
                == Some(self.support_release_id.as_str())
            && authorization.support_release_digest == self.support_release_digest
            && authorization.source_declaration_digest == self.source_declaration_digest
            && authorization.scope_program_digest == self.scope_program_digest
            && authorization.scope_programs.declaration_id == self.declaration_id
            && authorization.contracts == self.selection
            && request_bound_probe(&authorization.probe).is_ok_and(|probe| probe == self.probe)
            && match self.operation {
                TypedAccessRequestOperation::ScopedTypedObservation => authorization
                    .select_scope_program(&self.program_id)
                    .ok()
                    .and_then(|program| declared_known_object_grants(program.program).ok())
                    .is_some_and(|grants| grants == self.grants),
                TypedAccessRequestOperation::CatalogDiscovery => {
                    self.program_id.is_empty()
                        && self.grants.is_empty()
                        && authorization.select_catalog_access().is_ok()
                }
                TypedAccessRequestOperation::DurableHistoryRuntime => {
                    self.program_id.is_empty() && self.grants.is_empty()
                }
            }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeProbeGrantRequestWire {
    access_request_contract_version: u32,
    adapter_id: String,
    support_release_id: String,
    support_release_digest: [u8; 32],
    source_declaration_digest: [u8; 32],
    scope_program_digest: [u8; 32],
    declaration_id: String,
    program_id: String,
    capability_topology: SupportCapabilityTopology,
    operation: TypedAccessRequestOperation,
    selection: ContractVersionSelectionWire,
    access_policy_digest: [u8; 32],
    probe: NativeArtifactProbeWire,
    grants: Vec<DeclaredKnownObjectGrantWire>,
    digest: [u8; 32],
}

impl NativeProbeGrantRequestWire {
    fn validate_structure(&self) -> Result<(), SupportContractError> {
        if self.access_request_contract_version != ACCESS_REQUEST_CONTRACT_VERSION {
            return Err(SupportContractError::invalid(
                "unsupported native-probe/grant request contract version",
            ));
        }
        validate_request_machine_id("request adapter id", &self.adapter_id)?;
        validate_request_machine_id("request support release id", &self.support_release_id)?;
        validate_request_machine_id("request declaration id", &self.declaration_id)?;
        require_nonzero_digest(
            Sha256Digest::from_bytes(self.support_release_digest),
            "support release digest",
        )?;
        require_nonzero_digest(
            Sha256Digest::from_bytes(self.source_declaration_digest),
            "source declaration digest",
        )?;
        require_nonzero_digest(
            Sha256Digest::from_bytes(self.scope_program_digest),
            "scope program digest",
        )?;
        require_nonzero_digest(
            Sha256Digest::from_bytes(self.access_policy_digest),
            "access policy digest",
        )?;
        if topology_for_operation(self.operation) != self.capability_topology {
            return Err(SupportContractError::invalid(
                "probe/grant request topology does not match its operation",
            ));
        }
        if self.grants.len() > MAX_ACCESS_REQUEST_GRANTS {
            return Err(SupportContractError::invalid(
                "probe/grant request exceeds the grant collection limit",
            ));
        }
        self.probe.validate()?;
        validate_request_selection(self.selection.view(), self.operation)?;
        match self.operation {
            TypedAccessRequestOperation::ScopedTypedObservation => {
                validate_request_machine_id("request program id", &self.program_id)?;
                if self.grants.is_empty() {
                    return Err(SupportContractError::invalid(
                        "scoped probe/grant request requires a bounded nonempty grant set",
                    ));
                }
            }
            TypedAccessRequestOperation::CatalogDiscovery
            | TypedAccessRequestOperation::DurableHistoryRuntime => {
                if !self.program_id.is_empty() || !self.grants.is_empty() {
                    return Err(SupportContractError::invalid(
                        "catalog and durable probe/grant requests cannot carry grants or a program id",
                    ));
                }
            }
        }
        let mut previous: Option<&str> = None;
        let mut root_count = 0;
        let mut encoded_bytes = 0usize;
        for grant in &self.grants {
            grant.validate(&mut encoded_bytes)?;
            if previous.is_some_and(|id| id >= grant.relation_id.as_str()) {
                return Err(SupportContractError::invalid(
                    "probe/grant relation ids must be strictly increasing",
                ));
            }
            previous = Some(&grant.relation_id);
            if grant.scope_root {
                root_count += 1;
            }
        }
        if self.operation == TypedAccessRequestOperation::ScopedTypedObservation && root_count != 1
        {
            return Err(SupportContractError::invalid(
                "scoped probe/grant request requires exactly one scope-root grant",
            ));
        }
        Ok(())
    }

    fn calculate_digest(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(b"spaghetti/rfc012a/native-probe-grant-request/v1\0");
        AccessRequestDigestCoordinates {
            contract_version: self.access_request_contract_version,
            adapter_id: &self.adapter_id,
            support_release_id: &self.support_release_id,
            support_release_digest: Sha256Digest::from_bytes(self.support_release_digest),
            source_declaration_digest: Sha256Digest::from_bytes(self.source_declaration_digest),
            scope_program_digest: Sha256Digest::from_bytes(self.scope_program_digest),
            declaration_id: &self.declaration_id,
            program_id: &self.program_id,
            topology: self.capability_topology,
            operation: self.operation,
            selection: self.selection.view(),
            access_policy_digest: Sha256Digest::from_bytes(self.access_policy_digest),
        }
        .write(&mut hasher);
        self.probe.hash(&mut hasher);
        hash_grants(&mut hasher, &self.grants);
        hasher.finalize().into()
    }

    fn parse(value: serde_json::Value) -> Result<Self, SupportContractError> {
        preflight_probe_grant_request(&value)?;
        let mut parsed: Self = serde_json::from_value(value).map_err(|_| {
            SupportContractError::invalid("native-probe/grant request is not valid JSON")
        })?;
        parsed.validate_structure()?;
        parsed.probe.markers.sort();
        parsed.probe.markers.dedup();
        if parsed.digest != parsed.calculate_digest() {
            return Err(SupportContractError::invalid(
                "native-probe/grant request digest does not match its canonical encoding",
            ));
        }
        Ok(parsed)
    }
}

/// Private scoped access-report retrieval request bound to one report digest.
struct ScopeAccessReportRetrieval {
    contract_version: u32,
    adapter_id: String,
    support_release_id: String,
    support_release_digest: Sha256Digest,
    source_declaration_digest: Sha256Digest,
    scope_program_digest: Sha256Digest,
    declaration_id: String,
    program_id: String,
    topology: SupportCapabilityTopology,
    operation: TypedAccessRequestOperation,
    selection: ContractVersionSelection,
    access_policy_digest: Sha256Digest,
    expected_report_digest: Sha256Digest,
    digest: Sha256Digest,
}

impl std::fmt::Debug for ScopeAccessReportRetrieval {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ScopeAccessReportRetrieval")
            .field("adapter_id", &self.adapter_id)
            .field("program_id", &self.program_id)
            .field("operation", &self.operation)
            .finish_non_exhaustive()
    }
}

impl ScopeAccessReportRetrieval {
    fn from_authorization(
        authorization: &TypedAccessAuthorization,
        program_id: &str,
        access_policy_digest: Sha256Digest,
        expected_report_digest: Sha256Digest,
    ) -> Result<Self, SupportContractError> {
        let access_policy_digest =
            require_nonzero_digest(access_policy_digest, "access policy digest")?;
        let expected_report_digest =
            require_nonzero_digest(expected_report_digest, "expected report digest")?;
        validate_request_selection(
            authorization.contracts.view(),
            TypedAccessRequestOperation::ScopedTypedObservation,
        )?;
        let program = authorization.select_scope_program(program_id)?;
        validate_request_identity_coordinates(
            &authorization.adapter_id,
            program.support_release_id(),
            &authorization.scope_programs.declaration_id,
            program.program_id(),
            TypedAccessRequestOperation::ScopedTypedObservation,
        )?;
        let mut request = Self {
            contract_version: ACCESS_REPORT_RETRIEVAL_CONTRACT_VERSION,
            adapter_id: authorization.adapter_id.clone(),
            support_release_id: program.support_release_id().to_string(),
            support_release_digest: authorization.support_release_digest,
            source_declaration_digest: authorization.source_declaration_digest,
            scope_program_digest: authorization.scope_program_digest,
            declaration_id: authorization.scope_programs.declaration_id.clone(),
            program_id: program.program_id().to_string(),
            topology: SupportCapabilityTopology::Scoped,
            operation: TypedAccessRequestOperation::ScopedTypedObservation,
            selection: authorization.contracts.clone(),
            access_policy_digest,
            expected_report_digest,
            digest: Sha256Digest::from_bytes([0; 32]),
        };
        request.digest = request.calculate_digest();
        Ok(request)
    }

    fn calculate_digest(&self) -> Sha256Digest {
        let mut hasher = Sha256::new();
        hasher.update(b"spaghetti/rfc012a/access-report-retrieval/v1\0");
        AccessRequestDigestCoordinates {
            contract_version: self.contract_version,
            adapter_id: &self.adapter_id,
            support_release_id: &self.support_release_id,
            support_release_digest: self.support_release_digest,
            source_declaration_digest: self.source_declaration_digest,
            scope_program_digest: self.scope_program_digest,
            declaration_id: &self.declaration_id,
            program_id: &self.program_id,
            topology: self.topology,
            operation: self.operation,
            selection: self.selection.view(),
            access_policy_digest: self.access_policy_digest,
        }
        .write(&mut hasher);
        hash_component(&mut hasher, self.expected_report_digest.as_bytes());
        Sha256Digest::from_bytes(hasher.finalize().into())
    }

    fn project(&self) -> ScopeAccessReportRetrievalWire {
        ScopeAccessReportRetrievalWire {
            access_report_retrieval_contract_version: self.contract_version,
            adapter_id: self.adapter_id.clone(),
            support_release_id: self.support_release_id.clone(),
            support_release_digest: *self.support_release_digest.as_bytes(),
            source_declaration_digest: *self.source_declaration_digest.as_bytes(),
            scope_program_digest: *self.scope_program_digest.as_bytes(),
            declaration_id: self.declaration_id.clone(),
            program_id: self.program_id.clone(),
            capability_topology: self.topology,
            operation: self.operation,
            selection: ContractVersionSelectionWire::from_selection(&self.selection),
            access_policy_digest: *self.access_policy_digest.as_bytes(),
            expected_report_digest: *self.expected_report_digest.as_bytes(),
            digest: *self.digest.as_bytes(),
        }
    }

    fn matches_authorization(&self, authorization: &TypedAccessAuthorization) -> bool {
        authorization.operation.operation == SupportOperation::ScopedTypedObservation
            && authorization.adapter_id == self.adapter_id
            && authorization.operation.support_release_id()
                == Some(self.support_release_id.as_str())
            && authorization.support_release_digest == self.support_release_digest
            && authorization.source_declaration_digest == self.source_declaration_digest
            && authorization.scope_program_digest == self.scope_program_digest
            && authorization.scope_programs.declaration_id == self.declaration_id
            && authorization.contracts == self.selection
            && authorization.select_scope_program(&self.program_id).is_ok()
    }

    fn matches_report_digest(&self, digest: Sha256Digest) -> bool {
        self.expected_report_digest == digest
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopeAccessReportRetrievalWire {
    access_report_retrieval_contract_version: u32,
    adapter_id: String,
    support_release_id: String,
    support_release_digest: [u8; 32],
    source_declaration_digest: [u8; 32],
    scope_program_digest: [u8; 32],
    declaration_id: String,
    program_id: String,
    capability_topology: SupportCapabilityTopology,
    operation: TypedAccessRequestOperation,
    selection: ContractVersionSelectionWire,
    access_policy_digest: [u8; 32],
    expected_report_digest: [u8; 32],
    digest: [u8; 32],
}

impl ScopeAccessReportRetrievalWire {
    fn validate_structure(&self) -> Result<(), SupportContractError> {
        if self.access_report_retrieval_contract_version != ACCESS_REPORT_RETRIEVAL_CONTRACT_VERSION
        {
            return Err(SupportContractError::invalid(
                "unsupported access-report retrieval contract version",
            ));
        }
        validate_request_machine_id("retrieval adapter id", &self.adapter_id)?;
        validate_request_machine_id("retrieval support release id", &self.support_release_id)?;
        validate_request_machine_id("retrieval declaration id", &self.declaration_id)?;
        validate_request_machine_id("retrieval program id", &self.program_id)?;
        require_nonzero_digest(
            Sha256Digest::from_bytes(self.support_release_digest),
            "support release digest",
        )?;
        require_nonzero_digest(
            Sha256Digest::from_bytes(self.source_declaration_digest),
            "source declaration digest",
        )?;
        require_nonzero_digest(
            Sha256Digest::from_bytes(self.scope_program_digest),
            "scope program digest",
        )?;
        require_nonzero_digest(
            Sha256Digest::from_bytes(self.access_policy_digest),
            "access policy digest",
        )?;
        require_nonzero_digest(
            Sha256Digest::from_bytes(self.expected_report_digest),
            "expected report digest",
        )?;
        if self.capability_topology != SupportCapabilityTopology::Scoped
            || self.operation != TypedAccessRequestOperation::ScopedTypedObservation
        {
            return Err(SupportContractError::invalid(
                "access-report retrieval is scoped observation only",
            ));
        }
        validate_request_selection(self.selection.view(), self.operation)?;
        Ok(())
    }

    fn calculate_digest(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(b"spaghetti/rfc012a/access-report-retrieval/v1\0");
        AccessRequestDigestCoordinates {
            contract_version: self.access_report_retrieval_contract_version,
            adapter_id: &self.adapter_id,
            support_release_id: &self.support_release_id,
            support_release_digest: Sha256Digest::from_bytes(self.support_release_digest),
            source_declaration_digest: Sha256Digest::from_bytes(self.source_declaration_digest),
            scope_program_digest: Sha256Digest::from_bytes(self.scope_program_digest),
            declaration_id: &self.declaration_id,
            program_id: &self.program_id,
            topology: self.capability_topology,
            operation: self.operation,
            selection: self.selection.view(),
            access_policy_digest: Sha256Digest::from_bytes(self.access_policy_digest),
        }
        .write(&mut hasher);
        hash_component(&mut hasher, &self.expected_report_digest);
        hasher.finalize().into()
    }

    fn parse(value: serde_json::Value) -> Result<Self, SupportContractError> {
        preflight_access_report_retrieval(&value)?;
        let parsed: Self = serde_json::from_value(value).map_err(|_| {
            SupportContractError::invalid("access-report retrieval request is not valid JSON")
        })?;
        parsed.validate_structure()?;
        if parsed.digest != parsed.calculate_digest() {
            return Err(SupportContractError::invalid(
                "access-report retrieval digest does not match its canonical encoding",
            ));
        }
        Ok(parsed)
    }

    fn matches_report_digest(&self, digest: &[u8; 32]) -> bool {
        &self.expected_report_digest == digest
    }
}

#[cfg(test)]
mod tests {
    use crate::adapter::{
        ScopeProgramDeclaration, ScopeRelationBounds, ScopeRelationDeclaration,
        ScopeRelationPrimitive, ScopeUnavailableBehavior,
    };

    use super::*;
    use serde_json::Value;

    #[derive(Debug, Deserialize)]
    struct SupportFixture {
        fixture_contract_version: u32,
        releases: Vec<SupportReleaseDescriptor>,
        runtime_cases: Vec<RuntimeFixtureCase>,
        contract_request: ContractVersionRequest,
        contract_offer: ContractVersionOffer,
        expected_contract_selection: ContractVersionSelection,
    }

    #[derive(Debug, Deserialize)]
    struct RuntimeFixtureCase {
        name: String,
        probe: NativeArtifactProbe,
        expected: CompatibilityDecision,
    }

    fn fixture() -> SupportFixture {
        serde_json::from_str(include_str!(
            "../../fixtures/contracts/rfc012a-support-v1.json"
        ))
        .unwrap()
    }

    fn fixture_binding(support_release_id: &str) -> AdapterSupportBinding {
        AdapterSupportBinding {
            support_release_id: support_release_id.to_string(),
            adapter_package_version: "1.0.0".to_string(),
            decoder_contract_version: 1,
            ads_digest: Sha256Digest::of(b"fixture-ads"),
            source_declaration_digest: Sha256Digest::of(b"fixture-source"),
            scope_program_digest: Sha256Digest::of(b"fixture-scope"),
        }
    }

    fn fixture_scope_manifest(
        adapter_id: &str,
        status: SupportReleaseStatus,
    ) -> ScopeProgramManifest {
        ScopeProgramManifest {
            schema_version: 1,
            declaration_id: format!("{adapter_id}-scope"),
            adapter_id: adapter_id.to_string(),
            ads_id: format!("{adapter_id}-ads"),
            status: match status {
                SupportReleaseStatus::Candidate => ScopeProgramStatus::Candidate,
                SupportReleaseStatus::Promoted => ScopeProgramStatus::Promoted,
                SupportReleaseStatus::Retired => ScopeProgramStatus::Retired,
            },
            roots: vec!["root".to_string()],
            programs: vec![ScopeProgramDeclaration {
                program_id: "observe-session".to_string(),
                root_entity_kind: "session".to_string(),
                root_relation_id: Some("root-object".to_string()),
                relations: vec![ScopeRelationDeclaration {
                    relation_id: "root-object".to_string(),
                    primitive: ScopeRelationPrimitive::KnownObject,
                    access_root: "root".to_string(),
                    locator: "known-object".to_string(),
                    identity_inputs: vec!["native-session-id".to_string()],
                    bounds: ScopeRelationBounds {
                        max_fan_out: 1,
                        max_depth: 1,
                        max_objects: 1,
                        max_bytes: 1024,
                        max_rows: 0,
                    },
                    statement_id: None,
                    parameter_names: None,
                    source_binding: None,
                    observation_binding: None,
                    unavailable_behavior: ScopeUnavailableBehavior::RecordUnavailable,
                    claim_refs: vec!["scope-evidence".to_string()],
                }],
                claim_refs: vec!["scope-evidence".to_string()],
            }],
            blockers: Vec::new(),
            claim_refs: vec!["scope-evidence".to_string()],
        }
    }

    fn fixture_catalog(releases: &[SupportReleaseDescriptor]) -> SupportCatalog {
        SupportCatalog::new(releases.iter().map(|descriptor| VerifiedSupportRelease {
            release_digest: Sha256Digest::of(descriptor.support_release_id.as_bytes()),
            adapter_id: descriptor.artifact_compatibility.family.clone(),
            descriptor: descriptor.clone(),
            adapter_binding: fixture_binding(&descriptor.support_release_id),
            scope_programs: fixture_scope_manifest(
                &descriptor.artifact_compatibility.family,
                descriptor.status,
            ),
            observation_source_contracts: BTreeMap::new(),
        }))
        .unwrap()
    }

    #[test]
    fn runtime_classification_matches_shared_fixture() {
        let fixture = fixture();
        assert_eq!(fixture.fixture_contract_version, 1);
        for case in fixture.runtime_cases {
            let actual = classify_runtime_support(&case.probe, &fixture.releases).unwrap();
            assert_eq!(actual, case.expected, "case {}", case.name);
        }
    }

    #[test]
    fn contract_selection_matches_shared_fixture() {
        let fixture = fixture();
        let selection =
            select_contract_versions(&fixture.contract_request, &fixture.contract_offer).unwrap();
        assert_eq!(selection, fixture.expected_contract_selection);

        let mut incompatible = fixture.contract_request;
        incompatible.model_major += 1;
        assert!(select_contract_versions(&incompatible, &fixture.contract_offer).is_err());
    }

    #[test]
    fn candidate_cannot_authorize_typed_access_or_observe_offer() {
        let fixture = fixture();
        let candidate_probe = fixture
            .runtime_cases
            .iter()
            .find(|case| case.name == "candidate-only-family")
            .unwrap();
        let catalog = fixture_catalog(&fixture.releases);
        let decision = catalog.classify(&candidate_probe.probe).unwrap();
        assert_eq!(
            decision.compatibility_class,
            CompatibilityClass::RecognizedUnverified
        );

        let mut malformed_request = fixture.contract_request;
        malformed_request.selection_contract_version = 0;
        let binding = fixture_binding("candidate-only-support-v1");
        let scope_programs =
            fixture_scope_manifest("candidate-agent", SupportReleaseStatus::Candidate);
        let error = catalog
            .authorize_typed_access(
                AdapterSupportRegistration::new("candidate-agent", &binding, &scope_programs),
                &candidate_probe.probe,
                SupportOperation::CatalogDiscovery,
                &malformed_request,
                &fixture.contract_offer,
            )
            .unwrap_err();
        assert!(error.to_string().contains("forbidden"));
    }

    #[test]
    fn exact_and_forward_catalog_decisions_issue_narrow_tokens() {
        let fixture = fixture();
        let catalog = fixture_catalog(&fixture.releases);
        let exact = fixture
            .runtime_cases
            .iter()
            .find(|case| case.name == "exact")
            .unwrap();
        let binding = fixture_binding("fixture-support-v1");
        let scope_programs =
            fixture_scope_manifest("fixture-agent", SupportReleaseStatus::Promoted);
        let (exact_decision, durable) = catalog
            .authorize_typed_access(
                AdapterSupportRegistration::new("fixture-agent", &binding, &scope_programs),
                &exact.probe,
                SupportOperation::DurableHistoryRuntime,
                &fixture.contract_request,
                &fixture.contract_offer,
            )
            .unwrap();
        assert_eq!(
            exact_decision.compatibility_class,
            CompatibilityClass::ExactSupported
        );
        assert_eq!(
            durable.operation().support_release_id(),
            Some("fixture-support-v1")
        );
        assert!(durable.select_catalog_access().is_err());
        assert!(durable.select_scope_program("observe-session").is_err());

        let (catalog_decision, catalog_authorization) = catalog
            .authorize_typed_access(
                AdapterSupportRegistration::new("fixture-agent", &binding, &scope_programs),
                &exact.probe,
                SupportOperation::CatalogDiscovery,
                &fixture.contract_request,
                &fixture.contract_offer,
            )
            .unwrap();
        assert_eq!(
            catalog_decision.compatibility_class,
            CompatibilityClass::ExactSupported
        );
        let catalog_access = catalog_authorization.select_catalog_access().unwrap();
        assert_eq!(catalog_access.adapter_id(), "fixture-agent");
        assert_eq!(catalog_access.support_release_id(), "fixture-support-v1");
        assert_eq!(
            catalog_access.support_release_digest(),
            Sha256Digest::of(b"fixture-support-v1")
        );
        assert_eq!(
            catalog_access.source_declaration_digest(),
            binding.source_declaration_digest()
        );
        assert_eq!(
            catalog_access.contracts(),
            &fixture.expected_contract_selection
        );
        assert!(catalog_access.source_contracts().is_some());
        assert!(catalog_authorization
            .select_scope_program("observe-session")
            .is_err());

        let mut no_query_pack = fixture.contract_request.clone();
        no_query_pack.query_pack_versions = None;
        let (_, no_query_pack_authorization) = catalog
            .authorize_typed_access(
                AdapterSupportRegistration::new("fixture-agent", &binding, &scope_programs),
                &exact.probe,
                SupportOperation::CatalogDiscovery,
                &no_query_pack,
                &fixture.contract_offer,
            )
            .unwrap();
        let error = no_query_pack_authorization
            .select_catalog_access()
            .unwrap_err();
        assert!(error.to_string().contains("query-pack"));

        let (_, scoped) = catalog
            .authorize_typed_access(
                AdapterSupportRegistration::new("fixture-agent", &binding, &scope_programs),
                &exact.probe,
                SupportOperation::ScopedTypedObservation,
                &fixture.contract_request,
                &fixture.contract_offer,
            )
            .unwrap();
        let program = scoped.select_scope_program("observe-session").unwrap();
        assert_eq!(program.adapter_id(), "fixture-agent");
        assert_eq!(program.support_release_id(), "fixture-support-v1");
        assert_eq!(program.observation_contract_version(), 1);
        assert!(scoped.select_catalog_access().is_err());
        assert!(scoped.select_scope_program("unknown-program").is_err());

        let forward_case = fixture
            .runtime_cases
            .iter()
            .find(|case| case.name == "forward-catalog")
            .unwrap();
        let forward_decision =
            classify_runtime_support(&forward_case.probe, &fixture.releases).unwrap();
        assert!(forward_decision
            .authorize(SupportOperation::DurableHistoryRuntime)
            .is_err());
        assert!(forward_decision
            .authorize(SupportOperation::CatalogDiscovery)
            .is_ok());
        let (forward_decision, forward_authorization) = catalog
            .authorize_typed_access(
                AdapterSupportRegistration::new("fixture-agent", &binding, &scope_programs),
                &forward_case.probe,
                SupportOperation::CatalogDiscovery,
                &fixture.contract_request,
                &fixture.contract_offer,
            )
            .unwrap();
        assert_eq!(
            forward_decision.compatibility_class,
            CompatibilityClass::RecognizedUnverified
        );
        let forward_access = forward_authorization.select_catalog_access().unwrap();
        assert_eq!(forward_access.adapter_id(), "fixture-agent");
        assert_eq!(forward_access.support_release_id(), "fixture-support-v1");
        assert_eq!(
            forward_access.contracts(),
            &fixture.expected_contract_selection
        );
        assert!(forward_access.source_contracts().is_some());

        let restricted = fixture
            .runtime_cases
            .iter()
            .find(|case| case.name == "exact-capability-restricted")
            .unwrap();
        let restricted_binding = fixture_binding("capability-restricted-support-v1");
        let restricted_scope = fixture_scope_manifest(
            "capability-restricted-agent",
            SupportReleaseStatus::Promoted,
        );
        let restricted_registration = AdapterSupportRegistration::new(
            "capability-restricted-agent",
            &restricted_binding,
            &restricted_scope,
        );
        for operation in [
            SupportOperation::CatalogDiscovery,
            SupportOperation::DurableHistoryRuntime,
            SupportOperation::ScopedTypedObservation,
        ] {
            let error = catalog
                .authorize_typed_access(
                    restricted_registration,
                    &restricted.probe,
                    operation,
                    &fixture.contract_request,
                    &fixture.contract_offer,
                )
                .unwrap_err();
            assert!(error.to_string().contains("forbidden"));
        }
    }

    #[test]
    fn support_bundle_verification_binds_every_referenced_document() {
        let paths = [
            (
                "ads",
                "support/ads.json",
                br#"{"adapter_id":"fixture-agent","ads_id":"fixture-agent-ads"}"#.as_slice(),
            ),
            (
                "source_declaration",
                "support/source.json",
                br#"{"adapter_id":"fixture-agent","ads_id":"fixture-agent-ads"}"#.as_slice(),
            ),
            (
                "scope_program",
                "support/scope.json",
                br#"{"schema_version":1,"declaration_id":"fixture-agent-scope","adapter_id":"fixture-agent","ads_id":"fixture-agent-ads","status":"promoted","roots":["root"],"programs":[{"program_id":"observe-session","root_entity_kind":"session","root_relation_id":"root-object","relations":[{"relation_id":"root-object","primitive":"KnownObject","access_root":"root","locator":"known-object","identity_inputs":["native-session-id"],"bounds":{"max_fan_out":1,"max_depth":1,"max_objects":1,"max_bytes":1024,"max_rows":0},"unavailable_behavior":"record_unavailable","claim_refs":["scope-evidence"]}],"claim_refs":["scope-evidence"]}],"blockers":[],"claim_refs":["scope-evidence"]}"#.as_slice(),
            ),
            (
                "evidence",
                "support/evidence.json",
                br#"{"adapter_id":"fixture-agent","ads_id":"fixture-agent-ads"}"#.as_slice(),
            ),
            (
                "conformance",
                "support/conformance.json",
                br#"{"adapter_id":"fixture-agent","support_release_id":"fixture-support"}"#
                    .as_slice(),
            ),
        ];
        let references = paths
            .iter()
            .map(|(kind, path, bytes)| {
                (
                    (*kind).to_string(),
                    serde_json::json!({"path": path, "sha256": Sha256Digest::of(bytes).to_string()}),
                )
            })
            .collect::<serde_json::Map<_, _>>();
        let release = serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "support_release_id": "fixture-support",
            "adapter_id": "fixture-agent",
            "status": "promoted",
            "artifact_compatibility": {
                "family": "fixture-agent",
                "platforms": ["test"],
                "exact_versions": ["1.0"],
                "ranges": [],
                "required_markers": [],
                "forward_catalog_only": false
            },
            "references": references,
            "versions": {"adapter_package": "1.0.0", "decoder_contract": 1},
            "capabilities": [
                {
                    "capability_id": "fixture-catalog",
                    "topology": "catalog",
                    "level": "supported",
                    "notes": null
                },
                {
                    "capability_id": "fixture-history",
                    "topology": "durable",
                    "level": "supported",
                    "notes": "fixture capability"
                },
                {
                    "capability_id": "fixture-observation",
                    "topology": "scoped",
                    "level": "degraded",
                    "notes": null
                }
            ]
        }))
        .unwrap();
        let documents = paths
            .iter()
            .map(|(_, path, bytes)| SupportBundleDocument::new(path, bytes))
            .collect::<Vec<_>>();

        let verified = verify_support_release_bundle(&release, &documents).unwrap();
        assert_eq!(verified.adapter_id(), "fixture-agent");
        assert_eq!(verified.descriptor().status, SupportReleaseStatus::Promoted);
        assert_eq!(verified.descriptor().capabilities.len(), 3);
        assert_eq!(
            verified.descriptor().declared_operation_permissions(),
            OperationPermissions {
                version_probe: true,
                catalog: true,
                durable: true,
                scoped_observation: false,
                bounded_drift: true,
            }
        );
        assert_eq!(
            verified.adapter_binding().adapter_package_version(),
            "1.0.0"
        );

        let mut tampered = documents.clone();
        tampered[0] = SupportBundleDocument::new("support/ads.json", b"{}");
        assert!(verify_support_release_bundle(&release, &tampered)
            .unwrap_err()
            .to_string()
            .contains("digest"));

        let mut missing_capabilities: Value = serde_json::from_slice(&release).unwrap();
        missing_capabilities
            .as_object_mut()
            .unwrap()
            .remove("capabilities");
        let missing_capabilities = serde_json::to_vec(&missing_capabilities).unwrap();
        assert!(
            verify_support_release_bundle(&missing_capabilities, &documents)
                .unwrap_err()
                .to_string()
                .contains("capabilities")
        );

        let mut invalid_notes: Value = serde_json::from_slice(&release).unwrap();
        invalid_notes["capabilities"][0]["notes"] = serde_json::json!(false);
        let invalid_notes = serde_json::to_vec(&invalid_notes).unwrap();
        assert!(verify_support_release_bundle(&invalid_notes, &documents)
            .unwrap_err()
            .to_string()
            .contains("notes"));
    }

    #[test]
    fn support_scope_source_binding_is_checked_against_the_digest_bound_source_document() {
        let scope = ScopeProgramManifest::from_json(br#"{
          "schema_version": 1,
          "declaration_id": "fixture-scope",
          "adapter_id": "fixture",
          "ads_id": "fixture-ads",
          "status": "promoted",
          "roots": ["root"],
          "programs": [{
            "program_id": "observe-session",
            "root_entity_kind": "session",
            "root_relation_id": "root-object",
            "relations": [
              {
                "relation_id": "root-object",
                "primitive": "KnownObject",
                "access_root": "root",
                "locator": "known-object",
                "identity_inputs": ["native-session-id"],
                "bounds": {"max_fan_out": 1, "max_depth": 1, "max_objects": 1, "max_bytes": 1024, "max_rows": 0},
                "unavailable_behavior": "record_unavailable",
                "claim_refs": ["scope-evidence"]
              },
              {
                "relation_id": "artifact-object",
                "primitive": "ArtifactLocatorFromEvidence",
                "access_root": "root",
                "locator": "artifacts/{native-session-id}",
                "identity_inputs": ["native-session-id"],
                "bounds": {"max_fan_out": 1, "max_depth": 1, "max_objects": 1, "max_bytes": 1024, "max_rows": 0},
                "source_binding": {"stream_id": "artifact-stream", "primitive": "ReplaceDocument", "max_object_bytes": 1024},
                "unavailable_behavior": "record_unavailable",
                "claim_refs": ["scope-evidence"]
              }
            ],
            "claim_refs": ["scope-evidence"]
          }],
          "blockers": [],
          "claim_refs": ["scope-evidence"]
        }"#)
        .unwrap();
        let mut source: SupportSourceDeclarationWire = serde_json::from_value(serde_json::json!({
            "streams": [{
                "stream_id": "artifact-stream",
                "root_id": "root",
                "primitive": "ReplaceDocument",
                "topologies": ["durable", "scoped"],
                "implementation_state": "existing",
                "bounds": {"max_object_bytes": 1024},
                "lifecycle": ["replace", "identity_change", "delete", "recreate"],
                "safe_decoder_state_boundary": "object_generation_revision"
            }]
        }))
        .unwrap();
        validate_scope_source_bindings(&scope, &source, false).unwrap();

        source.streams[0].bounds.max_object_bytes = Some(2048);
        assert!(validate_scope_source_bindings(&scope, &source, false)
            .unwrap_err()
            .to_string()
            .contains("does not match"));
        source.streams[0].bounds.max_object_bytes = Some(1024);
        source.streams[0].topologies = vec!["durable".to_string()];
        assert!(validate_scope_source_bindings(&scope, &source, false)
            .unwrap_err()
            .to_string()
            .contains("does not match"));
    }

    #[test]
    fn promoted_directory_membership_retains_exact_bounds_and_lifecycle() {
        let scope = fixture_scope_manifest("fixture", SupportReleaseStatus::Promoted);
        let mut source: SupportSourceDeclarationWire = serde_json::from_value(serde_json::json!({
            "streams": [{
                "stream_id": "session-membership",
                "root_id": "sessions",
                "relative_patterns": ["**/summary.json"],
                "decoder_id": "fixture-membership",
                "authority": "supplemental",
                "primitive": "DirectoryMembership",
                "topologies": ["durable"],
                "implementation_state": "existing",
                "bounds": {"max_entries": 100_000, "max_depth": 8},
                "lifecycle": ["membership_change", "identity_change", "delete", "recreate"],
                "safe_decoder_state_boundary": "full_snapshot"
            }]
        }))
        .unwrap();
        let contracts = validate_scope_source_bindings(&scope, &source, true).unwrap();
        assert_eq!(
            contracts.get("session-membership").unwrap().driver(),
            AuthorizedObservationSourceDriver::DirectoryMembership {
                max_entries: 100_000,
                max_depth: 8,
            }
        );

        source.streams[0]
            .lifecycle
            .retain(|value| value != "identity_change");
        assert!(validate_scope_source_bindings(&scope, &source, true)
            .unwrap_err()
            .to_string()
            .contains("no closed common-driver contract"));
    }

    #[test]
    fn observation_binding_is_checked_against_the_digest_bound_scoped_stream() {
        let scope = ScopeProgramManifest::from_json(br#"{
          "schema_version": 1,
          "declaration_id": "fixture-scope",
          "adapter_id": "fixture",
          "ads_id": "fixture-ads",
          "status": "promoted",
          "roots": ["root"],
          "programs": [{
            "program_id": "observe-session",
            "root_entity_kind": "session",
            "root_relation_id": "root-object",
            "relations": [
              {
                "relation_id": "root-object",
                "primitive": "KnownObject",
                "access_root": "root",
                "locator": "known-object",
                "identity_inputs": ["native-session-id"],
                "bounds": {"max_fan_out": 1, "max_depth": 1, "max_objects": 1, "max_bytes": 1024, "max_rows": 0},
                "observation_binding": {"stream_id": "root-stream", "source_pattern": "sessions/*.jsonl"},
                "unavailable_behavior": "record_unavailable",
                "claim_refs": ["scope-evidence"]
              },
              {
                "relation_id": "descendants",
                "primitive": "ChildDirectoryByNativeId",
                "access_root": "root",
                "locator": "sessions/{native-session-id}/children",
                "identity_inputs": ["native-session-id"],
                "bounds": {"max_fan_out": 8, "max_depth": 4, "max_objects": 8, "max_bytes": 8192, "max_rows": 0},
                "observation_binding": {"stream_id": "descendant-stream", "source_pattern": "sessions/*/children/**/entry-*.jsonl", "relative_selector": "**/entry-*.jsonl"},
                "unavailable_behavior": "record_unavailable",
                "claim_refs": ["scope-evidence"]
              }
            ],
            "claim_refs": ["scope-evidence"]
          }],
          "blockers": [],
          "claim_refs": ["scope-evidence"]
        }"#)
        .unwrap();
        let mut source: SupportSourceDeclarationWire = serde_json::from_value(serde_json::json!({
            "streams": [
                {
                    "stream_id": "root-stream",
                    "root_id": "root",
                    "relative_patterns": ["sessions/*.jsonl"],
                    "decoder_id": "fixture-root",
                    "authority": "canonical",
                    "primitive": "AppendDelimited",
                    "topologies": ["scoped"],
                    "implementation_state": "existing",
                    "bounds": {"max_record_bytes": 512, "max_batch_bytes": 1024, "max_records_per_batch": 16},
                    "lifecycle": ["append", "partial_write", "truncate", "identity_change", "delete", "recreate"],
                    "safe_decoder_state_boundary": "object_generation_cursor"
                },
                {
                    "stream_id": "descendant-stream",
                    "root_id": "root",
                    "relative_patterns": ["sessions/*/children/**/entry-*.jsonl"],
                    "decoder_id": "fixture-descendant",
                    "authority": "canonical",
                    "primitive": "AppendDelimited",
                    "topologies": ["durable", "scoped"],
                    "implementation_state": "existing",
                    "bounds": {"max_record_bytes": 4096, "max_batch_bytes": 8192, "max_records_per_batch": 64},
                    "lifecycle": ["append", "partial_write", "truncate", "identity_change", "delete", "recreate"],
                    "safe_decoder_state_boundary": "object_generation_cursor"
                }
            ]
        }))
        .unwrap();
        let contracts = validate_scope_source_bindings(&scope, &source, false).unwrap();
        assert_eq!(
            contracts.get("root-stream").unwrap().driver(),
            AuthorizedObservationSourceDriver::AppendDelimited {
                max_record_bytes: 512,
                max_batch_bytes: 1024,
                max_records_per_batch: 16,
            }
        );
        assert_eq!(
            contracts.get("descendant-stream").unwrap().driver(),
            AuthorizedObservationSourceDriver::AppendDelimited {
                max_record_bytes: 4096,
                max_batch_bytes: 8192,
                max_records_per_batch: 64,
            }
        );
        let contract = contracts.get("descendant-stream").unwrap();
        assert_eq!(
            contract.relative_patterns(),
            ["sessions/*/children/**/entry-*.jsonl"]
        );
        assert_eq!(contract.decoder_id(), "fixture-descendant");
        assert_eq!(
            contract.authority(),
            AuthorizedObservationSourceAuthority::Canonical
        );

        source.streams[1].relative_patterns[0] = "sessions/*/other/**".to_string();
        assert!(validate_scope_source_bindings(&scope, &source, false).is_err());
        source.streams[1].relative_patterns[0] = "sessions/*/children/**/entry-*.jsonl".to_string();
        source.streams[1]
            .relative_patterns
            .push("sessions/*/children/**/entry-*.jsonl".to_string());
        assert!(validate_scope_source_bindings(&scope, &source, false).is_err());
        source.streams[1].relative_patterns.pop();
        source.streams[1].decoder_id = None;
        assert!(validate_scope_source_bindings(&scope, &source, false).is_err());
        source.streams[1].decoder_id = Some("fixture-descendant".to_string());
        source.streams[1].authority = Some("private".to_string());
        assert!(validate_scope_source_bindings(&scope, &source, false).is_err());
        source.streams[1].authority = Some("canonical".to_string());
        source.streams[1].bounds.max_records_per_batch = None;
        assert!(validate_scope_source_bindings(&scope, &source, false).is_err());
        source.streams[1].bounds.max_records_per_batch = Some(64);
        source.streams[1].bounds.max_record_bytes = Some(16_384);
        assert!(validate_scope_source_bindings(&scope, &source, false).is_err());
        source.streams[1].bounds.max_record_bytes = Some(4096);
        source.streams[1].safe_decoder_state_boundary = "object_generation_revision".to_string();
        assert!(validate_scope_source_bindings(&scope, &source, false).is_err());

        source.streams[1].primitive = "PresenceObject".to_string();
        source.streams[1].bounds.max_object_bytes = Some(1024);
        source.streams[1].bounds.max_record_bytes = None;
        source.streams[1].bounds.max_batch_bytes = None;
        source.streams[1].lifecycle = vec![
            "replace".to_string(),
            "delete".to_string(),
            "recreate".to_string(),
        ];
        let contracts = validate_scope_source_bindings(&scope, &source, false).unwrap();
        assert_eq!(
            contracts.get("descendant-stream").unwrap().driver(),
            AuthorizedObservationSourceDriver::PresenceObject {
                max_object_bytes: 1024,
            }
        );
        source.streams[1].primitive = "DirectoryMembership".to_string();
        assert!(validate_scope_source_bindings(&scope, &source, false).is_err());
    }

    #[test]
    fn invalid_and_ambiguous_release_contracts_fail_closed() {
        let fixture = fixture();
        let exact = fixture
            .runtime_cases
            .iter()
            .find(|case| case.name == "exact")
            .unwrap();
        let mut duplicated = fixture.releases.clone();
        duplicated.push(duplicated[1].clone());
        assert!(classify_runtime_support(&exact.probe, &duplicated).is_err());

        let mut overlapping = fixture.releases.clone();
        let mut second = overlapping[1].clone();
        second.support_release_id = "fixture-support-v2".to_string();
        overlapping.push(second);
        let decision = classify_runtime_support(&exact.probe, &overlapping).unwrap();
        assert_eq!(
            decision.reason,
            CompatibilityReason::AmbiguousPromotedRelease
        );
        assert!(!decision.permissions.durable);

        let mut duplicated_capability = fixture.releases[1].clone();
        duplicated_capability
            .capabilities
            .push(duplicated_capability.capabilities[0].clone());
        assert!(
            classify_runtime_support(&exact.probe, &[duplicated_capability])
                .unwrap_err()
                .to_string()
                .contains("duplicate support capability")
        );

        let mut oversized_capability = fixture.releases[1].clone();
        oversized_capability.capabilities[0].capability_id = "é".repeat(65);
        assert!(
            classify_runtime_support(&exact.probe, &[oversized_capability])
                .unwrap_err()
                .to_string()
                .contains("exceeds 128 bytes")
        );

        let mut absent_capabilities = fixture.releases[1].clone();
        absent_capabilities.capabilities.clear();
        assert!(
            classify_runtime_support(&exact.probe, &[absent_capabilities])
                .unwrap_err()
                .to_string()
                .contains("must not be empty")
        );

        let restricted = fixture
            .runtime_cases
            .iter()
            .find(|case| case.name == "exact-capability-restricted")
            .unwrap();
        let mut absent_topologies = fixture.releases[2].clone();
        absent_topologies
            .capabilities
            .retain(|capability| capability.capability_id == "restricted-history");
        let decision = classify_runtime_support(&restricted.probe, &[absent_topologies]).unwrap();
        assert!(decision.permissions.durable);
        assert!(!decision.permissions.catalog);
        assert!(!decision.permissions.scoped_observation);
    }

    #[test]
    fn serialized_decisions_do_not_expose_authorization_tokens() {
        let fixture = fixture();
        let exact = fixture
            .runtime_cases
            .iter()
            .find(|case| case.name == "exact")
            .unwrap();
        let decision = classify_runtime_support(&exact.probe, &fixture.releases).unwrap();
        let value = serde_json::to_value(decision).unwrap();
        let Value::Object(fields) = value else {
            panic!("decision must serialize as an object")
        };
        assert!(!fields.contains_key("authorization"));
    }

    fn fixture_access_policy_digest() -> Sha256Digest {
        Sha256Digest::of(b"fixture-access-policy-v1")
    }

    fn recompute_probe_grant_digest(value: serde_json::Value) -> serde_json::Value {
        let mut wire: NativeProbeGrantRequestWire = serde_json::from_value(value).unwrap();
        wire.digest = wire.calculate_digest();
        serde_json::to_value(wire).unwrap()
    }

    fn exact_bound_markers() -> Vec<String> {
        let mut markers = vec!["native.marker".to_string()];
        markers.extend((1..MAX_ACCESS_REQUEST_MARKERS).map(|index| format!("marker-{index:02}")));
        markers
    }

    fn one_over_markers() -> Vec<String> {
        let mut markers = exact_bound_markers();
        markers.push(format!("marker-{MAX_ACCESS_REQUEST_MARKERS:02}"));
        markers
    }

    fn exact_bound_families() -> serde_json::Map<String, Value> {
        (0..MAX_ACCESS_REQUEST_FACT_FAMILIES)
            .map(|index| (format!("family-{index:02}"), serde_json::json!(1)))
            .collect()
    }

    fn one_over_families() -> serde_json::Map<String, Value> {
        (0..=MAX_ACCESS_REQUEST_FACT_FAMILIES)
            .map(|index| (format!("family-{index:02}"), serde_json::json!(1)))
            .collect()
    }

    fn padded_machine_id(prefix: &str, len: usize) -> String {
        let mut identifier = prefix.to_string();
        identifier.extend(std::iter::repeat_n('a', len.saturating_sub(prefix.len())));
        identifier
    }

    fn exact_bound_grants() -> Vec<Value> {
        (0..MAX_ACCESS_REQUEST_GRANTS)
            .map(|index| {
                serde_json::json!({
                    "relation_id": padded_machine_id(&format!("g{index:03}"), 128),
                    "scope_root": index == 0,
                    "access_root": padded_machine_id("r", 127),
                    "identity_input_names": ["x"],
                })
            })
            .collect()
    }

    fn one_over_grants() -> Vec<Value> {
        let mut grants = exact_bound_grants();
        grants[MAX_ACCESS_REQUEST_GRANTS - 1]["identity_input_names"] = serde_json::json!(["xx"]);
        grants
    }

    fn fixture_report_digest() -> Sha256Digest {
        Sha256Digest::parse(
            "sha256:88bb0c1f93d06df1af8a8740b73aae7a94a10daf142b8f6f9b0d21ca01972e43",
        )
        .unwrap()
    }

    fn authorize_fixture(
        operation: SupportOperation,
    ) -> (NativeArtifactProbe, TypedAccessAuthorization) {
        let fixture = fixture();
        let exact = fixture
            .runtime_cases
            .iter()
            .find(|case| case.name == "exact")
            .unwrap();
        let catalog = fixture_catalog(&fixture.releases);
        let binding = fixture_binding("fixture-support-v1");
        let scope_programs =
            fixture_scope_manifest("fixture-agent", SupportReleaseStatus::Promoted);
        let (_, authorization) = catalog
            .authorize_typed_access(
                AdapterSupportRegistration::new("fixture-agent", &binding, &scope_programs),
                &exact.probe,
                operation,
                &fixture.contract_request,
                &fixture.contract_offer,
            )
            .unwrap();
        (exact.probe.clone(), authorization)
    }

    #[test]
    fn typed_authorization_debug_redacts_scope_locators() {
        let (_, authorization) = authorize_fixture(SupportOperation::ScopedTypedObservation);
        let rendered = format!("{authorization:?}");
        assert!(rendered.contains("TypedAccessAuthorization"));
        assert!(!rendered.contains("known-object"));
        assert!(!rendered.contains("native-session-id"));
        assert!(!rendered.contains("1.2.3"));
        assert!(!rendered.contains("native.marker"));
        assert!(!rendered.contains("darwin"));
        assert!(!rendered.contains("probe_family"));
        assert!(!rendered.contains("probe_platform"));
        let program = authorization
            .select_scope_program("observe-session")
            .unwrap();
        let rendered = format!("{program:?}");
        assert!(rendered.contains("observe-session"));
        assert!(!rendered.contains("known-object"));
    }

    #[test]
    fn scoped_probe_grant_and_retrieval_bind_authorization_and_report_digest() {
        let (probe, authorization) = authorize_fixture(SupportOperation::ScopedTypedObservation);
        let request = authorization
            .native_probe_grant_request(fixture_access_policy_digest(), Some("observe-session"))
            .unwrap();
        assert!(request.matches_authorization(&authorization));
        let wire = request.project();
        assert_eq!(wire.probe, NativeArtifactProbeWire::from_probe(&probe));
        assert_eq!(wire.grants.len(), 1);
        assert_eq!(wire.grants[0].relation_id, "root-object");
        assert!(wire.grants[0].scope_root);
        assert_eq!(wire.grants[0].access_root, "root");
        assert_eq!(
            wire.grants[0].identity_input_names,
            vec!["native-session-id".to_string()]
        );
        let encoded = serde_json::to_string(&wire).unwrap();
        assert!(!encoded.contains('\\'));
        assert!(!encoded.contains("/etc/"));
        assert!(!encoded.contains("secret"));
        let parsed =
            NativeProbeGrantRequestWire::parse(serde_json::to_value(&wire).unwrap()).unwrap();
        assert_eq!(parsed.digest, wire.digest);
        // Portable parse cannot construct the private request type.

        let retrieval = authorization
            .access_report_retrieval(
                "observe-session",
                fixture_access_policy_digest(),
                fixture_report_digest(),
            )
            .unwrap();
        assert!(retrieval.matches_authorization(&authorization));
        assert!(retrieval.matches_report_digest(fixture_report_digest()));
        assert!(!retrieval.matches_report_digest(Sha256Digest::of(b"other-report")));
        let retrieval_wire = retrieval.project();
        let parsed_retrieval =
            ScopeAccessReportRetrievalWire::parse(serde_json::to_value(&retrieval_wire).unwrap())
                .unwrap();
        assert!(parsed_retrieval.matches_report_digest(fixture_report_digest().as_bytes()));
        assert!(
            !parsed_retrieval.matches_report_digest(Sha256Digest::of(b"other-report").as_bytes())
        );
    }

    #[test]
    fn catalog_and_durable_probe_requests_have_no_grants() {
        let (_, catalog_authorization) = authorize_fixture(SupportOperation::CatalogDiscovery);
        let catalog_request = catalog_authorization
            .native_probe_grant_request(fixture_access_policy_digest(), None)
            .unwrap();
        assert!(catalog_request.matches_authorization(&catalog_authorization));
        assert!(catalog_request.project().grants.is_empty());
        assert!(catalog_request.project().program_id.is_empty());
        assert!(catalog_authorization
            .native_probe_grant_request(fixture_access_policy_digest(), Some("observe-session"),)
            .is_err());
        assert!(catalog_authorization
            .access_report_retrieval(
                "observe-session",
                fixture_access_policy_digest(),
                fixture_report_digest(),
            )
            .is_err());

        let (_, durable_authorization) = authorize_fixture(SupportOperation::DurableHistoryRuntime);
        let durable_request = durable_authorization
            .native_probe_grant_request(fixture_access_policy_digest(), None)
            .unwrap();
        assert!(durable_request.project().grants.is_empty());
        assert!(!durable_request.matches_authorization(&catalog_authorization));
        assert!(!catalog_request.matches_authorization(&durable_authorization));
    }

    #[test]
    fn minted_request_binds_the_classify_time_probe() {
        let fixture = fixture();
        let catalog = fixture_catalog(&fixture.releases);
        let binding = fixture_binding("fixture-support-v1");
        let scope_programs =
            fixture_scope_manifest("fixture-agent", SupportReleaseStatus::Promoted);
        let exact = fixture
            .runtime_cases
            .iter()
            .find(|case| case.name == "exact")
            .unwrap();
        let range = fixture
            .runtime_cases
            .iter()
            .find(|case| case.name == "range")
            .unwrap();
        let registration =
            AdapterSupportRegistration::new("fixture-agent", &binding, &scope_programs);
        let (_, exact_authorization) = catalog
            .authorize_typed_access(
                registration,
                &exact.probe,
                SupportOperation::ScopedTypedObservation,
                &fixture.contract_request,
                &fixture.contract_offer,
            )
            .unwrap();
        let (_, range_authorization) = catalog
            .authorize_typed_access(
                registration,
                &range.probe,
                SupportOperation::ScopedTypedObservation,
                &fixture.contract_request,
                &fixture.contract_offer,
            )
            .unwrap();
        let exact_request = exact_authorization
            .native_probe_grant_request(fixture_access_policy_digest(), Some("observe-session"))
            .unwrap();
        let range_request = range_authorization
            .native_probe_grant_request(fixture_access_policy_digest(), Some("observe-session"))
            .unwrap();
        assert_eq!(
            exact_request.project().probe,
            NativeArtifactProbeWire::from_probe(&exact.probe)
        );
        assert_eq!(
            range_request.project().probe,
            NativeArtifactProbeWire::from_probe(&range.probe)
        );
        assert_ne!(exact.probe, range.probe);
        assert!(exact_request.matches_authorization(&exact_authorization));
        assert!(!exact_request.matches_authorization(&range_authorization));
        assert!(!range_request.matches_authorization(&exact_authorization));
    }

    #[test]
    fn candidate_and_restricted_authorizations_cannot_mint_requests() {
        let fixture = fixture();
        let catalog = fixture_catalog(&fixture.releases);
        let candidate = fixture
            .runtime_cases
            .iter()
            .find(|case| case.name == "candidate-only-family")
            .unwrap();
        let candidate_binding = fixture_binding("candidate-only-support-v1");
        let candidate_scope =
            fixture_scope_manifest("candidate-agent", SupportReleaseStatus::Candidate);
        assert!(catalog
            .authorize_typed_access(
                AdapterSupportRegistration::new(
                    "candidate-agent",
                    &candidate_binding,
                    &candidate_scope,
                ),
                &candidate.probe,
                SupportOperation::ScopedTypedObservation,
                &fixture.contract_request,
                &fixture.contract_offer,
            )
            .is_err());

        let restricted = fixture
            .runtime_cases
            .iter()
            .find(|case| case.name == "exact-capability-restricted")
            .unwrap();
        let restricted_binding = fixture_binding("capability-restricted-support-v1");
        let restricted_scope = fixture_scope_manifest(
            "capability-restricted-agent",
            SupportReleaseStatus::Promoted,
        );
        assert!(catalog
            .authorize_typed_access(
                AdapterSupportRegistration::new(
                    "capability-restricted-agent",
                    &restricted_binding,
                    &restricted_scope,
                ),
                &restricted.probe,
                SupportOperation::ScopedTypedObservation,
                &fixture.contract_request,
                &fixture.contract_offer,
            )
            .unwrap_err()
            .to_string()
            .contains("forbidden"));
    }

    #[test]
    fn probe_grant_and_retrieval_reject_zero_policy_wrong_program_and_replay() {
        let (_, authorization) = authorize_fixture(SupportOperation::ScopedTypedObservation);
        assert!(authorization
            .native_probe_grant_request(Sha256Digest::from_bytes([0; 32]), Some("observe-session"),)
            .unwrap_err()
            .to_string()
            .contains("nonzero"));
        assert!(authorization
            .native_probe_grant_request(fixture_access_policy_digest(), Some("unknown"))
            .is_err());
        assert!(authorization
            .native_probe_grant_request(fixture_access_policy_digest(), None)
            .is_err());
        assert!(authorization
            .access_report_retrieval(
                "observe-session",
                Sha256Digest::from_bytes([0; 32]),
                fixture_report_digest(),
            )
            .is_err());
        assert!(authorization
            .access_report_retrieval(
                "observe-session",
                fixture_access_policy_digest(),
                Sha256Digest::from_bytes([0; 32]),
            )
            .is_err());

        let request = authorization
            .native_probe_grant_request(fixture_access_policy_digest(), Some("observe-session"))
            .unwrap();
        let mut tampered = serde_json::to_value(request.project()).unwrap();
        tampered["access_policy_digest"][0] =
            serde_json::json!(tampered["access_policy_digest"][0].as_u64().unwrap() ^ 1);
        assert!(NativeProbeGrantRequestWire::parse(tampered).is_err());

        let mut extra_field = serde_json::to_value(request.project()).unwrap();
        extra_field["/Users/alice/private/session.jsonl"] = serde_json::json!(true);
        let extra_error = NativeProbeGrantRequestWire::parse(extra_field)
            .unwrap_err()
            .to_string();
        assert!(extra_error.contains("unknown field"));
        assert!(!extra_error.contains("/Users/alice"));
        assert!(!extra_error.contains("session.jsonl"));
        let mut wrong_typed_path = serde_json::to_value(request.project()).unwrap();
        wrong_typed_path["probe"]["version"] = serde_json::json!(["/tmp/secret"]);
        let typed_error = NativeProbeGrantRequestWire::parse(wrong_typed_path)
            .unwrap_err()
            .to_string();
        assert!(!typed_error.contains("/tmp/secret"));
        assert!(!typed_error.contains("secret"));

        let mut extra_grant = serde_json::to_value(request.project()).unwrap();
        extra_grant["grants"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "relation_id": "sibling-object",
                "scope_root": false,
                "access_root": "root",
                "identity_input_names": ["native-session-id"]
            }));
        assert!(NativeProbeGrantRequestWire::parse(extra_grant).is_err());

        let mut path_marker = serde_json::to_value(request.project()).unwrap();
        path_marker["probe"]["markers"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!("/tmp/secret"));
        let path_error =
            NativeProbeGrantRequestWire::parse(recompute_probe_grant_digest(path_marker))
                .unwrap_err()
                .to_string();
        assert!(path_error.contains("machine identifier"));
        assert!(!path_error.contains("/tmp/secret"));
        let mut nul_marker = serde_json::to_value(request.project()).unwrap();
        nul_marker["probe"]["markers"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!("\u{0000}secret"));
        let nul_error =
            NativeProbeGrantRequestWire::parse(recompute_probe_grant_digest(nul_marker))
                .unwrap_err()
                .to_string();
        assert!(nul_error.contains("machine identifier"));
        assert!(!nul_error.contains('\0'));
        assert!(!nul_error.contains("secret"));
        let mut unicode_version = serde_json::to_value(request.project()).unwrap();
        unicode_version["probe"]["version"] = serde_json::json!("é".repeat(80));
        let unicode_error =
            NativeProbeGrantRequestWire::parse(recompute_probe_grant_digest(unicode_version))
                .unwrap_err()
                .to_string();
        assert!(unicode_error.contains("machine identifier"));
        assert!(!unicode_error.contains('é'));
        let mut oversized_families = serde_json::to_value(request.project()).unwrap();
        let families = (0..5_000)
            .map(|index| (format!("family-{index}"), serde_json::json!(1)))
            .collect::<serde_json::Map<_, _>>();
        oversized_families["selection"]["fact_family_versions"] =
            serde_json::Value::Object(families);
        assert!(
            NativeProbeGrantRequestWire::parse(recompute_probe_grant_digest(oversized_families))
                .is_err()
        );
        let mut catalog_without_pack = serde_json::to_value(
            authorize_fixture(SupportOperation::CatalogDiscovery)
                .1
                .native_probe_grant_request(fixture_access_policy_digest(), None)
                .unwrap()
                .project(),
        )
        .unwrap();
        catalog_without_pack["selection"]["query_pack_version"] = serde_json::Value::Null;
        assert!(
            NativeProbeGrantRequestWire::parse(recompute_probe_grant_digest(catalog_without_pack))
                .is_err()
        );
        let mut missing_version = serde_json::to_value(request.project()).unwrap();
        missing_version["probe"]
            .as_object_mut()
            .unwrap()
            .remove("version");
        assert!(NativeProbeGrantRequestWire::parse(missing_version).is_err());
        let mut missing_query_pack = serde_json::to_value(
            authorize_fixture(SupportOperation::DurableHistoryRuntime)
                .1
                .native_probe_grant_request(fixture_access_policy_digest(), None)
                .unwrap()
                .project(),
        )
        .unwrap();
        missing_query_pack["selection"]
            .as_object_mut()
            .unwrap()
            .remove("query_pack_version");
        assert!(NativeProbeGrantRequestWire::parse(missing_query_pack).is_err());
        let mut missing_observation = serde_json::to_value(request.project()).unwrap();
        missing_observation["selection"]
            .as_object_mut()
            .unwrap()
            .remove("observation_contract_version");
        assert!(NativeProbeGrantRequestWire::parse(missing_observation).is_err());
        let mut exact_markers = serde_json::to_value(request.project()).unwrap();
        exact_markers["probe"]["markers"] = serde_json::json!(exact_bound_markers());
        NativeProbeGrantRequestWire::parse(recompute_probe_grant_digest(exact_markers)).unwrap();
        let mut over_marker_request = serde_json::to_value(request.project()).unwrap();
        over_marker_request["probe"]["markers"] = serde_json::json!(one_over_markers());
        assert!(
            NativeProbeGrantRequestWire::parse(recompute_probe_grant_digest(over_marker_request))
                .is_err()
        );
        let mut exact_families = serde_json::to_value(request.project()).unwrap();
        exact_families["selection"]["fact_family_versions"] =
            serde_json::Value::Object(exact_bound_families());
        NativeProbeGrantRequestWire::parse(recompute_probe_grant_digest(exact_families)).unwrap();
        let mut over_family_request = serde_json::to_value(request.project()).unwrap();
        over_family_request["selection"]["fact_family_versions"] =
            serde_json::Value::Object(one_over_families());
        assert!(
            NativeProbeGrantRequestWire::parse(recompute_probe_grant_digest(over_family_request))
                .is_err()
        );
        let mut one_over_identifier = serde_json::to_value(request.project()).unwrap();
        one_over_identifier["probe"]["version"] = serde_json::json!("a".repeat(129));
        assert!(
            NativeProbeGrantRequestWire::parse(recompute_probe_grant_digest(one_over_identifier))
                .is_err()
        );
        let mut oversized_program = serde_json::to_value(request.project()).unwrap();
        oversized_program["program_id"] =
            serde_json::json!("a".repeat(MAX_IDENTIFIER_BYTES + 4096));
        assert!(NativeProbeGrantRequestWire::parse(oversized_program).is_err());
        let mut oversized_retrieval_program = serde_json::to_value(
            authorization
                .access_report_retrieval(
                    "observe-session",
                    fixture_access_policy_digest(),
                    fixture_report_digest(),
                )
                .unwrap()
                .project(),
        )
        .unwrap();
        oversized_retrieval_program["program_id"] =
            serde_json::json!("a".repeat(MAX_IDENTIFIER_BYTES + 4096));
        assert!(ScopeAccessReportRetrievalWire::parse(oversized_retrieval_program).is_err());
        let mut exact_grants = serde_json::to_value(request.project()).unwrap();
        exact_grants["grants"] = serde_json::json!(exact_bound_grants());
        NativeProbeGrantRequestWire::parse(recompute_probe_grant_digest(exact_grants)).unwrap();
        let mut over_grants = serde_json::to_value(request.project()).unwrap();
        over_grants["grants"] = serde_json::json!(one_over_grants());
        assert!(NativeProbeGrantRequestWire::parse(over_grants).is_err());
        let mut malformed_digest = serde_json::to_value(request.project()).unwrap();
        malformed_digest["digest"] = serde_json::json!("/tmp/secret");
        let digest_error = NativeProbeGrantRequestWire::parse(malformed_digest)
            .unwrap_err()
            .to_string();
        assert!(!digest_error.contains("/tmp/secret"));
        assert!(!digest_error.contains("secret"));
        let mut malformed_policy = serde_json::to_value(request.project()).unwrap();
        malformed_policy["access_policy_digest"] = serde_json::json!("/tmp/secret");
        let policy_error = NativeProbeGrantRequestWire::parse(malformed_policy)
            .unwrap_err()
            .to_string();
        assert!(!policy_error.contains("/tmp/secret"));
        assert!(!policy_error.contains("secret"));

        let mut marker_order_a = serde_json::to_value(request.project()).unwrap();
        marker_order_a["probe"]["markers"] = serde_json::json!(["native.marker", "extra.marker"]);
        let mut marker_order_b = serde_json::to_value(request.project()).unwrap();
        marker_order_b["probe"]["markers"] = serde_json::json!(["extra.marker", "native.marker"]);
        let digest_a = recompute_probe_grant_digest(marker_order_a.clone());
        let digest_b = recompute_probe_grant_digest(marker_order_b);
        assert_eq!(digest_a["digest"], digest_b["digest"]);
        let parsed_markers = NativeProbeGrantRequestWire::parse(digest_a).unwrap();
        assert_eq!(
            parsed_markers.probe.markers,
            vec!["extra.marker".to_string(), "native.marker".to_string()]
        );

        let mut zero_release = serde_json::to_value(request.project()).unwrap();
        zero_release["support_release_digest"] = serde_json::json!(vec![0u8; 32]);
        assert!(NativeProbeGrantRequestWire::parse(zero_release).is_err());

        let mut selection_drift = serde_json::to_value(request.project()).unwrap();
        selection_drift["selection"]["model_major"] = serde_json::json!(2);
        assert!(NativeProbeGrantRequestWire::parse(selection_drift).is_err());

        let retrieval = authorization
            .access_report_retrieval(
                "observe-session",
                fixture_access_policy_digest(),
                fixture_report_digest(),
            )
            .unwrap();
        let mut retrieval_replay = serde_json::to_value(retrieval.project()).unwrap();
        retrieval_replay["expected_report_digest"] =
            serde_json::json!(Sha256Digest::of(b"other-report").as_bytes().to_vec());
        assert!(ScopeAccessReportRetrievalWire::parse(retrieval_replay).is_err());
        let (_, durable) = authorize_fixture(SupportOperation::DurableHistoryRuntime);
        assert!(!retrieval.matches_authorization(&durable));
    }

    #[test]
    fn authorization_denies_candidate_before_probe_bounds() {
        let fixture = fixture();
        let catalog = fixture_catalog(&fixture.releases);
        let candidate = fixture
            .runtime_cases
            .iter()
            .find(|case| case.name == "candidate-only-family")
            .unwrap();
        let mut oversized = candidate.probe.clone();
        oversized.markers.extend(one_over_markers());
        let candidate_binding = fixture_binding("candidate-only-support-v1");
        let candidate_scope =
            fixture_scope_manifest("candidate-agent", SupportReleaseStatus::Candidate);
        let error = catalog
            .authorize_typed_access(
                AdapterSupportRegistration::new(
                    "candidate-agent",
                    &candidate_binding,
                    &candidate_scope,
                ),
                &oversized,
                SupportOperation::ScopedTypedObservation,
                &fixture.contract_request,
                &fixture.contract_offer,
            )
            .unwrap_err()
            .to_string();
        assert!(error.contains("forbidden"));
        assert!(!error.contains("marker"));
    }

    #[test]
    fn authorization_bounds_probe_before_retention() {
        let fixture = fixture();
        let catalog = fixture_catalog(&fixture.releases);
        let exact = fixture
            .runtime_cases
            .iter()
            .find(|case| case.name == "exact")
            .unwrap();
        let binding = fixture_binding("fixture-support-v1");
        let scope_programs =
            fixture_scope_manifest("fixture-agent", SupportReleaseStatus::Promoted);
        let mut bounded = exact.probe.clone();
        bounded.markers = exact_bound_markers();
        catalog
            .authorize_typed_access(
                AdapterSupportRegistration::new("fixture-agent", &binding, &scope_programs),
                &bounded,
                SupportOperation::ScopedTypedObservation,
                &fixture.contract_request,
                &fixture.contract_offer,
            )
            .unwrap();
        let mut oversized = exact.probe.clone();
        oversized.markers = one_over_markers();
        let error = catalog
            .authorize_typed_access(
                AdapterSupportRegistration::new("fixture-agent", &binding, &scope_programs),
                &oversized,
                SupportOperation::ScopedTypedObservation,
                &fixture.contract_request,
                &fixture.contract_offer,
            )
            .unwrap_err()
            .to_string();
        assert!(error.contains("marker collection limit"));
    }

    #[test]
    fn minted_projections_roundtrip_portable_parse() {
        for operation in [
            SupportOperation::ScopedTypedObservation,
            SupportOperation::CatalogDiscovery,
            SupportOperation::DurableHistoryRuntime,
        ] {
            let (_, authorization) = authorize_fixture(operation);
            let program_id = (operation == SupportOperation::ScopedTypedObservation)
                .then_some("observe-session");
            let request = authorization
                .native_probe_grant_request(fixture_access_policy_digest(), program_id)
                .unwrap();
            let projected = serde_json::to_value(request.project()).unwrap();
            NativeProbeGrantRequestWire::parse(projected).unwrap();
        }
        let (_, scoped) = authorize_fixture(SupportOperation::ScopedTypedObservation);
        let retrieval = scoped
            .access_report_retrieval(
                "observe-session",
                fixture_access_policy_digest(),
                fixture_report_digest(),
            )
            .unwrap();
        ScopeAccessReportRetrievalWire::parse(serde_json::to_value(retrieval.project()).unwrap())
            .unwrap();
    }

    #[test]
    fn mint_rejects_support_valid_request_invalid_coordinates() {
        let fixture = fixture();
        let exact = fixture
            .runtime_cases
            .iter()
            .find(|case| case.name == "exact")
            .unwrap();
        let mut releases = fixture.releases.clone();
        for release in &mut releases {
            if release.support_release_id == "fixture-support-v1" {
                release.support_release_id = "Fixture-Support-v1".to_string();
            }
        }
        let catalog = fixture_catalog(&releases);
        let binding = fixture_binding("Fixture-Support-v1");
        let scope_programs =
            fixture_scope_manifest("fixture-agent", SupportReleaseStatus::Promoted);
        let (_, authorization) = catalog
            .authorize_typed_access(
                AdapterSupportRegistration::new("fixture-agent", &binding, &scope_programs),
                &exact.probe,
                SupportOperation::ScopedTypedObservation,
                &fixture.contract_request,
                &fixture.contract_offer,
            )
            .unwrap();
        let error = authorization
            .native_probe_grant_request(fixture_access_policy_digest(), Some("observe-session"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("machine identifier"));
        assert!(!error.contains("Fixture-Support-v1"));
        let retrieval_error = authorization
            .access_report_retrieval(
                "observe-session",
                fixture_access_policy_digest(),
                fixture_report_digest(),
            )
            .unwrap_err()
            .to_string();
        assert!(retrieval_error.contains("machine identifier"));
        assert!(!retrieval_error.contains("Fixture-Support-v1"));
    }

    #[test]
    #[ignore = "emit helper for freezing rfc012a-access-request-v1.json"]
    fn emit_access_request_fixture() {
        let (_, scoped) = authorize_fixture(SupportOperation::ScopedTypedObservation);
        let request = scoped
            .native_probe_grant_request(fixture_access_policy_digest(), Some("observe-session"))
            .unwrap();
        let (_, catalog) = authorize_fixture(SupportOperation::CatalogDiscovery);
        let catalog_request = catalog
            .native_probe_grant_request(fixture_access_policy_digest(), None)
            .unwrap();
        let (_, durable) = authorize_fixture(SupportOperation::DurableHistoryRuntime);
        let durable_request = durable
            .native_probe_grant_request(fixture_access_policy_digest(), None)
            .unwrap();
        let retrieval = scoped
            .access_report_retrieval(
                "observe-session",
                fixture_access_policy_digest(),
                fixture_report_digest(),
            )
            .unwrap();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "fixture_contract_version": 1,
                "probe_grant_request": request.project(),
                "catalog_probe_grant_request": catalog_request.project(),
                "durable_probe_grant_request": durable_request.project(),
                "retrieval_request": retrieval.project(),
                "expected_probe_grant_digest": request.digest.to_string(),
                "expected_durable_probe_grant_digest": durable_request.digest.to_string(),
                "expected_retrieval_digest": retrieval.digest.to_string(),
            }))
            .unwrap()
        );
    }

    #[test]
    fn shared_access_request_fixture_matches_rust_authority_projection() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../fixtures/contracts/rfc012a-access-request-v1.json"
        ))
        .unwrap();
        assert_eq!(fixture["fixture_contract_version"], 1);
        let (_, scoped) = authorize_fixture(SupportOperation::ScopedTypedObservation);
        let request = scoped
            .native_probe_grant_request(fixture_access_policy_digest(), Some("observe-session"))
            .unwrap();
        let expected_probe =
            NativeProbeGrantRequestWire::parse(fixture["probe_grant_request"].clone()).unwrap();
        assert_eq!(request.project(), expected_probe);
        assert_eq!(
            format!("{}", request.digest),
            fixture["expected_probe_grant_digest"].as_str().unwrap()
        );

        let (_, catalog) = authorize_fixture(SupportOperation::CatalogDiscovery);
        let catalog_request = catalog
            .native_probe_grant_request(fixture_access_policy_digest(), None)
            .unwrap();
        let expected_catalog =
            NativeProbeGrantRequestWire::parse(fixture["catalog_probe_grant_request"].clone())
                .unwrap();
        assert_eq!(catalog_request.project(), expected_catalog);

        let (_, durable) = authorize_fixture(SupportOperation::DurableHistoryRuntime);
        let durable_request = durable
            .native_probe_grant_request(fixture_access_policy_digest(), None)
            .unwrap();
        let expected_durable =
            NativeProbeGrantRequestWire::parse(fixture["durable_probe_grant_request"].clone())
                .unwrap();
        assert_eq!(durable_request.project(), expected_durable);
        assert_eq!(
            format!("{}", durable_request.digest),
            fixture["expected_durable_probe_grant_digest"]
                .as_str()
                .unwrap()
        );

        let retrieval = scoped
            .access_report_retrieval(
                "observe-session",
                fixture_access_policy_digest(),
                fixture_report_digest(),
            )
            .unwrap();
        let expected_retrieval =
            ScopeAccessReportRetrievalWire::parse(fixture["retrieval_request"].clone()).unwrap();
        assert_eq!(retrieval.project(), expected_retrieval);
        assert_eq!(
            format!("{}", retrieval.digest),
            fixture["expected_retrieval_digest"].as_str().unwrap()
        );
    }
}
