//! RFC 012 Factory.ai candidate adapter.
//!
//! Native meaning stays here. Watchers, cursors, retries, and storage stay in
//! the existing DirectorySnapshot, AppendDelimited, and ReplaceDocument drivers.

use serde_json::Value;

use crate::adapter::{
    AdapterDiagnostic, AdapterError, AdapterErrorClass, AdapterId, AdapterManifest,
    AdapterObjectContext, AdapterSupportBinding, AgentAdapter, Availability, CapabilityDeclaration,
    CapabilityGranularity, CapabilityId, CapabilitySupport, ConsistencyPolicy, DecodeContext,
    DecodeDisposition, DecoderId, DeletionPolicy, DiscoveryContext, DriverSpec, EntityKey,
    EntityScope, Fact, FactBatch, ObjectSelector, RawRetentionPolicy, ScopeProgramManifest,
    SessionFact, SourceInstance, SourceInstanceKey, SourceInstanceSpec, SourceObjectDescriptor,
    SourceRoot, StreamAuthority, StreamId, StreamSpec, SupportLevel,
};
use crate::source::{
    platform_path_key, AppendDelimitedConfig, DirectorySnapshotConfig, IngestPriority,
    ReplaceDocumentConfig, SourceRecord, SourceRecordState,
};

const ADAPTER_ID: &str = "factory";
const SCOPE_PROGRAM_DOCUMENT: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../agent-support/factory/2026-08-21/scope-programs.json"
));
const MEMBERSHIP_STREAM: &str = "session-membership";
const TRANSCRIPT_STREAM: &str = "session-transcripts";
const DOCUMENT_STREAM: &str = "session-documents";
const MEMBERSHIP_DECODER: &str = "factory-session-membership";
const TRANSCRIPT_DECODER: &str = "factory-transcript-record";
const DOCUMENT_DECODER: &str = "factory-session-document";
const NATIVE_PROJECT_KEY: &str = "factory";
const SESSION_DOCUMENT_MAX_BYTES: usize = 1024 * 1024;

const HISTORY_SESSIONS: &str = "history.sessions";
const SOURCE_LIVE: &str = "source.live";
const SOURCE_RECONCILE: &str = "source.reconcile";
const SOURCE_RESUME_CURSOR: &str = "source.resume_cursor";

pub struct FactoryAdapter {
    manifest: AdapterManifest,
}

impl FactoryAdapter {
    pub fn new() -> Self {
        Self {
            manifest: AdapterManifest {
                id: AdapterId::new(ADAPTER_ID).expect("static Factory adapter id is valid"),
                display_name: "Factory.ai".to_string(),
                adapter_version: env!("CARGO_PKG_VERSION").to_string(),
                contract_version: 1,
                support_binding: Some(
                    AdapterSupportBinding::new(
                        "factory-support-2026-08-21",
                        env!("CARGO_PKG_VERSION"),
                        1,
                        "sha256:d5448cc906505854fb4ad19a9756eaea7a3ac8b58bf91b638c511d3eb6bea613",
                        "sha256:7380e8a26ed7d3ba324a9f467a57167612eb0cadf94d2115ce58327732c342b0",
                        "sha256:1605bee5e0091e8690d6c13bee0668aa5d765ed81d6b9869f6f3e3d1d39d679b",
                    )
                    .expect("static Factory support binding is valid"),
                ),
                scope_programs: Some(
                    ScopeProgramManifest::from_json(SCOPE_PROGRAM_DOCUMENT)
                        .expect("static Factory scope program is valid"),
                ),
                source_schema_versions: vec![
                    "factory-session-jsonl-v1".to_string(),
                    "factory-session-json-v1".to_string(),
                ],
                capabilities: factory_capabilities(),
            },
        }
    }

    fn adapter_id(&self) -> &AdapterId {
        &self.manifest.id
    }
}

impl Default for FactoryAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentAdapter for FactoryAdapter {
    fn manifest(&self) -> &AdapterManifest {
        &self.manifest
    }

    fn discover(
        &self,
        context: &DiscoveryContext,
    ) -> Result<Vec<SourceInstanceSpec>, AdapterError> {
        context
            .configured_roots
            .iter()
            .map(|configured_root| {
                let canonical = std::fs::canonicalize(configured_root).map_err(|error| {
                    AdapterError::new(
                        AdapterErrorClass::Transient,
                        "factory_root_unavailable",
                        format!("{}: {error}", configured_root.to_string_lossy()),
                    )
                })?;
                if !canonical.is_dir() {
                    return Err(AdapterError::new(
                        AdapterErrorClass::AdapterFatal,
                        "factory_root_not_directory",
                        canonical.to_string_lossy(),
                    ));
                }
                Ok(SourceInstanceSpec {
                    identity_contract_version: 1,
                    stable_key: SourceInstanceKey::new(platform_path_key(&canonical))?,
                    display_name: format!("Factory.ai ({})", canonical.to_string_lossy()),
                    roots: vec![
                        SourceRoot {
                            name: "home".to_string(),
                            path: canonical.clone(),
                        },
                        SourceRoot {
                            name: "sessions".to_string(),
                            path: canonical.join("sessions"),
                        },
                    ],
                    discovery_reason: "configured Factory data root".to_string(),
                })
            })
            .collect()
    }

    fn streams(&self, instance: &SourceInstance) -> Result<Vec<StreamSpec>, AdapterError> {
        let streams = vec![
            StreamSpec {
                id: StreamId::new(MEMBERSHIP_STREAM)?,
                driver: DriverSpec::DirectorySnapshot(DirectorySnapshotConfig {
                    max_entries: 100_000,
                    max_entries_per_directory: 100_000,
                    max_depth: 8,
                }),
                selector: selector(vec!["**/*.jsonl", "**/session.json"]),
                decoder: DecoderId::new(MEMBERSHIP_DECODER)?,
                authority: StreamAuthority::Supplemental,
                entity_scope: EntityScope::Instance,
                priority: IngestPriority::ForegroundRepair,
                consistency: ConsistencyPolicy::SnapshotDiff,
                deletion: DeletionPolicy::MirrorSource,
                retention: RawRetentionPolicy::None,
                capabilities: source_capabilities(),
            },
            StreamSpec {
                id: StreamId::new(TRANSCRIPT_STREAM)?,
                driver: DriverSpec::AppendDelimited(AppendDelimitedConfig::json_lines()),
                selector: selector(vec!["**/*.jsonl"]),
                decoder: DecoderId::new(TRANSCRIPT_DECODER)?,
                authority: StreamAuthority::Canonical,
                entity_scope: EntityScope::Session,
                priority: IngestPriority::Interactive,
                consistency: ConsistencyPolicy::IncrementalCursor,
                deletion: DeletionPolicy::MirrorSource,
                retention: RawRetentionPolicy::HashOnly,
                capabilities: transcript_capabilities(),
            },
            StreamSpec {
                id: StreamId::new(DOCUMENT_STREAM)?,
                driver: DriverSpec::ReplaceDocument(ReplaceDocumentConfig {
                    max_document_bytes: SESSION_DOCUMENT_MAX_BYTES,
                }),
                selector: selector(vec!["**/session.json"]),
                decoder: DecoderId::new(DOCUMENT_DECODER)?,
                authority: StreamAuthority::Supplemental,
                entity_scope: EntityScope::Session,
                priority: IngestPriority::Interactive,
                consistency: ConsistencyPolicy::SnapshotReplace,
                deletion: DeletionPolicy::MirrorSource,
                retention: RawRetentionPolicy::HashOnly,
                capabilities: session_capabilities(),
            },
        ];
        for stream in &streams {
            stream.validate(instance)?;
        }
        Ok(streams)
    }

    fn bootstrap_object(
        &self,
        _instance: &SourceInstance,
        _object: &SourceObjectDescriptor,
    ) -> Result<AdapterObjectContext, AdapterError> {
        Ok(AdapterObjectContext::empty())
    }

    fn decode(
        &self,
        context: DecodeContext<'_>,
        record: &SourceRecord,
        output: &mut FactBatch,
    ) -> Result<DecodeDisposition, AdapterError> {
        match context.decoder.as_str() {
            MEMBERSHIP_DECODER => Ok(DecodeDisposition::IgnoredKnown),
            TRANSCRIPT_DECODER => decode_transcript(self.adapter_id(), record, output),
            DOCUMENT_DECODER => decode_session_document(self.adapter_id(), record, output),
            _ => Err(AdapterError::unknown_decoder(context.decoder)),
        }
    }
}

fn selector(include: Vec<&str>) -> ObjectSelector {
    ObjectSelector {
        root_name: "sessions".to_string(),
        include: include.into_iter().map(str::to_owned).collect(),
        exclude: Vec::new(),
    }
}

fn capability(
    id: &'static str,
    level: SupportLevel,
    granularity: CapabilityGranularity,
    notes: Option<&'static str>,
) -> CapabilityDeclaration {
    CapabilityDeclaration {
        id: CapabilityId::new(id).expect("static Factory capability id is valid"),
        support: CapabilitySupport {
            level,
            granularity,
            availability: Availability::Live,
            notes: notes.map(str::to_owned),
        },
    }
}

fn factory_capabilities() -> Vec<CapabilityDeclaration> {
    vec![
        capability(
            HISTORY_SESSIONS,
            SupportLevel::Native,
            CapabilityGranularity::Session,
            None,
        ),
        capability(
            SOURCE_LIVE,
            SupportLevel::Native,
            CapabilityGranularity::Instance,
            None,
        ),
        capability(
            SOURCE_RECONCILE,
            SupportLevel::Native,
            CapabilityGranularity::Instance,
            None,
        ),
        capability(
            SOURCE_RESUME_CURSOR,
            SupportLevel::Native,
            CapabilityGranularity::Record,
            None,
        ),
    ]
}

fn ids(values: &[&'static str]) -> Vec<CapabilityId> {
    values
        .iter()
        .map(|id| CapabilityId::new(*id).expect("static Factory stream capability id is valid"))
        .collect()
}

fn source_capabilities() -> Vec<CapabilityId> {
    ids(&[SOURCE_LIVE, SOURCE_RECONCILE, SOURCE_RESUME_CURSOR])
}

fn transcript_capabilities() -> Vec<CapabilityId> {
    ids(&[
        HISTORY_SESSIONS,
        SOURCE_LIVE,
        SOURCE_RECONCILE,
        SOURCE_RESUME_CURSOR,
    ])
}

fn session_capabilities() -> Vec<CapabilityId> {
    ids(&[HISTORY_SESSIONS, SOURCE_LIVE, SOURCE_RECONCILE])
}

fn decode_transcript(
    adapter_id: &AdapterId,
    record: &SourceRecord,
    output: &mut FactBatch,
) -> Result<DecodeDisposition, AdapterError> {
    if record.state == SourceRecordState::Absent {
        return Ok(DecodeDisposition::IgnoredKnown);
    }
    decode_session_record(adapter_id, record, output, true)
}

fn decode_session_document(
    adapter_id: &AdapterId,
    record: &SourceRecord,
    output: &mut FactBatch,
) -> Result<DecodeDisposition, AdapterError> {
    if record.state == SourceRecordState::Absent {
        return Ok(DecodeDisposition::IgnoredKnown);
    }
    decode_session_record(adapter_id, record, output, false)
}

fn decode_session_record(
    adapter_id: &AdapterId,
    record: &SourceRecord,
    output: &mut FactBatch,
    require_turn_type: bool,
) -> Result<DecodeDisposition, AdapterError> {
    let value: Value = match serde_json::from_slice(&record.payload) {
        Ok(value) => value,
        Err(_) => {
            return preserve_unknown(
                record,
                output,
                None,
                "Factory record is not JSON".to_string(),
            );
        }
    };
    let Some(object) = value.as_object() else {
        return preserve_unknown(
            record,
            output,
            None,
            "Factory record is not an object".to_string(),
        );
    };
    let session_id = object
        .get("sessionId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let record_type = object.get("type").and_then(Value::as_str);
    let mapped = match (session_id, record_type, require_turn_type) {
        (Some(session_id), Some("user" | "assistant"), true) => Some(session_id),
        (Some(session_id), _, false) => Some(session_id),
        _ => None,
    };
    let Some(session_id) = mapped else {
        return preserve_unknown(
            record,
            output,
            record_type.map(str::to_owned),
            "Factory record is not a mapped sessionId turn".to_string(),
        );
    };
    let session = EntityKey::native(
        adapter_id,
        record.source_instance_id,
        "session",
        session_id.as_bytes(),
    )?;
    let project = EntityKey::native(
        adapter_id,
        record.source_instance_id,
        "project",
        NATIVE_PROJECT_KEY.as_bytes(),
    )?;
    output.push(
        record,
        Fact::Session(SessionFact {
            session,
            project,
            native_session_id: session_id.to_string(),
            native_project_key: NATIVE_PROJECT_KEY.to_string(),
            cwd: None,
            git_branch: None,
            first_prompt: None,
            ai_title: None,
            custom_title: None,
            source_time: None,
        }),
    )?;
    Ok(DecodeDisposition::Applied)
}

fn preserve_unknown(
    record: &SourceRecord,
    output: &mut FactBatch,
    native_kind: Option<String>,
    reason: String,
) -> Result<DecodeDisposition, AdapterError> {
    output.push(
        record,
        Fact::UnknownRecord {
            native_kind,
            raw_payload: record.payload.clone(),
            reason: reason.clone(),
        },
    )?;
    output.push_diagnostic(AdapterDiagnostic {
        class: AdapterErrorClass::RecordPermanent,
        code: "factory_preserved_unknown".to_string(),
        message: reason,
    })?;
    Ok(DecodeDisposition::PreservedUnknown)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use crate::adapter::{AdapterRegistry, DiscoveryContext, Fact, SourceInstance};
    use crate::source::{AppendDelimitedFile, AppendItem, RecordOrigin, SourceMediaType};

    use super::*;

    fn production_adapter_source() -> &'static str {
        include_str!("adapter.rs")
            .split("\n#[cfg(test)]")
            .next()
            .expect("factory adapter keeps tests behind cfg(test)")
    }

    #[test]
    fn registers_and_discovers_a_temp_fixture_tree() {
        let root = TempDir::new().unwrap();
        fs::create_dir_all(root.path().join("sessions")).unwrap();
        fs::write(
            root.path().join("sessions/session-001.jsonl"),
            "{\"sessionId\":\"factory-session-001\",\"type\":\"user\"}\n",
        )
        .unwrap();
        fs::write(
            root.path().join("sessions/session.json"),
            "{\"sessionId\":\"factory-session-001\"}\n",
        )
        .unwrap();

        let registry = AdapterRegistry::builder()
            .register(FactoryAdapter::new())
            .build()
            .unwrap();
        let adapter_id = AdapterId::new(ADAPTER_ID).unwrap();
        let adapter = registry.resolve(adapter_id.as_str()).unwrap();
        let spec = adapter
            .discover(&DiscoveryContext {
                configured_roots: vec![root.path().to_path_buf()],
                observed_at: 1,
            })
            .unwrap()
            .remove(0);
        let instance = SourceInstance { id: 1, spec };
        let streams = adapter.streams(&instance).unwrap();
        assert_eq!(adapter.manifest().id.as_str(), ADAPTER_ID);
        assert_eq!(streams.len(), 3);
        assert!(streams
            .iter()
            .any(|stream| matches!(stream.driver, DriverSpec::DirectorySnapshot(_))));
        assert!(streams
            .iter()
            .any(|stream| matches!(stream.driver, DriverSpec::AppendDelimited(_))));
        assert!(streams
            .iter()
            .any(|stream| matches!(stream.driver, DriverSpec::ReplaceDocument(_))));
        assert_eq!(
            instance.root("sessions").unwrap(),
            instance.root("home").unwrap().join("sessions")
        );
    }

    #[test]
    fn decodes_session_fact_through_common_append_driver() {
        let root = TempDir::new().unwrap();
        let path = root.path().join("session-001.jsonl");
        fs::write(
            &path,
            "{\"sessionId\":\"factory-session-001\",\"type\":\"assistant\"}\n",
        )
        .unwrap();
        let driver = AppendDelimitedFile::new(AppendDelimitedConfig::json_lines()).unwrap();
        let origin = RecordOrigin {
            source_instance_id: 7,
            stream_id: 8,
            object_id: 9,
            observed_at: 10,
            source_timestamp_hint: None,
            media_type: SourceMediaType::new("application/x-ndjson").unwrap(),
        };
        let records = match driver.read(&path, None, &origin, false).unwrap() {
            crate::source::AppendRead::Batch { items, .. } => items
                .into_iter()
                .filter_map(|item| match item {
                    AppendItem::Record(record) => Some(record),
                    AppendItem::Quarantined(_) => None,
                })
                .collect::<Vec<_>>(),
            other => panic!("unexpected append read {other:?}"),
        };
        assert_eq!(records.len(), 1);

        let adapter = FactoryAdapter::new();
        let decoder = DecoderId::new(TRANSCRIPT_DECODER).unwrap();
        let context = AdapterObjectContext::empty();
        let mut batch = FactBatch::new(16, 4).unwrap();
        let disposition = adapter
            .decode(
                DecodeContext {
                    decoder: &decoder,
                    object_context: &context,
                    decoder_state: None,
                },
                &records[0],
                &mut batch,
            )
            .unwrap();
        assert_eq!(disposition, DecodeDisposition::Applied);
        let session = batch.facts().iter().find_map(|fact| match &fact.value {
            Fact::Session(session) => Some(session),
            _ => None,
        });
        let session = session.expect("Factory jsonl line must decode a SessionFact");
        assert_eq!(session.native_session_id, "factory-session-001");
        assert_eq!(session.native_project_key, NATIVE_PROJECT_KEY);
        assert!(session.cwd.is_none());
        assert!(session.first_prompt.is_none());
    }

    #[test]
    fn does_not_require_vendor_observer_or_query_files() {
        let production = production_adapter_source();
        for needle in [
            "crate::claude",
            "crate::codex",
            "crate::grok",
            "crate::engine",
            "crate::scoped_observation",
            "crate::napi_engine",
            "crate::catalog_contract",
            "crate::observation_contract",
        ] {
            assert!(
                !production.contains(needle),
                "Factory adapter must not require {needle}"
            );
        }
        assert!(production.contains("DirectorySnapshot"));
        assert!(production.contains("AppendDelimited"));
        assert!(production.contains("ReplaceDocument"));
        let module = include_str!("mod.rs");
        assert!(!module.contains("crate::engine"));
        assert!(!module.contains("crate::scoped_observation"));
    }
}
