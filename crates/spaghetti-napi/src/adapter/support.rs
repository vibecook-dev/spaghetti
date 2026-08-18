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

use super::scope::{ScopeProgramDeclaration, ScopeProgramManifest, ScopeProgramStatus};

pub const SUPPORT_SELECTION_CONTRACT_VERSION: u32 = 1;
pub const CONTRACT_VERSION_SELECTION_VERSION: u32 = 1;
pub const SUPPORT_RELEASE_SCHEMA_VERSION: u32 = 1;

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
    pub artifact_compatibility: ArtifactCompatibilityDeclaration,
}

impl SupportReleaseDescriptor {
    pub fn validate(&self) -> Result<(), SupportContractError> {
        validate_identifier("support release id", &self.support_release_id)?;
        self.artifact_compatibility.validate()
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
struct SupportReleaseWire {
    schema_version: u32,
    support_release_id: String,
    adapter_id: String,
    status: SupportReleaseStatus,
    artifact_compatibility: ArtifactCompatibilityDeclaration,
    references: SupportReferenceSetWire,
    versions: SupportVersionsWire,
}

#[derive(Debug, Deserialize)]
struct SupportDocumentIdentityWire {
    adapter_id: String,
    #[serde(default)]
    ads_id: Option<String>,
    #[serde(default)]
    support_release_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedSupportRelease {
    release_digest: Sha256Digest,
    descriptor: SupportReleaseDescriptor,
    adapter_id: String,
    adapter_binding: AdapterSupportBinding,
    scope_programs: ScopeProgramManifest,
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
    let descriptor = SupportReleaseDescriptor {
        support_release_id: release.support_release_id,
        status: release.status,
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
            "source_declaration" => source_declaration_digest = Some(actual_digest),
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
    })
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
    const SUPPORTED: Self = Self {
        version_probe: true,
        catalog: true,
        durable: true,
        scoped_observation: true,
        bounded_drift: true,
    };

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
            permissions: OperationPermissions::SUPPORTED,
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
        .filter(|release| release.artifact_compatibility.forward_catalog_only)
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
        Ok((
            decision,
            TypedAccessAuthorization {
                operation: operation_authorization,
                contracts,
                adapter_id: adapter_id.to_string(),
                support_release_digest: release.release_digest,
                scope_program_digest: release.adapter_binding.scope_program_digest,
                scope_programs: release.scope_programs.clone(),
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedAccessAuthorization {
    operation: OperationAuthorization,
    contracts: ContractVersionSelection,
    adapter_id: String,
    support_release_digest: Sha256Digest,
    scope_program_digest: Sha256Digest,
    scope_programs: ScopeProgramManifest,
}

impl TypedAccessAuthorization {
    pub fn operation(&self) -> &OperationAuthorization {
        &self.operation
    }

    pub fn contracts(&self) -> &ContractVersionSelection {
        &self.contracts
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

/// A program selected from the exact promoted declaration embedded in a
/// Rust-issued scoped-observation authorization. It borrows the authorization
/// and cannot be serialized or constructed by an adapter.
#[derive(Debug)]
pub struct AuthorizedScopeProgram<'a> {
    authorization: &'a TypedAccessAuthorization,
    program: &'a ScopeProgramDeclaration,
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
                SupportOperation::DurableHistoryRuntime,
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
        assert!(durable.select_scope_program("observe-session").is_err());

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
        assert!(scoped.select_scope_program("unknown-program").is_err());

        let forward = fixture
            .runtime_cases
            .iter()
            .find(|case| case.name == "forward-catalog")
            .unwrap();
        let forward = classify_runtime_support(&forward.probe, &fixture.releases).unwrap();
        assert!(forward
            .authorize(SupportOperation::DurableHistoryRuntime)
            .is_err());
        assert!(forward
            .authorize(SupportOperation::CatalogDiscovery)
            .is_ok());
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
                br#"{"schema_version":1,"declaration_id":"fixture-agent-scope","adapter_id":"fixture-agent","ads_id":"fixture-agent-ads","status":"promoted","roots":["root"],"programs":[{"program_id":"observe-session","root_entity_kind":"session","relations":[{"relation_id":"root-object","primitive":"KnownObject","access_root":"root","locator":"known-object","identity_inputs":["native-session-id"],"bounds":{"max_fan_out":1,"max_depth":1,"max_objects":1,"max_bytes":1024,"max_rows":0},"unavailable_behavior":"record_unavailable","claim_refs":["scope-evidence"]}],"claim_refs":["scope-evidence"]}],"blockers":[],"claim_refs":["scope-evidence"]}"#.as_slice(),
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
            "versions": {"adapter_package": "1.0.0", "decoder_contract": 1}
        }))
        .unwrap();
        let documents = paths
            .iter()
            .map(|(_, path, bytes)| SupportBundleDocument::new(path, bytes))
            .collect::<Vec<_>>();

        let verified = verify_support_release_bundle(&release, &documents).unwrap();
        assert_eq!(verified.adapter_id(), "fixture-agent");
        assert_eq!(verified.descriptor().status, SupportReleaseStatus::Promoted);
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

        let mut overlapping = fixture.releases;
        let mut second = overlapping[1].clone();
        second.support_release_id = "fixture-support-v2".to_string();
        overlapping.push(second);
        let decision = classify_runtime_support(&exact.probe, &overlapping).unwrap();
        assert_eq!(
            decision.reason,
            CompatibilityReason::AmbiguousPromotedRelease
        );
        assert!(!decision.permissions.durable);
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
}
