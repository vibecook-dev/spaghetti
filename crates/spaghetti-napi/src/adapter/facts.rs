use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::source::SourceRecord;

use super::{AdapterDiagnostic, AdapterError, AdapterId};

const FACT_HASH_BYTES: usize = 32;
const MAX_ENTITY_KEY_BYTES: usize = 8 * 1024;

mod base64_bytes {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(value: &[u8], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&STANDARD.encode(value))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        STANDARD.decode(value).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EntityKey(Vec<u8>);

impl EntityKey {
    pub fn native(
        adapter_id: &AdapterId,
        source_instance_id: u64,
        entity_kind: &str,
        native_key: &[u8],
    ) -> Result<Self, AdapterError> {
        if entity_kind.trim().is_empty() || native_key.is_empty() {
            return Err(AdapterError::invalid_contract(
                "entity kind and native key must not be empty",
            ));
        }
        let mut bytes = Vec::with_capacity(
            adapter_id.as_str().len() + entity_kind.len() + native_key.len() + 32,
        );
        push_component(&mut bytes, adapter_id.as_str().as_bytes());
        bytes.extend_from_slice(&source_instance_id.to_be_bytes());
        push_component(&mut bytes, entity_kind.as_bytes());
        push_component(&mut bytes, native_key);
        if bytes.len() > MAX_ENTITY_KEY_BYTES {
            return Err(AdapterError::invalid_contract(format!(
                "entity key exceeds {MAX_ENTITY_KEY_BYTES} bytes"
            )));
        }
        Ok(Self(bytes))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for EntityKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("EntityKey")
            .field(&blake3::hash(&self.0).to_hex().as_str())
            .finish()
    }
}

fn push_component(output: &mut Vec<u8>, value: &[u8]) {
    output.extend_from_slice(&(value.len() as u64).to_be_bytes());
    output.extend_from_slice(value);
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct FactId([u8; FACT_HASH_BYTES]);

impl FactId {
    pub fn as_bytes(&self) -> &[u8; FACT_HASH_BYTES] {
        &self.0
    }
}

impl fmt::Debug for FactId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("FactId")
            .field(&blake3::Hash::from_bytes(self.0).to_hex().as_str())
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactProvenance {
    pub source_instance_id: u64,
    pub stream_id: u64,
    pub object_id: u64,
    pub generation: u64,
    pub cursor_start: Vec<u8>,
    pub cursor_end: Vec<u8>,
    pub record_hash: [u8; 32],
    pub local_fact_ordinal: u32,
    pub observed_at: i64,
}

impl FactProvenance {
    fn from_record(record: &SourceRecord, local_fact_ordinal: u32) -> Self {
        Self {
            source_instance_id: record.source_instance_id,
            stream_id: record.stream_id,
            object_id: record.object_id,
            generation: record.generation,
            cursor_start: record.cursor_start.as_bytes().to_vec(),
            cursor_end: record.cursor_end.as_bytes().to_vec(),
            record_hash: *record.payload_hash.as_bytes(),
            local_fact_ordinal,
            observed_at: record.observed_at,
        }
    }

    fn fact_id(&self, kind: &str) -> FactId {
        let mut hasher = blake3::Hasher::new();
        hasher.update(&self.object_id.to_be_bytes());
        hasher.update(&self.generation.to_be_bytes());
        hasher.update(&(self.cursor_start.len() as u64).to_be_bytes());
        hasher.update(&self.cursor_start);
        hasher.update(&(self.cursor_end.len() as u64).to_be_bytes());
        hasher.update(&self.cursor_end);
        hasher.update(&self.record_hash);
        hasher.update(kind.as_bytes());
        hasher.update(&self.local_fact_ordinal.to_be_bytes());
        FactId(*hasher.finalize().as_bytes())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimestampQuality {
    NativeExact,
    NativeApproximate,
    FileMetadataFallback,
    Derived,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QualifiedTimestamp {
    pub value: String,
    pub quality: TimestampQuality,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionFact {
    pub session: EntityKey,
    pub project: EntityKey,
    pub native_session_id: String,
    pub native_project_key: String,
    pub cwd: Option<String>,
    pub git_branch: Option<String>,
    pub first_prompt: Option<String>,
    pub ai_title: Option<String>,
    pub custom_title: Option<String>,
    pub source_time: Option<QualifiedTimestamp>,
}

/// One native entry from a project-level `sessions-index.json` snapshot.
/// The entry is metadata about a possible transcript, not proof that the
/// transcript object or any message history is currently available.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionIndexEntrySnapshot {
    pub session: EntityKey,
    pub native_session_id: String,
    pub full_path: String,
    pub file_mtime_ms: u64,
    pub first_prompt: String,
    pub summary: Option<String>,
    pub message_count: u64,
    pub created_at: QualifiedTimestamp,
    pub modified_at: QualifiedTimestamp,
    pub git_branch: String,
    pub project_path: String,
    pub is_sidechain: bool,
}

/// One complete, replaceable project session-index document. The original
/// native snapshot is retained for forward-compatible inspection while the
/// normalized entries support deterministic joins and conflict reduction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionIndexSnapshotFact {
    pub project: EntityKey,
    pub native_project_key: String,
    pub native_version: u64,
    pub original_path: Option<String>,
    pub entries: Vec<SessionIndexEntrySnapshot>,
    pub native_snapshot: Value,
}

/// One independently replaceable project-memory document. An adapter may mark
/// a native index document, while links and other embedded references remain
/// content rather than asserted entity relationships.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectMemoryDocumentFact {
    pub document: EntityKey,
    pub project: EntityKey,
    pub native_project_key: String,
    pub native_document_path: String,
    pub title: String,
    pub content: String,
    pub size_bytes: u64,
    pub is_index: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageRole {
    User,
    Assistant,
    System,
    Summary,
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Thinking {
        text: String,
        redacted: bool,
    },
    ToolCall {
        native_id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        native_call_id: String,
        content: Value,
        is_error: bool,
    },
    Image {
        media_type: String,
        data_hash: [u8; 32],
    },
    Document {
        media_type: String,
        data_hash: [u8; 32],
    },
    Native {
        native_kind: String,
        value: Value,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageFact {
    pub message: EntityKey,
    pub session: EntityKey,
    pub native_message_id: Option<String>,
    pub native_kind: String,
    pub role: MessageRole,
    pub content: Vec<ContentBlock>,
    pub source_time: Option<QualifiedTimestamp>,
    pub parent_native_message_id: Option<String>,
    pub model: Option<String>,
    pub search_text: Option<String>,
    pub raw_json: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunFact {
    pub run: EntityKey,
    pub session: EntityKey,
    pub native_run_id: String,
    pub parent_run: Option<EntityKey>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DelegationKind {
    VendorNativeSubagent,
    ForkedConversation,
    ChildProcess,
    Other(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RelationStrength {
    Layout,
    NativeIndirect,
    NativeExplicit,
}

/// One independently replayable assertion that a run is delegated from a
/// parent. The referenced runs need not have arrived yet; the common reducer
/// resolves them later without dropping child activity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegationFact {
    pub child_run: EntityKey,
    pub parent_run: Option<EntityKey>,
    pub session: EntityKey,
    pub kind: DelegationKind,
    pub relation_strength: RelationStrength,
    pub native_child_id: Option<String>,
    pub native_task_id: Option<String>,
    pub label: Option<String>,
    pub prompt: Option<String>,
    pub cwd: Option<String>,
    pub worktree_path: Option<String>,
    pub source_time: Option<QualifiedTimestamp>,
}

/// One replaceable native metadata snapshot for a delegated child run. This
/// intentionally carries no parent relation: a sidecar can enrich a child
/// without upgrading layout-derived lineage to native evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegationMetadataFact {
    pub child_run: EntityKey,
    pub session: EntityKey,
    pub native_child_id: String,
    pub agent_type: String,
    pub description: Option<String>,
    pub name: Option<String>,
    pub spawn_depth: Option<u32>,
    pub worktree_path: Option<String>,
    pub native_task_id: Option<String>,
}

/// One native tool invocation that may spawn a delegated child. The child can
/// arrive later through a metadata or result stream; the common reducer joins
/// the stable native task id without assigning cross-object callback order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegationSpawnFact {
    pub spawn: EntityKey,
    pub parent_run: EntityKey,
    pub parent_message: EntityKey,
    pub session: EntityKey,
    pub native_task_id: String,
    pub tool_name: String,
    pub label: Option<String>,
    pub prompt: Option<String>,
    pub requested_agent_type: Option<String>,
    pub source_time: Option<QualifiedTimestamp>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamMemberSnapshot {
    pub member: EntityKey,
    pub native_agent_id: String,
    pub native_name: String,
    pub agent_type: Option<String>,
    pub model: Option<String>,
    pub prompt: Option<String>,
    pub color: Option<String>,
    pub plan_mode_required: Option<bool>,
    pub joined_at: QualifiedTimestamp,
    pub tmux_pane_id: String,
    pub cwd: String,
    pub subscriptions: Vec<String>,
    pub backend_type: Option<String>,
}

/// One authoritative team configuration snapshot. Membership proves only
/// configuration membership; runtime activity must come from separate run or
/// presence evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamSnapshotFact {
    pub team: EntityKey,
    pub native_team_id: String,
    pub name: String,
    pub description: Option<String>,
    pub created_at: QualifiedTimestamp,
    pub lead_member: Option<EntityKey>,
    pub native_lead_agent_id: String,
    pub lead_session: EntityKey,
    pub native_lead_session_id: String,
    pub members: Vec<TeamMemberSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamInboxMessageSnapshot {
    pub message: EntityKey,
    pub sender: EntityKey,
    pub native_message_id: Option<String>,
    pub native_kind: Option<String>,
    pub native_version: Option<u32>,
    pub native_sender_name: String,
    pub text: String,
    pub summary: Option<String>,
    pub color: Option<String>,
    pub source_time: QualifiedTimestamp,
    pub read: bool,
}

/// One whole-file team inbox snapshot. Missing messages in a replacement are
/// retracted; a `read` change updates the stable message rather than creating
/// a second message identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamInboxSnapshotFact {
    pub inbox: EntityKey,
    pub team: EntityKey,
    pub recipient: EntityKey,
    pub native_team_id: String,
    pub native_recipient_name: String,
    pub messages: Vec<TeamInboxMessageSnapshot>,
}

/// One currently materialized native presence object. The fact proves that
/// the agent-owned registry entry exists; it does not prove that the host PID
/// is still alive or turn silence into completion evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresenceFact {
    pub presence: EntityKey,
    pub session: EntityKey,
    pub run: EntityKey,
    pub native_session_id: String,
    pub native_pid: u32,
    pub cwd: String,
    pub started_at: QualifiedTimestamp,
    pub native_kind: Option<String>,
    pub entrypoint: Option<String>,
    pub name: Option<String>,
    pub native_status: Option<String>,
    pub updated_at: Option<QualifiedTimestamp>,
    pub status_updated_at: Option<QualifiedTimestamp>,
    pub native_process_started_at: Option<String>,
    pub version: Option<String>,
    pub peer_protocol: Option<u32>,
    pub name_source: Option<String>,
    pub bridge_session_id: Option<String>,
    pub messaging_socket_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskCollectionKind {
    TodoList,
    NativeTaskList,
    Other(String),
}

/// Whether one source document describes the complete collection or one
/// independently replaceable item within it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskSnapshotCoverage {
    Complete,
    ItemDocument,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskStatus {
    Pending,
    InProgress,
    Completed,
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskItemSnapshot {
    pub task: EntityKey,
    pub native_task_id: Option<String>,
    pub subject: String,
    pub description: Option<String>,
    pub active_form: Option<String>,
    pub native_owner: Option<String>,
    pub status: TaskStatus,
    pub blocks: Vec<String>,
    pub blocked_by: Vec<String>,
}

/// One replaceable task-bearing document. A complete snapshot retracts items
/// missing from its replacement; an item document retracts only that native
/// item when the document disappears. Optional scope relations are asserted
/// only when the native layout makes them unambiguous.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSnapshotFact {
    pub collection: EntityKey,
    pub session: Option<EntityKey>,
    pub run: Option<EntityKey>,
    pub team: Option<EntityKey>,
    pub native_collection_id: String,
    pub native_owner_id: Option<String>,
    pub kind: TaskCollectionKind,
    pub coverage: TaskSnapshotCoverage,
    pub items: Vec<TaskItemSnapshot>,
}

/// One replaceable plan document. Plans remain independently queryable when
/// no transcript has yet supplied a trustworthy session relation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanSnapshotFact {
    pub plan: EntityKey,
    pub native_plan_id: String,
    pub title: String,
    pub content: String,
    pub size_bytes: u64,
    pub source_time: Option<QualifiedTimestamp>,
}

/// Whether native artifact metadata came from a complete checkpoint record or
/// an incremental update to an earlier checkpoint. Both are historical
/// observations: `Checkpoint` does not retract artifacts from older records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArtifactObservationKind {
    Checkpoint,
    Delta,
}

/// Whether the native record names a content blob. A `NotCaptured` artifact
/// is positive native evidence that a path was newly created without a backup;
/// it is distinct from a named blob that has not arrived yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArtifactCapture {
    ContentExpected,
    NotCaptured,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactMetadataEntry {
    pub artifact: EntityKey,
    pub native_artifact_id: Option<String>,
    pub tracking_path: String,
    pub real_parent_dir: Option<String>,
    pub version: u64,
    pub backup_time: QualifiedTimestamp,
    pub capture: ArtifactCapture,
}

/// Artifact metadata carried by one transcript checkpoint or delta. The fact
/// is session-scoped and deliberately does not assert that a run produced the
/// tracked file. Content may arrive independently through an artifact blob
/// stream and is joined by the stable native backup name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactMetadataSnapshotFact {
    pub session: EntityKey,
    pub native_message_id: String,
    pub native_snapshot_message_id: String,
    pub observation_kind: ArtifactObservationKind,
    pub is_snapshot_update: bool,
    pub source_time: Option<QualifiedTimestamp>,
    pub artifacts: Vec<ArtifactMetadataEntry>,
}

/// One independently replaceable native artifact-content blob. Its path
/// supplies session, native hash, and version but not a tracked path or run;
/// those relations remain pending until transcript metadata arrives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactContentFact {
    pub artifact: EntityKey,
    pub session: EntityKey,
    pub native_artifact_id: String,
    pub native_file_hash: String,
    pub version: u64,
    #[serde(with = "base64_bytes")]
    pub content: Vec<u8>,
    pub size_bytes: u64,
}

/// Normalized workflow-container status. This status applies only to the
/// workflow orchestration record; it is never copied onto member child runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkflowStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Other(String),
}

/// One independently replaceable native workflow run summary. The full
/// snapshot remains available for forward-compatible query while the common
/// fields provide stable indexing and conflict reduction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowSnapshotFact {
    pub workflow: EntityKey,
    pub session: EntityKey,
    pub project: EntityKey,
    pub native_workflow_id: String,
    pub native_task_id: String,
    pub name: String,
    pub native_status: String,
    pub status: WorkflowStatus,
    pub default_model: String,
    pub script: String,
    pub script_path: String,
    pub args: Option<String>,
    pub summary: String,
    pub error: Option<String>,
    pub started_at: QualifiedTimestamp,
    pub finished_at: QualifiedTimestamp,
    pub duration_ms: u64,
    pub agent_count: u64,
    pub total_tokens: u64,
    pub total_tool_calls: u64,
    pub native_snapshot: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkflowMemberEventKind {
    Started,
    Result,
}

/// One append-only workflow journal event. It proves membership and native
/// start/result observation, but a result does not by itself classify the
/// child run as succeeded or failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowMemberEventFact {
    pub workflow: EntityKey,
    pub member: EntityKey,
    pub child_run: EntityKey,
    pub session: EntityKey,
    pub project: EntityKey,
    pub native_workflow_id: String,
    pub native_agent_id: String,
    pub native_event_key: String,
    pub kind: WorkflowMemberEventKind,
    pub result: Option<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EvidenceKind {
    RunDeclared,
    RunStarted,
    ActivityObserved,
    WaitingObserved,
    InputRequested,
    TerminalSucceeded,
    TerminalFailed,
    TerminalCancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EvidenceStrength {
    Layout,
    Presence,
    NativeActivity,
    NativeExplicit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunEvidenceFact {
    pub run: EntityKey,
    pub kind: EvidenceKind,
    pub strength: EvidenceStrength,
    pub native_state: Option<String>,
    pub source_time: Option<QualifiedTimestamp>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
}

impl TokenUsage {
    pub fn is_zero(self) -> bool {
        self.input_tokens == 0
            && self.output_tokens == 0
            && self.cache_creation_tokens == 0
            && self.cache_read_tokens == 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UsageScope {
    Record,
    Message,
    Turn,
    Run,
    Session,
    Team,
    Project,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UsageAccounting {
    Delta,
    Cumulative,
    Snapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ValueQuality {
    NativeExact,
    NativeApproximate,
    DerivedExact,
    Estimated,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageFact {
    pub subject: EntityKey,
    pub session: EntityKey,
    pub scope: UsageScope,
    pub accounting: UsageAccounting,
    pub quality: ValueQuality,
    pub values: TokenUsage,
    pub model: Option<String>,
    pub source_time: Option<QualifiedTimestamp>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Fact {
    Session(SessionFact),
    SessionIndexSnapshot(SessionIndexSnapshotFact),
    ProjectMemoryDocument(ProjectMemoryDocumentFact),
    Message(MessageFact),
    Run(RunFact),
    Delegation(DelegationFact),
    DelegationMetadata(DelegationMetadataFact),
    DelegationSpawn(DelegationSpawnFact),
    TeamSnapshot(TeamSnapshotFact),
    TeamInboxSnapshot(TeamInboxSnapshotFact),
    Presence(PresenceFact),
    TaskSnapshot(TaskSnapshotFact),
    PlanSnapshot(PlanSnapshotFact),
    ArtifactMetadataSnapshot(ArtifactMetadataSnapshotFact),
    ArtifactContent(ArtifactContentFact),
    WorkflowSnapshot(WorkflowSnapshotFact),
    WorkflowMemberEvent(WorkflowMemberEventFact),
    RunEvidence(RunEvidenceFact),
    Usage(UsageFact),
    UnknownRecord {
        native_kind: Option<String>,
        raw_payload: Vec<u8>,
        reason: String,
    },
}

impl Fact {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Session(_) => "session",
            Self::SessionIndexSnapshot(_) => "session_index_snapshot",
            Self::ProjectMemoryDocument(_) => "project_memory_document",
            Self::Message(_) => "message",
            Self::Run(_) => "run",
            Self::Delegation(_) => "delegation",
            Self::DelegationMetadata(_) => "delegation_metadata",
            Self::DelegationSpawn(_) => "delegation_spawn",
            Self::TeamSnapshot(_) => "team_snapshot",
            Self::TeamInboxSnapshot(_) => "team_inbox_snapshot",
            Self::Presence(_) => "presence",
            Self::TaskSnapshot(_) => "task_snapshot",
            Self::PlanSnapshot(_) => "plan_snapshot",
            Self::ArtifactMetadataSnapshot(_) => "artifact_metadata_snapshot",
            Self::ArtifactContent(_) => "artifact_content",
            Self::WorkflowSnapshot(_) => "workflow_snapshot",
            Self::WorkflowMemberEvent(_) => "workflow_member_event",
            Self::RunEvidence(_) => "run_evidence",
            Self::Usage(_) => "usage",
            Self::UnknownRecord { .. } => "unknown_record",
        }
    }

    pub fn entity_key(&self) -> Option<&EntityKey> {
        match self {
            Self::Session(fact) => Some(&fact.session),
            Self::SessionIndexSnapshot(fact) => Some(&fact.project),
            Self::ProjectMemoryDocument(fact) => Some(&fact.document),
            Self::Message(fact) => Some(&fact.message),
            Self::Run(fact) => Some(&fact.run),
            Self::Delegation(fact) => Some(&fact.child_run),
            Self::DelegationMetadata(fact) => Some(&fact.child_run),
            Self::DelegationSpawn(fact) => Some(&fact.spawn),
            Self::TeamSnapshot(fact) => Some(&fact.team),
            Self::TeamInboxSnapshot(fact) => Some(&fact.inbox),
            Self::Presence(fact) => Some(&fact.presence),
            Self::TaskSnapshot(fact) => Some(&fact.collection),
            Self::PlanSnapshot(fact) => Some(&fact.plan),
            Self::ArtifactMetadataSnapshot(fact) => Some(&fact.session),
            Self::ArtifactContent(fact) => Some(&fact.artifact),
            Self::WorkflowSnapshot(fact) => Some(&fact.workflow),
            Self::WorkflowMemberEvent(fact) => Some(&fact.member),
            Self::RunEvidence(fact) => Some(&fact.run),
            Self::Usage(fact) => Some(&fact.subject),
            Self::UnknownRecord { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FactEnvelope {
    pub id: FactId,
    pub provenance: FactProvenance,
    pub value: Fact,
}

pub struct FactBatch {
    max_facts: usize,
    max_diagnostics: usize,
    facts: Vec<FactEnvelope>,
    diagnostics: Vec<AdapterDiagnostic>,
    next_decoder_state: Option<Vec<u8>>,
    next_record_ordinals: BTreeMap<RecordFactKey, u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct RecordFactKey {
    object_id: u64,
    generation: u64,
    cursor_start: Vec<u8>,
    cursor_end: Vec<u8>,
    record_hash: [u8; 32],
}

impl RecordFactKey {
    fn from_record(record: &SourceRecord) -> Self {
        Self {
            object_id: record.object_id,
            generation: record.generation,
            cursor_start: record.cursor_start.as_bytes().to_vec(),
            cursor_end: record.cursor_end.as_bytes().to_vec(),
            record_hash: *record.payload_hash.as_bytes(),
        }
    }
}

impl FactBatch {
    pub const MAX_DECODER_STATE_BYTES: usize = 64 * 1024;

    pub fn new(max_facts: usize, max_diagnostics: usize) -> Result<Self, AdapterError> {
        if max_facts == 0 || max_diagnostics == 0 {
            return Err(AdapterError::invalid_contract(
                "fact and diagnostic bounds must be greater than zero",
            ));
        }
        Ok(Self {
            max_facts,
            max_diagnostics,
            facts: Vec::new(),
            diagnostics: Vec::new(),
            next_decoder_state: None,
            next_record_ordinals: BTreeMap::new(),
        })
    }

    pub fn push(&mut self, record: &SourceRecord, value: Fact) -> Result<FactId, AdapterError> {
        if self.facts.len() == self.max_facts {
            return Err(AdapterError::invalid_contract(format!(
                "fact batch exceeds {} facts",
                self.max_facts
            )));
        }
        let ordinal = self
            .next_record_ordinals
            .entry(RecordFactKey::from_record(record))
            .or_insert(0);
        let local_fact_ordinal = *ordinal;
        *ordinal = ordinal.checked_add(1).ok_or_else(|| {
            AdapterError::invalid_contract("source record exceeds provenance ordinal range")
        })?;
        let provenance = FactProvenance::from_record(record, local_fact_ordinal);
        let id = provenance.fact_id(value.kind());
        self.facts.push(FactEnvelope {
            id,
            provenance,
            value,
        });
        Ok(id)
    }

    pub fn push_diagnostic(&mut self, diagnostic: AdapterDiagnostic) -> Result<(), AdapterError> {
        if self.diagnostics.len() == self.max_diagnostics {
            return Err(AdapterError::invalid_contract(format!(
                "fact batch exceeds {} diagnostics",
                self.max_diagnostics
            )));
        }
        self.diagnostics.push(diagnostic);
        Ok(())
    }

    pub fn set_next_decoder_state(&mut self, state: Vec<u8>) -> Result<(), AdapterError> {
        if state.len() > Self::MAX_DECODER_STATE_BYTES {
            return Err(AdapterError::invalid_contract(format!(
                "decoder state exceeds {} bytes",
                Self::MAX_DECODER_STATE_BYTES
            )));
        }
        self.next_decoder_state = Some(state);
        Ok(())
    }

    pub fn facts(&self) -> &[FactEnvelope] {
        &self.facts
    }

    pub fn diagnostics(&self) -> &[AdapterDiagnostic] {
        &self.diagnostics
    }

    pub fn next_decoder_state(&self) -> Option<&[u8]> {
        self.next_decoder_state.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use crate::source::{RecordOrigin, SourceCursor, SourceMediaType, SourceRecord};

    use super::*;

    fn record() -> SourceRecord {
        SourceRecord::new(
            &RecordOrigin {
                source_instance_id: 1,
                stream_id: 2,
                object_id: 3,
                observed_at: 4,
                source_timestamp_hint: None,
                media_type: SourceMediaType::new("application/json").unwrap(),
            },
            1,
            SourceCursor::append_offset(0),
            SourceCursor::append_offset(3),
            0,
            b"{}".to_vec(),
        )
    }

    #[test]
    fn fact_identity_is_deterministic_and_local_ordinal_distinguishes_facts() {
        let record = record();
        let mut first = FactBatch::new(4, 4).unwrap();
        first
            .push(
                &record,
                Fact::UnknownRecord {
                    native_kind: None,
                    raw_payload: b"{}".to_vec(),
                    reason: "test".to_string(),
                },
            )
            .unwrap();
        first
            .push(
                &record,
                Fact::UnknownRecord {
                    native_kind: None,
                    raw_payload: b"{}".to_vec(),
                    reason: "test".to_string(),
                },
            )
            .unwrap();
        let mut replay = FactBatch::new(4, 4).unwrap();
        replay
            .push(
                &record,
                Fact::UnknownRecord {
                    native_kind: None,
                    raw_payload: b"{}".to_vec(),
                    reason: "test".to_string(),
                },
            )
            .unwrap();
        assert_eq!(first.facts()[0].id, replay.facts()[0].id);
        assert_ne!(first.facts()[0].id, first.facts()[1].id);
    }

    #[test]
    fn fact_identity_does_not_depend_on_driver_batch_shape() {
        let first_record = record();
        let second_record = SourceRecord::new(
            &RecordOrigin {
                source_instance_id: 1,
                stream_id: 2,
                object_id: 3,
                observed_at: 5,
                source_timestamp_hint: None,
                media_type: SourceMediaType::new("application/json").unwrap(),
            },
            1,
            SourceCursor::append_offset(3),
            SourceCursor::append_offset(6),
            1,
            b"[]".to_vec(),
        );
        let unknown = || Fact::UnknownRecord {
            native_kind: None,
            raw_payload: Vec::new(),
            reason: "test".to_string(),
        };

        let mut grouped = FactBatch::new(4, 4).unwrap();
        grouped.push(&first_record, unknown()).unwrap();
        let grouped_second = grouped.push(&second_record, unknown()).unwrap();
        let mut isolated = FactBatch::new(4, 4).unwrap();
        let isolated_second = isolated.push(&second_record, unknown()).unwrap();

        assert_eq!(grouped_second, isolated_second);
        assert_eq!(grouped.facts()[1].provenance.local_fact_ordinal, 0);
    }

    #[test]
    fn fact_and_decoder_state_bounds_are_enforced() {
        let record = record();
        let mut batch = FactBatch::new(1, 1).unwrap();
        batch
            .push(
                &record,
                Fact::UnknownRecord {
                    native_kind: None,
                    raw_payload: Vec::new(),
                    reason: "test".to_string(),
                },
            )
            .unwrap();
        assert!(batch
            .push(
                &record,
                Fact::UnknownRecord {
                    native_kind: None,
                    raw_payload: Vec::new(),
                    reason: "overflow".to_string(),
                }
            )
            .is_err());
        assert!(batch
            .set_next_decoder_state(vec![0; FactBatch::MAX_DECODER_STATE_BYTES + 1])
            .is_err());
    }

    #[test]
    fn artifact_content_json_round_trip_preserves_arbitrary_bytes() {
        let adapter_id = AdapterId::new("test-adapter").unwrap();
        let artifact = EntityKey::native(&adapter_id, 1, "artifact", b"backup@v1").unwrap();
        let session = EntityKey::native(&adapter_id, 1, "session", b"session-1").unwrap();
        let fact = Fact::ArtifactContent(ArtifactContentFact {
            artifact,
            session,
            native_artifact_id: "backup@v1".to_string(),
            native_file_hash: "backup".to_string(),
            version: 1,
            content: vec![0, 0xff, b'\n'],
            size_bytes: 3,
        });

        let encoded = serde_json::to_value(&fact).unwrap();
        assert_eq!(encoded["ArtifactContent"]["content"], "AP8K");
        assert_eq!(serde_json::from_value::<Fact>(encoded).unwrap(), fact);
    }
}
