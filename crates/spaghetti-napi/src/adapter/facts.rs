use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::source::SourceRecord;

use super::{AdapterDiagnostic, AdapterError, AdapterId};

const FACT_HASH_BYTES: usize = 32;
const MAX_ENTITY_KEY_BYTES: usize = 8 * 1024;

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
    Message(MessageFact),
    Run(RunFact),
    Delegation(DelegationFact),
    DelegationMetadata(DelegationMetadataFact),
    DelegationSpawn(DelegationSpawnFact),
    TeamSnapshot(TeamSnapshotFact),
    TeamInboxSnapshot(TeamInboxSnapshotFact),
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
            Self::Message(_) => "message",
            Self::Run(_) => "run",
            Self::Delegation(_) => "delegation",
            Self::DelegationMetadata(_) => "delegation_metadata",
            Self::DelegationSpawn(_) => "delegation_spawn",
            Self::TeamSnapshot(_) => "team_snapshot",
            Self::TeamInboxSnapshot(_) => "team_inbox_snapshot",
            Self::RunEvidence(_) => "run_evidence",
            Self::Usage(_) => "usage",
            Self::UnknownRecord { .. } => "unknown_record",
        }
    }

    pub fn entity_key(&self) -> Option<&EntityKey> {
        match self {
            Self::Session(fact) => Some(&fact.session),
            Self::Message(fact) => Some(&fact.message),
            Self::Run(fact) => Some(&fact.run),
            Self::Delegation(fact) => Some(&fact.child_run),
            Self::DelegationMetadata(fact) => Some(&fact.child_run),
            Self::DelegationSpawn(fact) => Some(&fact.spawn),
            Self::TeamSnapshot(fact) => Some(&fact.team),
            Self::TeamInboxSnapshot(fact) => Some(&fact.inbox),
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
}
