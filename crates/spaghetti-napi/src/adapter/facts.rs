use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::source::SourceRecord;

use super::{
    AdapterDiagnostic, AdapterError, AdapterId, CanonicalEntityKey, CanonicalFactId,
    CanonicalSourceInstanceKey, CapabilityId, ContractCompleteness, DependencyRevision,
    FactRevisionId, QualifiedUnknownReason, QualifiedValue, QualifiedValueQuality,
    SemanticRevisionRef, SourceRecordId,
};

const FACT_HASH_BYTES: usize = 32;
const MAX_ENTITY_KEY_BYTES: usize = 8 * 1024;
const MAX_USAGE_RESPONSE_KEY_BYTES: usize = 8 * 1024;
const MAX_USAGE_PROVENANCE_FIELD_BYTES: usize = 256;
const MAX_RUNTIME_SEMANTIC_TEXT_BYTES: usize = 8 * 1024;
const MAX_CANONICAL_ARTIFACTS_PER_FACT: usize = 4 * 1024;

mod base64_bytes {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    use serde::{Deserialize, Deserializer, Serializer};

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum EncodedBytes {
        Base64(String),
        LegacyArray(Vec<u8>),
    }

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
        match EncodedBytes::deserialize(deserializer)? {
            EncodedBytes::Base64(value) => STANDARD.decode(value).map_err(serde::de::Error::custom),
            EncodedBytes::LegacyArray(value) => Ok(value),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EntityKey(#[serde(with = "base64_bytes")] Vec<u8>);

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

/// Stable RFC 012A identity inputs bound before adapter decoding. This context
/// deliberately excludes numeric catalog IDs, database state, observation
/// phase, and delivery sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactSemanticContext {
    adapter_id: AdapterId,
    source_instance_key: CanonicalSourceInstanceKey,
    stream_key: Arc<[u8]>,
    object_key: Arc<[u8]>,
    framing_contract_version: u32,
}

impl FactSemanticContext {
    pub fn new(
        adapter_id: &AdapterId,
        source_instance_identity_contract_version: u32,
        stable_instance_discriminator: &[u8],
        stream_key: &[u8],
        object_key: &[u8],
        framing_contract_version: u32,
    ) -> Result<Self, AdapterError> {
        let source_instance_key = CanonicalSourceInstanceKey::derive(
            source_instance_identity_contract_version,
            stable_instance_discriminator,
        )
        .map_err(semantic_identity_error)?;
        // Validate every remaining component at binding time rather than
        // allowing the first record to discover an invalid object contract.
        SourceRecordId::derive(
            adapter_id.as_str(),
            &source_instance_key,
            stream_key,
            object_key,
            1,
            b"semantic-context-validation",
            framing_contract_version,
        )
        .map_err(semantic_identity_error)?;
        Ok(Self {
            adapter_id: adapter_id.clone(),
            source_instance_key,
            stream_key: Arc::from(stream_key),
            object_key: Arc::from(object_key),
            framing_contract_version,
        })
    }

    pub fn source_instance_key(&self) -> CanonicalSourceInstanceKey {
        self.source_instance_key
    }

    pub fn adapter_id(&self) -> &AdapterId {
        &self.adapter_id
    }

    pub fn stream_key(&self) -> &[u8] {
        self.stream_key.as_ref()
    }

    pub fn object_key(&self) -> &[u8] {
        self.object_key.as_ref()
    }

    pub fn source_record_id(&self, record: &SourceRecord) -> Result<SourceRecordId, AdapterError> {
        let logical_position = logical_record_position(record);
        SourceRecordId::derive(
            self.adapter_id.as_str(),
            &self.source_instance_key,
            self.stream_key.as_ref(),
            self.object_key.as_ref(),
            record.generation,
            &logical_position,
            self.framing_contract_version,
        )
        .map_err(semantic_identity_error)
    }

    fn canonical_entity_key(
        &self,
        entity_kind: &str,
        stable_native_entity_key: &[u8],
    ) -> Result<CanonicalEntityKey, AdapterError> {
        CanonicalEntityKey::derive(
            self.adapter_id.as_str(),
            &self.source_instance_key,
            entity_kind,
            stable_native_entity_key,
        )
        .map_err(semantic_identity_error)
    }

    fn canonical_root_actor_run_key(
        &self,
        stable_native_session_key: &[u8],
        declared_native_run_discriminator: Option<&[u8]>,
    ) -> Result<CanonicalEntityKey, AdapterError> {
        let session = self.canonical_entity_key("session", stable_native_session_key)?;
        CanonicalEntityKey::derive_root_actor_run(
            self.adapter_id.as_str(),
            &self.source_instance_key,
            &session,
            declared_native_run_discriminator,
        )
        .map_err(semantic_identity_error)
    }

    fn object_scoped_native_fact_key(
        &self,
        generation: u64,
        stable_native_fact_key: &[u8],
    ) -> Result<Vec<u8>, AdapterError> {
        if stable_native_fact_key.is_empty() {
            return Err(AdapterError::invalid_contract(
                "object-scoped native fact key must not be empty",
            ));
        }
        let mut key = Vec::with_capacity(
            self.stream_key.len() + self.object_key.len() + stable_native_fact_key.len() + 64,
        );
        key.extend_from_slice(b"object-scoped-native-fact-v1\0");
        push_component(&mut key, self.stream_key.as_ref());
        push_component(&mut key, self.object_key.as_ref());
        key.extend_from_slice(&generation.to_be_bytes());
        push_component(&mut key, stable_native_fact_key);
        Ok(key)
    }
}

/// Parallel RFC 012A identity carried while the RFC 011 store key remains the
/// durable primary key during migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactSemanticRevision {
    pub source_record_id: SourceRecordId,
    pub fact_id: CanonicalFactId,
    pub fact_revision_id: FactRevisionId,
    pub semantic_revision_ref: SemanticRevisionRef,
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

/// One independently replaceable persisted tool-result text document.
///
/// The native file may supplement a transcript tool call or inline result,
/// but its presence alone is not message or run evidence. Correlation is
/// performed by the common projector using the session-scoped native tool ID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedToolResultFact {
    pub result: EntityKey,
    pub session: EntityKey,
    pub project: EntityKey,
    pub native_project_key: String,
    pub native_session_id: String,
    pub native_tool_use_id: String,
    pub native_document_path: String,
    pub content: String,
    pub size_bytes: u64,
}

/// The precedence layer represented by one interpretation-settings document.
///
/// The common pack deliberately models only the source instance's global and
/// local documents. Managed policy, command-line overrides, and project roots
/// outside the configured source instance are not inferred.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterpretationSettingsLayer {
    Global,
    Local,
}

/// Whether the current source document could be interpreted safely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterpretationSettingsDocumentStatus {
    Valid,
    Invalid,
}

/// Redacted hook metadata retained without command, prompt, URL, or agent
/// bodies. Counts describe declarations, not successful hook executions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookEventSummary {
    pub declared_matcher_count: u64,
    pub declared_hook_count: u64,
}

/// Allowlisted settings that materially affect source interpretation.
///
/// Arbitrary native keys, environment values, hook bodies, status-line
/// commands, marketplace locations, and UI preferences are intentionally not
/// represented. Optional collections distinguish an absent field from an
/// explicitly present empty collection so scope merging stays truthful.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterpretationSettingsSnapshot {
    pub agent: Option<String>,
    pub model: Option<String>,
    pub effort_level: Option<String>,
    pub plans_directory: Option<String>,
    pub always_thinking_enabled: Option<bool>,
    pub auto_compact_enabled: Option<bool>,
    pub skip_auto_permission_prompt: Option<bool>,
    pub permission_default_mode: Option<String>,
    pub disable_bypass_permissions_mode: Option<String>,
    pub disable_auto_mode: Option<String>,
    pub permission_allow: Option<Vec<String>>,
    pub permission_ask: Option<Vec<String>>,
    pub permission_deny: Option<Vec<String>>,
    pub enabled_plugins: Option<BTreeMap<String, bool>>,
    pub hook_events: Option<BTreeMap<String, HookEventSummary>>,
}

/// One independently replaceable interpretation-settings document.
///
/// Invalid documents retain only redacted health evidence. They never become
/// an empty or last-known-good settings layer, which prevents stale permission
/// state from being silently presented as current.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterpretationSettingsFact {
    pub document: EntityKey,
    pub scope: EntityKey,
    pub layer: InterpretationSettingsLayer,
    pub native_document_path: String,
    pub document_status: InterpretationSettingsDocumentStatus,
    pub settings: Option<InterpretationSettingsSnapshot>,
    pub error_code: Option<String>,
    pub size_bytes: u64,
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
    /// Run that emitted the message. This is an explicit common relation,
    /// not inferred later from source paths or callback ordering.
    pub run: EntityKey,
    pub native_message_id: Option<String>,
    pub native_kind: String,
    pub role: MessageRole,
    pub content: Vec<ContentBlock>,
    pub source_time: Option<QualifiedTimestamp>,
    pub parent_native_message_id: Option<String>,
    pub model: Option<String>,
    pub search_text: Option<String>,
    /// Lossless native JSON used by the canonical detail projection. The
    /// common writer stores this once in `canonical_messages.raw_json`; fact
    /// audit JSON intentionally omits the duplicate body and retains the
    /// record hash/provenance as required by HashOnly transcript streams.
    #[serde(default, skip_serializing)]
    pub raw_json: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunFact {
    pub run: EntityKey,
    pub session: EntityKey,
    pub native_run_id: String,
    pub parent_run: Option<EntityKey>,
}

/// Topology-neutral identity and lineage for one runtime actor. This is the
/// canonical RFC 012C counterpart to the legacy catalog-local `RunFact` and
/// deliberately contains no database IDs or adapter-specific actor kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorRunRole {
    Root,
    Child,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActorRunRevisionFact {
    pub actor_run: CanonicalEntityKey,
    pub session: CanonicalEntityKey,
    pub role: ActorRunRole,
    pub parent_actor_run: Option<CanonicalEntityKey>,
    pub native_session_id: Option<String>,
    pub native_actor_id: Option<String>,
    pub native_actor_type: Option<String>,
}

impl ActorRunRevisionFact {
    pub(crate) fn validate(&self) -> Result<(), AdapterError> {
        match self.role {
            ActorRunRole::Root if self.parent_actor_run.is_some() => {
                return Err(AdapterError::invalid_contract(
                    "root actor run cannot declare a parent actor run",
                ));
            }
            ActorRunRole::Child if self.parent_actor_run.is_none() => {
                return Err(AdapterError::invalid_contract(
                    "child actor run must declare a parent actor run",
                ));
            }
            ActorRunRole::Root | ActorRunRole::Child => {}
        }
        if self.parent_actor_run.as_ref() == Some(&self.actor_run) {
            return Err(AdapterError::invalid_contract(
                "actor run cannot be its own parent",
            ));
        }
        for (field, value) in [
            ("native_session_id", self.native_session_id.as_deref()),
            ("native_actor_id", self.native_actor_id.as_deref()),
            ("native_actor_type", self.native_actor_type.as_deref()),
        ] {
            validate_runtime_semantic_text(field, value)?;
        }
        Ok(())
    }

    /// Canonical value identity for one actor-run revision. Source occurrence
    /// remains separate provenance; replaying the same normalized actor value
    /// from another record must retain one durable/scoped join identity.
    pub(crate) fn semantic_revision_key(&self) -> Result<[u8; FACT_HASH_BYTES], AdapterError> {
        self.validate()?;
        let mut encoded = Vec::new();
        encoded.extend_from_slice(b"spaghetti/runtime.actor-run/semantic-revision\0");
        encoded.extend_from_slice(&1_u32.to_be_bytes());
        push_component(&mut encoded, self.actor_run.as_bytes());
        push_component(&mut encoded, self.session.as_bytes());
        encoded.push(match self.role {
            ActorRunRole::Root => 1,
            ActorRunRole::Child => 2,
        });
        push_optional_component(
            &mut encoded,
            self.parent_actor_run
                .as_ref()
                .map(|key| key.as_bytes().as_slice()),
        );
        push_optional_component(
            &mut encoded,
            self.native_session_id.as_deref().map(str::as_bytes),
        );
        push_optional_component(
            &mut encoded,
            self.native_actor_id.as_deref().map(str::as_bytes),
        );
        push_optional_component(
            &mut encoded,
            self.native_actor_type.as_deref().map(str::as_bytes),
        );
        Ok(*blake3::hash(&encoded).as_bytes())
    }
}

/// Orthogonal affiliation dimensions. An actor may simultaneously belong to
/// both a team and a workflow; neither dimension changes response identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorAffiliationDimension {
    Team,
    Workflow,
}

/// Replaceable affiliation state. `Unknown` records explicit ambiguity;
/// absence of a fact is not interpreted as either membership or removal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorAffiliationState {
    Present,
    Removed,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActorAffiliationRevisionFact {
    pub affiliation: CanonicalEntityKey,
    pub actor_run: CanonicalEntityKey,
    pub session: CanonicalEntityKey,
    pub dimension: ActorAffiliationDimension,
    pub target: CanonicalEntityKey,
    pub member: Option<CanonicalEntityKey>,
    pub native_target_id: Option<String>,
    pub native_member_id: Option<String>,
    pub state: ActorAffiliationState,
    pub effective_at: Option<QualifiedTimestamp>,
}

impl ActorAffiliationRevisionFact {
    pub(crate) fn validate(&self) -> Result<(), AdapterError> {
        for (field, value) in [
            ("native_target_id", self.native_target_id.as_deref()),
            ("native_member_id", self.native_member_id.as_deref()),
            (
                "effective_at.value",
                self.effective_at
                    .as_ref()
                    .map(|timestamp| timestamp.value.as_str()),
            ),
        ] {
            validate_runtime_semantic_text(field, value)?;
        }
        Ok(())
    }

    /// Canonical value identity for one affiliation revision. This realizes
    /// RFC 012C's explicit revision-key axis: relation identity remains the
    /// fact key while state, qualification, and normalized attributes select
    /// the replaceable semantic revision.
    pub(crate) fn semantic_revision_key(&self) -> Result<[u8; FACT_HASH_BYTES], AdapterError> {
        self.validate()?;
        let mut encoded = Vec::new();
        encoded.extend_from_slice(b"spaghetti/runtime.actor-affiliation/semantic-revision\0");
        encoded.extend_from_slice(&1_u32.to_be_bytes());
        push_component(&mut encoded, self.affiliation.as_bytes());
        push_component(&mut encoded, self.actor_run.as_bytes());
        push_component(&mut encoded, self.session.as_bytes());
        encoded.push(match self.dimension {
            ActorAffiliationDimension::Team => 1,
            ActorAffiliationDimension::Workflow => 2,
        });
        push_component(&mut encoded, self.target.as_bytes());
        push_optional_component(
            &mut encoded,
            self.member.as_ref().map(|key| key.as_bytes().as_slice()),
        );
        push_optional_component(
            &mut encoded,
            self.native_target_id.as_deref().map(str::as_bytes),
        );
        push_optional_component(
            &mut encoded,
            self.native_member_id.as_deref().map(str::as_bytes),
        );
        encoded.push(match self.state {
            ActorAffiliationState::Present => 1,
            ActorAffiliationState::Removed => 2,
            ActorAffiliationState::Unknown => 3,
        });
        match &self.effective_at {
            Some(timestamp) => {
                encoded.push(1);
                push_component(&mut encoded, timestamp.value.as_bytes());
                encoded.push(timestamp_quality_revision_tag(timestamp.quality));
            }
            None => encoded.push(0),
        }
        Ok(*blake3::hash(&encoded).as_bytes())
    }
}

fn validate_runtime_semantic_text(field: &str, value: Option<&str>) -> Result<(), AdapterError> {
    if value.is_some_and(|value| {
        value.is_empty() || value.len() > MAX_RUNTIME_SEMANTIC_TEXT_BYTES || value.trim() != value
    }) {
        return Err(AdapterError::invalid_contract(format!(
            "runtime semantic {field} must contain 1..={MAX_RUNTIME_SEMANTIC_TEXT_BYTES} canonical bytes when present"
        )));
    }
    Ok(())
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
    /// Topology-neutral RFC 012A identity carried beside the RFC 011 durable
    /// key while artifact storage completes its semantic-key migration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_artifact: Option<CanonicalEntityKey>,
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
    /// Topology-neutral RFC 012A base session for scoped/durable identity
    /// parity. Legacy producers may omit it; canonical producers may not mix
    /// canonical and legacy-only entries in one fact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_session: Option<CanonicalEntityKey>,
    pub native_message_id: String,
    pub native_snapshot_message_id: String,
    pub observation_kind: ArtifactObservationKind,
    pub is_snapshot_update: bool,
    pub source_time: Option<QualifiedTimestamp>,
    pub artifacts: Vec<ArtifactMetadataEntry>,
}

impl ArtifactMetadataSnapshotFact {
    pub(crate) fn semantic_revision_key(
        &self,
    ) -> Result<Option<[u8; FACT_HASH_BYTES]>, AdapterError> {
        let has_canonical_artifact = self
            .artifacts
            .iter()
            .any(|artifact| artifact.canonical_artifact.is_some());
        let Some(canonical_session) = self.canonical_session else {
            if has_canonical_artifact {
                return Err(AdapterError::invalid_contract(
                    "canonical artifact metadata cannot omit its canonical session",
                ));
            }
            return Ok(None);
        };
        if self.artifacts.len() > MAX_CANONICAL_ARTIFACTS_PER_FACT {
            return Err(AdapterError::invalid_contract(format!(
                "canonical artifact metadata exceeds {MAX_CANONICAL_ARTIFACTS_PER_FACT} entries"
            )));
        }
        validate_runtime_semantic_text(
            "artifact_metadata.native_message_id",
            Some(&self.native_message_id),
        )?;
        validate_runtime_semantic_text(
            "artifact_metadata.native_snapshot_message_id",
            Some(&self.native_snapshot_message_id),
        )?;
        validate_runtime_semantic_text(
            "artifact_metadata.source_time",
            self.source_time.as_ref().map(|value| value.value.as_str()),
        )?;

        let mut artifacts = self.artifacts.iter().collect::<Vec<_>>();
        artifacts.sort_by_key(|artifact| artifact.canonical_artifact);
        let mut canonical_keys = BTreeSet::new();
        let mut encoded = Vec::new();
        encoded.extend_from_slice(b"spaghetti/runtime.artifact-metadata/semantic-revision\0");
        encoded.extend_from_slice(&1_u32.to_be_bytes());
        push_component(&mut encoded, canonical_session.as_bytes());
        push_component(&mut encoded, self.native_message_id.as_bytes());
        push_component(&mut encoded, self.native_snapshot_message_id.as_bytes());
        encoded.push(match self.observation_kind {
            ArtifactObservationKind::Checkpoint => 1,
            ArtifactObservationKind::Delta => 2,
        });
        encoded.push(u8::from(self.is_snapshot_update));
        push_optional_qualified_timestamp(&mut encoded, self.source_time.as_ref());
        encoded.extend_from_slice(&(artifacts.len() as u64).to_be_bytes());
        for artifact in artifacts {
            let canonical_artifact = artifact.canonical_artifact.ok_or_else(|| {
                AdapterError::invalid_contract(
                    "canonical artifact metadata cannot mix canonical and legacy-only entries",
                )
            })?;
            if !canonical_keys.insert(canonical_artifact) {
                return Err(AdapterError::invalid_contract(
                    "canonical artifact metadata repeats an artifact identity",
                ));
            }
            if artifact.version == 0 {
                return Err(AdapterError::invalid_contract(
                    "canonical artifact metadata version must be positive",
                ));
            }
            for (field, value) in [
                (
                    "artifact_metadata.native_artifact_id",
                    artifact.native_artifact_id.as_deref(),
                ),
                (
                    "artifact_metadata.tracking_path",
                    Some(artifact.tracking_path.as_str()),
                ),
                (
                    "artifact_metadata.real_parent_dir",
                    artifact.real_parent_dir.as_deref(),
                ),
                (
                    "artifact_metadata.backup_time",
                    Some(artifact.backup_time.value.as_str()),
                ),
            ] {
                validate_runtime_semantic_text(field, value)?;
            }
            push_component(&mut encoded, canonical_artifact.as_bytes());
            push_optional_component(
                &mut encoded,
                artifact.native_artifact_id.as_deref().map(str::as_bytes),
            );
            push_component(&mut encoded, artifact.tracking_path.as_bytes());
            push_optional_component(
                &mut encoded,
                artifact.real_parent_dir.as_deref().map(str::as_bytes),
            );
            encoded.extend_from_slice(&artifact.version.to_be_bytes());
            push_qualified_timestamp(&mut encoded, &artifact.backup_time);
            encoded.push(match artifact.capture {
                ArtifactCapture::ContentExpected => 1,
                ArtifactCapture::NotCaptured => 2,
            });
        }
        Ok(Some(*blake3::hash(&encoded).as_bytes()))
    }
}

/// One independently replaceable native artifact-content blob. Its path
/// supplies session, native hash, and version but not a tracked path or run;
/// those relations remain pending until transcript metadata arrives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactContentFact {
    pub artifact: EntityKey,
    pub session: EntityKey,
    /// Topology-neutral identities retained in parallel with the legacy
    /// projection keys until the durable artifact schema migrates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_artifact: Option<CanonicalEntityKey>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_session: Option<CanonicalEntityKey>,
    pub native_artifact_id: String,
    pub native_file_hash: String,
    pub version: u64,
    #[serde(with = "base64_bytes")]
    pub content: Vec<u8>,
    pub size_bytes: u64,
}

impl ArtifactContentFact {
    pub(crate) fn semantic_revision_key(
        &self,
    ) -> Result<Option<[u8; FACT_HASH_BYTES]>, AdapterError> {
        let (canonical_artifact, canonical_session) =
            match (self.canonical_artifact, self.canonical_session) {
                (None, None) => return Ok(None),
                (Some(artifact), Some(session)) => (artifact, session),
                _ => {
                    return Err(AdapterError::invalid_contract(
                        "canonical artifact content requires both artifact and session identities",
                    ));
                }
            };
        if self.version == 0 || u64::try_from(self.content.len()).ok() != Some(self.size_bytes) {
            return Err(AdapterError::invalid_contract(
                "canonical artifact content has an invalid version or byte count",
            ));
        }
        validate_runtime_semantic_text(
            "artifact_content.native_artifact_id",
            Some(&self.native_artifact_id),
        )?;
        validate_runtime_semantic_text(
            "artifact_content.native_file_hash",
            Some(&self.native_file_hash),
        )?;
        let content_digest = blake3::hash(&self.content);
        let mut encoded = Vec::new();
        encoded.extend_from_slice(b"spaghetti/runtime.artifact-content/semantic-revision\0");
        encoded.extend_from_slice(&1_u32.to_be_bytes());
        push_component(&mut encoded, canonical_artifact.as_bytes());
        push_component(&mut encoded, canonical_session.as_bytes());
        push_component(&mut encoded, self.native_artifact_id.as_bytes());
        push_component(&mut encoded, self.native_file_hash.as_bytes());
        encoded.extend_from_slice(&self.version.to_be_bytes());
        encoded.extend_from_slice(&self.size_bytes.to_be_bytes());
        push_component(&mut encoded, content_digest.as_bytes());
        Ok(Some(*blake3::hash(&encoded).as_bytes()))
    }
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

/// How a native usage response is identified inside one source object and
/// generation. The enclosing canonical fact key supplies that scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageResponseIdentity {
    NativeMessageId,
    SourceRecordFallback,
}

/// Agent-neutral authority classification for a qualified runtime value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageValueAuthority {
    NativeResponse,
    AdapterDerived,
}

/// Bounded evidence describing how one common value maps to native evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageValueProvenance {
    pub native_field: String,
    pub normalization_contract_version: u32,
}

pub type UsageQualifiedValue<T> = QualifiedValue<T, UsageValueAuthority, UsageValueProvenance>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageBucketsV2 {
    pub input_tokens: UsageQualifiedValue<u64>,
    pub output_tokens: UsageQualifiedValue<u64>,
    pub cache_creation_input_tokens: UsageQualifiedValue<u64>,
    pub cache_read_input_tokens: UsageQualifiedValue<u64>,
}

/// RFC 012C response-level, replaceable usage snapshot. `FactSemanticRevision`
/// owns its public usage/revision identity; this payload carries the typed
/// reducer value and stable canonical session/actor attribution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageRevisionV2Fact {
    pub session: CanonicalEntityKey,
    pub actor_run: CanonicalEntityKey,
    #[serde(with = "base64_bytes")]
    pub response_key: Vec<u8>,
    pub response_identity: UsageResponseIdentity,
    pub native_message_id: Option<String>,
    pub request_id: Option<String>,
    pub buckets: UsageBucketsV2,
    pub model: Option<UsageQualifiedValue<String>>,
    pub effort: Option<UsageQualifiedValue<String>>,
    pub source_time: Option<QualifiedTimestamp>,
}

impl UsageRevisionV2Fact {
    pub(crate) fn validate(&self) -> Result<(), AdapterError> {
        if self.response_key.is_empty() || self.response_key.len() > MAX_USAGE_RESPONSE_KEY_BYTES {
            return Err(AdapterError::invalid_contract(format!(
                "usage-v2 response key must contain 1..={MAX_USAGE_RESPONSE_KEY_BYTES} bytes"
            )));
        }
        match self.response_identity {
            UsageResponseIdentity::NativeMessageId => {
                let native_message_id = self
                    .native_message_id
                    .as_deref()
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| {
                        AdapterError::invalid_contract(
                            "native usage response identity requires native_message_id",
                        )
                    })?;
                if self.response_key != native_message_id.as_bytes() {
                    return Err(AdapterError::invalid_contract(
                        "native usage response key must equal native_message_id",
                    ));
                }
            }
            UsageResponseIdentity::SourceRecordFallback => {
                if self.native_message_id.is_some() {
                    return Err(AdapterError::invalid_contract(
                        "source-record usage fallback cannot claim a native_message_id",
                    ));
                }
            }
        }
        if self
            .request_id
            .as_ref()
            .is_some_and(|value| value.is_empty())
        {
            return Err(AdapterError::invalid_contract(
                "usage-v2 request_id must be non-empty when present",
            ));
        }
        for value in [
            &self.buckets.input_tokens,
            &self.buckets.output_tokens,
            &self.buckets.cache_creation_input_tokens,
            &self.buckets.cache_read_input_tokens,
        ] {
            validate_usage_qualified_value(value)?;
        }
        for value in [&self.model, &self.effort].into_iter().flatten() {
            validate_usage_qualified_value(value)?;
            if value.value.as_ref().is_some_and(|value| value.is_empty()) {
                return Err(AdapterError::invalid_contract(
                    "usage-v2 model/effort value must be non-empty",
                ));
            }
        }
        validate_runtime_semantic_text(
            "source_time.value",
            self.source_time
                .as_ref()
                .map(|timestamp| timestamp.value.as_str()),
        )?;
        Ok(())
    }

    /// Deterministic semantic value identity for one response revision.
    ///
    /// The enclosing canonical fact ID already owns the source-object,
    /// generation, and response identity. This key deliberately encodes the
    /// complete normalized value as well, so an exact native repeat has the
    /// same revision while any counter, qualification, attribution, model,
    /// effort, request correlation, or native-time correction has a distinct
    /// revision. Raw source-record identity remains available separately in
    /// `FactSemanticRevision::source_record_id`.
    pub fn semantic_revision_key(&self) -> Result<[u8; FACT_HASH_BYTES], AdapterError> {
        self.validate()?;
        let mut encoded = Vec::new();
        encoded.extend_from_slice(b"spaghetti/runtime.usage-v2/semantic-revision\0");
        encoded.extend_from_slice(&1_u32.to_be_bytes());
        push_component(&mut encoded, self.session.as_bytes());
        push_component(&mut encoded, self.actor_run.as_bytes());
        encoded.push(match self.response_identity {
            UsageResponseIdentity::NativeMessageId => 1,
            UsageResponseIdentity::SourceRecordFallback => 2,
        });
        push_component(&mut encoded, &self.response_key);
        push_optional_component(
            &mut encoded,
            self.native_message_id.as_deref().map(str::as_bytes),
        );
        push_optional_component(&mut encoded, self.request_id.as_deref().map(str::as_bytes));
        push_usage_qualified_value(&mut encoded, &self.buckets.input_tokens, |output, value| {
            output.extend_from_slice(&value.to_be_bytes());
        });
        push_usage_qualified_value(
            &mut encoded,
            &self.buckets.output_tokens,
            |output, value| {
                output.extend_from_slice(&value.to_be_bytes());
            },
        );
        push_usage_qualified_value(
            &mut encoded,
            &self.buckets.cache_creation_input_tokens,
            |output, value| output.extend_from_slice(&value.to_be_bytes()),
        );
        push_usage_qualified_value(
            &mut encoded,
            &self.buckets.cache_read_input_tokens,
            |output, value| output.extend_from_slice(&value.to_be_bytes()),
        );
        push_optional_usage_qualified_value(&mut encoded, self.model.as_ref(), |output, value| {
            push_component(output, value.as_bytes());
        });
        push_optional_usage_qualified_value(&mut encoded, self.effort.as_ref(), |output, value| {
            push_component(output, value.as_bytes());
        });
        match self.source_time.as_ref() {
            Some(source_time) => {
                encoded.push(1);
                push_component(&mut encoded, source_time.value.as_bytes());
                encoded.push(timestamp_quality_revision_tag(source_time.quality));
            }
            None => encoded.push(0),
        }
        Ok(*blake3::hash(&encoded).as_bytes())
    }
}

fn push_optional_component(output: &mut Vec<u8>, value: Option<&[u8]>) {
    match value {
        Some(value) => {
            output.push(1);
            push_component(output, value);
        }
        None => output.push(0),
    }
}

fn push_qualified_timestamp(output: &mut Vec<u8>, value: &QualifiedTimestamp) {
    push_component(output, value.value.as_bytes());
    output.push(timestamp_quality_revision_tag(value.quality));
}

fn push_optional_qualified_timestamp(output: &mut Vec<u8>, value: Option<&QualifiedTimestamp>) {
    match value {
        Some(value) => {
            output.push(1);
            push_qualified_timestamp(output, value);
        }
        None => output.push(0),
    }
}

fn push_optional_usage_qualified_value<T>(
    output: &mut Vec<u8>,
    value: Option<&UsageQualifiedValue<T>>,
    push_value: impl FnOnce(&mut Vec<u8>, &T),
) {
    match value {
        Some(value) => {
            output.push(1);
            push_usage_qualified_value(output, value, push_value);
        }
        None => output.push(0),
    }
}

fn push_usage_qualified_value<T>(
    output: &mut Vec<u8>,
    value: &UsageQualifiedValue<T>,
    push_value: impl FnOnce(&mut Vec<u8>, &T),
) {
    match value.value.as_ref() {
        Some(value) => {
            output.push(1);
            push_value(output, value);
        }
        None => output.push(0),
    }
    output.push(usage_quality_revision_tag(value.quality));
    output.push(match value.authority {
        UsageValueAuthority::NativeResponse => 1,
        UsageValueAuthority::AdapterDerived => 2,
    });
    output.push(match value.completeness {
        ContractCompleteness::Complete => 1,
        ContractCompleteness::Partial => 2,
        ContractCompleteness::Unknown => 3,
    });
    output.push(match value.unknown_reason {
        None => 0,
        Some(QualifiedUnknownReason::Missing) => 1,
        Some(QualifiedUnknownReason::Unsupported) => 2,
        Some(QualifiedUnknownReason::Withheld) => 3,
        Some(QualifiedUnknownReason::NotYetObserved) => 4,
        Some(QualifiedUnknownReason::Ambiguous) => 5,
        Some(QualifiedUnknownReason::Malformed) => 6,
    });
    match value.effective_at {
        Some(effective_at) => {
            output.push(1);
            output.extend_from_slice(&effective_at.to_be_bytes());
        }
        None => output.push(0),
    }
    push_component(output, value.provenance.native_field.as_bytes());
    output.extend_from_slice(
        &value
            .provenance
            .normalization_contract_version
            .to_be_bytes(),
    );
}

fn usage_quality_revision_tag(quality: QualifiedValueQuality) -> u8 {
    match quality {
        QualifiedValueQuality::Exact => 1,
        QualifiedValueQuality::NativeClaimed => 2,
        QualifiedValueQuality::Derived => 3,
        QualifiedValueQuality::Estimated => 4,
        QualifiedValueQuality::Unknown => 5,
    }
}

fn timestamp_quality_revision_tag(quality: TimestampQuality) -> u8 {
    match quality {
        TimestampQuality::NativeExact => 1,
        TimestampQuality::NativeApproximate => 2,
        TimestampQuality::FileMetadataFallback => 3,
        TimestampQuality::Derived => 4,
    }
}

fn validate_usage_qualified_value<T>(value: &UsageQualifiedValue<T>) -> Result<(), AdapterError> {
    let unknown = value.quality == QualifiedValueQuality::Unknown;
    if unknown != value.value.is_none() || unknown != value.unknown_reason.is_some() {
        return Err(AdapterError::invalid_contract(
            "usage-v2 qualified value must pair unknown quality with an absent value and reason",
        ));
    }
    if unknown && value.completeness == ContractCompleteness::Complete {
        return Err(AdapterError::invalid_contract(
            "unknown usage-v2 value cannot claim complete coverage",
        ));
    }
    if value.provenance.native_field.is_empty()
        || value.provenance.native_field.len() > MAX_USAGE_PROVENANCE_FIELD_BYTES
        || value.provenance.normalization_contract_version == 0
    {
        return Err(AdapterError::invalid_contract(
            "usage-v2 value provenance is empty, oversized, or unversioned",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Fact {
    Session(SessionFact),
    SessionIndexSnapshot(SessionIndexSnapshotFact),
    ProjectMemoryDocument(ProjectMemoryDocumentFact),
    PersistedToolResult(PersistedToolResultFact),
    InterpretationSettings(InterpretationSettingsFact),
    Message(MessageFact),
    Run(RunFact),
    ActorRunRevision(ActorRunRevisionFact),
    ActorAffiliationRevision(ActorAffiliationRevisionFact),
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
    UsageRevisionV2(UsageRevisionV2Fact),
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
            Self::PersistedToolResult(_) => "persisted_tool_result",
            Self::InterpretationSettings(_) => "interpretation_settings",
            Self::Message(_) => "message",
            Self::Run(_) => "run",
            Self::ActorRunRevision(_) => "runtime.actor-run",
            Self::ActorAffiliationRevision(_) => "runtime.actor-affiliation",
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
            Self::UsageRevisionV2(_) => "runtime.usage-v2",
            Self::UnknownRecord { .. } => "unknown_record",
        }
    }

    pub fn entity_key(&self) -> Option<&EntityKey> {
        match self {
            Self::Session(fact) => Some(&fact.session),
            Self::SessionIndexSnapshot(fact) => Some(&fact.project),
            Self::ProjectMemoryDocument(fact) => Some(&fact.document),
            Self::PersistedToolResult(fact) => Some(&fact.result),
            Self::InterpretationSettings(fact) => Some(&fact.document),
            Self::Message(fact) => Some(&fact.message),
            Self::Run(fact) => Some(&fact.run),
            Self::ActorRunRevision(_) | Self::ActorAffiliationRevision(_) => None,
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
            Self::UsageRevisionV2(_) => None,
            Self::UnknownRecord { .. } => None,
        }
    }

    fn required_value_semantic_revision_key(
        &self,
    ) -> Result<Option<[u8; FACT_HASH_BYTES]>, AdapterError> {
        match self {
            Self::ActorRunRevision(revision) => revision.semantic_revision_key().map(Some),
            Self::ActorAffiliationRevision(revision) => revision.semantic_revision_key().map(Some),
            Self::ArtifactMetadataSnapshot(revision) => revision.semantic_revision_key(),
            Self::ArtifactContent(revision) => revision.semantic_revision_key(),
            _ => Ok(None),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FactEnvelope {
    pub id: FactId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic_revision: Option<FactSemanticRevision>,
    pub provenance: FactProvenance,
    pub value: Fact,
}

pub struct FactBatch {
    max_facts: usize,
    max_diagnostics: usize,
    facts: Vec<FactEnvelope>,
    diagnostics: Vec<AdapterDiagnostic>,
    unscoped_permanent_diagnostic: bool,
    diagnostic_coverage_gaps: BTreeSet<CapabilityId>,
    dependency_reads: Vec<DependencyRevision>,
    next_decoder_state: Option<Vec<u8>>,
    next_record_ordinals: BTreeMap<RecordFactKey, u32>,
    semantic_context: Option<FactSemanticContext>,
    semantic_revisions: std::collections::BTreeSet<FactRevisionId>,
    semantic_record_cache: Option<Box<SemanticRecordCache>>,
    fact_build_time: Duration,
}

struct SemanticRecordCache {
    generation: u64,
    cursor_start: Vec<u8>,
    cursor_end: Vec<u8>,
    snapshot_ordinal: Option<u32>,
    source_record_id: SourceRecordId,
}

impl SemanticRecordCache {
    fn matches(&self, record: &SourceRecord) -> bool {
        self.generation == record.generation
            && self.cursor_start == record.cursor_start.as_bytes()
            && self.cursor_end == record.cursor_end.as_bytes()
            && self.snapshot_ordinal
                == record
                    .cursor_start
                    .append_offset_value()
                    .zip(record.cursor_end.append_offset_value())
                    .is_none()
                    .then_some(record.ordinal_in_batch)
    }

    fn new(record: &SourceRecord, source_record_id: SourceRecordId) -> Self {
        let snapshot_ordinal = record
            .cursor_start
            .append_offset_value()
            .zip(record.cursor_end.append_offset_value())
            .is_none()
            .then_some(record.ordinal_in_batch);
        Self {
            generation: record.generation,
            cursor_start: record.cursor_start.as_bytes().to_vec(),
            cursor_end: record.cursor_end.as_bytes().to_vec(),
            snapshot_ordinal,
            source_record_id,
        }
    }
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
            unscoped_permanent_diagnostic: false,
            diagnostic_coverage_gaps: BTreeSet::new(),
            dependency_reads: Vec::new(),
            next_decoder_state: None,
            next_record_ordinals: BTreeMap::new(),
            semantic_context: None,
            semantic_revisions: std::collections::BTreeSet::new(),
            semantic_record_cache: None,
            fact_build_time: Duration::ZERO,
        })
    }

    pub(crate) fn new_with_semantic_context(
        max_facts: usize,
        max_diagnostics: usize,
        semantic_context: FactSemanticContext,
    ) -> Result<Self, AdapterError> {
        let mut batch = Self::new(max_facts, max_diagnostics)?;
        batch.semantic_context = Some(semantic_context);
        Ok(batch)
    }

    /// Legacy RFC 011 emission. It intentionally does not synthesize an RFC
    /// 012A revision from the local ordinal; adapters migrate each fact family
    /// through `push_native` or `push_derived` with an explicit semantic key.
    pub fn push(&mut self, record: &SourceRecord, value: Fact) -> Result<FactId, AdapterError> {
        self.push_internal(record, value, None)
    }

    /// Emit a replaceable native fact whose stable key may span several source
    /// records. The source record is the default semantic revision boundary;
    /// fact families with a canonical value revision key override that default
    /// here so adapters cannot accidentally choose a weaker identity.
    pub fn push_native(
        &mut self,
        record: &SourceRecord,
        stable_native_fact_key: &[u8],
        value: Fact,
    ) -> Result<FactId, AdapterError> {
        let revision_key = value.required_value_semantic_revision_key()?;
        let semantic = self.semantic_revision(
            record,
            value.kind(),
            true,
            stable_native_fact_key,
            revision_key.as_ref().map(|key| key.as_slice()),
        )?;
        self.push_internal(record, value, Some(semantic))
    }

    /// Emit a replaceable native fact whose identity is local to the stable
    /// source stream/object/generation tuple. This is the required shape for
    /// response IDs that vendors only guarantee within one transcript.
    pub fn push_native_object_scoped(
        &mut self,
        record: &SourceRecord,
        stable_native_fact_key: &[u8],
        value: Fact,
    ) -> Result<FactId, AdapterError> {
        let revision_key = value.required_value_semantic_revision_key()?;
        let semantic_key = self
            .semantic_context
            .as_ref()
            .ok_or_else(|| {
                AdapterError::invalid_contract(
                    "canonical fact emission requires a bound semantic decode context",
                )
            })?
            .object_scoped_native_fact_key(record.generation, stable_native_fact_key)?;
        let semantic = self.semantic_revision(
            record,
            value.kind(),
            true,
            &semantic_key,
            revision_key.as_ref().map(|key| key.as_slice()),
        )?;
        self.push_internal(record, value, Some(semantic))
    }

    /// Emit an object-scoped native fact with a value-derived semantic
    /// revision. Equal revisions in one batch are idempotently suppressed;
    /// reusing an explicit revision key for a different normalized value is a
    /// contract error.
    pub fn push_native_object_scoped_with_revision(
        &mut self,
        record: &SourceRecord,
        stable_native_fact_key: &[u8],
        source_or_semantic_revision: &[u8],
        value: Fact,
    ) -> Result<FactId, AdapterError> {
        if value
            .required_value_semantic_revision_key()?
            .is_some_and(|expected| expected.as_slice() != source_or_semantic_revision)
        {
            return Err(AdapterError::invalid_contract(
                "fact family requires its canonical value semantic revision key",
            ));
        }
        let semantic_key = self
            .semantic_context
            .as_ref()
            .ok_or_else(|| {
                AdapterError::invalid_contract(
                    "canonical fact emission requires a bound semantic decode context",
                )
            })?
            .object_scoped_native_fact_key(record.generation, stable_native_fact_key)?;
        let semantic = self.semantic_revision(
            record,
            value.kind(),
            true,
            &semantic_key,
            Some(source_or_semantic_revision),
        )?;
        if let Some(existing) = self.facts.iter().find(|envelope| {
            envelope
                .semantic_revision
                .is_some_and(|candidate| candidate.fact_revision_id == semantic.fact_revision_id)
        }) {
            if existing.value != value {
                return Err(AdapterError::invalid_contract(
                    "one canonical fact revision cannot encode different normalized values",
                ));
            }
            return Ok(existing.id);
        }
        self.push_internal(record, value, Some(semantic))
    }

    /// Derive an RFC 012A entity identity from the same topology-neutral source
    /// context used for canonical fact revisions.
    pub fn canonical_entity_key(
        &self,
        entity_kind: &str,
        stable_native_entity_key: &[u8],
    ) -> Result<CanonicalEntityKey, AdapterError> {
        self.semantic_context
            .as_ref()
            .ok_or_else(|| {
                AdapterError::invalid_contract(
                    "canonical entity derivation requires a bound semantic decode context",
                )
            })?
            .canonical_entity_key(entity_kind, stable_native_entity_key)
    }

    /// Derive the canonical RFC 012C root actor from this batch's source
    /// instance and final base session identity. Child actor keys continue to
    /// use their support-declared native/fallback derivations.
    pub fn canonical_root_actor_run_key(
        &self,
        stable_native_session_key: &[u8],
        declared_native_run_discriminator: Option<&[u8]>,
    ) -> Result<CanonicalEntityKey, AdapterError> {
        self.semantic_context
            .as_ref()
            .ok_or_else(|| {
                AdapterError::invalid_contract(
                    "canonical root actor derivation requires a bound semantic decode context",
                )
            })?
            .canonical_root_actor_run_key(
                stable_native_session_key,
                declared_native_run_discriminator,
            )
    }

    /// Emit a native fact whose revision is not fully owned by the primary
    /// source record (for example, one incorporating a declared dependency
    /// revision). The explicit revision key must encode every semantic input
    /// under the fact family's versioned contract.
    pub fn push_native_with_revision(
        &mut self,
        record: &SourceRecord,
        stable_native_fact_key: &[u8],
        source_or_semantic_revision: &[u8],
        value: Fact,
    ) -> Result<FactId, AdapterError> {
        if value
            .required_value_semantic_revision_key()?
            .is_some_and(|expected| expected.as_slice() != source_or_semantic_revision)
        {
            return Err(AdapterError::invalid_contract(
                "fact family requires its canonical value semantic revision key",
            ));
        }
        let semantic = self.semantic_revision(
            record,
            value.kind(),
            true,
            stable_native_fact_key,
            Some(source_or_semantic_revision),
        )?;
        self.push_internal(record, value, Some(semantic))
    }

    /// Emit a record-owned fact. The caller-provided subkey is part of the
    /// versioned decoder contract and distinguishes same-kind facts without
    /// depending on map iteration, batching, or scheduling.
    pub fn push_derived(
        &mut self,
        record: &SourceRecord,
        deterministic_semantic_subkey: &[u8],
        value: Fact,
    ) -> Result<FactId, AdapterError> {
        let revision_key = value.required_value_semantic_revision_key()?;
        let semantic = self.semantic_revision(
            record,
            value.kind(),
            false,
            deterministic_semantic_subkey,
            revision_key.as_ref().map(|key| key.as_slice()),
        )?;
        self.push_internal(record, value, Some(semantic))
    }

    /// Emit a record-derived fact with an explicit semantic revision key when
    /// the primary source record alone does not own the decoded value.
    pub fn push_derived_with_revision(
        &mut self,
        record: &SourceRecord,
        deterministic_semantic_subkey: &[u8],
        source_or_semantic_revision: &[u8],
        value: Fact,
    ) -> Result<FactId, AdapterError> {
        if value
            .required_value_semantic_revision_key()?
            .is_some_and(|expected| expected.as_slice() != source_or_semantic_revision)
        {
            return Err(AdapterError::invalid_contract(
                "fact family requires its canonical value semantic revision key",
            ));
        }
        let semantic = self.semantic_revision(
            record,
            value.kind(),
            false,
            deterministic_semantic_subkey,
            Some(source_or_semantic_revision),
        )?;
        self.push_internal(record, value, Some(semantic))
    }

    fn push_internal(
        &mut self,
        record: &SourceRecord,
        value: Fact,
        semantic_revision: Option<FactSemanticRevision>,
    ) -> Result<FactId, AdapterError> {
        let started = Instant::now();
        let result = (|| {
            if self.facts.len() == self.max_facts {
                return Err(AdapterError::invalid_contract(format!(
                    "fact batch exceeds {} facts",
                    self.max_facts
                )));
            }
            if semantic_revision.as_ref().is_some_and(|semantic| {
                self.semantic_revisions.contains(&semantic.fact_revision_id)
            }) {
                return Err(AdapterError::invalid_contract(
                    "fact batch emitted the same canonical fact revision more than once",
                ));
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
            if let Some(semantic) = semantic_revision {
                let inserted = self.semantic_revisions.insert(semantic.fact_revision_id);
                debug_assert!(inserted, "canonical revision was checked before mutation");
            }
            self.facts.push(FactEnvelope {
                id,
                semantic_revision,
                provenance,
                value,
            });
            Ok(id)
        })();
        self.fact_build_time = self.fact_build_time.saturating_add(started.elapsed());
        result
    }

    fn semantic_revision(
        &mut self,
        record: &SourceRecord,
        fact_kind: &str,
        native: bool,
        semantic_key: &[u8],
        explicit_revision: Option<&[u8]>,
    ) -> Result<FactSemanticRevision, AdapterError> {
        let context = self.semantic_context.as_ref().ok_or_else(|| {
            AdapterError::invalid_contract(
                "canonical fact emission requires a bound semantic decode context",
            )
        })?;
        let source_record_id = match &self.semantic_record_cache {
            Some(cache) if cache.matches(record) => cache.source_record_id,
            _ => {
                let source_record_id = context.source_record_id(record)?;
                self.semantic_record_cache =
                    Some(Box::new(SemanticRecordCache::new(record, source_record_id)));
                source_record_id
            }
        };
        let fact_id = if native {
            CanonicalFactId::native(
                context.adapter_id.as_str(),
                &context.source_instance_key,
                fact_kind,
                semantic_key,
            )
        } else {
            CanonicalFactId::derived(
                context.adapter_id.as_str(),
                &context.source_instance_key,
                fact_kind,
                &source_record_id,
                semantic_key,
            )
        }
        .map_err(semantic_identity_error)?;
        let revision_key = explicit_revision.unwrap_or_else(|| source_record_id.as_bytes());
        let fact_revision_id =
            FactRevisionId::derive(&fact_id, 1, revision_key).map_err(semantic_identity_error)?;
        Ok(FactSemanticRevision {
            source_record_id,
            fact_id,
            fact_revision_id,
            semantic_revision_ref: SemanticRevisionRef::new(fact_revision_id),
        })
    }

    pub fn push_diagnostic(&mut self, diagnostic: AdapterDiagnostic) -> Result<(), AdapterError> {
        self.ensure_diagnostic_capacity()?;
        if diagnostic.class == super::AdapterErrorClass::RecordPermanent {
            self.unscoped_permanent_diagnostic = true;
        }
        self.diagnostics.push(diagnostic);
        Ok(())
    }

    /// Retain a record diagnostic while limiting its coverage consequence to
    /// the declared capabilities whose evidence the decoder could not prove.
    /// An empty scope is invalid: adapters must use `push_diagnostic` when the
    /// loss may affect every capability supplied by the stream.
    pub fn push_scoped_diagnostic(
        &mut self,
        diagnostic: AdapterDiagnostic,
        affected_capabilities: impl IntoIterator<Item = CapabilityId>,
    ) -> Result<(), AdapterError> {
        let affected_capabilities = affected_capabilities.into_iter().collect::<BTreeSet<_>>();
        if affected_capabilities.is_empty() {
            return Err(AdapterError::invalid_contract(
                "scoped diagnostic must affect at least one capability",
            ));
        }
        self.ensure_diagnostic_capacity()?;
        if diagnostic.class == super::AdapterErrorClass::RecordPermanent {
            self.diagnostic_coverage_gaps.extend(affected_capabilities);
        }
        self.diagnostics.push(diagnostic);
        Ok(())
    }

    fn ensure_diagnostic_capacity(&self) -> Result<(), AdapterError> {
        if self.diagnostics.len() == self.max_diagnostics {
            return Err(AdapterError::invalid_contract(format!(
                "fact batch exceeds {} diagnostics",
                self.max_diagnostics
            )));
        }
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

    pub(crate) fn can_append(&self, other: &Self) -> bool {
        self.facts
            .len()
            .checked_add(other.facts.len())
            .is_some_and(|count| count <= self.max_facts)
            && self
                .diagnostics
                .len()
                .checked_add(other.diagnostics.len())
                .is_some_and(|count| count <= self.max_diagnostics)
    }

    pub(crate) fn append(&mut self, mut other: Self) -> Result<(), AdapterError> {
        if !self.can_append(&other) {
            return Err(AdapterError::invalid_contract(format!(
                "fact batch exceeds {} facts or {} diagnostics",
                self.max_facts, self.max_diagnostics
            )));
        }
        match (&self.semantic_context, &other.semantic_context) {
            (Some(current), Some(incoming)) if current != incoming => {
                return Err(AdapterError::invalid_contract(
                    "fact batch merge crosses canonical source object contexts",
                ));
            }
            (None, Some(incoming)) => self.semantic_context = Some(incoming.clone()),
            _ => {}
        }
        let repeated_revisions = other
            .semantic_revisions
            .iter()
            .filter(|revision| self.semantic_revisions.contains(revision))
            .copied()
            .collect::<Vec<_>>();
        for revision in &repeated_revisions {
            let existing = self.facts.iter().find(|envelope| {
                envelope
                    .semantic_revision
                    .is_some_and(|semantic| semantic.fact_revision_id == *revision)
            });
            let incoming = other.facts.iter().find(|envelope| {
                envelope
                    .semantic_revision
                    .is_some_and(|semantic| semantic.fact_revision_id == *revision)
            });
            let idempotent_value_revision =
                existing.zip(incoming).is_some_and(|(existing, incoming)| {
                    matches!(
                        (&existing.value, &incoming.value),
                        (Fact::UsageRevisionV2(_), Fact::UsageRevisionV2(_))
                            | (Fact::ActorRunRevision(_), Fact::ActorRunRevision(_))
                            | (
                                Fact::ActorAffiliationRevision(_),
                                Fact::ActorAffiliationRevision(_)
                            )
                            | (
                                Fact::ArtifactMetadataSnapshot(_),
                                Fact::ArtifactMetadataSnapshot(_)
                            )
                            | (Fact::ArtifactContent(_), Fact::ArtifactContent(_))
                    ) && existing.value == incoming.value
                        && existing
                            .semantic_revision
                            .zip(incoming.semantic_revision)
                            .is_some_and(|(existing, incoming)| {
                                existing.fact_id == incoming.fact_id
                                    && existing.semantic_revision_ref
                                        == incoming.semantic_revision_ref
                            })
                });
            if !idempotent_value_revision {
                return Err(AdapterError::invalid_contract(
                    "fact batch merge repeats a canonical fact revision",
                ));
            }
        }
        if !repeated_revisions.is_empty() {
            let repeated_revisions = repeated_revisions.into_iter().collect::<BTreeSet<_>>();
            other.facts.retain(|envelope| {
                envelope
                    .semantic_revision
                    .is_none_or(|semantic| !repeated_revisions.contains(&semantic.fact_revision_id))
            });
            other
                .semantic_revisions
                .retain(|revision| !repeated_revisions.contains(revision));
        }
        self.facts.append(&mut other.facts);
        self.diagnostics.append(&mut other.diagnostics);
        self.unscoped_permanent_diagnostic |= other.unscoped_permanent_diagnostic;
        self.diagnostic_coverage_gaps
            .append(&mut other.diagnostic_coverage_gaps);
        self.semantic_revisions
            .append(&mut other.semantic_revisions);
        for dependency in other.dependency_reads {
            self.add_dependency_read(dependency)?;
        }
        if other.next_decoder_state.is_some() {
            self.next_decoder_state = other.next_decoder_state;
        }
        self.fact_build_time = self.fact_build_time.saturating_add(other.fact_build_time);
        Ok(())
    }

    pub(crate) fn fact_build_time(&self) -> Duration {
        self.fact_build_time
    }

    pub fn facts(&self) -> &[FactEnvelope] {
        &self.facts
    }

    pub fn diagnostics(&self) -> &[AdapterDiagnostic] {
        &self.diagnostics
    }

    pub(crate) fn has_unscoped_permanent_diagnostic(&self) -> bool {
        self.unscoped_permanent_diagnostic
    }

    pub(crate) fn diagnostic_coverage_gaps(&self) -> &BTreeSet<CapabilityId> {
        &self.diagnostic_coverage_gaps
    }

    pub fn dependency_reads(&self) -> &[DependencyRevision] {
        &self.dependency_reads
    }

    pub fn add_dependency_read(
        &mut self,
        dependency: DependencyRevision,
    ) -> Result<(), AdapterError> {
        const MAX_DEPENDENCY_READS: usize = 256;
        if self.dependency_reads.contains(&dependency) {
            return Ok(());
        }
        if self.dependency_reads.len() == MAX_DEPENDENCY_READS {
            return Err(AdapterError::invalid_contract(format!(
                "fact batch exceeds {MAX_DEPENDENCY_READS} dependency reads"
            )));
        }
        self.dependency_reads.push(dependency);
        self.dependency_reads.sort();
        Ok(())
    }

    pub fn next_decoder_state(&self) -> Option<&[u8]> {
        self.next_decoder_state.as_deref()
    }

    /// Raw-retention policy is enforced by the common coordinator after
    /// decoding. Removing the payload does not change the fact kind or its
    /// provenance-derived identity; the durable record hash remains available.
    pub(crate) fn redact_unknown_record_payloads(&mut self) {
        self.replace_unknown_record_payloads(&[]);
    }

    pub(crate) fn replace_unknown_record_payloads(&mut self, replacement: &[u8]) {
        for envelope in &mut self.facts {
            if let Fact::UnknownRecord { raw_payload, .. } = &mut envelope.value {
                raw_payload.clear();
                raw_payload.extend_from_slice(replacement);
            }
        }
    }
}

fn logical_record_position(record: &SourceRecord) -> Vec<u8> {
    let append_range = record
        .cursor_start
        .append_offset_value()
        .zip(record.cursor_end.append_offset_value());
    let mut position = Vec::new();
    match append_range {
        Some((start, end)) => {
            position.push(1);
            position.extend_from_slice(&start.to_be_bytes());
            position.extend_from_slice(&end.to_be_bytes());
        }
        None => {
            position.push(2);
            push_component(&mut position, record.cursor_start.as_bytes());
            push_component(&mut position, record.cursor_end.as_bytes());
            position.extend_from_slice(&record.ordinal_in_batch.to_be_bytes());
        }
    }
    position
}

fn semantic_identity_error(error: super::SemanticContractError) -> AdapterError {
    AdapterError::invalid_contract(format!("invalid canonical fact identity: {error}"))
}

#[cfg(test)]
mod tests {
    use crate::adapter::AdapterErrorClass;
    use crate::source::{RecordOrigin, Revision, SourceCursor, SourceMediaType, SourceRecord};

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

    fn semantic_context() -> FactSemanticContext {
        FactSemanticContext::new(
            &AdapterId::new("fixture").unwrap(),
            1,
            b"stable-source-instance",
            b"transcript",
            b"session.jsonl",
            1,
        )
        .unwrap()
    }

    fn unknown() -> Fact {
        Fact::UnknownRecord {
            native_kind: Some("fixture".to_string()),
            raw_payload: Vec::new(),
            reason: "test".to_string(),
        }
    }

    fn exact_usage_value<T>(value: T, native_field: &str) -> UsageQualifiedValue<T> {
        QualifiedValue::from_parts(
            Some(value),
            QualifiedValueQuality::Exact,
            UsageValueAuthority::NativeResponse,
            ContractCompleteness::Complete,
            None,
            None,
            UsageValueProvenance {
                native_field: native_field.to_string(),
                normalization_contract_version: 1,
            },
        )
        .unwrap()
    }

    fn usage_v2_fact(batch: &FactBatch, input_tokens: u64) -> UsageRevisionV2Fact {
        UsageRevisionV2Fact {
            session: batch
                .canonical_entity_key("session", b"native-session")
                .unwrap(),
            actor_run: batch.canonical_entity_key("run", b"native-run").unwrap(),
            response_key: b"response-1".to_vec(),
            response_identity: UsageResponseIdentity::NativeMessageId,
            native_message_id: Some("response-1".to_string()),
            request_id: Some("request-1".to_string()),
            buckets: UsageBucketsV2 {
                input_tokens: exact_usage_value(input_tokens, "message.usage.input_tokens"),
                output_tokens: exact_usage_value(2, "message.usage.output_tokens"),
                cache_creation_input_tokens: exact_usage_value(
                    3,
                    "message.usage.cache_creation_input_tokens",
                ),
                cache_read_input_tokens: exact_usage_value(
                    4,
                    "message.usage.cache_read_input_tokens",
                ),
            },
            model: Some(exact_usage_value("model-1".to_string(), "message.model")),
            effort: None,
            source_time: Some(QualifiedTimestamp {
                value: "2026-08-16T00:00:00Z".to_string(),
                quality: TimestampQuality::NativeExact,
            }),
        }
    }

    #[test]
    fn runtime_actor_contract_keeps_lineage_and_affiliation_state_explicit() {
        let batch = FactBatch::new_with_semantic_context(1, 1, semantic_context()).unwrap();
        let session = batch.canonical_entity_key("session", b"session-1").unwrap();
        let root = batch.canonical_entity_key("run", b"root").unwrap();
        let child = batch.canonical_entity_key("run", b"child").unwrap();
        let workflow = batch
            .canonical_entity_key("workflow", b"workflow-1")
            .unwrap();
        let affiliation = batch
            .canonical_entity_key("actor_affiliation", b"child/workflow-1")
            .unwrap();

        assert!(ActorRunRevisionFact {
            actor_run: child,
            session,
            role: ActorRunRole::Child,
            parent_actor_run: Some(root),
            native_session_id: Some("session-1".to_string()),
            native_actor_id: Some("child".to_string()),
            native_actor_type: None,
        }
        .validate()
        .is_ok());
        assert!(ActorRunRevisionFact {
            actor_run: child,
            session,
            role: ActorRunRole::Child,
            parent_actor_run: None,
            native_session_id: None,
            native_actor_id: None,
            native_actor_type: None,
        }
        .validate()
        .is_err());
        assert!(ActorRunRevisionFact {
            actor_run: root,
            session,
            role: ActorRunRole::Root,
            parent_actor_run: Some(root),
            native_session_id: None,
            native_actor_id: None,
            native_actor_type: None,
        }
        .validate()
        .is_err());

        let valid_affiliation = ActorAffiliationRevisionFact {
            affiliation,
            actor_run: child,
            session,
            dimension: ActorAffiliationDimension::Workflow,
            target: workflow,
            member: None,
            native_target_id: Some("workflow-1".to_string()),
            native_member_id: Some("child".to_string()),
            state: ActorAffiliationState::Removed,
            effective_at: None,
        };
        assert!(valid_affiliation.validate().is_ok());
        assert_eq!(
            Fact::ActorAffiliationRevision(valid_affiliation.clone()).kind(),
            "runtime.actor-affiliation"
        );
        assert!(ActorAffiliationRevisionFact {
            native_member_id: Some(String::new()),
            ..valid_affiliation
        }
        .validate()
        .is_err());
    }

    #[test]
    fn actor_and_affiliation_revisions_use_canonical_value_identity() {
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
            b"{}".to_vec(),
        );
        let keys = FactBatch::new_with_semantic_context(4, 2, semantic_context()).unwrap();
        let session = keys.canonical_entity_key("session", b"session-1").unwrap();
        let root = keys
            .canonical_root_actor_run_key(b"session-1", None)
            .unwrap();
        let child = keys.canonical_entity_key("run", b"child-1").unwrap();
        let actor = ActorRunRevisionFact {
            actor_run: child,
            session,
            role: ActorRunRole::Child,
            parent_actor_run: Some(root),
            native_session_id: Some("session-1".to_string()),
            native_actor_id: Some("child-1".to_string()),
            native_actor_type: Some("subagent".to_string()),
        };
        let actor_key = actor.semantic_revision_key().unwrap();

        let mut first = FactBatch::new_with_semantic_context(2, 2, semantic_context()).unwrap();
        first
            .push_native(
                &first_record,
                b"child-1",
                Fact::ActorRunRevision(actor.clone()),
            )
            .unwrap();
        let mut second = FactBatch::new_with_semantic_context(2, 2, semantic_context()).unwrap();
        second
            .push_native(
                &second_record,
                b"child-1",
                Fact::ActorRunRevision(actor.clone()),
            )
            .unwrap();
        let first_semantic = first.facts()[0].semantic_revision.unwrap();
        let second_semantic = second.facts()[0].semantic_revision.unwrap();
        assert_ne!(
            first_semantic.source_record_id,
            second_semantic.source_record_id
        );
        assert_eq!(first_semantic.fact_id, second_semantic.fact_id);
        assert_eq!(
            first_semantic.fact_revision_id,
            second_semantic.fact_revision_id
        );
        assert_eq!(
            first_semantic.fact_revision_id,
            FactRevisionId::derive(&first_semantic.fact_id, 1, &actor_key).unwrap()
        );
        first.append(second).unwrap();
        assert_eq!(first.facts().len(), 1, "exact actor replay is idempotent");

        let mut changed_actor = actor.clone();
        changed_actor.native_actor_type = Some("workflow-child".to_string());
        assert_ne!(
            actor.semantic_revision_key().unwrap(),
            changed_actor.semantic_revision_key().unwrap()
        );
        let mut forged = FactBatch::new_with_semantic_context(1, 1, semantic_context()).unwrap();
        assert!(forged
            .push_native_with_revision(
                &first_record,
                b"child-1",
                b"source-owned-weaker-key",
                Fact::ActorRunRevision(actor.clone()),
            )
            .is_err());
        assert!(forged
            .push_derived_with_revision(
                &first_record,
                b"child-1",
                b"source-owned-weaker-key",
                Fact::ActorRunRevision(actor.clone()),
            )
            .is_err());

        let affiliation = ActorAffiliationRevisionFact {
            affiliation: keys
                .canonical_entity_key("actor_affiliation", b"child-1/team/team-1")
                .unwrap(),
            actor_run: child,
            session,
            dimension: ActorAffiliationDimension::Team,
            target: keys.canonical_entity_key("team", b"team-1").unwrap(),
            member: None,
            native_target_id: Some("team-1".to_string()),
            native_member_id: None,
            state: ActorAffiliationState::Present,
            effective_at: None,
        };
        let mut removed = affiliation.clone();
        removed.state = ActorAffiliationState::Removed;
        assert_ne!(
            affiliation.semantic_revision_key().unwrap(),
            removed.semantic_revision_key().unwrap()
        );
        let mut whitespace = affiliation;
        whitespace.native_target_id = Some(" team-1".to_string());
        assert!(whitespace.semantic_revision_key().is_err());
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
    fn canonical_revision_ignores_catalog_ids_observation_time_and_append_batch_ordinal() {
        let first = record();
        let replay = SourceRecord::new(
            &RecordOrigin {
                source_instance_id: 101,
                stream_id: 202,
                object_id: 303,
                observed_at: 9_999,
                source_timestamp_hint: None,
                media_type: SourceMediaType::new("application/json").unwrap(),
            },
            first.generation,
            first.cursor_start.clone(),
            first.cursor_end.clone(),
            77,
            first.payload.clone(),
        );
        let mut durable = FactBatch::new_with_semantic_context(4, 4, semantic_context()).unwrap();
        durable
            .push_derived(&first, b"unknown-record", unknown())
            .unwrap();
        let mut scoped = FactBatch::new_with_semantic_context(4, 4, semantic_context()).unwrap();
        scoped
            .push_derived(&replay, b"unknown-record", unknown())
            .unwrap();

        assert_ne!(durable.facts()[0].id, scoped.facts()[0].id);
        assert_eq!(
            durable.facts()[0].semantic_revision,
            scoped.facts()[0].semantic_revision
        );

        let mut reset = replay;
        reset.generation = reset.generation.checked_add(1).unwrap();
        assert_ne!(
            semantic_context().source_record_id(&first).unwrap(),
            semantic_context().source_record_id(&reset).unwrap()
        );
    }

    #[test]
    fn native_fact_keeps_identity_and_changes_revision_across_source_records() {
        let first = record();
        let second = SourceRecord::new(
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
            0,
            b"[]".to_vec(),
        );
        let mut first_batch =
            FactBatch::new_with_semantic_context(4, 4, semantic_context()).unwrap();
        first_batch
            .push_native(&first, b"native-message-1", unknown())
            .unwrap();
        let mut correction =
            FactBatch::new_with_semantic_context(4, 4, semantic_context()).unwrap();
        correction
            .push_native(&second, b"native-message-1", unknown())
            .unwrap();
        let first_semantic = first_batch.facts()[0].semantic_revision.unwrap();
        let correction_semantic = correction.facts()[0].semantic_revision.unwrap();

        assert_eq!(first_semantic.fact_id, correction_semantic.fact_id);
        assert_ne!(
            first_semantic.fact_revision_id,
            correction_semantic.fact_revision_id
        );
        assert_ne!(
            first_semantic.semantic_revision_ref,
            correction_semantic.semantic_revision_ref
        );
    }

    #[test]
    fn object_scoped_native_identity_is_topology_stable_and_generation_local() {
        let first = record();
        let topology_replay = SourceRecord::new(
            &RecordOrigin {
                source_instance_id: 101,
                stream_id: 202,
                object_id: 303,
                observed_at: 9_999,
                source_timestamp_hint: None,
                media_type: SourceMediaType::new("application/json").unwrap(),
            },
            first.generation,
            first.cursor_start.clone(),
            first.cursor_end.clone(),
            0,
            first.payload.clone(),
        );
        let mut durable = FactBatch::new_with_semantic_context(2, 2, semantic_context()).unwrap();
        let durable_session = durable
            .canonical_entity_key("session", b"native-session")
            .unwrap();
        durable
            .push_native_object_scoped(&first, b"response-1", unknown())
            .unwrap();
        let mut scoped = FactBatch::new_with_semantic_context(2, 2, semantic_context()).unwrap();
        let scoped_session = scoped
            .canonical_entity_key("session", b"native-session")
            .unwrap();
        scoped
            .push_native_object_scoped(&topology_replay, b"response-1", unknown())
            .unwrap();
        assert_eq!(durable_session, scoped_session);
        assert_eq!(
            durable.facts()[0].semantic_revision,
            scoped.facts()[0].semantic_revision
        );

        let other_object_context = FactSemanticContext::new(
            &AdapterId::new("fixture").unwrap(),
            1,
            b"stable-source-instance",
            b"transcript",
            b"other-session.jsonl",
            1,
        )
        .unwrap();
        let mut other_object =
            FactBatch::new_with_semantic_context(1, 1, other_object_context).unwrap();
        other_object
            .push_native_object_scoped(&first, b"response-1", unknown())
            .unwrap();
        assert_ne!(
            durable.facts()[0].semantic_revision.unwrap().fact_id,
            other_object.facts()[0].semantic_revision.unwrap().fact_id
        );

        let mut next_generation_record = first.clone();
        next_generation_record.generation = 2;
        let mut next_generation =
            FactBatch::new_with_semantic_context(1, 1, semantic_context()).unwrap();
        next_generation
            .push_native_object_scoped(&next_generation_record, b"response-1", unknown())
            .unwrap();
        assert_ne!(
            durable.facts()[0].semantic_revision.unwrap().fact_id,
            next_generation.facts()[0]
                .semantic_revision
                .unwrap()
                .fact_id
        );
    }

    #[test]
    fn usage_v2_semantic_revision_is_value_derived_and_batch_idempotent() {
        let first = record();
        let second = SourceRecord::new(
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
            0,
            b"[]".to_vec(),
        );
        let mut batch = FactBatch::new_with_semantic_context(4, 2, semantic_context()).unwrap();
        let original = usage_v2_fact(&batch, 1);
        let original_revision = original.semantic_revision_key().unwrap();
        let first_id = batch
            .push_native_object_scoped_with_revision(
                &first,
                b"response-1",
                &original_revision,
                Fact::UsageRevisionV2(original.clone()),
            )
            .unwrap();
        let repeated_id = batch
            .push_native_object_scoped_with_revision(
                &second,
                b"response-1",
                &original_revision,
                Fact::UsageRevisionV2(original),
            )
            .unwrap();
        assert_eq!(first_id, repeated_id);
        assert_eq!(batch.facts().len(), 1);

        let correction = usage_v2_fact(&batch, 2);
        let correction_revision = correction.semantic_revision_key().unwrap();
        assert_ne!(correction_revision, original_revision);
        batch
            .push_native_object_scoped_with_revision(
                &second,
                b"response-1",
                &correction_revision,
                Fact::UsageRevisionV2(correction),
            )
            .unwrap();
        assert_eq!(batch.facts().len(), 2);
        let before = batch.facts()[0].semantic_revision.unwrap();
        let after = batch.facts()[1].semantic_revision.unwrap();
        assert_eq!(before.fact_id, after.fact_id);
        assert_ne!(before.fact_revision_id, after.fact_revision_id);
    }

    #[test]
    fn usage_v2_semantic_revision_is_idempotent_across_batch_merge() {
        let first = record();
        let second = SourceRecord::new(
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
            0,
            b"[]".to_vec(),
        );
        let mut merged = FactBatch::new_with_semantic_context(4, 2, semantic_context()).unwrap();
        let value = usage_v2_fact(&merged, 1);
        let revision = value.semantic_revision_key().unwrap();
        merged
            .push_native_object_scoped_with_revision(
                &first,
                b"response-1",
                &revision,
                Fact::UsageRevisionV2(value.clone()),
            )
            .unwrap();
        let mut repeated = FactBatch::new_with_semantic_context(4, 2, semantic_context()).unwrap();
        repeated
            .push_native_object_scoped_with_revision(
                &second,
                b"response-1",
                &revision,
                Fact::UsageRevisionV2(value.clone()),
            )
            .unwrap();
        assert_ne!(
            merged.facts()[0]
                .semantic_revision
                .unwrap()
                .source_record_id,
            repeated.facts()[0]
                .semantic_revision
                .unwrap()
                .source_record_id
        );
        merged.append(repeated).unwrap();
        assert_eq!(merged.facts().len(), 1);
        assert_eq!(merged.semantic_revisions.len(), 1);

        let mut conflicting =
            FactBatch::new_with_semantic_context(4, 2, semantic_context()).unwrap();
        conflicting
            .push_native_object_scoped_with_revision(
                &second,
                b"response-1",
                &revision,
                Fact::UsageRevisionV2(value),
            )
            .unwrap();
        let Fact::UsageRevisionV2(conflicting_value) =
            &mut conflicting.facts.get_mut(0).unwrap().value
        else {
            unreachable!()
        };
        conflicting_value.buckets.input_tokens.value = Some(99);
        assert!(merged.append(conflicting).is_err());
    }

    #[test]
    fn usage_v2_semantic_revision_covers_normalized_metadata() {
        let batch = FactBatch::new_with_semantic_context(1, 1, semantic_context()).unwrap();
        let original = usage_v2_fact(&batch, 1);
        let original_revision = original.semantic_revision_key().unwrap();

        let mut variants = Vec::new();
        let mut request = original.clone();
        request.request_id = Some("request-2".to_string());
        variants.push(request);
        let mut qualification = original.clone();
        qualification.buckets.input_tokens.quality = QualifiedValueQuality::Derived;
        variants.push(qualification);
        let mut effective_at = original.clone();
        effective_at.buckets.input_tokens.effective_at = Some(42);
        variants.push(effective_at);
        let mut provenance = original.clone();
        provenance
            .buckets
            .input_tokens
            .provenance
            .normalization_contract_version = 2;
        variants.push(provenance);
        let mut model = original.clone();
        model.model.as_mut().unwrap().value = Some("model-2".to_string());
        variants.push(model);
        let mut effort = original.clone();
        effort.effort = Some(exact_usage_value("high".to_string(), "message.effort"));
        variants.push(effort);
        let mut source_time = original.clone();
        source_time.source_time.as_mut().unwrap().value = "2026-08-16T00:00:01Z".to_string();
        variants.push(source_time);
        let mut actor = original.clone();
        actor.actor_run = batch.canonical_entity_key("run", b"other-run").unwrap();
        variants.push(actor);

        for variant in variants {
            assert_ne!(variant.semantic_revision_key().unwrap(), original_revision);
        }
    }

    #[test]
    fn explicit_dependency_revision_changes_revision_without_rekeying_fact() {
        let record = record();
        let mut before = FactBatch::new_with_semantic_context(2, 2, semantic_context()).unwrap();
        before
            .push_native_with_revision(
                &record,
                b"native-message-1",
                b"dependency-revision-1",
                unknown(),
            )
            .unwrap();
        let mut after = FactBatch::new_with_semantic_context(2, 2, semantic_context()).unwrap();
        after
            .push_native_with_revision(
                &record,
                b"native-message-1",
                b"dependency-revision-2",
                unknown(),
            )
            .unwrap();
        let before = before.facts()[0].semantic_revision.unwrap();
        let after = after.facts()[0].semantic_revision.unwrap();

        assert_eq!(before.source_record_id, after.source_record_id);
        assert_eq!(before.fact_id, after.fact_id);
        assert_ne!(before.fact_revision_id, after.fact_revision_id);

        let mut invalid = FactBatch::new_with_semantic_context(1, 1, semantic_context()).unwrap();
        assert!(invalid
            .push_native_with_revision(&record, b"native-message-1", b"", unknown())
            .is_err());
        assert!(invalid.facts().is_empty());
    }

    #[test]
    fn snapshot_record_ordinal_is_part_of_canonical_record_identity() {
        let cursor_start = SourceCursor::snapshot(Revision::digest(b"before"));
        let cursor_end = SourceCursor::snapshot(Revision::digest(b"after"));
        let origin = RecordOrigin {
            source_instance_id: 1,
            stream_id: 2,
            object_id: 3,
            observed_at: 4,
            source_timestamp_hint: None,
            media_type: SourceMediaType::new("application/json").unwrap(),
        };
        let first = SourceRecord::new(
            &origin,
            2,
            cursor_start.clone(),
            cursor_end.clone(),
            0,
            b"same".to_vec(),
        );
        let second = SourceRecord::new(&origin, 2, cursor_start, cursor_end, 1, b"same".to_vec());

        assert_ne!(
            semantic_context().source_record_id(&first).unwrap(),
            semantic_context().source_record_id(&second).unwrap()
        );
    }

    #[test]
    fn duplicate_canonical_revision_is_rejected_without_consuming_legacy_ordinal() {
        let record = record();
        let mut batch = FactBatch::new_with_semantic_context(4, 4, semantic_context()).unwrap();
        batch.push_derived(&record, b"first", unknown()).unwrap();
        assert!(batch.push_derived(&record, b"first", unknown()).is_err());
        batch.push_derived(&record, b"second", unknown()).unwrap();

        assert_eq!(batch.facts().len(), 2);
        assert_eq!(batch.facts()[1].provenance.local_fact_ordinal, 1);
    }

    #[test]
    fn canonical_emission_without_bound_context_fails_closed() {
        let mut batch = FactBatch::new(1, 1).unwrap();
        assert!(batch
            .push_native(&record(), b"native-message-1", unknown())
            .is_err());
        assert!(batch.facts().is_empty());
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
    fn append_rejects_batches_that_would_exceed_diagnostic_bound() {
        let record = record();
        let diagnostic = || AdapterDiagnostic {
            class: AdapterErrorClass::RecordPermanent,
            code: "overflow".to_string(),
            message: "too many".to_string(),
        };
        let mut left = FactBatch::new(8, 2).unwrap();
        left.push_diagnostic(diagnostic()).unwrap();
        left.push_diagnostic(diagnostic()).unwrap();
        let mut right = FactBatch::new(8, 2).unwrap();
        right
            .push(
                &record,
                Fact::UnknownRecord {
                    native_kind: None,
                    raw_payload: Vec::new(),
                    reason: "overflow".to_string(),
                },
            )
            .unwrap();
        right.push_diagnostic(diagnostic()).unwrap();
        assert!(!left.can_append(&right));
        assert!(left.append(right).is_err());
    }

    #[test]
    fn artifact_content_json_round_trip_preserves_arbitrary_bytes() {
        let adapter_id = AdapterId::new("test-adapter").unwrap();
        let artifact = EntityKey::native(&adapter_id, 1, "artifact", b"backup@v1").unwrap();
        let session = EntityKey::native(&adapter_id, 1, "session", b"session-1").unwrap();
        let fact = Fact::ArtifactContent(ArtifactContentFact {
            artifact,
            session,
            canonical_artifact: None,
            canonical_session: None,
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

    #[test]
    fn canonical_artifact_metadata_revision_is_order_invariant_and_fail_closed() {
        let context = semantic_context();
        let adapter_id = AdapterId::new("fixture").unwrap();
        let legacy_session =
            EntityKey::native(&adapter_id, 1, "session", b"native-session").unwrap();
        let canonical_session = context
            .canonical_entity_key("session", b"native-session")
            .unwrap();
        let timestamp = |value: &str| QualifiedTimestamp {
            value: value.to_string(),
            quality: TimestampQuality::NativeExact,
        };
        let entry = |name: &str, version: u64| ArtifactMetadataEntry {
            artifact: EntityKey::native(&adapter_id, 1, "artifact", name.as_bytes()).unwrap(),
            canonical_artifact: Some(
                context
                    .canonical_entity_key("artifact", name.as_bytes())
                    .unwrap(),
            ),
            native_artifact_id: Some(name.to_string()),
            tracking_path: format!("src/{name}"),
            real_parent_dir: Some("/fixture/src".to_string()),
            version,
            backup_time: timestamp("2026-08-18T00:00:00Z"),
            capture: ArtifactCapture::ContentExpected,
        };
        let first = entry("a@v1", 1);
        let second = entry("b@v2", 2);
        let fact = |artifacts: Vec<ArtifactMetadataEntry>| ArtifactMetadataSnapshotFact {
            session: legacy_session.clone(),
            canonical_session: Some(canonical_session),
            native_message_id: "message-1".to_string(),
            native_snapshot_message_id: "snapshot-1".to_string(),
            observation_kind: ArtifactObservationKind::Checkpoint,
            is_snapshot_update: true,
            source_time: Some(timestamp("2026-08-18T00:00:01Z")),
            artifacts,
        };
        let forward = fact(vec![first.clone(), second.clone()]);
        let reversed = fact(vec![second, first]);
        assert_eq!(
            forward.semantic_revision_key().unwrap(),
            reversed.semantic_revision_key().unwrap()
        );

        let mut partial = forward.clone();
        partial.artifacts[0].canonical_artifact = None;
        assert!(partial.semantic_revision_key().is_err());
        let mut duplicate = forward;
        duplicate.artifacts[1].canonical_artifact = duplicate.artifacts[0].canonical_artifact;
        assert!(duplicate.semantic_revision_key().is_err());
    }

    #[test]
    fn canonical_artifact_content_revision_binds_value_not_legacy_topology() {
        let context = semantic_context();
        let adapter_id = AdapterId::new("fixture").unwrap();
        let canonical_artifact = context
            .canonical_entity_key("artifact", b"backup@v1")
            .unwrap();
        let canonical_session = context
            .canonical_entity_key("session", b"native-session")
            .unwrap();
        let fact = |source_instance_id, content: &[u8]| ArtifactContentFact {
            artifact: EntityKey::native(&adapter_id, source_instance_id, "artifact", b"backup@v1")
                .unwrap(),
            session: EntityKey::native(
                &adapter_id,
                source_instance_id,
                "session",
                b"native-session",
            )
            .unwrap(),
            canonical_artifact: Some(canonical_artifact),
            canonical_session: Some(canonical_session),
            native_artifact_id: "backup@v1".to_string(),
            native_file_hash: "backup".to_string(),
            version: 1,
            content: content.to_vec(),
            size_bytes: content.len() as u64,
        };
        let first = fact(1, b"content");
        let topology_replay = fact(99, b"content");
        assert_ne!(first.artifact, topology_replay.artifact);
        assert_eq!(
            first.semantic_revision_key().unwrap(),
            topology_replay.semantic_revision_key().unwrap()
        );
        assert_ne!(
            first.semantic_revision_key().unwrap(),
            fact(1, b"changed").semantic_revision_key().unwrap()
        );

        let mut invalid = first;
        invalid.canonical_session = None;
        assert!(invalid.semantic_revision_key().is_err());
    }

    #[test]
    fn entity_keys_use_compact_base64_and_accept_legacy_byte_arrays() {
        let key = EntityKey(vec![0, 1, 2, 250, 255]);
        assert_eq!(serde_json::to_string(&key).unwrap(), r#""AAEC+v8=""#);
        assert_eq!(
            serde_json::from_str::<EntityKey>("[0,1,2,250,255]").unwrap(),
            key
        );
    }

    #[test]
    fn message_audit_json_omits_the_single_copy_native_payload() {
        let key = EntityKey(vec![1, 2, 3]);
        let fact = Fact::Message(MessageFact {
            message: key.clone(),
            session: key.clone(),
            run: key,
            native_message_id: Some("m1".to_string()),
            native_kind: "assistant".to_string(),
            role: MessageRole::Assistant,
            content: vec![ContentBlock::Text {
                text: "visible".to_string(),
            }],
            source_time: None,
            parent_native_message_id: None,
            model: None,
            search_text: Some("visible".to_string()),
            raw_json: br#"{"secret":"native-only"}"#.to_vec(),
        });

        let encoded = serde_json::to_value(&fact).unwrap();
        assert!(encoded["Message"].get("raw_json").is_none());
        let decoded = serde_json::from_value::<Fact>(encoded).unwrap();
        let Fact::Message(decoded) = decoded else {
            panic!("expected message fact");
        };
        assert!(decoded.raw_json.is_empty());
        assert_eq!(decoded.search_text.as_deref(), Some("visible"));
    }

    #[test]
    fn unknown_payload_redaction_preserves_fact_identity_and_provenance_hash() {
        let record = record();
        let mut batch = FactBatch::new(1, 1).unwrap();
        let fact_id = batch
            .push(
                &record,
                Fact::UnknownRecord {
                    native_kind: Some("future".to_string()),
                    raw_payload: b"sensitive".to_vec(),
                    reason: "future decoder".to_string(),
                },
            )
            .unwrap();
        let payload_hash = batch.facts()[0].provenance.record_hash;

        batch.redact_unknown_record_payloads();

        assert_eq!(batch.facts()[0].id, fact_id);
        assert_eq!(batch.facts()[0].provenance.record_hash, payload_hash);
        let Fact::UnknownRecord { raw_payload, .. } = &batch.facts()[0].value else {
            panic!("expected unknown record");
        };
        assert!(raw_payload.is_empty());
    }
}
