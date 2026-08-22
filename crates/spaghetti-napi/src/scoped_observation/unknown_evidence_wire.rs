//! Contextual portable RFC 012C bounded unknown-native-evidence snapshot.
//!
//! This is a value projection of the topology-neutral reducer shared by
//! durable and scoped execution. It carries exact complete-set totals/digest
//! plus the policy-bounded deterministic samples. It is not a source locator,
//! source-access grant, durable query, observer event, replacement barrier, or
//! transport authority. An enclosing query/event remains responsible for
//! binding the snapshot to its caller-held source/scope context.

use std::collections::BTreeSet;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::adapter::{BoundedNativeEvidence, SourceRecordId};
use crate::unknown_evidence_reducer::{
    UnknownEvidenceAggregateSnapshot, UnknownEvidenceOccurrence, MAX_UNKNOWN_EVIDENCE_OCCURRENCES,
    MAX_UNKNOWN_EVIDENCE_SAMPLES, UNKNOWN_EVIDENCE_AGGREGATE_CONTRACT_VERSION,
};

pub(crate) const UNKNOWN_EVIDENCE_SNAPSHOT_CONTRACT_VERSION: u32 = 1;

const REFERENCE_PREFIX: &str = "v1:";
const DIGEST_BYTES: usize = 32;
const DIGEST_ENCODED_BYTES: usize = 43;
const JS_SAFE_INTEGER_MAX: u64 = 9_007_199_254_740_991;
const MAX_FAMILY_HINT_BYTES: usize = 128;
const MAX_DIAGNOSTIC_SHAPE_ITEMS: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum UnknownEvidenceSnapshotContractError {
    #[error("invalid bounded unknown-evidence snapshot: {message}")]
    Invalid { message: String },
    #[error("bounded unknown-evidence snapshot does not match caller-held reducer state")]
    ContextMismatch,
}

impl UnknownEvidenceSnapshotContractError {
    fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid {
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct UnknownEvidenceSampleWire {
    source_record_id: SourceRecordId,
    family_hint: Option<String>,
    observed_bytes: u64,
    payload_digest: String,
    sanitized_excerpt: JsonValue,
}

impl UnknownEvidenceSampleWire {
    fn from_internal(
        occurrence: &UnknownEvidenceOccurrence,
    ) -> Result<Self, UnknownEvidenceSnapshotContractError> {
        let sanitized_excerpt = serde_json::from_slice(&occurrence.evidence.sanitized_excerpt)
            .map_err(|_| {
                UnknownEvidenceSnapshotContractError::invalid(
                    "unknown-evidence excerpt is not sanitized JSON",
                )
            })?;
        let value = Self {
            source_record_id: occurrence.evidence.source_record_id,
            family_hint: occurrence.family_hint.clone(),
            observed_bytes: occurrence.evidence.observed_bytes,
            payload_digest: encode_opaque(&occurrence.evidence.payload_digest),
            sanitized_excerpt,
        };
        value.to_internal()?;
        Ok(value)
    }

    fn to_internal(
        &self,
    ) -> Result<UnknownEvidenceOccurrence, UnknownEvidenceSnapshotContractError> {
        if self
            .source_record_id
            .as_bytes()
            .iter()
            .all(|byte| *byte == 0)
            || self.observed_bytes > JS_SAFE_INTEGER_MAX
        {
            return Err(UnknownEvidenceSnapshotContractError::invalid(
                "unknown-evidence sample identity or byte count is invalid",
            ));
        }
        let payload_digest = decode_opaque_exact(&self.payload_digest, "unknown payload digest")?;
        if payload_digest.iter().all(|byte| *byte == 0) {
            return Err(UnknownEvidenceSnapshotContractError::invalid(
                "unknown payload digest must not be zero",
            ));
        }
        let sanitized_excerpt = serde_json::to_vec(&self.sanitized_excerpt).map_err(|_| {
            UnknownEvidenceSnapshotContractError::invalid(
                "unknown-evidence excerpt is not portable JSON",
            )
        })?;
        UnknownEvidenceOccurrence::new(
            self.family_hint.clone(),
            BoundedNativeEvidence {
                source_record_id: self.source_record_id,
                observed_bytes: self.observed_bytes,
                payload_digest,
                sanitized_excerpt,
            },
        )
        .map_err(|_| {
            UnknownEvidenceSnapshotContractError::invalid(
                "unknown-evidence sample is not a bounded sanitized occurrence",
            )
        })
    }
}

/// Caller-held reducer result. It is intentionally non-Serde. Debug output
/// withholds source identities, family hints, payload digests, and excerpts.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct UnknownEvidenceSnapshotConsumerContext {
    expected_complete_count: u64,
    expected_complete_observed_bytes: u64,
    expected_aggregate_digest: String,
    expected_samples: Vec<UnknownEvidenceSampleWire>,
}

impl std::fmt::Debug for UnknownEvidenceSnapshotConsumerContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UnknownEvidenceSnapshotConsumerContext")
            .field("expected_complete_count", &self.expected_complete_count)
            .field("expected_sample_count", &self.expected_samples.len())
            .finish_non_exhaustive()
    }
}

impl UnknownEvidenceSnapshotConsumerContext {
    pub(crate) fn from_expected(
        expected: &UnknownEvidenceAggregateSnapshot,
    ) -> Result<Self, UnknownEvidenceSnapshotContractError> {
        let expected_samples = expected
            .samples
            .iter()
            .map(UnknownEvidenceSampleWire::from_internal)
            .collect::<Result<Vec<_>, _>>()?;
        validate_snapshot(
            expected.complete_count,
            expected.complete_observed_bytes,
            &expected.aggregate_digest,
            &expected_samples,
        )?;
        Ok(Self {
            expected_complete_count: expected.complete_count,
            expected_complete_observed_bytes: expected.complete_observed_bytes,
            expected_aggregate_digest: encode_opaque(&expected.aggregate_digest),
            expected_samples,
        })
    }

    pub(crate) fn wire(&self) -> UnknownEvidenceSnapshotContextWire {
        UnknownEvidenceSnapshotContextWire {
            expected_complete_count: self.expected_complete_count,
            expected_complete_observed_bytes: self.expected_complete_observed_bytes,
            expected_aggregate_digest: self.expected_aggregate_digest.clone(),
            expected_samples: self.expected_samples.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct UnknownEvidenceSnapshotContextWire {
    expected_complete_count: u64,
    expected_complete_observed_bytes: u64,
    expected_aggregate_digest: String,
    expected_samples: Vec<UnknownEvidenceSampleWire>,
}

/// Serialize-only aggregate/sample value. A received payload cannot establish
/// its own complete-set digest or deterministic sample expectation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct UnknownEvidenceSnapshotWire {
    unknown_evidence_snapshot_contract_version: u32,
    unknown_evidence_aggregate_contract_version: u32,
    complete_count: u64,
    complete_observed_bytes: u64,
    aggregate_digest: String,
    samples: Vec<UnknownEvidenceSampleWire>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UnknownEvidenceSnapshotInput {
    unknown_evidence_snapshot_contract_version: u32,
    unknown_evidence_aggregate_contract_version: u32,
    complete_count: u64,
    complete_observed_bytes: u64,
    aggregate_digest: String,
    samples: Vec<UnknownEvidenceSampleWire>,
}

impl UnknownEvidenceSnapshotWire {
    pub(crate) fn from_context(
        context: &UnknownEvidenceSnapshotConsumerContext,
    ) -> Result<Self, UnknownEvidenceSnapshotContractError> {
        let aggregate_digest = decode_opaque_exact(
            &context.expected_aggregate_digest,
            "expected unknown-evidence aggregate digest",
        )?;
        validate_snapshot(
            context.expected_complete_count,
            context.expected_complete_observed_bytes,
            &aggregate_digest,
            &context.expected_samples,
        )?;
        Ok(Self {
            unknown_evidence_snapshot_contract_version: UNKNOWN_EVIDENCE_SNAPSHOT_CONTRACT_VERSION,
            unknown_evidence_aggregate_contract_version:
                UNKNOWN_EVIDENCE_AGGREGATE_CONTRACT_VERSION,
            complete_count: context.expected_complete_count,
            complete_observed_bytes: context.expected_complete_observed_bytes,
            aggregate_digest: context.expected_aggregate_digest.clone(),
            samples: context.expected_samples.clone(),
        })
    }

    pub(crate) fn from_wire_value_for_context(
        value: JsonValue,
        context: &UnknownEvidenceSnapshotConsumerContext,
    ) -> Result<Self, UnknownEvidenceSnapshotContractError> {
        preflight_wire_value(&value)?;
        let input: UnknownEvidenceSnapshotInput = serde_json::from_value(value).map_err(|_| {
            UnknownEvidenceSnapshotContractError::invalid("invalid snapshot fields")
        })?;
        if input.unknown_evidence_snapshot_contract_version
            != UNKNOWN_EVIDENCE_SNAPSHOT_CONTRACT_VERSION
            || input.unknown_evidence_aggregate_contract_version
                != UNKNOWN_EVIDENCE_AGGREGATE_CONTRACT_VERSION
        {
            return Err(UnknownEvidenceSnapshotContractError::invalid(
                "unsupported unknown-evidence snapshot contract version",
            ));
        }
        let aggregate_digest =
            decode_opaque_exact(&input.aggregate_digest, "unknown-evidence aggregate digest")?;
        validate_snapshot(
            input.complete_count,
            input.complete_observed_bytes,
            &aggregate_digest,
            &input.samples,
        )?;
        let wire = Self {
            unknown_evidence_snapshot_contract_version: input
                .unknown_evidence_snapshot_contract_version,
            unknown_evidence_aggregate_contract_version: input
                .unknown_evidence_aggregate_contract_version,
            complete_count: input.complete_count,
            complete_observed_bytes: input.complete_observed_bytes,
            aggregate_digest: input.aggregate_digest,
            samples: input.samples,
        };
        if wire.complete_count != context.expected_complete_count
            || wire.complete_observed_bytes != context.expected_complete_observed_bytes
            || wire.aggregate_digest != context.expected_aggregate_digest
            || wire.samples != context.expected_samples
        {
            return Err(UnknownEvidenceSnapshotContractError::ContextMismatch);
        }
        Ok(wire)
    }
}

fn validate_snapshot(
    complete_count: u64,
    complete_observed_bytes: u64,
    aggregate_digest: &[u8; DIGEST_BYTES],
    samples: &[UnknownEvidenceSampleWire],
) -> Result<(), UnknownEvidenceSnapshotContractError> {
    if complete_count > MAX_UNKNOWN_EVIDENCE_OCCURRENCES as u64
        || complete_count > JS_SAFE_INTEGER_MAX
        || complete_observed_bytes > JS_SAFE_INTEGER_MAX
        || aggregate_digest.iter().all(|byte| *byte == 0)
        || samples.len()
            != usize::try_from(complete_count)
                .unwrap_or(usize::MAX)
                .min(MAX_UNKNOWN_EVIDENCE_SAMPLES)
    {
        return Err(UnknownEvidenceSnapshotContractError::invalid(
            "unknown-evidence totals, digest, or sample count are invalid",
        ));
    }
    let mut identities = BTreeSet::new();
    let mut sampled_bytes = 0_u64;
    for sample in samples {
        let occurrence = sample.to_internal()?;
        if !identities.insert(occurrence.evidence.source_record_id) {
            return Err(UnknownEvidenceSnapshotContractError::invalid(
                "unknown-evidence samples contain duplicate source identity",
            ));
        }
        sampled_bytes = sampled_bytes
            .checked_add(occurrence.evidence.observed_bytes)
            .ok_or_else(|| {
                UnknownEvidenceSnapshotContractError::invalid(
                    "unknown-evidence sample bytes overflow",
                )
            })?;
    }
    if sampled_bytes > complete_observed_bytes
        || (complete_count <= MAX_UNKNOWN_EVIDENCE_SAMPLES as u64
            && sampled_bytes != complete_observed_bytes)
    {
        return Err(UnknownEvidenceSnapshotContractError::invalid(
            "unknown-evidence sample bytes do not match complete totals",
        ));
    }
    Ok(())
}

fn preflight_wire_value(value: &JsonValue) -> Result<(), UnknownEvidenceSnapshotContractError> {
    let input = exact_object(
        value,
        "unknown-evidence snapshot",
        &[
            "unknown_evidence_snapshot_contract_version",
            "unknown_evidence_aggregate_contract_version",
            "complete_count",
            "complete_observed_bytes",
            "aggregate_digest",
            "samples",
        ],
    )?;
    let samples = input["samples"].as_array().ok_or_else(|| {
        UnknownEvidenceSnapshotContractError::invalid("unknown-evidence samples must be an array")
    })?;
    if samples.len() > MAX_UNKNOWN_EVIDENCE_SAMPLES {
        return Err(UnknownEvidenceSnapshotContractError::invalid(
            "unknown-evidence samples exceed the portable bound",
        ));
    }
    for sample in samples {
        let sample = exact_object(
            sample,
            "unknown-evidence sample",
            &[
                "source_record_id",
                "family_hint",
                "observed_bytes",
                "payload_digest",
                "sanitized_excerpt",
            ],
        )?;
        preflight_bounded_string(
            &sample["source_record_id"],
            "unknown-evidence source record id",
            REFERENCE_PREFIX.len() + DIGEST_ENCODED_BYTES,
        )?;
        match &sample["family_hint"] {
            JsonValue::Null => {}
            value => preflight_bounded_string(
                value,
                "unknown-evidence family hint",
                MAX_FAMILY_HINT_BYTES,
            )?,
        }
        preflight_bounded_string(
            &sample["payload_digest"],
            "unknown-evidence payload digest",
            REFERENCE_PREFIX.len() + DIGEST_ENCODED_BYTES,
        )?;
        preflight_sanitized_excerpt(&sample["sanitized_excerpt"])?;
    }
    preflight_bounded_string(
        &input["aggregate_digest"],
        "unknown-evidence aggregate digest",
        REFERENCE_PREFIX.len() + DIGEST_ENCODED_BYTES,
    )?;
    Ok(())
}

fn preflight_sanitized_excerpt(
    value: &JsonValue,
) -> Result<(), UnknownEvidenceSnapshotContractError> {
    let object = value.as_object().ok_or_else(|| {
        UnknownEvidenceSnapshotContractError::invalid(
            "unknown-evidence sanitized excerpt must be an object",
        )
    })?;
    let kind = object
        .get("kind")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| {
            UnknownEvidenceSnapshotContractError::invalid(
                "unknown-evidence sanitized excerpt kind is invalid",
            )
        })?;
    match kind {
        "json_object" => {
            let object = exact_object(
                value,
                "sanitized object excerpt",
                &["bytes", "hash", "kind", "members", "shape", "truncated"],
            )?;
            preflight_bounded_string(&object["hash"], "sanitized excerpt hash", 64)?;
            let shape = object["shape"].as_array().ok_or_else(|| {
                UnknownEvidenceSnapshotContractError::invalid(
                    "sanitized object shape must be an array",
                )
            })?;
            if shape.len() > MAX_DIAGNOSTIC_SHAPE_ITEMS {
                return Err(UnknownEvidenceSnapshotContractError::invalid(
                    "sanitized object shape exceeds the portable bound",
                ));
            }
            for entry in shape {
                let entry = exact_object(
                    entry,
                    "sanitized object shape item",
                    &["key_hash", "value_kind"],
                )?;
                preflight_bounded_string(&entry["key_hash"], "sanitized key hash", 12)?;
                preflight_bounded_string(&entry["value_kind"], "sanitized value kind", 7)?;
            }
        }
        "json_array" => {
            let object = exact_object(
                value,
                "sanitized array excerpt",
                &["bytes", "hash", "item_kinds", "items", "kind", "truncated"],
            )?;
            preflight_bounded_string(&object["hash"], "sanitized excerpt hash", 64)?;
            let item_kinds = object["item_kinds"].as_array().ok_or_else(|| {
                UnknownEvidenceSnapshotContractError::invalid(
                    "sanitized array kinds must be an array",
                )
            })?;
            if item_kinds.len() > MAX_DIAGNOSTIC_SHAPE_ITEMS {
                return Err(UnknownEvidenceSnapshotContractError::invalid(
                    "sanitized array shape exceeds the portable bound",
                ));
            }
            for kind in item_kinds {
                preflight_bounded_string(kind, "sanitized value kind", 7)?;
            }
        }
        "null" | "boolean" | "number" | "string" | "opaque" => {
            let object = exact_object(
                value,
                "sanitized scalar excerpt",
                &["bytes", "hash", "kind"],
            )?;
            preflight_bounded_string(&object["hash"], "sanitized excerpt hash", 64)?;
        }
        _ => {
            return Err(UnknownEvidenceSnapshotContractError::invalid(
                "unknown-evidence sanitized excerpt kind is invalid",
            ));
        }
    }
    Ok(())
}

fn preflight_bounded_string(
    value: &JsonValue,
    label: &str,
    exact_max_bytes: usize,
) -> Result<(), UnknownEvidenceSnapshotContractError> {
    let value = value.as_str().ok_or_else(|| {
        UnknownEvidenceSnapshotContractError::invalid(format!("{label} must be a string"))
    })?;
    if value.len() > exact_max_bytes {
        return Err(UnknownEvidenceSnapshotContractError::invalid(format!(
            "{label} exceeds the portable bound"
        )));
    }
    Ok(())
}

fn exact_object<'a>(
    value: &'a JsonValue,
    label: &str,
    fields: &[&str],
) -> Result<&'a serde_json::Map<String, JsonValue>, UnknownEvidenceSnapshotContractError> {
    let object = value.as_object().ok_or_else(|| {
        UnknownEvidenceSnapshotContractError::invalid(format!("{label} must be an object"))
    })?;
    if object.len() != fields.len()
        || fields.iter().any(|field| !object.contains_key(*field))
        || object.keys().any(|field| !fields.contains(&field.as_str()))
    {
        return Err(UnknownEvidenceSnapshotContractError::invalid(format!(
            "{label} fields do not match the exact contract"
        )));
    }
    Ok(object)
}

fn encode_opaque(bytes: &[u8; DIGEST_BYTES]) -> String {
    format!("{REFERENCE_PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes))
}

fn decode_opaque_exact(
    value: &str,
    label: &str,
) -> Result<[u8; DIGEST_BYTES], UnknownEvidenceSnapshotContractError> {
    let encoded = value.strip_prefix(REFERENCE_PREFIX).ok_or_else(|| {
        UnknownEvidenceSnapshotContractError::invalid(format!("{label} is not v1"))
    })?;
    if encoded.len() != DIGEST_ENCODED_BYTES || encoded.contains('=') {
        return Err(UnknownEvidenceSnapshotContractError::invalid(format!(
            "{label} is not canonical base64url"
        )));
    }
    let decoded = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| {
        UnknownEvidenceSnapshotContractError::invalid(format!("{label} is not canonical base64url"))
    })?;
    if decoded.len() != DIGEST_BYTES || URL_SAFE_NO_PAD.encode(&decoded) != encoded {
        return Err(UnknownEvidenceSnapshotContractError::invalid(format!(
            "{label} must contain exactly {DIGEST_BYTES} canonical bytes"
        )));
    }
    decoded.try_into().map_err(|_| {
        UnknownEvidenceSnapshotContractError::invalid(format!(
            "{label} must contain exactly {DIGEST_BYTES} canonical bytes"
        ))
    })
}

#[cfg(test)]
mod tests;
