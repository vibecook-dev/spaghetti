//! Topology-neutral RFC 012C unknown-native-evidence reduction.
//!
//! The reducer retains an explicitly bounded current-generation set, computes
//! one exact digest over the complete set, and chooses transported samples by
//! topology-neutral source identity. Arrival order, task scheduling, database
//! IDs, and observer delivery coordinates cannot affect either result.

use std::collections::BTreeMap;

use serde_json::{Map as JsonMap, Value as JsonValue};

use crate::adapter::{BoundedNativeEvidence, SourceRecordId, MAX_UNKNOWN_RAW_PAYLOAD_BYTES};
use crate::decode_runtime::{MAX_DIAGNOSTIC_EXCERPT_BYTES, MAX_DIAGNOSTIC_SHAPE_ITEMS};

pub(crate) const UNKNOWN_EVIDENCE_AGGREGATE_CONTRACT_VERSION: u32 = 1;
pub(crate) const MAX_UNKNOWN_EVIDENCE_OCCURRENCES: usize = 65_536;
pub(crate) const MAX_UNKNOWN_EVIDENCE_SAMPLES: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnknownEvidenceReduction {
    Unchanged,
    Upsert,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum UnknownEvidenceReductionError {
    #[error("invalid bounded unknown evidence")]
    InvalidEvidence,
    #[error("duplicate unknown-evidence source identity")]
    DuplicateIdentity,
    #[error("unknown-evidence reduction capacity exhausted")]
    CapacityExhausted,
    #[error("unknown-evidence aggregate counter overflow")]
    CounterOverflow,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UnknownEvidenceOccurrence {
    pub family_hint: Option<String>,
    pub evidence: BoundedNativeEvidence,
}

impl UnknownEvidenceOccurrence {
    pub(crate) fn new(
        family_hint: Option<String>,
        evidence: BoundedNativeEvidence,
    ) -> Result<Self, UnknownEvidenceReductionError> {
        let value = Self {
            family_hint,
            evidence,
        };
        value.validate()?;
        Ok(value)
    }

    fn validate(&self) -> Result<(), UnknownEvidenceReductionError> {
        if self
            .family_hint
            .as_deref()
            .is_some_and(|value| !is_safe_family_hint(value))
            || self.evidence.observed_bytes > MAX_UNKNOWN_RAW_PAYLOAD_BYTES as u64
            || self.evidence.sanitized_excerpt.is_empty()
            || self.evidence.sanitized_excerpt.len() > MAX_DIAGNOSTIC_EXCERPT_BYTES
            || !is_valid_sanitized_excerpt(&self.evidence)
        {
            return Err(UnknownEvidenceReductionError::InvalidEvidence);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UnknownEvidenceAggregateSnapshot {
    pub complete_count: u64,
    pub complete_observed_bytes: u64,
    pub aggregate_digest: [u8; 32],
    pub samples: Vec<UnknownEvidenceOccurrence>,
}

impl UnknownEvidenceAggregateSnapshot {
    /// The fixed-policy empty current-generation snapshot used by tests and
    /// by carriers that have not observed an unknown native record. Derive it
    /// through the reducer so the empty digest cannot drift from the shared
    /// aggregation law.
    pub(crate) fn empty_policy() -> Self {
        UnknownEvidenceReducer::new(
            MAX_UNKNOWN_EVIDENCE_OCCURRENCES,
            MAX_UNKNOWN_EVIDENCE_SAMPLES,
        )
        .and_then(|reducer| reducer.snapshot())
        .expect("the fixed unknown-evidence reducer policy is valid")
    }
}

#[derive(Debug, Clone)]
pub(crate) struct UnknownEvidenceReducer {
    max_occurrences: usize,
    max_samples: usize,
    current: BTreeMap<SourceRecordId, UnknownEvidenceOccurrence>,
}

impl UnknownEvidenceReducer {
    pub(crate) fn new(
        max_occurrences: usize,
        max_samples: usize,
    ) -> Result<Self, UnknownEvidenceReductionError> {
        if max_occurrences == 0
            || max_occurrences > MAX_UNKNOWN_EVIDENCE_OCCURRENCES
            || max_samples > max_occurrences
            || max_samples > MAX_UNKNOWN_EVIDENCE_SAMPLES
        {
            return Err(UnknownEvidenceReductionError::InvalidEvidence);
        }
        Ok(Self {
            max_occurrences,
            max_samples,
            current: BTreeMap::new(),
        })
    }

    pub(crate) fn len(&self) -> usize {
        self.current.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.current.is_empty()
    }

    pub(crate) fn contains(&self, source_record_id: &SourceRecordId) -> bool {
        self.current.contains_key(source_record_id)
    }

    pub(crate) fn apply(
        &mut self,
        occurrence: UnknownEvidenceOccurrence,
    ) -> Result<UnknownEvidenceReduction, UnknownEvidenceReductionError> {
        let reduction = self.classify_apply(&occurrence)?;
        if reduction == UnknownEvidenceReduction::Upsert {
            let key = occurrence.evidence.source_record_id;
            self.current.insert(key, occurrence);
        }
        Ok(reduction)
    }

    pub(crate) fn classify_apply(
        &self,
        occurrence: &UnknownEvidenceOccurrence,
    ) -> Result<UnknownEvidenceReduction, UnknownEvidenceReductionError> {
        occurrence.validate()?;
        let key = occurrence.evidence.source_record_id;
        if self.current.get(&key) == Some(occurrence) {
            return Ok(UnknownEvidenceReduction::Unchanged);
        }
        if !self.current.contains_key(&key) && self.current.len() == self.max_occurrences {
            return Err(UnknownEvidenceReductionError::CapacityExhausted);
        }
        Ok(UnknownEvidenceReduction::Upsert)
    }

    pub(crate) fn retract(&mut self, source_record_id: &SourceRecordId) -> bool {
        self.current.remove(source_record_id).is_some()
    }

    pub(crate) fn clear(&mut self) {
        self.current.clear();
    }

    /// Replace the complete current-generation set atomically. Duplicate
    /// source identities are contract errors even when their values match.
    pub(crate) fn replace_complete(
        &mut self,
        occurrences: impl IntoIterator<Item = UnknownEvidenceOccurrence>,
    ) -> Result<(), UnknownEvidenceReductionError> {
        let mut replacement = BTreeMap::new();
        for occurrence in occurrences {
            occurrence.validate()?;
            if replacement.len() == self.max_occurrences {
                return Err(UnknownEvidenceReductionError::CapacityExhausted);
            }
            let key = occurrence.evidence.source_record_id;
            if replacement.insert(key, occurrence).is_some() {
                return Err(UnknownEvidenceReductionError::DuplicateIdentity);
            }
        }
        self.current = replacement;
        Ok(())
    }

    pub(crate) fn snapshot(
        &self,
    ) -> Result<UnknownEvidenceAggregateSnapshot, UnknownEvidenceReductionError> {
        let complete_count = u64::try_from(self.current.len())
            .map_err(|_| UnknownEvidenceReductionError::CounterOverflow)?;
        let complete_observed_bytes = self.current.values().try_fold(0_u64, |total, value| {
            total
                .checked_add(value.evidence.observed_bytes)
                .ok_or(UnknownEvidenceReductionError::CounterOverflow)
        })?;

        let mut hasher = blake3::Hasher::new();
        hasher.update(b"spaghetti/runtime.unknown-evidence/aggregate\0");
        hasher.update(&UNKNOWN_EVIDENCE_AGGREGATE_CONTRACT_VERSION.to_be_bytes());
        hasher.update(&complete_count.to_be_bytes());
        hasher.update(&complete_observed_bytes.to_be_bytes());
        for occurrence in self.current.values() {
            hash_occurrence(&mut hasher, occurrence);
        }

        let mut ranked = self
            .current
            .values()
            .map(|occurrence| {
                (
                    sample_rank(&occurrence.evidence.source_record_id),
                    occurrence,
                )
            })
            .collect::<Vec<_>>();
        ranked.sort_by_key(|(rank, occurrence)| (*rank, occurrence.evidence.source_record_id));
        let samples = ranked
            .into_iter()
            .take(self.max_samples)
            .map(|(_, occurrence)| occurrence.clone())
            .collect();

        Ok(UnknownEvidenceAggregateSnapshot {
            complete_count,
            complete_observed_bytes,
            aggregate_digest: *hasher.finalize().as_bytes(),
            samples,
        })
    }
}

fn is_safe_family_hint(value: &str) -> bool {
    const MAX_FAMILY_HINT_BYTES: usize = 128;
    let mut bytes = value.bytes();
    matches!(bytes.next(), Some(first) if first.is_ascii_lowercase() || first.is_ascii_digit())
        && value.len() <= MAX_FAMILY_HINT_BYTES
        && bytes.all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
}

fn is_valid_sanitized_excerpt(evidence: &BoundedNativeEvidence) -> bool {
    let Ok(JsonValue::Object(value)) =
        serde_json::from_slice::<JsonValue>(&evidence.sanitized_excerpt)
    else {
        return false;
    };
    let Some(kind) = value.get("kind").and_then(JsonValue::as_str) else {
        return false;
    };
    let expected_hash = blake3::Hash::from_bytes(evidence.payload_digest)
        .to_hex()
        .to_string();
    if value.get("bytes").and_then(JsonValue::as_u64) != Some(evidence.observed_bytes)
        || value.get("hash").and_then(JsonValue::as_str) != Some(expected_hash.as_str())
    {
        return false;
    }

    match kind {
        "json_object" => validate_object_shape(&value, evidence.observed_bytes),
        "json_array" => validate_array_shape(&value, evidence.observed_bytes),
        "null" | "boolean" | "number" | "string" | "opaque" => {
            exact_fields(&value, &["bytes", "hash", "kind"])
        }
        _ => false,
    }
}

fn validate_object_shape(value: &JsonMap<String, JsonValue>, observed_bytes: u64) -> bool {
    if !exact_fields(
        value,
        &["bytes", "hash", "kind", "members", "shape", "truncated"],
    ) {
        return false;
    }
    let Some(members) = value.get("members").and_then(JsonValue::as_u64) else {
        return false;
    };
    if members > observed_bytes {
        return false;
    }
    let Some(shape) = value.get("shape").and_then(JsonValue::as_array) else {
        return false;
    };
    let expected_len = usize::try_from(members)
        .unwrap_or(usize::MAX)
        .min(MAX_DIAGNOSTIC_SHAPE_ITEMS);
    if shape.len() != expected_len
        || value.get("truncated").and_then(JsonValue::as_bool)
            != Some(members > MAX_DIAGNOSTIC_SHAPE_ITEMS as u64)
    {
        return false;
    }
    shape.iter().all(|entry| {
        let JsonValue::Object(entry) = entry else {
            return false;
        };
        exact_fields(entry, &["key_hash", "value_kind"])
            && entry
                .get("key_hash")
                .and_then(JsonValue::as_str)
                .is_some_and(|value| {
                    value.len() == 12
                        && value.bytes().all(|byte| byte.is_ascii_hexdigit())
                        && value.bytes().all(|byte| !byte.is_ascii_uppercase())
                })
            && entry
                .get("value_kind")
                .and_then(JsonValue::as_str)
                .is_some_and(is_json_value_kind)
    })
}

fn validate_array_shape(value: &JsonMap<String, JsonValue>, observed_bytes: u64) -> bool {
    if !exact_fields(
        value,
        &["bytes", "hash", "item_kinds", "items", "kind", "truncated"],
    ) {
        return false;
    }
    let Some(items) = value.get("items").and_then(JsonValue::as_u64) else {
        return false;
    };
    if items > observed_bytes {
        return false;
    }
    let Some(item_kinds) = value.get("item_kinds").and_then(JsonValue::as_array) else {
        return false;
    };
    let expected_len = usize::try_from(items)
        .unwrap_or(usize::MAX)
        .min(MAX_DIAGNOSTIC_SHAPE_ITEMS);
    item_kinds.len() == expected_len
        && value.get("truncated").and_then(JsonValue::as_bool)
            == Some(items > MAX_DIAGNOSTIC_SHAPE_ITEMS as u64)
        && item_kinds
            .iter()
            .all(|kind| kind.as_str().is_some_and(is_json_value_kind))
}

fn exact_fields(value: &JsonMap<String, JsonValue>, fields: &[&str]) -> bool {
    value.len() == fields.len()
        && fields.iter().all(|field| value.contains_key(*field))
        && value.keys().all(|field| fields.contains(&field.as_str()))
}

fn is_json_value_kind(value: &str) -> bool {
    matches!(
        value,
        "null" | "boolean" | "number" | "string" | "array" | "object"
    )
}

fn hash_component(hasher: &mut blake3::Hasher, value: &[u8]) {
    hasher.update(&(value.len() as u64).to_be_bytes());
    hasher.update(value);
}

fn hash_occurrence(hasher: &mut blake3::Hasher, occurrence: &UnknownEvidenceOccurrence) {
    hash_component(hasher, occurrence.evidence.source_record_id.as_bytes());
    match occurrence.family_hint.as_deref() {
        Some(family_hint) => {
            hasher.update(&[1]);
            hash_component(hasher, family_hint.as_bytes());
        }
        None => {
            hasher.update(&[0]);
        }
    }
    hasher.update(&occurrence.evidence.observed_bytes.to_be_bytes());
    hash_component(hasher, &occurrence.evidence.payload_digest);
    hash_component(
        hasher,
        blake3::hash(&occurrence.evidence.sanitized_excerpt).as_bytes(),
    );
}

fn sample_rank(source_record_id: &SourceRecordId) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"spaghetti/runtime.unknown-evidence/sample-rank\0");
    hasher.update(&UNKNOWN_EVIDENCE_AGGREGATE_CONTRACT_VERSION.to_be_bytes());
    hash_component(&mut hasher, source_record_id.as_bytes());
    *hasher.finalize().as_bytes()
}

#[cfg(test)]
mod tests {
    use crate::adapter::{CanonicalSourceInstanceKey, SourceRecordId};

    use super::*;

    fn source_record_id(index: u64) -> SourceRecordId {
        SourceRecordId::derive(
            "unknown-fixture",
            &CanonicalSourceInstanceKey::derive(1, b"unknown-source").unwrap(),
            b"events",
            b"session.jsonl",
            1,
            &index.to_be_bytes(),
            1,
        )
        .unwrap()
    }

    fn occurrence(index: u64, payload: &[u8]) -> UnknownEvidenceOccurrence {
        let payload_digest = *blake3::hash(payload).as_bytes();
        UnknownEvidenceOccurrence::new(
            Some(format!("future.kind-{index}")),
            BoundedNativeEvidence {
                source_record_id: source_record_id(index),
                observed_bytes: payload.len() as u64,
                payload_digest,
                sanitized_excerpt: format!(
                    "{{\"bytes\":{},\"hash\":\"{}\",\"kind\":\"opaque\"}}",
                    payload.len(),
                    blake3::Hash::from_bytes(payload_digest).to_hex()
                )
                .into_bytes(),
            },
        )
        .unwrap()
    }

    #[test]
    fn aggregate_and_samples_are_arrival_order_independent() {
        let values = (0..8)
            .map(|index| occurrence(index, format!("payload-{index}").as_bytes()))
            .collect::<Vec<_>>();
        let mut forward = UnknownEvidenceReducer::new(8, 3).unwrap();
        let mut reverse = UnknownEvidenceReducer::new(8, 3).unwrap();
        for value in &values {
            assert_eq!(
                forward.apply(value.clone()).unwrap(),
                UnknownEvidenceReduction::Upsert
            );
        }
        for value in values.iter().rev() {
            reverse.apply(value.clone()).unwrap();
        }

        let forward = forward.snapshot().unwrap();
        let reverse = reverse.snapshot().unwrap();
        assert_eq!(forward, reverse);
        assert_eq!(forward.complete_count, 8);
        assert_eq!(forward.samples.len(), 3);
        let mut expected = values.iter().collect::<Vec<_>>();
        expected.sort_by_key(|value| {
            (
                sample_rank(&value.evidence.source_record_id),
                value.evidence.source_record_id,
            )
        });
        assert_eq!(
            forward
                .samples
                .iter()
                .map(|value| value.evidence.source_record_id)
                .collect::<Vec<_>>(),
            expected
                .into_iter()
                .take(3)
                .map(|value| value.evidence.source_record_id)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn exact_replay_correction_retraction_and_reset_are_current_generation_laws() {
        let first = occurrence(1, b"first");
        let correction = occurrence(1, b"corrected");
        let mut reducer = UnknownEvidenceReducer::new(2, 1).unwrap();
        assert_eq!(
            reducer.apply(first.clone()).unwrap(),
            UnknownEvidenceReduction::Upsert
        );
        let first_digest = reducer.snapshot().unwrap().aggregate_digest;
        assert_eq!(
            reducer.apply(first).unwrap(),
            UnknownEvidenceReduction::Unchanged
        );
        assert_eq!(reducer.snapshot().unwrap().aggregate_digest, first_digest);
        assert_eq!(
            reducer.apply(correction.clone()).unwrap(),
            UnknownEvidenceReduction::Upsert
        );
        assert_eq!(reducer.len(), 1);
        assert_ne!(reducer.snapshot().unwrap().aggregate_digest, first_digest);
        assert!(reducer.retract(&correction.evidence.source_record_id));
        assert!(!reducer.retract(&correction.evidence.source_record_id));
        assert!(reducer.is_empty());
        reducer.apply(occurrence(2, b"next")).unwrap();
        reducer.clear();
        assert!(reducer.is_empty());
    }

    #[test]
    fn complete_replacement_and_capacity_fail_atomically() {
        let retained = occurrence(1, b"retained");
        let mut reducer = UnknownEvidenceReducer::new(2, 1).unwrap();
        reducer.apply(retained.clone()).unwrap();
        let before = reducer.snapshot().unwrap();

        assert_eq!(
            reducer.replace_complete([
                occurrence(2, b"two"),
                occurrence(3, b"three"),
                occurrence(4, b"four"),
            ]),
            Err(UnknownEvidenceReductionError::CapacityExhausted)
        );
        assert_eq!(reducer.snapshot().unwrap(), before);

        assert_eq!(
            reducer.replace_complete([retained.clone(), retained]),
            Err(UnknownEvidenceReductionError::DuplicateIdentity)
        );
        assert_eq!(reducer.snapshot().unwrap(), before);

        let mut invalid = occurrence(2, b"invalid");
        invalid.evidence.sanitized_excerpt =
            vec![0; MAX_DIAGNOSTIC_EXCERPT_BYTES.saturating_add(1)];
        assert_eq!(
            reducer.replace_complete([invalid]),
            Err(UnknownEvidenceReductionError::InvalidEvidence)
        );
        assert_eq!(reducer.snapshot().unwrap(), before);

        let mut oversized = occurrence(2, b"oversized");
        oversized.evidence.observed_bytes = MAX_UNKNOWN_RAW_PAYLOAD_BYTES as u64 + 1;
        assert_eq!(
            reducer.apply(oversized),
            Err(UnknownEvidenceReductionError::InvalidEvidence)
        );
        assert_eq!(reducer.snapshot().unwrap(), before);

        let mut unsanitized = occurrence(2, b"private");
        unsanitized.evidence.sanitized_excerpt =
            br#"{"path":"/Users/alice/private.json"}"#.to_vec();
        assert_eq!(
            reducer.apply(unsanitized),
            Err(UnknownEvidenceReductionError::InvalidEvidence)
        );
        assert_eq!(reducer.snapshot().unwrap(), before);

        let mut mismatched = occurrence(2, b"mismatched");
        mismatched.evidence.sanitized_excerpt = diagnostic_excerpt_for_test(b"different-payload");
        assert_eq!(
            reducer.apply(mismatched),
            Err(UnknownEvidenceReductionError::InvalidEvidence)
        );
        assert_eq!(reducer.snapshot().unwrap(), before);

        reducer
            .replace_complete([occurrence(2, b"two"), occurrence(3, b"three")])
            .unwrap();
        assert_eq!(reducer.len(), 2);
    }

    #[test]
    fn unsampled_evidence_still_changes_the_complete_aggregate() {
        let mut reducer = UnknownEvidenceReducer::new(4, 1).unwrap();
        for index in 0..4 {
            reducer
                .apply(occurrence(index, format!("payload-{index}").as_bytes()))
                .unwrap();
        }
        let before = reducer.snapshot().unwrap();
        let sampled = before.samples[0].evidence.source_record_id;
        let unsampled_index = (0..4)
            .find(|index| source_record_id(*index) != sampled)
            .unwrap();
        reducer
            .apply(occurrence(unsampled_index, b"corrected-unsampled"))
            .unwrap();
        let after = reducer.snapshot().unwrap();
        assert_eq!(
            before
                .samples
                .iter()
                .map(|value| value.evidence.source_record_id)
                .collect::<Vec<_>>(),
            after
                .samples
                .iter()
                .map(|value| value.evidence.source_record_id)
                .collect::<Vec<_>>()
        );
        assert_ne!(before.aggregate_digest, after.aggregate_digest);
        assert_eq!(after.complete_count, 4);
    }

    fn diagnostic_excerpt_for_test(payload: &[u8]) -> Vec<u8> {
        crate::decode_runtime::diagnostic_excerpt(payload)
    }
}
