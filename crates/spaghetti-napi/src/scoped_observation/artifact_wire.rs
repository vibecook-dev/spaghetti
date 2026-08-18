//! Attachment-bound RFC 012D bounded-artifact wire contract.
//!
//! This module freezes the portable request/result shape without granting any
//! native locator or source-read authority. A request can be minted only from
//! an existing authorized attachment and contains no path. A future common
//! mediator must still consume an exact `ArtifactLocatorFromEvidence`
//! reservation before it may construct an outcome. Portable consumption is
//! bound to the caller-held in-process command; neither the context wire nor a
//! response can authorize access by itself.

use std::fmt;
use std::sync::Arc;

use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine as _;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value as JsonValue;

use crate::adapter::{CanonicalEntityKey, Sha256Digest};
use crate::observation_contract::ObservationContractSelection;

use super::{
    ScopedObservationAccessHost, ScopedObservationAttachmentAuthority,
    ScopedObservationRootIdentity,
};

pub(crate) const SCOPED_ARTIFACT_CONTRACT_VERSION: u32 = 1;

const DIGEST_BYTES: usize = 32;
const REFERENCE_PREFIX: &str = "v1:";
const MAX_ARTIFACT_KIND_BYTES: usize = 128;
const MAX_ARTIFACT_REQUEST_BYTES: u64 = 2_147_483_648;
const MAX_INLINE_ARTIFACT_BYTES: u64 = 8 * 1024 * 1024;
const MAX_INLINE_BASE64_BYTES: usize = 11_184_812;
const JS_SAFE_INTEGER_MAX: u64 = 9_007_199_254_740_991;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ScopedArtifactContractError {
    #[error("invalid scoped artifact contract: {message}")]
    Invalid { message: String },
    #[error("scoped artifact command belongs to another observer attachment")]
    ForeignCommand,
    #[error("scoped artifact request cannot be prepared after attachment close")]
    Closed,
}

impl ScopedArtifactContractError {
    fn invalid(message: impl fmt::Display) -> Self {
        Self::Invalid {
            message: message.to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ScopedArtifactContentPolicy {
    MetadataOnly,
    HashOnly,
    Inline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ScopedArtifactUnavailableReason {
    OutOfScope,
    Denied,
    Missing,
    OverLimit,
    ChangedGeneration,
    Unsupported,
    Malformed,
    Unstable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ScopedArtifactCompletenessWire {
    Complete,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ScopedArtifactLocatorDisclosureWire {
    Withheld,
}

/// One native mediator result. Content is deliberately redacted from Debug;
/// constructing this value does not itself perform or authorize a read.
pub(crate) enum ScopedArtifactReadOutcome {
    Available {
        generation: u64,
        provenance_ref: [u8; DIGEST_BYTES],
        size_bytes: u64,
        content_hash: Option<Sha256Digest>,
        content: Option<Vec<u8>>,
    },
    Unavailable {
        reason: ScopedArtifactUnavailableReason,
        observed_generation: Option<u64>,
        observed_bytes: Option<u64>,
        provenance_ref: Option<[u8; DIGEST_BYTES]>,
    },
}

impl fmt::Debug for ScopedArtifactReadOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Available {
                generation,
                size_bytes,
                content_hash,
                content,
                ..
            } => formatter
                .debug_struct("Available")
                .field("generation", generation)
                .field("size_bytes", size_bytes)
                .field("has_content_hash", &content_hash.is_some())
                .field("has_inline_content", &content.is_some())
                .finish(),
            Self::Unavailable {
                reason,
                observed_generation,
                observed_bytes,
                provenance_ref,
            } => formatter
                .debug_struct("Unavailable")
                .field("reason", reason)
                .field("observed_generation", observed_generation)
                .field("observed_bytes", observed_bytes)
                .field("has_provenance", &provenance_ref.is_some())
                .finish(),
        }
    }
}

/// In-process authority retained beside the portable request context. The
/// opaque references are correlation only; Rust continues to rely on `Arc`
/// identity when matching the command to its attachment.
#[derive(Clone)]
pub(crate) struct ScopedArtifactReadCommand {
    attachment_authority: Arc<ScopedObservationAttachmentAuthority>,
    contract_selection: ObservationContractSelection,
    root: ScopedObservationRootIdentity,
    artifact_key: CanonicalEntityKey,
    artifact_kind: String,
    expected_generation: Option<u64>,
    max_bytes: u64,
    content_policy: ScopedArtifactContentPolicy,
    attachment_ref: [u8; DIGEST_BYTES],
    request_id: [u8; DIGEST_BYTES],
}

struct ScopedArtifactReadParameters {
    artifact_key: CanonicalEntityKey,
    artifact_kind: String,
    expected_generation: Option<u64>,
    max_bytes: u64,
    content_policy: ScopedArtifactContentPolicy,
}

impl fmt::Debug for ScopedArtifactReadCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScopedArtifactReadCommand")
            .field("artifact_kind", &self.artifact_kind)
            .field("expected_generation", &self.expected_generation)
            .field("max_bytes", &self.max_bytes)
            .field("content_policy", &self.content_policy)
            .field("attachment_ref", &encode_opaque(&self.attachment_ref))
            .field("request_id", &encode_opaque(&self.request_id))
            .finish_non_exhaustive()
    }
}

impl ScopedArtifactReadCommand {
    fn new(
        attachment_authority: Arc<ScopedObservationAttachmentAuthority>,
        contract_selection: ObservationContractSelection,
        root: ScopedObservationRootIdentity,
        parameters: ScopedArtifactReadParameters,
    ) -> Result<Self, ScopedArtifactContractError> {
        let ScopedArtifactReadParameters {
            artifact_key,
            artifact_kind,
            expected_generation,
            max_bytes,
            content_policy,
        } = parameters;
        validate_identifier("artifact kind", &artifact_kind, MAX_ARTIFACT_KIND_BYTES)?;
        if artifact_key.as_bytes().iter().all(|byte| *byte == 0) {
            return Err(ScopedArtifactContractError::invalid(
                "artifact key must not be the zero reference",
            ));
        }
        validate_positive_portable("artifact max bytes", max_bytes)?;
        if max_bytes > MAX_ARTIFACT_REQUEST_BYTES {
            return Err(ScopedArtifactContractError::invalid(
                "artifact max bytes exceeds the portable request safety bound",
            ));
        }
        if content_policy == ScopedArtifactContentPolicy::Inline
            && max_bytes > MAX_INLINE_ARTIFACT_BYTES
        {
            return Err(ScopedArtifactContractError::invalid(
                "inline artifact max bytes exceeds the portable inline safety bound",
            ));
        }
        if let Some(generation) = expected_generation {
            validate_positive_portable("artifact expected generation", generation)?;
        }
        if attachment_authority.token == 0 {
            return Err(ScopedArtifactContractError::invalid(
                "attachment correlation token must be positive",
            ));
        }
        let root_wire = ScopedArtifactRootWire::from_root(&root);
        let attachment_ref =
            derive_attachment_ref(attachment_authority.token, &contract_selection, &root_wire)?;
        let request_id = derive_request_id(
            &attachment_ref,
            &artifact_key,
            &artifact_kind,
            expected_generation,
            max_bytes,
            content_policy,
        );
        Ok(Self {
            attachment_authority,
            contract_selection,
            root,
            artifact_key,
            artifact_kind,
            expected_generation,
            max_bytes,
            content_policy,
            attachment_ref,
            request_id,
        })
    }

    fn matches_host(&self, host: &ScopedObservationAccessHost) -> bool {
        Arc::ptr_eq(&self.attachment_authority, &host.attachment_authority)
            && self.contract_selection == host.observation_contract
            && self.root == host.root_identity
    }

    pub(crate) fn context_wire(&self) -> ScopedArtifactReadContextWire {
        ScopedArtifactReadContextWire::from_command(self)
    }

    pub(crate) fn observed(
        &self,
        outcome: ScopedArtifactReadOutcome,
    ) -> Result<ScopedObservedArtifactWire, ScopedArtifactContractError> {
        ScopedObservedArtifactWire::from_outcome(self, outcome)
    }

    pub(crate) fn parse_observed(
        &self,
        value: JsonValue,
    ) -> Result<ScopedObservedArtifactWire, ScopedArtifactContractError> {
        ScopedObservedArtifactWire::from_wire_value_for_command(value, self)
    }
}

impl ScopedObservationAccessHost {
    /// Prepare a path-free portable artifact request bound to this attachment.
    /// This performs no native access and is not a substitute for the future
    /// evidence-derived locator authorization.
    pub(crate) fn prepare_portable_artifact_read(
        &self,
        artifact_key: CanonicalEntityKey,
        artifact_kind: impl Into<String>,
        expected_generation: Option<u64>,
        max_bytes: u64,
        content_policy: ScopedArtifactContentPolicy,
    ) -> Result<ScopedArtifactReadCommand, ScopedArtifactContractError> {
        if self.state.closed.load(std::sync::atomic::Ordering::Acquire) {
            return Err(ScopedArtifactContractError::Closed);
        }
        ScopedArtifactReadCommand::new(
            Arc::clone(&self.attachment_authority),
            self.observation_contract.clone(),
            self.root_identity.clone(),
            ScopedArtifactReadParameters {
                artifact_key,
                artifact_kind: artifact_kind.into(),
                expected_generation,
                max_bytes,
                content_policy,
            },
        )
    }

    pub(crate) fn validate_portable_artifact_command(
        &self,
        command: &ScopedArtifactReadCommand,
    ) -> Result<(), ScopedArtifactContractError> {
        if self.state.closed.load(std::sync::atomic::Ordering::Acquire) {
            return Err(ScopedArtifactContractError::Closed);
        }
        if command.matches_host(self) {
            Ok(())
        } else {
            Err(ScopedArtifactContractError::ForeignCommand)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopedArtifactRootWire {
    adapter_id: String,
    source_instance_key: crate::adapter::CanonicalSourceInstanceKey,
    session_ref: crate::adapter::ExternalEntityRef,
    session_key: CanonicalEntityKey,
    root_actor_run_key: CanonicalEntityKey,
    #[serde(deserialize_with = "deserialize_required_option")]
    native_session_claim: Option<crate::adapter::NativeIdentityClaim>,
}

impl ScopedArtifactRootWire {
    fn from_root(root: &ScopedObservationRootIdentity) -> Self {
        Self {
            adapter_id: root.adapter_id.as_str().to_owned(),
            source_instance_key: root.source_instance_key,
            session_ref: root.session_ref,
            session_key: root.session_key,
            root_actor_run_key: root.root_actor_run_key,
            native_session_claim: root.native_session_claim.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ScopedArtifactReadContextWire {
    contract_selection: ObservationContractSelection,
    root: ScopedArtifactRootWire,
    attachment_ref: String,
    request_id: String,
    artifact_key: CanonicalEntityKey,
    artifact_kind: String,
    expected_generation: Option<u64>,
    max_bytes: u64,
    content_policy: ScopedArtifactContentPolicy,
}

impl ScopedArtifactReadContextWire {
    fn from_command(command: &ScopedArtifactReadCommand) -> Self {
        Self {
            contract_selection: command.contract_selection.clone(),
            root: ScopedArtifactRootWire::from_root(&command.root),
            attachment_ref: encode_opaque(&command.attachment_ref),
            request_id: encode_opaque(&command.request_id),
            artifact_key: command.artifact_key,
            artifact_kind: command.artifact_kind.clone(),
            expected_generation: command.expected_generation,
            max_bytes: command.max_bytes,
            content_policy: command.content_policy,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopedArtifactReadContextInput {
    contract_selection: JsonValue,
    root: ScopedArtifactRootWire,
    attachment_ref: String,
    request_id: String,
    artifact_key: CanonicalEntityKey,
    artifact_kind: String,
    #[serde(deserialize_with = "deserialize_required_option")]
    expected_generation: Option<u64>,
    max_bytes: u64,
    content_policy: ScopedArtifactContentPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum ScopedArtifactOutcomeWire {
    Available {
        generation: u64,
        provenance_ref: String,
        size_bytes: u64,
        completeness: ScopedArtifactCompletenessWire,
        #[serde(deserialize_with = "deserialize_required_option")]
        content_hash: Option<String>,
        #[serde(deserialize_with = "deserialize_required_option")]
        content_base64: Option<String>,
    },
    Unavailable {
        reason: ScopedArtifactUnavailableReason,
        #[serde(deserialize_with = "deserialize_required_option")]
        observed_generation: Option<u64>,
        #[serde(deserialize_with = "deserialize_required_option")]
        observed_bytes: Option<u64>,
        #[serde(deserialize_with = "deserialize_required_option")]
        provenance_ref: Option<String>,
        completeness: ScopedArtifactCompletenessWire,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ScopedObservedArtifactWire {
    scoped_artifact_contract_version: u32,
    request: ScopedArtifactReadContextWire,
    locator_disclosure: ScopedArtifactLocatorDisclosureWire,
    outcome: ScopedArtifactOutcomeWire,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopedObservedArtifactInput {
    scoped_artifact_contract_version: u32,
    request: JsonValue,
    locator_disclosure: ScopedArtifactLocatorDisclosureWire,
    outcome: ScopedArtifactOutcomeWire,
}

impl ScopedObservedArtifactWire {
    fn from_outcome(
        command: &ScopedArtifactReadCommand,
        outcome: ScopedArtifactReadOutcome,
    ) -> Result<Self, ScopedArtifactContractError> {
        let outcome = match outcome {
            ScopedArtifactReadOutcome::Available {
                generation,
                provenance_ref,
                size_bytes,
                content_hash,
                content,
            } => {
                validate_available_raw(
                    command,
                    generation,
                    size_bytes,
                    content_hash.as_ref(),
                    content.as_deref(),
                )?;
                ScopedArtifactOutcomeWire::Available {
                    generation,
                    provenance_ref: encode_opaque(&provenance_ref),
                    size_bytes,
                    completeness: ScopedArtifactCompletenessWire::Complete,
                    content_hash: content_hash.map(|value| value.to_string()),
                    content_base64: content.map(|value| STANDARD.encode(value)),
                }
            }
            ScopedArtifactReadOutcome::Unavailable {
                reason,
                observed_generation,
                observed_bytes,
                provenance_ref,
            } => ScopedArtifactOutcomeWire::Unavailable {
                reason,
                observed_generation,
                observed_bytes,
                provenance_ref: provenance_ref.map(|value| encode_opaque(&value)),
                completeness: ScopedArtifactCompletenessWire::Unavailable,
            },
        };
        let value = Self {
            scoped_artifact_contract_version: SCOPED_ARTIFACT_CONTRACT_VERSION,
            request: command.context_wire(),
            locator_disclosure: ScopedArtifactLocatorDisclosureWire::Withheld,
            outcome,
        };
        value.validate_for_command(command)?;
        Ok(value)
    }

    pub(crate) fn from_wire_value_for_command(
        value: JsonValue,
        expected: &ScopedArtifactReadCommand,
    ) -> Result<Self, ScopedArtifactContractError> {
        preflight_wire_value(&value)?;
        let input: ScopedObservedArtifactInput = serde_json::from_value(value)
            .map_err(|error| ScopedArtifactContractError::invalid(error.to_string()))?;
        let request = parse_context_for_command(input.request, expected)?;
        let value = Self {
            scoped_artifact_contract_version: input.scoped_artifact_contract_version,
            request,
            locator_disclosure: input.locator_disclosure,
            outcome: input.outcome,
        };
        value.validate_for_command(expected)?;
        Ok(value)
    }

    fn validate_for_command(
        &self,
        expected: &ScopedArtifactReadCommand,
    ) -> Result<(), ScopedArtifactContractError> {
        if self.scoped_artifact_contract_version != SCOPED_ARTIFACT_CONTRACT_VERSION
            || self.locator_disclosure != ScopedArtifactLocatorDisclosureWire::Withheld
            || self.request != expected.context_wire()
        {
            return Err(ScopedArtifactContractError::ForeignCommand);
        }
        match &self.outcome {
            ScopedArtifactOutcomeWire::Available {
                generation,
                provenance_ref,
                size_bytes,
                completeness,
                content_hash,
                content_base64,
            } => {
                validate_positive_portable("artifact generation", *generation)?;
                if expected
                    .expected_generation
                    .is_some_and(|value| value != *generation)
                {
                    return Err(ScopedArtifactContractError::invalid(
                        "available artifact generation differs from the expected generation",
                    ));
                }
                decode_opaque_exact(provenance_ref, "artifact provenance reference")?;
                validate_portable("artifact size", *size_bytes)?;
                if *size_bytes > expected.max_bytes
                    || *completeness != ScopedArtifactCompletenessWire::Complete
                {
                    return Err(ScopedArtifactContractError::invalid(
                        "available artifact exceeds the request or is not complete",
                    ));
                }
                validate_available_content(
                    expected.content_policy,
                    *size_bytes,
                    content_hash.as_deref(),
                    content_base64.as_deref(),
                )?;
            }
            ScopedArtifactOutcomeWire::Unavailable {
                reason,
                observed_generation,
                observed_bytes,
                provenance_ref,
                completeness,
            } => {
                if *completeness != ScopedArtifactCompletenessWire::Unavailable {
                    return Err(ScopedArtifactContractError::invalid(
                        "unavailable artifact must report unavailable completeness",
                    ));
                }
                if let Some(generation) = observed_generation {
                    validate_positive_portable("observed artifact generation", *generation)?;
                }
                if let Some(bytes) = observed_bytes {
                    validate_portable("observed artifact bytes", *bytes)?;
                }
                if provenance_ref.is_some() != observed_generation.is_some() {
                    return Err(ScopedArtifactContractError::invalid(
                        "artifact provenance and observed generation must be present together",
                    ));
                }
                if let Some(value) = provenance_ref {
                    decode_opaque_exact(value, "artifact provenance reference")?;
                }
                match reason {
                    ScopedArtifactUnavailableReason::ChangedGeneration => {
                        let expected_generation =
                            expected.expected_generation.ok_or_else(|| {
                                ScopedArtifactContractError::invalid(
                                    "changed-generation requires a caller-held expected generation",
                                )
                            })?;
                        if observed_generation.is_none_or(|value| value == expected_generation)
                            || observed_bytes.is_some()
                        {
                            return Err(ScopedArtifactContractError::invalid(
                                "changed-generation requires a distinct observed generation and no byte claim",
                            ));
                        }
                    }
                    ScopedArtifactUnavailableReason::OverLimit => {
                        if observed_bytes.is_none_or(|value| value <= expected.max_bytes) {
                            return Err(ScopedArtifactContractError::invalid(
                                "over-limit requires observed bytes above the request maximum",
                            ));
                        }
                    }
                    _ if observed_bytes.is_some() => {
                        return Err(ScopedArtifactContractError::invalid(
                            "only over-limit may report observed artifact bytes",
                        ));
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }
}

fn validate_available_raw(
    command: &ScopedArtifactReadCommand,
    generation: u64,
    size_bytes: u64,
    content_hash: Option<&Sha256Digest>,
    content: Option<&[u8]>,
) -> Result<(), ScopedArtifactContractError> {
    validate_positive_portable("artifact generation", generation)?;
    validate_portable("artifact size", size_bytes)?;
    if command
        .expected_generation
        .is_some_and(|expected| expected != generation)
        || size_bytes > command.max_bytes
    {
        return Err(ScopedArtifactContractError::invalid(
            "available artifact does not match the requested generation or byte bound",
        ));
    }
    match command.content_policy {
        ScopedArtifactContentPolicy::MetadataOnly
            if content_hash.is_none() && content.is_none() => {}
        ScopedArtifactContentPolicy::HashOnly if content_hash.is_some() && content.is_none() => {}
        ScopedArtifactContentPolicy::Inline => {
            let content = content.ok_or_else(|| {
                ScopedArtifactContractError::invalid(
                    "inline artifact response requires content before encoding",
                )
            })?;
            if size_bytes > MAX_INLINE_ARTIFACT_BYTES
                || u64::try_from(content.len()).ok() != Some(size_bytes)
                || content_hash.is_none_or(|expected| Sha256Digest::of(content) != *expected)
            {
                return Err(ScopedArtifactContractError::invalid(
                    "inline artifact content, size, or hash is inconsistent",
                ));
            }
        }
        _ => {
            return Err(ScopedArtifactContractError::invalid(
                "available artifact fields do not match the requested content policy",
            ));
        }
    }
    Ok(())
}

fn preflight_wire_value(value: &JsonValue) -> Result<(), ScopedArtifactContractError> {
    let artifact_kind = value
        .as_object()
        .and_then(|input| input.get("request"))
        .and_then(JsonValue::as_object)
        .and_then(|request| request.get("artifact_kind"));
    if let Some(JsonValue::String(kind)) = artifact_kind {
        if kind.len() > MAX_ARTIFACT_KIND_BYTES {
            return Err(ScopedArtifactContractError::invalid(
                "artifact kind exceeds the pre-decode safety bound",
            ));
        }
    }
    let content = value
        .as_object()
        .and_then(|input| input.get("outcome"))
        .and_then(JsonValue::as_object)
        .and_then(|outcome| outcome.get("content_base64"));
    if let Some(JsonValue::String(encoded)) = content {
        if encoded.len() > MAX_INLINE_BASE64_BYTES {
            return Err(ScopedArtifactContractError::invalid(
                "inline artifact base64 exceeds the pre-decode safety bound",
            ));
        }
    }
    Ok(())
}

fn parse_context_for_command(
    value: JsonValue,
    expected: &ScopedArtifactReadCommand,
) -> Result<ScopedArtifactReadContextWire, ScopedArtifactContractError> {
    let input: ScopedArtifactReadContextInput = serde_json::from_value(value)
        .map_err(|error| ScopedArtifactContractError::invalid(error.to_string()))?;
    let contract_selection = ObservationContractSelection::from_wire_value_for_expected(
        input.contract_selection,
        &expected.contract_selection,
    )
    .map_err(ScopedArtifactContractError::invalid)?;
    let context = ScopedArtifactReadContextWire {
        contract_selection,
        root: input.root,
        attachment_ref: input.attachment_ref,
        request_id: input.request_id,
        artifact_key: input.artifact_key,
        artifact_kind: input.artifact_kind,
        expected_generation: input.expected_generation,
        max_bytes: input.max_bytes,
        content_policy: input.content_policy,
    };
    decode_opaque_exact(&context.attachment_ref, "artifact attachment reference")?;
    decode_opaque_exact(&context.request_id, "artifact request id")?;
    if context != expected.context_wire() {
        return Err(ScopedArtifactContractError::ForeignCommand);
    }
    Ok(context)
}

fn validate_available_content(
    policy: ScopedArtifactContentPolicy,
    size_bytes: u64,
    content_hash: Option<&str>,
    content_base64: Option<&str>,
) -> Result<(), ScopedArtifactContractError> {
    match policy {
        ScopedArtifactContentPolicy::MetadataOnly => {
            if content_hash.is_some() || content_base64.is_some() {
                return Err(ScopedArtifactContractError::invalid(
                    "metadata-only artifact response cannot disclose hash or content",
                ));
            }
        }
        ScopedArtifactContentPolicy::HashOnly => {
            parse_sha256(content_hash.ok_or_else(|| {
                ScopedArtifactContractError::invalid(
                    "hash-only artifact response requires a content hash",
                )
            })?)?;
            if content_base64.is_some() {
                return Err(ScopedArtifactContractError::invalid(
                    "hash-only artifact response cannot disclose inline content",
                ));
            }
        }
        ScopedArtifactContentPolicy::Inline => {
            if size_bytes > MAX_INLINE_ARTIFACT_BYTES {
                return Err(ScopedArtifactContractError::invalid(
                    "inline artifact exceeds the portable inline safety bound",
                ));
            }
            let expected_hash = parse_sha256(content_hash.ok_or_else(|| {
                ScopedArtifactContractError::invalid(
                    "inline artifact response requires a content hash",
                )
            })?)?;
            let encoded = content_base64.ok_or_else(|| {
                ScopedArtifactContractError::invalid(
                    "inline artifact response requires canonical base64 content",
                )
            })?;
            if encoded.len() > MAX_INLINE_BASE64_BYTES {
                return Err(ScopedArtifactContractError::invalid(
                    "inline artifact base64 exceeds the pre-decode safety bound",
                ));
            }
            let decoded = STANDARD.decode(encoded).map_err(|_| {
                ScopedArtifactContractError::invalid("inline artifact content is not base64")
            })?;
            if STANDARD.encode(&decoded) != encoded
                || u64::try_from(decoded.len()).ok() != Some(size_bytes)
                || Sha256Digest::of(&decoded) != expected_hash
            {
                return Err(ScopedArtifactContractError::invalid(
                    "inline artifact content, size, or hash is inconsistent",
                ));
            }
        }
    }
    Ok(())
}

fn derive_attachment_ref(
    attachment_token: u64,
    selection: &ObservationContractSelection,
    root: &ScopedArtifactRootWire,
) -> Result<[u8; DIGEST_BYTES], ScopedArtifactContractError> {
    let selection = serde_json::to_vec(selection).map_err(ScopedArtifactContractError::invalid)?;
    let root = serde_json::to_vec(root).map_err(ScopedArtifactContractError::invalid)?;
    let mut hasher = blake3::Hasher::new();
    hash_part(&mut hasher, b"spaghetti.rfc012d.artifact-attachment.v1");
    hash_part(&mut hasher, &attachment_token.to_be_bytes());
    hash_part(&mut hasher, &selection);
    hash_part(&mut hasher, &root);
    Ok(*hasher.finalize().as_bytes())
}

fn derive_request_id(
    attachment_ref: &[u8; DIGEST_BYTES],
    artifact_key: &CanonicalEntityKey,
    artifact_kind: &str,
    expected_generation: Option<u64>,
    max_bytes: u64,
    content_policy: ScopedArtifactContentPolicy,
) -> [u8; DIGEST_BYTES] {
    let mut hasher = blake3::Hasher::new();
    hash_part(&mut hasher, b"spaghetti.rfc012d.artifact-request.v1");
    hash_part(&mut hasher, attachment_ref);
    hash_part(&mut hasher, artifact_key.as_bytes());
    hash_part(&mut hasher, artifact_kind.as_bytes());
    match expected_generation {
        Some(generation) => {
            hash_part(&mut hasher, &[1]);
            hash_part(&mut hasher, &generation.to_be_bytes());
        }
        None => hash_part(&mut hasher, &[0]),
    }
    hash_part(&mut hasher, &max_bytes.to_be_bytes());
    hash_part(
        &mut hasher,
        &[match content_policy {
            ScopedArtifactContentPolicy::MetadataOnly => 1,
            ScopedArtifactContentPolicy::HashOnly => 2,
            ScopedArtifactContentPolicy::Inline => 3,
        }],
    );
    *hasher.finalize().as_bytes()
}

fn hash_part(hasher: &mut blake3::Hasher, bytes: &[u8]) {
    hasher.update(&(bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
}

fn validate_identifier(
    label: &str,
    value: &str,
    max_bytes: usize,
) -> Result<(), ScopedArtifactContractError> {
    if value.is_empty()
        || value.len() > max_bytes
        || value.trim() != value
        || value.bytes().any(|byte| {
            !(byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'.' | b'_' | b'-'))
        })
        || !value.as_bytes()[0].is_ascii_lowercase()
    {
        return Err(ScopedArtifactContractError::invalid(format!(
            "{label} is not a bounded canonical identifier"
        )));
    }
    Ok(())
}

fn validate_positive_portable(label: &str, value: u64) -> Result<(), ScopedArtifactContractError> {
    if value == 0 {
        return Err(ScopedArtifactContractError::invalid(format!(
            "{label} must be positive"
        )));
    }
    validate_portable(label, value)
}

fn validate_portable(label: &str, value: u64) -> Result<(), ScopedArtifactContractError> {
    if value > JS_SAFE_INTEGER_MAX {
        return Err(ScopedArtifactContractError::invalid(format!(
            "{label} exceeds the portable integer range"
        )));
    }
    Ok(())
}

fn parse_sha256(value: &str) -> Result<Sha256Digest, ScopedArtifactContractError> {
    Sha256Digest::parse(value).map_err(ScopedArtifactContractError::invalid)
}

fn encode_opaque(bytes: &[u8]) -> String {
    format!("{REFERENCE_PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes))
}

fn decode_opaque_exact(
    value: &str,
    label: &str,
) -> Result<[u8; DIGEST_BYTES], ScopedArtifactContractError> {
    let encoded = value.strip_prefix(REFERENCE_PREFIX).ok_or_else(|| {
        ScopedArtifactContractError::invalid(format!("{label} is not a v1 reference"))
    })?;
    if encoded.is_empty() || encoded.contains('=') {
        return Err(ScopedArtifactContractError::invalid(format!(
            "{label} is not canonical base64url"
        )));
    }
    let decoded = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| {
        ScopedArtifactContractError::invalid(format!("{label} is not canonical base64url"))
    })?;
    let decoded: [u8; DIGEST_BYTES] = decoded.try_into().map_err(|_| {
        ScopedArtifactContractError::invalid(format!("{label} must contain exactly 32 bytes"))
    })?;
    if decoded.iter().all(|byte| *byte == 0) || encode_opaque(&decoded) != value {
        return Err(ScopedArtifactContractError::invalid(format!(
            "{label} is not canonical nonzero base64url"
        )));
    }
    Ok(decoded)
}

fn deserialize_required_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

#[cfg(test)]
mod tests;
