//! RFC 012A topology-independent semantic contracts.
//!
//! These types deliberately coexist with RFC 011's engine-local `EntityKey`
//! and `FactId` during migration. The legacy keys contain numeric catalog IDs
//! and must never be serialized as RFC 012 external or cross-topology
//! references.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::marker::PhantomData;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::de::{Error as _, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub const EXTERNAL_ENTITY_REFERENCE_VERSION: u32 = 1;
pub const SEMANTIC_REFERENCE_CONTRACT_VERSION: u32 = 1;
pub const SOURCE_COVERAGE_CONTRACT_VERSION: u32 = 1;
pub const SOURCE_COVERAGE_SET_CONTRACT_VERSION: u32 = 1;

const CANONICAL_SOURCE_INSTANCE_KEY_VERSION: u32 = 1;
const CANONICAL_ENTITY_KEY_VERSION: u32 = 1;
const ROOT_ACTOR_RUN_KEY_VERSION: u32 = 1;
const SOURCE_RECORD_ID_VERSION: u32 = 1;
const CANONICAL_FACT_ID_VERSION: u32 = 1;
const FACT_REVISION_ID_VERSION: u32 = 1;
const REFERENCE_ENCODING_VERSION: &str = "v1";
const DIGEST_BYTES: usize = 32;
const MAX_COMPONENT_BYTES: usize = 64 * 1024;
const MAX_COVERAGE_MEMBERSHIP_BYTES: usize = 64 * 1024 * 1024;
const MAX_IDENTIFIER_BYTES: usize = 256;
const MAX_COVERAGE_POINTS_PER_SET: usize = 250_000;
const MAX_COVERAGE_ABSENCES_PER_SET: usize = 250_000;
const MAX_COVERAGE_ERRORS_PER_SET: usize = 4_096;
const MAX_COVERAGE_UNAVAILABLE_REASON_BYTES: usize = 1_024;
const MAX_COVERAGE_ERROR_CODE_BYTES: usize = 64;
const JS_SAFE_INTEGER_MAX_U64: u64 = 9_007_199_254_740_991;
const JS_SAFE_INTEGER_MAX_I64: i64 = 9_007_199_254_740_991;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct SemanticContractError {
    message: String,
}

impl SemanticContractError {
    fn invalid(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

fn validate_identifier(label: &str, value: &str) -> Result<(), SemanticContractError> {
    if value.is_empty() || value.trim() != value {
        return Err(SemanticContractError::invalid(format!(
            "{label} must be non-empty and must not have surrounding whitespace"
        )));
    }
    if value.len() > MAX_IDENTIFIER_BYTES {
        return Err(SemanticContractError::invalid(format!(
            "{label} exceeds {MAX_IDENTIFIER_BYTES} bytes"
        )));
    }
    Ok(())
}

fn validate_component(label: &str, value: &[u8]) -> Result<(), SemanticContractError> {
    if value.is_empty() {
        return Err(SemanticContractError::invalid(format!(
            "{label} must not be empty"
        )));
    }
    if value.len() > MAX_COMPONENT_BYTES {
        return Err(SemanticContractError::invalid(format!(
            "{label} exceeds {MAX_COMPONENT_BYTES} bytes"
        )));
    }
    Ok(())
}

fn validate_portable_u64(label: &str, value: u64) -> Result<(), SemanticContractError> {
    if value > JS_SAFE_INTEGER_MAX_U64 {
        return Err(SemanticContractError::invalid(format!(
            "{label} exceeds the portable safe-integer range"
        )));
    }
    Ok(())
}

fn validate_portable_generation(label: &str, value: u64) -> Result<(), SemanticContractError> {
    validate_portable_u64(label, value)?;
    if value == 0 {
        return Err(SemanticContractError::invalid(format!(
            "{label} must be greater than zero"
        )));
    }
    Ok(())
}

fn validate_portable_i64(label: &str, value: i64) -> Result<(), SemanticContractError> {
    if !(-JS_SAFE_INTEGER_MAX_I64..=JS_SAFE_INTEGER_MAX_I64).contains(&value) {
        return Err(SemanticContractError::invalid(format!(
            "{label} exceeds the portable safe-integer range"
        )));
    }
    Ok(())
}

fn validate_coverage_error_code(value: &str) -> Result<(), SemanticContractError> {
    if value.is_empty() || value.len() > MAX_COVERAGE_ERROR_CODE_BYTES {
        return Err(SemanticContractError::invalid(format!(
            "coverage error code must contain 1 to {MAX_COVERAGE_ERROR_CODE_BYTES} bytes"
        )));
    }
    let mut bytes = value.bytes();
    if !bytes.next().is_some_and(|byte| byte.is_ascii_lowercase())
        || !bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err(SemanticContractError::invalid(
            "coverage error code must be a lowercase ASCII machine code",
        ));
    }
    Ok(())
}

fn deserialize_present_non_null<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

fn deserialize_bounded_vec<'de, D, T, const MAX: usize>(
    deserializer: D,
    label: &'static str,
) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct BoundedVecVisitor<T, const MAX: usize> {
        label: &'static str,
        marker: PhantomData<T>,
    }

    impl<'de, T, const MAX: usize> Visitor<'de> for BoundedVecVisitor<T, MAX>
    where
        T: Deserialize<'de>,
    {
        type Value = Vec<T>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(formatter, "{} with at most {MAX} entries", self.label)
        }

        fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            if sequence.size_hint().is_some_and(|size| size > MAX) {
                return Err(A::Error::custom(format!(
                    "{} exceeds {MAX} entries",
                    self.label
                )));
            }
            let mut values = Vec::with_capacity(sequence.size_hint().unwrap_or(0).min(MAX));
            while let Some(value) = sequence.next_element()? {
                if values.len() == MAX {
                    return Err(A::Error::custom(format!(
                        "{} exceeds {MAX} entries",
                        self.label
                    )));
                }
                values.push(value);
            }
            Ok(values)
        }
    }

    deserializer.deserialize_seq(BoundedVecVisitor::<T, MAX> {
        label,
        marker: PhantomData,
    })
}

fn deserialize_coverage_points<'de, D>(
    deserializer: D,
) -> Result<Vec<SourceCoveragePoint>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_bounded_vec::<_, _, MAX_COVERAGE_POINTS_PER_SET>(deserializer, "coverage points")
}

fn deserialize_coverage_absences<'de, D>(deserializer: D) -> Result<Vec<CoverageAbsence>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_bounded_vec::<_, _, MAX_COVERAGE_ABSENCES_PER_SET>(
        deserializer,
        "coverage absences",
    )
}

fn deserialize_coverage_errors<'de, D>(deserializer: D) -> Result<Vec<CoverageError>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_bounded_vec::<_, _, MAX_COVERAGE_ERRORS_PER_SET>(deserializer, "coverage errors")
}

fn contract_digest(domain: &[u8], components: &[&[u8]]) -> [u8; DIGEST_BYTES] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"spaghetti/rfc012a/contract\0");
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
            "opaque reference has an unsupported encoding version",
        ));
    };
    let decoded = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|error| D::Error::custom(format!("invalid opaque reference: {error}")))?;
    decoded
        .try_into()
        .map_err(|_| D::Error::custom("opaque reference must contain exactly 32 digest bytes"))
}

macro_rules! opaque_digest_type {
    ($name:ident) => {
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name([u8; DIGEST_BYTES]);

        impl $name {
            pub fn as_bytes(&self) -> &[u8; DIGEST_BYTES] {
                &self.0
            }

            pub(crate) fn from_digest(value: [u8; DIGEST_BYTES]) -> Self {
                Self(value)
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

opaque_digest_type!(CanonicalSourceInstanceKey);
opaque_digest_type!(CanonicalEntityKey);
opaque_digest_type!(SourceRecordId);
opaque_digest_type!(CanonicalFactId);
opaque_digest_type!(FactRevisionId);
opaque_digest_type!(CoverageStreamKey);
opaque_digest_type!(CoverageObjectKey);
opaque_digest_type!(CoverageDeclarationDigest);
opaque_digest_type!(CoverageMembershipRevision);
opaque_digest_type!(CoveragePositionRef);

/// Incremental encoder for a single coverage-membership contract component.
/// It produces the same digest as [`CoverageMembershipRevision::derive`] for
/// inputs that fit the legacy component bound, while allowing a source set to
/// hash its bounded membership without materializing one large byte buffer.
pub(crate) struct CoverageMembershipRevisionBuilder {
    hasher: blake3::Hasher,
    expected_bytes: usize,
    written_bytes: usize,
}

impl CoverageMembershipRevisionBuilder {
    pub(crate) fn update(&mut self, bytes: &[u8]) -> Result<(), SemanticContractError> {
        let written_bytes = self
            .written_bytes
            .checked_add(bytes.len())
            .ok_or_else(|| SemanticContractError::invalid("coverage membership length overflow"))?;
        if written_bytes > self.expected_bytes {
            return Err(SemanticContractError::invalid(
                "coverage membership wrote more than its declared byte length",
            ));
        }
        self.hasher.update(bytes);
        self.written_bytes = written_bytes;
        Ok(())
    }

    pub(crate) fn finish(self) -> Result<CoverageMembershipRevision, SemanticContractError> {
        if self.written_bytes != self.expected_bytes {
            return Err(SemanticContractError::invalid(format!(
                "coverage membership wrote {} of {} declared bytes",
                self.written_bytes, self.expected_bytes
            )));
        }
        Ok(CoverageMembershipRevision::from_digest(
            *self.hasher.finalize().as_bytes(),
        ))
    }
}

impl CanonicalSourceInstanceKey {
    pub fn derive(
        namespace_version: u32,
        stable_instance_discriminator: &[u8],
    ) -> Result<Self, SemanticContractError> {
        if namespace_version == 0 {
            return Err(SemanticContractError::invalid(
                "source-instance namespace version must be greater than zero",
            ));
        }
        validate_component(
            "stable source-instance discriminator",
            stable_instance_discriminator,
        )?;
        Ok(Self::from_digest(contract_digest(
            b"source-instance-key",
            &[
                &CANONICAL_SOURCE_INSTANCE_KEY_VERSION.to_be_bytes(),
                &namespace_version.to_be_bytes(),
                stable_instance_discriminator,
            ],
        )))
    }
}

impl CanonicalEntityKey {
    pub fn derive(
        adapter_id: &str,
        source_instance_key: &CanonicalSourceInstanceKey,
        entity_kind: &str,
        native_or_declared_fallback_key: &[u8],
    ) -> Result<Self, SemanticContractError> {
        validate_identifier("adapter id", adapter_id)?;
        validate_identifier("entity kind", entity_kind)?;
        validate_component(
            "native or declared fallback entity key",
            native_or_declared_fallback_key,
        )?;
        Ok(Self::from_digest(contract_digest(
            b"entity-key",
            &[
                &CANONICAL_ENTITY_KEY_VERSION.to_be_bytes(),
                adapter_id.as_bytes(),
                source_instance_key.as_bytes(),
                entity_kind.as_bytes(),
                native_or_declared_fallback_key,
            ],
        )))
    }

    /// Derive the RFC 012C root actor/run identity from the already-final base
    /// session identity and `Root` role. A support-declared native run
    /// discriminator may refine the singleton root, but never replaces the
    /// base session as key material.
    pub fn derive_root_actor_run(
        adapter_id: &str,
        source_instance_key: &CanonicalSourceInstanceKey,
        root_session_key: &CanonicalEntityKey,
        declared_native_run_discriminator: Option<&[u8]>,
    ) -> Result<Self, SemanticContractError> {
        let (discriminator_kind, discriminator) = match declared_native_run_discriminator {
            Some(value) => {
                validate_component("declared native root-run discriminator", value)?;
                (b"declared-native-run".as_slice(), value)
            }
            None => (b"singleton".as_slice(), b"singleton-root".as_slice()),
        };
        let stable_run_discriminator = contract_digest(
            b"root-actor-run-key",
            &[
                &ROOT_ACTOR_RUN_KEY_VERSION.to_be_bytes(),
                root_session_key.as_bytes(),
                b"root",
                discriminator_kind,
                discriminator,
            ],
        );
        Self::derive(
            adapter_id,
            source_instance_key,
            "run",
            &stable_run_discriminator,
        )
    }
}

impl SourceRecordId {
    #[allow(clippy::too_many_arguments)]
    pub fn derive(
        adapter_id: &str,
        source_instance_key: &CanonicalSourceInstanceKey,
        stream_key: &[u8],
        object_key: &[u8],
        generation: u64,
        logical_record_range_or_revision: &[u8],
        framing_contract_version: u32,
    ) -> Result<Self, SemanticContractError> {
        validate_identifier("adapter id", adapter_id)?;
        validate_component("stream key", stream_key)?;
        validate_component("object key", object_key)?;
        validate_component(
            "logical record range or document revision",
            logical_record_range_or_revision,
        )?;
        if framing_contract_version == 0 {
            return Err(SemanticContractError::invalid(
                "framing contract version must be greater than zero",
            ));
        }
        Ok(Self::from_digest(contract_digest(
            b"source-record-id",
            &[
                &SOURCE_RECORD_ID_VERSION.to_be_bytes(),
                adapter_id.as_bytes(),
                source_instance_key.as_bytes(),
                stream_key,
                object_key,
                &generation.to_be_bytes(),
                logical_record_range_or_revision,
                &framing_contract_version.to_be_bytes(),
            ],
        )))
    }
}

impl CanonicalFactId {
    pub fn native(
        adapter_id: &str,
        source_instance_key: &CanonicalSourceInstanceKey,
        fact_kind: &str,
        stable_native_fact_key: &[u8],
    ) -> Result<Self, SemanticContractError> {
        Self::derive(
            b"native",
            adapter_id,
            source_instance_key,
            fact_kind,
            stable_native_fact_key,
        )
    }

    pub fn derived(
        adapter_id: &str,
        source_instance_key: &CanonicalSourceInstanceKey,
        fact_kind: &str,
        source_record_id: &SourceRecordId,
        deterministic_semantic_subkey: &[u8],
    ) -> Result<Self, SemanticContractError> {
        validate_component(
            "deterministic semantic subkey",
            deterministic_semantic_subkey,
        )?;
        let mut derived_key =
            Vec::with_capacity(DIGEST_BYTES + deterministic_semantic_subkey.len());
        derived_key.extend_from_slice(source_record_id.as_bytes());
        derived_key.extend_from_slice(deterministic_semantic_subkey);
        Self::derive(
            b"derived",
            adapter_id,
            source_instance_key,
            fact_kind,
            &derived_key,
        )
    }

    fn derive(
        key_kind: &[u8],
        adapter_id: &str,
        source_instance_key: &CanonicalSourceInstanceKey,
        fact_kind: &str,
        fact_key: &[u8],
    ) -> Result<Self, SemanticContractError> {
        validate_identifier("adapter id", adapter_id)?;
        validate_identifier("fact kind", fact_kind)?;
        validate_component("fact key", fact_key)?;
        Ok(Self::from_digest(contract_digest(
            b"fact-id",
            &[
                &CANONICAL_FACT_ID_VERSION.to_be_bytes(),
                adapter_id.as_bytes(),
                source_instance_key.as_bytes(),
                fact_kind.as_bytes(),
                key_kind,
                fact_key,
            ],
        )))
    }
}

impl FactRevisionId {
    pub fn derive(
        fact_id: &CanonicalFactId,
        revision_contract_version: u32,
        source_or_semantic_revision: &[u8],
    ) -> Result<Self, SemanticContractError> {
        if revision_contract_version == 0 {
            return Err(SemanticContractError::invalid(
                "fact revision contract version must be greater than zero",
            ));
        }
        validate_component(
            "source or semantic fact revision",
            source_or_semantic_revision,
        )?;
        Ok(Self::from_digest(contract_digest(
            b"fact-revision-id",
            &[
                &FACT_REVISION_ID_VERSION.to_be_bytes(),
                fact_id.as_bytes(),
                &revision_contract_version.to_be_bytes(),
                source_or_semantic_revision,
            ],
        )))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ExternalEntityRef {
    pub external_entity_reference_version: u32,
    pub entity_key: CanonicalEntityKey,
}

impl ExternalEntityRef {
    pub fn new(entity_key: CanonicalEntityKey) -> Self {
        Self {
            external_entity_reference_version: EXTERNAL_ENTITY_REFERENCE_VERSION,
            entity_key,
        }
    }

    fn validate(&self) -> Result<(), SemanticContractError> {
        if self.external_entity_reference_version != EXTERNAL_ENTITY_REFERENCE_VERSION {
            return Err(SemanticContractError::invalid(format!(
                "unsupported external entity reference version {}",
                self.external_entity_reference_version
            )));
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for ExternalEntityRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            external_entity_reference_version: u32,
            entity_key: CanonicalEntityKey,
        }

        let wire = Wire::deserialize(deserializer)?;
        let value = Self {
            external_entity_reference_version: wire.external_entity_reference_version,
            entity_key: wire.entity_key,
        };
        value.validate().map_err(D::Error::custom)?;
        Ok(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SemanticRevisionRef {
    pub semantic_reference_contract_version: u32,
    pub fact_revision_id: FactRevisionId,
}

impl SemanticRevisionRef {
    pub fn new(fact_revision_id: FactRevisionId) -> Self {
        Self {
            semantic_reference_contract_version: SEMANTIC_REFERENCE_CONTRACT_VERSION,
            fact_revision_id,
        }
    }

    fn validate(&self) -> Result<(), SemanticContractError> {
        if self.semantic_reference_contract_version != SEMANTIC_REFERENCE_CONTRACT_VERSION {
            return Err(SemanticContractError::invalid(format!(
                "unsupported semantic reference contract version {}",
                self.semantic_reference_contract_version
            )));
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for SemanticRevisionRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            semantic_reference_contract_version: u32,
            fact_revision_id: FactRevisionId,
        }

        let wire = Wire::deserialize(deserializer)?;
        let value = Self {
            semantic_reference_contract_version: wire.semantic_reference_contract_version,
            fact_revision_id: wire.fact_revision_id,
        };
        value.validate().map_err(D::Error::custom)?;
        Ok(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeIdentity {
    pub native_namespace: String,
    pub native_id: String,
}

impl NativeIdentity {
    fn validate(&self) -> Result<(), SemanticContractError> {
        validate_identifier("native identity namespace", &self.native_namespace)?;
        validate_identifier("native identity", &self.native_id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualifiedValueQuality {
    Exact,
    NativeClaimed,
    Derived,
    Estimated,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContractCompleteness {
    Complete,
    Partial,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualifiedUnknownReason {
    Missing,
    Unsupported,
    Withheld,
    NotYetObserved,
    Ambiguous,
    Malformed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QualifiedValue<T, A = String, P = Vec<SemanticRevisionRef>> {
    pub value: Option<T>,
    pub quality: QualifiedValueQuality,
    pub authority: A,
    pub completeness: ContractCompleteness,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unknown_reason: Option<QualifiedUnknownReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_at: Option<i64>,
    pub provenance: P,
}

impl<T, A, P> QualifiedValue<T, A, P> {
    #[allow(clippy::too_many_arguments)]
    pub fn from_parts(
        value: Option<T>,
        quality: QualifiedValueQuality,
        authority: A,
        completeness: ContractCompleteness,
        unknown_reason: Option<QualifiedUnknownReason>,
        effective_at: Option<i64>,
        provenance: P,
    ) -> Result<Self, SemanticContractError> {
        let unknown = quality == QualifiedValueQuality::Unknown;
        if unknown != value.is_none() {
            return Err(SemanticContractError::invalid(
                "quality is Unknown if and only if value is absent",
            ));
        }
        if unknown != unknown_reason.is_some() {
            return Err(SemanticContractError::invalid(
                "unknown_reason is present if and only if quality is Unknown",
            ));
        }
        if let Some(effective_at) = effective_at {
            validate_portable_i64("effective_at", effective_at)?;
        }
        Ok(Self {
            value,
            quality,
            authority,
            completeness,
            unknown_reason,
            effective_at,
            provenance,
        })
    }
}

impl<'de, T, A, P> Deserialize<'de> for QualifiedValue<T, A, P>
where
    T: Deserialize<'de>,
    A: Deserialize<'de>,
    P: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(
            deny_unknown_fields,
            bound(deserialize = "T: Deserialize<'de>, A: Deserialize<'de>, P: Deserialize<'de>")
        )]
        struct Wire<T, A, P> {
            #[serde(deserialize_with = "deserialize_required_option")]
            value: Option<T>,
            quality: QualifiedValueQuality,
            authority: A,
            completeness: ContractCompleteness,
            #[serde(default, deserialize_with = "deserialize_present_non_null")]
            unknown_reason: Option<QualifiedUnknownReason>,
            #[serde(default, deserialize_with = "deserialize_present_non_null")]
            effective_at: Option<i64>,
            provenance: P,
        }

        fn deserialize_required_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
        where
            D: Deserializer<'de>,
            T: Deserialize<'de>,
        {
            Option::<T>::deserialize(deserializer)
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::from_parts(
            wire.value,
            wire.quality,
            wire.authority,
            wire.completeness,
            wire.unknown_reason,
            wire.effective_at,
            wire.provenance,
        )
        .map_err(D::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NativeIdentityClaim {
    pub entity_ref: ExternalEntityRef,
    pub identity: QualifiedValue<NativeIdentity>,
}

impl NativeIdentityClaim {
    pub fn new(
        entity_ref: ExternalEntityRef,
        identity: QualifiedValue<NativeIdentity>,
    ) -> Result<Self, SemanticContractError> {
        let value = Self {
            entity_ref,
            identity,
        };
        value.validate()?;
        Ok(value)
    }

    fn validate(&self) -> Result<(), SemanticContractError> {
        self.entity_ref.validate()?;
        validate_identifier("qualified value authority", &self.identity.authority)?;
        if let Some(identity) = &self.identity.value {
            identity.validate()?;
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for NativeIdentityClaim {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            entity_ref: ExternalEntityRef,
            identity: QualifiedValue<NativeIdentity>,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.entity_ref, wire.identity).map_err(D::Error::custom)
    }
}

fn derive_coverage_key(
    domain: &[u8],
    namespace: &str,
    stable_key: &[u8],
) -> Result<[u8; DIGEST_BYTES], SemanticContractError> {
    validate_identifier("coverage key namespace", namespace)?;
    validate_component("coverage stable key", stable_key)?;
    Ok(contract_digest(domain, &[namespace.as_bytes(), stable_key]))
}

impl CoverageStreamKey {
    pub fn derive(namespace: &str, stable_key: &[u8]) -> Result<Self, SemanticContractError> {
        derive_coverage_key(b"coverage-stream-key", namespace, stable_key).map(Self::from_digest)
    }
}

impl CoverageObjectKey {
    pub fn derive(namespace: &str, stable_key: &[u8]) -> Result<Self, SemanticContractError> {
        derive_coverage_key(b"coverage-object-key", namespace, stable_key).map(Self::from_digest)
    }
}

impl CoverageDeclarationDigest {
    pub fn derive(declaration: &[u8]) -> Result<Self, SemanticContractError> {
        validate_component("source or scope declaration", declaration)?;
        Ok(Self::from_digest(contract_digest(
            b"coverage-declaration",
            &[declaration],
        )))
    }
}

impl CoverageMembershipRevision {
    pub fn derive(membership: &[u8]) -> Result<Self, SemanticContractError> {
        validate_component("coverage membership", membership)?;
        Ok(Self::from_digest(contract_digest(
            b"coverage-membership",
            &[membership],
        )))
    }

    pub(crate) fn begin_streaming(
        encoded_membership_bytes: usize,
    ) -> Result<CoverageMembershipRevisionBuilder, SemanticContractError> {
        if encoded_membership_bytes == 0 {
            return Err(SemanticContractError::invalid(
                "coverage membership must not be empty",
            ));
        }
        if encoded_membership_bytes > MAX_COVERAGE_MEMBERSHIP_BYTES {
            return Err(SemanticContractError::invalid(format!(
                "coverage membership exceeds {MAX_COVERAGE_MEMBERSHIP_BYTES} streaming bytes"
            )));
        }
        let mut hasher = blake3::Hasher::new();
        let domain = b"coverage-membership";
        hasher.update(b"spaghetti/rfc012a/contract\0");
        hasher.update(&(domain.len() as u64).to_be_bytes());
        hasher.update(domain);
        hasher.update(&1_u64.to_be_bytes());
        hasher.update(&(encoded_membership_bytes as u64).to_be_bytes());
        Ok(CoverageMembershipRevisionBuilder {
            hasher,
            expected_bytes: encoded_membership_bytes,
            written_bytes: 0,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CoverageDomain {
    Decode,
    FactFamily { family: String, version: u32 },
    ProjectionPack { pack: String, version: u32 },
}

impl<'de> Deserialize<'de> for CoverageDomain {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "snake_case")]
        enum Kind {
            Decode,
            FactFamily,
            ProjectionPack,
        }

        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            kind: Kind,
            #[serde(default, deserialize_with = "deserialize_present_non_null")]
            family: Option<String>,
            #[serde(default, deserialize_with = "deserialize_present_non_null")]
            pack: Option<String>,
            #[serde(default, deserialize_with = "deserialize_present_non_null")]
            version: Option<u32>,
        }

        let wire = Wire::deserialize(deserializer)?;
        let value = match (wire.kind, wire.family, wire.pack, wire.version) {
            (Kind::Decode, None, None, None) => Self::Decode,
            (Kind::FactFamily, Some(family), None, Some(version)) => {
                Self::FactFamily { family, version }
            }
            (Kind::ProjectionPack, None, Some(pack), Some(version)) => {
                Self::ProjectionPack { pack, version }
            }
            _ => {
                return Err(D::Error::custom(
                    "coverage domain fields do not match its kind",
                ));
            }
        };
        value.validate().map_err(D::Error::custom)?;
        Ok(value)
    }
}

impl CoverageDomain {
    fn validate(&self) -> Result<(), SemanticContractError> {
        match self {
            Self::Decode => Ok(()),
            Self::FactFamily { family, version } => {
                validate_identifier("fact family", family)?;
                if *version == 0 {
                    return Err(SemanticContractError::invalid(
                        "fact-family version must be greater than zero",
                    ));
                }
                Ok(())
            }
            Self::ProjectionPack { pack, version } => {
                validate_identifier("projection pack", pack)?;
                if *version == 0 {
                    return Err(SemanticContractError::invalid(
                        "projection-pack version must be greater than zero",
                    ));
                }
                Ok(())
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoveragePositionKind {
    AppendCursor,
    DocumentRevision,
    SnapshotRevision,
    DatabaseWatermark,
    KeyRangeToken,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CoveragePosition {
    pub kind: CoveragePositionKind,
    pub opaque: CoveragePositionRef,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monotonic_order: Option<u64>,
}

impl<'de> Deserialize<'de> for CoveragePosition {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            kind: CoveragePositionKind,
            opaque: CoveragePositionRef,
            #[serde(default, deserialize_with = "deserialize_present_non_null")]
            monotonic_order: Option<u64>,
        }

        let wire = Wire::deserialize(deserializer)?;
        let value = Self {
            kind: wire.kind,
            opaque: wire.opaque,
            monotonic_order: wire.monotonic_order,
        };
        value.validate().map_err(D::Error::custom)?;
        Ok(value)
    }
}

impl CoveragePosition {
    pub fn derive(
        kind: CoveragePositionKind,
        opaque_native_position: &[u8],
        monotonic_order: Option<u64>,
    ) -> Result<Self, SemanticContractError> {
        validate_component("coverage position", opaque_native_position)?;
        let kind_tag = [kind.contract_tag()];
        let value = Self {
            kind,
            opaque: CoveragePositionRef::from_digest(contract_digest(
                b"coverage-position",
                &[&kind_tag, opaque_native_position],
            )),
            monotonic_order,
        };
        value.validate()?;
        Ok(value)
    }

    fn validate(&self) -> Result<(), SemanticContractError> {
        if let Some(order) = self.monotonic_order {
            validate_portable_u64("coverage monotonic order", order)?;
        }
        Ok(())
    }
}

impl CoveragePositionKind {
    fn contract_tag(self) -> u8 {
        match self {
            Self::AppendCursor => 1,
            Self::DocumentRevision => 2,
            Self::SnapshotRevision => 3,
            Self::DatabaseWatermark => 4,
            Self::KeyRangeToken => 5,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CoverageStatus {
    CompleteThrough,
    ExactSnapshot,
    Partial,
    Unavailable { reason: String },
}

impl<'de> Deserialize<'de> for CoverageStatus {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "snake_case")]
        enum Kind {
            CompleteThrough,
            ExactSnapshot,
            Partial,
            Unavailable,
        }

        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            kind: Kind,
            #[serde(default, deserialize_with = "deserialize_present_non_null")]
            reason: Option<String>,
        }

        let wire = Wire::deserialize(deserializer)?;
        let value = match (wire.kind, wire.reason) {
            (Kind::CompleteThrough, None) => Self::CompleteThrough,
            (Kind::ExactSnapshot, None) => Self::ExactSnapshot,
            (Kind::Partial, None) => Self::Partial,
            (Kind::Unavailable, Some(reason)) => Self::Unavailable { reason },
            _ => {
                return Err(D::Error::custom(
                    "coverage status fields do not match its kind",
                ));
            }
        };
        value.validate().map_err(D::Error::custom)?;
        Ok(value)
    }
}

impl CoverageStatus {
    fn validate(&self) -> Result<(), SemanticContractError> {
        if let Self::Unavailable { reason } = self {
            if reason.is_empty() || reason.trim() != reason {
                return Err(SemanticContractError::invalid(
                    "unavailable coverage requires a non-empty canonical reason",
                ));
            }
            if reason.len() > MAX_COVERAGE_UNAVAILABLE_REASON_BYTES {
                return Err(SemanticContractError::invalid(format!(
                    "unavailable coverage reason exceeds {MAX_COVERAGE_UNAVAILABLE_REASON_BYTES} bytes"
                )));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct CoverageProvenance {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_record_id: Option<SourceRecordId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic_revision_ref: Option<SemanticRevisionRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<i64>,
}

impl<'de> Deserialize<'de> for CoverageProvenance {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            #[serde(default, deserialize_with = "deserialize_present_non_null")]
            source_record_id: Option<SourceRecordId>,
            #[serde(default, deserialize_with = "deserialize_present_non_null")]
            semantic_revision_ref: Option<SemanticRevisionRef>,
            #[serde(default, deserialize_with = "deserialize_present_non_null")]
            observed_at: Option<i64>,
        }

        let wire = Wire::deserialize(deserializer)?;
        let value = Self {
            source_record_id: wire.source_record_id,
            semantic_revision_ref: wire.semantic_revision_ref,
            observed_at: wire.observed_at,
        };
        value.validate().map_err(D::Error::custom)?;
        Ok(value)
    }
}

impl CoverageProvenance {
    fn validate(&self) -> Result<(), SemanticContractError> {
        if let Some(observed_at) = self.observed_at {
            validate_portable_i64("coverage observed_at", observed_at)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceCoveragePoint {
    pub coverage_contract_version: u32,
    pub coverage_domain: CoverageDomain,
    pub adapter_id: String,
    pub source_instance_key: CanonicalSourceInstanceKey,
    pub stream_key: CoverageStreamKey,
    pub object_key: CoverageObjectKey,
    pub generation: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<CoveragePosition>,
    pub status: CoverageStatus,
    pub provenance: CoverageProvenance,
}

impl SourceCoveragePoint {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        coverage_domain: CoverageDomain,
        adapter_id: impl Into<String>,
        source_instance_key: CanonicalSourceInstanceKey,
        stream_key: CoverageStreamKey,
        object_key: CoverageObjectKey,
        generation: u64,
        position: Option<CoveragePosition>,
        status: CoverageStatus,
        provenance: CoverageProvenance,
    ) -> Result<Self, SemanticContractError> {
        let value = Self {
            coverage_contract_version: SOURCE_COVERAGE_CONTRACT_VERSION,
            coverage_domain,
            adapter_id: adapter_id.into(),
            source_instance_key,
            stream_key,
            object_key,
            generation,
            position,
            status,
            provenance,
        };
        value.validate()?;
        Ok(value)
    }

    fn validate(&self) -> Result<(), SemanticContractError> {
        if self.coverage_contract_version != SOURCE_COVERAGE_CONTRACT_VERSION {
            return Err(SemanticContractError::invalid(format!(
                "unsupported source coverage contract version {}",
                self.coverage_contract_version
            )));
        }
        self.coverage_domain.validate()?;
        validate_identifier("coverage adapter id", &self.adapter_id)?;
        validate_portable_generation("coverage generation", self.generation)?;
        if let Some(position) = &self.position {
            position.validate()?;
        }
        self.status.validate()?;
        self.provenance.validate()?;
        match (&self.status, &self.position) {
            (CoverageStatus::CompleteThrough, Some(position))
                if matches!(
                    position.kind,
                    CoveragePositionKind::AppendCursor
                        | CoveragePositionKind::DatabaseWatermark
                        | CoveragePositionKind::KeyRangeToken
                ) => {}
            (CoverageStatus::ExactSnapshot, Some(position))
                if matches!(
                    position.kind,
                    CoveragePositionKind::DocumentRevision | CoveragePositionKind::SnapshotRevision
                ) => {}
            (CoverageStatus::Partial, _) => {}
            (CoverageStatus::Unavailable { .. }, _) => {}
            (CoverageStatus::CompleteThrough, _) => {
                return Err(SemanticContractError::invalid(
                    "complete-through coverage requires an ordered position",
                ));
            }
            (CoverageStatus::ExactSnapshot, _) => {
                return Err(SemanticContractError::invalid(
                    "exact-snapshot coverage requires a snapshot position",
                ));
            }
        }
        Ok(())
    }

    fn coordinate(&self) -> CoverageCoordinate {
        CoverageCoordinate {
            stream_key: self.stream_key,
            object_key: self.object_key,
            generation: self.generation,
        }
    }
}

impl<'de> Deserialize<'de> for SourceCoveragePoint {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            coverage_contract_version: u32,
            coverage_domain: CoverageDomain,
            adapter_id: String,
            source_instance_key: CanonicalSourceInstanceKey,
            stream_key: CoverageStreamKey,
            object_key: CoverageObjectKey,
            generation: u64,
            #[serde(default, deserialize_with = "deserialize_present_non_null")]
            position: Option<CoveragePosition>,
            status: CoverageStatus,
            provenance: CoverageProvenance,
        }

        let wire = Wire::deserialize(deserializer)?;
        let value = Self {
            coverage_contract_version: wire.coverage_contract_version,
            coverage_domain: wire.coverage_domain,
            adapter_id: wire.adapter_id,
            source_instance_key: wire.source_instance_key,
            stream_key: wire.stream_key,
            object_key: wire.object_key,
            generation: wire.generation,
            position: wire.position,
            status: wire.status,
            provenance: wire.provenance,
        };
        value.validate().map_err(D::Error::custom)?;
        Ok(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CoverageScope {
    pub adapter_id: String,
    pub source_instance_key: CanonicalSourceInstanceKey,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_entity_key: Option<CanonicalEntityKey>,
    pub support_release_id: String,
    pub source_or_scope_declaration_digest: CoverageDeclarationDigest,
}

impl<'de> Deserialize<'de> for CoverageScope {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            adapter_id: String,
            source_instance_key: CanonicalSourceInstanceKey,
            #[serde(default, deserialize_with = "deserialize_present_non_null")]
            root_entity_key: Option<CanonicalEntityKey>,
            support_release_id: String,
            source_or_scope_declaration_digest: CoverageDeclarationDigest,
        }

        let wire = Wire::deserialize(deserializer)?;
        let value = Self {
            adapter_id: wire.adapter_id,
            source_instance_key: wire.source_instance_key,
            root_entity_key: wire.root_entity_key,
            support_release_id: wire.support_release_id,
            source_or_scope_declaration_digest: wire.source_or_scope_declaration_digest,
        };
        value.validate().map_err(D::Error::custom)?;
        Ok(value)
    }
}

impl CoverageScope {
    fn validate(&self) -> Result<(), SemanticContractError> {
        validate_identifier("coverage scope adapter id", &self.adapter_id)?;
        validate_identifier("support release id", &self.support_release_id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageAbsenceKind {
    Absent,
    Deleted,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct CoverageAbsence {
    pub stream_key: CoverageStreamKey,
    pub object_key: CoverageObjectKey,
    pub generation: u64,
    pub kind: CoverageAbsenceKind,
}

impl<'de> Deserialize<'de> for CoverageAbsence {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            stream_key: CoverageStreamKey,
            object_key: CoverageObjectKey,
            generation: u64,
            kind: CoverageAbsenceKind,
        }

        let wire = Wire::deserialize(deserializer)?;
        let value = Self {
            stream_key: wire.stream_key,
            object_key: wire.object_key,
            generation: wire.generation,
            kind: wire.kind,
        };
        value.validate().map_err(D::Error::custom)?;
        Ok(value)
    }
}

impl CoverageAbsence {
    fn validate(&self) -> Result<(), SemanticContractError> {
        validate_portable_generation("coverage absence generation", self.generation)
    }

    fn coordinate(&self) -> CoverageCoordinate {
        CoverageCoordinate {
            stream_key: self.stream_key,
            object_key: self.object_key,
            generation: self.generation,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct CoverageError {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_key: Option<CoverageStreamKey>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object_key: Option<CoverageObjectKey>,
    pub code: String,
}

impl<'de> Deserialize<'de> for CoverageError {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            #[serde(default, deserialize_with = "deserialize_present_non_null")]
            stream_key: Option<CoverageStreamKey>,
            #[serde(default, deserialize_with = "deserialize_present_non_null")]
            object_key: Option<CoverageObjectKey>,
            code: String,
        }

        let wire = Wire::deserialize(deserializer)?;
        let value = Self {
            stream_key: wire.stream_key,
            object_key: wire.object_key,
            code: wire.code,
        };
        value.validate().map_err(D::Error::custom)?;
        Ok(value)
    }
}

impl CoverageError {
    fn validate(&self) -> Result<(), SemanticContractError> {
        validate_coverage_error_code(&self.code)?;
        if self.object_key.is_some() && self.stream_key.is_none() {
            return Err(SemanticContractError::invalid(
                "coverage error object key requires a stream key",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageSetCompleteness {
    Complete,
    Partial,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceCoverageSet {
    pub coverage_set_contract_version: u32,
    pub coverage_domain: CoverageDomain,
    pub scope: CoverageScope,
    pub membership_revision: CoverageMembershipRevision,
    pub points: Vec<SourceCoveragePoint>,
    pub explicit_absence_or_deletion: Vec<CoverageAbsence>,
    pub explicit_errors: Vec<CoverageError>,
    pub completeness: CoverageSetCompleteness,
}

impl SourceCoverageSet {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        coverage_domain: CoverageDomain,
        scope: CoverageScope,
        membership_revision: CoverageMembershipRevision,
        points: Vec<SourceCoveragePoint>,
        explicit_absence_or_deletion: Vec<CoverageAbsence>,
        explicit_errors: Vec<CoverageError>,
        completeness: CoverageSetCompleteness,
    ) -> Result<Self, SemanticContractError> {
        let value = Self {
            coverage_set_contract_version: SOURCE_COVERAGE_SET_CONTRACT_VERSION,
            coverage_domain,
            scope,
            membership_revision,
            points,
            explicit_absence_or_deletion,
            explicit_errors,
            completeness,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<(), SemanticContractError> {
        if self.coverage_set_contract_version != SOURCE_COVERAGE_SET_CONTRACT_VERSION {
            return Err(SemanticContractError::invalid(format!(
                "unsupported source coverage set contract version {}",
                self.coverage_set_contract_version
            )));
        }
        self.coverage_domain.validate()?;
        self.scope.validate()?;
        if self.points.len() > MAX_COVERAGE_POINTS_PER_SET {
            return Err(SemanticContractError::invalid(format!(
                "coverage set exceeds {MAX_COVERAGE_POINTS_PER_SET} points"
            )));
        }
        if self.explicit_absence_or_deletion.len() > MAX_COVERAGE_ABSENCES_PER_SET {
            return Err(SemanticContractError::invalid(format!(
                "coverage set exceeds {MAX_COVERAGE_ABSENCES_PER_SET} absences"
            )));
        }
        if self.explicit_errors.len() > MAX_COVERAGE_ERRORS_PER_SET {
            return Err(SemanticContractError::invalid(format!(
                "coverage set exceeds {MAX_COVERAGE_ERRORS_PER_SET} errors"
            )));
        }
        let mut coordinates = BTreeSet::new();
        for point in &self.points {
            point.validate()?;
            if point.coverage_domain != self.coverage_domain
                || point.adapter_id != self.scope.adapter_id
                || point.source_instance_key != self.scope.source_instance_key
            {
                return Err(SemanticContractError::invalid(
                    "coverage point does not belong to its set domain and scope",
                ));
            }
            if !coordinates.insert(point.coordinate()) {
                return Err(SemanticContractError::invalid(
                    "coverage set contains a duplicate object generation",
                ));
            }
        }
        let mut absences = BTreeSet::new();
        for absence in &self.explicit_absence_or_deletion {
            absence.validate()?;
            let coordinate = absence.coordinate();
            if coordinates.contains(&coordinate) || !absences.insert(coordinate) {
                return Err(SemanticContractError::invalid(
                    "coverage absence conflicts with a point or duplicate absence",
                ));
            }
        }
        let mut errors = BTreeSet::new();
        for error in &self.explicit_errors {
            error.validate()?;
            if !errors.insert(error) {
                return Err(SemanticContractError::invalid(
                    "coverage set contains a duplicate explicit error",
                ));
            }
        }
        if self.completeness == CoverageSetCompleteness::Complete
            && (!self.explicit_errors.is_empty()
                || self.points.iter().any(|point| {
                    matches!(
                        point.status,
                        CoverageStatus::Partial | CoverageStatus::Unavailable { .. }
                    )
                }))
        {
            return Err(SemanticContractError::invalid(
                "complete coverage cannot contain errors, partial points, or unavailable points",
            ));
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for SourceCoverageSet {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            coverage_set_contract_version: u32,
            coverage_domain: CoverageDomain,
            scope: CoverageScope,
            membership_revision: CoverageMembershipRevision,
            #[serde(deserialize_with = "deserialize_coverage_points")]
            points: Vec<SourceCoveragePoint>,
            #[serde(deserialize_with = "deserialize_coverage_absences")]
            explicit_absence_or_deletion: Vec<CoverageAbsence>,
            #[serde(deserialize_with = "deserialize_coverage_errors")]
            explicit_errors: Vec<CoverageError>,
            completeness: CoverageSetCompleteness,
        }

        let wire = Wire::deserialize(deserializer)?;
        let value = Self {
            coverage_set_contract_version: wire.coverage_set_contract_version,
            coverage_domain: wire.coverage_domain,
            scope: wire.scope,
            membership_revision: wire.membership_revision,
            points: wire.points,
            explicit_absence_or_deletion: wire.explicit_absence_or_deletion,
            explicit_errors: wire.explicit_errors,
            completeness: wire.completeness,
        };
        value.validate().map_err(D::Error::custom)?;
        Ok(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct CoverageCoordinate {
    stream_key: CoverageStreamKey,
    object_key: CoverageObjectKey,
    generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageComparison {
    Equal,
    Dominates,
    Behind,
    Incomparable,
}

fn compare_positions(
    candidate: &CoveragePosition,
    baseline: &CoveragePosition,
) -> CoverageComparison {
    if candidate.kind != baseline.kind {
        return CoverageComparison::Incomparable;
    }
    if candidate.opaque == baseline.opaque {
        return CoverageComparison::Equal;
    }
    match (candidate.monotonic_order, baseline.monotonic_order) {
        (Some(candidate), Some(baseline)) if candidate > baseline => CoverageComparison::Dominates,
        (Some(candidate), Some(baseline)) if candidate < baseline => CoverageComparison::Behind,
        (Some(_), Some(_)) => CoverageComparison::Incomparable,
        _ => CoverageComparison::Incomparable,
    }
}

fn point_dominates(candidate: &SourceCoveragePoint, baseline: &SourceCoveragePoint) -> bool {
    if candidate.coordinate() != baseline.coordinate() {
        return false;
    }
    if candidate.status == baseline.status && candidate.position == baseline.position {
        return true;
    }
    let candidate_complete = matches!(
        candidate.status,
        CoverageStatus::CompleteThrough | CoverageStatus::ExactSnapshot
    );
    if !candidate_complete {
        return false;
    }
    let status_compatible = matches!(
        (&candidate.status, &baseline.status),
        (
            CoverageStatus::CompleteThrough,
            CoverageStatus::CompleteThrough
        ) | (CoverageStatus::ExactSnapshot, CoverageStatus::ExactSnapshot)
            | (CoverageStatus::CompleteThrough, CoverageStatus::Partial)
            | (CoverageStatus::ExactSnapshot, CoverageStatus::Partial)
            | (
                CoverageStatus::CompleteThrough,
                CoverageStatus::Unavailable { .. }
            )
            | (
                CoverageStatus::ExactSnapshot,
                CoverageStatus::Unavailable { .. }
            )
    );
    if !status_compatible {
        return false;
    }
    match (&candidate.position, &baseline.position) {
        (Some(_), None) => true,
        (Some(candidate), Some(baseline)) => matches!(
            compare_positions(candidate, baseline),
            CoverageComparison::Equal | CoverageComparison::Dominates
        ),
        _ => false,
    }
}

fn set_dominates(candidate: &SourceCoverageSet, baseline: &SourceCoverageSet) -> bool {
    if candidate.completeness != CoverageSetCompleteness::Complete {
        return false;
    }
    let candidate_points: BTreeMap<_, _> = candidate
        .points
        .iter()
        .map(|point| (point.coordinate(), point))
        .collect();
    if !baseline.points.iter().all(|point| {
        candidate_points
            .get(&point.coordinate())
            .is_some_and(|candidate| point_dominates(candidate, point))
    }) {
        return false;
    }
    let candidate_absences: BTreeSet<_> = candidate.explicit_absence_or_deletion.iter().collect();
    baseline
        .explicit_absence_or_deletion
        .iter()
        .all(|absence| candidate_absences.contains(absence))
}

fn coverage_semantically_equal(
    candidate: &SourceCoverageSet,
    baseline: &SourceCoverageSet,
) -> bool {
    if candidate.completeness != baseline.completeness
        || candidate.points.len() != baseline.points.len()
        || candidate.explicit_absence_or_deletion.len()
            != baseline.explicit_absence_or_deletion.len()
        || candidate.explicit_errors.len() != baseline.explicit_errors.len()
    {
        return false;
    }
    let baseline_points: BTreeMap<_, _> = baseline
        .points
        .iter()
        .map(|point| (point.coordinate(), point))
        .collect();
    let points_equal = candidate.points.iter().all(|point| {
        baseline_points
            .get(&point.coordinate())
            .is_some_and(|other| {
                point.coverage_domain == other.coverage_domain
                    && point.adapter_id == other.adapter_id
                    && point.source_instance_key == other.source_instance_key
                    && point.position == other.position
                    && point.status == other.status
            })
    });
    let candidate_absences: BTreeSet<_> = candidate.explicit_absence_or_deletion.iter().collect();
    let baseline_absences: BTreeSet<_> = baseline.explicit_absence_or_deletion.iter().collect();
    let candidate_errors: BTreeSet<_> = candidate.explicit_errors.iter().collect();
    let baseline_errors: BTreeSet<_> = baseline.explicit_errors.iter().collect();
    points_equal && candidate_absences == baseline_absences && candidate_errors == baseline_errors
}

/// Compare coverage only when both sets use the same contract, domain, scope,
/// and membership revision. Opaque positions are never compared lexically.
/// Distinct positions dominate only when the common driver supplied monotonic
/// order metadata.
pub fn compare_coverage(
    candidate: &SourceCoverageSet,
    baseline: &SourceCoverageSet,
) -> Result<CoverageComparison, SemanticContractError> {
    candidate.validate()?;
    baseline.validate()?;
    if candidate.coverage_set_contract_version != baseline.coverage_set_contract_version
        || candidate.coverage_domain != baseline.coverage_domain
        || candidate.scope != baseline.scope
        || candidate.membership_revision != baseline.membership_revision
    {
        return Ok(CoverageComparison::Incomparable);
    }
    if coverage_semantically_equal(candidate, baseline) {
        return Ok(CoverageComparison::Equal);
    }
    let candidate_dominates = set_dominates(candidate, baseline);
    let baseline_dominates = set_dominates(baseline, candidate);
    Ok(match (candidate_dominates, baseline_dominates) {
        (true, true) => CoverageComparison::Equal,
        (true, false) => CoverageComparison::Dominates,
        (false, true) => CoverageComparison::Behind,
        (false, false) => CoverageComparison::Incomparable,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn source_key() -> CanonicalSourceInstanceKey {
        CanonicalSourceInstanceKey::derive(1, b"fixture-device/root-a").unwrap()
    }

    fn coverage_set(order: u64, generation: u64) -> SourceCoverageSet {
        let source = source_key();
        let domain = CoverageDomain::FactFamily {
            family: "runtime.usage-v2".to_string(),
            version: 1,
        };
        let point = SourceCoveragePoint::new(
            domain.clone(),
            "claude-code",
            source,
            CoverageStreamKey::derive("claude-code", b"transcript").unwrap(),
            CoverageObjectKey::derive("claude-code", b"session-1.jsonl").unwrap(),
            generation,
            Some(
                CoveragePosition::derive(
                    CoveragePositionKind::AppendCursor,
                    &order.to_be_bytes(),
                    Some(order),
                )
                .unwrap(),
            ),
            CoverageStatus::CompleteThrough,
            CoverageProvenance::default(),
        )
        .unwrap();
        SourceCoverageSet::new(
            domain,
            CoverageScope {
                adapter_id: "claude-code".to_string(),
                source_instance_key: source,
                root_entity_key: None,
                support_release_id: "claude-code@fixture-v1".to_string(),
                source_or_scope_declaration_digest: CoverageDeclarationDigest::derive(
                    b"fixture-scope-v1",
                )
                .unwrap(),
            },
            CoverageMembershipRevision::derive(b"root+session-1").unwrap(),
            vec![point],
            Vec::new(),
            Vec::new(),
            CoverageSetCompleteness::Complete,
        )
        .unwrap()
    }

    fn partial_coverage_wire() -> serde_json::Value {
        let mut wire = serde_json::to_value(coverage_set(10, 1)).unwrap();
        wire["completeness"] = json!("partial");
        wire["explicit_absence_or_deletion"] = json!([{
            "stream_key": CoverageStreamKey::derive("claude-code", b"other-stream").unwrap(),
            "object_key": CoverageObjectKey::derive("claude-code", b"missing.jsonl").unwrap(),
            "generation": 2,
            "kind": "absent"
        }]);
        wire["explicit_errors"] = json!([{
            "stream_key": CoverageStreamKey::derive("claude-code", b"error-stream").unwrap(),
            "object_key": CoverageObjectKey::derive("claude-code", b"error.jsonl").unwrap(),
            "code": "retryable_read"
        }]);
        assert!(serde_json::from_value::<SourceCoverageSet>(wire.clone()).is_ok());
        wire
    }

    #[test]
    fn streaming_membership_digest_matches_legacy_and_scales_past_one_component() {
        let components: [&[u8]; 3] = [b"provider-stream", b"object-key", b"generation-one"];
        let mut encoded = Vec::new();
        for component in components {
            encoded.extend_from_slice(&(component.len() as u64).to_be_bytes());
            encoded.extend_from_slice(component);
        }
        let legacy = CoverageMembershipRevision::derive(&encoded).unwrap();
        let mut streaming = CoverageMembershipRevision::begin_streaming(encoded.len()).unwrap();
        for component in components {
            streaming
                .update(&(component.len() as u64).to_be_bytes())
                .unwrap();
            streaming.update(component).unwrap();
        }
        assert_eq!(streaming.finish().unwrap(), legacy);

        let chunk = [7_u8; 1_024];
        let total = 65 * chunk.len();
        assert!(CoverageMembershipRevision::derive(&vec![7_u8; total]).is_err());
        let mut first = CoverageMembershipRevision::begin_streaming(total).unwrap();
        let mut replay = CoverageMembershipRevision::begin_streaming(total).unwrap();
        for _ in 0..65 {
            first.update(&chunk).unwrap();
            replay.update(&chunk).unwrap();
        }
        assert_eq!(first.finish().unwrap(), replay.finish().unwrap());

        let mut incomplete = CoverageMembershipRevision::begin_streaming(2).unwrap();
        incomplete.update(b"x").unwrap();
        assert!(incomplete.finish().is_err());
        let mut overflow = CoverageMembershipRevision::begin_streaming(1).unwrap();
        assert!(overflow.update(b"xx").is_err());
    }

    #[test]
    fn external_references_are_stable_and_hide_native_identity() {
        let source = source_key();
        let first = ExternalEntityRef::new(
            CanonicalEntityKey::derive("claude-code", &source, "session", b"native-secret")
                .unwrap(),
        );
        let replay = ExternalEntityRef::new(
            CanonicalEntityKey::derive("claude-code", &source, "session", b"native-secret")
                .unwrap(),
        );
        assert_eq!(first, replay);
        let encoded = serde_json::to_string(&first).unwrap();
        assert!(!encoded.contains("native-secret"));
        assert!(!encoded.contains("fixture-device"));
        assert_eq!(
            serde_json::from_str::<ExternalEntityRef>(&encoded).unwrap(),
            first
        );
        let mut unknown_field = serde_json::to_value(first).unwrap();
        unknown_field["future_identity_meaning"] = json!(true);
        assert!(serde_json::from_value::<ExternalEntityRef>(unknown_field).is_err());
    }

    #[test]
    fn root_actor_run_identity_is_session_and_role_derived() {
        let source = source_key();
        let session =
            CanonicalEntityKey::derive("claude-code", &source, "session", b"native-secret")
                .unwrap();
        let singleton =
            CanonicalEntityKey::derive_root_actor_run("claude-code", &source, &session, None)
                .unwrap();
        assert_eq!(
            singleton,
            CanonicalEntityKey::derive_root_actor_run("claude-code", &source, &session, None)
                .unwrap()
        );
        assert_ne!(
            singleton,
            CanonicalEntityKey::derive("claude-code", &source, "run", b"native-secret").unwrap()
        );

        let other_session =
            CanonicalEntityKey::derive("claude-code", &source, "session", b"other-session")
                .unwrap();
        assert_ne!(
            singleton,
            CanonicalEntityKey::derive_root_actor_run(
                "claude-code",
                &source,
                &other_session,
                None,
            )
            .unwrap()
        );
        let declared = CanonicalEntityKey::derive_root_actor_run(
            "claude-code",
            &source,
            &session,
            Some(b"declared-run"),
        )
        .unwrap();
        assert_ne!(declared, singleton);
        assert!(CanonicalEntityKey::derive_root_actor_run(
            "claude-code",
            &source,
            &session,
            Some(b""),
        )
        .is_err());
    }

    #[test]
    fn semantic_references_are_topology_independent_revision_keys() {
        let source = source_key();
        let fact =
            CanonicalFactId::native("claude-code", &source, "runtime.usage-v2", b"message-1")
                .unwrap();
        let first = SemanticRevisionRef::new(
            FactRevisionId::derive(&fact, 1, b"response-revision-1").unwrap(),
        );
        let scoped = SemanticRevisionRef::new(
            FactRevisionId::derive(&fact, 1, b"response-revision-1").unwrap(),
        );
        let correction = SemanticRevisionRef::new(
            FactRevisionId::derive(&fact, 1, b"response-revision-2").unwrap(),
        );
        assert_eq!(first, scoped);
        assert_ne!(first, correction);
        let mut unknown_field = serde_json::to_value(first).unwrap();
        unknown_field["future_revision_meaning"] = json!(true);
        assert!(serde_json::from_value::<SemanticRevisionRef>(unknown_field).is_err());
    }

    #[test]
    fn qualified_values_reject_missing_value_reason_mismatches() {
        let known = QualifiedValue::from_parts(
            Some(0_u64),
            QualifiedValueQuality::Exact,
            "native-response".to_string(),
            ContractCompleteness::Partial,
            None,
            None,
            Vec::<SemanticRevisionRef>::new(),
        )
        .unwrap();
        assert_eq!(known.value, Some(0));
        assert!(QualifiedValue::from_parts(
            None::<u64>,
            QualifiedValueQuality::Exact,
            "native-response".to_string(),
            ContractCompleteness::Unknown,
            Some(QualifiedUnknownReason::Missing),
            None,
            Vec::<SemanticRevisionRef>::new(),
        )
        .is_err());
        assert!(serde_json::from_value::<QualifiedValue<u64>>(json!({
            "value": null,
            "quality": "unknown",
            "authority": "native-response",
            "completeness": "unknown",
            "effective_at": null,
            "provenance": []
        }))
        .is_err());
        assert!(serde_json::from_value::<QualifiedValue<u64>>(json!({
            "quality": "unknown",
            "authority": "native-response",
            "completeness": "unknown",
            "unknown_reason": "missing",
            "provenance": []
        }))
        .is_err());
        assert!(serde_json::from_value::<QualifiedValue<u64>>(json!({
            "value": 0,
            "quality": "exact",
            "authority": "native-response",
            "completeness": "complete",
            "unknown_reason": null,
            "provenance": []
        }))
        .is_err());
        assert!(serde_json::from_value::<QualifiedValue<u64>>(json!({
            "value": 0,
            "quality": "exact",
            "authority": "native-response",
            "completeness": "complete",
            "effective_at": null,
            "provenance": []
        }))
        .is_err());
        assert!(serde_json::from_value::<QualifiedValue<u64>>(json!({
            "value": null,
            "quality": "unknown",
            "authority": "native-response",
            "completeness": "unknown",
            "unknown_reason": "missing",
            "effective_at": null,
            "provenance": []
        }))
        .is_err());

        let entity_ref = ExternalEntityRef::new(
            CanonicalEntityKey::derive("claude-code", &source_key(), "session", b"s1").unwrap(),
        );
        assert!(serde_json::from_value::<NativeIdentityClaim>(json!({
            "entity_ref": entity_ref,
            "identity": {
                "value": { "native_namespace": "claude-code/session", "native_id": "" },
                "quality": "native_claimed",
                "authority": "transcript",
                "completeness": "complete",
                "provenance": []
            }
        }))
        .is_err());
    }

    #[test]
    fn incompatible_reference_and_coverage_majors_are_rejected() {
        let reference = ExternalEntityRef::new(
            CanonicalEntityKey::derive("claude-code", &source_key(), "session", b"s1").unwrap(),
        );
        let mut encoded = serde_json::to_value(reference).unwrap();
        encoded["external_entity_reference_version"] = json!(2);
        assert!(serde_json::from_value::<ExternalEntityRef>(encoded).is_err());

        let mut coverage = serde_json::to_value(coverage_set(10, 1)).unwrap();
        coverage["coverage_set_contract_version"] = json!(2);
        assert!(serde_json::from_value::<SourceCoverageSet>(coverage).is_err());
    }

    #[test]
    fn coverage_comparison_is_driver_ordered_and_generation_safe() {
        let baseline = coverage_set(10, 1);
        let equal = coverage_set(10, 1);
        let newer = coverage_set(20, 1);
        let reset = coverage_set(20, 2);
        assert_eq!(
            compare_coverage(&equal, &baseline).unwrap(),
            CoverageComparison::Equal
        );
        assert_eq!(
            compare_coverage(&newer, &baseline).unwrap(),
            CoverageComparison::Dominates
        );
        assert_eq!(
            compare_coverage(&baseline, &newer).unwrap(),
            CoverageComparison::Behind
        );
        assert_eq!(
            compare_coverage(&reset, &baseline).unwrap(),
            CoverageComparison::Incomparable
        );

        let mut partial_json = serde_json::to_value(&baseline).unwrap();
        partial_json["completeness"] = json!("partial");
        partial_json["points"][0]["status"] = json!({ "kind": "partial" });
        let partial = serde_json::from_value::<SourceCoverageSet>(partial_json).unwrap();
        assert_eq!(
            compare_coverage(&partial, &partial).unwrap(),
            CoverageComparison::Equal
        );
    }

    #[test]
    fn complete_sets_reject_partial_points_and_errors() {
        let complete = coverage_set(10, 1);
        let mut encoded = serde_json::to_value(complete).unwrap();
        encoded["points"][0]["status"] = json!({ "kind": "partial" });
        assert!(serde_json::from_value::<SourceCoverageSet>(encoded).is_err());
    }

    #[test]
    fn coverage_wire_rejects_unknown_nested_fields_and_explicit_nulls() {
        let base = partial_coverage_wire();
        type Mutation = (&'static str, Box<dyn Fn(&mut serde_json::Value)>);
        let mut mutations: Vec<Mutation> = vec![
            ("set", Box::new(|value| value["future"] = json!(true))),
            (
                "domain",
                Box::new(|value| value["coverage_domain"]["future"] = json!(true)),
            ),
            (
                "decode domain",
                Box::new(|value| {
                    value["coverage_domain"] = json!({ "kind": "decode", "future": true })
                }),
            ),
            (
                "point",
                Box::new(|value| value["points"][0]["future"] = json!(true)),
            ),
            (
                "point domain",
                Box::new(|value| value["points"][0]["coverage_domain"]["future"] = json!(true)),
            ),
            (
                "position",
                Box::new(|value| value["points"][0]["position"]["future"] = json!(true)),
            ),
            (
                "status",
                Box::new(|value| value["points"][0]["status"]["future"] = json!(true)),
            ),
            (
                "provenance",
                Box::new(|value| value["points"][0]["provenance"]["future"] = json!(true)),
            ),
            (
                "scope",
                Box::new(|value| value["scope"]["future"] = json!(true)),
            ),
            (
                "absence",
                Box::new(|value| value["explicit_absence_or_deletion"][0]["future"] = json!(true)),
            ),
            (
                "error",
                Box::new(|value| value["explicit_errors"][0]["future"] = json!(true)),
            ),
            (
                "null position",
                Box::new(|value| value["points"][0]["position"] = serde_json::Value::Null),
            ),
            (
                "null order",
                Box::new(|value| {
                    value["points"][0]["position"]["monotonic_order"] = serde_json::Value::Null
                }),
            ),
            (
                "null observed_at",
                Box::new(|value| {
                    value["points"][0]["provenance"]["observed_at"] = serde_json::Value::Null
                }),
            ),
            (
                "null source record",
                Box::new(|value| {
                    value["points"][0]["provenance"]["source_record_id"] = serde_json::Value::Null
                }),
            ),
            (
                "null root",
                Box::new(|value| value["scope"]["root_entity_key"] = serde_json::Value::Null),
            ),
            (
                "null error object",
                Box::new(|value| {
                    value["explicit_errors"][0]["object_key"] = serde_json::Value::Null
                }),
            ),
            (
                "null error stream",
                Box::new(|value| {
                    value["explicit_errors"][0]["stream_key"] = serde_json::Value::Null
                }),
            ),
        ];
        for (label, mutate) in mutations.drain(..) {
            let mut wire = base.clone();
            mutate(&mut wire);
            assert!(
                serde_json::from_value::<SourceCoverageSet>(wire).is_err(),
                "coverage mutation {label} must fail closed"
            );
        }
    }

    #[test]
    fn coverage_wire_enforces_portable_numbers_and_evidence_bounds() {
        let base = partial_coverage_wire();
        let mut zero_generation = base.clone();
        zero_generation["points"][0]["generation"] = json!(0);
        assert!(serde_json::from_value::<SourceCoverageSet>(zero_generation).is_err());
        let mut zero_absence_generation = base.clone();
        zero_absence_generation["explicit_absence_or_deletion"][0]["generation"] = json!(0);
        assert!(serde_json::from_value::<SourceCoverageSet>(zero_absence_generation).is_err());

        for path in [
            &["points", "0", "generation"][..],
            &["points", "0", "position", "monotonic_order"][..],
            &["explicit_absence_or_deletion", "0", "generation"][..],
        ] {
            let mut wire = base.clone();
            let mut target = &mut wire;
            for component in &path[..path.len() - 1] {
                target = if let Ok(index) = component.parse::<usize>() {
                    &mut target[index]
                } else {
                    &mut target[*component]
                };
            }
            target[path[path.len() - 1]] = json!(JS_SAFE_INTEGER_MAX_U64 + 1);
            assert!(serde_json::from_value::<SourceCoverageSet>(wire).is_err());
        }

        let mut observed_at = base.clone();
        observed_at["points"][0]["provenance"]["observed_at"] =
            json!(-(JS_SAFE_INTEGER_MAX_I64 + 1));
        assert!(serde_json::from_value::<SourceCoverageSet>(observed_at).is_err());

        let mut reason = base.clone();
        reason["points"][0]["status"] = json!({
            "kind": "unavailable",
            "reason": "é".repeat((MAX_COVERAGE_UNAVAILABLE_REASON_BYTES / 2) + 1)
        });
        assert!(serde_json::from_value::<SourceCoverageSet>(reason).is_err());

        let mut errors = base.clone();
        errors["explicit_errors"] = json!(vec![
            json!({ "code": "bounded" });
            MAX_COVERAGE_ERRORS_PER_SET + 1
        ]);
        assert!(serde_json::from_value::<SourceCoverageSet>(errors).is_err());

        for invalid_code in [
            "a".repeat(MAX_COVERAGE_ERROR_CODE_BYTES + 1),
            "retryable-read".to_string(),
            "read failed at /Users/alice/private\nretry".to_string(),
        ] {
            let mut error_code = base.clone();
            error_code["explicit_errors"][0]["code"] = json!(invalid_code);
            assert!(serde_json::from_value::<SourceCoverageSet>(error_code).is_err());
        }

        let mut duplicate_errors = base.clone();
        duplicate_errors["explicit_errors"] = json!([
            { "code": "duplicate" },
            { "code": "duplicate" }
        ]);
        assert!(serde_json::from_value::<SourceCoverageSet>(duplicate_errors).is_err());

        let mut orphan_object = base;
        orphan_object["explicit_errors"] = json!([{
            "object_key": CoverageObjectKey::derive("claude-code", b"orphan.jsonl").unwrap(),
            "code": "orphan-object"
        }]);
        assert!(serde_json::from_value::<SourceCoverageSet>(orphan_object).is_err());
    }

    #[test]
    fn public_coverage_leaf_wires_cannot_bypass_semantic_validation() {
        let mut position = serde_json::to_value(
            CoveragePosition::derive(
                CoveragePositionKind::AppendCursor,
                b"leaf-position",
                Some(1),
            )
            .unwrap(),
        )
        .unwrap();
        position["monotonic_order"] = json!(JS_SAFE_INTEGER_MAX_U64 + 1);
        assert!(serde_json::from_value::<CoveragePosition>(position).is_err());

        assert!(serde_json::from_value::<CoverageProvenance>(json!({
            "observed_at": JS_SAFE_INTEGER_MAX_I64 + 1
        }))
        .is_err());

        let mut scope = serde_json::to_value(coverage_set(10, 1).scope).unwrap();
        scope["adapter_id"] = json!("");
        assert!(serde_json::from_value::<CoverageScope>(scope).is_err());

        let absence = &partial_coverage_wire()["explicit_absence_or_deletion"][0];
        let mut absence = absence.clone();
        absence["generation"] = json!(0);
        assert!(serde_json::from_value::<CoverageAbsence>(absence).is_err());

        let error = &partial_coverage_wire()["explicit_errors"][0];
        let mut error = error.clone();
        error["code"] = json!("read failed at /Users/alice/private\nretry");
        assert!(serde_json::from_value::<CoverageError>(error).is_err());
        assert!(serde_json::from_value::<CoverageError>(json!({
            "object_key": CoverageObjectKey::derive("claude-code", b"orphan.jsonl").unwrap(),
            "code": "orphan_object"
        }))
        .is_err());
    }

    #[test]
    fn cross_language_fixture_is_stable() {
        let source = source_key();
        let external = ExternalEntityRef::new(
            CanonicalEntityKey::derive("claude-code", &source, "session", b"session-1").unwrap(),
        );
        let fact =
            CanonicalFactId::native("claude-code", &source, "runtime.usage-v2", b"message-1")
                .unwrap();
        let semantic = SemanticRevisionRef::new(
            FactRevisionId::derive(&fact, 1, b"response-revision-1").unwrap(),
        );
        let native_identity_claim = NativeIdentityClaim::new(
            external,
            QualifiedValue::from_parts(
                Some(NativeIdentity {
                    native_namespace: "claude-code/session".to_string(),
                    native_id: "session-1".to_string(),
                }),
                QualifiedValueQuality::NativeClaimed,
                "transcript".to_string(),
                ContractCompleteness::Complete,
                None,
                None,
                vec![semantic],
            )
            .unwrap(),
        )
        .unwrap();
        let known = QualifiedValue::from_parts(
            Some(0_u64),
            QualifiedValueQuality::Exact,
            "native-response".to_string(),
            ContractCompleteness::Partial,
            None,
            Some(1_776_211_200_000),
            vec![semantic],
        )
        .unwrap();
        let unknown = QualifiedValue::from_parts(
            None::<String>,
            QualifiedValueQuality::Unknown,
            "native-response".to_string(),
            ContractCompleteness::Unknown,
            Some(QualifiedUnknownReason::Withheld),
            None,
            Vec::<SemanticRevisionRef>::new(),
        )
        .unwrap();
        let fixture = json!({
            "fixture_contract_version": 1,
            "canonical_source_instance_key": source,
            "external_entity_ref": external,
            "native_identity_claim": native_identity_claim,
            "semantic_revision_ref": semantic,
            "qualified_known_zero": known,
            "qualified_unknown": unknown,
            "coverage": {
                "baseline": coverage_set(10, 1),
                "dominant": coverage_set(20, 1),
                "reset": coverage_set(20, 2),
                "expected": {
                    "dominant_vs_baseline": "dominates",
                    "baseline_vs_dominant": "behind",
                    "reset_vs_baseline": "incomparable"
                }
            }
        });
        let expected = serde_json::from_str::<serde_json::Value>(include_str!(
            "../../fixtures/contracts/rfc012a-v1.json"
        ))
        .unwrap();
        assert_eq!(fixture, expected);
    }
}
