//! RFC 011 Claude Code adapter declaration and native record decoders.
//!
//! Filesystem framing, checkpoints, generations, scheduling, and retries stay
//! in the common source layer. This module owns Claude path and JSON meaning.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::adapter::{
    AdapterDiagnostic, AdapterError, AdapterErrorClass, AdapterId, AdapterManifest,
    AdapterObjectContext, AgentAdapter, Availability, CapabilityDeclaration, CapabilityGranularity,
    CapabilityId, CapabilitySupport, ConsistencyPolicy, ContentBlock, DecodeContext,
    DecodeDisposition, DecoderId, DelegationFact, DelegationKind, DelegationMetadataFact,
    DelegationSpawnFact, DeletionPolicy, DiscoveryContext, DriverSpec, EntityKey, EntityScope,
    EvidenceKind, EvidenceStrength, Fact, FactBatch, MessageFact, MessageRole, ObjectSelector,
    QualifiedTimestamp, RawRetentionPolicy, RelationStrength, RunEvidenceFact, RunFact,
    SessionFact, SourceInstance, SourceInstanceKey, SourceInstanceSpec, SourceObjectDescriptor,
    SourceRoot, StreamAuthority, StreamId, StreamSpec, SupportLevel, TeamInboxMessageSnapshot,
    TeamInboxSnapshotFact, TeamMemberSnapshot, TeamSnapshotFact, TimestampQuality, TokenUsage,
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
const TEAM_CONFIG_STREAM: &str = "team-configs";
const TEAM_INBOX_STREAM: &str = "team-inboxes";
const PARENT_DECODER: &str = "claude-session-record";
const SUBAGENT_DECODER: &str = "claude-subagent-record";
const SUBAGENT_META_DECODER: &str = "claude-subagent-metadata";
const TEAM_CONFIG_DECODER: &str = "claude-team-config";
const TEAM_INBOX_DECODER: &str = "claude-team-inbox";
const OBJECT_CONTEXT_VERSION: u32 = 1;
const SUBAGENT_META_MAX_BYTES: usize = 64 * 1024;
const TEAM_CONFIG_MAX_BYTES: usize = 1024 * 1024;
const TEAM_INBOX_MAX_BYTES: usize = 4 * 1024 * 1024;
const TEAM_MEMBER_LIMIT: usize = 256;
const TEAM_INBOX_MESSAGE_LIMIT: usize = 4_096;

const HISTORY_SESSIONS: &str = "history.sessions";
const HISTORY_MESSAGES: &str = "history.messages";
const HISTORY_CONTENT_BLOCKS: &str = "history.content_blocks";
const HISTORY_TIMESTAMPS: &str = "history.timestamps";
const HISTORY_MODEL_IDENTITY: &str = "history.model_identity";
const RUNTIME_SESSION_ACTIVITY: &str = "runtime.session_activity";
const RUNTIME_SUBAGENTS: &str = "runtime.subagents";
const RUNTIME_TEAMS: &str = "runtime.teams";
const RUNTIME_TEAM_INBOX: &str = "runtime.team_inbox";
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
                contract_version: 5,
                source_schema_versions: vec![
                    "claude-code-jsonl-v1".to_string(),
                    "claude-code-subagent-meta-v1".to_string(),
                    "claude-code-team-config-v1".to_string(),
                    "claude-code-team-inbox-v2".to_string(),
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
                    SourceRoot {
                        name: "teams".to_string(),
                        path: canonical.join("teams"),
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
                capabilities: transcript_capabilities(),
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
                capabilities: transcript_capabilities(),
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
            StreamSpec {
                id: StreamId::new(TEAM_CONFIG_STREAM)?,
                driver: DriverSpec::ReplaceDocument(ReplaceDocumentConfig {
                    max_document_bytes: TEAM_CONFIG_MAX_BYTES,
                }),
                selector: ObjectSelector {
                    root_name: "teams".to_string(),
                    include: vec!["*/config.json".to_string()],
                    exclude: Vec::new(),
                },
                decoder: DecoderId::new(TEAM_CONFIG_DECODER)?,
                authority: StreamAuthority::Canonical,
                entity_scope: EntityScope::Custom("team".to_string()),
                priority: IngestPriority::ForegroundRepair,
                consistency: ConsistencyPolicy::SnapshotReplace,
                deletion: DeletionPolicy::MirrorSource,
                retention: RawRetentionPolicy::Full,
                capabilities: team_config_capabilities(),
            },
            StreamSpec {
                id: StreamId::new(TEAM_INBOX_STREAM)?,
                driver: DriverSpec::ReplaceDocument(ReplaceDocumentConfig {
                    max_document_bytes: TEAM_INBOX_MAX_BYTES,
                }),
                selector: ObjectSelector {
                    root_name: "teams".to_string(),
                    include: vec!["*/inboxes/*.json".to_string()],
                    exclude: Vec::new(),
                },
                decoder: DecoderId::new(TEAM_INBOX_DECODER)?,
                authority: StreamAuthority::Canonical,
                entity_scope: EntityScope::Custom("team_inbox".to_string()),
                priority: IngestPriority::ForegroundRepair,
                consistency: ConsistencyPolicy::SnapshotReplace,
                deletion: DeletionPolicy::MirrorSource,
                retention: RawRetentionPolicy::Full,
                capabilities: team_inbox_capabilities(),
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
            TEAM_CONFIG_STREAM => {
                encode_object_context(&ClaudeTeamConfigContext::from_path(&object.relative_path)?)?
            }
            TEAM_INBOX_STREAM => {
                encode_object_context(&ClaudeTeamInboxContext::from_path(&object.relative_path)?)?
            }
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
            TEAM_CONFIG_DECODER => {
                let object_context = ClaudeTeamConfigContext::decode(context.object_context)?;
                decode_team_config(self.adapter_id(), &object_context, record, output)
            }
            TEAM_INBOX_DECODER => {
                let object_context = ClaudeTeamInboxContext::decode(context.object_context)?;
                decode_team_inbox(self.adapter_id(), &object_context, record, output)
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
                "child identity is native; layout lineage remains durable and matching native spawn/metadata tool-use IDs strengthen it explicitly; silence never implies completion",
            ),
        ),
        live_native(RUNTIME_TEAMS, CapabilityGranularity::Team),
        live_native(RUNTIME_TEAM_INBOX, CapabilityGranularity::Message),
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

fn transcript_capabilities() -> Vec<CapabilityId> {
    [
        HISTORY_SESSIONS,
        HISTORY_MESSAGES,
        HISTORY_CONTENT_BLOCKS,
        HISTORY_TIMESTAMPS,
        HISTORY_MODEL_IDENTITY,
        RUNTIME_SESSION_ACTIVITY,
        USAGE_INPUT_TOKENS,
        USAGE_OUTPUT_TOKENS,
        USAGE_CACHE_TOKENS,
        RUNTIME_SUBAGENTS,
        SOURCE_LIVE,
        SOURCE_RECONCILE,
        SOURCE_RESUME_CURSOR,
    ]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ClaudeTeamConfigContext {
    native_team_id: String,
}

impl ClaudeTeamConfigContext {
    fn from_path(relative_path: &Path) -> Result<Self, AdapterError> {
        let components = utf8_components(relative_path)?;
        if components.len() != 2 || components[1] != "config.json" || components[0].is_empty() {
            return Err(path_error(
                relative_path,
                "team config path must be <team>/config.json",
            ));
        }
        Ok(Self {
            native_team_id: components[0].clone(),
        })
    }

    fn decode(context: &AdapterObjectContext) -> Result<Self, AdapterError> {
        decode_object_context(context)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ClaudeTeamInboxContext {
    native_team_id: String,
    native_recipient_name: String,
}

impl ClaudeTeamInboxContext {
    fn from_path(relative_path: &Path) -> Result<Self, AdapterError> {
        let components = utf8_components(relative_path)?;
        if components.len() != 3 || components[0].is_empty() || components[1] != "inboxes" {
            return Err(path_error(
                relative_path,
                "team inbox path must be <team>/inboxes/<recipient>.json",
            ));
        }
        let Some(native_recipient_name) = components[2]
            .strip_suffix(".json")
            .filter(|name| !name.is_empty())
        else {
            return Err(path_error(relative_path, "invalid team inbox file name"));
        };
        Ok(Self {
            native_team_id: components[0].clone(),
            native_recipient_name: native_recipient_name.to_string(),
        })
    }

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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeTeamConfigDocument {
    name: String,
    #[serde(default)]
    description: Option<String>,
    created_at: i64,
    lead_agent_id: String,
    lead_session_id: String,
    members: Vec<ClaudeTeamMemberDocument>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeTeamMemberDocument {
    agent_id: String,
    name: String,
    #[serde(default)]
    agent_type: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    color: Option<String>,
    #[serde(default)]
    plan_mode_required: Option<bool>,
    joined_at: i64,
    tmux_pane_id: String,
    cwd: String,
    subscriptions: Vec<String>,
    #[serde(default)]
    backend_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ClaudeTeamInboxMessageDocument {
    from: String,
    text: String,
    #[serde(default)]
    summary: Option<String>,
    timestamp: String,
    #[serde(default)]
    color: Option<String>,
    read: bool,
    #[serde(default, rename = "msg_id")]
    message_id: Option<String>,
    #[serde(default, rename = "msgV")]
    message_version: Option<u32>,
    #[serde(default, rename = "type")]
    kind: Option<String>,
}

fn decode_team_config(
    adapter_id: &AdapterId,
    context: &ClaudeTeamConfigContext,
    record: &SourceRecord,
    output: &mut FactBatch,
) -> Result<DecodeDisposition, AdapterError> {
    let document: ClaudeTeamConfigDocument = match serde_json::from_slice(&record.payload) {
        Ok(document) => document,
        Err(error) => {
            preserve_unknown(
                record,
                output,
                Some("team_config".to_string()),
                format!("Claude team config is not a supported JSON object: {error}"),
            )?;
            return Ok(DecodeDisposition::PreservedUnknown);
        }
    };
    if document.members.len() > TEAM_MEMBER_LIMIT {
        preserve_unknown(
            record,
            output,
            Some("team_config".to_string()),
            format!("Claude team config exceeds the {TEAM_MEMBER_LIMIT} member bound"),
        )?;
        return Ok(DecodeDisposition::PreservedUnknown);
    }
    let Some(name) = nonempty(&document.name) else {
        return preserve_team_config_contract_loss(record, output, "team name is empty");
    };
    let Some(native_lead_agent_id) = nonempty(&document.lead_agent_id) else {
        return preserve_team_config_contract_loss(record, output, "lead agent id is empty");
    };
    let Some(native_lead_session_id) = nonempty(&document.lead_session_id) else {
        return preserve_team_config_contract_loss(record, output, "lead session id is empty");
    };
    let team = EntityKey::native(
        adapter_id,
        record.source_instance_id,
        "team",
        context.native_team_id.as_bytes(),
    )?;
    let mut member_names = BTreeSet::new();
    let mut members = Vec::with_capacity(document.members.len());
    for member in document.members {
        let Some(native_name) = nonempty(&member.name) else {
            return preserve_team_config_contract_loss(record, output, "member name is empty");
        };
        let Some(native_agent_id) = nonempty(&member.agent_id) else {
            return preserve_team_config_contract_loss(record, output, "member agent id is empty");
        };
        if !member_names.insert(native_name.clone()) {
            return preserve_team_config_contract_loss(
                record,
                output,
                "member names are not unique",
            );
        }
        members.push(TeamMemberSnapshot {
            member: team_member_key(
                adapter_id,
                record.source_instance_id,
                &context.native_team_id,
                &native_name,
            )?,
            native_agent_id,
            native_name,
            agent_type: member.agent_type.as_deref().and_then(nonempty),
            model: member.model.as_deref().and_then(nonempty),
            prompt: member.prompt.as_deref().and_then(nonempty),
            color: member.color.as_deref().and_then(nonempty),
            plan_mode_required: member.plan_mode_required,
            joined_at: epoch_millis_timestamp(member.joined_at),
            tmux_pane_id: member.tmux_pane_id,
            cwd: member.cwd,
            subscriptions: member.subscriptions,
            backend_type: member.backend_type.as_deref().and_then(nonempty),
        });
    }
    let lead_member = members
        .iter()
        .find(|member| member.native_agent_id == native_lead_agent_id)
        .map(|member| member.member.clone());
    output.push(
        record,
        Fact::TeamSnapshot(TeamSnapshotFact {
            team,
            native_team_id: context.native_team_id.clone(),
            name,
            description: document.description.as_deref().and_then(nonempty),
            created_at: epoch_millis_timestamp(document.created_at),
            lead_member,
            native_lead_agent_id,
            lead_session: EntityKey::native(
                adapter_id,
                record.source_instance_id,
                "session",
                native_lead_session_id.as_bytes(),
            )?,
            native_lead_session_id,
            members,
        }),
    )?;
    Ok(DecodeDisposition::Applied)
}

fn preserve_team_config_contract_loss(
    record: &SourceRecord,
    output: &mut FactBatch,
    detail: &str,
) -> Result<DecodeDisposition, AdapterError> {
    preserve_unknown(
        record,
        output,
        Some("team_config".to_string()),
        format!("Claude team config {detail}"),
    )?;
    Ok(DecodeDisposition::PreservedUnknown)
}

fn decode_team_inbox(
    adapter_id: &AdapterId,
    context: &ClaudeTeamInboxContext,
    record: &SourceRecord,
    output: &mut FactBatch,
) -> Result<DecodeDisposition, AdapterError> {
    let documents: Vec<ClaudeTeamInboxMessageDocument> =
        match serde_json::from_slice(&record.payload) {
            Ok(documents) => documents,
            Err(error) => {
                preserve_unknown(
                    record,
                    output,
                    Some("team_inbox".to_string()),
                    format!("Claude team inbox is not a supported JSON array: {error}"),
                )?;
                return Ok(DecodeDisposition::PreservedUnknown);
            }
        };
    if documents.len() > TEAM_INBOX_MESSAGE_LIMIT {
        preserve_unknown(
            record,
            output,
            Some("team_inbox".to_string()),
            format!("Claude team inbox exceeds the {TEAM_INBOX_MESSAGE_LIMIT} message bound"),
        )?;
        return Ok(DecodeDisposition::PreservedUnknown);
    }
    let team = EntityKey::native(
        adapter_id,
        record.source_instance_id,
        "team",
        context.native_team_id.as_bytes(),
    )?;
    let recipient = team_member_key(
        adapter_id,
        record.source_instance_id,
        &context.native_team_id,
        &context.native_recipient_name,
    )?;
    let mut native_ids = BTreeSet::new();
    let mut legacy_occurrences = BTreeMap::<[u8; 32], u32>::new();
    let mut messages = Vec::with_capacity(documents.len());
    for document in documents {
        let Some(native_sender_name) = nonempty(&document.from) else {
            return preserve_team_inbox_contract_loss(record, output, "sender is empty");
        };
        let Some(timestamp) = nonempty(&document.timestamp) else {
            return preserve_team_inbox_contract_loss(record, output, "timestamp is empty");
        };
        let native_message_id = document.message_id.as_deref().and_then(nonempty);
        let mut native_message_key = Vec::new();
        push_key_component(&mut native_message_key, context.native_team_id.as_bytes());
        push_key_component(
            &mut native_message_key,
            context.native_recipient_name.as_bytes(),
        );
        if let Some(message_id) = &native_message_id {
            if !native_ids.insert(message_id.clone()) {
                return preserve_team_inbox_contract_loss(
                    record,
                    output,
                    "contains duplicate native message ids",
                );
            }
            push_key_component(&mut native_message_key, b"native-id");
            push_key_component(&mut native_message_key, message_id.as_bytes());
        } else {
            let mut hasher = blake3::Hasher::new();
            hash_component(&mut hasher, native_sender_name.as_bytes());
            hash_component(&mut hasher, timestamp.as_bytes());
            hash_component(&mut hasher, document.text.as_bytes());
            let digest = *hasher.finalize().as_bytes();
            let occurrence = legacy_occurrences.entry(digest).or_default();
            push_key_component(&mut native_message_key, b"legacy-fingerprint");
            push_key_component(&mut native_message_key, &digest);
            push_key_component(&mut native_message_key, &occurrence.to_be_bytes());
            *occurrence = occurrence.saturating_add(1);
        }
        messages.push(TeamInboxMessageSnapshot {
            message: EntityKey::native(
                adapter_id,
                record.source_instance_id,
                "team_inbox_message",
                &native_message_key,
            )?,
            sender: team_member_key(
                adapter_id,
                record.source_instance_id,
                &context.native_team_id,
                &native_sender_name,
            )?,
            native_message_id,
            native_kind: document.kind.as_deref().and_then(nonempty),
            native_version: document.message_version,
            native_sender_name,
            text: document.text,
            summary: document.summary.as_deref().and_then(nonempty),
            color: document.color.as_deref().and_then(nonempty),
            source_time: native_timestamp(&timestamp),
            read: document.read,
        });
    }
    let mut native_inbox_key = Vec::new();
    push_key_component(&mut native_inbox_key, context.native_team_id.as_bytes());
    push_key_component(
        &mut native_inbox_key,
        context.native_recipient_name.as_bytes(),
    );
    output.push(
        record,
        Fact::TeamInboxSnapshot(TeamInboxSnapshotFact {
            inbox: EntityKey::native(
                adapter_id,
                record.source_instance_id,
                "team_inbox",
                &native_inbox_key,
            )?,
            team,
            recipient,
            native_team_id: context.native_team_id.clone(),
            native_recipient_name: context.native_recipient_name.clone(),
            messages,
        }),
    )?;
    Ok(DecodeDisposition::Applied)
}

fn preserve_team_inbox_contract_loss(
    record: &SourceRecord,
    output: &mut FactBatch,
    detail: &str,
) -> Result<DecodeDisposition, AdapterError> {
    preserve_unknown(
        record,
        output,
        Some("team_inbox".to_string()),
        format!("Claude team inbox {detail}"),
    )?;
    Ok(DecodeDisposition::PreservedUnknown)
}

fn team_member_key(
    adapter_id: &AdapterId,
    source_instance_id: u64,
    native_team_id: &str,
    native_member_name: &str,
) -> Result<EntityKey, AdapterError> {
    let mut native_key = Vec::new();
    push_key_component(&mut native_key, native_team_id.as_bytes());
    push_key_component(&mut native_key, native_member_name.as_bytes());
    EntityKey::native(adapter_id, source_instance_id, "team_member", &native_key)
}

fn epoch_millis_timestamp(value: i64) -> QualifiedTimestamp {
    QualifiedTimestamp {
        value: crate::core::timefmt::epoch_ms_to_iso8601(value as f64),
        quality: TimestampQuality::NativeExact,
    }
}

fn hash_component(hasher: &mut blake3::Hasher, value: &[u8]) {
    hasher.update(&(value.len() as u64).to_be_bytes());
    hasher.update(value);
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
            native_run_id: run_native_key.clone(),
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
    let spawn_descriptors = delegation_spawn_descriptors(&content);
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
            run: run.clone(),
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
                subject: message.clone(),
                session: session.clone(),
                scope: UsageScope::Message,
                accounting: UsageAccounting::Delta,
                quality: ValueQuality::NativeExact,
                values: usage,
                model,
                source_time: source_time.clone(),
            }),
        )?;
    }
    for descriptor in spawn_descriptors {
        let mut native_spawn_key = Vec::new();
        push_key_component(&mut native_spawn_key, run_native_key.as_bytes());
        push_key_component(&mut native_spawn_key, descriptor.native_task_id.as_bytes());
        output.push(
            record,
            Fact::DelegationSpawn(DelegationSpawnFact {
                spawn: EntityKey::native(
                    adapter_id,
                    record.source_instance_id,
                    "delegation_spawn",
                    &native_spawn_key,
                )?,
                parent_run: run.clone(),
                parent_message: message.clone(),
                session: session.clone(),
                native_task_id: descriptor.native_task_id,
                tool_name: descriptor.tool_name,
                label: descriptor.label,
                prompt: descriptor.prompt,
                requested_agent_type: descriptor.requested_agent_type,
                source_time: source_time.clone(),
            }),
        )?;
    }
    Ok(DecodeDisposition::Applied)
}

struct DelegationSpawnDescriptor {
    native_task_id: String,
    tool_name: String,
    label: Option<String>,
    prompt: Option<String>,
    requested_agent_type: Option<String>,
}

fn delegation_spawn_descriptors(content: &[ContentBlock]) -> Vec<DelegationSpawnDescriptor> {
    content
        .iter()
        .filter_map(|block| {
            let ContentBlock::ToolCall {
                native_id,
                name,
                input,
            } = block
            else {
                return None;
            };
            if !matches!(name.as_str(), "Task" | "Agent") || native_id.trim().is_empty() {
                return None;
            }
            Some(DelegationSpawnDescriptor {
                native_task_id: native_id.clone(),
                tool_name: name.clone(),
                label: nonempty_field(input, "description")
                    .or_else(|| nonempty_field(input, "name")),
                prompt: nonempty_field(input, "prompt"),
                requested_agent_type: nonempty_field(input, "subagent_type")
                    .or_else(|| nonempty_field(input, "agent_type")),
            })
        })
        .collect()
}

fn team_config_capabilities() -> Vec<CapabilityId> {
    [
        RUNTIME_TEAMS,
        SOURCE_LIVE,
        SOURCE_RECONCILE,
        SOURCE_RESUME_CURSOR,
    ]
    .into_iter()
    .map(|id| CapabilityId::new(id).expect("static Claude stream capability id is valid"))
    .collect()
}

fn team_inbox_capabilities() -> Vec<CapabilityId> {
    [
        RUNTIME_TEAMS,
        RUNTIME_TEAM_INBOX,
        SOURCE_LIVE,
        SOURCE_RECONCILE,
        SOURCE_RESUME_CURSOR,
    ]
    .into_iter()
    .map(|id| CapabilityId::new(id).expect("static Claude stream capability id is valid"))
    .collect()
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
        assert_eq!(discovered[0].roots.len(), 3);
        assert_eq!(discovered[0].roots[2].name, "teams");
        assert_eq!(
            discovered[0].roots[2].path,
            std::fs::canonicalize(root.path()).unwrap().join("teams")
        );
        assert_eq!(streams.len(), 5);
        assert_eq!(adapter.manifest().contract_version, 5);
        assert!(adapter
            .manifest()
            .source_schema_versions
            .iter()
            .any(|version| version == "claude-code-subagent-meta-v1"));
        assert!(adapter
            .manifest()
            .source_schema_versions
            .iter()
            .any(|version| version == "claude-code-team-config-v1"));
        assert!(adapter
            .manifest()
            .source_schema_versions
            .iter()
            .any(|version| version == "claude-code-team-inbox-v2"));
        assert!(matches!(streams[0].driver, DriverSpec::AppendDelimited(_)));
        assert!(matches!(streams[1].driver, DriverSpec::AppendDelimited(_)));
        assert!(matches!(streams[2].driver, DriverSpec::ReplaceDocument(_)));
        assert!(matches!(
            streams[3].driver,
            DriverSpec::ReplaceDocument(ReplaceDocumentConfig {
                max_document_bytes: TEAM_CONFIG_MAX_BYTES
            })
        ));
        assert!(matches!(
            streams[4].driver,
            DriverSpec::ReplaceDocument(ReplaceDocumentConfig {
                max_document_bytes: TEAM_INBOX_MAX_BYTES
            })
        ));
        assert_eq!(streams[0].decoder.as_str(), PARENT_DECODER);
        assert_eq!(streams[1].decoder.as_str(), SUBAGENT_DECODER);
        assert_eq!(streams[2].decoder.as_str(), SUBAGENT_META_DECODER);
        assert_eq!(streams[3].decoder.as_str(), TEAM_CONFIG_DECODER);
        assert_eq!(streams[4].decoder.as_str(), TEAM_INBOX_DECODER);
        assert_eq!(streams[2].authority, StreamAuthority::Supplemental);
        assert_eq!(streams[2].consistency, ConsistencyPolicy::SnapshotReplace);
        assert_eq!(streams[3].authority, StreamAuthority::Canonical);
        assert_eq!(streams[3].consistency, ConsistencyPolicy::SnapshotReplace);
        assert_eq!(streams[4].authority, StreamAuthority::Canonical);
        assert_eq!(streams[4].consistency, ConsistencyPolicy::SnapshotReplace);
        assert!(streams[0]
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
        assert!(streams[3]
            .capabilities
            .iter()
            .any(|capability| capability.as_str() == RUNTIME_TEAMS));
        assert!(streams[4]
            .capabilities
            .iter()
            .any(|capability| capability.as_str() == RUNTIME_TEAM_INBOX));
        let support = adapter
            .manifest()
            .capabilities
            .iter()
            .find(|capability| capability.id.as_str() == RUNTIME_SUBAGENTS)
            .unwrap();
        assert_eq!(support.support.level, SupportLevel::Derived);
        assert_eq!(support.support.granularity, CapabilityGranularity::Run);
        assert_eq!(support.support.availability, Availability::Live);
        let teams = adapter
            .manifest()
            .capabilities
            .iter()
            .find(|capability| capability.id.as_str() == RUNTIME_TEAMS)
            .unwrap();
        assert_eq!(teams.support.level, SupportLevel::Native);
        assert_eq!(teams.support.granularity, CapabilityGranularity::Team);
        let inbox = adapter
            .manifest()
            .capabilities
            .iter()
            .find(|capability| capability.id.as_str() == RUNTIME_TEAM_INBOX)
            .unwrap();
        assert_eq!(inbox.support.level, SupportLevel::Native);
        assert_eq!(inbox.support.granularity, CapabilityGranularity::Message);
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
    fn team_config_snapshot_preserves_native_membership_without_activity_inference() {
        let root = TempDir::new().unwrap();
        let adapter = ClaudeCodeAdapter::new();
        let object_context = adapter
            .bootstrap_object(
                &instance(root.path()),
                &object(TEAM_CONFIG_STREAM, "alpha/config.json"),
            )
            .unwrap();
        let record = record(
            br#"{
              "name":"alpha",
              "description":"ship the engine",
              "createdAt":1786406400000,
              "leadAgentId":"lead@alpha",
              "leadSessionId":"01234567-89ab-cdef-0123-456789abcdef",
              "members":[{
                "agentId":"lead@alpha",
                "name":"team-lead",
                "agentType":"general-purpose",
                "model":"claude-opus",
                "prompt":"coordinate",
                "color":"blue",
                "planModeRequired":true,
                "joinedAt":1786406400001,
                "tmuxPaneId":"%1",
                "cwd":"/repo",
                "subscriptions":["changes"],
                "backendType":"tmux"
              }]
            }"#,
        );
        let mut batch = FactBatch::new(8, 4).unwrap();
        let disposition = adapter
            .decode(
                DecodeContext {
                    decoder: &DecoderId::new(TEAM_CONFIG_DECODER).unwrap(),
                    object_context: &object_context,
                },
                &record,
                &mut batch,
            )
            .unwrap();

        assert_eq!(disposition, DecodeDisposition::Applied);
        assert_eq!(batch.facts().len(), 1);
        let Fact::TeamSnapshot(snapshot) = &batch.facts()[0].value else {
            panic!("expected team snapshot");
        };
        assert_eq!(snapshot.native_team_id, "alpha");
        assert_eq!(snapshot.name, "alpha");
        assert_eq!(snapshot.native_lead_agent_id, "lead@alpha");
        assert_eq!(snapshot.native_lead_session_id, SESSION);
        assert_eq!(
            snapshot.lead_member.as_ref(),
            Some(&snapshot.members[0].member)
        );
        assert_eq!(snapshot.members.len(), 1);
        assert_eq!(snapshot.members[0].native_name, "team-lead");
        assert_eq!(snapshot.members[0].subscriptions, ["changes"]);
        assert_eq!(snapshot.members[0].backend_type.as_deref(), Some("tmux"));
        assert!(fact_values(&batch).all(|fact| !matches!(fact, Fact::RunEvidence(_))));
    }

    #[test]
    fn team_inbox_snapshot_uses_native_ids_and_keeps_legacy_identity_across_read_edits() {
        let root = TempDir::new().unwrap();
        let adapter = ClaudeCodeAdapter::new();
        let object_context = adapter
            .bootstrap_object(
                &instance(root.path()),
                &object(TEAM_INBOX_STREAM, "alpha/inboxes/team-lead.json"),
            )
            .unwrap();
        let decode = |payload: &[u8]| {
            let record = record(payload);
            let mut batch = FactBatch::new(8, 4).unwrap();
            let disposition = adapter
                .decode(
                    DecodeContext {
                        decoder: &DecoderId::new(TEAM_INBOX_DECODER).unwrap(),
                        object_context: &object_context,
                    },
                    &record,
                    &mut batch,
                )
                .unwrap();
            assert_eq!(disposition, DecodeDisposition::Applied);
            let Fact::TeamInboxSnapshot(snapshot) = &batch.facts()[0].value else {
                panic!("expected team inbox snapshot");
            };
            snapshot.clone()
        };
        let first = decode(
            br#"[
              {"from":"worker","text":"native","timestamp":"2026-08-11T00:00:00Z","read":false,"msg_id":"msg-1","msgV":1,"type":"message"},
              {"from":"worker","text":"legacy","summary":"status","timestamp":"2026-08-11T00:00:01Z","read":false}
            ]"#,
        );
        let updated = decode(
            br#"[
              {"from":"worker","text":"native","timestamp":"2026-08-11T00:00:00Z","read":true,"msg_id":"msg-1","msgV":1,"type":"message"},
              {"from":"worker","text":"legacy","summary":"status","timestamp":"2026-08-11T00:00:01Z","read":true}
            ]"#,
        );

        assert_eq!(first.native_team_id, "alpha");
        assert_eq!(first.native_recipient_name, "team-lead");
        assert_eq!(first.messages.len(), 2);
        assert_eq!(
            first.messages[0].native_message_id.as_deref(),
            Some("msg-1")
        );
        assert_eq!(first.messages[0].native_version, Some(1));
        assert_eq!(first.messages[0].native_kind.as_deref(), Some("message"));
        assert_eq!(first.messages[0].message, updated.messages[0].message);
        assert_eq!(first.messages[1].message, updated.messages[1].message);
        assert!(!first.messages[1].read);
        assert!(updated.messages[1].read);
    }

    #[test]
    fn unsupported_team_documents_are_preserved_for_future_decoders() {
        let root = TempDir::new().unwrap();
        let adapter = ClaudeCodeAdapter::new();
        let object_context = adapter
            .bootstrap_object(
                &instance(root.path()),
                &object(TEAM_INBOX_STREAM, "alpha/inboxes/team-lead.json"),
            )
            .unwrap();
        let record = record(br#"{"messages":[]}"#);
        let mut batch = FactBatch::new(8, 4).unwrap();
        let disposition = adapter
            .decode(
                DecodeContext {
                    decoder: &DecoderId::new(TEAM_INBOX_DECODER).unwrap(),
                    object_context: &object_context,
                },
                &record,
                &mut batch,
            )
            .unwrap();

        assert_eq!(disposition, DecodeDisposition::PreservedUnknown);
        assert!(matches!(batch.facts()[0].value, Fact::UnknownRecord { .. }));
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
    fn native_task_tool_call_emits_a_parent_scoped_spawn_fact() {
        let root = TempDir::new().unwrap();
        let adapter = ClaudeCodeAdapter::new();
        let object_context = adapter
            .bootstrap_object(
                &instance(root.path()),
                &object(PARENT_STREAM, &format!("project/{SESSION}.jsonl")),
            )
            .unwrap();
        let task_record = record(
            format!(
                r#"{{"type":"assistant","uuid":"spawn-message","parentUuid":"u1","timestamp":"2026-08-11T00:00:00Z","sessionId":"{SESSION}","cwd":"/repo","version":"1","gitBranch":"main","isSidechain":false,"userType":"external","requestId":"r1","message":{{"model":"claude-sonnet","id":"api1","type":"message","role":"assistant","content":[{{"type":"tool_use","id":"tool-spawn-1","name":"Task","input":{{"description":"Map the parser","prompt":"Inspect the implementation","subagent_type":"Explore"}}}}]}}}}"#
            )
            .as_bytes(),
        );
        let mut batch = FactBatch::new(8, 4).unwrap();
        adapter
            .decode(
                DecodeContext {
                    decoder: &DecoderId::new(PARENT_DECODER).unwrap(),
                    object_context: &object_context,
                },
                &task_record,
                &mut batch,
            )
            .unwrap();
        let spawn = fact_values(&batch)
            .find_map(|fact| match fact {
                Fact::DelegationSpawn(spawn) => Some(spawn),
                _ => None,
            })
            .unwrap();
        assert_eq!(spawn.native_task_id, "tool-spawn-1");
        assert_eq!(spawn.tool_name, "Task");
        assert_eq!(spawn.label.as_deref(), Some("Map the parser"));
        assert_eq!(spawn.prompt.as_deref(), Some("Inspect the implementation"));
        assert_eq!(spawn.requested_agent_type.as_deref(), Some("Explore"));
        let parent_run = fact_values(&batch)
            .find_map(|fact| match fact {
                Fact::Run(run) => Some(&run.run),
                _ => None,
            })
            .unwrap();
        assert_eq!(&spawn.parent_run, parent_run);
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
