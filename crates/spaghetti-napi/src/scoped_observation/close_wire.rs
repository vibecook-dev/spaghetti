//! Attachment-bound RFC 012D close acknowledgement.
//!
//! The public API remains `close() -> Future<void>`. This crate-private wire
//! receipt is the transport proof behind that future: it can be produced only
//! after the existing two-part cancellation barrier has observed zero owned
//! operations/watcher tasks and the exact attachment-owned consumer drain has
//! closed. Internal counters are deliberately not serialized as portable trust
//! claims. Consumption requires the caller-held command, including its exact
//! in-process attachment authority, negotiated selection, and root.

use std::sync::Arc;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value as JsonValue;

use crate::adapter::{
    CanonicalEntityKey, CanonicalSourceInstanceKey, ExternalEntityRef, NativeIdentityClaim,
};
use crate::observation_contract::ObservationContractSelection;

use super::{
    ScopedObservationAccessHost, ScopedObservationAttachmentAuthority,
    ScopedObservationCloseBarrier, ScopedObservationCloseError, ScopedObservationCloseState,
    ScopedObservationConsumerDrain, ScopedObservationRootIdentity,
};

pub(crate) const SCOPED_CLOSE_RECEIPT_CONTRACT_VERSION: u32 = 1;

const DIGEST_BYTES: usize = 32;
const MAX_ROOT_PROVENANCE: usize = 64;
const REFERENCE_PREFIX: &str = "v1:";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ScopedCloseContractError {
    #[error("invalid scoped close contract: {message}")]
    Invalid { message: String },
    #[error("scoped close command belongs to another observer attachment")]
    ForeignCommand,
    #[error("scoped consumer drain belongs to another observer attachment")]
    ForeignDrain,
    #[error("scoped close acknowledgement cannot be emitted before barrier completion")]
    NotComplete,
}

impl ScopedCloseContractError {
    fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid {
            message: message.into(),
        }
    }
}

/// In-memory close command retained by the future observer facade. The opaque
/// wire references correlate transport messages; only this `Arc` authority is
/// accepted as attachment authorization inside Rust.
#[derive(Clone)]
pub(crate) struct ScopedCloseCommand {
    attachment_authority: Arc<ScopedObservationAttachmentAuthority>,
    contract_selection: ObservationContractSelection,
    root: ScopedObservationRootIdentity,
    attachment_ref: [u8; DIGEST_BYTES],
    close_request_id: [u8; DIGEST_BYTES],
}

impl std::fmt::Debug for ScopedCloseCommand {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ScopedCloseCommand")
            .field("attachment_ref", &encode_opaque(&self.attachment_ref))
            .field("close_request_id", &encode_opaque(&self.close_request_id))
            .finish_non_exhaustive()
    }
}

impl ScopedCloseCommand {
    fn new(
        attachment_authority: Arc<ScopedObservationAttachmentAuthority>,
        contract_selection: ObservationContractSelection,
        root: ScopedObservationRootIdentity,
    ) -> Result<Self, ScopedCloseContractError> {
        if attachment_authority.token == 0 {
            return Err(ScopedCloseContractError::invalid(
                "attachment correlation token must be positive",
            ));
        }
        let attachment_ref =
            derive_attachment_ref(attachment_authority.token, &contract_selection, &root)?;
        let close_request_id = derive_close_request_id(&attachment_ref);
        Ok(Self {
            attachment_authority,
            contract_selection,
            root,
            attachment_ref,
            close_request_id,
        })
    }

    fn matches_host(
        &self,
        host: &ScopedObservationAccessHost,
    ) -> Result<bool, ScopedCloseContractError> {
        if !Arc::ptr_eq(&self.attachment_authority, &host.attachment_authority)
            || self.contract_selection != host.observation_contract
            || self.root != host.root_identity
        {
            return Ok(false);
        }
        let attachment_ref = derive_attachment_ref(
            host.attachment_authority.token,
            &host.observation_contract,
            &host.root_identity,
        )?;
        Ok(self.attachment_ref == attachment_ref
            && self.close_request_id == derive_close_request_id(&attachment_ref))
    }

    pub(crate) fn context_wire(&self) -> ScopedCloseContextWire {
        ScopedCloseContextWire::from_command(self)
    }
}

/// One exact host/consumer close operation. Retaining the command beside the
/// barrier prevents a completed barrier from another attachment from minting
/// this receipt.
pub(crate) struct ScopedObservationCloseOperation {
    command: ScopedCloseCommand,
    barrier: ScopedObservationCloseBarrier,
}

impl ScopedObservationCloseOperation {
    pub(crate) fn context_wire(&self) -> ScopedCloseContextWire {
        self.command.context_wire()
    }

    pub(crate) fn barrier(&self) -> ScopedObservationCloseBarrier {
        self.barrier.clone()
    }

    pub(crate) fn state(&self) -> ScopedObservationCloseState {
        self.barrier.state()
    }

    pub(crate) fn receipt_if_complete(
        &self,
    ) -> Result<ScopedCloseReceiptWire, ScopedCloseContractError> {
        ScopedCloseReceiptWire::from_completed_command(&self.command, self.barrier.state())
    }

    pub(crate) fn parse_receipt(
        &self,
        value: JsonValue,
    ) -> Result<ScopedCloseReceiptWire, ScopedCloseContractError> {
        ScopedCloseReceiptWire::from_wire_value_for_operation(value, self)
    }

    pub(crate) fn wait(&self) -> Result<ScopedCloseReceiptWire, ScopedCloseContractError> {
        ScopedCloseReceiptWire::from_completed_command(&self.command, self.barrier.wait())
    }

    pub(crate) async fn wait_async(
        &self,
    ) -> Result<ScopedCloseReceiptWire, ScopedCloseContractError> {
        ScopedCloseReceiptWire::from_completed_command(
            &self.command,
            self.barrier.wait_async().await,
        )
    }
}

impl ScopedObservationAccessHost {
    /// Mint the one idempotent close command bound to this exact attachment.
    /// Calling this method performs no cancellation and opens no source.
    pub(crate) fn prepare_portable_close(
        &self,
    ) -> Result<ScopedCloseCommand, ScopedCloseContractError> {
        ScopedCloseCommand::new(
            Arc::clone(&self.attachment_authority),
            self.observation_contract.clone(),
            self.root_identity.clone(),
        )
    }

    /// Request cancellation, close the exact sole consumer drain, and retain
    /// the attachment-bound command until the existing barrier completes.
    pub(crate) fn close_portable_with_consumer(
        &self,
        command: &ScopedCloseCommand,
        drain: &mut ScopedObservationConsumerDrain,
    ) -> Result<ScopedObservationCloseOperation, ScopedCloseContractError> {
        if !command.matches_host(self)? {
            return Err(ScopedCloseContractError::ForeignCommand);
        }
        let barrier = self
            .close_with_consumer(drain)
            .map_err(|error| match error {
                ScopedObservationCloseError::ForeignDrain => ScopedCloseContractError::ForeignDrain,
            })?;
        Ok(ScopedObservationCloseOperation {
            command: command.clone(),
            barrier,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ScopedCloseOutcomeWire {
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopedCloseRootWire {
    adapter_id: String,
    source_instance_key: CanonicalSourceInstanceKey,
    session_ref: ExternalEntityRef,
    session_key: CanonicalEntityKey,
    root_actor_run_key: CanonicalEntityKey,
    #[serde(deserialize_with = "deserialize_required_option")]
    native_session_claim: Option<NativeIdentityClaim>,
}

impl ScopedCloseRootWire {
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
pub(crate) struct ScopedCloseContextWire {
    contract_selection: ObservationContractSelection,
    root: ScopedCloseRootWire,
    attachment_ref: String,
    close_request_id: String,
}

impl ScopedCloseContextWire {
    fn from_command(command: &ScopedCloseCommand) -> Self {
        Self {
            contract_selection: command.contract_selection.clone(),
            root: ScopedCloseRootWire::from_root(&command.root),
            attachment_ref: encode_opaque(&command.attachment_ref),
            close_request_id: encode_opaque(&command.close_request_id),
        }
    }
}

/// Portable completion receipt. It intentionally contains no active-work
/// counts, applied sequence, timestamp, locator, or payload field: completion
/// is established by the native barrier before construction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ScopedCloseReceiptWire {
    scoped_close_receipt_contract_version: u32,
    contract_selection: ObservationContractSelection,
    root: ScopedCloseRootWire,
    attachment_ref: String,
    close_request_id: String,
    outcome: ScopedCloseOutcomeWire,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopedCloseReceiptInput {
    scoped_close_receipt_contract_version: u32,
    contract_selection: JsonValue,
    root: ScopedCloseRootWire,
    attachment_ref: String,
    close_request_id: String,
    outcome: ScopedCloseOutcomeWire,
}

impl ScopedCloseReceiptWire {
    fn from_completed_command(
        command: &ScopedCloseCommand,
        state: ScopedObservationCloseState,
    ) -> Result<Self, ScopedCloseContractError> {
        validate_completed_state(state)?;
        Ok(Self {
            scoped_close_receipt_contract_version: SCOPED_CLOSE_RECEIPT_CONTRACT_VERSION,
            contract_selection: command.contract_selection.clone(),
            root: ScopedCloseRootWire::from_root(&command.root),
            attachment_ref: encode_opaque(&command.attachment_ref),
            close_request_id: encode_opaque(&command.close_request_id),
            outcome: ScopedCloseOutcomeWire::Closed,
        })
    }

    pub(crate) fn from_wire_value_for_operation(
        value: JsonValue,
        expected_operation: &ScopedObservationCloseOperation,
    ) -> Result<Self, ScopedCloseContractError> {
        validate_completed_state(expected_operation.barrier.state())?;
        validate_raw_receipt_shape(&value)?;
        let expected_command = &expected_operation.command;
        let input: ScopedCloseReceiptInput = serde_json::from_value(value)
            .map_err(|error| ScopedCloseContractError::invalid(error.to_string()))?;
        let contract_selection = ObservationContractSelection::from_wire_value_for_expected(
            input.contract_selection,
            &expected_command.contract_selection,
        )
        .map_err(|error| ScopedCloseContractError::invalid(error.to_string()))?;
        let receipt = Self {
            scoped_close_receipt_contract_version: input.scoped_close_receipt_contract_version,
            contract_selection,
            root: input.root,
            attachment_ref: input.attachment_ref,
            close_request_id: input.close_request_id,
            outcome: input.outcome,
        };
        receipt.validate_for_command(expected_command)?;
        Ok(receipt)
    }

    fn validate_for_command(
        &self,
        expected_command: &ScopedCloseCommand,
    ) -> Result<(), ScopedCloseContractError> {
        if self.scoped_close_receipt_contract_version != SCOPED_CLOSE_RECEIPT_CONTRACT_VERSION
            || self.contract_selection.lifecycle_contract_version
                != SCOPED_CLOSE_RECEIPT_CONTRACT_VERSION
            || self.contract_selection != expected_command.contract_selection
            || self.root != ScopedCloseRootWire::from_root(&expected_command.root)
            || self.outcome != ScopedCloseOutcomeWire::Closed
            || decode_opaque_exact(&self.attachment_ref, "attachment_ref")?
                != expected_command.attachment_ref
            || decode_opaque_exact(&self.close_request_id, "close_request_id")?
                != expected_command.close_request_id
        {
            return Err(ScopedCloseContractError::invalid(
                "close receipt does not match the caller-held attachment command",
            ));
        }
        Ok(())
    }
}

fn validate_raw_receipt_shape(value: &JsonValue) -> Result<(), ScopedCloseContractError> {
    let receipt = exact_object(
        value,
        "scoped close receipt",
        &[
            "scoped_close_receipt_contract_version",
            "contract_selection",
            "root",
            "attachment_ref",
            "close_request_id",
            "outcome",
        ],
        &[],
    )?;
    let root = exact_object(
        &receipt["root"],
        "scoped close root",
        &[
            "adapter_id",
            "source_instance_key",
            "session_ref",
            "session_key",
            "root_actor_run_key",
            "native_session_claim",
        ],
        &[],
    )?;
    let claim = &root["native_session_claim"];
    if claim.is_null() {
        return Ok(());
    }
    let claim = exact_object(
        claim,
        "native session claim",
        &["entity_ref", "identity"],
        &[],
    )?;
    let identity = exact_object(
        &claim["identity"],
        "native session qualified identity",
        &[
            "value",
            "quality",
            "authority",
            "completeness",
            "provenance",
        ],
        &["unknown_reason", "effective_at"],
    )?;
    for optional in ["unknown_reason", "effective_at"] {
        if identity.get(optional).is_some_and(JsonValue::is_null) {
            return Err(ScopedCloseContractError::invalid(format!(
                "native session qualified identity field {optional} cannot be null"
            )));
        }
    }
    if !identity["value"].is_null() {
        exact_object(
            &identity["value"],
            "native session identity",
            &["native_namespace", "native_id"],
            &[],
        )?;
    }
    let provenance = identity["provenance"].as_array().ok_or_else(|| {
        ScopedCloseContractError::invalid("native session claim provenance must be an array")
    })?;
    if provenance.len() > MAX_ROOT_PROVENANCE {
        return Err(ScopedCloseContractError::invalid(
            "native session claim provenance exceeds 64 entries",
        ));
    }
    for reference in provenance {
        exact_object(
            reference,
            "native session claim provenance",
            &["semantic_reference_contract_version", "fact_revision_id"],
            &[],
        )?;
    }
    Ok(())
}

fn exact_object<'a>(
    value: &'a JsonValue,
    label: &str,
    required: &[&str],
    optional: &[&str],
) -> Result<&'a serde_json::Map<String, JsonValue>, ScopedCloseContractError> {
    let object = value
        .as_object()
        .ok_or_else(|| ScopedCloseContractError::invalid(format!("{label} must be an object")))?;
    for field in required {
        if !object.contains_key(*field) {
            return Err(ScopedCloseContractError::invalid(format!(
                "{label} is missing field {field}"
            )));
        }
    }
    if let Some(field) = object
        .keys()
        .find(|field| !required.contains(&field.as_str()) && !optional.contains(&field.as_str()))
    {
        return Err(ScopedCloseContractError::invalid(format!(
            "{label} contains unknown field {field}"
        )));
    }
    Ok(object)
}

fn validate_completed_state(
    state: ScopedObservationCloseState,
) -> Result<(), ScopedCloseContractError> {
    if !state.close_requested
        || state.active_operations != 0
        || state.active_watcher_tasks != 0
        || state.consumer_drain_pending
        || !state.complete
    {
        return Err(ScopedCloseContractError::NotComplete);
    }
    Ok(())
}

fn derive_attachment_ref(
    attachment_token: u64,
    selection: &ObservationContractSelection,
    root: &ScopedObservationRootIdentity,
) -> Result<[u8; DIGEST_BYTES], ScopedCloseContractError> {
    let selection = serde_json::to_vec(selection)
        .map_err(|error| ScopedCloseContractError::invalid(error.to_string()))?;
    let root = serde_json::to_vec(&ScopedCloseRootWire::from_root(root))
        .map_err(|error| ScopedCloseContractError::invalid(error.to_string()))?;
    let mut hasher = blake3::Hasher::new();
    hash_part(&mut hasher, b"spaghetti.rfc012d.close-attachment.v1");
    hash_part(&mut hasher, &attachment_token.to_be_bytes());
    hash_part(&mut hasher, &selection);
    hash_part(&mut hasher, &root);
    Ok(*hasher.finalize().as_bytes())
}

fn derive_close_request_id(attachment_ref: &[u8; DIGEST_BYTES]) -> [u8; DIGEST_BYTES] {
    let mut hasher = blake3::Hasher::new();
    hash_part(&mut hasher, b"spaghetti.rfc012d.close-request.v1");
    hash_part(&mut hasher, attachment_ref);
    *hasher.finalize().as_bytes()
}

fn hash_part(hasher: &mut blake3::Hasher, bytes: &[u8]) {
    hasher.update(&(bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
}

fn encode_opaque(bytes: &[u8]) -> String {
    format!("{REFERENCE_PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes))
}

fn decode_opaque_exact(
    value: &str,
    label: &str,
) -> Result<[u8; DIGEST_BYTES], ScopedCloseContractError> {
    let encoded = value.strip_prefix(REFERENCE_PREFIX).ok_or_else(|| {
        ScopedCloseContractError::invalid(format!("{label} is not a v1 reference"))
    })?;
    if encoded.is_empty() || encoded.contains('=') {
        return Err(ScopedCloseContractError::invalid(format!(
            "{label} is not canonical base64url"
        )));
    }
    let decoded = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| {
        ScopedCloseContractError::invalid(format!("{label} is not canonical base64url"))
    })?;
    let decoded: [u8; DIGEST_BYTES] = decoded.try_into().map_err(|_| {
        ScopedCloseContractError::invalid(format!("{label} must contain exactly 32 bytes"))
    })?;
    if encode_opaque(&decoded) != value {
        return Err(ScopedCloseContractError::invalid(format!(
            "{label} is not canonical base64url"
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
