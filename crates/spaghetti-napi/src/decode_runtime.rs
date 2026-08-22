//! Store-agnostic adapter decoder invocation for RFC 012.
//!
//! Durable ingestion and database-free scoped observation both enter adapters
//! through this boundary. It owns panic containment, disposition validation,
//! raw-retention enforcement, and decoder-state extraction; it owns no source
//! discovery, persistence, projection, or delivery topology.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::{Duration, Instant};

use crate::adapter::{
    AdapterError, AdapterErrorClass, AdapterObjectContext, AgentAdapter, CapabilityId,
    DecodeContext, DecodeDisposition, DecoderId, FactBatch, FactSemanticContext,
    RawRetentionPolicy, SourceAccess, SourceInstance, SourceObjectDescriptor,
};
use crate::source::SourceRecord;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DecodeRuntimeLimits {
    pub max_facts: usize,
    pub max_diagnostics: usize,
}

pub(crate) struct DecodeRuntimeRequest<'a, A: AgentAdapter + ?Sized> {
    pub adapter: &'a A,
    pub decoder: &'a DecoderId,
    pub object_context: &'a AdapterObjectContext,
    pub source_access: &'a dyn SourceAccess,
    pub record: &'a SourceRecord,
    pub semantic_context: &'a FactSemanticContext,
    pub decoder_state: Option<&'a [u8]>,
    pub retention: RawRetentionPolicy,
    pub limits: DecodeRuntimeLimits,
}

pub(crate) struct DecodedFactBatch {
    pub disposition: DecodeDisposition,
    pub mapping_disposition: RecordMappingDisposition,
    pub batch: FactBatch,
    pub next_decoder_state: Option<Vec<u8>>,
    pub quarantined: bool,
    pub unscoped_permanent_diagnostic: bool,
    pub diagnostic_coverage_gaps: Vec<CapabilityId>,
}

/// RFC 012A's topology-independent outcome for one complete source record.
///
/// Adapters still return the smaller legacy enum while the remaining native
/// decoders migrate.  This common boundary turns that value into the exact
/// structured contract consumed by durable and scoped execution, so neither
/// topology invents its own classification or exposes unbounded native data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RecordMappingDisposition {
    Mapped {
        fact_count: u32,
    },
    IgnoredKnown {
        reason_code: String,
    },
    RetainedUnknown {
        family_hint: Option<String>,
        bounded_evidence: BoundedNativeEvidence,
    },
    BufferedIncomplete,
    Malformed {
        reason_code: String,
        bounded_diagnostic: Vec<u8>,
    },
    UnsupportedVersion {
        observed_version: String,
    },
}

/// Value-free evidence retained for an unknown source record regardless of
/// the selected raw-retention policy.  The excerpt contains only hashed JSON
/// keys, value kinds, sizes, and a digest; it never contains native values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BoundedNativeEvidence {
    pub observed_bytes: u64,
    pub payload_digest: [u8; 32],
    pub sanitized_excerpt: Vec<u8>,
}

/// A decode attempt always reports timing, including controlled failures.
pub(crate) struct DecodeRuntimeAttempt {
    pub result: Result<DecodedFactBatch, AdapterError>,
    pub adapter_elapsed: Duration,
    pub fact_build_time: Duration,
}

/// Invoke only the adapter's explicit dependency-free bootstrap opt-in. Panic
/// payloads are never surfaced because object descriptors can contain private
/// native identity material.
pub(crate) fn bootstrap_object_without_source_access<A: AgentAdapter + ?Sized>(
    adapter: &A,
    instance: &SourceInstance,
    object: &SourceObjectDescriptor,
) -> Result<AdapterObjectContext, AdapterError> {
    match catch_unwind(AssertUnwindSafe(|| {
        adapter.bootstrap_object_without_source_access(instance, object)
    })) {
        Ok(result) => result,
        Err(_) => Err(AdapterError::new(
            AdapterErrorClass::AdapterFatal,
            "adapter_panic",
            "adapter panicked at the controlled object-bootstrap boundary",
        )),
    }
}

pub(crate) fn decode_record<A: AgentAdapter + ?Sized>(
    request: DecodeRuntimeRequest<'_, A>,
) -> DecodeRuntimeAttempt {
    let mut batch = match FactBatch::new_with_semantic_context(
        request.limits.max_facts,
        request.limits.max_diagnostics,
        request.semantic_context.clone(),
    ) {
        Ok(batch) => batch,
        Err(error) => {
            return DecodeRuntimeAttempt {
                result: Err(error),
                adapter_elapsed: Duration::ZERO,
                fact_build_time: Duration::ZERO,
            };
        }
    };

    let adapter_started = Instant::now();
    let adapter_result = catch_unwind(AssertUnwindSafe(|| {
        request.adapter.decode_with_access(
            DecodeContext {
                decoder: request.decoder,
                object_context: request.object_context,
                decoder_state: request.decoder_state,
            },
            request.record,
            &mut batch,
            request.source_access,
        )
    }));
    let adapter_elapsed = adapter_started.elapsed();
    let fact_build_time = batch.fact_build_time();

    let result = match adapter_result {
        Err(_) => Err(AdapterError::new(
            AdapterErrorClass::AdapterFatal,
            "adapter_panic",
            // Panic payloads are deliberately omitted: adapters parse private
            // native content and formatted payloads are unsafe telemetry.
            "adapter panicked at the controlled decode boundary",
        )),
        Ok(Err(error)) => Err(error),
        Ok(Ok(disposition)) => finish_decode(request.record, request.retention, disposition, batch),
    };
    DecodeRuntimeAttempt {
        result,
        adapter_elapsed,
        fact_build_time,
    }
}

fn finish_decode(
    record: &SourceRecord,
    retention: RawRetentionPolicy,
    disposition: DecodeDisposition,
    mut batch: FactBatch,
) -> Result<DecodedFactBatch, AdapterError> {
    let fact_count = batch.facts().len();
    match disposition {
        DecodeDisposition::Applied if fact_count == 0 => {
            return Err(AdapterError::invalid_contract(
                "adapter returned Applied without facts",
            ));
        }
        DecodeDisposition::IgnoredKnown | DecodeDisposition::RetryTransient if fact_count != 0 => {
            return Err(AdapterError::invalid_contract(format!(
                "adapter returned {disposition:?} with {fact_count} facts"
            )));
        }
        _ => {}
    }

    let mapping_disposition = record_mapping_disposition(record, disposition, &batch)?;

    match retention {
        RawRetentionPolicy::Full => {}
        RawRetentionPolicy::DiagnosticExcerpt => {
            let excerpt = diagnostic_excerpt(&record.payload);
            batch.replace_unknown_record_payloads(&excerpt);
        }
        RawRetentionPolicy::HashOnly | RawRetentionPolicy::None => {
            batch.redact_unknown_record_payloads();
        }
    }
    let quarantined = batch
        .diagnostics()
        .iter()
        .any(|diagnostic| diagnostic.class == AdapterErrorClass::RecordPermanent);
    let unscoped_permanent_diagnostic = batch.has_unscoped_permanent_diagnostic();
    let diagnostic_coverage_gaps = batch.diagnostic_coverage_gaps().iter().cloned().collect();
    let next_decoder_state = batch.next_decoder_state().map(ToOwned::to_owned);
    Ok(DecodedFactBatch {
        disposition,
        mapping_disposition,
        batch,
        next_decoder_state,
        quarantined,
        unscoped_permanent_diagnostic,
        diagnostic_coverage_gaps,
    })
}

const LEGACY_IGNORED_REASON_CODE: &str = "known_record_ignored";

fn record_mapping_disposition(
    record: &SourceRecord,
    disposition: DecodeDisposition,
    batch: &FactBatch,
) -> Result<RecordMappingDisposition, AdapterError> {
    match disposition {
        DecodeDisposition::Applied => {
            if batch.facts().iter().any(|envelope| {
                matches!(&envelope.value, crate::adapter::Fact::UnknownRecord { .. })
            }) {
                return Err(AdapterError::invalid_contract(
                    "mapped disposition cannot carry unknown-record evidence",
                ));
            }
            let fact_count = u32::try_from(batch.facts().len()).map_err(|_| {
                AdapterError::invalid_contract("mapped fact count exceeds the portable range")
            })?;
            Ok(RecordMappingDisposition::Mapped { fact_count })
        }
        DecodeDisposition::IgnoredKnown => Ok(RecordMappingDisposition::IgnoredKnown {
            // Existing adapter contracts did not carry a reason. Keep one
            // closed migration code until each decoder contract supplies a
            // more specific machine classification.
            reason_code: LEGACY_IGNORED_REASON_CODE.to_string(),
        }),
        DecodeDisposition::PreservedUnknown => {
            let [envelope] = batch.facts() else {
                return Err(AdapterError::invalid_contract(
                    "retained-unknown disposition requires exactly one evidence fact",
                ));
            };
            let crate::adapter::Fact::UnknownRecord {
                native_kind,
                raw_payload,
                ..
            } = &envelope.value
            else {
                return Err(AdapterError::invalid_contract(
                    "retained-unknown disposition requires unknown-record evidence",
                ));
            };
            if raw_payload.as_slice() != record.payload.as_slice() {
                return Err(AdapterError::invalid_contract(
                    "retained-unknown evidence must bind the complete source record",
                ));
            }
            let observed_bytes = u64::try_from(record.payload.len()).map_err(|_| {
                AdapterError::invalid_contract(
                    "retained-unknown source record exceeds the portable byte range",
                )
            })?;
            Ok(RecordMappingDisposition::RetainedUnknown {
                family_hint: native_kind
                    .as_deref()
                    .filter(|value| is_safe_family_hint(value))
                    .map(ToOwned::to_owned),
                bounded_evidence: BoundedNativeEvidence {
                    observed_bytes,
                    payload_digest: *record.payload_hash.as_bytes(),
                    sanitized_excerpt: diagnostic_excerpt(&record.payload),
                },
            })
        }
        DecodeDisposition::RetryTransient => Ok(RecordMappingDisposition::BufferedIncomplete),
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

pub(crate) const MAX_DIAGNOSTIC_EXCERPT_BYTES: usize = 1_024;
const MAX_DIAGNOSTIC_SHAPE_ITEMS: usize = 16;

/// Produce useful quarantine context without retaining native values or even
/// native JSON property names. Dynamic property names can themselves contain
/// secrets, so only their hashes and value kinds are exposed.
pub(crate) fn diagnostic_excerpt(payload: &[u8]) -> Vec<u8> {
    let payload_hash = blake3::hash(payload).to_hex().to_string();
    let shape = match serde_json::from_slice::<serde_json::Value>(payload) {
        Ok(serde_json::Value::Object(object)) => {
            let keys = object
                .iter()
                .take(MAX_DIAGNOSTIC_SHAPE_ITEMS)
                .map(|(key, value)| {
                    let key_hash = blake3::hash(key.as_bytes()).to_hex().to_string();
                    serde_json::json!({
                        "key_hash": &key_hash[..12],
                        "value_kind": json_value_kind(value),
                    })
                })
                .collect::<Vec<_>>();
            serde_json::json!({
                "kind": "json_object",
                "bytes": payload.len(),
                "hash": payload_hash,
                "members": object.len(),
                "shape": keys,
                "truncated": object.len() > MAX_DIAGNOSTIC_SHAPE_ITEMS,
            })
        }
        Ok(serde_json::Value::Array(array)) => {
            let items = array
                .iter()
                .take(MAX_DIAGNOSTIC_SHAPE_ITEMS)
                .map(json_value_kind)
                .collect::<Vec<_>>();
            serde_json::json!({
                "kind": "json_array",
                "bytes": payload.len(),
                "hash": payload_hash,
                "items": array.len(),
                "item_kinds": items,
                "truncated": array.len() > MAX_DIAGNOSTIC_SHAPE_ITEMS,
            })
        }
        Ok(value) => serde_json::json!({
            "kind": json_value_kind(&value),
            "bytes": payload.len(),
            "hash": payload_hash,
        }),
        Err(_) => serde_json::json!({
            "kind": "opaque",
            "bytes": payload.len(),
            "hash": payload_hash,
        }),
    };
    let encoded = serde_json::to_vec(&shape).unwrap_or_else(|_| {
        format!(r#"{{"kind":"redacted","bytes":{}}}"#, payload.len()).into_bytes()
    });
    debug_assert!(encoded.len() <= MAX_DIAGNOSTIC_EXCERPT_BYTES);
    if encoded.len() <= MAX_DIAGNOSTIC_EXCERPT_BYTES {
        encoded
    } else {
        encoded[..MAX_DIAGNOSTIC_EXCERPT_BYTES].to_vec()
    }
}

fn json_value_kind(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use crate::adapter::{
        AdapterDiagnostic, AdapterId, AdapterManifest, DiscoveryContext, EntityKey, EvidenceKind,
        EvidenceStrength, Fact, RunEvidenceFact, SourceInstance, SourceInstanceKey,
        SourceInstanceSpec, SourceObjectDescriptor, SourceObjectList, SourceObjectListRequest,
        SourceQuery, SourceRows, SourceSnapshot, StreamId, StreamSpec,
    };
    use crate::source::{RecordOrigin, SourceCursor, SourceMediaType};

    use super::*;

    #[derive(Clone, Copy)]
    enum FixtureMode {
        StatefulUnknown,
        AppliedEmpty,
        RetryWithFact,
        Panic,
    }

    #[derive(Clone, Copy)]
    enum FixtureBootstrapMode {
        Unsupported,
        Empty,
        Panic,
    }

    struct FixtureAdapter {
        manifest: AdapterManifest,
        mode: FixtureMode,
        bootstrap_mode: FixtureBootstrapMode,
    }

    impl FixtureAdapter {
        fn new(mode: FixtureMode) -> Self {
            Self {
                manifest: AdapterManifest {
                    id: AdapterId::new("decode-fixture").unwrap(),
                    display_name: "decode fixture".to_string(),
                    adapter_version: "1.0.0".to_string(),
                    contract_version: 1,
                    support_binding: None,
                    scope_programs: None,
                    source_schema_versions: Vec::new(),
                    capabilities: Vec::new(),
                },
                mode,
                bootstrap_mode: FixtureBootstrapMode::Unsupported,
            }
        }

        fn with_bootstrap_mode(mut self, bootstrap_mode: FixtureBootstrapMode) -> Self {
            self.bootstrap_mode = bootstrap_mode;
            self
        }
    }

    impl AgentAdapter for FixtureAdapter {
        fn manifest(&self) -> &AdapterManifest {
            &self.manifest
        }

        fn discover(
            &self,
            _context: &DiscoveryContext,
        ) -> Result<Vec<SourceInstanceSpec>, AdapterError> {
            Ok(Vec::new())
        }

        fn streams(&self, _instance: &SourceInstance) -> Result<Vec<StreamSpec>, AdapterError> {
            Ok(Vec::new())
        }

        fn bootstrap_object_without_source_access(
            &self,
            _instance: &SourceInstance,
            _object: &SourceObjectDescriptor,
        ) -> Result<AdapterObjectContext, AdapterError> {
            match self.bootstrap_mode {
                FixtureBootstrapMode::Unsupported => Err(AdapterError::invalid_contract(
                    "fixture does not opt in to dependency-free bootstrap",
                )),
                FixtureBootstrapMode::Empty => Ok(AdapterObjectContext::empty()),
                FixtureBootstrapMode::Panic => {
                    panic!("private bootstrap panic /Users/alice/private/session.jsonl")
                }
            }
        }

        fn decode(
            &self,
            context: DecodeContext<'_>,
            record: &SourceRecord,
            output: &mut FactBatch,
        ) -> Result<DecodeDisposition, AdapterError> {
            match self.mode {
                FixtureMode::AppliedEmpty => Ok(DecodeDisposition::Applied),
                FixtureMode::Panic => panic!("fixture decode panic"),
                FixtureMode::StatefulUnknown | FixtureMode::RetryWithFact => {
                    output.push_derived(
                        record,
                        b"unknown-record",
                        Fact::UnknownRecord {
                            native_kind: Some("fixture".to_string()),
                            raw_payload: record.payload.clone(),
                            reason: "fixture".to_string(),
                        },
                    )?;
                    let mut state = context.decoder_state.unwrap_or_default().to_vec();
                    state.extend_from_slice(&record.payload);
                    output.set_next_decoder_state(state)?;
                    if record.payload == b"quarantine" {
                        output.push_diagnostic(AdapterDiagnostic {
                            class: AdapterErrorClass::RecordPermanent,
                            code: "fixture_quarantine".to_string(),
                            message: "fixture record is quarantined".to_string(),
                        })?;
                    }
                    Ok(match self.mode {
                        FixtureMode::StatefulUnknown => DecodeDisposition::PreservedUnknown,
                        FixtureMode::RetryWithFact => DecodeDisposition::RetryTransient,
                        _ => unreachable!(),
                    })
                }
            }
        }
    }

    struct NoSourceAccess;

    impl SourceAccess for NoSourceAccess {
        fn read_object(
            &self,
            _root_name: &str,
            _relative_path: &std::path::Path,
            _max_bytes: usize,
        ) -> Result<SourceSnapshot, AdapterError> {
            Err(AdapterError::invalid_contract("unexpected source access"))
        }

        fn query_source_db(&self, _query: &SourceQuery) -> Result<SourceRows, AdapterError> {
            Err(AdapterError::invalid_contract("unexpected source access"))
        }

        fn list_objects(
            &self,
            _request: &SourceObjectListRequest,
        ) -> Result<SourceObjectList, AdapterError> {
            Err(AdapterError::invalid_contract("unexpected source access"))
        }
    }

    fn record(payload: &[u8]) -> SourceRecord {
        SourceRecord::new(
            &RecordOrigin {
                source_instance_id: 1,
                stream_id: 2,
                object_id: 3,
                observed_at: 4,
                source_timestamp_hint: None,
                media_type: SourceMediaType::new("application/x-ndjson").unwrap(),
            },
            1,
            SourceCursor::append_offset(0),
            SourceCursor::append_offset(payload.len() as u64),
            0,
            payload.to_vec(),
        )
    }

    fn semantic_context() -> FactSemanticContext {
        FactSemanticContext::new(
            &AdapterId::new("decode-fixture").unwrap(),
            1,
            b"fixture-source-instance",
            b"fixture-records",
            b"record.jsonl",
            1,
        )
        .unwrap()
    }

    fn run<'a>(
        adapter: &'a FixtureAdapter,
        record: &'a SourceRecord,
        state: Option<&'a [u8]>,
        retention: RawRetentionPolicy,
    ) -> DecodeRuntimeAttempt {
        let decoder = DecoderId::new("fixture-v1").unwrap();
        let object_context = AdapterObjectContext::empty();
        let semantic_context = semantic_context();
        decode_record(DecodeRuntimeRequest {
            adapter,
            decoder: &decoder,
            object_context: &object_context,
            source_access: &NoSourceAccess,
            record,
            semantic_context: &semantic_context,
            decoder_state: state,
            retention,
            limits: DecodeRuntimeLimits {
                max_facts: 8,
                max_diagnostics: 8,
            },
        })
    }

    fn bootstrap_coordinates() -> (SourceInstance, SourceObjectDescriptor) {
        (
            SourceInstance {
                id: 1,
                spec: SourceInstanceSpec {
                    identity_contract_version: 1,
                    stable_key: SourceInstanceKey::new(b"private-source-instance".to_vec())
                        .unwrap(),
                    display_name: "private source".to_string(),
                    roots: Vec::new(),
                    discovery_reason: "fixture".to_string(),
                },
            },
            SourceObjectDescriptor {
                stream_id: StreamId::new("private-stream").unwrap(),
                object_key: b"private/session.jsonl".to_vec(),
                relative_path: std::path::PathBuf::from("private/session.jsonl"),
            },
        )
    }

    #[test]
    fn dependency_free_bootstrap_requires_opt_in_and_contains_private_panics() {
        let (instance, object) = bootstrap_coordinates();
        let unsupported = bootstrap_object_without_source_access(
            &FixtureAdapter::new(FixtureMode::AppliedEmpty),
            &instance,
            &object,
        )
        .unwrap_err();
        assert_eq!(unsupported.class, AdapterErrorClass::InvalidContract);

        let opted_in = bootstrap_object_without_source_access(
            &FixtureAdapter::new(FixtureMode::AppliedEmpty)
                .with_bootstrap_mode(FixtureBootstrapMode::Empty),
            &instance,
            &object,
        )
        .unwrap();
        assert_eq!(opted_in, AdapterObjectContext::empty());

        let panic = bootstrap_object_without_source_access(
            &FixtureAdapter::new(FixtureMode::AppliedEmpty)
                .with_bootstrap_mode(FixtureBootstrapMode::Panic),
            &instance,
            &object,
        )
        .unwrap_err();
        assert_eq!(panic.class, AdapterErrorClass::AdapterFatal);
        assert_eq!(panic.code, "adapter_panic");
        for private in ["/Users/", "alice", "private", "session.jsonl"] {
            assert!(!panic.message.contains(private));
        }
    }

    #[test]
    fn shared_decode_boundary_is_deterministic_stateful_and_retention_safe() {
        let adapter = FixtureAdapter::new(FixtureMode::StatefulUnknown);
        let record = record(b"quarantine");
        let first = run(
            &adapter,
            &record,
            Some(b"prior-"),
            RawRetentionPolicy::HashOnly,
        )
        .result
        .unwrap();
        let replay = run(
            &adapter,
            &record,
            Some(b"prior-"),
            RawRetentionPolicy::HashOnly,
        )
        .result
        .unwrap();

        assert_eq!(first.disposition, DecodeDisposition::PreservedUnknown);
        let RecordMappingDisposition::RetainedUnknown {
            family_hint,
            bounded_evidence,
        } = &first.mapping_disposition
        else {
            panic!("expected structured retained-unknown disposition");
        };
        assert_eq!(family_hint.as_deref(), Some("fixture"));
        assert_eq!(bounded_evidence.observed_bytes, 10);
        assert_eq!(
            bounded_evidence.payload_digest,
            *record.payload_hash.as_bytes()
        );
        assert_eq!(
            bounded_evidence.sanitized_excerpt,
            diagnostic_excerpt(&record.payload)
        );
        assert!(bounded_evidence.sanitized_excerpt.len() <= MAX_DIAGNOSTIC_EXCERPT_BYTES);
        assert!(first.quarantined);
        assert_eq!(
            first.next_decoder_state.as_deref(),
            Some(b"prior-quarantine".as_slice())
        );
        assert_eq!(first.batch.facts()[0].id, replay.batch.facts()[0].id);
        assert!(first.batch.facts()[0].semantic_revision.is_some());
        assert_eq!(
            first.batch.facts()[0].semantic_revision,
            replay.batch.facts()[0].semantic_revision
        );
        let Fact::UnknownRecord { raw_payload, .. } = &first.batch.facts()[0].value else {
            panic!("expected retained unknown fact");
        };
        assert!(raw_payload.is_empty());
    }

    #[test]
    fn structured_mapping_dispositions_are_closed_bounded_and_privacy_safe() {
        let record = record(br#"{"/Users/alice/private/session.jsonl":"secret"}"#);
        let context = semantic_context();

        let mut mapped = FactBatch::new_with_semantic_context(1, 1, context.clone()).unwrap();
        mapped
            .push_derived(
                &record,
                b"mapped",
                Fact::RunEvidence(RunEvidenceFact {
                    run: EntityKey::native(
                        &AdapterId::new("decode-fixture").unwrap(),
                        1,
                        "run",
                        b"run-1",
                    )
                    .unwrap(),
                    kind: EvidenceKind::ActivityObserved,
                    strength: EvidenceStrength::NativeActivity,
                    native_state: None,
                    source_time: None,
                }),
            )
            .unwrap();
        let mapped = finish_decode(
            &record,
            RawRetentionPolicy::None,
            DecodeDisposition::Applied,
            mapped,
        )
        .unwrap();
        assert_eq!(
            mapped.mapping_disposition,
            RecordMappingDisposition::Mapped { fact_count: 1 }
        );

        let ignored = finish_decode(
            &record,
            RawRetentionPolicy::None,
            DecodeDisposition::IgnoredKnown,
            FactBatch::new_with_semantic_context(1, 1, context.clone()).unwrap(),
        )
        .unwrap();
        assert_eq!(
            ignored.mapping_disposition,
            RecordMappingDisposition::IgnoredKnown {
                reason_code: "known_record_ignored".to_string(),
            }
        );

        let buffered = finish_decode(
            &record,
            RawRetentionPolicy::None,
            DecodeDisposition::RetryTransient,
            FactBatch::new_with_semantic_context(1, 1, context.clone()).unwrap(),
        )
        .unwrap();
        assert_eq!(
            buffered.mapping_disposition,
            RecordMappingDisposition::BufferedIncomplete
        );

        let mut unknown = FactBatch::new_with_semantic_context(1, 1, context).unwrap();
        unknown
            .push_derived(
                &record,
                b"unknown",
                Fact::UnknownRecord {
                    native_kind: Some("/Users/alice/private/session.jsonl".to_string()),
                    raw_payload: record.payload.clone(),
                    reason: "private native shape".to_string(),
                },
            )
            .unwrap();
        let unknown = finish_decode(
            &record,
            RawRetentionPolicy::Full,
            DecodeDisposition::PreservedUnknown,
            unknown,
        )
        .unwrap();
        let RecordMappingDisposition::RetainedUnknown {
            family_hint,
            bounded_evidence,
        } = unknown.mapping_disposition
        else {
            panic!("expected retained unknown");
        };
        assert_eq!(family_hint, None);
        let excerpt = String::from_utf8(bounded_evidence.sanitized_excerpt).unwrap();
        for private in ["/Users/", "alice", "private", "session.jsonl", "secret"] {
            assert!(!excerpt.contains(private));
        }

        for invalid_batch in [
            FactBatch::new_with_semantic_context(1, 1, semantic_context()).unwrap(),
            {
                let mut batch =
                    FactBatch::new_with_semantic_context(1, 1, semantic_context()).unwrap();
                batch
                    .push_derived(
                        &record,
                        b"partial",
                        Fact::UnknownRecord {
                            native_kind: Some("fixture".to_string()),
                            raw_payload: b"not-the-record".to_vec(),
                            reason: "fixture".to_string(),
                        },
                    )
                    .unwrap();
                batch
            },
        ] {
            assert!(finish_decode(
                &record,
                RawRetentionPolicy::None,
                DecodeDisposition::PreservedUnknown,
                invalid_batch,
            )
            .is_err());
        }
    }

    #[test]
    fn shared_decode_boundary_rejects_invalid_dispositions_and_contains_panics() {
        let record = record(b"value");
        let empty = run(
            &FixtureAdapter::new(FixtureMode::AppliedEmpty),
            &record,
            None,
            RawRetentionPolicy::None,
        )
        .result;
        let Err(empty) = empty else {
            panic!("Applied without facts must fail");
        };
        assert_eq!(empty.class, AdapterErrorClass::InvalidContract);

        let retry = run(
            &FixtureAdapter::new(FixtureMode::RetryWithFact),
            &record,
            None,
            RawRetentionPolicy::None,
        )
        .result;
        let Err(retry) = retry else {
            panic!("RetryTransient with facts must fail");
        };
        assert_eq!(retry.class, AdapterErrorClass::InvalidContract);

        let panic = run(
            &FixtureAdapter::new(FixtureMode::Panic),
            &record,
            None,
            RawRetentionPolicy::None,
        )
        .result;
        let Err(panic) = panic else {
            panic!("adapter panic must be contained");
        };
        assert_eq!(panic.class, AdapterErrorClass::AdapterFatal);
        assert_eq!(panic.code, "adapter_panic");
        assert!(!panic.message.contains("fixture decode panic"));
    }
}
