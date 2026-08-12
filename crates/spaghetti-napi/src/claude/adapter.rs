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
    AdapterObjectContext, AgentAdapter, ArtifactCapture, ArtifactContentFact,
    ArtifactMetadataEntry, ArtifactMetadataSnapshotFact, ArtifactObservationKind, Availability,
    CapabilityDeclaration, CapabilityGranularity, CapabilityId, CapabilitySupport,
    ConsistencyPolicy, ContentBlock, DecodeContext, DecodeDisposition, DecoderId, DelegationFact,
    DelegationKind, DelegationMetadataFact, DelegationSpawnFact, DeletionPolicy, DiscoveryContext,
    DriverSpec, EntityKey, EntityScope, EvidenceKind, EvidenceStrength, Fact, FactBatch,
    MessageFact, MessageRole, ObjectSelector, PlanSnapshotFact, PresenceFact, QualifiedTimestamp,
    RawRetentionPolicy, RelationStrength, RunEvidenceFact, RunFact, SessionFact, SourceInstance,
    SourceInstanceKey, SourceInstanceSpec, SourceObjectDescriptor, SourceRoot, StreamAuthority,
    StreamId, StreamSpec, SupportLevel, TaskCollectionKind, TaskItemSnapshot, TaskSnapshotCoverage,
    TaskSnapshotFact, TaskStatus, TeamInboxMessageSnapshot, TeamInboxSnapshotFact,
    TeamMemberSnapshot, TeamSnapshotFact, TimestampQuality, TokenUsage, UsageAccounting, UsageFact,
    UsageScope, ValueQuality, WorkflowMemberEventFact, WorkflowMemberEventKind,
    WorkflowSnapshotFact, WorkflowStatus,
};
use crate::claude::message_extractor;
use crate::claude::session_metadata;
use crate::claude::types::content::{
    AssistantContentBlock, ToolResultContent, UserContentBlock, UserMessageContent,
};
use crate::claude::types::SessionMessage;
use crate::source::{
    platform_path_key, AppendDelimitedConfig, IngestPriority, PresenceObjectConfig,
    ReplaceDocumentConfig, SourceDriverError, SourceRecord, SourceRecordState,
};

const ADAPTER_ID: &str = "claude-code";
const PARENT_STREAM: &str = "session-transcripts";
const SUBAGENT_STREAM: &str = "subagent-transcripts";
const SUBAGENT_META_STREAM: &str = "subagent-metadata";
const TEAM_CONFIG_STREAM: &str = "team-configs";
const TEAM_INBOX_STREAM: &str = "team-inboxes";
const ACTIVE_SESSION_STREAM: &str = "active-sessions";
const TODO_STREAM: &str = "todo-snapshots";
const TASK_ITEM_STREAM: &str = "task-items";
const PLAN_STREAM: &str = "plan-documents";
const ARTIFACT_CONTENT_STREAM: &str = "file-history-blobs";
const WORKFLOW_RUN_STREAM: &str = "workflow-runs";
const WORKFLOW_JOURNAL_STREAM: &str = "workflow-journals";
const PARENT_DECODER: &str = "claude-session-record";
const SUBAGENT_DECODER: &str = "claude-subagent-record";
const SUBAGENT_META_DECODER: &str = "claude-subagent-metadata";
const TEAM_CONFIG_DECODER: &str = "claude-team-config";
const TEAM_INBOX_DECODER: &str = "claude-team-inbox";
const ACTIVE_SESSION_DECODER: &str = "claude-active-session";
const TODO_DECODER: &str = "claude-todo-snapshot";
const TASK_ITEM_DECODER: &str = "claude-task-item";
const PLAN_DECODER: &str = "claude-plan-document";
const ARTIFACT_CONTENT_DECODER: &str = "claude-file-history-blob";
const WORKFLOW_RUN_DECODER: &str = "claude-workflow-run";
const WORKFLOW_JOURNAL_DECODER: &str = "claude-workflow-journal";
const OBJECT_CONTEXT_VERSION: u32 = 1;
const SUBAGENT_META_MAX_BYTES: usize = 64 * 1024;
const TEAM_CONFIG_MAX_BYTES: usize = 1024 * 1024;
const TEAM_INBOX_MAX_BYTES: usize = 4 * 1024 * 1024;
const ACTIVE_SESSION_MAX_BYTES: usize = 64 * 1024;
const TODO_MAX_BYTES: usize = 1024 * 1024;
const TASK_ITEM_MAX_BYTES: usize = 256 * 1024;
const PLAN_MAX_BYTES: usize = 4 * 1024 * 1024;
const ARTIFACT_CONTENT_MAX_BYTES: usize = 1024 * 1024;
const WORKFLOW_RUN_MAX_BYTES: usize = 1024 * 1024;
const TEAM_MEMBER_LIMIT: usize = 256;
const TEAM_INBOX_MESSAGE_LIMIT: usize = 4_096;
const TODO_ITEM_LIMIT: usize = 4_096;
const ARTIFACT_METADATA_LIMIT: usize = 512;

const HISTORY_SESSIONS: &str = "history.sessions";
const HISTORY_MESSAGES: &str = "history.messages";
const HISTORY_CONTENT_BLOCKS: &str = "history.content_blocks";
const HISTORY_TIMESTAMPS: &str = "history.timestamps";
const HISTORY_MODEL_IDENTITY: &str = "history.model_identity";
const RUNTIME_SESSION_ACTIVITY: &str = "runtime.session_activity";
const RUNTIME_SUBAGENTS: &str = "runtime.subagents";
const RUNTIME_TEAMS: &str = "runtime.teams";
const RUNTIME_TEAM_INBOX: &str = "runtime.team_inbox";
const RUNTIME_PRESENCE: &str = "runtime.presence";
const RUNTIME_TASKS: &str = "runtime.tasks";
const RUNTIME_ARTIFACTS: &str = "runtime.artifacts";
const RUNTIME_WORKFLOWS: &str = "runtime.workflows";
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
                contract_version: 9,
                source_schema_versions: vec![
                    "claude-code-jsonl-v1".to_string(),
                    "claude-code-subagent-meta-v1".to_string(),
                    "claude-code-team-config-v1".to_string(),
                    "claude-code-team-inbox-v2".to_string(),
                    "claude-code-active-session-v1".to_string(),
                    "claude-code-todo-v1".to_string(),
                    "claude-code-task-item-v1".to_string(),
                    "claude-code-plan-v1".to_string(),
                    "claude-code-file-history-v1".to_string(),
                    "claude-code-workflow-v1".to_string(),
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
                    SourceRoot {
                        name: "sessions".to_string(),
                        path: canonical.join("sessions"),
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
            StreamSpec {
                id: StreamId::new(ACTIVE_SESSION_STREAM)?,
                driver: DriverSpec::Presence(PresenceObjectConfig {
                    include_content: true,
                    max_content_bytes: ACTIVE_SESSION_MAX_BYTES,
                }),
                selector: ObjectSelector {
                    root_name: "sessions".to_string(),
                    include: vec!["*.json".to_string()],
                    exclude: Vec::new(),
                },
                decoder: DecoderId::new(ACTIVE_SESSION_DECODER)?,
                authority: StreamAuthority::Canonical,
                entity_scope: EntityScope::Custom("process_presence".to_string()),
                priority: IngestPriority::Interactive,
                consistency: ConsistencyPolicy::SnapshotReplace,
                deletion: DeletionPolicy::MirrorSource,
                retention: RawRetentionPolicy::Full,
                capabilities: presence_capabilities(),
            },
            StreamSpec {
                id: StreamId::new(TODO_STREAM)?,
                driver: DriverSpec::ReplaceDocument(ReplaceDocumentConfig {
                    max_document_bytes: TODO_MAX_BYTES,
                }),
                selector: ObjectSelector {
                    root_name: "home".to_string(),
                    include: vec!["todos/*-agent-*.json".to_string()],
                    exclude: Vec::new(),
                },
                decoder: DecoderId::new(TODO_DECODER)?,
                authority: StreamAuthority::Canonical,
                entity_scope: EntityScope::Custom("task_collection".to_string()),
                priority: IngestPriority::Interactive,
                consistency: ConsistencyPolicy::SnapshotReplace,
                deletion: DeletionPolicy::MirrorSource,
                retention: RawRetentionPolicy::Full,
                capabilities: task_capabilities(),
            },
            StreamSpec {
                id: StreamId::new(TASK_ITEM_STREAM)?,
                driver: DriverSpec::ReplaceDocument(ReplaceDocumentConfig {
                    max_document_bytes: TASK_ITEM_MAX_BYTES,
                }),
                selector: ObjectSelector {
                    root_name: "home".to_string(),
                    include: vec!["tasks/*/*.json".to_string()],
                    exclude: Vec::new(),
                },
                decoder: DecoderId::new(TASK_ITEM_DECODER)?,
                authority: StreamAuthority::Canonical,
                entity_scope: EntityScope::Custom("task".to_string()),
                priority: IngestPriority::Interactive,
                consistency: ConsistencyPolicy::SnapshotReplace,
                deletion: DeletionPolicy::MirrorSource,
                retention: RawRetentionPolicy::Full,
                capabilities: task_capabilities(),
            },
            StreamSpec {
                id: StreamId::new(PLAN_STREAM)?,
                driver: DriverSpec::ReplaceDocument(ReplaceDocumentConfig {
                    max_document_bytes: PLAN_MAX_BYTES,
                }),
                selector: ObjectSelector {
                    root_name: "home".to_string(),
                    include: vec!["plans/*.md".to_string()],
                    exclude: Vec::new(),
                },
                decoder: DecoderId::new(PLAN_DECODER)?,
                authority: StreamAuthority::Canonical,
                entity_scope: EntityScope::Custom("plan".to_string()),
                priority: IngestPriority::Interactive,
                consistency: ConsistencyPolicy::SnapshotReplace,
                deletion: DeletionPolicy::MirrorSource,
                retention: RawRetentionPolicy::Full,
                capabilities: task_capabilities(),
            },
            StreamSpec {
                id: StreamId::new(ARTIFACT_CONTENT_STREAM)?,
                driver: DriverSpec::ReplaceDocument(ReplaceDocumentConfig {
                    max_document_bytes: ARTIFACT_CONTENT_MAX_BYTES,
                }),
                selector: ObjectSelector {
                    root_name: "home".to_string(),
                    include: vec!["file-history/*/*@v*".to_string()],
                    exclude: Vec::new(),
                },
                decoder: DecoderId::new(ARTIFACT_CONTENT_DECODER)?,
                authority: StreamAuthority::Canonical,
                entity_scope: EntityScope::Custom("artifact".to_string()),
                priority: IngestPriority::ForegroundRepair,
                consistency: ConsistencyPolicy::SnapshotReplace,
                deletion: DeletionPolicy::MirrorSource,
                retention: RawRetentionPolicy::Full,
                capabilities: artifact_capabilities(),
            },
            StreamSpec {
                id: StreamId::new(WORKFLOW_RUN_STREAM)?,
                driver: DriverSpec::ReplaceDocument(ReplaceDocumentConfig {
                    max_document_bytes: WORKFLOW_RUN_MAX_BYTES,
                }),
                selector: ObjectSelector {
                    root_name: "projects".to_string(),
                    include: vec!["*/*/workflows/wf_*.json".to_string()],
                    exclude: Vec::new(),
                },
                decoder: DecoderId::new(WORKFLOW_RUN_DECODER)?,
                authority: StreamAuthority::Canonical,
                entity_scope: EntityScope::Custom("workflow".to_string()),
                priority: IngestPriority::ForegroundRepair,
                consistency: ConsistencyPolicy::SnapshotReplace,
                deletion: DeletionPolicy::MirrorSource,
                retention: RawRetentionPolicy::Full,
                capabilities: workflow_capabilities(),
            },
            StreamSpec {
                id: StreamId::new(WORKFLOW_JOURNAL_STREAM)?,
                driver: DriverSpec::AppendDelimited(AppendDelimitedConfig::json_lines()),
                selector: ObjectSelector {
                    root_name: "projects".to_string(),
                    include: vec!["*/*/subagents/workflows/*/journal.jsonl".to_string()],
                    exclude: Vec::new(),
                },
                decoder: DecoderId::new(WORKFLOW_JOURNAL_DECODER)?,
                authority: StreamAuthority::Canonical,
                entity_scope: EntityScope::Custom("workflow_member".to_string()),
                priority: IngestPriority::Interactive,
                consistency: ConsistencyPolicy::IncrementalCursor,
                deletion: DeletionPolicy::MirrorSource,
                retention: RawRetentionPolicy::Full,
                capabilities: workflow_capabilities(),
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
            ACTIVE_SESSION_STREAM => encode_object_context(
                &ClaudeActiveSessionContext::from_path(&object.relative_path)?,
            )?,
            TODO_STREAM => {
                encode_object_context(&ClaudeTodoContext::from_path(&object.relative_path)?)?
            }
            TASK_ITEM_STREAM => {
                encode_object_context(&ClaudeTaskItemContext::from_path(&object.relative_path)?)?
            }
            PLAN_STREAM => {
                encode_object_context(&ClaudePlanContext::from_path(&object.relative_path)?)?
            }
            ARTIFACT_CONTENT_STREAM => encode_object_context(
                &ClaudeArtifactContentContext::from_path(&object.relative_path)?,
            )?,
            WORKFLOW_RUN_STREAM => encode_object_context(&ClaudeWorkflowContext::run_from_path(
                &object.relative_path,
            )?)?,
            WORKFLOW_JOURNAL_STREAM => encode_object_context(
                &ClaudeWorkflowContext::journal_from_path(&object.relative_path)?,
            )?,
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
            ACTIVE_SESSION_DECODER => {
                let object_context = ClaudeActiveSessionContext::decode(context.object_context)?;
                decode_active_session(self.adapter_id(), &object_context, record, output)
            }
            TODO_DECODER => {
                let object_context = ClaudeTodoContext::decode(context.object_context)?;
                decode_todo_snapshot(self.adapter_id(), &object_context, record, output)
            }
            TASK_ITEM_DECODER => {
                let object_context = ClaudeTaskItemContext::decode(context.object_context)?;
                decode_task_item(self.adapter_id(), &object_context, record, output)
            }
            PLAN_DECODER => {
                let object_context = ClaudePlanContext::decode(context.object_context)?;
                decode_plan_document(self.adapter_id(), &object_context, record, output)
            }
            ARTIFACT_CONTENT_DECODER => {
                let object_context = ClaudeArtifactContentContext::decode(context.object_context)?;
                decode_artifact_content(self.adapter_id(), &object_context, record, output)
            }
            WORKFLOW_RUN_DECODER => {
                let object_context = ClaudeWorkflowContext::decode(context.object_context)?;
                decode_workflow_run(self.adapter_id(), &object_context, record, output)
            }
            WORKFLOW_JOURNAL_DECODER => {
                let object_context = ClaudeWorkflowContext::decode(context.object_context)?;
                decode_workflow_journal(self.adapter_id(), &object_context, record, output)
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
        capability(
            RUNTIME_PRESENCE,
            SupportLevel::Native,
            CapabilityGranularity::Custom("process_presence".to_string()),
            Availability::Live,
            Some(
                "agent-owned registry presence is durable; host PID liveness and time-based freshness remain transient assessments",
            ),
        ),
        capability(
            RUNTIME_TASKS,
            SupportLevel::Native,
            CapabilityGranularity::Custom("task".to_string()),
            Availability::Live,
            Some(
                "todo files are complete snapshots, numbered task files are item documents, and task status never implies run completion",
            ),
        ),
        capability(
            RUNTIME_ARTIFACTS,
            SupportLevel::Native,
            CapabilityGranularity::Custom("artifact".to_string()),
            Availability::Live,
            Some(
                "file-history metadata and backup blobs are joined by native session and backup name; capture is session-attributed and never implies that a run produced the tracked file",
            ),
        ),
        capability(
            RUNTIME_WORKFLOWS,
            SupportLevel::Native,
            CapabilityGranularity::Custom("workflow".to_string()),
            Availability::EventuallyLive,
            Some(
                "workflow summaries and append journals preserve native workflow/member state; workflow terminal status never implies terminal child-run state",
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
        RUNTIME_ARTIFACTS,
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

fn presence_capabilities() -> Vec<CapabilityId> {
    [
        RUNTIME_PRESENCE,
        SOURCE_LIVE,
        SOURCE_RECONCILE,
        SOURCE_RESUME_CURSOR,
    ]
    .into_iter()
    .map(|id| CapabilityId::new(id).expect("static Claude stream capability id is valid"))
    .collect()
}

fn task_capabilities() -> Vec<CapabilityId> {
    [
        RUNTIME_TASKS,
        SOURCE_LIVE,
        SOURCE_RECONCILE,
        SOURCE_RESUME_CURSOR,
    ]
    .into_iter()
    .map(|id| CapabilityId::new(id).expect("static Claude stream capability id is valid"))
    .collect()
}

fn artifact_capabilities() -> Vec<CapabilityId> {
    [
        RUNTIME_ARTIFACTS,
        SOURCE_LIVE,
        SOURCE_RECONCILE,
        SOURCE_RESUME_CURSOR,
    ]
    .into_iter()
    .map(|id| CapabilityId::new(id).expect("static Claude stream capability id is valid"))
    .collect()
}

fn workflow_capabilities() -> Vec<CapabilityId> {
    [
        RUNTIME_WORKFLOWS,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ClaudeActiveSessionContext {
    native_pid: u32,
}

impl ClaudeActiveSessionContext {
    fn from_path(relative_path: &Path) -> Result<Self, AdapterError> {
        let components = utf8_components(relative_path)?;
        if components.len() != 1 {
            return Err(path_error(
                relative_path,
                "active session path must be <pid>.json",
            ));
        }
        let Some(stem) = components[0]
            .strip_suffix(".json")
            .filter(|stem| !stem.is_empty() && stem.bytes().all(|byte| byte.is_ascii_digit()))
        else {
            return Err(path_error(
                relative_path,
                "active session file name must contain a numeric pid",
            ));
        };
        let native_pid = stem
            .parse::<u32>()
            .ok()
            .filter(|pid| *pid > 0)
            .ok_or_else(|| path_error(relative_path, "active session pid is out of range"))?;
        Ok(Self { native_pid })
    }

    fn decode(context: &AdapterObjectContext) -> Result<Self, AdapterError> {
        decode_object_context(context)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ClaudeTodoContext {
    native_session_id: String,
    native_agent_id: String,
}

impl ClaudeTodoContext {
    fn from_path(relative_path: &Path) -> Result<Self, AdapterError> {
        let components = utf8_components(relative_path)?;
        if components.len() != 2 || components[0] != "todos" {
            return Err(path_error(
                relative_path,
                "todo path must be todos/<session>-agent-<agent>.json",
            ));
        }
        let Some(stem) = components[1].strip_suffix(".json") else {
            return Err(path_error(relative_path, "todo file must end in .json"));
        };
        let Some((native_session_id, native_agent_id)) = stem.rsplit_once("-agent-") else {
            return Err(path_error(
                relative_path,
                "todo file name has no agent delimiter",
            ));
        };
        if native_session_id.is_empty() || native_agent_id.is_empty() {
            return Err(path_error(
                relative_path,
                "todo session and agent ids must not be empty",
            ));
        }
        Ok(Self {
            native_session_id: native_session_id.to_string(),
            native_agent_id: native_agent_id.to_string(),
        })
    }

    fn decode(context: &AdapterObjectContext) -> Result<Self, AdapterError> {
        decode_object_context(context)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ClaudeTaskItemContext {
    native_collection_id: String,
    native_task_id: String,
}

impl ClaudeTaskItemContext {
    fn from_path(relative_path: &Path) -> Result<Self, AdapterError> {
        let components = utf8_components(relative_path)?;
        if components.len() != 3 || components[0] != "tasks" || components[1].is_empty() {
            return Err(path_error(
                relative_path,
                "task path must be tasks/<collection>/<positive-id>.json",
            ));
        }
        let Some(stem) = components[2].strip_suffix(".json") else {
            return Err(path_error(relative_path, "task item must end in .json"));
        };
        let canonical_id = stem
            .parse::<u64>()
            .ok()
            .filter(|value| *value > 0)
            .map(|value| value.to_string());
        if canonical_id.as_deref() != Some(stem) {
            return Err(path_error(
                relative_path,
                "task item file name must be a canonical positive integer",
            ));
        }
        Ok(Self {
            native_collection_id: components[1].clone(),
            native_task_id: stem.to_string(),
        })
    }

    fn decode(context: &AdapterObjectContext) -> Result<Self, AdapterError> {
        decode_object_context(context)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ClaudePlanContext {
    native_plan_id: String,
}

impl ClaudePlanContext {
    fn from_path(relative_path: &Path) -> Result<Self, AdapterError> {
        let components = utf8_components(relative_path)?;
        if components.len() != 2 || components[0] != "plans" {
            return Err(path_error(
                relative_path,
                "plan path must be plans/<slug>.md",
            ));
        }
        let Some(native_plan_id) = components[1]
            .strip_suffix(".md")
            .filter(|value| !value.is_empty())
        else {
            return Err(path_error(relative_path, "plan slug must not be empty"));
        };
        Ok(Self {
            native_plan_id: native_plan_id.to_string(),
        })
    }

    fn decode(context: &AdapterObjectContext) -> Result<Self, AdapterError> {
        decode_object_context(context)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ClaudeArtifactContentContext {
    native_session_id: String,
    native_artifact_id: String,
    native_file_hash: String,
    version: u64,
}

impl ClaudeArtifactContentContext {
    fn from_path(relative_path: &Path) -> Result<Self, AdapterError> {
        let components = utf8_components(relative_path)?;
        if components.len() != 3 || components[0] != "file-history" {
            return Err(path_error(
                relative_path,
                "artifact content path must be file-history/<session>/<hash>@v<version>",
            ));
        }
        if !is_uuid(&components[1]) {
            return Err(path_error(
                relative_path,
                "artifact content session directory is not a UUID",
            ));
        }
        let Some((native_file_hash, version)) = parse_artifact_file_name(&components[2]) else {
            return Err(path_error(
                relative_path,
                "artifact content name must be lowercase-hex@v<canonical-positive-version>",
            ));
        };
        Ok(Self {
            native_session_id: components[1].clone(),
            native_artifact_id: components[2].clone(),
            native_file_hash,
            version,
        })
    }

    fn decode(context: &AdapterObjectContext) -> Result<Self, AdapterError> {
        decode_object_context(context)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ClaudeWorkflowContext {
    project_slug: String,
    native_session_id: String,
    native_workflow_id: String,
}

impl ClaudeWorkflowContext {
    fn run_from_path(relative_path: &Path) -> Result<Self, AdapterError> {
        let components = utf8_components(relative_path)?;
        if components.len() != 4 || components[2] != "workflows" {
            return Err(path_error(
                relative_path,
                "workflow run path must be <project>/<session>/workflows/<workflow>.json",
            ));
        }
        let native_workflow_id = components[3]
            .strip_suffix(".json")
            .ok_or_else(|| path_error(relative_path, "workflow run must be a JSON document"))?;
        Self::validate(
            relative_path,
            &components[0],
            &components[1],
            native_workflow_id,
        )
    }

    fn journal_from_path(relative_path: &Path) -> Result<Self, AdapterError> {
        let components = utf8_components(relative_path)?;
        if components.len() != 6
            || components[2] != "subagents"
            || components[3] != "workflows"
            || components[5] != "journal.jsonl"
        {
            return Err(path_error(
                relative_path,
                "workflow journal path must be <project>/<session>/subagents/workflows/<workflow>/journal.jsonl",
            ));
        }
        Self::validate(
            relative_path,
            &components[0],
            &components[1],
            &components[4],
        )
    }

    fn validate(
        path: &Path,
        project_slug: &str,
        native_session_id: &str,
        native_workflow_id: &str,
    ) -> Result<Self, AdapterError> {
        if project_slug.is_empty() {
            return Err(path_error(path, "workflow project slug must not be empty"));
        }
        if !is_uuid(native_session_id) {
            return Err(path_error(path, "workflow session is not a UUID"));
        }
        if !native_workflow_id
            .strip_prefix("wf_")
            .is_some_and(|suffix| !suffix.is_empty())
        {
            return Err(path_error(
                path,
                "workflow identity must start with wf_ and include a suffix",
            ));
        }
        Ok(Self {
            project_slug: project_slug.to_string(),
            native_session_id: native_session_id.to_string(),
            native_workflow_id: native_workflow_id.to_string(),
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeActiveSessionDocument {
    pid: u32,
    session_id: String,
    cwd: String,
    started_at: i64,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    entrypoint: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    updated_at: Option<i64>,
    #[serde(default)]
    status_updated_at: Option<i64>,
    #[serde(default)]
    proc_start: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    peer_protocol: Option<u32>,
    #[serde(default)]
    name_source: Option<String>,
    #[serde(default)]
    bridge_session_id: Option<String>,
    #[serde(default)]
    messaging_socket_path: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeTodoItemDocument {
    content: String,
    status: String,
    #[serde(default)]
    active_form: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeTaskItemDocument {
    id: String,
    subject: String,
    description: String,
    #[serde(default)]
    active_form: Option<String>,
    #[serde(default)]
    owner: Option<String>,
    status: String,
    #[serde(default)]
    blocks: Vec<String>,
    #[serde(default)]
    blocked_by: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeArtifactCheckpointDocument {
    message_id: String,
    timestamp: String,
    #[serde(default)]
    tracked_file_backups: BTreeMap<String, ClaudeArtifactBackupDocument>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeArtifactSnapshotDocument {
    message_id: String,
    #[serde(default)]
    is_snapshot_update: bool,
    snapshot: ClaudeArtifactCheckpointDocument,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeArtifactDeltaDocument {
    message_id: String,
    snapshot_message_id: String,
    #[serde(default)]
    timestamp: Option<String>,
    tracking_path: String,
    backup: ClaudeArtifactBackupDocument,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeArtifactBackupDocument {
    backup_file_name: Value,
    version: u64,
    backup_time: String,
    #[serde(default)]
    real_parent_dir: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeWorkflowRunDocument {
    run_id: String,
    timestamp: String,
    task_id: String,
    script: String,
    script_path: String,
    #[serde(default)]
    args: Option<String>,
    agent_count: u64,
    duration_ms: u64,
    summary: String,
    workflow_name: String,
    status: String,
    start_time: u64,
    default_model: String,
    total_tokens: u64,
    total_tool_calls: u64,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeWorkflowJournalDocument {
    #[serde(rename = "type")]
    kind: String,
    agent_id: String,
    key: String,
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

fn decode_active_session(
    adapter_id: &AdapterId,
    context: &ClaudeActiveSessionContext,
    record: &SourceRecord,
    output: &mut FactBatch,
) -> Result<DecodeDisposition, AdapterError> {
    if record.state == SourceRecordState::Absent {
        return Ok(DecodeDisposition::IgnoredKnown);
    }
    let document: ClaudeActiveSessionDocument = match serde_json::from_slice(&record.payload) {
        Ok(document) => document,
        Err(error) => {
            preserve_unknown(
                record,
                output,
                Some("active_session".to_string()),
                format!("Claude active session is not a supported JSON object: {error}"),
            )?;
            return Ok(DecodeDisposition::PreservedUnknown);
        }
    };
    if document.pid == 0 || document.pid != context.native_pid {
        return preserve_active_session_contract_loss(
            record,
            output,
            "payload pid does not match the source file name",
        );
    }
    let Some(native_session_id) = nonempty(&document.session_id) else {
        return preserve_active_session_contract_loss(record, output, "session id is empty");
    };
    let Some(cwd) = nonempty(&document.cwd) else {
        return preserve_active_session_contract_loss(record, output, "cwd is empty");
    };
    if document.started_at < 0
        || document.updated_at.is_some_and(|value| value < 0)
        || document.status_updated_at.is_some_and(|value| value < 0)
    {
        return preserve_active_session_contract_loss(
            record,
            output,
            "contains a negative epoch-millisecond timestamp",
        );
    }

    let native_process_started_at = document.proc_start.as_deref().and_then(nonempty);
    let mut native_presence_key = Vec::new();
    push_key_component(&mut native_presence_key, &document.pid.to_be_bytes());
    push_key_component(&mut native_presence_key, native_session_id.as_bytes());
    match &native_process_started_at {
        Some(process_start) => {
            push_key_component(&mut native_presence_key, b"proc-start");
            push_key_component(&mut native_presence_key, process_start.as_bytes());
        }
        None => {
            push_key_component(&mut native_presence_key, b"session-start");
            push_key_component(&mut native_presence_key, &document.started_at.to_be_bytes());
        }
    }

    output.push(
        record,
        Fact::Presence(PresenceFact {
            presence: EntityKey::native(
                adapter_id,
                record.source_instance_id,
                "presence",
                &native_presence_key,
            )?,
            session: EntityKey::native(
                adapter_id,
                record.source_instance_id,
                "session",
                native_session_id.as_bytes(),
            )?,
            run: EntityKey::native(
                adapter_id,
                record.source_instance_id,
                "run",
                native_session_id.as_bytes(),
            )?,
            native_session_id,
            native_pid: document.pid,
            cwd,
            started_at: epoch_millis_timestamp(document.started_at),
            native_kind: document.kind.as_deref().and_then(nonempty),
            entrypoint: document.entrypoint.as_deref().and_then(nonempty),
            name: document.name.as_deref().and_then(nonempty),
            native_status: document.status.as_deref().and_then(nonempty),
            updated_at: document.updated_at.map(epoch_millis_timestamp),
            status_updated_at: document.status_updated_at.map(epoch_millis_timestamp),
            native_process_started_at,
            version: document.version.as_deref().and_then(nonempty),
            peer_protocol: document.peer_protocol,
            name_source: document.name_source.as_deref().and_then(nonempty),
            bridge_session_id: document.bridge_session_id.as_deref().and_then(nonempty),
            messaging_socket_path: document.messaging_socket_path.as_deref().and_then(nonempty),
        }),
    )?;
    Ok(DecodeDisposition::Applied)
}

fn preserve_active_session_contract_loss(
    record: &SourceRecord,
    output: &mut FactBatch,
    detail: &str,
) -> Result<DecodeDisposition, AdapterError> {
    preserve_unknown(
        record,
        output,
        Some("active_session".to_string()),
        format!("Claude active session {detail}"),
    )?;
    Ok(DecodeDisposition::PreservedUnknown)
}

fn decode_todo_snapshot(
    adapter_id: &AdapterId,
    context: &ClaudeTodoContext,
    record: &SourceRecord,
    output: &mut FactBatch,
) -> Result<DecodeDisposition, AdapterError> {
    let documents: Vec<ClaudeTodoItemDocument> = match serde_json::from_slice(&record.payload) {
        Ok(documents) => documents,
        Err(error) => {
            preserve_unknown(
                record,
                output,
                Some("todo_snapshot".to_string()),
                format!("Claude todo snapshot is not a supported JSON array: {error}"),
            )?;
            return Ok(DecodeDisposition::PreservedUnknown);
        }
    };
    if documents.len() > TODO_ITEM_LIMIT {
        return preserve_task_contract_loss(
            record,
            output,
            "todo_snapshot",
            &format!("exceeds the {TODO_ITEM_LIMIT} item bound"),
        );
    }

    let mut native_collection_key = Vec::new();
    push_key_component(&mut native_collection_key, b"todo");
    push_key_component(
        &mut native_collection_key,
        context.native_session_id.as_bytes(),
    );
    push_key_component(
        &mut native_collection_key,
        context.native_agent_id.as_bytes(),
    );
    let collection = EntityKey::native(
        adapter_id,
        record.source_instance_id,
        "task_collection",
        &native_collection_key,
    )?;

    let mut occurrences: BTreeMap<[u8; 32], u32> = BTreeMap::new();
    let mut items = Vec::with_capacity(documents.len());
    for document in documents {
        let Some(subject) = nonempty(&document.content) else {
            return preserve_task_contract_loss(
                record,
                output,
                "todo_snapshot",
                "contains an item with empty content",
            );
        };
        let Some(native_status) = nonempty(&document.status) else {
            return preserve_task_contract_loss(
                record,
                output,
                "todo_snapshot",
                "contains an item with empty status",
            );
        };
        let digest = *blake3::hash(subject.as_bytes()).as_bytes();
        let occurrence = occurrences.entry(digest).or_default();
        let mut native_task_key = native_collection_key.clone();
        push_key_component(&mut native_task_key, b"content-fingerprint");
        push_key_component(&mut native_task_key, &digest);
        push_key_component(&mut native_task_key, &occurrence.to_be_bytes());
        *occurrence = occurrence.saturating_add(1);
        items.push(TaskItemSnapshot {
            task: EntityKey::native(
                adapter_id,
                record.source_instance_id,
                "task",
                &native_task_key,
            )?,
            native_task_id: None,
            subject,
            description: None,
            active_form: document.active_form.as_deref().and_then(nonempty),
            native_owner: None,
            status: task_status(&native_status),
            blocks: Vec::new(),
            blocked_by: Vec::new(),
        });
    }

    let session = EntityKey::native(
        adapter_id,
        record.source_instance_id,
        "session",
        context.native_session_id.as_bytes(),
    )?;
    let run = (context.native_agent_id == context.native_session_id)
        .then(|| {
            EntityKey::native(
                adapter_id,
                record.source_instance_id,
                "run",
                context.native_session_id.as_bytes(),
            )
        })
        .transpose()?;
    output.push(
        record,
        Fact::TaskSnapshot(TaskSnapshotFact {
            collection,
            session: Some(session),
            run,
            team: None,
            native_collection_id: format!(
                "{}-agent-{}",
                context.native_session_id, context.native_agent_id
            ),
            native_owner_id: Some(context.native_agent_id.clone()),
            kind: TaskCollectionKind::TodoList,
            coverage: TaskSnapshotCoverage::Complete,
            items,
        }),
    )?;
    Ok(DecodeDisposition::Applied)
}

fn decode_task_item(
    adapter_id: &AdapterId,
    context: &ClaudeTaskItemContext,
    record: &SourceRecord,
    output: &mut FactBatch,
) -> Result<DecodeDisposition, AdapterError> {
    let document: ClaudeTaskItemDocument = match serde_json::from_slice(&record.payload) {
        Ok(document) => document,
        Err(error) => {
            preserve_unknown(
                record,
                output,
                Some("task_item".to_string()),
                format!("Claude task item is not a supported JSON object: {error}"),
            )?;
            return Ok(DecodeDisposition::PreservedUnknown);
        }
    };
    if document.id != context.native_task_id {
        return preserve_task_contract_loss(
            record,
            output,
            "task_item",
            "payload id does not match the source file name",
        );
    }
    let Some(subject) = nonempty(&document.subject) else {
        return preserve_task_contract_loss(record, output, "task_item", "subject is empty");
    };
    let Some(native_status) = nonempty(&document.status) else {
        return preserve_task_contract_loss(record, output, "task_item", "status is empty");
    };
    if document
        .blocks
        .iter()
        .chain(document.blocked_by.iter())
        .any(|value| value.trim().is_empty())
    {
        return preserve_task_contract_loss(
            record,
            output,
            "task_item",
            "contains an empty dependency id",
        );
    }

    let mut native_collection_key = Vec::new();
    push_key_component(&mut native_collection_key, b"task-directory");
    push_key_component(
        &mut native_collection_key,
        context.native_collection_id.as_bytes(),
    );
    let mut native_task_key = native_collection_key.clone();
    push_key_component(&mut native_task_key, context.native_task_id.as_bytes());
    output.push(
        record,
        Fact::TaskSnapshot(TaskSnapshotFact {
            collection: EntityKey::native(
                adapter_id,
                record.source_instance_id,
                "task_collection",
                &native_collection_key,
            )?,
            // A Claude task-directory name can be a session id, team name, or
            // other native scope. Keep it unjoined until another native fact
            // disambiguates it rather than guessing from its spelling.
            session: None,
            run: None,
            team: None,
            native_collection_id: context.native_collection_id.clone(),
            native_owner_id: None,
            kind: TaskCollectionKind::NativeTaskList,
            coverage: TaskSnapshotCoverage::ItemDocument,
            items: vec![TaskItemSnapshot {
                task: EntityKey::native(
                    adapter_id,
                    record.source_instance_id,
                    "task",
                    &native_task_key,
                )?,
                native_task_id: Some(context.native_task_id.clone()),
                subject,
                description: Some(document.description),
                active_form: document.active_form.as_deref().and_then(nonempty),
                native_owner: document.owner.as_deref().and_then(nonempty),
                status: task_status(&native_status),
                blocks: document.blocks,
                blocked_by: document.blocked_by,
            }],
        }),
    )?;
    Ok(DecodeDisposition::Applied)
}

fn decode_plan_document(
    adapter_id: &AdapterId,
    context: &ClaudePlanContext,
    record: &SourceRecord,
    output: &mut FactBatch,
) -> Result<DecodeDisposition, AdapterError> {
    let content = match std::str::from_utf8(&record.payload) {
        Ok(content) => content.to_string(),
        Err(error) => {
            preserve_unknown(
                record,
                output,
                Some("plan_document".to_string()),
                format!("Claude plan document is not valid UTF-8: {error}"),
            )?;
            return Ok(DecodeDisposition::PreservedUnknown);
        }
    };
    let title = first_markdown_heading(&content).unwrap_or_else(|| context.native_plan_id.clone());
    output.push(
        record,
        Fact::PlanSnapshot(PlanSnapshotFact {
            plan: EntityKey::native(
                adapter_id,
                record.source_instance_id,
                "plan",
                context.native_plan_id.as_bytes(),
            )?,
            native_plan_id: context.native_plan_id.clone(),
            title,
            size_bytes: record.payload.len() as u64,
            content,
            source_time: None,
        }),
    )?;
    Ok(DecodeDisposition::Applied)
}

fn decode_artifact_content(
    adapter_id: &AdapterId,
    context: &ClaudeArtifactContentContext,
    record: &SourceRecord,
    output: &mut FactBatch,
) -> Result<DecodeDisposition, AdapterError> {
    if record.state == SourceRecordState::Absent {
        return Ok(DecodeDisposition::IgnoredKnown);
    }
    let session = EntityKey::native(
        adapter_id,
        record.source_instance_id,
        "session",
        context.native_session_id.as_bytes(),
    )?;
    output.push(
        record,
        Fact::ArtifactContent(ArtifactContentFact {
            artifact: artifact_key(
                adapter_id,
                record.source_instance_id,
                &context.native_session_id,
                Some(&context.native_artifact_id),
                None,
                context.version,
                None,
            )?,
            session,
            native_artifact_id: context.native_artifact_id.clone(),
            native_file_hash: context.native_file_hash.clone(),
            version: context.version,
            size_bytes: record.payload.len() as u64,
            content: record.payload.clone(),
        }),
    )?;
    Ok(DecodeDisposition::Applied)
}

fn decode_workflow_run(
    adapter_id: &AdapterId,
    context: &ClaudeWorkflowContext,
    record: &SourceRecord,
    output: &mut FactBatch,
) -> Result<DecodeDisposition, AdapterError> {
    if record.state == SourceRecordState::Absent {
        return Ok(DecodeDisposition::IgnoredKnown);
    }
    let native_snapshot: Value = match serde_json::from_slice(&record.payload) {
        Ok(value) => value,
        Err(error) => {
            return preserve_workflow_contract_loss(
                record,
                output,
                "workflow_run",
                &format!("is not valid JSON: {error}"),
            );
        }
    };
    let document: ClaudeWorkflowRunDocument = match serde_json::from_value(native_snapshot.clone())
    {
        Ok(document) => document,
        Err(error) => {
            return preserve_workflow_contract_loss(
                record,
                output,
                "workflow_run",
                &format!("is not a supported run document: {error}"),
            );
        }
    };
    if document.run_id != context.native_workflow_id {
        return preserve_workflow_contract_loss(
            record,
            output,
            "workflow_run",
            "payload runId does not match the source file name",
        );
    }
    for (field, value) in [
        ("taskId", document.task_id.as_str()),
        ("workflowName", document.workflow_name.as_str()),
        ("status", document.status.as_str()),
        ("defaultModel", document.default_model.as_str()),
        ("script", document.script.as_str()),
        ("scriptPath", document.script_path.as_str()),
        ("summary", document.summary.as_str()),
        ("timestamp", document.timestamp.as_str()),
    ] {
        if value.trim().is_empty() {
            return preserve_workflow_contract_loss(
                record,
                output,
                "workflow_run",
                &format!("has an empty {field}"),
            );
        }
    }
    let Ok(start_time) = i64::try_from(document.start_time) else {
        return preserve_workflow_contract_loss(
            record,
            output,
            "workflow_run",
            "startTime exceeds the supported epoch-millisecond range",
        );
    };
    let workflow = workflow_key(adapter_id, record.source_instance_id, context)?;
    output.push(
        record,
        Fact::WorkflowSnapshot(WorkflowSnapshotFact {
            workflow,
            session: EntityKey::native(
                adapter_id,
                record.source_instance_id,
                "session",
                context.native_session_id.as_bytes(),
            )?,
            project: EntityKey::native(
                adapter_id,
                record.source_instance_id,
                "project",
                context.project_slug.as_bytes(),
            )?,
            native_workflow_id: document.run_id,
            native_task_id: document.task_id,
            name: document.workflow_name,
            native_status: document.status.clone(),
            status: workflow_status(&document.status),
            default_model: document.default_model,
            script: document.script,
            script_path: document.script_path,
            args: document.args,
            summary: document.summary,
            error: document.error,
            started_at: epoch_millis_timestamp(start_time),
            finished_at: native_timestamp(&document.timestamp),
            duration_ms: document.duration_ms,
            agent_count: document.agent_count,
            total_tokens: document.total_tokens,
            total_tool_calls: document.total_tool_calls,
            native_snapshot,
        }),
    )?;
    Ok(DecodeDisposition::Applied)
}

fn decode_workflow_journal(
    adapter_id: &AdapterId,
    context: &ClaudeWorkflowContext,
    record: &SourceRecord,
    output: &mut FactBatch,
) -> Result<DecodeDisposition, AdapterError> {
    if record.state == SourceRecordState::Absent {
        return Ok(DecodeDisposition::IgnoredKnown);
    }
    let value: Value = match serde_json::from_slice(&record.payload) {
        Ok(value) => value,
        Err(error) => {
            return preserve_workflow_contract_loss(
                record,
                output,
                "workflow_journal",
                &format!("record is not valid JSON: {error}"),
            );
        }
    };
    let document: ClaudeWorkflowJournalDocument = match serde_json::from_value(value.clone()) {
        Ok(document) => document,
        Err(error) => {
            return preserve_workflow_contract_loss(
                record,
                output,
                "workflow_journal",
                &format!("record is not supported: {error}"),
            );
        }
    };
    let Some(native_agent_id) = nonempty(&document.agent_id) else {
        return preserve_workflow_contract_loss(
            record,
            output,
            "workflow_journal",
            "record has an empty agentId",
        );
    };
    let Some(native_event_key) = nonempty(&document.key) else {
        return preserve_workflow_contract_loss(
            record,
            output,
            "workflow_journal",
            "record has an empty key",
        );
    };
    let (kind, result) = match document.kind.as_str() {
        "started" if value.get("result").is_none() => (WorkflowMemberEventKind::Started, None),
        "result" => {
            let Some(result) = value.get("result").cloned() else {
                return preserve_workflow_contract_loss(
                    record,
                    output,
                    "workflow_journal",
                    "result record is missing its result value",
                );
            };
            (WorkflowMemberEventKind::Result, Some(result))
        }
        "started" => {
            return preserve_workflow_contract_loss(
                record,
                output,
                "workflow_journal",
                "started record unexpectedly contains a result value",
            );
        }
        _ => {
            return preserve_workflow_contract_loss(
                record,
                output,
                "workflow_journal",
                "record has an unsupported event type",
            );
        }
    };

    let workflow = workflow_key(adapter_id, record.source_instance_id, context)?;
    let mut member_native_key = workflow_native_key(context);
    push_key_component(&mut member_native_key, native_agent_id.as_bytes());
    let child_run_native_key = format!(
        "{}\0{}\0{}",
        context.native_session_id, context.native_workflow_id, native_agent_id
    );
    output.push(
        record,
        Fact::WorkflowMemberEvent(WorkflowMemberEventFact {
            workflow,
            member: EntityKey::native(
                adapter_id,
                record.source_instance_id,
                "workflow_member",
                &member_native_key,
            )?,
            child_run: EntityKey::native(
                adapter_id,
                record.source_instance_id,
                "run",
                child_run_native_key.as_bytes(),
            )?,
            session: EntityKey::native(
                adapter_id,
                record.source_instance_id,
                "session",
                context.native_session_id.as_bytes(),
            )?,
            project: EntityKey::native(
                adapter_id,
                record.source_instance_id,
                "project",
                context.project_slug.as_bytes(),
            )?,
            native_workflow_id: context.native_workflow_id.clone(),
            native_agent_id,
            native_event_key,
            kind,
            result,
        }),
    )?;
    Ok(DecodeDisposition::Applied)
}

fn preserve_workflow_contract_loss(
    record: &SourceRecord,
    output: &mut FactBatch,
    native_kind: &str,
    detail: &str,
) -> Result<DecodeDisposition, AdapterError> {
    preserve_unknown(
        record,
        output,
        Some(native_kind.to_string()),
        format!("Claude {native_kind} {detail}"),
    )?;
    Ok(DecodeDisposition::PreservedUnknown)
}

fn preserve_task_contract_loss(
    record: &SourceRecord,
    output: &mut FactBatch,
    native_kind: &str,
    detail: &str,
) -> Result<DecodeDisposition, AdapterError> {
    preserve_unknown(
        record,
        output,
        Some(native_kind.to_string()),
        format!("Claude {native_kind} {detail}"),
    )?;
    Ok(DecodeDisposition::PreservedUnknown)
}

fn task_status(native_status: &str) -> TaskStatus {
    match native_status {
        "pending" => TaskStatus::Pending,
        "in_progress" => TaskStatus::InProgress,
        "completed" => TaskStatus::Completed,
        other => TaskStatus::Other(other.to_string()),
    }
}

fn first_markdown_heading(content: &str) -> Option<String> {
    content.lines().find_map(|line| {
        let rest = line.strip_prefix('#')?;
        let first = rest.chars().next()?;
        if !first.is_whitespace() {
            return None;
        }
        nonempty(rest.trim_start_matches(char::is_whitespace))
    })
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
    match artifact_metadata_snapshot(
        adapter_id,
        record.source_instance_id,
        context,
        session,
        &value,
    ) {
        Ok(Some(fact)) => {
            output.push(record, Fact::ArtifactMetadataSnapshot(fact))?;
        }
        Ok(None) => {}
        Err(detail) => {
            output.push_diagnostic(AdapterDiagnostic {
                class: AdapterErrorClass::RecordPermanent,
                code: "claude_artifact_projection_loss".to_string(),
                message: detail,
            })?;
        }
    }
    Ok(DecodeDisposition::Applied)
}

fn artifact_metadata_snapshot(
    adapter_id: &AdapterId,
    source_instance_id: u64,
    context: &ClaudeTranscriptContext,
    session: EntityKey,
    value: &Value,
) -> Result<Option<ArtifactMetadataSnapshotFact>, String> {
    let native_kind = value.get("type").and_then(Value::as_str);
    let (
        native_message_id,
        native_snapshot_message_id,
        observation_kind,
        is_snapshot_update,
        source_time,
        documents,
    ) = match native_kind {
        Some("file-history-snapshot") => {
            let document: ClaudeArtifactSnapshotDocument = serde_json::from_value(value.clone())
                .map_err(|error| {
                    format!("Claude file-history snapshot is not supported: {error}")
                })?;
            let documents = document
                .snapshot
                .tracked_file_backups
                .into_iter()
                .collect::<Vec<_>>();
            (
                document.message_id,
                document.snapshot.message_id,
                ArtifactObservationKind::Checkpoint,
                document.is_snapshot_update,
                nonempty(&document.snapshot.timestamp).map(|value| native_timestamp(&value)),
                documents,
            )
        }
        Some("file-history-delta") => {
            let document: ClaudeArtifactDeltaDocument = serde_json::from_value(value.clone())
                .map_err(|error| format!("Claude file-history delta is not supported: {error}"))?;
            let source_time = document
                .timestamp
                .as_deref()
                .and_then(nonempty)
                .map(|value| native_timestamp(&value));
            (
                document.message_id,
                document.snapshot_message_id,
                ArtifactObservationKind::Delta,
                false,
                source_time,
                vec![(document.tracking_path, document.backup)],
            )
        }
        _ => return Ok(None),
    };
    let native_message_id = nonempty(&native_message_id)
        .ok_or_else(|| "Claude file-history message id is empty".to_string())?;
    let native_snapshot_message_id = nonempty(&native_snapshot_message_id)
        .ok_or_else(|| "Claude file-history snapshot message id is empty".to_string())?;
    if documents.len() > ARTIFACT_METADATA_LIMIT {
        return Err(format!(
            "Claude file-history observation exceeds the {ARTIFACT_METADATA_LIMIT} item bound"
        ));
    }

    let mut artifact_keys = BTreeSet::new();
    let mut artifacts = Vec::with_capacity(documents.len());
    for (tracking_path, document) in documents {
        let tracking_path = nonempty(&tracking_path)
            .ok_or_else(|| "Claude file-history tracking path is empty".to_string())?;
        if document.version == 0 {
            return Err("Claude file-history version must be positive".to_string());
        }
        let backup_time = nonempty(&document.backup_time)
            .ok_or_else(|| "Claude file-history backup time is empty".to_string())?;
        let (native_artifact_id, capture) = match document.backup_file_name {
            Value::String(file_name) => {
                let file_name = nonempty(&file_name)
                    .ok_or_else(|| "Claude file-history backup file name is empty".to_string())?;
                let Some((_, file_version)) = parse_artifact_file_name(&file_name) else {
                    return Err(format!(
                        "Claude file-history backup file name is invalid: {file_name}"
                    ));
                };
                if file_version != document.version {
                    return Err(format!(
                        "Claude file-history backup version {} disagrees with {file_name}",
                        document.version
                    ));
                }
                (Some(file_name), ArtifactCapture::ContentExpected)
            }
            Value::Null => (None, ArtifactCapture::NotCaptured),
            _ => {
                return Err(
                    "Claude file-history backup file name must be a string or null".to_string(),
                );
            }
        };
        let artifact = artifact_key(
            adapter_id,
            source_instance_id,
            &context.session_id,
            native_artifact_id.as_deref(),
            Some(&tracking_path),
            document.version,
            Some(&backup_time),
        )
        .map_err(|error| format!("Claude file-history artifact identity is invalid: {error}"))?;
        if !artifact_keys.insert(artifact.clone()) {
            return Err(
                "Claude file-history observation maps multiple paths to one artifact".to_string(),
            );
        }
        artifacts.push(ArtifactMetadataEntry {
            artifact,
            native_artifact_id,
            tracking_path,
            real_parent_dir: document.real_parent_dir.as_deref().and_then(nonempty),
            version: document.version,
            backup_time: native_timestamp(&backup_time),
            capture,
        });
    }
    Ok(Some(ArtifactMetadataSnapshotFact {
        session,
        native_message_id,
        native_snapshot_message_id,
        observation_kind,
        is_snapshot_update,
        source_time,
        artifacts,
    }))
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

fn parse_artifact_file_name(file_name: &str) -> Option<(String, u64)> {
    let (native_file_hash, version_text) = file_name.rsplit_once("@v")?;
    if native_file_hash.is_empty()
        || !native_file_hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    let version = version_text
        .parse::<u64>()
        .ok()
        .filter(|value| *value > 0)?;
    (version.to_string() == version_text).then(|| (native_file_hash.to_string(), version))
}

fn artifact_key(
    adapter_id: &AdapterId,
    source_instance_id: u64,
    native_session_id: &str,
    native_artifact_id: Option<&str>,
    tracking_path: Option<&str>,
    version: u64,
    backup_time: Option<&str>,
) -> Result<EntityKey, AdapterError> {
    let mut native_key = Vec::new();
    push_key_component(&mut native_key, native_session_id.as_bytes());
    match native_artifact_id {
        Some(native_artifact_id) => {
            push_key_component(&mut native_key, b"named-backup");
            push_key_component(&mut native_key, native_artifact_id.as_bytes());
        }
        None => {
            push_key_component(&mut native_key, b"not-captured");
            push_key_component(
                &mut native_key,
                tracking_path
                    .expect("unbacked artifact requires a tracking path")
                    .as_bytes(),
            );
            push_key_component(&mut native_key, &version.to_be_bytes());
            push_key_component(
                &mut native_key,
                backup_time
                    .expect("unbacked artifact requires a backup time")
                    .as_bytes(),
            );
        }
    }
    EntityKey::native(adapter_id, source_instance_id, "artifact", &native_key)
}

fn workflow_native_key(context: &ClaudeWorkflowContext) -> Vec<u8> {
    let mut native_key = Vec::new();
    push_key_component(&mut native_key, context.native_session_id.as_bytes());
    push_key_component(&mut native_key, context.native_workflow_id.as_bytes());
    native_key
}

fn workflow_key(
    adapter_id: &AdapterId,
    source_instance_id: u64,
    context: &ClaudeWorkflowContext,
) -> Result<EntityKey, AdapterError> {
    EntityKey::native(
        adapter_id,
        source_instance_id,
        "workflow",
        &workflow_native_key(context),
    )
}

fn workflow_status(native_status: &str) -> WorkflowStatus {
    match native_status {
        "pending" | "queued" => WorkflowStatus::Pending,
        "running" | "in_progress" => WorkflowStatus::Running,
        "completed" => WorkflowStatus::Succeeded,
        "failed" => WorkflowStatus::Failed,
        "cancelled" | "canceled" | "killed" => WorkflowStatus::Cancelled,
        other => WorkflowStatus::Other(other.to_string()),
    }
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

    fn presence_record(payload: &[u8]) -> SourceRecord {
        SourceRecord::new(
            &RecordOrigin {
                source_instance_id: 7,
                stream_id: 8,
                object_id: 9,
                observed_at: 10,
                source_timestamp_hint: None,
                media_type: SourceMediaType::new("application/json").unwrap(),
            },
            1,
            crate::source::SourceCursor::presence(crate::source::Revision::ZERO),
            crate::source::SourceCursor::presence(crate::source::Revision::digest(payload)),
            0,
            payload.to_vec(),
        )
    }

    fn document_record(payload: &[u8]) -> SourceRecord {
        SourceRecord::new(
            &RecordOrigin {
                source_instance_id: 7,
                stream_id: 8,
                object_id: 9,
                observed_at: 10,
                source_timestamp_hint: None,
                media_type: SourceMediaType::new("application/json").unwrap(),
            },
            1,
            crate::source::SourceCursor::snapshot(crate::source::Revision::ZERO),
            crate::source::SourceCursor::snapshot(crate::source::Revision::digest(payload)),
            0,
            payload.to_vec(),
        )
    }

    fn absent_presence_record() -> SourceRecord {
        SourceRecord::absent(
            &RecordOrigin {
                source_instance_id: 7,
                stream_id: 8,
                object_id: 9,
                observed_at: 10,
                source_timestamp_hint: None,
                media_type: SourceMediaType::new("application/json").unwrap(),
            },
            1,
            crate::source::SourceCursor::presence(crate::source::Revision::digest(b"present")),
            crate::source::SourceCursor::presence(crate::source::Revision::digest(b"absent")),
            0,
        )
    }

    fn absent_document_record() -> SourceRecord {
        SourceRecord::absent(
            &RecordOrigin {
                source_instance_id: 7,
                stream_id: 8,
                object_id: 9,
                observed_at: 10,
                source_timestamp_hint: None,
                media_type: SourceMediaType::new("text/plain").unwrap(),
            },
            1,
            crate::source::SourceCursor::snapshot(crate::source::Revision::digest(b"present")),
            crate::source::SourceCursor::snapshot(crate::source::Revision::digest(b"absent")),
            0,
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
        assert_eq!(discovered[0].roots.len(), 4);
        assert_eq!(discovered[0].roots[2].name, "teams");
        assert_eq!(
            discovered[0].roots[2].path,
            std::fs::canonicalize(root.path()).unwrap().join("teams")
        );
        assert_eq!(discovered[0].roots[3].name, "sessions");
        assert_eq!(
            discovered[0].roots[3].path,
            std::fs::canonicalize(root.path()).unwrap().join("sessions")
        );
        assert_eq!(streams.len(), 12);
        assert_eq!(adapter.manifest().contract_version, 9);
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
        assert!(adapter
            .manifest()
            .source_schema_versions
            .iter()
            .any(|version| version == "claude-code-active-session-v1"));
        assert!(adapter
            .manifest()
            .source_schema_versions
            .iter()
            .any(|version| version == "claude-code-todo-v1"));
        assert!(adapter
            .manifest()
            .source_schema_versions
            .iter()
            .any(|version| version == "claude-code-task-item-v1"));
        assert!(adapter
            .manifest()
            .source_schema_versions
            .iter()
            .any(|version| version == "claude-code-plan-v1"));
        assert!(adapter
            .manifest()
            .source_schema_versions
            .iter()
            .any(|version| version == "claude-code-file-history-v1"));
        assert!(adapter
            .manifest()
            .source_schema_versions
            .iter()
            .any(|version| version == "claude-code-workflow-v1"));
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
        assert!(matches!(
            streams[5].driver,
            DriverSpec::Presence(PresenceObjectConfig {
                include_content: true,
                max_content_bytes: ACTIVE_SESSION_MAX_BYTES
            })
        ));
        assert!(matches!(
            streams[6].driver,
            DriverSpec::ReplaceDocument(ReplaceDocumentConfig {
                max_document_bytes: TODO_MAX_BYTES
            })
        ));
        assert!(matches!(
            streams[7].driver,
            DriverSpec::ReplaceDocument(ReplaceDocumentConfig {
                max_document_bytes: TASK_ITEM_MAX_BYTES
            })
        ));
        assert!(matches!(
            streams[8].driver,
            DriverSpec::ReplaceDocument(ReplaceDocumentConfig {
                max_document_bytes: PLAN_MAX_BYTES
            })
        ));
        assert!(matches!(
            streams[9].driver,
            DriverSpec::ReplaceDocument(ReplaceDocumentConfig {
                max_document_bytes: ARTIFACT_CONTENT_MAX_BYTES
            })
        ));
        assert!(matches!(
            streams[10].driver,
            DriverSpec::ReplaceDocument(ReplaceDocumentConfig {
                max_document_bytes: WORKFLOW_RUN_MAX_BYTES
            })
        ));
        assert!(matches!(streams[11].driver, DriverSpec::AppendDelimited(_)));
        assert_eq!(streams[0].decoder.as_str(), PARENT_DECODER);
        assert_eq!(streams[1].decoder.as_str(), SUBAGENT_DECODER);
        assert_eq!(streams[2].decoder.as_str(), SUBAGENT_META_DECODER);
        assert_eq!(streams[3].decoder.as_str(), TEAM_CONFIG_DECODER);
        assert_eq!(streams[4].decoder.as_str(), TEAM_INBOX_DECODER);
        assert_eq!(streams[5].decoder.as_str(), ACTIVE_SESSION_DECODER);
        assert_eq!(streams[6].decoder.as_str(), TODO_DECODER);
        assert_eq!(streams[7].decoder.as_str(), TASK_ITEM_DECODER);
        assert_eq!(streams[8].decoder.as_str(), PLAN_DECODER);
        assert_eq!(streams[9].decoder.as_str(), ARTIFACT_CONTENT_DECODER);
        assert_eq!(streams[10].decoder.as_str(), WORKFLOW_RUN_DECODER);
        assert_eq!(streams[11].decoder.as_str(), WORKFLOW_JOURNAL_DECODER);
        assert_eq!(streams[2].authority, StreamAuthority::Supplemental);
        assert_eq!(streams[2].consistency, ConsistencyPolicy::SnapshotReplace);
        assert_eq!(streams[3].authority, StreamAuthority::Canonical);
        assert_eq!(streams[3].consistency, ConsistencyPolicy::SnapshotReplace);
        assert_eq!(streams[4].authority, StreamAuthority::Canonical);
        assert_eq!(streams[4].consistency, ConsistencyPolicy::SnapshotReplace);
        assert_eq!(streams[5].authority, StreamAuthority::Canonical);
        assert_eq!(streams[5].priority, IngestPriority::Interactive);
        assert_eq!(streams[5].consistency, ConsistencyPolicy::SnapshotReplace);
        for stream in &streams[6..=8] {
            assert_eq!(stream.authority, StreamAuthority::Canonical);
            assert_eq!(stream.priority, IngestPriority::Interactive);
            assert_eq!(stream.consistency, ConsistencyPolicy::SnapshotReplace);
            assert!(stream
                .capabilities
                .iter()
                .any(|capability| capability.as_str() == RUNTIME_TASKS));
        }
        assert_eq!(streams[9].authority, StreamAuthority::Canonical);
        assert_eq!(streams[9].priority, IngestPriority::ForegroundRepair);
        assert_eq!(streams[9].consistency, ConsistencyPolicy::SnapshotReplace);
        assert!(streams[9]
            .capabilities
            .iter()
            .any(|capability| capability.as_str() == RUNTIME_ARTIFACTS));
        assert_eq!(streams[10].authority, StreamAuthority::Canonical);
        assert_eq!(streams[10].priority, IngestPriority::ForegroundRepair);
        assert_eq!(streams[10].consistency, ConsistencyPolicy::SnapshotReplace);
        assert_eq!(streams[11].authority, StreamAuthority::Canonical);
        assert_eq!(streams[11].priority, IngestPriority::Interactive);
        assert_eq!(
            streams[11].consistency,
            ConsistencyPolicy::IncrementalCursor
        );
        for stream in &streams[10..=11] {
            assert!(stream
                .capabilities
                .iter()
                .any(|capability| capability.as_str() == RUNTIME_WORKFLOWS));
        }
        assert!(streams[0]
            .capabilities
            .iter()
            .any(|capability| capability.as_str() == RUNTIME_SUBAGENTS));
        assert!(streams[1]
            .capabilities
            .iter()
            .any(|capability| capability.as_str() == RUNTIME_SUBAGENTS));
        assert!(streams[0]
            .capabilities
            .iter()
            .any(|capability| capability.as_str() == RUNTIME_ARTIFACTS));
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
        assert!(streams[5]
            .capabilities
            .iter()
            .any(|capability| capability.as_str() == RUNTIME_PRESENCE));
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
        let presence = adapter
            .manifest()
            .capabilities
            .iter()
            .find(|capability| capability.id.as_str() == RUNTIME_PRESENCE)
            .unwrap();
        assert_eq!(presence.support.level, SupportLevel::Native);
        assert_eq!(
            presence.support.granularity,
            CapabilityGranularity::Custom("process_presence".to_string())
        );
        assert_eq!(presence.support.availability, Availability::Live);
        let tasks = adapter
            .manifest()
            .capabilities
            .iter()
            .find(|capability| capability.id.as_str() == RUNTIME_TASKS)
            .unwrap();
        assert_eq!(tasks.support.level, SupportLevel::Native);
        assert_eq!(
            tasks.support.granularity,
            CapabilityGranularity::Custom("task".to_string())
        );
        assert_eq!(tasks.support.availability, Availability::Live);
        let artifacts = adapter
            .manifest()
            .capabilities
            .iter()
            .find(|capability| capability.id.as_str() == RUNTIME_ARTIFACTS)
            .unwrap();
        assert_eq!(artifacts.support.level, SupportLevel::Native);
        assert_eq!(
            artifacts.support.granularity,
            CapabilityGranularity::Custom("artifact".to_string())
        );
        assert_eq!(artifacts.support.availability, Availability::Live);
        let workflows = adapter
            .manifest()
            .capabilities
            .iter()
            .find(|capability| capability.id.as_str() == RUNTIME_WORKFLOWS)
            .unwrap();
        assert_eq!(workflows.support.level, SupportLevel::Native);
        assert_eq!(
            workflows.support.granularity,
            CapabilityGranularity::Custom("workflow".to_string())
        );
        assert_eq!(workflows.support.availability, Availability::EventuallyLive);
        assert!(workflows
            .support
            .notes
            .as_deref()
            .is_some_and(|notes| notes.contains("never implies terminal child-run state")));
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
    fn active_session_context_requires_one_positive_numeric_pid_component() {
        let root = TempDir::new().unwrap();
        let adapter = ClaudeCodeAdapter::new();
        let context = adapter
            .bootstrap_object(
                &instance(root.path()),
                &object(ACTIVE_SESSION_STREAM, "4242.json"),
            )
            .unwrap();
        assert_eq!(
            ClaudeActiveSessionContext::decode(&context).unwrap(),
            ClaudeActiveSessionContext { native_pid: 4242 }
        );
        for invalid in ["0.json", "pid.json", "4242.txt", "nested/4242.json"] {
            assert!(adapter
                .bootstrap_object(
                    &instance(root.path()),
                    &object(ACTIVE_SESSION_STREAM, invalid),
                )
                .is_err());
        }
    }

    #[test]
    fn task_todo_and_plan_contexts_require_exact_native_paths() {
        let root = TempDir::new().unwrap();
        let adapter = ClaudeCodeAdapter::new();

        let todo = adapter
            .bootstrap_object(
                &instance(root.path()),
                &object(TODO_STREAM, &format!("todos/{SESSION}-agent-worker.json")),
            )
            .unwrap();
        assert_eq!(
            ClaudeTodoContext::decode(&todo).unwrap(),
            ClaudeTodoContext {
                native_session_id: SESSION.to_string(),
                native_agent_id: "worker".to_string(),
            }
        );
        let task = adapter
            .bootstrap_object(
                &instance(root.path()),
                &object(TASK_ITEM_STREAM, "tasks/rewrite/12.json"),
            )
            .unwrap();
        assert_eq!(
            ClaudeTaskItemContext::decode(&task).unwrap(),
            ClaudeTaskItemContext {
                native_collection_id: "rewrite".to_string(),
                native_task_id: "12".to_string(),
            }
        );
        let plan = adapter
            .bootstrap_object(
                &instance(root.path()),
                &object(PLAN_STREAM, "plans/ship-it.md"),
            )
            .unwrap();
        assert_eq!(
            ClaudePlanContext::decode(&plan).unwrap(),
            ClaudePlanContext {
                native_plan_id: "ship-it".to_string(),
            }
        );
        let artifact = adapter
            .bootstrap_object(
                &instance(root.path()),
                &object(
                    ARTIFACT_CONTENT_STREAM,
                    &format!("file-history/{SESSION}/71f902cd51ee4c6e@v12"),
                ),
            )
            .unwrap();
        assert_eq!(
            ClaudeArtifactContentContext::decode(&artifact).unwrap(),
            ClaudeArtifactContentContext {
                native_session_id: SESSION.to_string(),
                native_artifact_id: "71f902cd51ee4c6e@v12".to_string(),
                native_file_hash: "71f902cd51ee4c6e".to_string(),
                version: 12,
            }
        );

        for (stream, invalid) in [
            (TODO_STREAM, "todos/no-agent-.json"),
            (TODO_STREAM, "nested/todos/s-agent-a.json"),
            (TASK_ITEM_STREAM, "tasks/list/0.json"),
            (TASK_ITEM_STREAM, "tasks/list/01.json"),
            (TASK_ITEM_STREAM, "tasks/list/id.json"),
            (PLAN_STREAM, "plans/.md"),
            (PLAN_STREAM, "plans/nested/plan.md"),
            (
                ARTIFACT_CONTENT_STREAM,
                "file-history/not-a-session/hash@v1",
            ),
            (
                ARTIFACT_CONTENT_STREAM,
                "file-history/01234567-89ab-cdef-0123-456789abcdef/HASH@v1",
            ),
            (
                ARTIFACT_CONTENT_STREAM,
                "file-history/01234567-89ab-cdef-0123-456789abcdef/hash@v01",
            ),
            (
                ARTIFACT_CONTENT_STREAM,
                "nested/file-history/01234567-89ab-cdef-0123-456789abcdef/hash@v1",
            ),
        ] {
            assert!(adapter
                .bootstrap_object(&instance(root.path()), &object(stream, invalid))
                .is_err());
        }
    }

    #[test]
    fn workflow_contexts_require_exact_native_paths() {
        let root = TempDir::new().unwrap();
        let adapter = ClaudeCodeAdapter::new();
        let run = adapter
            .bootstrap_object(
                &instance(root.path()),
                &object(
                    WORKFLOW_RUN_STREAM,
                    &format!("project/{SESSION}/workflows/wf_main.json"),
                ),
            )
            .unwrap();
        let journal = adapter
            .bootstrap_object(
                &instance(root.path()),
                &object(
                    WORKFLOW_JOURNAL_STREAM,
                    &format!("project/{SESSION}/subagents/workflows/wf_main/journal.jsonl"),
                ),
            )
            .unwrap();
        let expected = ClaudeWorkflowContext {
            project_slug: "project".to_string(),
            native_session_id: SESSION.to_string(),
            native_workflow_id: "wf_main".to_string(),
        };
        assert_eq!(ClaudeWorkflowContext::decode(&run).unwrap(), expected);
        assert_eq!(ClaudeWorkflowContext::decode(&journal).unwrap(), expected);

        for (stream, invalid) in [
            (
                WORKFLOW_RUN_STREAM,
                "project/not-a-session/workflows/wf_main.json",
            ),
            (
                WORKFLOW_RUN_STREAM,
                &format!("project/{SESSION}/workflows/wf_.json"),
            ),
            (
                WORKFLOW_RUN_STREAM,
                &format!("project/{SESSION}/workflows/main.json"),
            ),
            (
                WORKFLOW_RUN_STREAM,
                &format!("project/{SESSION}/nested/workflows/wf_main.json"),
            ),
            (
                WORKFLOW_JOURNAL_STREAM,
                &format!("project/{SESSION}/workflows/wf_main/journal.jsonl"),
            ),
            (
                WORKFLOW_JOURNAL_STREAM,
                &format!("project/{SESSION}/subagents/workflows/wf_main/events.jsonl"),
            ),
        ] {
            assert!(adapter
                .bootstrap_object(&instance(root.path()), &object(stream, invalid))
                .is_err());
        }
    }

    #[test]
    fn todo_snapshot_is_complete_scoped_and_stable_across_status_edits() {
        let root = TempDir::new().unwrap();
        let adapter = ClaudeCodeAdapter::new();
        let object_context = adapter
            .bootstrap_object(
                &instance(root.path()),
                &object(
                    TODO_STREAM,
                    &format!("todos/{SESSION}-agent-{SESSION}.json"),
                ),
            )
            .unwrap();
        let first = document_record(
            br#"[
              {"content":"Add task projection","status":"pending","activeForm":"Adding task projection"},
              {"content":"Run parity","status":"future_native_status"}
            ]"#,
        );
        let mut batch = FactBatch::new(4, 4).unwrap();
        let disposition = adapter
            .decode(
                DecodeContext {
                    decoder: &DecoderId::new(TODO_DECODER).unwrap(),
                    object_context: &object_context,
                },
                &first,
                &mut batch,
            )
            .unwrap();
        assert_eq!(disposition, DecodeDisposition::Applied);
        let Fact::TaskSnapshot(snapshot) = &batch.facts()[0].value else {
            panic!("expected todo task snapshot");
        };
        assert_eq!(snapshot.kind, TaskCollectionKind::TodoList);
        assert_eq!(snapshot.coverage, TaskSnapshotCoverage::Complete);
        assert!(snapshot.session.is_some());
        assert!(snapshot.run.is_some());
        assert!(snapshot.team.is_none());
        assert_eq!(snapshot.native_owner_id.as_deref(), Some(SESSION));
        assert_eq!(snapshot.items.len(), 2);
        assert_eq!(snapshot.items[0].subject, "Add task projection");
        assert_eq!(snapshot.items[0].status, TaskStatus::Pending);
        assert_eq!(
            snapshot.items[1].status,
            TaskStatus::Other("future_native_status".to_string())
        );
        assert!(!fact_values(&batch).any(|fact| matches!(fact, Fact::RunEvidence(_))));

        let changed = document_record(
            br#"[{"content":"Add task projection","status":"completed","activeForm":"Adding task projection"}]"#,
        );
        let mut changed_batch = FactBatch::new(4, 4).unwrap();
        adapter
            .decode(
                DecodeContext {
                    decoder: &DecoderId::new(TODO_DECODER).unwrap(),
                    object_context: &object_context,
                },
                &changed,
                &mut changed_batch,
            )
            .unwrap();
        let Fact::TaskSnapshot(changed_snapshot) = &changed_batch.facts()[0].value else {
            panic!("expected updated todo task snapshot");
        };
        assert_eq!(changed_snapshot.items[0].task, snapshot.items[0].task);
        assert_eq!(changed_snapshot.items[0].status, TaskStatus::Completed);
    }

    #[test]
    fn numbered_task_item_preserves_dependencies_without_guessing_scope() {
        let root = TempDir::new().unwrap();
        let adapter = ClaudeCodeAdapter::new();
        let object_context = adapter
            .bootstrap_object(
                &instance(root.path()),
                &object(TASK_ITEM_STREAM, "tasks/truffle-rust-rewrite/1.json"),
            )
            .unwrap();
        let record = document_record(
            br#"{
              "id":"1",
              "subject":"Port the task pack",
              "description":"Keep replacement provenance",
              "activeForm":"Porting the task pack",
              "owner":"worker",
              "status":"in_progress",
              "blocks":["2"],
              "blockedBy":["3"]
            }"#,
        );
        let mut batch = FactBatch::new(4, 4).unwrap();
        let disposition = adapter
            .decode(
                DecodeContext {
                    decoder: &DecoderId::new(TASK_ITEM_DECODER).unwrap(),
                    object_context: &object_context,
                },
                &record,
                &mut batch,
            )
            .unwrap();
        assert_eq!(disposition, DecodeDisposition::Applied);
        let Fact::TaskSnapshot(snapshot) = &batch.facts()[0].value else {
            panic!("expected native task snapshot");
        };
        assert_eq!(snapshot.kind, TaskCollectionKind::NativeTaskList);
        assert_eq!(snapshot.coverage, TaskSnapshotCoverage::ItemDocument);
        assert!(snapshot.session.is_none());
        assert!(snapshot.run.is_none());
        assert!(snapshot.team.is_none());
        assert_eq!(snapshot.items.len(), 1);
        let item = &snapshot.items[0];
        assert_eq!(item.native_task_id.as_deref(), Some("1"));
        assert_eq!(item.native_owner.as_deref(), Some("worker"));
        assert_eq!(item.status, TaskStatus::InProgress);
        assert_eq!(item.blocks, ["2"]);
        assert_eq!(item.blocked_by, ["3"]);
        assert!(!fact_values(&batch).any(|fact| matches!(fact, Fact::RunEvidence(_))));

        let mismatch = document_record(
            br#"{"id":"2","subject":"wrong file","description":"","status":"pending"}"#,
        );
        let mut mismatch_batch = FactBatch::new(2, 2).unwrap();
        assert_eq!(
            adapter
                .decode(
                    DecodeContext {
                        decoder: &DecoderId::new(TASK_ITEM_DECODER).unwrap(),
                        object_context: &object_context,
                    },
                    &mismatch,
                    &mut mismatch_batch,
                )
                .unwrap(),
            DecodeDisposition::PreservedUnknown
        );
        assert!(matches!(
            mismatch_batch.facts()[0].value,
            Fact::UnknownRecord { .. }
        ));
    }

    #[test]
    fn plan_document_preserves_markdown_and_falls_back_to_the_native_slug() {
        let root = TempDir::new().unwrap();
        let adapter = ClaudeCodeAdapter::new();
        let object_context = adapter
            .bootstrap_object(
                &instance(root.path()),
                &object(PLAN_STREAM, "plans/ship-it.md"),
            )
            .unwrap();
        let record = document_record(b"preamble\r\n# Ship It\r\n\r\nBody.\r\n");
        let mut batch = FactBatch::new(2, 2).unwrap();
        assert_eq!(
            adapter
                .decode(
                    DecodeContext {
                        decoder: &DecoderId::new(PLAN_DECODER).unwrap(),
                        object_context: &object_context,
                    },
                    &record,
                    &mut batch,
                )
                .unwrap(),
            DecodeDisposition::Applied
        );
        let Fact::PlanSnapshot(plan) = &batch.facts()[0].value else {
            panic!("expected plan snapshot");
        };
        assert_eq!(plan.native_plan_id, "ship-it");
        assert_eq!(plan.title, "Ship It");
        assert_eq!(plan.content.as_bytes(), record.payload);
        assert_eq!(plan.size_bytes, record.payload.len() as u64);

        let headless = document_record(b"No heading here.\n");
        let mut headless_batch = FactBatch::new(2, 2).unwrap();
        adapter
            .decode(
                DecodeContext {
                    decoder: &DecoderId::new(PLAN_DECODER).unwrap(),
                    object_context: &object_context,
                },
                &headless,
                &mut headless_batch,
            )
            .unwrap();
        let Fact::PlanSnapshot(headless_plan) = &headless_batch.facts()[0].value else {
            panic!("expected headless plan snapshot");
        };
        assert_eq!(headless_plan.title, "ship-it");

        let mut invalid_batch = FactBatch::new(2, 2).unwrap();
        assert_eq!(
            adapter
                .decode(
                    DecodeContext {
                        decoder: &DecoderId::new(PLAN_DECODER).unwrap(),
                        object_context: &object_context,
                    },
                    &document_record(&[0xff]),
                    &mut invalid_batch,
                )
                .unwrap(),
            DecodeDisposition::PreservedUnknown
        );
    }

    #[test]
    fn file_history_checkpoint_and_delta_emit_joinable_session_artifacts() {
        let root = TempDir::new().unwrap();
        let adapter = ClaudeCodeAdapter::new();
        let object_context = adapter
            .bootstrap_object(
                &instance(root.path()),
                &object(PARENT_STREAM, &format!("project/{SESSION}.jsonl")),
            )
            .unwrap();
        let checkpoint = record(
            br#"{
              "type":"file-history-snapshot",
              "messageId":"checkpoint-update",
              "isSnapshotUpdate":true,
              "snapshot":{
                "messageId":"checkpoint-root",
                "timestamp":"2026-08-11T20:00:00.000Z",
                "trackedFileBackups":{
                  "src/lib.rs":{
                    "backupFileName":"71f902cd51ee4c6e@v2",
                    "version":2,
                    "backupTime":"2026-08-11T20:01:00.000Z",
                    "realParentDir":"/repo/src"
                  },
                  "src/new.rs":{
                    "backupFileName":null,
                    "version":1,
                    "backupTime":"2026-08-11T20:02:00.000Z",
                    "realParentDir":"/repo/src"
                  }
                }
              }
            }"#,
        );
        let mut batch = FactBatch::new(16, 8).unwrap();
        assert_eq!(
            adapter
                .decode(
                    DecodeContext {
                        decoder: &DecoderId::new(PARENT_DECODER).unwrap(),
                        object_context: &object_context,
                    },
                    &checkpoint,
                    &mut batch,
                )
                .unwrap(),
            DecodeDisposition::Applied
        );
        let metadata = fact_values(&batch)
            .find_map(|fact| match fact {
                Fact::ArtifactMetadataSnapshot(fact) => Some(fact),
                _ => None,
            })
            .expect("checkpoint should emit artifact metadata");
        assert_eq!(
            metadata.observation_kind,
            ArtifactObservationKind::Checkpoint
        );
        assert!(metadata.is_snapshot_update);
        assert_eq!(metadata.native_message_id, "checkpoint-update");
        assert_eq!(metadata.native_snapshot_message_id, "checkpoint-root");
        assert_eq!(
            metadata
                .source_time
                .as_ref()
                .map(|time| time.value.as_str()),
            Some("2026-08-11T20:00:00.000Z")
        );
        assert_eq!(metadata.artifacts.len(), 2);
        let named = metadata
            .artifacts
            .iter()
            .find(|artifact| artifact.tracking_path == "src/lib.rs")
            .unwrap();
        assert_eq!(
            named.native_artifact_id.as_deref(),
            Some("71f902cd51ee4c6e@v2")
        );
        assert_eq!(named.capture, ArtifactCapture::ContentExpected);
        assert_eq!(named.real_parent_dir.as_deref(), Some("/repo/src"));
        assert_eq!(named.version, 2);
        let named_key = named.artifact.clone();
        let unbacked = metadata
            .artifacts
            .iter()
            .find(|artifact| artifact.tracking_path == "src/new.rs")
            .unwrap();
        assert!(unbacked.native_artifact_id.is_none());
        assert_eq!(unbacked.capture, ArtifactCapture::NotCaptured);
        assert_ne!(unbacked.artifact, named_key);

        let delta = record(
            br#"{
              "type":"file-history-delta",
              "messageId":"delta-1",
              "snapshotMessageId":"checkpoint-root",
              "timestamp":"2026-08-11T20:03:00.000Z",
              "trackingPath":"src/lib.rs",
              "backup":{
                "backupFileName":"71f902cd51ee4c6e@v2",
                "version":2,
                "backupTime":"2026-08-11T20:01:00.000Z",
                "realParentDir":"/repo/src"
              }
            }"#,
        );
        let mut delta_batch = FactBatch::new(16, 8).unwrap();
        adapter
            .decode(
                DecodeContext {
                    decoder: &DecoderId::new(PARENT_DECODER).unwrap(),
                    object_context: &object_context,
                },
                &delta,
                &mut delta_batch,
            )
            .unwrap();
        let delta_metadata = fact_values(&delta_batch)
            .find_map(|fact| match fact {
                Fact::ArtifactMetadataSnapshot(fact) => Some(fact),
                _ => None,
            })
            .expect("delta should emit artifact metadata");
        assert_eq!(
            delta_metadata.observation_kind,
            ArtifactObservationKind::Delta
        );
        assert!(!delta_metadata.is_snapshot_update);
        assert_eq!(delta_metadata.native_message_id, "delta-1");
        assert_eq!(delta_metadata.artifacts[0].artifact, named_key);
        assert_eq!(
            delta_metadata
                .source_time
                .as_ref()
                .map(|time| time.value.as_str()),
            Some("2026-08-11T20:03:00.000Z")
        );
        assert!(!fact_values(&delta_batch).any(|fact| matches!(
            fact,
            Fact::Delegation(_) | Fact::DelegationSpawn(_) | Fact::DelegationMetadata(_)
        )));
    }

    #[test]
    fn malformed_file_history_metadata_is_diagnosed_without_losing_the_message() {
        let root = TempDir::new().unwrap();
        let adapter = ClaudeCodeAdapter::new();
        let object_context = adapter
            .bootstrap_object(
                &instance(root.path()),
                &object(PARENT_STREAM, &format!("project/{SESSION}.jsonl")),
            )
            .unwrap();
        let mismatched = record(
            br#"{
              "type":"file-history-delta",
              "messageId":"delta-bad",
              "snapshotMessageId":"checkpoint",
              "trackingPath":"src/lib.rs",
              "backup":{
                "backupFileName":"71f902cd51ee4c6e@v2",
                "version":3,
                "backupTime":"2026-08-11T20:01:00.000Z"
              }
            }"#,
        );
        let mut batch = FactBatch::new(16, 8).unwrap();
        assert_eq!(
            adapter
                .decode(
                    DecodeContext {
                        decoder: &DecoderId::new(PARENT_DECODER).unwrap(),
                        object_context: &object_context,
                    },
                    &mismatched,
                    &mut batch,
                )
                .unwrap(),
            DecodeDisposition::Applied
        );
        assert!(fact_values(&batch).any(|fact| matches!(fact, Fact::Message(_))));
        assert!(!fact_values(&batch).any(|fact| matches!(fact, Fact::ArtifactMetadataSnapshot(_))));
        assert!(batch
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code == "claude_artifact_projection_loss"));

        // Missing is not equivalent to the native format's explicit null:
        // silently accepting it would manufacture positive non-capture.
        let missing_capture_marker = record(
            br#"{
              "type":"file-history-delta",
              "messageId":"delta-missing-capture",
              "snapshotMessageId":"checkpoint",
              "trackingPath":"src/new.rs",
              "backup":{
                "version":1,
                "backupTime":"2026-08-11T20:02:00.000Z"
              }
            }"#,
        );
        let mut missing_batch = FactBatch::new(16, 8).unwrap();
        adapter
            .decode(
                DecodeContext {
                    decoder: &DecoderId::new(PARENT_DECODER).unwrap(),
                    object_context: &object_context,
                },
                &missing_capture_marker,
                &mut missing_batch,
            )
            .unwrap();
        assert!(fact_values(&missing_batch).any(|fact| matches!(fact, Fact::Message(_))));
        assert!(!fact_values(&missing_batch)
            .any(|fact| matches!(fact, Fact::ArtifactMetadataSnapshot(_))));
        assert!(missing_batch
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code == "claude_artifact_projection_loss"));
    }

    #[test]
    fn file_history_blob_preserves_exact_content_and_absence_semantics() {
        let root = TempDir::new().unwrap();
        let adapter = ClaudeCodeAdapter::new();
        let object_context = adapter
            .bootstrap_object(
                &instance(root.path()),
                &object(
                    ARTIFACT_CONTENT_STREAM,
                    &format!("file-history/{SESSION}/71f902cd51ee4c6e@v2"),
                ),
            )
            .unwrap();
        let decoder = DecoderId::new(ARTIFACT_CONTENT_DECODER).unwrap();
        let content = document_record(b"before edit\r\n");
        let mut batch = FactBatch::new(4, 4).unwrap();
        assert_eq!(
            adapter
                .decode(
                    DecodeContext {
                        decoder: &decoder,
                        object_context: &object_context,
                    },
                    &content,
                    &mut batch,
                )
                .unwrap(),
            DecodeDisposition::Applied
        );
        let Fact::ArtifactContent(artifact) = &batch.facts()[0].value else {
            panic!("expected artifact content");
        };
        assert_eq!(
            artifact.session,
            EntityKey::native(adapter.adapter_id(), 7, "session", SESSION.as_bytes()).unwrap()
        );
        assert_eq!(artifact.native_artifact_id, "71f902cd51ee4c6e@v2");
        assert_eq!(artifact.native_file_hash, "71f902cd51ee4c6e");
        assert_eq!(artifact.version, 2);
        assert_eq!(artifact.content, content.payload);
        assert_eq!(artifact.size_bytes, content.payload.len() as u64);
        assert_eq!(
            artifact.artifact,
            artifact_key(
                adapter.adapter_id(),
                7,
                SESSION,
                Some("71f902cd51ee4c6e@v2"),
                None,
                2,
                None,
            )
            .unwrap()
        );

        let mut absent_batch = FactBatch::new(2, 2).unwrap();
        assert_eq!(
            adapter
                .decode(
                    DecodeContext {
                        decoder: &decoder,
                        object_context: &object_context,
                    },
                    &absent_document_record(),
                    &mut absent_batch,
                )
                .unwrap(),
            DecodeDisposition::IgnoredKnown
        );
        assert!(absent_batch.facts().is_empty());

        let mut empty_batch = FactBatch::new(2, 2).unwrap();
        adapter
            .decode(
                DecodeContext {
                    decoder: &decoder,
                    object_context: &object_context,
                },
                &document_record(b""),
                &mut empty_batch,
            )
            .unwrap();
        let Fact::ArtifactContent(empty) = &empty_batch.facts()[0].value else {
            panic!("expected present empty artifact content");
        };
        assert!(empty.content.is_empty());
        assert_eq!(empty.size_bytes, 0);

        let mut binary_batch = FactBatch::new(2, 2).unwrap();
        assert_eq!(
            adapter
                .decode(
                    DecodeContext {
                        decoder: &decoder,
                        object_context: &object_context,
                    },
                    &document_record(&[0xff]),
                    &mut binary_batch,
                )
                .unwrap(),
            DecodeDisposition::Applied
        );
        let Fact::ArtifactContent(binary) = &binary_batch.facts()[0].value else {
            panic!("expected byte-exact binary artifact content");
        };
        assert_eq!(binary.content, [0xff]);
    }

    #[test]
    fn workflow_run_decoder_preserves_native_snapshot_and_normalizes_container_status() {
        let root = TempDir::new().unwrap();
        let adapter = ClaudeCodeAdapter::new();
        let object_context = adapter
            .bootstrap_object(
                &instance(root.path()),
                &object(
                    WORKFLOW_RUN_STREAM,
                    &format!("project/{SESSION}/workflows/wf_main.json"),
                ),
            )
            .unwrap();
        let decoder = DecoderId::new(WORKFLOW_RUN_DECODER).unwrap();
        let payload = br#"{
          "runId":"wf_main",
          "timestamp":"2026-08-11T00:00:01.005Z",
          "taskId":"task-main",
          "script":"await run({ task: 'inspect' });",
          "scriptPath":"/repo/workflows/main.js",
          "args":"--careful",
          "agentCount":1,
          "durationMs":1005,
          "summary":"inspection complete",
          "workflowName":"Inspect",
          "status":"completed",
          "startTime":1786406400000,
          "defaultModel":"claude-sonnet",
          "totalTokens":123,
          "totalToolCalls":4,
          "phases":[{"name":"inspect","status":"completed"}],
          "futureField":{"kept":true}
        }"#;
        let record = document_record(payload);
        let mut batch = FactBatch::new(4, 2).unwrap();
        assert_eq!(
            adapter
                .decode(
                    DecodeContext {
                        decoder: &decoder,
                        object_context: &object_context,
                    },
                    &record,
                    &mut batch,
                )
                .unwrap(),
            DecodeDisposition::Applied
        );
        let Fact::WorkflowSnapshot(workflow) = &batch.facts()[0].value else {
            panic!("expected workflow snapshot");
        };
        assert_eq!(workflow.native_workflow_id, "wf_main");
        assert_eq!(workflow.native_task_id, "task-main");
        assert_eq!(workflow.name, "Inspect");
        assert_eq!(workflow.native_status, "completed");
        assert_eq!(workflow.status, WorkflowStatus::Succeeded);
        assert_eq!(workflow.default_model, "claude-sonnet");
        assert_eq!(workflow.args.as_deref(), Some("--careful"));
        assert_eq!(workflow.agent_count, 1);
        assert_eq!(workflow.duration_ms, 1005);
        assert_eq!(workflow.total_tokens, 123);
        assert_eq!(workflow.total_tool_calls, 4);
        assert_eq!(
            workflow.started_at,
            epoch_millis_timestamp(1_786_406_400_000)
        );
        assert_eq!(workflow.finished_at.value, "2026-08-11T00:00:01.005Z");
        assert_eq!(
            workflow.native_snapshot["futureField"],
            serde_json::json!({"kept": true})
        );
        assert_eq!(
            workflow.workflow,
            workflow_key(
                adapter.adapter_id(),
                7,
                &ClaudeWorkflowContext::decode(&object_context).unwrap()
            )
            .unwrap()
        );

        let mut absent = FactBatch::new(1, 1).unwrap();
        assert_eq!(
            adapter
                .decode(
                    DecodeContext {
                        decoder: &decoder,
                        object_context: &object_context,
                    },
                    &absent_document_record(),
                    &mut absent,
                )
                .unwrap(),
            DecodeDisposition::IgnoredKnown
        );

        let mut mismatched_value = serde_json::from_slice::<serde_json::Value>(payload).unwrap();
        mismatched_value["runId"] = serde_json::json!("wf_other");
        let mismatched_payload = serde_json::to_vec(&mismatched_value).unwrap();
        let mismatched = document_record(&mismatched_payload);
        let mut mismatched_batch = FactBatch::new(2, 2).unwrap();
        assert_eq!(
            adapter
                .decode(
                    DecodeContext {
                        decoder: &decoder,
                        object_context: &object_context,
                    },
                    &mismatched,
                    &mut mismatched_batch,
                )
                .unwrap(),
            DecodeDisposition::PreservedUnknown
        );
        assert!(matches!(
            mismatched_batch.facts()[0].value,
            Fact::UnknownRecord { .. }
        ));
    }

    #[test]
    fn workflow_journal_decoder_correlates_child_runs_and_preserves_contract_loss() {
        let root = TempDir::new().unwrap();
        let adapter = ClaudeCodeAdapter::new();
        let object_context = adapter
            .bootstrap_object(
                &instance(root.path()),
                &object(
                    WORKFLOW_JOURNAL_STREAM,
                    &format!("project/{SESSION}/subagents/workflows/wf_main/journal.jsonl"),
                ),
            )
            .unwrap();
        let decoder = DecoderId::new(WORKFLOW_JOURNAL_DECODER).unwrap();
        let started_record = record(br#"{"type":"started","agentId":"a1","key":"step-1"}"#);
        let mut started_batch = FactBatch::new(2, 2).unwrap();
        adapter
            .decode(
                DecodeContext {
                    decoder: &decoder,
                    object_context: &object_context,
                },
                &started_record,
                &mut started_batch,
            )
            .unwrap();
        let Fact::WorkflowMemberEvent(started) = &started_batch.facts()[0].value else {
            panic!("expected workflow member start");
        };
        assert_eq!(started.native_workflow_id, "wf_main");
        assert_eq!(started.native_agent_id, "a1");
        assert_eq!(started.native_event_key, "step-1");
        assert_eq!(started.kind, WorkflowMemberEventKind::Started);
        assert!(started.result.is_none());
        let child_context = ClaudeTranscriptContext::subagent(Path::new(&format!(
            "project/{SESSION}/subagents/workflows/wf_main/agents/agent-a1.jsonl"
        )))
        .unwrap();
        assert_eq!(
            started.child_run,
            EntityKey::native(
                adapter.adapter_id(),
                7,
                "run",
                child_context.run_native_key().as_bytes(),
            )
            .unwrap()
        );

        let result_record =
            record(br#"{"type":"result","agentId":"a1","key":"step-1","result":{"answer":42}}"#);
        let mut result_batch = FactBatch::new(2, 2).unwrap();
        adapter
            .decode(
                DecodeContext {
                    decoder: &decoder,
                    object_context: &object_context,
                },
                &result_record,
                &mut result_batch,
            )
            .unwrap();
        let Fact::WorkflowMemberEvent(result) = &result_batch.facts()[0].value else {
            panic!("expected workflow member result");
        };
        assert_eq!(result.kind, WorkflowMemberEventKind::Result);
        assert_eq!(result.member, started.member);
        assert_eq!(result.child_run, started.child_run);
        assert_eq!(result.result, Some(serde_json::json!({"answer": 42})));

        for invalid in [
            br#"{"type":"result","agentId":"a1","key":"step-1"}"#.as_slice(),
            br#"{"type":"started","agentId":"a1","key":"step-1","result":null}"#.as_slice(),
            br#"{"type":"future","agentId":"a1","key":"step-1"}"#.as_slice(),
            br#"{"type":"result","agentId":"","key":"step-1","result":"x"}"#.as_slice(),
        ] {
            let mut invalid_batch = FactBatch::new(2, 2).unwrap();
            assert_eq!(
                adapter
                    .decode(
                        DecodeContext {
                            decoder: &decoder,
                            object_context: &object_context,
                        },
                        &record(invalid),
                        &mut invalid_batch,
                    )
                    .unwrap(),
                DecodeDisposition::PreservedUnknown
            );
            assert!(matches!(
                invalid_batch.facts()[0].value,
                Fact::UnknownRecord { .. }
            ));
        }
    }

    #[test]
    fn active_session_presence_preserves_native_fields_without_host_liveness() {
        let root = TempDir::new().unwrap();
        let adapter = ClaudeCodeAdapter::new();
        let object_context = adapter
            .bootstrap_object(
                &instance(root.path()),
                &object(ACTIVE_SESSION_STREAM, "4242.json"),
            )
            .unwrap();
        let record = presence_record(
            br#"{
              "pid":4242,
              "sessionId":"01234567-89ab-cdef-0123-456789abcdef",
              "cwd":"/repo",
              "startedAt":1786468310233,
              "kind":"interactive",
              "entrypoint":"cli",
              "name":"engine work",
              "status":"idle",
              "updatedAt":1786471704949,
              "statusUpdatedAt":1786471704000,
              "procStart":"Tue Aug 11 17:11:48 2026",
              "version":"2.1.227",
              "peerProtocol":1,
              "nameSource":"derived",
              "bridgeSessionId":"bridge-1",
              "messagingSocketPath":"/tmp/claude-4242.sock"
            }"#,
        );
        let mut batch = FactBatch::new(4, 4).unwrap();
        let disposition = adapter
            .decode(
                DecodeContext {
                    decoder: &DecoderId::new(ACTIVE_SESSION_DECODER).unwrap(),
                    object_context: &object_context,
                },
                &record,
                &mut batch,
            )
            .unwrap();
        assert_eq!(disposition, DecodeDisposition::Applied);
        assert_eq!(batch.facts().len(), 1);
        let Fact::Presence(presence) = &batch.facts()[0].value else {
            panic!("expected active-session presence fact");
        };
        assert_eq!(presence.native_pid, 4242);
        assert_eq!(presence.native_session_id, SESSION);
        assert_eq!(presence.cwd, "/repo");
        assert_eq!(presence.started_at.quality, TimestampQuality::NativeExact);
        assert_eq!(presence.native_kind.as_deref(), Some("interactive"));
        assert_eq!(presence.entrypoint.as_deref(), Some("cli"));
        assert_eq!(presence.name.as_deref(), Some("engine work"));
        assert_eq!(presence.native_status.as_deref(), Some("idle"));
        assert_eq!(
            presence.native_process_started_at.as_deref(),
            Some("Tue Aug 11 17:11:48 2026")
        );
        assert_eq!(presence.version.as_deref(), Some("2.1.227"));
        assert_eq!(presence.peer_protocol, Some(1));
        assert_eq!(presence.name_source.as_deref(), Some("derived"));
        assert_eq!(presence.bridge_session_id.as_deref(), Some("bridge-1"));
        assert_eq!(
            presence.messaging_socket_path.as_deref(),
            Some("/tmp/claude-4242.sock")
        );
        assert!(!fact_values(&batch).any(|fact| matches!(fact, Fact::RunEvidence(_))));

        let changed = presence_record(
            br#"{
              "pid":4242,
              "sessionId":"01234567-89ab-cdef-0123-456789abcdef",
              "cwd":"/repo",
              "startedAt":1786468310233,
              "status":"working",
              "updatedAt":1786471800000,
              "procStart":"Tue Aug 11 17:11:48 2026"
            }"#,
        );
        let mut changed_batch = FactBatch::new(4, 4).unwrap();
        adapter
            .decode(
                DecodeContext {
                    decoder: &DecoderId::new(ACTIVE_SESSION_DECODER).unwrap(),
                    object_context: &object_context,
                },
                &changed,
                &mut changed_batch,
            )
            .unwrap();
        let Fact::Presence(changed_presence) = &changed_batch.facts()[0].value else {
            panic!("expected updated presence fact");
        };
        assert_eq!(changed_presence.presence, presence.presence);
    }

    #[test]
    fn active_session_absence_and_invalid_present_content_stay_distinct() {
        let root = TempDir::new().unwrap();
        let adapter = ClaudeCodeAdapter::new();
        let object_context = adapter
            .bootstrap_object(
                &instance(root.path()),
                &object(ACTIVE_SESSION_STREAM, "4242.json"),
            )
            .unwrap();
        let decoder = DecoderId::new(ACTIVE_SESSION_DECODER).unwrap();

        let mut absent_batch = FactBatch::new(2, 2).unwrap();
        let absent = adapter
            .decode(
                DecodeContext {
                    decoder: &decoder,
                    object_context: &object_context,
                },
                &absent_presence_record(),
                &mut absent_batch,
            )
            .unwrap();
        assert_eq!(absent, DecodeDisposition::IgnoredKnown);
        assert!(absent_batch.facts().is_empty());

        let mut empty_batch = FactBatch::new(2, 2).unwrap();
        let empty = adapter
            .decode(
                DecodeContext {
                    decoder: &decoder,
                    object_context: &object_context,
                },
                &presence_record(b""),
                &mut empty_batch,
            )
            .unwrap();
        assert_eq!(empty, DecodeDisposition::PreservedUnknown);
        assert!(matches!(
            empty_batch.facts()[0].value,
            Fact::UnknownRecord { .. }
        ));

        let mismatch_record =
            presence_record(br#"{"pid":9,"sessionId":"session","cwd":"/repo","startedAt":1}"#);
        let mut mismatch_batch = FactBatch::new(2, 2).unwrap();
        let mismatch = adapter
            .decode(
                DecodeContext {
                    decoder: &decoder,
                    object_context: &object_context,
                },
                &mismatch_record,
                &mut mismatch_batch,
            )
            .unwrap();
        assert_eq!(mismatch, DecodeDisposition::PreservedUnknown);
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
