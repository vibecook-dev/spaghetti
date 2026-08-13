//! RFC 011 Grok adapter.
//!
//! Grok keeps one canonical append transcript plus replaceable/append
//! sidecars in each session directory. Sidecar content is declared as its own
//! stream, while a bounded, confined dependency snapshot supplies transcript
//! timestamp enrichment. A changed dependency changes object context and the
//! common coordinator generation-replays that transcript, so sidecar-first
//! and transcript-first arrival converge without post-commit adapter SQL.

use std::path::{Component, Path};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::adapter::{
    AdapterDiagnostic, AdapterError, AdapterErrorClass, AdapterId, AdapterManifest,
    AdapterObjectContext, AgentAdapter, Availability, CapabilityDeclaration, CapabilityGranularity,
    CapabilityId, CapabilitySupport, ConsistencyPolicy, ContentBlock, DecodeContext,
    DecodeDisposition, DecoderId, DeletionPolicy, DiscoveryContext, DriverSpec, EntityKey,
    EntityScope, EvidenceKind, EvidenceStrength, Fact, FactBatch, MessageFact, MessageRole,
    ObjectSelector, QualifiedTimestamp, RawRetentionPolicy, RunEvidenceFact, RunFact, SessionFact,
    SourceAccess, SourceInstance, SourceInstanceKey, SourceInstanceSpec, SourceObjectDescriptor,
    SourceRoot, SourceSnapshot, StreamAuthority, StreamId, StreamSpec, SupportLevel,
    TimestampQuality, TokenUsage, UsageAccounting, UsageFact, UsageScope, ValueQuality,
};
use crate::source::{
    platform_path_key, read_stable_file_confined, AppendDelimitedConfig, DirectorySnapshotConfig,
    IngestPriority, ReplaceDocumentConfig, SourceRecord, SourceRecordState, StableRead,
};

const ADAPTER_ID: &str = "grok";
const MEMBERSHIP_STREAM: &str = "session-membership";
const TRANSCRIPT_STREAM: &str = "chat-history";
const SUMMARY_STREAM: &str = "session-summaries";
const EVENTS_STREAM: &str = "session-events";
const SIGNALS_STREAM: &str = "session-signals";
const UPDATES_STREAM: &str = "ignored-ui-updates";

const MEMBERSHIP_DECODER: &str = "grok-session-membership";
const TRANSCRIPT_DECODER: &str = "grok-chat-record";
const SUMMARY_DECODER: &str = "grok-summary";
const EVENTS_DECODER: &str = "grok-event";
const SIGNALS_DECODER: &str = "grok-signals";
const UPDATES_DECODER: &str = "grok-ignored-update";

const OBJECT_CONTEXT_VERSION: u32 = 1;
const DECODER_STATE_VERSION: u32 = 1;
const SUMMARY_MAX_BYTES: usize = 1024 * 1024;
const SIGNALS_MAX_BYTES: usize = 256 * 1024;
const EVENT_DEPENDENCY_MAX_BYTES: usize = 8 * 1024 * 1024;
const SEARCH_TEXT_MAX_UTF16: usize = 2_000;

const HISTORY_SESSIONS: &str = "history.sessions";
const HISTORY_MESSAGES: &str = "history.messages";
const HISTORY_CONTENT_BLOCKS: &str = "history.content_blocks";
const HISTORY_TIMESTAMPS: &str = "history.timestamps";
const RUNTIME_SESSION_ACTIVITY: &str = "runtime.session_activity";
const USAGE_INPUT_TOKENS: &str = "usage.input_tokens";
const SOURCE_LIVE: &str = "source.live";
const SOURCE_RECONCILE: &str = "source.reconcile";
const SOURCE_RESUME_CURSOR: &str = "source.resume_cursor";

pub struct GrokAdapter {
    manifest: AdapterManifest,
}

impl GrokAdapter {
    pub fn new() -> Self {
        Self {
            manifest: AdapterManifest {
                id: AdapterId::new(ADAPTER_ID).expect("static Grok adapter id is valid"),
                display_name: "Grok".to_string(),
                adapter_version: env!("CARGO_PKG_VERSION").to_string(),
                contract_version: 1,
                source_schema_versions: vec![
                    "grok-chat-history-jsonl-v1".to_string(),
                    "grok-summary-json-v1".to_string(),
                    "grok-events-jsonl-v1".to_string(),
                    "grok-signals-json-v1".to_string(),
                ],
                capabilities: grok_capabilities(),
            },
        }
    }

    fn adapter_id(&self) -> &AdapterId {
        &self.manifest.id
    }
}

impl Default for GrokAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentAdapter for GrokAdapter {
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
                        "grok_root_unavailable",
                        format!("{}: {error}", configured_root.to_string_lossy()),
                    )
                })?;
                if !canonical.is_dir() {
                    return Err(AdapterError::new(
                        AdapterErrorClass::AdapterFatal,
                        "grok_root_not_directory",
                        canonical.to_string_lossy(),
                    ));
                }
                Ok(SourceInstanceSpec {
                    stable_key: SourceInstanceKey::new(platform_path_key(&canonical))?,
                    display_name: format!("Grok ({})", canonical.to_string_lossy()),
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
                    discovery_reason: "configured Grok data root".to_string(),
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
                    max_depth: 8,
                }),
                selector: selector(vec![
                    "**/chat_history.jsonl",
                    "**/summary.json",
                    "**/events.jsonl",
                    "**/signals.json",
                ]),
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
                selector: selector(vec!["**/chat_history.jsonl"]),
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
                id: StreamId::new(SUMMARY_STREAM)?,
                driver: DriverSpec::ReplaceDocument(ReplaceDocumentConfig {
                    max_document_bytes: SUMMARY_MAX_BYTES,
                }),
                selector: selector(vec!["**/summary.json"]),
                decoder: DecoderId::new(SUMMARY_DECODER)?,
                authority: StreamAuthority::Supplemental,
                entity_scope: EntityScope::Session,
                priority: IngestPriority::Interactive,
                consistency: ConsistencyPolicy::SnapshotReplace,
                deletion: DeletionPolicy::MirrorSource,
                retention: RawRetentionPolicy::HashOnly,
                capabilities: session_capabilities(),
            },
            StreamSpec {
                id: StreamId::new(EVENTS_STREAM)?,
                driver: DriverSpec::AppendDelimited(AppendDelimitedConfig::json_lines()),
                selector: selector(vec!["**/events.jsonl"]),
                decoder: DecoderId::new(EVENTS_DECODER)?,
                authority: StreamAuthority::Supplemental,
                entity_scope: EntityScope::Run,
                priority: IngestPriority::Interactive,
                consistency: ConsistencyPolicy::IncrementalCursor,
                deletion: DeletionPolicy::MirrorSource,
                retention: RawRetentionPolicy::HashOnly,
                capabilities: runtime_capabilities(),
            },
            StreamSpec {
                id: StreamId::new(SIGNALS_STREAM)?,
                driver: DriverSpec::ReplaceDocument(ReplaceDocumentConfig {
                    max_document_bytes: SIGNALS_MAX_BYTES,
                }),
                selector: selector(vec!["**/signals.json"]),
                decoder: DecoderId::new(SIGNALS_DECODER)?,
                authority: StreamAuthority::Supplemental,
                entity_scope: EntityScope::Session,
                priority: IngestPriority::Interactive,
                consistency: ConsistencyPolicy::SnapshotReplace,
                deletion: DeletionPolicy::MirrorSource,
                retention: RawRetentionPolicy::HashOnly,
                capabilities: usage_capabilities(),
            },
            StreamSpec {
                id: StreamId::new(UPDATES_STREAM)?,
                driver: DriverSpec::AppendDelimited(AppendDelimitedConfig::json_lines()),
                selector: selector(vec!["**/updates.jsonl"]),
                decoder: DecoderId::new(UPDATES_DECODER)?,
                authority: StreamAuthority::IgnoredDerived,
                entity_scope: EntityScope::Session,
                priority: IngestPriority::Maintenance,
                consistency: ConsistencyPolicy::IncrementalCursor,
                deletion: DeletionPolicy::MirrorSource,
                retention: RawRetentionPolicy::None,
                capabilities: Vec::new(),
            },
        ];
        for stream in &streams {
            stream.validate(instance)?;
        }
        Ok(streams)
    }

    fn bootstrap_object(
        &self,
        instance: &SourceInstance,
        object: &SourceObjectDescriptor,
    ) -> Result<AdapterObjectContext, AdapterError> {
        let root = instance.root("sessions")?;
        bootstrap_grok_object_context(object, |relative_path, max_bytes| {
            read_dependency(root, relative_path, max_bytes)
        })
    }

    fn bootstrap_object_with_access(
        &self,
        _instance: &SourceInstance,
        object: &SourceObjectDescriptor,
        source_access: &dyn SourceAccess,
    ) -> Result<AdapterObjectContext, AdapterError> {
        bootstrap_grok_object_context(object, |relative_path, max_bytes| {
            let snapshot = source_access.read_object("sessions", relative_path, max_bytes)?;
            Ok(dependency_from_snapshot(snapshot))
        })
    }

    fn decode(
        &self,
        context: DecodeContext<'_>,
        record: &SourceRecord,
        output: &mut FactBatch,
    ) -> Result<DecodeDisposition, AdapterError> {
        let session = GrokSessionContext::decode(context.object_context)?;
        match context.decoder.as_str() {
            TRANSCRIPT_DECODER => decode_transcript(
                self.adapter_id(),
                &session,
                context.decoder_state,
                record,
                output,
            ),
            SUMMARY_DECODER => decode_summary(self.adapter_id(), &session, record, output),
            EVENTS_DECODER => decode_event(self.adapter_id(), &session, record, output),
            SIGNALS_DECODER => decode_signals(self.adapter_id(), &session, record, output),
            UPDATES_DECODER => Ok(DecodeDisposition::IgnoredKnown),
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
    availability: Availability,
    notes: Option<&'static str>,
) -> CapabilityDeclaration {
    CapabilityDeclaration {
        id: CapabilityId::new(id).expect("static Grok capability id is valid"),
        support: CapabilitySupport {
            level,
            granularity,
            availability,
            notes: notes.map(str::to_owned),
        },
    }
}

fn grok_capabilities() -> Vec<CapabilityDeclaration> {
    vec![
        capability(
            HISTORY_SESSIONS,
            SupportLevel::Native,
            CapabilityGranularity::Session,
            Availability::Live,
            None,
        ),
        capability(
            HISTORY_MESSAGES,
            SupportLevel::Native,
            CapabilityGranularity::Message,
            Availability::Live,
            None,
        ),
        capability(
            HISTORY_CONTENT_BLOCKS,
            SupportLevel::Native,
            CapabilityGranularity::Message,
            Availability::Live,
            None,
        ),
        capability(
            HISTORY_TIMESTAMPS,
            SupportLevel::Derived,
            CapabilityGranularity::Message,
            Availability::EventuallyLive,
            Some("turn and loop timestamps are joined from events.jsonl; chat records carry no native per-message timestamp"),
        ),
        capability(
            RUNTIME_SESSION_ACTIVITY,
            SupportLevel::Native,
            CapabilityGranularity::Run,
            Availability::Live,
            Some("turn events prove activity and waiting, while silence never proves session completion"),
        ),
        capability(
            USAGE_INPUT_TOKENS,
            SupportLevel::Estimated,
            CapabilityGranularity::Session,
            Availability::EventuallyLive,
            Some("signals.contextTokensUsed is retained at session scope and never fabricated as exact per-message usage"),
        ),
        capability(
            SOURCE_LIVE,
            SupportLevel::Native,
            CapabilityGranularity::Instance,
            Availability::Live,
            None,
        ),
        capability(
            SOURCE_RECONCILE,
            SupportLevel::Native,
            CapabilityGranularity::Instance,
            Availability::Live,
            None,
        ),
        capability(
            SOURCE_RESUME_CURSOR,
            SupportLevel::Native,
            CapabilityGranularity::Record,
            Availability::Live,
            None,
        ),
    ]
}

fn ids(values: &[&'static str]) -> Vec<CapabilityId> {
    values
        .iter()
        .map(|id| CapabilityId::new(*id).expect("static Grok stream capability id is valid"))
        .collect()
}

fn source_capabilities() -> Vec<CapabilityId> {
    ids(&[SOURCE_LIVE, SOURCE_RECONCILE, SOURCE_RESUME_CURSOR])
}

fn transcript_capabilities() -> Vec<CapabilityId> {
    ids(&[
        HISTORY_SESSIONS,
        HISTORY_MESSAGES,
        HISTORY_CONTENT_BLOCKS,
        HISTORY_TIMESTAMPS,
        RUNTIME_SESSION_ACTIVITY,
        SOURCE_LIVE,
        SOURCE_RECONCILE,
        SOURCE_RESUME_CURSOR,
    ])
}

fn session_capabilities() -> Vec<CapabilityId> {
    ids(&[HISTORY_SESSIONS, SOURCE_LIVE, SOURCE_RECONCILE])
}

fn runtime_capabilities() -> Vec<CapabilityId> {
    ids(&[
        HISTORY_TIMESTAMPS,
        RUNTIME_SESSION_ACTIVITY,
        SOURCE_LIVE,
        SOURCE_RECONCILE,
        SOURCE_RESUME_CURSOR,
    ])
}

fn usage_capabilities() -> Vec<CapabilityId> {
    ids(&[USAGE_INPUT_TOKENS, SOURCE_LIVE, SOURCE_RECONCILE])
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct GrokSessionContext {
    encoded_cwd: String,
    cwd: String,
    native_project_key: String,
    session_id: String,
    created_at: Option<String>,
    updated_at: Option<String>,
    generated_title: Option<String>,
    session_summary: Option<String>,
    git_branch: Option<String>,
    summary_revision: Option<[u8; 32]>,
    events_revision: Option<[u8; 32]>,
    clock: Vec<ClockTurn>,
    clock_truncated: bool,
}

impl GrokSessionContext {
    fn decode(context: &AdapterObjectContext) -> Result<Self, AdapterError> {
        if context.version() != OBJECT_CONTEXT_VERSION {
            return Err(AdapterError::new(
                AdapterErrorClass::StreamFatal,
                "grok_object_context_version",
                format!(
                    "unsupported Grok object context version {}",
                    context.version()
                ),
            ));
        }
        serde_json::from_slice(context.payload()).map_err(|error| {
            AdapterError::new(
                AdapterErrorClass::StreamFatal,
                "grok_object_context_decode",
                error.to_string(),
            )
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ClockTurn {
    start_index: u32,
    start_time: String,
    loops: Vec<ClockLoop>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ClockLoop {
    loop_time: String,
    first_token_time: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct TranscriptState {
    version: u32,
    next_index: u32,
    active_turn_start: Option<u32>,
    assistant_index: usize,
    last_agent_time: Option<String>,
}

impl TranscriptState {
    fn decode(value: Option<&[u8]>) -> Result<Self, AdapterError> {
        let Some(value) = value else {
            return Ok(Self {
                version: DECODER_STATE_VERSION,
                ..Self::default()
            });
        };
        let state: Self = serde_json::from_slice(value).map_err(|error| {
            AdapterError::new(
                AdapterErrorClass::StreamFatal,
                "grok_decoder_state_decode",
                error.to_string(),
            )
        })?;
        if state.version != DECODER_STATE_VERSION {
            return Err(AdapterError::new(
                AdapterErrorClass::StreamFatal,
                "grok_decoder_state_version",
                format!("unsupported Grok decoder state version {}", state.version),
            ));
        }
        Ok(state)
    }

    fn store(&self, output: &mut FactBatch) -> Result<(), AdapterError> {
        output.set_next_decoder_state(serde_json::to_vec(self).map_err(|error| {
            AdapterError::new(
                AdapterErrorClass::InvalidContract,
                "grok_decoder_state_encode",
                error.to_string(),
            )
        })?)
    }
}

struct Dependency {
    bytes: Vec<u8>,
    revision: [u8; 32],
}

fn bootstrap_grok_object_context(
    object: &SourceObjectDescriptor,
    mut read: impl FnMut(&Path, usize) -> Result<Option<Dependency>, AdapterError>,
) -> Result<AdapterObjectContext, AdapterError> {
    let mut session = session_context_from_path(&object.relative_path)?;
    if object.stream_id.as_str() == TRANSCRIPT_STREAM {
        let session_dir = object
            .relative_path
            .parent()
            .ok_or_else(|| invalid_path(&object.relative_path))?;
        if let Some(dependency) = read(&session_dir.join("summary.json"), SUMMARY_MAX_BYTES)? {
            session.summary_revision = Some(dependency.revision);
            if let Ok(summary) = serde_json::from_slice::<Value>(&dependency.bytes) {
                apply_summary(&mut session, &summary);
            }
        }
        if let Some(dependency) = read(
            &session_dir.join("events.jsonl"),
            EVENT_DEPENDENCY_MAX_BYTES,
        )? {
            session.events_revision = Some(dependency.revision);
            session.clock = build_clock(&dependency.bytes);
        }
    }
    encode_object_context(session)
}

fn dependency_from_snapshot(snapshot: SourceSnapshot) -> Option<Dependency> {
    match (snapshot.payload, snapshot.oversized) {
        (Some(bytes), _) => Some(Dependency {
            bytes,
            revision: snapshot.revision.revision,
        }),
        (None, true) => Some(Dependency {
            bytes: Vec::new(),
            revision: snapshot.revision.revision,
        }),
        (None, false) => None,
    }
}

fn read_dependency(
    root: &Path,
    relative_path: &Path,
    max_bytes: usize,
) -> Result<Option<Dependency>, AdapterError> {
    match read_stable_file_confined(root, relative_path, max_bytes).map_err(|error| {
        AdapterError::new(
            AdapterErrorClass::Transient,
            "grok_dependency_read",
            format!("{}: {error}", relative_path.to_string_lossy()),
        )
    })? {
        StableRead::Missing | StableRead::Unstable => Ok(None),
        StableRead::Oversized(stamp) => {
            let mut identity = Vec::new();
            identity.extend_from_slice(&stamp.len.to_be_bytes());
            identity.extend_from_slice(&stamp.modified_ns.to_be_bytes());
            Ok(Some(Dependency {
                bytes: Vec::new(),
                revision: *blake3::hash(&identity).as_bytes(),
            }))
        }
        StableRead::Stable {
            bytes, revision, ..
        } => Ok(Some(Dependency {
            bytes,
            revision: *revision.as_bytes(),
        })),
    }
}

fn encode_object_context(
    mut session: GrokSessionContext,
) -> Result<AdapterObjectContext, AdapterError> {
    loop {
        let payload = serde_json::to_vec(&session).map_err(|error| {
            AdapterError::new(
                AdapterErrorClass::InvalidContract,
                "grok_object_context_encode",
                error.to_string(),
            )
        })?;
        if payload.len() <= AdapterObjectContext::MAX_BYTES {
            return AdapterObjectContext::new(OBJECT_CONTEXT_VERSION, payload);
        }
        if session.clock.is_empty() {
            return Err(AdapterError::invalid_contract(
                "Grok object metadata exceeds object-context bound",
            ));
        }
        session.clock.remove(0);
        session.clock_truncated = true;
    }
}

fn session_context_from_path(path: &Path) -> Result<GrokSessionContext, AdapterError> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(invalid_path(path));
    }
    let components = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>();
    if components.len() != 3 {
        return Err(invalid_path(path));
    }
    let encoded_cwd = components[0].clone();
    let cwd = percent_decode(&encoded_cwd).ok_or_else(|| invalid_path(path))?;
    if cwd.trim().is_empty() || components[1].trim().is_empty() {
        return Err(invalid_path(path));
    }
    Ok(GrokSessionContext {
        encoded_cwd,
        native_project_key: encode_project_key(&cwd),
        cwd,
        session_id: components[1].clone(),
        ..GrokSessionContext::default()
    })
}

fn invalid_path(path: &Path) -> AdapterError {
    AdapterError::new(
        AdapterErrorClass::RecordPermanent,
        "grok_invalid_session_path",
        path.to_string_lossy(),
    )
}

fn apply_summary(session: &mut GrokSessionContext, value: &Value) {
    if let Some(info) = value.get("info").and_then(Value::as_object) {
        if let Some(id) = nonempty_string(info.get("id")) {
            session.session_id = id;
        }
        if let Some(cwd) = nonempty_string(info.get("cwd")) {
            session.cwd = cwd;
            session.native_project_key = encode_project_key(&session.cwd);
        }
    }
    if let Some(git_root) = nonempty_string(value.get("git_root_dir")) {
        if session.cwd.is_empty() {
            session.cwd = git_root.trim_end_matches('/').to_string();
            session.native_project_key = encode_project_key(&session.cwd);
        }
    }
    session.created_at = nonempty_string(value.get("created_at"));
    session.updated_at = nonempty_string(value.get("updated_at"))
        .or_else(|| nonempty_string(value.get("last_active_at")));
    session.generated_title = nonempty_string(value.get("generated_title"));
    session.session_summary = nonempty_string(value.get("session_summary"));
    session.git_branch = nonempty_string(value.get("head_branch"));
}

fn build_clock(bytes: &[u8]) -> Vec<ClockTurn> {
    #[derive(Clone)]
    struct Event {
        kind: String,
        time: String,
        count: Option<u32>,
    }
    let events = bytes
        .split(|byte| *byte == b'\n')
        .filter_map(|line| serde_json::from_slice::<Value>(line).ok())
        .filter_map(|value| {
            Some(Event {
                kind: value.get("type")?.as_str()?.to_string(),
                time: value.get("ts")?.as_str()?.to_string(),
                count: value
                    .get("conversation_message_count")
                    .and_then(Value::as_u64)
                    .and_then(|value| u32::try_from(value).ok()),
            })
        })
        .collect::<Vec<_>>();
    let turn_positions = events
        .iter()
        .enumerate()
        .filter_map(|(index, event)| {
            (event.kind == "turn_started")
                .then_some(event.count)
                .flatten()
                .map(|count| (index, count))
        })
        .collect::<Vec<_>>();
    if turn_positions.is_empty() {
        return Vec::new();
    }
    let mut epoch_start = 0;
    for index in 1..turn_positions.len() {
        if turn_positions[index].1 < turn_positions[index - 1].1 {
            epoch_start = index;
        }
    }
    let selected = &turn_positions[epoch_start..];
    selected
        .iter()
        .enumerate()
        .map(|(turn_index, (event_index, count))| {
            let end = selected
                .get(turn_index + 1)
                .map(|(index, _)| *index)
                .unwrap_or(events.len());
            let mut loops = Vec::new();
            let mut first_tokens = Vec::new();
            for event in &events[*event_index + 1..end] {
                match event.kind.as_str() {
                    "loop_started" => loops.push(event.time.clone()),
                    "first_token" => first_tokens.push(event.time.clone()),
                    _ => {}
                }
            }
            let loop_count = loops.len().max(first_tokens.len());
            ClockTurn {
                start_index: *count,
                start_time: events[*event_index].time.clone(),
                loops: (0..loop_count)
                    .map(|index| ClockLoop {
                        loop_time: loops
                            .get(index)
                            .cloned()
                            .unwrap_or_else(|| events[*event_index].time.clone()),
                        first_token_time: first_tokens.get(index).cloned(),
                    })
                    .collect(),
            }
        })
        .collect()
}

fn decode_transcript(
    adapter_id: &AdapterId,
    session_context: &GrokSessionContext,
    decoder_state: Option<&[u8]>,
    record: &SourceRecord,
    output: &mut FactBatch,
) -> Result<DecodeDisposition, AdapterError> {
    if record.state == SourceRecordState::Absent {
        return Ok(DecodeDisposition::IgnoredKnown);
    }
    let mut state = TranscriptState::decode(decoder_state)?;
    let value: Value = match serde_json::from_slice(&record.payload) {
        Ok(value) => value,
        Err(error) => {
            preserve_unknown(
                record,
                output,
                None,
                format!("malformed Grok chat record: {error}"),
            )?;
            state.next_index = state.next_index.saturating_add(1);
            state.store(output)?;
            return Ok(DecodeDisposition::PreservedUnknown);
        }
    };
    let Some(object) = value.as_object() else {
        preserve_unknown(
            record,
            output,
            None,
            "Grok chat record is not an object".to_string(),
        )?;
        state.next_index = state.next_index.saturating_add(1);
        state.store(output)?;
        return Ok(DecodeDisposition::PreservedUnknown);
    };
    let kind = object
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let Some((role, content, search_text, native_message_id)) = decode_chat_content(kind, object)
    else {
        preserve_unknown(
            record,
            output,
            Some(kind.to_string()),
            format!("unknown Grok chat record kind {kind}"),
        )?;
        state.next_index = state.next_index.saturating_add(1);
        state.store(output)?;
        return Ok(DecodeDisposition::PreservedUnknown);
    };
    let index = state.next_index;
    let source_time = timestamp_for_record(session_context, &mut state, index, kind).map(|value| {
        QualifiedTimestamp {
            value,
            quality: TimestampQuality::Derived,
        }
    });
    let (session, project, run) =
        entity_keys(adapter_id, record.source_instance_id, session_context)?;
    if index == 0 {
        output.push(
            record,
            Fact::Session(session_fact(
                session_context,
                session.clone(),
                project.clone(),
            )),
        )?;
        output.push(
            record,
            Fact::Run(RunFact {
                run: run.clone(),
                session: session.clone(),
                native_run_id: session_context.session_id.clone(),
                parent_run: None,
            }),
        )?;
        output.push(
            record,
            Fact::RunEvidence(RunEvidenceFact {
                run: run.clone(),
                kind: EvidenceKind::RunDeclared,
                strength: EvidenceStrength::Layout,
                native_state: Some("chat_history".to_string()),
                source_time: session_context.created_at.as_deref().map(native_timestamp),
            }),
        )?;
    }
    let message_key = message_native_key(
        &session_context.session_id,
        kind,
        native_message_id.as_deref(),
        index,
    );
    let message = EntityKey::native(
        adapter_id,
        record.source_instance_id,
        "message",
        &message_key,
    )?;
    output.push(
        record,
        Fact::Message(MessageFact {
            message,
            session: session.clone(),
            run: run.clone(),
            native_message_id,
            native_kind: kind.to_string(),
            role,
            content,
            source_time: source_time.clone(),
            parent_native_message_id: None,
            model: None,
            search_text,
            raw_json: serde_json::to_vec(&value).map_err(|error| {
                AdapterError::new(
                    AdapterErrorClass::RecordPermanent,
                    "grok_raw_json_encode",
                    error.to_string(),
                )
            })?,
        }),
    )?;
    output.push(
        record,
        Fact::RunEvidence(RunEvidenceFact {
            run,
            kind: EvidenceKind::ActivityObserved,
            strength: EvidenceStrength::NativeActivity,
            native_state: Some(format!("chat_history/{kind}")),
            source_time,
        }),
    )?;
    state.next_index = state.next_index.saturating_add(1);
    state.store(output)?;
    Ok(DecodeDisposition::Applied)
}

fn decode_summary(
    adapter_id: &AdapterId,
    path_context: &GrokSessionContext,
    record: &SourceRecord,
    output: &mut FactBatch,
) -> Result<DecodeDisposition, AdapterError> {
    if record.state == SourceRecordState::Absent {
        return Ok(DecodeDisposition::IgnoredKnown);
    }
    let value: Value = match serde_json::from_slice(&record.payload) {
        Ok(value) => value,
        Err(_) => return Ok(DecodeDisposition::RetryTransient),
    };
    if !value.is_object() {
        return Ok(DecodeDisposition::RetryTransient);
    }
    let mut context = path_context.clone();
    apply_summary(&mut context, &value);
    let (session, project, _) = entity_keys(adapter_id, record.source_instance_id, &context)?;
    output.push(
        record,
        Fact::Session(session_fact(&context, session, project)),
    )?;
    Ok(DecodeDisposition::Applied)
}

fn decode_event(
    adapter_id: &AdapterId,
    context: &GrokSessionContext,
    record: &SourceRecord,
    output: &mut FactBatch,
) -> Result<DecodeDisposition, AdapterError> {
    if record.state == SourceRecordState::Absent {
        return Ok(DecodeDisposition::IgnoredKnown);
    }
    let value: Value = match serde_json::from_slice(&record.payload) {
        Ok(value) => value,
        Err(error) => {
            preserve_unknown(
                record,
                output,
                None,
                format!("malformed Grok event record: {error}"),
            )?;
            return Ok(DecodeDisposition::PreservedUnknown);
        }
    };
    let Some(object) = value.as_object() else {
        preserve_unknown(
            record,
            output,
            None,
            "Grok event record is not an object".to_string(),
        )?;
        return Ok(DecodeDisposition::PreservedUnknown);
    };
    let kind = object
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let source_time = nonempty_string(object.get("ts")).map(|value| native_timestamp(&value));
    let evidence = match kind {
        "turn_started" => EvidenceKind::RunStarted,
        "turn_ended" => EvidenceKind::WaitingObserved,
        "loop_started" | "first_token" => EvidenceKind::ActivityObserved,
        _ => {
            preserve_unknown(
                record,
                output,
                Some(kind.to_string()),
                format!("unknown Grok event kind {kind}"),
            )?;
            return Ok(DecodeDisposition::PreservedUnknown);
        }
    };
    let (_, _, run) = entity_keys(adapter_id, record.source_instance_id, context)?;
    output.push(
        record,
        Fact::RunEvidence(RunEvidenceFact {
            run,
            kind: evidence,
            strength: EvidenceStrength::NativeExplicit,
            native_state: Some(match object.get("outcome").and_then(Value::as_str) {
                Some(outcome) => format!("{kind}/{outcome}"),
                None => kind.to_string(),
            }),
            source_time,
        }),
    )?;
    Ok(DecodeDisposition::Applied)
}

fn decode_signals(
    adapter_id: &AdapterId,
    context: &GrokSessionContext,
    record: &SourceRecord,
    output: &mut FactBatch,
) -> Result<DecodeDisposition, AdapterError> {
    if record.state == SourceRecordState::Absent {
        return Ok(DecodeDisposition::IgnoredKnown);
    }
    let value: Value = match serde_json::from_slice(&record.payload) {
        Ok(value) => value,
        Err(_) => return Ok(DecodeDisposition::RetryTransient),
    };
    let Some(object) = value.as_object() else {
        return Ok(DecodeDisposition::RetryTransient);
    };
    let context_tokens = object
        .get("contextTokensUsed")
        .or_else(|| object.get("context_tokens_used"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if context_tokens == 0 {
        return Ok(DecodeDisposition::IgnoredKnown);
    }
    let (session, _, _) = entity_keys(adapter_id, record.source_instance_id, context)?;
    output.push(
        record,
        Fact::Usage(UsageFact {
            subject: session.clone(),
            session,
            scope: UsageScope::Session,
            accounting: UsageAccounting::Snapshot,
            quality: ValueQuality::Estimated,
            values: TokenUsage {
                input_tokens: context_tokens,
                ..TokenUsage::default()
            },
            model: None,
            source_time: context.updated_at.as_deref().map(native_timestamp),
        }),
    )?;
    Ok(DecodeDisposition::Applied)
}

type DecodedChatContent = (
    MessageRole,
    Vec<ContentBlock>,
    Option<String>,
    Option<String>,
);

fn decode_chat_content(kind: &str, object: &Map<String, Value>) -> Option<DecodedChatContent> {
    match kind {
        "system" => {
            let text = readable_text(object.get("content"));
            Some((
                MessageRole::System,
                vec![ContentBlock::Text { text }],
                None,
                None,
            ))
        }
        "user" => {
            let human = human_user_text(object);
            let content = chat_blocks(object.get("content"));
            Some((
                if human.is_some() {
                    MessageRole::User
                } else {
                    MessageRole::Other("context".to_string())
                },
                content,
                human.and_then(truncate_search_text),
                None,
            ))
        }
        "assistant" => {
            let mut content = chat_blocks(object.get("content"));
            if let Some(calls) = object.get("tool_calls").and_then(Value::as_array) {
                for call in calls.iter().filter_map(Value::as_object) {
                    let native_id =
                        nonempty_string(call.get("id")).unwrap_or_else(|| "unknown".to_string());
                    let name = nonempty_string(call.get("name"))
                        .unwrap_or_else(|| "Unknown Tool".to_string());
                    content.push(ContentBlock::ToolCall {
                        native_id,
                        name,
                        input: tool_input(call.get("arguments")),
                    });
                }
            }
            let search = readable_search_text(&content);
            Some((MessageRole::Assistant, content, search, None))
        }
        "reasoning" => {
            let text = readable_text(object.get("summary")).trim().to_string();
            Some((
                MessageRole::Assistant,
                vec![ContentBlock::Thinking {
                    text: text.clone(),
                    redacted: text.is_empty() && object.contains_key("encrypted_content"),
                }],
                truncate_search_text(text),
                nonempty_string(object.get("id")),
            ))
        }
        "tool_result" => {
            let id = nonempty_string(object.get("tool_call_id"));
            Some((
                MessageRole::Other("tool_result".to_string()),
                vec![ContentBlock::ToolResult {
                    native_call_id: id.clone().unwrap_or_else(|| "unknown".to_string()),
                    content: object.get("content").cloned().unwrap_or(Value::Null),
                    is_error: object.get("is_error").and_then(Value::as_bool) == Some(true),
                }],
                truncate_search_text(readable_text(object.get("content"))),
                id,
            ))
        }
        "backend_tool_call" => {
            let nested = object.get("kind").and_then(Value::as_object)?;
            let id = nonempty_string(nested.get("id"));
            let name = nonempty_string(nested.get("tool_type"))
                .unwrap_or_else(|| "Backend Tool".to_string());
            let input = tool_input(nested.get("action"));
            let search = truncate_search_text(format!("{name} {}", stable_json(&input)));
            Some((
                MessageRole::Assistant,
                vec![ContentBlock::ToolCall {
                    native_id: id.clone().unwrap_or_else(|| "unknown".to_string()),
                    name,
                    input,
                }],
                search,
                id,
            ))
        }
        _ => None,
    }
}

fn timestamp_for_record(
    context: &GrokSessionContext,
    state: &mut TranscriptState,
    index: u32,
    kind: &str,
) -> Option<String> {
    let turn = context
        .clock
        .iter()
        .rev()
        .find(|turn| turn.start_index <= index);
    let Some(turn) = turn else {
        return matches!(kind, "system" | "user")
            .then(|| context.created_at.clone())
            .flatten();
    };
    if state.active_turn_start != Some(turn.start_index) {
        state.active_turn_start = Some(turn.start_index);
        state.assistant_index = 0;
        state.last_agent_time = Some(turn.start_time.clone());
    }
    match kind {
        "user" | "system" => Some(turn.start_time.clone()),
        "reasoning" => Some(
            turn.loops
                .get(state.assistant_index)
                .or_else(|| turn.loops.last())
                .map(|loop_| loop_.loop_time.clone())
                .unwrap_or_else(|| turn.start_time.clone()),
        ),
        "assistant" => {
            let timestamp = turn
                .loops
                .get(state.assistant_index)
                .and_then(|loop_| {
                    loop_
                        .first_token_time
                        .clone()
                        .or_else(|| Some(loop_.loop_time.clone()))
                })
                .unwrap_or_else(|| turn.start_time.clone());
            state.assistant_index = state.assistant_index.saturating_add(1);
            state.last_agent_time = Some(timestamp.clone());
            Some(timestamp)
        }
        "tool_result" => state
            .last_agent_time
            .clone()
            .or_else(|| Some(turn.start_time.clone())),
        "backend_tool_call" => Some(
            turn.loops
                .get(state.assistant_index)
                .map(|loop_| loop_.loop_time.clone())
                .or_else(|| state.last_agent_time.clone())
                .unwrap_or_else(|| turn.start_time.clone()),
        ),
        _ => None,
    }
}

fn session_fact(
    context: &GrokSessionContext,
    session: EntityKey,
    project: EntityKey,
) -> SessionFact {
    let title = context
        .generated_title
        .as_deref()
        .or(context.session_summary.as_deref())
        .map(|value| crate::core::text::truncate_utf16(value, 200).to_string());
    SessionFact {
        session,
        project,
        native_session_id: context.session_id.clone(),
        native_project_key: context.native_project_key.clone(),
        cwd: Some(context.cwd.clone()),
        git_branch: context.git_branch.clone(),
        first_prompt: title.clone(),
        ai_title: title,
        custom_title: None,
        source_time: context.created_at.as_deref().map(native_timestamp),
    }
}

fn entity_keys(
    adapter_id: &AdapterId,
    source_instance_id: u64,
    context: &GrokSessionContext,
) -> Result<(EntityKey, EntityKey, EntityKey), AdapterError> {
    let session = EntityKey::native(
        adapter_id,
        source_instance_id,
        "session",
        context.session_id.as_bytes(),
    )?;
    let project = EntityKey::native(
        adapter_id,
        source_instance_id,
        "project",
        context.native_project_key.as_bytes(),
    )?;
    let run = EntityKey::native(
        adapter_id,
        source_instance_id,
        "run",
        context.session_id.as_bytes(),
    )?;
    Ok((session, project, run))
}

fn chat_blocks(value: Option<&Value>) -> Vec<ContentBlock> {
    let Some(value) = value else {
        return Vec::new();
    };
    if let Some(text) = value.as_str() {
        return vec![ContentBlock::Text {
            text: text.to_string(),
        }];
    }
    let Some(values) = value.as_array() else {
        return vec![ContentBlock::Native {
            native_kind: "content".to_string(),
            value: value.clone(),
        }];
    };
    values
        .iter()
        .filter_map(|value| {
            let object = value.as_object()?;
            let kind = object
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            if matches!(kind, "text" | "input_text" | "output_text" | "summary_text") {
                return Some(ContentBlock::Text {
                    text: object
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                });
            }
            if kind == "image" {
                let source = object
                    .get("url")
                    .or_else(|| object.get("data"))
                    .map(Value::to_string)
                    .unwrap_or_default();
                return Some(ContentBlock::Image {
                    media_type: object
                        .get("media_type")
                        .and_then(Value::as_str)
                        .unwrap_or("application/octet-stream")
                        .to_string(),
                    data_hash: *blake3::hash(source.as_bytes()).as_bytes(),
                });
            }
            Some(ContentBlock::Native {
                native_kind: kind.to_string(),
                value: value.clone(),
            })
        })
        .collect()
}

fn human_user_text(object: &Map<String, Value>) -> Option<String> {
    if nonempty_string(object.get("synthetic_reason")).is_some() {
        return None;
    }
    let text = readable_text(object.get("content"));
    if let Some(start) = text.to_ascii_lowercase().find("<user_query>") {
        let content_start = start + "<user_query>".len();
        let remainder = &text[content_start..];
        if let Some(end) = remainder.to_ascii_lowercase().find("</user_query>") {
            let query = remainder[..end].trim();
            return (!query.is_empty()).then(|| query.to_string());
        }
    }
    let image_count = object
        .get("content")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter(|block| block.get("type").and_then(Value::as_str) == Some("image"))
                .count()
        })
        .unwrap_or(0);
    if image_count > 0 || text.to_ascii_lowercase().contains("<image_files") {
        return Some(if image_count == 1 {
            "Image attachment".to_string()
        } else {
            format!("{} image attachments", image_count.max(1))
        });
    }
    let lowered = text.to_ascii_lowercase();
    if lowered.contains("<user_info") || lowered.contains("<system-reminder") {
        return None;
    }
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

fn readable_text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(value)) => value.clone(),
        Some(Value::Array(values)) => values
            .iter()
            .filter_map(|value| value.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn readable_search_text(blocks: &[ContentBlock]) -> Option<String> {
    let parts = blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } | ContentBlock::Thinking { text, .. } => Some(text.clone()),
            ContentBlock::ToolCall { name, input, .. } => {
                Some(format!("{name} {}", stable_json(input)))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    truncate_search_text(parts.join("\n"))
}

fn truncate_search_text(text: String) -> Option<String> {
    (!text.is_empty())
        .then(|| crate::core::text::truncate_utf16(&text, SEARCH_TEXT_MAX_UTF16).to_string())
}

fn tool_input(value: Option<&Value>) -> Value {
    let value = value.cloned().unwrap_or_else(|| Value::Object(Map::new()));
    if let Some(text) = value.as_str() {
        serde_json::from_str(text).unwrap_or_else(|_| {
            Value::Object(Map::from_iter([(
                "input".to_string(),
                Value::String(text.to_string()),
            )]))
        })
    } else if value.is_object() {
        value
    } else if let Value::Array(values) = value {
        Value::Object(Map::from_iter([(
            "items".to_string(),
            Value::Array(values),
        )]))
    } else {
        Value::Object(Map::from_iter([("input".to_string(), value)]))
    }
}

fn stable_json(value: &Value) -> String {
    fn sorted(value: &Value) -> Value {
        match value {
            Value::Array(values) => Value::Array(values.iter().map(sorted).collect()),
            Value::Object(object) => Value::Object(
                object
                    .iter()
                    .map(|(key, value)| (key.clone(), sorted(value)))
                    .collect(),
            ),
            value => value.clone(),
        }
    }
    serde_json::to_string(&sorted(value)).unwrap_or_default()
}

fn nonempty_string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn native_timestamp(value: &str) -> QualifiedTimestamp {
    QualifiedTimestamp {
        value: value.to_string(),
        quality: TimestampQuality::NativeExact,
    }
}

fn message_native_key(
    session_id: &str,
    kind: &str,
    native_message_id: Option<&str>,
    index: u32,
) -> Vec<u8> {
    let mut key = Vec::new();
    push_key_component(&mut key, session_id.as_bytes());
    push_key_component(&mut key, kind.as_bytes());
    if let Some(native_message_id) = native_message_id {
        push_key_component(&mut key, native_message_id.as_bytes());
    } else {
        push_key_component(&mut key, &index.to_be_bytes());
    }
    key
}

fn push_key_component(output: &mut Vec<u8>, value: &[u8]) {
    output.extend_from_slice(&(value.len() as u64).to_be_bytes());
    output.extend_from_slice(value);
}

fn percent_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            output.push((from_hex(bytes[index + 1])? << 4) | from_hex(bytes[index + 2])?);
            index += 3;
        } else {
            output.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(output).ok()
}

fn from_hex(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn encode_project_key(cwd: &str) -> String {
    cwd.replace(['/', '\\'], "-")
}

fn preserve_unknown(
    record: &SourceRecord,
    output: &mut FactBatch,
    native_kind: Option<String>,
    reason: String,
) -> Result<(), AdapterError> {
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
        code: "grok_preserved_unknown".to_string(),
        message: reason,
    })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use tempfile::TempDir;

    use crate::adapter::AdapterRegistry;
    use crate::engine::{EngineOptions, ReconcileRequest, SpaghettiEngineCore};

    use super::*;

    #[test]
    fn declares_transcript_sidecars_membership_and_ignored_projection() {
        let root = TempDir::new().unwrap();
        fs::create_dir_all(root.path().join("sessions")).unwrap();
        let adapter = GrokAdapter::new();
        let spec = adapter
            .discover(&DiscoveryContext {
                configured_roots: vec![root.path().to_path_buf()],
                observed_at: 1,
            })
            .unwrap()
            .remove(0);
        let instance = SourceInstance { id: 1, spec };
        let streams = adapter.streams(&instance).unwrap();
        assert_eq!(streams.len(), 6);
        assert!(streams
            .iter()
            .any(|stream| matches!(stream.driver, DriverSpec::DirectorySnapshot(_))));
        assert!(streams.iter().any(|stream| {
            stream.id.as_str() == UPDATES_STREAM
                && stream.authority == StreamAuthority::IgnoredDerived
        }));
    }

    #[test]
    fn latest_event_epoch_builds_turn_loop_clock() {
        let clock = build_clock(
            br#"{"ts":"2026-01-01T08:00:00Z","type":"turn_started","conversation_message_count":50}
{"ts":"2026-01-01T09:00:00Z","type":"turn_started","conversation_message_count":1}
{"ts":"2026-01-01T09:00:01Z","type":"loop_started"}
{"ts":"2026-01-01T09:00:02Z","type":"first_token"}
"#,
        );
        assert_eq!(clock.len(), 1);
        assert_eq!(clock[0].start_index, 1);
        assert_eq!(
            clock[0].loops[0].first_token_time.as_deref(),
            Some("2026-01-01T09:00:02Z")
        );
    }

    #[test]
    fn fixture_reconciles_all_sidecars_through_common_engine() {
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/small-grok/.grok");
        let temp = TempDir::new().unwrap();
        let database_path = temp.path().join("grok-observation.sqlite");
        let registry = AdapterRegistry::builder()
            .register(GrokAdapter::new())
            .build()
            .unwrap();
        let engine = SpaghettiEngineCore::open_with_registry(
            EngineOptions {
                database_path: database_path.clone(),
                query_workers: Some(1),
                owner_label: Some("grok-adapter-test".to_string()),
            },
            registry,
        )
        .unwrap();
        let first = engine
            .reconcile_adapter(ADAPTER_ID, ReconcileRequest::manual(vec![fixture.clone()]))
            .unwrap();
        assert_eq!(first.records_quarantined, 0);
        assert_eq!(first.retries_required, 0);
        let second = engine
            .reconcile_adapter(ADAPTER_ID, ReconcileRequest::manual(vec![fixture]))
            .unwrap();
        assert_eq!(second.commits, 0);
        engine.shutdown().unwrap();

        let connection = rusqlite::Connection::open(database_path).unwrap();
        let count = |table: &str| {
            connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap()
        };
        assert_eq!(count("canonical_sessions"), 4);
        assert_eq!(count("canonical_messages"), 16);
        let timed_messages: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM canonical_messages WHERE source_time IS NOT NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(timed_messages, 16);
        let estimated: i64 = connection
            .query_row(
                "SELECT SUM(estimated_input_tokens) FROM usage_totals",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(estimated, 17_400);
        assert_eq!(count("usage_contributions"), 4);
    }

    #[test]
    fn sidecar_first_transcript_first_and_fresh_build_converge() {
        fn run(order: &str) -> Vec<String> {
            let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(
                "fixtures/small-grok/.grok/sessions/%2Ftmp%2Fgrok-proj-a/019f5d62-bb11-7c70-b2c6-13166fe9fdee",
            );
            let temp = TempDir::new().unwrap();
            let root = temp.path().join(".grok");
            let session =
                root.join("sessions/%2Ftmp%2Fgrok-proj-a/019f5d62-bb11-7c70-b2c6-13166fe9fdee");
            fs::create_dir_all(&session).unwrap();
            let database_path = temp.path().join("observation.sqlite");
            let engine = SpaghettiEngineCore::open_with_registry(
                EngineOptions {
                    database_path: database_path.clone(),
                    query_workers: Some(1),
                    owner_label: Some(format!("grok-{order}")),
                },
                AdapterRegistry::builder()
                    .register(GrokAdapter::new())
                    .build()
                    .unwrap(),
            )
            .unwrap();
            let copy = |name: &str| fs::copy(fixture.join(name), session.join(name)).unwrap();
            match order {
                "sidecar-first" => {
                    for name in ["summary.json", "events.jsonl", "signals.json"] {
                        copy(name);
                    }
                    engine
                        .reconcile_adapter(ADAPTER_ID, ReconcileRequest::manual(vec![root.clone()]))
                        .unwrap();
                    copy("chat_history.jsonl");
                }
                "transcript-first" => {
                    copy("chat_history.jsonl");
                    engine
                        .reconcile_adapter(ADAPTER_ID, ReconcileRequest::manual(vec![root.clone()]))
                        .unwrap();
                    for name in ["summary.json", "events.jsonl", "signals.json"] {
                        copy(name);
                    }
                }
                "fresh" => {
                    for name in [
                        "chat_history.jsonl",
                        "summary.json",
                        "events.jsonl",
                        "signals.json",
                    ] {
                        copy(name);
                    }
                }
                _ => unreachable!(),
            }
            engine
                .reconcile_adapter(ADAPTER_ID, ReconcileRequest::manual(vec![root]))
                .unwrap();
            engine.shutdown().unwrap();
            let connection = rusqlite::Connection::open(database_path).unwrap();
            let mut rows = Vec::new();
            let mut messages = connection
                .prepare(
                    r#"
                    SELECT native_kind, role, CAST(content_json AS TEXT),
                           COALESCE(source_time, ''), CAST(raw_json AS TEXT)
                    FROM canonical_messages ORDER BY cursor_end
                    "#,
                )
                .unwrap();
            rows.extend(
                messages
                    .query_map([], |row| {
                        Ok(format!(
                            "message|{}|{}|{}|{}|{}",
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, String>(4)?,
                        ))
                    })
                    .unwrap()
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap(),
            );
            let session_row: String = connection
                .query_row(
                    r#"
                    SELECT native_session_id || '|' || native_project_key || '|' ||
                           COALESCE(cwd, '') || '|' || COALESCE(git_branch, '') || '|' ||
                           COALESCE(first_prompt, '') || '|' || COALESCE(ai_title, '') || '|' ||
                           COALESCE(source_time, '')
                    FROM canonical_sessions
                    "#,
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            rows.push(format!("session|{session_row}"));
            let usage: (i64, i64) = connection
                .query_row(
                    r#"
                    SELECT SUM(estimated_input_tokens), COUNT(*)
                    FROM usage_totals
                    "#,
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            rows.push(format!("usage|{}|{}", usage.0, usage.1));
            rows
        }

        let fresh = run("fresh");
        assert_eq!(fresh, run("sidecar-first"));
        assert_eq!(fresh, run("transcript-first"));
    }

    #[test]
    fn changed_sidecars_replace_usage_and_generation_replay_timestamps() {
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(
            "fixtures/small-grok/.grok/sessions/%2Ftmp%2Fgrok-proj-a/019f5d62-bb11-7c70-b2c6-13166fe9fdee",
        );
        let temp = TempDir::new().unwrap();
        let root = temp.path().join(".grok");
        let session =
            root.join("sessions/%2Ftmp%2Fgrok-proj-a/019f5d62-bb11-7c70-b2c6-13166fe9fdee");
        fs::create_dir_all(&session).unwrap();
        for name in [
            "chat_history.jsonl",
            "summary.json",
            "events.jsonl",
            "signals.json",
        ] {
            fs::copy(fixture.join(name), session.join(name)).unwrap();
        }
        let database_path = temp.path().join("observation.sqlite");
        let engine = SpaghettiEngineCore::open_with_registry(
            EngineOptions {
                database_path: database_path.clone(),
                query_workers: Some(1),
                owner_label: Some("grok-sidecar-change".to_string()),
            },
            AdapterRegistry::builder()
                .register(GrokAdapter::new())
                .build()
                .unwrap(),
        )
        .unwrap();
        engine
            .reconcile_adapter(ADAPTER_ID, ReconcileRequest::manual(vec![root.clone()]))
            .unwrap();
        fs::write(
            session.join("signals.json"),
            r#"{"contextTokensUsed":1234,"contextWindowTokens":500000,"turnCount":1}"#,
        )
        .unwrap();
        fs::write(
            session.join("events.jsonl"),
            concat!(
                r#"{"ts":"2026-04-01T15:00:05.000Z","type":"turn_started","conversation_message_count":0,"turn_number":0}"#,
                "\n",
                r#"{"ts":"2026-04-01T15:00:05.100Z","type":"loop_started","loop_index":0}"#,
                "\n",
                r#"{"ts":"2026-04-01T15:00:06.000Z","type":"first_token"}"#,
                "\n",
                r#"{"ts":"2026-04-01T15:00:10.000Z","type":"turn_ended","outcome":"completed"}"#,
                "\n",
            ),
        )
        .unwrap();
        let outcome = engine
            .reconcile_adapter(ADAPTER_ID, ReconcileRequest::manual(vec![root]))
            .unwrap();
        assert!(
            outcome.objects_changed >= 3,
            "events, signals, transcript replay"
        );
        engine.shutdown().unwrap();
        let connection = rusqlite::Connection::open(database_path).unwrap();
        let usage: i64 = connection
            .query_row(
                "SELECT SUM(estimated_input_tokens) FROM usage_totals",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(usage, 1_234);
        let timestamps = connection
            .prepare("SELECT source_time FROM canonical_messages ORDER BY cursor_end")
            .unwrap()
            .query_map([], |row| row.get::<_, Option<String>>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            timestamps,
            vec![
                Some("2026-04-01T15:00:05.000Z".to_string()),
                Some("2026-04-01T15:00:06.000Z".to_string()),
            ]
        );
    }
}
