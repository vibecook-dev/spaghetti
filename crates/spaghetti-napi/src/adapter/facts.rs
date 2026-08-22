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
const MAX_EFFECTIVE_STATE_PROVENANCE_FIELD_BYTES: usize = 256;
const JS_SAFE_INTEGER_MAX_U64: u64 = 9_007_199_254_740_991;
const JS_SAFE_INTEGER_MAX_I64: i64 = 9_007_199_254_740_991;
const MAX_RUNTIME_SEMANTIC_TEXT_BYTES: usize = 8 * 1024;
const MAX_CANONICAL_ARTIFACTS_PER_FACT: usize = 4 * 1024;
const MAX_USER_INPUT_QUESTIONS: usize = 32;
const MAX_USER_INPUT_OPTIONS: usize = 32;
const MAX_RECORD_MAPPINGS_PER_BATCH: usize = 65_536;

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

    pub(crate) fn framing_contract_version(&self) -> u32 {
        self.framing_contract_version
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

/// RFC 012C structured user-input request. One correlated lifecycle entity
/// identified by native tool-use id; questions are typed rather than native
/// payload fragments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserInputKind {
    Choice,
    MultiChoice,
    FreeText,
    Mixed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserInputLifecycleState {
    #[serde(alias = "open")]
    Pending,
    Resolved,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserInputOperation {
    Upsert,
    Retract,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserInputOption {
    pub label: String,
    pub description: Option<String>,
    pub preview: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserInputQuestion {
    pub header: Option<String>,
    pub prompt: String,
    pub options: Vec<UserInputOption>,
    pub multi_select: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserInputRequestRevisionFact {
    pub session: CanonicalEntityKey,
    pub actor_run: CanonicalEntityKey,
    pub native_tool_use_id: String,
    pub kind: UserInputKind,
    pub questions: Vec<UserInputQuestion>,
    pub state: UserInputLifecycleState,
    pub operation: UserInputOperation,
    pub completeness: ContractCompleteness,
    pub result_reference: Option<String>,
}

impl UserInputRequestRevisionFact {
    pub(crate) fn validate(&self) -> Result<(), AdapterError> {
        if self.native_tool_use_id.is_empty()
            || self.native_tool_use_id.len() > MAX_RUNTIME_SEMANTIC_TEXT_BYTES
            || self.native_tool_use_id.trim() != self.native_tool_use_id
        {
            return Err(AdapterError::invalid_contract(
                "user-input native_tool_use_id must be canonical bounded text",
            ));
        }
        if self.questions.is_empty() || self.questions.len() > MAX_USER_INPUT_QUESTIONS {
            return Err(AdapterError::invalid_contract(format!(
                "user-input questions must contain 1..={MAX_USER_INPUT_QUESTIONS} typed questions"
            )));
        }
        for question in &self.questions {
            validate_runtime_semantic_text("question header", question.header.as_deref())?;
            validate_runtime_semantic_text("question prompt", Some(question.prompt.as_str()))?;
            if question.prompt.is_empty() {
                return Err(AdapterError::invalid_contract(
                    "user-input question prompt must not be empty",
                ));
            }
            if question.options.len() > MAX_USER_INPUT_OPTIONS {
                return Err(AdapterError::invalid_contract(format!(
                    "user-input question options exceed {MAX_USER_INPUT_OPTIONS}"
                )));
            }
            for option in &question.options {
                validate_runtime_semantic_text("option label", Some(option.label.as_str()))?;
                if option.label.is_empty() {
                    return Err(AdapterError::invalid_contract(
                        "user-input option label must not be empty",
                    ));
                }
                validate_runtime_semantic_text(
                    "option description",
                    option.description.as_deref(),
                )?;
                validate_runtime_semantic_text("option preview", option.preview.as_deref())?;
            }
        }
        if self.state == UserInputLifecycleState::Pending && self.result_reference.is_some() {
            return Err(AdapterError::invalid_contract(
                "pending user-input cannot carry a result_reference",
            ));
        }
        if self.state == UserInputLifecycleState::Resolved && self.result_reference.is_none() {
            return Err(AdapterError::invalid_contract(
                "resolved user-input requires a typed result_reference",
            ));
        }
        validate_runtime_semantic_text("result_reference", self.result_reference.as_deref())?;
        Ok(())
    }

    pub(crate) fn semantic_revision_key(&self) -> Result<[u8; FACT_HASH_BYTES], AdapterError> {
        self.validate()?;
        let mut encoded = Vec::new();
        encoded.extend_from_slice(b"spaghetti/runtime.user-input-request/semantic-revision\0");
        encoded.extend_from_slice(&1_u32.to_be_bytes());
        push_component(&mut encoded, self.session.as_bytes());
        push_component(&mut encoded, self.actor_run.as_bytes());
        push_component(&mut encoded, self.native_tool_use_id.as_bytes());
        encoded.push(match self.kind {
            UserInputKind::Choice => 1,
            UserInputKind::MultiChoice => 2,
            UserInputKind::FreeText => 3,
            UserInputKind::Mixed => 4,
        });
        encoded.extend_from_slice(&(self.questions.len() as u64).to_be_bytes());
        for question in &self.questions {
            push_optional_component(&mut encoded, question.header.as_deref().map(str::as_bytes));
            push_component(&mut encoded, question.prompt.as_bytes());
            encoded.push(u8::from(question.multi_select));
            encoded.extend_from_slice(&(question.options.len() as u64).to_be_bytes());
            for option in &question.options {
                push_component(&mut encoded, option.label.as_bytes());
                push_optional_component(
                    &mut encoded,
                    option.description.as_deref().map(str::as_bytes),
                );
                push_optional_component(&mut encoded, option.preview.as_deref().map(str::as_bytes));
            }
        }
        encoded.push(match self.state {
            UserInputLifecycleState::Pending => 1,
            UserInputLifecycleState::Resolved => 2,
            UserInputLifecycleState::Failed => 3,
            UserInputLifecycleState::Cancelled => 4,
        });
        encoded.push(match self.operation {
            UserInputOperation::Upsert => 1,
            UserInputOperation::Retract => 2,
        });
        encoded.push(match self.completeness {
            ContractCompleteness::Complete => 1,
            ContractCompleteness::Partial => 2,
            ContractCompleteness::Unknown => 3,
        });
        push_optional_component(
            &mut encoded,
            self.result_reference.as_deref().map(str::as_bytes),
        );
        Ok(*blake3::hash(&encoded).as_bytes())
    }
}

const MAX_MESSAGE_CONTENT_BLOCKS: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRevisionRole {
    User,
    Assistant,
    System,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskLifecycleState {
    Created,
    Updated,
    Completed,
    Failed,
    Cancelled,
    Removed,
}

/// RFC 012C current-generation message. Ordered block keys are the complete
/// snapshot when completeness is complete; a partial list cannot prove absence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageRevisionFact {
    pub session: CanonicalEntityKey,
    pub actor_run: CanonicalEntityKey,
    pub native_message_id: String,
    pub role: MessageRevisionRole,
    pub ordered_content_block_keys: Vec<String>,
    pub completeness: ContractCompleteness,
    pub operation: UserInputOperation,
}

impl MessageRevisionFact {
    pub(crate) fn validate(&self) -> Result<(), AdapterError> {
        validate_runtime_semantic_text("native_message_id", Some(self.native_message_id.as_str()))?;
        if self.ordered_content_block_keys.is_empty()
            || self.ordered_content_block_keys.len() > MAX_MESSAGE_CONTENT_BLOCKS
        {
            return Err(AdapterError::invalid_contract(
                "message ordered_content_block_keys must be a bounded non-empty snapshot",
            ));
        }
        let mut seen = BTreeSet::new();
        for key in &self.ordered_content_block_keys {
            validate_runtime_semantic_text("content block key", Some(key.as_str()))?;
            if !seen.insert(key.as_str()) {
                return Err(AdapterError::invalid_contract(
                    "message content block keys must be unique in one snapshot",
                ));
            }
        }
        Ok(())
    }

    pub(crate) fn semantic_revision_key(&self) -> Result<[u8; FACT_HASH_BYTES], AdapterError> {
        self.validate()?;
        let mut encoded = Vec::new();
        encoded.extend_from_slice(b"spaghetti/runtime.message/semantic-revision\0");
        encoded.extend_from_slice(&1_u32.to_be_bytes());
        push_component(&mut encoded, self.session.as_bytes());
        push_component(&mut encoded, self.actor_run.as_bytes());
        push_component(&mut encoded, self.native_message_id.as_bytes());
        encoded.push(match self.role {
            MessageRevisionRole::User => 1,
            MessageRevisionRole::Assistant => 2,
            MessageRevisionRole::System => 3,
        });
        encoded.extend_from_slice(&(self.ordered_content_block_keys.len() as u64).to_be_bytes());
        for key in &self.ordered_content_block_keys {
            push_component(&mut encoded, key.as_bytes());
        }
        encoded.push(match self.operation {
            UserInputOperation::Upsert => 1,
            UserInputOperation::Retract => 2,
        });
        encoded.push(match self.completeness {
            ContractCompleteness::Complete => 1,
            ContractCompleteness::Partial => 2,
            ContractCompleteness::Unknown => 3,
        });
        Ok(*blake3::hash(&encoded).as_bytes())
    }
}

/// Closed, bounded RFC 012C content-block payload. Structured native values
/// cross the common boundary only as typed fields or a digest-bound extension;
/// arbitrary native JSON remains retention-policy evidence, not semantic
/// reducer state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ContentBlockRevisionValue {
    Text {
        text: String,
    },
    Thinking {
        text: String,
        redacted: bool,
    },
    ToolCall {
        tool_name: String,
        input_digest: [u8; 32],
    },
    ToolResult {
        content_digest: [u8; 32],
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
    NativeExtension {
        native_kind: String,
        value_digest: [u8; 32],
    },
}

/// One independently replaceable content block in a current-generation
/// message log. `message` is the topology-neutral canonical message fact key;
/// block identity itself is supplied to `FactBatch` by a declared native key
/// or a deterministic message/ordinal fallback.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContentBlockRevisionFact {
    pub session: CanonicalEntityKey,
    pub actor_run: CanonicalEntityKey,
    pub message: CanonicalFactId,
    pub native_content_block_id: Option<String>,
    pub ordinal: u32,
    pub content: ContentBlockRevisionValue,
    pub native_tool_call_or_result_id: Option<String>,
    pub completeness: ContractCompleteness,
    pub operation: UserInputOperation,
}

impl ContentBlockRevisionFact {
    pub(crate) fn validate(&self) -> Result<(), AdapterError> {
        validate_runtime_semantic_text(
            "native_content_block_id",
            self.native_content_block_id.as_deref(),
        )?;
        validate_runtime_semantic_text(
            "native_tool_call_or_result_id",
            self.native_tool_call_or_result_id.as_deref(),
        )?;
        let is_tool_content = match &self.content {
            ContentBlockRevisionValue::Text { text }
            | ContentBlockRevisionValue::Thinking { text, .. } => {
                validate_bounded_content_text(text)?;
                false
            }
            ContentBlockRevisionValue::ToolCall {
                tool_name,
                input_digest,
            } => {
                validate_runtime_semantic_text("content block tool_name", Some(tool_name))?;
                validate_content_digest("tool-call input", input_digest)?;
                true
            }
            ContentBlockRevisionValue::ToolResult { content_digest, .. } => {
                validate_content_digest("tool-result content", content_digest)?;
                true
            }
            ContentBlockRevisionValue::Image {
                media_type,
                data_hash,
            }
            | ContentBlockRevisionValue::Document {
                media_type,
                data_hash,
            } => {
                validate_content_media_type(media_type)?;
                validate_content_digest("binary content", data_hash)?;
                false
            }
            ContentBlockRevisionValue::NativeExtension {
                native_kind,
                value_digest,
            } => {
                if !is_bounded_native_field(native_kind, MAX_EFFECTIVE_STATE_PROVENANCE_FIELD_BYTES)
                {
                    return Err(AdapterError::invalid_contract(
                        "content block native extension kind must be a bounded machine identifier",
                    ));
                }
                validate_content_digest("native extension", value_digest)?;
                false
            }
        };
        if self.native_tool_call_or_result_id.is_some() && !is_tool_content {
            return Err(AdapterError::invalid_contract(
                "content block tool identity may be present only for tool call/result content",
            ));
        }
        Ok(())
    }

    /// Stable identity input for a block inside one canonical message. Native
    /// block IDs are commonly only message-local; the message fact identity is
    /// therefore always part of the key. Blocks without a stable native ID use
    /// their declared ordinal as the deterministic fallback.
    pub(crate) fn stable_native_fact_key(&self) -> Result<Vec<u8>, AdapterError> {
        self.validate()?;
        let mut encoded = Vec::new();
        encoded.extend_from_slice(b"spaghetti/runtime.content-block/stable-native-key\0");
        encoded.extend_from_slice(&1_u32.to_be_bytes());
        push_component(&mut encoded, self.message.as_bytes());
        match self.native_content_block_id.as_deref() {
            Some(native_id) => {
                encoded.push(1);
                push_component(&mut encoded, native_id.as_bytes());
            }
            None => {
                encoded.push(2);
                encoded.extend_from_slice(&self.ordinal.to_be_bytes());
            }
        }
        Ok(encoded)
    }

    pub(crate) fn semantic_revision_key(&self) -> Result<[u8; FACT_HASH_BYTES], AdapterError> {
        self.validate()?;
        let mut encoded = Vec::new();
        encoded.extend_from_slice(b"spaghetti/runtime.content-block/semantic-revision\0");
        encoded.extend_from_slice(&1_u32.to_be_bytes());
        push_component(&mut encoded, self.session.as_bytes());
        push_component(&mut encoded, self.actor_run.as_bytes());
        push_component(&mut encoded, self.message.as_bytes());
        push_optional_component(
            &mut encoded,
            self.native_content_block_id.as_deref().map(str::as_bytes),
        );
        encoded.extend_from_slice(&self.ordinal.to_be_bytes());
        push_content_block_revision_value(&mut encoded, &self.content);
        push_optional_component(
            &mut encoded,
            self.native_tool_call_or_result_id
                .as_deref()
                .map(str::as_bytes),
        );
        encoded.push(match self.operation {
            UserInputOperation::Upsert => 1,
            UserInputOperation::Retract => 2,
        });
        encoded.push(match self.completeness {
            ContractCompleteness::Complete => 1,
            ContractCompleteness::Partial => 2,
            ContractCompleteness::Unknown => 3,
        });
        Ok(*blake3::hash(&encoded).as_bytes())
    }
}

fn validate_content_digest(field: &str, digest: &[u8; 32]) -> Result<(), AdapterError> {
    if digest.iter().all(|byte| *byte == 0) {
        return Err(AdapterError::invalid_contract(format!(
            "content block {field} digest must be nonzero"
        )));
    }
    Ok(())
}

fn validate_bounded_content_text(value: &str) -> Result<(), AdapterError> {
    if value.len() > MAX_RUNTIME_SEMANTIC_TEXT_BYTES {
        return Err(AdapterError::invalid_contract(format!(
            "content block text exceeds {MAX_RUNTIME_SEMANTIC_TEXT_BYTES} UTF-8 bytes"
        )));
    }
    Ok(())
}

fn validate_content_media_type(value: &str) -> Result<(), AdapterError> {
    let Some((kind, subtype)) = value.split_once('/') else {
        return Err(AdapterError::invalid_contract(
            "content block media_type must be a canonical MIME type",
        ));
    };
    let valid_part = |part: &str| {
        !part.is_empty()
            && part.len() <= 127
            && part.bytes().all(|byte| {
                matches!(
                    byte,
                    b'a'..=b'z'
                        | b'0'..=b'9'
                        | b'!'
                        | b'#'
                        | b'$'
                        | b'&'
                        | b'^'
                        | b'_'
                        | b'.'
                        | b'+'
                        | b'-'
                )
            })
    };
    if !valid_part(kind) || !valid_part(subtype) || subtype.contains('/') {
        return Err(AdapterError::invalid_contract(
            "content block media_type must be a canonical MIME type",
        ));
    }
    Ok(())
}

fn push_content_block_revision_value(output: &mut Vec<u8>, value: &ContentBlockRevisionValue) {
    match value {
        ContentBlockRevisionValue::Text { text } => {
            output.push(1);
            push_component(output, text.as_bytes());
        }
        ContentBlockRevisionValue::Thinking { text, redacted } => {
            output.push(2);
            push_component(output, text.as_bytes());
            output.push(u8::from(*redacted));
        }
        ContentBlockRevisionValue::ToolCall {
            tool_name,
            input_digest,
        } => {
            output.push(3);
            push_component(output, tool_name.as_bytes());
            output.extend_from_slice(input_digest);
        }
        ContentBlockRevisionValue::ToolResult {
            content_digest,
            is_error,
        } => {
            output.push(4);
            output.extend_from_slice(content_digest);
            output.push(u8::from(*is_error));
        }
        ContentBlockRevisionValue::Image {
            media_type,
            data_hash,
        } => {
            output.push(5);
            push_component(output, media_type.as_bytes());
            output.extend_from_slice(data_hash);
        }
        ContentBlockRevisionValue::Document {
            media_type,
            data_hash,
        } => {
            output.push(6);
            push_component(output, media_type.as_bytes());
            output.extend_from_slice(data_hash);
        }
        ContentBlockRevisionValue::NativeExtension {
            native_kind,
            value_digest,
        } => {
            output.push(7);
            push_component(output, native_kind.as_bytes());
            output.extend_from_slice(value_digest);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeCompactionPhase {
    Started,
    Boundary,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeProgressState {
    Pending,
    Active,
    Waiting,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeQueueOperation {
    Enqueue,
    Dequeue,
    Drain,
    Remove,
}

/// Closed, typed native marker values. Host assessments deliberately have no
/// representation here; they belong to a distinct future assessment family.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum NativeRuntimeMarkerValue {
    Compaction {
        phase: NativeCompactionPhase,
        trigger: Option<String>,
        pre_tokens: Option<u64>,
    },
    Progress {
        state: NativeProgressState,
        completed: Option<u64>,
        total: Option<u64>,
        detail_digest: Option<[u8; 32]>,
    },
    Queue {
        operation: NativeQueueOperation,
        depth: Option<u64>,
        item_digest: Option<[u8; 32]>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeRuntimeMarkerProvenance {
    pub native_field: String,
    pub normalization_contract_version: u32,
}

/// One native compaction/progress/queue marker in the current source
/// generation. Corrections retain `native_marker_id`; complete retraction or
/// source-generation loss removes the entity. Quality is intentionally
/// restricted to exact/native-claimed evidence so a host heuristic cannot be
/// serialized as a native marker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeRuntimeMarkerRevisionFact {
    pub session: CanonicalEntityKey,
    pub actor_run: CanonicalEntityKey,
    pub native_marker_id: String,
    pub correlated_native_id: Option<String>,
    pub value: NativeRuntimeMarkerValue,
    pub quality: QualifiedValueQuality,
    pub effective_at: Option<i64>,
    pub provenance: NativeRuntimeMarkerProvenance,
    pub completeness: ContractCompleteness,
    pub operation: UserInputOperation,
}

impl NativeRuntimeMarkerRevisionFact {
    pub(crate) fn validate(&self) -> Result<(), AdapterError> {
        validate_runtime_semantic_text("native_marker_id", Some(&self.native_marker_id))?;
        validate_runtime_semantic_text(
            "correlated_native_id",
            self.correlated_native_id.as_deref(),
        )?;
        if !matches!(
            self.quality,
            QualifiedValueQuality::Exact | QualifiedValueQuality::NativeClaimed
        ) {
            return Err(AdapterError::invalid_contract(
                "native runtime marker quality must be exact or native_claimed",
            ));
        }
        if self.effective_at.is_some_and(|effective_at| {
            !(-JS_SAFE_INTEGER_MAX_I64..=JS_SAFE_INTEGER_MAX_I64).contains(&effective_at)
        }) {
            return Err(AdapterError::invalid_contract(
                "native runtime marker effective_at exceeds the portable safe-integer range",
            ));
        }
        if !is_bounded_native_field(
            &self.provenance.native_field,
            MAX_EFFECTIVE_STATE_PROVENANCE_FIELD_BYTES,
        ) || self.provenance.normalization_contract_version == 0
        {
            return Err(AdapterError::invalid_contract(
                "native runtime marker provenance is empty, oversized, uncanonical, or unversioned",
            ));
        }
        match &self.value {
            NativeRuntimeMarkerValue::Compaction {
                trigger,
                pre_tokens,
                ..
            } => {
                if let Some(trigger) = trigger {
                    if !is_bounded_native_field(trigger, MAX_EFFECTIVE_STATE_PROVENANCE_FIELD_BYTES)
                    {
                        return Err(AdapterError::invalid_contract(
                            "native compaction trigger must be a bounded machine identifier",
                        ));
                    }
                }
                validate_native_marker_counter("compaction pre_tokens", *pre_tokens)?;
            }
            NativeRuntimeMarkerValue::Progress {
                completed,
                total,
                detail_digest,
                ..
            } => {
                validate_native_marker_counter("progress completed", *completed)?;
                validate_native_marker_counter("progress total", *total)?;
                if (*completed)
                    .zip(*total)
                    .is_some_and(|(completed, total)| completed > total)
                {
                    return Err(AdapterError::invalid_contract(
                        "native progress completed cannot exceed total",
                    ));
                }
                validate_optional_native_marker_digest("progress detail", detail_digest.as_ref())?;
            }
            NativeRuntimeMarkerValue::Queue {
                depth, item_digest, ..
            } => {
                validate_native_marker_counter("queue depth", *depth)?;
                validate_optional_native_marker_digest("queue item", item_digest.as_ref())?;
            }
        }
        Ok(())
    }

    pub(crate) fn stable_native_fact_key(&self) -> Result<Vec<u8>, AdapterError> {
        self.validate()?;
        let mut encoded = Vec::new();
        encoded.extend_from_slice(b"spaghetti/runtime.native-marker/stable-native-key\0");
        encoded.extend_from_slice(&1_u32.to_be_bytes());
        push_component(&mut encoded, self.session.as_bytes());
        push_component(&mut encoded, self.actor_run.as_bytes());
        encoded.push(native_runtime_marker_kind_tag(&self.value));
        push_component(&mut encoded, self.native_marker_id.as_bytes());
        Ok(encoded)
    }

    pub(crate) fn semantic_revision_key(&self) -> Result<[u8; FACT_HASH_BYTES], AdapterError> {
        self.validate()?;
        let mut encoded = Vec::new();
        encoded.extend_from_slice(b"spaghetti/runtime.native-marker/semantic-revision\0");
        encoded.extend_from_slice(&1_u32.to_be_bytes());
        push_component(&mut encoded, self.session.as_bytes());
        push_component(&mut encoded, self.actor_run.as_bytes());
        push_component(&mut encoded, self.native_marker_id.as_bytes());
        push_optional_component(
            &mut encoded,
            self.correlated_native_id.as_deref().map(str::as_bytes),
        );
        push_native_runtime_marker_value(&mut encoded, &self.value);
        encoded.push(match self.quality {
            QualifiedValueQuality::Exact => 1,
            QualifiedValueQuality::NativeClaimed => 2,
            QualifiedValueQuality::Derived
            | QualifiedValueQuality::Estimated
            | QualifiedValueQuality::Unknown => unreachable!("validate rejects non-native quality"),
        });
        match self.effective_at {
            Some(effective_at) => {
                encoded.push(1);
                encoded.extend_from_slice(&effective_at.to_be_bytes());
            }
            None => encoded.push(0),
        }
        push_component(&mut encoded, self.provenance.native_field.as_bytes());
        encoded.extend_from_slice(&self.provenance.normalization_contract_version.to_be_bytes());
        encoded.push(match self.operation {
            UserInputOperation::Upsert => 1,
            UserInputOperation::Retract => 2,
        });
        encoded.push(match self.completeness {
            ContractCompleteness::Complete => 1,
            ContractCompleteness::Partial => 2,
            ContractCompleteness::Unknown => 3,
        });
        Ok(*blake3::hash(&encoded).as_bytes())
    }
}

fn validate_native_marker_counter(field: &str, value: Option<u64>) -> Result<(), AdapterError> {
    if value.is_some_and(|value| value > JS_SAFE_INTEGER_MAX_U64) {
        return Err(AdapterError::invalid_contract(format!(
            "native runtime marker {field} exceeds the portable safe-integer range"
        )));
    }
    Ok(())
}

fn validate_optional_native_marker_digest(
    field: &str,
    digest: Option<&[u8; 32]>,
) -> Result<(), AdapterError> {
    if digest.is_some_and(|digest| digest.iter().all(|byte| *byte == 0)) {
        return Err(AdapterError::invalid_contract(format!(
            "native runtime marker {field} digest must be nonzero"
        )));
    }
    Ok(())
}

fn native_runtime_marker_kind_tag(value: &NativeRuntimeMarkerValue) -> u8 {
    match value {
        NativeRuntimeMarkerValue::Compaction { .. } => 1,
        NativeRuntimeMarkerValue::Progress { .. } => 2,
        NativeRuntimeMarkerValue::Queue { .. } => 3,
    }
}

fn push_native_runtime_marker_value(output: &mut Vec<u8>, value: &NativeRuntimeMarkerValue) {
    output.push(native_runtime_marker_kind_tag(value));
    match value {
        NativeRuntimeMarkerValue::Compaction {
            phase,
            trigger,
            pre_tokens,
        } => {
            output.push(match phase {
                NativeCompactionPhase::Started => 1,
                NativeCompactionPhase::Boundary => 2,
                NativeCompactionPhase::Completed => 3,
                NativeCompactionPhase::Failed => 4,
            });
            push_optional_component(output, trigger.as_deref().map(str::as_bytes));
            push_optional_u64(output, *pre_tokens);
        }
        NativeRuntimeMarkerValue::Progress {
            state,
            completed,
            total,
            detail_digest,
        } => {
            output.push(match state {
                NativeProgressState::Pending => 1,
                NativeProgressState::Active => 2,
                NativeProgressState::Waiting => 3,
                NativeProgressState::Completed => 4,
                NativeProgressState::Failed => 5,
                NativeProgressState::Cancelled => 6,
            });
            push_optional_u64(output, *completed);
            push_optional_u64(output, *total);
            push_optional_component(output, detail_digest.as_ref().map(|value| value.as_slice()));
        }
        NativeRuntimeMarkerValue::Queue {
            operation,
            depth,
            item_digest,
        } => {
            output.push(match operation {
                NativeQueueOperation::Enqueue => 1,
                NativeQueueOperation::Dequeue => 2,
                NativeQueueOperation::Drain => 3,
                NativeQueueOperation::Remove => 4,
            });
            push_optional_u64(output, *depth);
            push_optional_component(output, item_digest.as_ref().map(|value| value.as_slice()));
        }
    }
}

fn push_optional_u64(output: &mut Vec<u8>, value: Option<u64>) {
    match value {
        Some(value) => {
            output.push(1);
            output.extend_from_slice(&value.to_be_bytes());
        }
        None => output.push(0),
    }
}

/// RFC 012C task revision. Individual upserts are revisioned entities; a
/// complete owned-set snapshot may retract members absent from that set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRevisionFact {
    pub session: CanonicalEntityKey,
    pub actor_run: CanonicalEntityKey,
    pub native_task_id: String,
    pub subject: String,
    pub state: TaskLifecycleState,
    pub completeness: ContractCompleteness,
    pub operation: UserInputOperation,
    pub owned_set: Option<Vec<String>>,
}

impl TaskRevisionFact {
    pub(crate) fn validate(&self) -> Result<(), AdapterError> {
        validate_runtime_semantic_text("native_task_id", Some(self.native_task_id.as_str()))?;
        validate_runtime_semantic_text("subject", Some(self.subject.as_str()))?;
        if let Some(owned_set) = &self.owned_set {
            if owned_set.is_empty() {
                return Err(AdapterError::invalid_contract(
                    "task owned_set must be omitted or non-empty",
                ));
            }
            let mut seen = BTreeSet::new();
            for member in owned_set {
                validate_runtime_semantic_text("owned task id", Some(member.as_str()))?;
                if !seen.insert(member.as_str()) {
                    return Err(AdapterError::invalid_contract(
                        "task owned_set members must be unique",
                    ));
                }
            }
        }
        Ok(())
    }

    pub(crate) fn semantic_revision_key(&self) -> Result<[u8; FACT_HASH_BYTES], AdapterError> {
        self.validate()?;
        let mut encoded = Vec::new();
        encoded.extend_from_slice(b"spaghetti/runtime.task/semantic-revision\0");
        encoded.extend_from_slice(&1_u32.to_be_bytes());
        push_component(&mut encoded, self.session.as_bytes());
        push_component(&mut encoded, self.actor_run.as_bytes());
        push_component(&mut encoded, self.native_task_id.as_bytes());
        push_component(&mut encoded, self.subject.as_bytes());
        encoded.push(match self.state {
            TaskLifecycleState::Created => 1,
            TaskLifecycleState::Updated => 2,
            TaskLifecycleState::Completed => 3,
            TaskLifecycleState::Failed => 4,
            TaskLifecycleState::Cancelled => 5,
            TaskLifecycleState::Removed => 6,
        });
        encoded.push(match self.operation {
            UserInputOperation::Upsert => 1,
            UserInputOperation::Retract => 2,
        });
        encoded.push(match self.completeness {
            ContractCompleteness::Complete => 1,
            ContractCompleteness::Partial => 2,
            ContractCompleteness::Unknown => 3,
        });
        match &self.owned_set {
            Some(owned_set) => {
                encoded.push(1);
                encoded.extend_from_slice(&(owned_set.len() as u64).to_be_bytes());
                for member in owned_set {
                    push_component(&mut encoded, member.as_bytes());
                }
            }
            None => encoded.push(0),
        }
        Ok(*blake3::hash(&encoded).as_bytes())
    }
}

const MAX_PLAN_STEPS: usize = 32;

/// RFC 012C plan revision. One revisioned entity; a complete ordered step
/// snapshot replaces prior steps, and a complete owned-set may retract members
/// absent from that set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanRevisionFact {
    pub session: CanonicalEntityKey,
    pub actor_run: CanonicalEntityKey,
    pub native_plan_id: String,
    pub subject: String,
    pub ordered_step_keys: Vec<String>,
    pub completeness: ContractCompleteness,
    pub operation: UserInputOperation,
    pub owned_set: Option<Vec<String>>,
}

impl PlanRevisionFact {
    pub(crate) fn validate(&self) -> Result<(), AdapterError> {
        validate_runtime_semantic_text("native_plan_id", Some(self.native_plan_id.as_str()))?;
        validate_runtime_semantic_text("subject", Some(self.subject.as_str()))?;
        if self.ordered_step_keys.is_empty() || self.ordered_step_keys.len() > MAX_PLAN_STEPS {
            return Err(AdapterError::invalid_contract(
                "plan ordered_step_keys must be a bounded non-empty snapshot",
            ));
        }
        let mut seen = BTreeSet::new();
        for key in &self.ordered_step_keys {
            validate_runtime_semantic_text("plan step key", Some(key.as_str()))?;
            if !seen.insert(key.as_str()) {
                return Err(AdapterError::invalid_contract(
                    "plan step keys must be unique in one snapshot",
                ));
            }
        }
        if let Some(owned_set) = &self.owned_set {
            if owned_set.is_empty() {
                return Err(AdapterError::invalid_contract(
                    "plan owned_set must be omitted or non-empty",
                ));
            }
            let mut owned_seen = BTreeSet::new();
            for member in owned_set {
                validate_runtime_semantic_text("owned plan id", Some(member.as_str()))?;
                if !owned_seen.insert(member.as_str()) {
                    return Err(AdapterError::invalid_contract(
                        "plan owned_set members must be unique",
                    ));
                }
            }
        }
        Ok(())
    }

    pub(crate) fn semantic_revision_key(&self) -> Result<[u8; FACT_HASH_BYTES], AdapterError> {
        self.validate()?;
        let mut encoded = Vec::new();
        encoded.extend_from_slice(b"spaghetti/runtime.plan/semantic-revision\0");
        encoded.extend_from_slice(&1_u32.to_be_bytes());
        push_component(&mut encoded, self.session.as_bytes());
        push_component(&mut encoded, self.actor_run.as_bytes());
        push_component(&mut encoded, self.native_plan_id.as_bytes());
        push_component(&mut encoded, self.subject.as_bytes());
        encoded.extend_from_slice(&(self.ordered_step_keys.len() as u64).to_be_bytes());
        for key in &self.ordered_step_keys {
            push_component(&mut encoded, key.as_bytes());
        }
        encoded.push(match self.operation {
            UserInputOperation::Upsert => 1,
            UserInputOperation::Retract => 2,
        });
        encoded.push(match self.completeness {
            ContractCompleteness::Complete => 1,
            ContractCompleteness::Partial => 2,
            ContractCompleteness::Unknown => 3,
        });
        match &self.owned_set {
            Some(owned_set) => {
                encoded.push(1);
                encoded.extend_from_slice(&(owned_set.len() as u64).to_be_bytes());
                for member in owned_set {
                    push_component(&mut encoded, member.as_bytes());
                }
            }
            None => encoded.push(0),
        }
        Ok(*blake3::hash(&encoded).as_bytes())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolRevisionKind {
    Call,
    Result,
}

/// RFC 012C tool call or result. Calls and results are separate correlated
/// lifecycle entities; unmatched results are retained, and correlation updates
/// the relationship without changing either entity key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolRevisionFact {
    pub session: CanonicalEntityKey,
    pub actor_run: CanonicalEntityKey,
    pub native_tool_id: String,
    pub kind: ToolRevisionKind,
    pub tool_name: String,
    pub correlated_native_id: Option<String>,
    pub completeness: ContractCompleteness,
    pub operation: UserInputOperation,
}

impl ToolRevisionFact {
    pub(crate) fn validate(&self) -> Result<(), AdapterError> {
        validate_runtime_semantic_text("native_tool_id", Some(self.native_tool_id.as_str()))?;
        validate_runtime_semantic_text("tool_name", Some(self.tool_name.as_str()))?;
        if let Some(correlated) = &self.correlated_native_id {
            validate_runtime_semantic_text("correlated_native_id", Some(correlated.as_str()))?;
            if correlated == &self.native_tool_id {
                return Err(AdapterError::invalid_contract(
                    "tool correlation must name the other call or result identity",
                ));
            }
        }
        Ok(())
    }

    pub(crate) fn semantic_revision_key(&self) -> Result<[u8; FACT_HASH_BYTES], AdapterError> {
        self.validate()?;
        let mut encoded = Vec::new();
        encoded.extend_from_slice(b"spaghetti/runtime.tool/semantic-revision\0");
        encoded.extend_from_slice(&1_u32.to_be_bytes());
        push_component(&mut encoded, self.session.as_bytes());
        push_component(&mut encoded, self.actor_run.as_bytes());
        push_component(&mut encoded, self.native_tool_id.as_bytes());
        encoded.push(match self.kind {
            ToolRevisionKind::Call => 1,
            ToolRevisionKind::Result => 2,
        });
        push_component(&mut encoded, self.tool_name.as_bytes());
        match &self.correlated_native_id {
            Some(correlated) => {
                encoded.push(1);
                push_component(&mut encoded, correlated.as_bytes());
            }
            None => encoded.push(0),
        }
        encoded.push(match self.operation {
            UserInputOperation::Upsert => 1,
            UserInputOperation::Retract => 2,
        });
        encoded.push(match self.completeness {
            ContractCompleteness::Complete => 1,
            ContractCompleteness::Partial => 2,
            ContractCompleteness::Unknown => 3,
        });
        Ok(*blake3::hash(&encoded).as_bytes())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectiveStateDimension {
    Model,
    Effort,
    SessionMode,
    PermissionMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectiveStateEvidenceKind {
    ConfiguredIntent,
    ResponseObserved,
    NativeTransition,
}

/// Authority that proves one qualified effective-state value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectiveStateValueAuthority {
    NativeConfiguration,
    NativeResponse,
    NativeTransition,
}

/// Bounded native coordinate used to normalize an effective-state value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectiveStateValueProvenance {
    pub native_field: String,
    pub normalization_contract_version: u32,
}

pub type EffectiveStateQualifiedValue<T> =
    QualifiedValue<T, EffectiveStateValueAuthority, EffectiveStateValueProvenance>;

/// RFC 012C effective runtime state. One revisioned entity per actor/dimension;
/// absence is unknown, not an inherited default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectiveStateRevisionFact {
    pub session: CanonicalEntityKey,
    pub actor_run: CanonicalEntityKey,
    pub dimension: EffectiveStateDimension,
    pub value: EffectiveStateQualifiedValue<String>,
    pub evidence_kind: EffectiveStateEvidenceKind,
    pub completeness: ContractCompleteness,
    pub operation: UserInputOperation,
}

impl EffectiveStateRevisionFact {
    pub(crate) fn validate(&self) -> Result<(), AdapterError> {
        validate_effective_state_qualified_value(&self.value)?;
        if self.value.completeness != self.completeness {
            return Err(AdapterError::invalid_contract(
                "effective-state value and revision completeness must match",
            ));
        }
        let authority_matches_evidence = matches!(
            (self.value.authority, self.evidence_kind),
            (
                EffectiveStateValueAuthority::NativeConfiguration,
                EffectiveStateEvidenceKind::ConfiguredIntent
            ) | (
                EffectiveStateValueAuthority::NativeResponse,
                EffectiveStateEvidenceKind::ResponseObserved
            ) | (
                EffectiveStateValueAuthority::NativeTransition,
                EffectiveStateEvidenceKind::NativeTransition
            )
        );
        if !authority_matches_evidence {
            return Err(AdapterError::invalid_contract(
                "effective-state value authority does not match its evidence kind",
            ));
        }
        Ok(())
    }

    pub(crate) fn semantic_revision_key(&self) -> Result<[u8; FACT_HASH_BYTES], AdapterError> {
        self.validate()?;
        let mut encoded = Vec::new();
        encoded.extend_from_slice(b"spaghetti/runtime.effective-state/semantic-revision\0");
        encoded.extend_from_slice(&1_u32.to_be_bytes());
        push_component(&mut encoded, self.session.as_bytes());
        push_component(&mut encoded, self.actor_run.as_bytes());
        encoded.push(match self.dimension {
            EffectiveStateDimension::Model => 1,
            EffectiveStateDimension::Effort => 2,
            EffectiveStateDimension::SessionMode => 3,
            EffectiveStateDimension::PermissionMode => 4,
        });
        push_effective_state_qualified_value(&mut encoded, &self.value);
        encoded.push(match self.evidence_kind {
            EffectiveStateEvidenceKind::ConfiguredIntent => 1,
            EffectiveStateEvidenceKind::ResponseObserved => 2,
            EffectiveStateEvidenceKind::NativeTransition => 3,
        });
        encoded.push(match self.operation {
            UserInputOperation::Upsert => 1,
            UserInputOperation::Retract => 2,
        });
        encoded.push(match self.completeness {
            ContractCompleteness::Complete => 1,
            ContractCompleteness::Partial => 2,
            ContractCompleteness::Unknown => 3,
        });
        Ok(*blake3::hash(&encoded).as_bytes())
    }
}

fn validate_effective_state_qualified_value(
    value: &EffectiveStateQualifiedValue<String>,
) -> Result<(), AdapterError> {
    let unknown = value.quality == QualifiedValueQuality::Unknown;
    if unknown != value.value.is_none() || unknown != value.unknown_reason.is_some() {
        return Err(AdapterError::invalid_contract(
            "effective-state qualified value must pair unknown quality with an absent value and reason",
        ));
    }
    validate_runtime_semantic_text("effective-state value", value.value.as_deref())?;
    if !is_bounded_native_field(
        &value.provenance.native_field,
        MAX_EFFECTIVE_STATE_PROVENANCE_FIELD_BYTES,
    ) || value.provenance.normalization_contract_version == 0
    {
        return Err(AdapterError::invalid_contract(
            "effective-state provenance is empty, oversized, uncanonical, or unversioned",
        ));
    }
    if value.effective_at.is_some_and(|effective_at| {
        !(-JS_SAFE_INTEGER_MAX_I64..=JS_SAFE_INTEGER_MAX_I64).contains(&effective_at)
    }) {
        return Err(AdapterError::invalid_contract(
            "effective-state effective_at exceeds the portable safe-integer range",
        ));
    }
    Ok(())
}

fn is_bounded_native_field(value: &str, max_bytes: usize) -> bool {
    let mut bytes = value.bytes();
    matches!(bytes.next(), Some(b'a'..=b'z'))
        && value.len() <= max_bytes
        && bytes.all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-'))
}

fn push_effective_state_qualified_value(
    output: &mut Vec<u8>,
    value: &EffectiveStateQualifiedValue<String>,
) {
    match value.value.as_ref() {
        Some(value) => {
            output.push(1);
            push_component(output, value.as_bytes());
        }
        None => output.push(0),
    }
    output.push(qualified_value_quality_revision_tag(value.quality));
    output.push(match value.authority {
        EffectiveStateValueAuthority::NativeConfiguration => 1,
        EffectiveStateValueAuthority::NativeResponse => 2,
        EffectiveStateValueAuthority::NativeTransition => 3,
    });
    output.push(match value.completeness {
        ContractCompleteness::Complete => 1,
        ContractCompleteness::Partial => 2,
        ContractCompleteness::Unknown => 3,
    });
    output.push(qualified_unknown_reason_revision_tag(value.unknown_reason));
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
        validate_runtime_semantic_text("native_message_id", self.native_message_id.as_deref())?;
        validate_runtime_semantic_text("request_id", self.request_id.as_deref())?;
        for value in [
            &self.buckets.input_tokens,
            &self.buckets.output_tokens,
            &self.buckets.cache_creation_input_tokens,
            &self.buckets.cache_read_input_tokens,
        ] {
            validate_usage_qualified_value(value)?;
            if let Some(tokens) = value.value {
                if tokens > JS_SAFE_INTEGER_MAX_U64 {
                    return Err(AdapterError::invalid_contract(
                        "usage-v2 token value exceeds the portable safe-integer range",
                    ));
                }
            }
        }
        for (field, value) in [("model", &self.model), ("effort", &self.effort)] {
            if let Some(value) = value {
                validate_usage_qualified_value(value)?;
                validate_runtime_semantic_text(field, value.value.as_deref())?;
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
    output.push(qualified_value_quality_revision_tag(value.quality));
    output.push(match value.authority {
        UsageValueAuthority::NativeResponse => 1,
        UsageValueAuthority::AdapterDerived => 2,
    });
    output.push(match value.completeness {
        ContractCompleteness::Complete => 1,
        ContractCompleteness::Partial => 2,
        ContractCompleteness::Unknown => 3,
    });
    output.push(qualified_unknown_reason_revision_tag(value.unknown_reason));
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

fn qualified_value_quality_revision_tag(quality: QualifiedValueQuality) -> u8 {
    match quality {
        QualifiedValueQuality::Exact => 1,
        QualifiedValueQuality::NativeClaimed => 2,
        QualifiedValueQuality::Derived => 3,
        QualifiedValueQuality::Estimated => 4,
        QualifiedValueQuality::Unknown => 5,
    }
}

fn qualified_unknown_reason_revision_tag(reason: Option<QualifiedUnknownReason>) -> u8 {
    match reason {
        None => 0,
        Some(QualifiedUnknownReason::Missing) => 1,
        Some(QualifiedUnknownReason::Unsupported) => 2,
        Some(QualifiedUnknownReason::Withheld) => 3,
        Some(QualifiedUnknownReason::NotYetObserved) => 4,
        Some(QualifiedUnknownReason::Ambiguous) => 5,
        Some(QualifiedUnknownReason::Malformed) => 6,
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
    if !is_bounded_native_field(
        &value.provenance.native_field,
        MAX_USAGE_PROVENANCE_FIELD_BYTES,
    ) || value.provenance.normalization_contract_version == 0
    {
        return Err(AdapterError::invalid_contract(
            "usage-v2 value provenance is empty, oversized, uncanonical, or unversioned",
        ));
    }
    if value.effective_at.is_some_and(|effective_at| {
        !(-JS_SAFE_INTEGER_MAX_I64..=JS_SAFE_INTEGER_MAX_I64).contains(&effective_at)
    }) {
        return Err(AdapterError::invalid_contract(
            "usage-v2 effective_at exceeds the portable safe-integer range",
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
    UserInputRequestRevision(UserInputRequestRevisionFact),
    MessageRevision(MessageRevisionFact),
    ContentBlockRevision(ContentBlockRevisionFact),
    NativeRuntimeMarkerRevision(NativeRuntimeMarkerRevisionFact),
    TaskRevision(TaskRevisionFact),
    PlanRevision(PlanRevisionFact),
    ToolRevision(ToolRevisionFact),
    EffectiveStateRevision(EffectiveStateRevisionFact),
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

pub(crate) const MAX_UNKNOWN_NATIVE_KIND_BYTES: usize = 128;
pub(crate) const MAX_UNKNOWN_REASON_BYTES: usize = 4 * 1024;
pub(crate) const MAX_UNKNOWN_RAW_PAYLOAD_BYTES: usize = 4 * 1024 * 1024;

/// Value-free RFC 012A evidence retained for an unknown source record
/// regardless of the selected raw-retention policy. The excerpt is produced
/// by the common decode boundary and contains no native values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BoundedNativeEvidence {
    pub source_record_id: SourceRecordId,
    pub observed_bytes: u64,
    pub payload_digest: [u8; 32],
    pub sanitized_excerpt: Vec<u8>,
}

/// RFC 012A's topology-independent outcome for one complete source record.
///
/// The common decode boundary constructs these values after adapter return.
/// They live beside `FactBatch` so a durable batch cannot lose or reorder the
/// record-level classification while append slices are merged for commit.
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
            Self::UserInputRequestRevision(_) => "runtime.user-input-request",
            Self::MessageRevision(_) => "runtime.message",
            Self::ContentBlockRevision(_) => "runtime.content-block",
            Self::NativeRuntimeMarkerRevision(_) => "runtime.native-marker",
            Self::TaskRevision(_) => "runtime.task",
            Self::PlanRevision(_) => "runtime.plan",
            Self::ToolRevision(_) => "runtime.tool",
            Self::EffectiveStateRevision(_) => "runtime.effective-state",
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

    fn validate_batch_shape(&self) -> Result<(), AdapterError> {
        let Self::UnknownRecord {
            native_kind,
            raw_payload,
            reason,
        } = self
        else {
            return Ok(());
        };
        if native_kind.as_ref().is_some_and(|native_kind| {
            native_kind.is_empty()
                || native_kind.len() > MAX_UNKNOWN_NATIVE_KIND_BYTES
                || native_kind.trim() != native_kind
                || native_kind.chars().any(char::is_control)
        }) {
            return Err(AdapterError::invalid_contract(
                "unknown-record native kind is empty, oversized, or noncanonical",
            ));
        }
        if reason.is_empty()
            || reason.len() > MAX_UNKNOWN_REASON_BYTES
            || reason.trim() != reason
            || reason.chars().any(char::is_control)
        {
            return Err(AdapterError::invalid_contract(
                "unknown-record reason is empty, oversized, or noncanonical",
            ));
        }
        if raw_payload.len() > MAX_UNKNOWN_RAW_PAYLOAD_BYTES {
            return Err(AdapterError::invalid_contract(
                "unknown-record retained payload exceeds the common evidence bound",
            ));
        }
        Ok(())
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
            Self::ActorRunRevision(_)
            | Self::ActorAffiliationRevision(_)
            | Self::UserInputRequestRevision(_)
            | Self::MessageRevision(_)
            | Self::ContentBlockRevision(_)
            | Self::NativeRuntimeMarkerRevision(_)
            | Self::TaskRevision(_)
            | Self::PlanRevision(_)
            | Self::ToolRevision(_)
            | Self::EffectiveStateRevision(_) => None,
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
            Self::UserInputRequestRevision(revision) => revision.semantic_revision_key().map(Some),
            Self::MessageRevision(revision) => revision.semantic_revision_key().map(Some),
            Self::ContentBlockRevision(revision) => revision.semantic_revision_key().map(Some),
            Self::NativeRuntimeMarkerRevision(revision) => {
                revision.semantic_revision_key().map(Some)
            }
            Self::TaskRevision(revision) => revision.semantic_revision_key().map(Some),
            Self::PlanRevision(revision) => revision.semantic_revision_key().map(Some),
            Self::ToolRevision(revision) => revision.semantic_revision_key().map(Some),
            Self::EffectiveStateRevision(revision) => revision.semantic_revision_key().map(Some),
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
    record_mapping_dispositions: Vec<RecordMappingDisposition>,
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
            record_mapping_dispositions: Vec::new(),
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
            value.validate_batch_shape()?;
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
            && self
                .record_mapping_dispositions
                .len()
                .checked_add(other.record_mapping_dispositions.len())
                .is_some_and(|count| count <= MAX_RECORD_MAPPINGS_PER_BATCH)
    }

    pub(crate) fn append(&mut self, mut other: Self) -> Result<(), AdapterError> {
        if !self.can_append(&other) {
            return Err(AdapterError::invalid_contract(format!(
                "fact batch exceeds {} facts, {} diagnostics, or the record-mapping bound",
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
                                Fact::UserInputRequestRevision(_),
                                Fact::UserInputRequestRevision(_)
                            )
                            | (Fact::MessageRevision(_), Fact::MessageRevision(_))
                            | (Fact::ContentBlockRevision(_), Fact::ContentBlockRevision(_))
                            | (
                                Fact::NativeRuntimeMarkerRevision(_),
                                Fact::NativeRuntimeMarkerRevision(_)
                            )
                            | (Fact::TaskRevision(_), Fact::TaskRevision(_))
                            | (Fact::PlanRevision(_), Fact::PlanRevision(_))
                            | (Fact::ToolRevision(_), Fact::ToolRevision(_))
                            | (
                                Fact::EffectiveStateRevision(_),
                                Fact::EffectiveStateRevision(_)
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
        self.record_mapping_dispositions
            .append(&mut other.record_mapping_dispositions);
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

    pub(crate) fn record_mapping_dispositions(&self) -> &[RecordMappingDisposition] {
        &self.record_mapping_dispositions
    }

    pub(crate) fn add_record_mapping_disposition(
        &mut self,
        disposition: RecordMappingDisposition,
    ) -> Result<(), AdapterError> {
        if self.record_mapping_dispositions.len() == MAX_RECORD_MAPPINGS_PER_BATCH {
            return Err(AdapterError::invalid_contract(
                "fact batch exceeds the record-mapping bound",
            ));
        }
        self.record_mapping_dispositions.push(disposition);
        Ok(())
    }

    pub fn dependency_reads(&self) -> &[DependencyRevision] {
        &self.dependency_reads
    }

    pub(crate) fn source_record_id(
        &self,
        record: &SourceRecord,
    ) -> Result<SourceRecordId, AdapterError> {
        self.semantic_context
            .as_ref()
            .ok_or_else(|| {
                AdapterError::invalid_contract(
                    "source-record identity requires a bound semantic decode context",
                )
            })?
            .source_record_id(record)
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

    fn effective_state_fact(batch: &FactBatch, value: &str) -> EffectiveStateRevisionFact {
        EffectiveStateRevisionFact {
            session: batch
                .canonical_entity_key("session", b"native-session")
                .unwrap(),
            actor_run: batch.canonical_entity_key("run", b"native-run").unwrap(),
            dimension: EffectiveStateDimension::Model,
            value: QualifiedValue::from_parts(
                Some(value.to_string()),
                QualifiedValueQuality::Exact,
                EffectiveStateValueAuthority::NativeResponse,
                ContractCompleteness::Complete,
                None,
                Some(42),
                EffectiveStateValueProvenance {
                    native_field: "response.model".to_string(),
                    normalization_contract_version: 1,
                },
            )
            .unwrap(),
            evidence_kind: EffectiveStateEvidenceKind::ResponseObserved,
            completeness: ContractCompleteness::Complete,
            operation: UserInputOperation::Upsert,
        }
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
    fn effective_state_semantic_revision_covers_qualified_evidence() {
        let batch = FactBatch::new_with_semantic_context(1, 1, semantic_context()).unwrap();
        let original = effective_state_fact(&batch, "model-a");
        let original_revision = original.semantic_revision_key().unwrap();

        let mut value = original.clone();
        value.value.value = Some("model-b".to_string());
        assert_ne!(value.semantic_revision_key().unwrap(), original_revision);

        let mut provenance = original.clone();
        provenance.value.provenance.native_field = "response.model_alias".to_string();
        assert_ne!(
            provenance.semantic_revision_key().unwrap(),
            original_revision
        );

        let mut path_provenance = original.clone();
        path_provenance.value.provenance.native_field = "/Users/alice/model".to_string();
        assert!(path_provenance.semantic_revision_key().is_err());

        let mut effective_at = original.clone();
        effective_at.value.effective_at = Some(43);
        assert_ne!(
            effective_at.semantic_revision_key().unwrap(),
            original_revision
        );

        let mut authority = original.clone();
        authority.value.authority = EffectiveStateValueAuthority::NativeTransition;
        assert!(authority.semantic_revision_key().is_err());

        let mut completeness = original.clone();
        completeness.value.completeness = ContractCompleteness::Partial;
        assert!(completeness.semantic_revision_key().is_err());

        let mut explicit_unknown = original;
        explicit_unknown.value.value = None;
        explicit_unknown.value.quality = QualifiedValueQuality::Unknown;
        explicit_unknown.value.unknown_reason = Some(QualifiedUnknownReason::NotYetObserved);
        assert!(explicit_unknown.semantic_revision_key().is_ok());
    }

    #[test]
    fn content_block_revision_is_typed_bounded_and_value_derived() {
        let context = semantic_context();
        let session = context
            .canonical_entity_key("session", b"native-session")
            .unwrap();
        let actor_run = context
            .canonical_entity_key("actor-run", b"native-run")
            .unwrap();
        let message = CanonicalFactId::native(
            context.adapter_id().as_str(),
            &context.source_instance_key(),
            "runtime.message",
            b"native-message",
        )
        .unwrap();
        let original = ContentBlockRevisionFact {
            session,
            actor_run,
            message,
            native_content_block_id: Some("block-0".to_string()),
            ordinal: 0,
            content: ContentBlockRevisionValue::Text {
                text: " draft\n".to_string(),
            },
            native_tool_call_or_result_id: None,
            completeness: ContractCompleteness::Complete,
            operation: UserInputOperation::Upsert,
        };
        let original_revision = original.semantic_revision_key().unwrap();

        let mut correction = original.clone();
        correction.content = ContentBlockRevisionValue::Text {
            text: "final".to_string(),
        };
        assert_eq!(
            correction.stable_native_fact_key().unwrap(),
            original.stable_native_fact_key().unwrap()
        );
        assert_ne!(
            correction.semantic_revision_key().unwrap(),
            original_revision
        );

        let mut another_message = original.clone();
        another_message.message = CanonicalFactId::native(
            context.adapter_id().as_str(),
            &context.source_instance_key(),
            "runtime.message",
            b"another-message",
        )
        .unwrap();
        assert_ne!(
            another_message.stable_native_fact_key().unwrap(),
            original.stable_native_fact_key().unwrap()
        );

        let mut moved = original.clone();
        moved.ordinal = 1;
        assert_ne!(moved.semantic_revision_key().unwrap(), original_revision);

        let mut partial = original.clone();
        partial.completeness = ContractCompleteness::Partial;
        assert_ne!(partial.semantic_revision_key().unwrap(), original_revision);

        let mut ordinal_fallback = original.clone();
        ordinal_fallback.native_content_block_id = None;
        let ordinal_zero = ordinal_fallback.stable_native_fact_key().unwrap();
        ordinal_fallback.ordinal = 1;
        assert_ne!(
            ordinal_fallback.stable_native_fact_key().unwrap(),
            ordinal_zero
        );

        let mut tool_call = original.clone();
        tool_call.content = ContentBlockRevisionValue::ToolCall {
            tool_name: "read".to_string(),
            input_digest: [7; 32],
        };
        assert!(tool_call.semantic_revision_key().is_ok());

        let mut image = original.clone();
        image.content = ContentBlockRevisionValue::Image {
            media_type: "image/png".to_string(),
            data_hash: [8; 32],
        };
        assert!(image.semantic_revision_key().is_ok());
        image.content = ContentBlockRevisionValue::Image {
            media_type: "/Users/alice/image.png".to_string(),
            data_hash: [8; 32],
        };
        assert!(image.semantic_revision_key().is_err());
        tool_call.native_tool_call_or_result_id = Some("tool-1".to_string());
        assert!(tool_call.semantic_revision_key().is_ok());
        tool_call.content = ContentBlockRevisionValue::ToolCall {
            tool_name: "read".to_string(),
            input_digest: [0; 32],
        };
        assert!(tool_call.semantic_revision_key().is_err());

        let mut text_with_tool_identity = original.clone();
        text_with_tool_identity.native_tool_call_or_result_id = Some("tool-1".to_string());
        assert!(text_with_tool_identity.semantic_revision_key().is_err());

        let mut extension = original;
        extension.content = ContentBlockRevisionValue::NativeExtension {
            native_kind: "/Users/alice/raw".to_string(),
            value_digest: [9; 32],
        };
        assert!(extension.semantic_revision_key().is_err());
    }

    #[test]
    fn native_runtime_marker_is_native_only_bounded_and_value_derived() {
        let context = semantic_context();
        let session = context
            .canonical_entity_key("session", b"native-session")
            .unwrap();
        let actor_run = context
            .canonical_entity_key("actor-run", b"native-run")
            .unwrap();
        let original = NativeRuntimeMarkerRevisionFact {
            session,
            actor_run,
            native_marker_id: "progress-1".to_string(),
            correlated_native_id: Some("tool-1".to_string()),
            value: NativeRuntimeMarkerValue::Progress {
                state: NativeProgressState::Active,
                completed: Some(1),
                total: Some(2),
                detail_digest: Some([7; 32]),
            },
            quality: QualifiedValueQuality::NativeClaimed,
            effective_at: Some(42),
            provenance: NativeRuntimeMarkerProvenance {
                native_field: "progress.data".to_string(),
                normalization_contract_version: 1,
            },
            completeness: ContractCompleteness::Complete,
            operation: UserInputOperation::Upsert,
        };
        let original_identity = original.stable_native_fact_key().unwrap();
        let original_revision = original.semantic_revision_key().unwrap();

        let mut correction = original.clone();
        correction.value = NativeRuntimeMarkerValue::Progress {
            state: NativeProgressState::Completed,
            completed: Some(2),
            total: Some(2),
            detail_digest: Some([8; 32]),
        };
        assert_eq!(
            correction.stable_native_fact_key().unwrap(),
            original_identity
        );
        assert_ne!(
            correction.semantic_revision_key().unwrap(),
            original_revision
        );

        let mut different_kind = original.clone();
        different_kind.value = NativeRuntimeMarkerValue::Compaction {
            phase: NativeCompactionPhase::Boundary,
            trigger: Some("auto".to_string()),
            pre_tokens: Some(2),
        };
        assert_ne!(
            different_kind.stable_native_fact_key().unwrap(),
            original_identity
        );
        assert!(different_kind.semantic_revision_key().is_ok());

        let mut queue = original.clone();
        queue.value = NativeRuntimeMarkerValue::Queue {
            operation: NativeQueueOperation::Enqueue,
            depth: Some(1),
            item_digest: Some([9; 32]),
        };
        assert!(queue.semantic_revision_key().is_ok());

        let mut host_assessment = original.clone();
        host_assessment.quality = QualifiedValueQuality::Derived;
        assert!(host_assessment.semantic_revision_key().is_err());
        host_assessment.quality = QualifiedValueQuality::Estimated;
        assert!(host_assessment.semantic_revision_key().is_err());

        let mut invalid_progress = original.clone();
        invalid_progress.value = NativeRuntimeMarkerValue::Progress {
            state: NativeProgressState::Active,
            completed: Some(3),
            total: Some(2),
            detail_digest: Some([7; 32]),
        };
        assert!(invalid_progress.semantic_revision_key().is_err());
        invalid_progress.value = NativeRuntimeMarkerValue::Progress {
            state: NativeProgressState::Active,
            completed: Some(JS_SAFE_INTEGER_MAX_U64 + 1),
            total: None,
            detail_digest: Some([7; 32]),
        };
        assert!(invalid_progress.semantic_revision_key().is_err());
        invalid_progress.value = NativeRuntimeMarkerValue::Progress {
            state: NativeProgressState::Active,
            completed: None,
            total: None,
            detail_digest: Some([0; 32]),
        };
        assert!(invalid_progress.semantic_revision_key().is_err());

        let mut invalid_text = original.clone();
        invalid_text.provenance.native_field = "/Users/alice/progress".to_string();
        assert!(invalid_text.semantic_revision_key().is_err());
        invalid_text = original.clone();
        invalid_text.native_marker_id = " marker ".to_string();
        assert!(invalid_text.semantic_revision_key().is_err());

        let mut invalid_time = original;
        invalid_time.effective_at = Some(JS_SAFE_INTEGER_MAX_I64 + 1);
        assert!(invalid_time.semantic_revision_key().is_err());
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
    fn usage_v2_validate_enforces_portable_safe_integers_and_canonical_text() {
        let batch = FactBatch::new_with_semantic_context(1, 1, semantic_context()).unwrap();
        let valid = usage_v2_fact(&batch, 1);
        assert!(valid.validate().is_ok());

        let mut exact_tokens = valid.clone();
        exact_tokens.buckets.input_tokens.value = Some(JS_SAFE_INTEGER_MAX_U64);
        assert!(exact_tokens.validate().is_ok());
        let mut oversized_tokens = valid.clone();
        oversized_tokens.buckets.input_tokens.value = Some(JS_SAFE_INTEGER_MAX_U64 + 1);
        assert!(oversized_tokens.validate().is_err());

        let mut exact_effective_at = valid.clone();
        exact_effective_at.buckets.input_tokens.effective_at = Some(JS_SAFE_INTEGER_MAX_I64);
        assert!(exact_effective_at.validate().is_ok());
        let mut oversized_effective_at = valid.clone();
        oversized_effective_at.buckets.input_tokens.effective_at =
            Some(JS_SAFE_INTEGER_MAX_I64 + 1);
        assert!(oversized_effective_at.validate().is_err());

        let mut exact_request = valid.clone();
        exact_request.request_id = Some("r".repeat(MAX_RUNTIME_SEMANTIC_TEXT_BYTES));
        assert!(exact_request.validate().is_ok());
        let mut oversized_request = valid.clone();
        oversized_request.request_id = Some("r".repeat(MAX_RUNTIME_SEMANTIC_TEXT_BYTES + 1));
        assert!(oversized_request.validate().is_err());

        let mut exact_provenance = valid.clone();
        exact_provenance
            .buckets
            .input_tokens
            .provenance
            .native_field = "p".repeat(MAX_USAGE_PROVENANCE_FIELD_BYTES);
        assert!(exact_provenance.validate().is_ok());
        let mut oversized_provenance = valid.clone();
        oversized_provenance
            .buckets
            .input_tokens
            .provenance
            .native_field = "p".repeat(MAX_USAGE_PROVENANCE_FIELD_BYTES + 1);
        assert!(oversized_provenance.validate().is_err());

        let mut empty_request = valid.clone();
        empty_request.request_id = Some(String::new());
        assert!(empty_request.validate().is_err());

        let mut padded_request = valid.clone();
        padded_request.request_id = Some(" request-1".to_string());
        assert!(padded_request.validate().is_err());

        let mut padded_provenance = valid.clone();
        padded_provenance
            .buckets
            .input_tokens
            .provenance
            .native_field = " message.usage.input_tokens".to_string();
        assert!(padded_provenance.validate().is_err());

        let mut padded_model = valid.clone();
        padded_model.model.as_mut().unwrap().value = Some(" model-1".to_string());
        assert!(padded_model.validate().is_err());
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

    #[test]
    fn unknown_record_evidence_bounds_fail_before_batch_mutation() {
        let record = record();
        let unknown = |native_kind: Option<String>, raw_payload: Vec<u8>, reason: String| {
            Fact::UnknownRecord {
                native_kind,
                raw_payload,
                reason,
            }
        };

        let mut exact = FactBatch::new(1, 1).unwrap();
        exact
            .push(
                &record,
                unknown(
                    Some("k".repeat(MAX_UNKNOWN_NATIVE_KIND_BYTES)),
                    vec![0; MAX_UNKNOWN_RAW_PAYLOAD_BYTES],
                    "r".repeat(MAX_UNKNOWN_REASON_BYTES),
                ),
            )
            .unwrap();
        assert_eq!(exact.facts().len(), 1);

        for invalid in [
            unknown(Some(String::new()), Vec::new(), "reason".to_string()),
            unknown(
                Some("k".repeat(MAX_UNKNOWN_NATIVE_KIND_BYTES + 1)),
                Vec::new(),
                "reason".to_string(),
            ),
            unknown(
                Some("kind".to_string()),
                Vec::new(),
                "r".repeat(MAX_UNKNOWN_REASON_BYTES + 1),
            ),
            unknown(
                Some("kind".to_string()),
                vec![0; MAX_UNKNOWN_RAW_PAYLOAD_BYTES + 1],
                "reason".to_string(),
            ),
            unknown(Some(" kind".to_string()), Vec::new(), "reason".to_string()),
            unknown(Some("kind".to_string()), Vec::new(), "reason\n".to_string()),
        ] {
            let mut batch = FactBatch::new(1, 1).unwrap();
            assert!(batch.push(&record, invalid).is_err());
            assert!(batch.facts().is_empty());
            assert!(batch
                .push(
                    &record,
                    unknown(Some("kind".to_string()), Vec::new(), "reason".to_string()),
                )
                .is_ok());
            assert_eq!(batch.facts()[0].provenance.local_fact_ordinal, 0);
        }
    }
}
