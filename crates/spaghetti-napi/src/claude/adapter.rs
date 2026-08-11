//! RFC 011 Claude Code adapter declaration and transcript decoder.
//!
//! Filesystem framing, checkpoints, generations, scheduling, and retries stay
//! in the common source layer. This module owns Claude path and JSON meaning.

use std::path::{Component, Path};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::adapter::{
    AdapterDiagnostic, AdapterError, AdapterErrorClass, AdapterId, AdapterManifest,
    AdapterObjectContext, AgentAdapter, Availability, CapabilityDeclaration, CapabilityGranularity,
    CapabilityId, CapabilitySupport, ConsistencyPolicy, ContentBlock, DecodeContext,
    DecodeDisposition, DecoderId, DelegationFact, DelegationKind, DelegationMetadataFact,
    DeletionPolicy, DiscoveryContext, DriverSpec, EntityKey, EntityScope, EvidenceKind,
    EvidenceStrength, Fact, FactBatch, MessageFact, MessageRole, ObjectSelector,
    QualifiedTimestamp, RawRetentionPolicy, RelationStrength, RunEvidenceFact, RunFact,
    SessionFact, SourceInstance, SourceInstanceKey, SourceInstanceSpec, SourceObjectDescriptor,
    SourceRoot, StreamAuthority, StreamId, StreamSpec, SupportLevel, TimestampQuality, TokenUsage,
    UsageAccounting, UsageFact, UsageScope, ValueQuality,
};
use crate::claude::message_extractor;
use crate::claude::session_metadata;
use crate::claude::types::content::{
    AssistantContentBlock, ToolResultContent, UserContentBlock, UserMessageContent,
};
use crate::claude::types::SessionMessage;
use crate::source::{
    platform_path_key, AppendDelimitedConfig, IngestPriority, ReplaceDocumentConfig,
    SourceDriverError, SourceRecord,
};

const ADAPTER_ID: &str = "claude-code";
const PARENT_STREAM: &str = "session-transcripts";
const SUBAGENT_STREAM: &str = "subagent-transcripts";
const SUBAGENT_META_STREAM: &str = "subagent-metadata";
const PARENT_DECODER: &str = "claude-session-record";
const SUBAGENT_DECODER: &str = "claude-subagent-record";
const SUBAGENT_META_DECODER: &str = "claude-subagent-metadata";
const OBJECT_CONTEXT_VERSION: u32 = 1;
const SUBAGENT_META_MAX_BYTES: usize = 64 * 1024;

const HISTORY_SESSIONS: &str = "history.sessions";
const HISTORY_MESSAGES: &str = "history.messages";
const HISTORY_CONTENT_BLOCKS: &str = "history.content_blocks";
const HISTORY_TIMESTAMPS: &str = "history.timestamps";
const HISTORY_MODEL_IDENTITY: &str = "history.model_identity";
const RUNTIME_SESSION_ACTIVITY: &str = "runtime.session_activity";
const RUNTIME_SUBAGENTS: &str = "runtime.subagents";
const USAGE_INPUT_TOKENS: &str = "usage.input_tokens";
const USAGE_OUTPUT_TOKENS: &str = "usage.output_tokens";
const USAGE_CACHE_TOKENS: &str = "usage.cache_tokens";
const SOURCE_LIVE: &str = "source.live";
const SOURCE_RECONCILE: &str = "source.reconcile";
const SOURCE_RESUME_CURSOR: &str = "source.resume_cursor";

pub struct ClaudeCodeAdapter {
    manifest: AdapterManifest,
}

impl ClaudeCodeAdapter {
    pub fn new() -> Self {
        Self {
            manifest: AdapterManifest {
                id: AdapterId::new(ADAPTER_ID).expect("static Claude adapter id is valid"),
                display_name: "Claude Code".to_string(),
                adapter_version: env!("CARGO_PKG_VERSION").to_string(),
                contract_version: 3,
                source_schema_versions: vec![
                    "claude-code-jsonl-v1".to_string(),
                    "claude-code-subagent-meta-v1".to_string(),
                ],
                capabilities: claude_capabilities(),
            },
        }
    }

    fn adapter_id(&self) -> &AdapterId {
        &self.manifest.id
    }
}

impl Default for ClaudeCodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentAdapter for ClaudeCodeAdapter {
    fn manifest(&self) -> &AdapterManifest {
        &self.manifest
    }

    fn discover(
        &self,
        context: &DiscoveryContext,
    ) -> Result<Vec<SourceInstanceSpec>, AdapterError> {
        let mut instances = Vec::with_capacity(context.configured_roots.len());
        for configured_root in &context.configured_roots {
            let canonical = std::fs::canonicalize(configured_root).map_err(|error| {
                AdapterError::new(
                    AdapterErrorClass::Transient,
                    "claude_root_unavailable",
                    format!("{}: {error}", configured_root.to_string_lossy()),
                )
            })?;
            let metadata = std::fs::metadata(&canonical).map_err(|error| {
                AdapterError::new(
                    AdapterErrorClass::Transient,
                    "claude_root_metadata",
                    format!("{}: {error}", canonical.to_string_lossy()),
                )
            })?;
            if !metadata.is_dir() {
                return Err(AdapterError::new(
                    AdapterErrorClass::AdapterFatal,
                    "claude_root_not_directory",
                    canonical.to_string_lossy(),
                ));
            }
            instances.push(SourceInstanceSpec {
                stable_key: SourceInstanceKey::new(platform_path_key(&canonical))?,
                display_name: format!("Claude Code ({})", canonical.to_string_lossy()),
                roots: vec![
                    SourceRoot {
                        name: "home".to_string(),
                        path: canonical.clone(),
                    },
                    SourceRoot {
                        name: "projects".to_string(),
                        path: canonical.join("projects"),
                    },
                ],
                discovery_reason: "configured Claude Code data root".to_string(),
            });
        }
        Ok(instances)
    }

    fn streams(&self, instance: &SourceInstance) -> Result<Vec<StreamSpec>, AdapterError> {
        let streams = vec![
            StreamSpec {
                id: StreamId::new(PARENT_STREAM)?,
                driver: DriverSpec::AppendDelimited(AppendDelimitedConfig::json_lines()),
                selector: ObjectSelector {
                    root_name: "projects".to_string(),
                    include: vec!["*/*.jsonl".to_string()],
                    exclude: Vec::new(),
                },
                decoder: DecoderId::new(PARENT_DECODER)?,
                authority: StreamAuthority::Canonical,
                entity_scope: EntityScope::Session,
                priority: IngestPriority::Interactive,
                consistency: ConsistencyPolicy::IncrementalCursor,
                deletion: DeletionPolicy::MirrorSource,
                retention: RawRetentionPolicy::Full,
                capabilities: transcript_capabilities(false),
            },
            StreamSpec {
                id: StreamId::new(SUBAGENT_STREAM)?,
                driver: DriverSpec::AppendDelimited(AppendDelimitedConfig::json_lines()),
                selector: ObjectSelector {
                    root_name: "projects".to_string(),
                    include: vec!["*/*/subagents/**/agent-*.jsonl".to_string()],
                    exclude: Vec::new(),
                },
                decoder: DecoderId::new(SUBAGENT_DECODER)?,
                authority: StreamAuthority::Canonical,
                entity_scope: EntityScope::Run,
                priority: IngestPriority::Interactive,
                consistency: ConsistencyPolicy::IncrementalCursor,
                deletion: DeletionPolicy::MirrorSource,
                retention: RawRetentionPolicy::Full,
                capabilities: transcript_capabilities(true),
            },
            StreamSpec {
                id: StreamId::new(SUBAGENT_META_STREAM)?,
                driver: DriverSpec::ReplaceDocument(ReplaceDocumentConfig {
                    max_document_bytes: SUBAGENT_META_MAX_BYTES,
                }),
                selector: ObjectSelector {
                    root_name: "projects".to_string(),
                    include: vec!["*/*/subagents/**/agent-*.meta.json".to_string()],
                    exclude: Vec::new(),
                },
                decoder: DecoderId::new(SUBAGENT_META_DECODER)?,
                authority: StreamAuthority::Supplemental,
                entity_scope: EntityScope::Run,
                priority: IngestPriority::ForegroundRepair,
                consistency: ConsistencyPolicy::SnapshotReplace,
                deletion: DeletionPolicy::MirrorSource,
                retention: RawRetentionPolicy::Full,
                capabilities: subagent_metadata_capabilities(),
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
        object: &SourceObjectDescriptor,
    ) -> Result<AdapterObjectContext, AdapterError> {
        let payload = match object.stream_id.as_str() {
            PARENT_STREAM => {
                encode_object_context(&ClaudeTranscriptContext::parent(&object.relative_path)?)?
            }
            SUBAGENT_STREAM => {
                encode_object_context(&ClaudeTranscriptContext::subagent(&object.relative_path)?)?
            }
            SUBAGENT_META_STREAM => encode_object_context(&ClaudeSubagentMetadataContext {
                child: ClaudeTranscriptContext::subagent_meta(&object.relative_path)?,
            })?,
            _ => {
                return Err(AdapterError::new(
                    AdapterErrorClass::StreamFatal,
                    "claude_unknown_stream",
                    format!("unknown Claude stream {}", object.stream_id),
                ));
            }
        };
        AdapterObjectContext::new(OBJECT_CONTEXT_VERSION, payload)
    }

    fn decode(
        &self,
        context: DecodeContext<'_>,
        record: &SourceRecord,
        output: &mut FactBatch,
    ) -> Result<DecodeDisposition, AdapterError> {
        match context.decoder.as_str() {
            PARENT_DECODER => {
                let object_context = ClaudeTranscriptContext::decode(context.object_context)?;
                if object_context.agent_id.is_some() {
                    return Err(decoder_context_mismatch());
                }
                decode_transcript_record(self.adapter_id(), &object_context, record, output)
            }
            SUBAGENT_DECODER => {
                let object_context = ClaudeTranscriptContext::decode(context.object_context)?;
                if object_context.agent_id.is_none() {
                    return Err(decoder_context_mismatch());
                }
                decode_transcript_record(self.adapter_id(), &object_context, record, output)
            }
            SUBAGENT_META_DECODER => {
                let object_context = ClaudeSubagentMetadataContext::decode(context.object_context)?;
                decode_subagent_metadata(self.adapter_id(), &object_context.child, record, output)
            }
            _ => Err(AdapterError::unknown_decoder(context.decoder)),
        }
    }
}

fn claude_capabilities() -> Vec<CapabilityDeclaration> {
    let live_native = |id, granularity| {
        capability(
            id,
            SupportLevel::Native,
            granularity,
            Availability::Live,
            None,
        )
    };
    vec![
        live_native(HISTORY_SESSIONS, CapabilityGranularity::Session),
        live_native(HISTORY_MESSAGES, CapabilityGranularity::Message),
        live_native(HISTORY_CONTENT_BLOCKS, CapabilityGranularity::Message),
        live_native(HISTORY_TIMESTAMPS, CapabilityGranularity::Message),
        live_native(HISTORY_MODEL_IDENTITY, CapabilityGranularity::Message),
        live_native(RUNTIME_SESSION_ACTIVITY, CapabilityGranularity::Run),
        capability(
            RUNTIME_SUBAGENTS,
            SupportLevel::Derived,
            CapabilityGranularity::Run,
            Availability::Live,
            Some(
                "child identity is native and metadata is supplemental; parent lineage currently comes from Claude's transcript layout; silence never implies completion",
            ),
        ),
        live_native(USAGE_INPUT_TOKENS, CapabilityGranularity::Message),
        live_native(USAGE_OUTPUT_TOKENS, CapabilityGranularity::Message),
        live_native(USAGE_CACHE_TOKENS, CapabilityGranularity::Message),
        live_native(SOURCE_LIVE, CapabilityGranularity::Instance),
        live_native(SOURCE_RECONCILE, CapabilityGranularity::Instance),
        live_native(SOURCE_RESUME_CURSOR, CapabilityGranularity::Record),
    ]
}

fn capability(
    id: &'static str,
    level: SupportLevel,
    granularity: CapabilityGranularity,
    availability: Availability,
    notes: Option<&'static str>,
) -> CapabilityDeclaration {
    CapabilityDeclaration {
        id: CapabilityId::new(id).expect("static Claude capability id is valid"),
        support: CapabilitySupport {
            level,
            granularity,
            availability,
            notes: notes.map(str::to_owned),
        },
    }
}

fn transcript_capabilities(include_subagents: bool) -> Vec<CapabilityId> {
    let mut capabilities = vec![
        HISTORY_SESSIONS,
        HISTORY_MESSAGES,
        HISTORY_CONTENT_BLOCKS,
        HISTORY_TIMESTAMPS,
        HISTORY_MODEL_IDENTITY,
        RUNTIME_SESSION_ACTIVITY,
        USAGE_INPUT_TOKENS,
        USAGE_OUTPUT_TOKENS,
        USAGE_CACHE_TOKENS,
        SOURCE_LIVE,
        SOURCE_RECONCILE,
        SOURCE_RESUME_CURSOR,
    ];
    if include_subagents {
        capabilities.push(RUNTIME_SUBAGENTS);
    }
    capabilities
        .into_iter()
        .map(|id| CapabilityId::new(id).expect("static Claude stream capability id is valid"))
        .collect()
}

fn subagent_metadata_capabilities() -> Vec<CapabilityId> {
    [
        RUNTIME_SUBAGENTS,
        SOURCE_LIVE,
        SOURCE_RECONCILE,
        SOURCE_RESUME_CURSOR,
    ]
    .into_iter()
    .map(|id| CapabilityId::new(id).expect("static Claude stream capability id is valid"))
    .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ClaudeTranscriptContext {
    project_slug: String,
    session_id: String,
    agent_id: Option<String>,
    workflow_id: Option<String>,
}

impl ClaudeTranscriptContext {
    fn parent(relative_path: &Path) -> Result<Self, AdapterError> {
        let components = utf8_components(relative_path)?;
        if components.len() != 2 {
            return Err(path_error(
                relative_path,
                "parent transcript path must have two components",
            ));
        }
        let session_id = jsonl_stem(&components[1], relative_path)?;
        if !is_uuid(&session_id) {
            return Err(path_error(
                relative_path,
                "parent transcript name is not a UUID",
            ));
        }
        Ok(Self {
            project_slug: components[0].clone(),
            session_id,
            agent_id: None,
            workflow_id: None,
        })
    }

    fn subagent(relative_path: &Path) -> Result<Self, AdapterError> {
        Self::subagent_object(relative_path, ".jsonl")
    }

    fn subagent_meta(relative_path: &Path) -> Result<Self, AdapterError> {
        Self::subagent_object(relative_path, ".meta.json")
    }

    fn subagent_object(relative_path: &Path, suffix: &str) -> Result<Self, AdapterError> {
        let components = utf8_components(relative_path)?;
        if components.len() < 4 || components.get(2).map(String::as_str) != Some("subagents") {
            return Err(path_error(
                relative_path,
                "subagent object path does not match Claude layout",
            ));
        }
        let file_name = components.last().expect("minimum component count checked");
        let Some(agent_id) = file_name
            .strip_prefix("agent-")
            .and_then(|name| name.strip_suffix(suffix))
            .filter(|value| !value.is_empty())
        else {
            return Err(path_error(relative_path, "invalid subagent object name"));
        };
        let workflow_id = components.windows(2).find_map(|pair| {
            (pair[0] == "workflows" && !pair[1].is_empty()).then(|| pair[1].clone())
        });
        Ok(Self {
            project_slug: components[0].clone(),
            session_id: components[1].clone(),
            agent_id: Some(agent_id.to_string()),
            workflow_id,
        })
    }

    fn decode(context: &AdapterObjectContext) -> Result<Self, AdapterError> {
        decode_object_context(context)
    }

    fn run_native_key(&self) -> String {
        match &self.agent_id {
            Some(agent_id) => format!(
                "{}\0{}\0{}",
                self.session_id,
                self.workflow_id.as_deref().unwrap_or(""),
                agent_id
            ),
            None => self.session_id.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ClaudeSubagentMetadataContext {
    child: ClaudeTranscriptContext,
}

impl ClaudeSubagentMetadataContext {
    fn decode(context: &AdapterObjectContext) -> Result<Self, AdapterError> {
        decode_object_context(context)
    }
}

fn encode_object_context<T: Serialize>(context: &T) -> Result<Vec<u8>, AdapterError> {
    serde_json::to_vec(context).map_err(|error| {
        AdapterError::new(
            AdapterErrorClass::AdapterFatal,
            "claude_context_encode",
            error.to_string(),
        )
    })
}

fn decode_object_context<T: DeserializeOwned>(
    context: &AdapterObjectContext,
) -> Result<T, AdapterError> {
    if context.version() != OBJECT_CONTEXT_VERSION {
        return Err(AdapterError::new(
            AdapterErrorClass::StreamFatal,
            "claude_context_version",
            format!(
                "unsupported Claude object context version {}",
                context.version()
            ),
        ));
    }
    serde_json::from_slice(context.payload()).map_err(|error| {
        AdapterError::new(
            AdapterErrorClass::StreamFatal,
            "claude_context_decode",
            error.to_string(),
        )
    })
}

fn decoder_context_mismatch() -> AdapterError {
    AdapterError::new(
        AdapterErrorClass::InvalidContract,
        "claude_decoder_context_mismatch",
        "Claude decoder does not match bootstrapped object kind",
    )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeSubagentMetadataDocument {
    agent_type: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    spawn_depth: Option<u32>,
    #[serde(default)]
    worktree_path: Option<String>,
    #[serde(default)]
    tool_use_id: Option<String>,
}

fn decode_subagent_metadata(
    adapter_id: &AdapterId,
    context: &ClaudeTranscriptContext,
    record: &SourceRecord,
    output: &mut FactBatch,
) -> Result<DecodeDisposition, AdapterError> {
    let document: ClaudeSubagentMetadataDocument = match serde_json::from_slice(&record.payload) {
        Ok(document) => document,
        Err(error) => {
            preserve_unknown(
                record,
                output,
                Some("subagent_metadata".to_string()),
                format!("Claude subagent metadata is not a supported JSON object: {error}"),
            )?;
            return Ok(DecodeDisposition::PreservedUnknown);
        }
    };
    let Some(agent_type) = nonempty(&document.agent_type) else {
        preserve_unknown(
            record,
            output,
            Some("subagent_metadata".to_string()),
            "Claude subagent metadata has an empty agentType".to_string(),
        )?;
        return Ok(DecodeDisposition::PreservedUnknown);
    };
    let agent_id = context.agent_id.as_deref().ok_or_else(|| {
        AdapterError::invalid_contract("subagent metadata context has no native child id")
    })?;
    let session = EntityKey::native(
        adapter_id,
        record.source_instance_id,
        "session",
        context.session_id.as_bytes(),
    )?;
    let run_native_key = context.run_native_key();
    let child_run = EntityKey::native(
        adapter_id,
        record.source_instance_id,
        "run",
        run_native_key.as_bytes(),
    )?;
    output.push(
        record,
        Fact::DelegationMetadata(DelegationMetadataFact {
            child_run,
            session,
            native_child_id: agent_id.to_string(),
            agent_type,
            description: document.description.as_deref().and_then(nonempty),
            name: document.name.as_deref().and_then(nonempty),
            spawn_depth: document.spawn_depth,
            worktree_path: document.worktree_path.as_deref().and_then(nonempty),
            native_task_id: document.tool_use_id.as_deref().and_then(nonempty),
        }),
    )?;
    Ok(DecodeDisposition::Applied)
}

fn decode_transcript_record(
    adapter_id: &AdapterId,
    context: &ClaudeTranscriptContext,
    record: &SourceRecord,
    output: &mut FactBatch,
) -> Result<DecodeDisposition, AdapterError> {
    let raw = match std::str::from_utf8(&record.payload) {
        Ok(raw) => raw,
        Err(error) => {
            preserve_unknown(
                record,
                output,
                None,
                format!("Claude JSONL record is not UTF-8: {error}"),
            )?;
            return Ok(DecodeDisposition::PreservedUnknown);
        }
    };
    let value: Value = match serde_json::from_str(raw) {
        Ok(value) => value,
        Err(error) => {
            preserve_unknown(
                record,
                output,
                None,
                format!("Claude JSONL record is not valid JSON: {error}"),
            )?;
            return Ok(DecodeDisposition::PreservedUnknown);
        }
    };
    let projection = message_extractor::project_jsonl_line(raw).map_err(|error| {
        AdapterError::new(
            AdapterErrorClass::RecordPermanent,
            "claude_projection",
            error.to_string(),
        )
    })?;
    let typed = serde_json::from_str::<SessionMessage>(raw);

    let project = EntityKey::native(
        adapter_id,
        record.source_instance_id,
        "project",
        context.project_slug.as_bytes(),
    )?;
    let session = EntityKey::native(
        adapter_id,
        record.source_instance_id,
        "session",
        context.session_id.as_bytes(),
    )?;
    let run_native_key = context.run_native_key();
    let run = EntityKey::native(
        adapter_id,
        record.source_instance_id,
        "run",
        run_native_key.as_bytes(),
    )?;
    let source_time = projection.timestamp.as_deref().map(native_timestamp);
    let metadata = if context.agent_id.is_none() {
        session_metadata::project_session_metadata(&value).unwrap_or_default()
    } else {
        session_metadata::SessionMetadataProjection::default()
    };
    output.push(
        record,
        Fact::Session(SessionFact {
            session: session.clone(),
            project,
            native_session_id: context.session_id.clone(),
            native_project_key: context.project_slug.clone(),
            cwd: nonempty_field(&value, "cwd"),
            git_branch: nonempty_field(&value, "gitBranch"),
            first_prompt: metadata.human_prompt,
            ai_title: metadata.ai_title,
            custom_title: metadata.custom_title,
            source_time: source_time.clone(),
        }),
    )?;
    let parent_run = if context.agent_id.is_some() {
        Some(EntityKey::native(
            adapter_id,
            record.source_instance_id,
            "run",
            context.session_id.as_bytes(),
        )?)
    } else {
        None
    };
    let delegation_parent = parent_run.clone();
    output.push(
        record,
        Fact::Run(RunFact {
            run: run.clone(),
            session: session.clone(),
            native_run_id: run_native_key,
            parent_run,
        }),
    )?;
    if let Some(agent_id) = &context.agent_id {
        output.push(
            record,
            Fact::Delegation(DelegationFact {
                child_run: run.clone(),
                parent_run: delegation_parent,
                session: session.clone(),
                kind: DelegationKind::VendorNativeSubagent,
                relation_strength: RelationStrength::Layout,
                native_child_id: Some(agent_id.clone()),
                native_task_id: None,
                label: None,
                prompt: None,
                cwd: nonempty_field(&value, "cwd"),
                worktree_path: None,
                source_time: source_time.clone(),
            }),
        )?;
    }

    let native_message_id = projection.uuid.filter(|value| !value.is_empty());
    let message_native_key = message_native_key(context, record, native_message_id.as_deref());
    let message = EntityKey::native(
        adapter_id,
        record.source_instance_id,
        "message",
        &message_native_key,
    )?;
    let (role, content, parent_native_message_id, model) = match &typed {
        Ok(message) => decode_message_content(message, &value),
        Err(error) => {
            output.push_diagnostic(AdapterDiagnostic {
                class: AdapterErrorClass::RecordPermanent,
                code: "claude_typed_projection_loss".to_string(),
                message: format!("type={}: {error}", projection.msg_type),
            })?;
            (
                role_from_kind(&projection.msg_type),
                Vec::new(),
                nonempty_field(&value, "parentUuid"),
                value
                    .get("message")
                    .and_then(|message| message.get("model"))
                    .and_then(Value::as_str)
                    .filter(|model| !model.is_empty())
                    .map(str::to_owned),
            )
        }
    };
    output.push(
        record,
        Fact::Message(MessageFact {
            message: message.clone(),
            session: session.clone(),
            native_message_id,
            native_kind: projection.msg_type,
            role,
            content,
            source_time: source_time.clone(),
            parent_native_message_id,
            model: model.clone(),
            search_text: projection.fts_text,
            raw_json: record.payload.clone(),
        }),
    )?;
    output.push(
        record,
        Fact::RunEvidence(RunEvidenceFact {
            run,
            kind: EvidenceKind::ActivityObserved,
            strength: EvidenceStrength::NativeActivity,
            native_state: None,
            source_time: source_time.clone(),
        }),
    )?;

    let usage = TokenUsage {
        input_tokens: projection.input_tokens,
        output_tokens: projection.output_tokens,
        cache_creation_tokens: projection.cache_creation_tokens,
        cache_read_tokens: projection.cache_read_tokens,
    };
    if !usage.is_zero() {
        output.push(
            record,
            Fact::Usage(UsageFact {
                subject: message,
                session,
                scope: UsageScope::Message,
                accounting: UsageAccounting::Delta,
                quality: ValueQuality::NativeExact,
                values: usage,
                model,
                source_time,
            }),
        )?;
    }
    Ok(DecodeDisposition::Applied)
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
        code: "claude_preserved_unknown".to_string(),
        message: reason,
    })
}

fn decode_message_content(
    message: &SessionMessage,
    raw: &Value,
) -> (
    MessageRole,
    Vec<ContentBlock>,
    Option<String>,
    Option<String>,
) {
    match message {
        SessionMessage::User(user) => {
            let blocks = match &user.message.content {
                UserMessageContent::Text(text) => vec![ContentBlock::Text { text: text.clone() }],
                UserMessageContent::Blocks(blocks) => blocks.iter().map(user_block).collect(),
            };
            (
                MessageRole::User,
                blocks,
                user.base.parent_uuid.clone(),
                None,
            )
        }
        SessionMessage::Assistant(assistant) => (
            MessageRole::Assistant,
            assistant
                .message
                .content
                .iter()
                .map(assistant_block)
                .collect(),
            assistant.base.parent_uuid.clone(),
            nonempty(&assistant.message.model),
        ),
        SessionMessage::System(system) => (
            MessageRole::System,
            system
                .content_str()
                .map(|text| {
                    vec![ContentBlock::Text {
                        text: text.to_string(),
                    }]
                })
                .unwrap_or_default(),
            system.base.parent_uuid.clone(),
            None,
        ),
        SessionMessage::Summary(summary) => (
            MessageRole::Summary,
            vec![ContentBlock::Text {
                text: summary.summary.clone(),
            }],
            nonempty(&summary.leaf_uuid),
            None,
        ),
        SessionMessage::Unknown => (
            MessageRole::Other("unknown".to_string()),
            vec![ContentBlock::Native {
                native_kind: raw
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string(),
                value: raw.clone(),
            }],
            nonempty_field(raw, "parentUuid"),
            None,
        ),
        _ => (
            role_from_kind(raw.get("type").and_then(Value::as_str).unwrap_or("unknown")),
            Vec::new(),
            nonempty_field(raw, "parentUuid"),
            None,
        ),
    }
}

fn assistant_block(block: &AssistantContentBlock) -> ContentBlock {
    match block {
        AssistantContentBlock::Text(block) => ContentBlock::Text {
            text: block.text.clone(),
        },
        AssistantContentBlock::Thinking(block) => ContentBlock::Thinking {
            text: block.thinking.clone(),
            redacted: false,
        },
        AssistantContentBlock::RedactedThinking(_) => ContentBlock::Thinking {
            text: String::new(),
            redacted: true,
        },
        AssistantContentBlock::ToolUse(block) => ContentBlock::ToolCall {
            native_id: block.id.clone(),
            name: block.name.clone(),
            input: block.input.clone(),
        },
    }
}

fn user_block(block: &UserContentBlock) -> ContentBlock {
    match block {
        UserContentBlock::Text(block) => ContentBlock::Text {
            text: block.text.clone(),
        },
        UserContentBlock::ToolResult(block) => ContentBlock::ToolResult {
            native_call_id: block.tool_use_id.clone(),
            content: match &block.content {
                ToolResultContent::Text(text) => Value::String(text.clone()),
                ToolResultContent::Blocks(blocks) => {
                    serde_json::to_value(blocks).unwrap_or(Value::Null)
                }
            },
            is_error: block.is_error.unwrap_or(false),
        },
        UserContentBlock::Image(block) => ContentBlock::Image {
            media_type: block.source.media_type.clone(),
            data_hash: *blake3::hash(block.source.data.as_bytes()).as_bytes(),
        },
        UserContentBlock::Document(block) => ContentBlock::Document {
            media_type: block.source.media_type.clone(),
            data_hash: *blake3::hash(block.source.data.as_bytes()).as_bytes(),
        },
    }
}

fn message_native_key(
    context: &ClaudeTranscriptContext,
    record: &SourceRecord,
    native_message_id: Option<&str>,
) -> Vec<u8> {
    let mut key = Vec::new();
    push_key_component(&mut key, context.session_id.as_bytes());
    push_key_component(
        &mut key,
        context.workflow_id.as_deref().unwrap_or("").as_bytes(),
    );
    push_key_component(
        &mut key,
        context.agent_id.as_deref().unwrap_or("").as_bytes(),
    );
    if let Some(native_message_id) = native_message_id {
        push_key_component(&mut key, native_message_id.as_bytes());
    } else {
        push_key_component(&mut key, &record.object_id.to_be_bytes());
        push_key_component(&mut key, &record.generation.to_be_bytes());
        push_key_component(&mut key, record.cursor_start.as_bytes());
        push_key_component(&mut key, record.cursor_end.as_bytes());
    }
    key
}

fn push_key_component(output: &mut Vec<u8>, value: &[u8]) {
    output.extend_from_slice(&(value.len() as u64).to_be_bytes());
    output.extend_from_slice(value);
}

fn role_from_kind(kind: &str) -> MessageRole {
    match kind {
        "user" => MessageRole::User,
        "assistant" => MessageRole::Assistant,
        "system" => MessageRole::System,
        "summary" => MessageRole::Summary,
        other => MessageRole::Other(other.to_string()),
    }
}

fn native_timestamp(value: &str) -> QualifiedTimestamp {
    QualifiedTimestamp {
        value: value.to_string(),
        quality: TimestampQuality::NativeExact,
    }
}

fn nonempty(value: &str) -> Option<String> {
    (!value.trim().is_empty()).then(|| value.to_string())
}

fn nonempty_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).and_then(nonempty)
}

fn utf8_components(path: &Path) -> Result<Vec<String>, AdapterError> {
    path.components()
        .map(|component| match component {
            Component::Normal(value) => value
                .to_str()
                .map(str::to_owned)
                .ok_or_else(|| path_error(path, "Claude source identifiers must be valid UTF-8")),
            _ => Err(path_error(path, "Claude object path is not confined")),
        })
        .collect()
}

fn jsonl_stem(file_name: &str, path: &Path) -> Result<String, AdapterError> {
    file_name
        .strip_suffix(".jsonl")
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| path_error(path, "Claude transcript does not have a JSONL name"))
}

fn is_uuid(value: &str) -> bool {
    let lengths = [8, 4, 4, 4, 12];
    let mut parts = value.split('-');
    lengths.iter().all(|length| {
        parts.next().is_some_and(|part| {
            part.len() == *length && part.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
    }) && parts.next().is_none()
}

fn path_error(path: &Path, detail: &str) -> AdapterError {
    AdapterError::new(
        AdapterErrorClass::StreamFatal,
        "claude_object_path",
        format!("{}: {detail}", path.to_string_lossy()),
    )
}

impl From<SourceDriverError> for AdapterError {
    fn from(error: SourceDriverError) -> Self {
        AdapterError::new(
            AdapterErrorClass::Transient,
            "source_driver",
            error.to_string(),
        )
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::adapter::{FactEnvelope, SourceInstance};
    use crate::source::{
        AppendDelimitedFile, AppendItem, AppendRead, RecordOrigin, SourceMediaType,
    };

    const SESSION: &str = "01234567-89ab-cdef-0123-456789abcdef";

    fn instance(root: &Path) -> SourceInstance {
        SourceInstance {
            id: 7,
            spec: SourceInstanceSpec {
                stable_key: SourceInstanceKey::new(platform_path_key(root)).unwrap(),
                display_name: "fixture".to_string(),
                roots: vec![SourceRoot {
                    name: "projects".to_string(),
                    path: root.join("projects"),
                }],
                discovery_reason: "fixture".to_string(),
            },
        }
    }

    fn object(stream: &str, relative: &str) -> SourceObjectDescriptor {
        SourceObjectDescriptor {
            stream_id: StreamId::new(stream).unwrap(),
            object_key: relative.as_bytes().to_vec(),
            relative_path: Path::new(relative).to_path_buf(),
        }
    }

    fn record(payload: &[u8]) -> SourceRecord {
        SourceRecord::new(
            &RecordOrigin {
                source_instance_id: 7,
                stream_id: 8,
                object_id: 9,
                observed_at: 10,
                source_timestamp_hint: None,
                media_type: SourceMediaType::new("application/x-ndjson").unwrap(),
            },
            1,
            crate::source::SourceCursor::append_offset(0),
            crate::source::SourceCursor::append_offset(payload.len() as u64 + 1),
            0,
            payload.to_vec(),
        )
    }

    fn fact_values(batch: &FactBatch) -> impl Iterator<Item = &Fact> {
        batch.facts().iter().map(|FactEnvelope { value, .. }| value)
    }

    #[test]
    fn discovery_and_streams_are_declarative_and_use_common_drivers() {
        let root = TempDir::new().unwrap();
        std::fs::create_dir(root.path().join("projects")).unwrap();
        let adapter = ClaudeCodeAdapter::new();
        let discovered = adapter
            .discover(&DiscoveryContext {
                configured_roots: vec![root.path().to_path_buf()],
                observed_at: 1,
            })
            .unwrap();
        assert_eq!(discovered.len(), 1);
        let streams = adapter
            .streams(&SourceInstance {
                id: 7,
                spec: discovered[0].clone(),
            })
            .unwrap();
        assert_eq!(streams.len(), 3);
        assert_eq!(adapter.manifest().contract_version, 3);
        assert!(adapter
            .manifest()
            .source_schema_versions
            .iter()
            .any(|version| version == "claude-code-subagent-meta-v1"));
        assert!(matches!(streams[0].driver, DriverSpec::AppendDelimited(_)));
        assert!(matches!(streams[1].driver, DriverSpec::AppendDelimited(_)));
        assert!(matches!(streams[2].driver, DriverSpec::ReplaceDocument(_)));
        assert_eq!(streams[0].decoder.as_str(), PARENT_DECODER);
        assert_eq!(streams[1].decoder.as_str(), SUBAGENT_DECODER);
        assert_eq!(streams[2].decoder.as_str(), SUBAGENT_META_DECODER);
        assert_eq!(streams[2].authority, StreamAuthority::Supplemental);
        assert_eq!(streams[2].consistency, ConsistencyPolicy::SnapshotReplace);
        assert!(!streams[0]
            .capabilities
            .iter()
            .any(|capability| capability.as_str() == RUNTIME_SUBAGENTS));
        assert!(streams[1]
            .capabilities
            .iter()
            .any(|capability| capability.as_str() == RUNTIME_SUBAGENTS));
        assert!(streams[2]
            .capabilities
            .iter()
            .any(|capability| capability.as_str() == RUNTIME_SUBAGENTS));
        let support = adapter
            .manifest()
            .capabilities
            .iter()
            .find(|capability| capability.id.as_str() == RUNTIME_SUBAGENTS)
            .unwrap();
        assert_eq!(support.support.level, SupportLevel::Derived);
        assert_eq!(support.support.granularity, CapabilityGranularity::Run);
        assert_eq!(support.support.availability, Availability::Live);
    }

    #[test]
    fn parent_context_is_derived_from_the_catalogued_relative_path() {
        let root = TempDir::new().unwrap();
        let adapter = ClaudeCodeAdapter::new();
        let context = adapter
            .bootstrap_object(
                &instance(root.path()),
                &object(PARENT_STREAM, &format!("project/{SESSION}.jsonl")),
            )
            .unwrap();
        let decoded = ClaudeTranscriptContext::decode(&context).unwrap();
        assert_eq!(decoded.project_slug, "project");
        assert_eq!(decoded.session_id, SESSION);
        assert!(decoded.agent_id.is_none());
    }

    #[test]
    fn assistant_record_emits_session_message_run_activity_and_exact_usage() {
        let root = TempDir::new().unwrap();
        let adapter = ClaudeCodeAdapter::new();
        let object_context = adapter
            .bootstrap_object(
                &instance(root.path()),
                &object(PARENT_STREAM, &format!("project/{SESSION}.jsonl")),
            )
            .unwrap();
        let record = record(
            format!(
                r#"{{"type":"assistant","uuid":"m1","parentUuid":"u1","timestamp":"2026-08-11T00:00:00Z","sessionId":"{SESSION}","cwd":"/repo","version":"1","gitBranch":"main","isSidechain":false,"userType":"external","requestId":"r1","message":{{"model":"claude-sonnet","id":"api1","type":"message","role":"assistant","content":[{{"type":"text","text":"hello"}},{{"type":"tool_use","id":"tool1","name":"Read","input":{{"file":"x"}}}}],"usage":{{"input_tokens":10,"output_tokens":5,"cache_creation_input_tokens":2,"cache_read_input_tokens":3}}}}}}"#
            )
            .as_bytes(),
        );
        let mut batch = FactBatch::new(8, 4).unwrap();
        let disposition = adapter
            .decode(
                DecodeContext {
                    decoder: &DecoderId::new(PARENT_DECODER).unwrap(),
                    object_context: &object_context,
                },
                &record,
                &mut batch,
            )
            .unwrap();
        assert_eq!(disposition, DecodeDisposition::Applied);
        assert_eq!(batch.facts().len(), 5);
        let message = fact_values(&batch)
            .find_map(|fact| match fact {
                Fact::Message(message) => Some(message),
                _ => None,
            })
            .unwrap();
        assert_eq!(message.role, MessageRole::Assistant);
        assert_eq!(message.content.len(), 2);
        assert_eq!(message.model.as_deref(), Some("claude-sonnet"));
        let usage = fact_values(&batch)
            .find_map(|fact| match fact {
                Fact::Usage(usage) => Some(usage),
                _ => None,
            })
            .unwrap();
        assert_eq!(usage.values.input_tokens, 10);
        assert_eq!(usage.values.cache_read_tokens, 3);
        assert_eq!(usage.scope, UsageScope::Message);
        assert_eq!(usage.accounting, UsageAccounting::Delta);
    }

    #[test]
    fn invalid_complete_json_is_preserved_unknown_and_can_advance() {
        let root = TempDir::new().unwrap();
        let adapter = ClaudeCodeAdapter::new();
        let object_context = adapter
            .bootstrap_object(
                &instance(root.path()),
                &object(PARENT_STREAM, &format!("project/{SESSION}.jsonl")),
            )
            .unwrap();
        let record = record(b"not-json");
        let mut batch = FactBatch::new(4, 4).unwrap();
        let disposition = adapter
            .decode(
                DecodeContext {
                    decoder: &DecoderId::new(PARENT_DECODER).unwrap(),
                    object_context: &object_context,
                },
                &record,
                &mut batch,
            )
            .unwrap();
        assert_eq!(disposition, DecodeDisposition::PreservedUnknown);
        assert!(matches!(batch.facts()[0].value, Fact::UnknownRecord { .. }));
        assert_eq!(batch.diagnostics().len(), 1);
    }

    #[test]
    fn shared_append_driver_never_sends_partial_json_to_claude_decoder() {
        let root = TempDir::new().unwrap();
        let path = root.path().join("session.jsonl");
        std::fs::write(&path, b"{\"type\":\"future\"}\n{\"type\":").unwrap();
        let driver = AppendDelimitedFile::new(AppendDelimitedConfig::json_lines()).unwrap();
        let AppendRead::Batch {
            items,
            checkpoint,
            needs_retry,
            ..
        } = driver
            .read(
                &path,
                None,
                &RecordOrigin {
                    source_instance_id: 7,
                    stream_id: 8,
                    object_id: 9,
                    observed_at: 10,
                    source_timestamp_hint: None,
                    media_type: SourceMediaType::new("application/x-ndjson").unwrap(),
                },
                false,
            )
            .unwrap()
        else {
            panic!("expected append batch");
        };
        assert_eq!(items.len(), 1);
        assert!(matches!(items[0], AppendItem::Record(_)));
        assert_eq!(checkpoint.committed_offset, 18);
        assert!(needs_retry);
    }

    #[test]
    fn subagent_context_preserves_parent_and_workflow_identity() {
        let root = TempDir::new().unwrap();
        let adapter = ClaudeCodeAdapter::new();
        let context = adapter
            .bootstrap_object(
                &instance(root.path()),
                &object(
                    SUBAGENT_STREAM,
                    &format!("project/{SESSION}/subagents/workflows/w1/agents/agent-a1.jsonl"),
                ),
            )
            .unwrap();
        let decoded = ClaudeTranscriptContext::decode(&context).unwrap();
        assert_eq!(decoded.session_id, SESSION);
        assert_eq!(decoded.agent_id.as_deref(), Some("a1"));
        assert_eq!(decoded.workflow_id.as_deref(), Some("w1"));

        let record = record(
            format!(
                r#"{{"type":"user","uuid":"child-message","parentUuid":null,"timestamp":"2026-08-11T00:00:00Z","sessionId":"{SESSION}","cwd":"/repo/.worktrees/a1","version":"1","gitBranch":"main","isSidechain":false,"userType":"external","message":{{"role":"user","content":"inspect the parser"}}}}"#
            )
            .as_bytes(),
        );
        let mut batch = FactBatch::new(8, 4).unwrap();
        adapter
            .decode(
                DecodeContext {
                    decoder: &DecoderId::new(SUBAGENT_DECODER).unwrap(),
                    object_context: &context,
                },
                &record,
                &mut batch,
            )
            .unwrap();
        let delegation = fact_values(&batch)
            .find_map(|fact| match fact {
                Fact::Delegation(delegation) => Some(delegation),
                _ => None,
            })
            .unwrap();
        assert_eq!(delegation.kind, DelegationKind::VendorNativeSubagent);
        assert_eq!(delegation.relation_strength, RelationStrength::Layout);
        assert_eq!(delegation.native_child_id.as_deref(), Some("a1"));
        assert_eq!(delegation.cwd.as_deref(), Some("/repo/.worktrees/a1"));
        assert!(delegation.parent_run.is_some());
    }

    #[test]
    fn subagent_metadata_uses_the_same_child_key_without_strengthening_lineage() {
        let root = TempDir::new().unwrap();
        let adapter = ClaudeCodeAdapter::new();
        let relative =
            format!("project/{SESSION}/subagents/workflows/w1/agents/agent-a1.meta.json");
        let context = adapter
            .bootstrap_object(
                &instance(root.path()),
                &object(SUBAGENT_META_STREAM, &relative),
            )
            .unwrap();
        let decoded = ClaudeSubagentMetadataContext::decode(&context)
            .unwrap()
            .child;
        assert_eq!(decoded.run_native_key(), format!("{SESSION}\0w1\0a1"));

        let metadata_record = record(
            br#"{"agentType":"Explore","description":"map the parser","name":"survey","spawnDepth":2,"worktreePath":"/repo/.worktrees/a1","toolUseId":"tool-7"}"#,
        );
        let mut mismatched_batch = FactBatch::new(4, 4).unwrap();
        assert!(adapter
            .decode(
                DecodeContext {
                    decoder: &DecoderId::new(SUBAGENT_DECODER).unwrap(),
                    object_context: &context,
                },
                &metadata_record,
                &mut mismatched_batch,
            )
            .is_err());
        let mut batch = FactBatch::new(4, 4).unwrap();
        let disposition = adapter
            .decode(
                DecodeContext {
                    decoder: &DecoderId::new(SUBAGENT_META_DECODER).unwrap(),
                    object_context: &context,
                },
                &metadata_record,
                &mut batch,
            )
            .unwrap();
        assert_eq!(disposition, DecodeDisposition::Applied);
        let metadata = fact_values(&batch)
            .find_map(|fact| match fact {
                Fact::DelegationMetadata(metadata) => Some(metadata),
                _ => None,
            })
            .unwrap();
        assert_eq!(metadata.native_child_id, "a1");
        assert_eq!(metadata.agent_type, "Explore");
        assert_eq!(metadata.description.as_deref(), Some("map the parser"));
        assert_eq!(metadata.name.as_deref(), Some("survey"));
        assert_eq!(metadata.spawn_depth, Some(2));
        assert_eq!(
            metadata.worktree_path.as_deref(),
            Some("/repo/.worktrees/a1")
        );
        assert_eq!(metadata.native_task_id.as_deref(), Some("tool-7"));
        assert!(!fact_values(&batch).any(|fact| matches!(fact, Fact::Delegation(_))));

        let malformed = record(b"{}");
        let mut unknown_batch = FactBatch::new(4, 4).unwrap();
        let disposition = adapter
            .decode(
                DecodeContext {
                    decoder: &DecoderId::new(SUBAGENT_META_DECODER).unwrap(),
                    object_context: &context,
                },
                &malformed,
                &mut unknown_batch,
            )
            .unwrap();
        assert_eq!(disposition, DecodeDisposition::PreservedUnknown);
        assert!(matches!(
            unknown_batch.facts()[0].value,
            Fact::UnknownRecord { .. }
        ));
        assert_eq!(unknown_batch.diagnostics().len(), 1);
    }
}
