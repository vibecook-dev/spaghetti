//! The one wire union the observer delivers.
//!
//! RFC 012D's provisional design gave every fact family its own wire module,
//! its own contract negotiation, and its own unavailability error. Eight of the
//! eleven families never shipped a wire at all. This module replaces all of
//! that with a single serde-tagged enum: the eleven RFC 012C semantic families
//! plus the observer's own lifecycle controls. Adding a family is a variant,
//! not a module.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::adapter::{CanonicalFactId, SemanticRevisionRef};
use crate::runtime_semantic_reducer::RuntimeSemanticFamily;

use super::identity::{ActorRef, ObserverEventId};

/// Which delivery discipline produced an event. Bootstrap is the initial cold
/// read, Live is an append observed while attached, Correction is replay after
/// a reset or inside a replacement epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ObserverPhase {
    Bootstrap,
    Live,
    Correction,
}

/// Whether the reduced entity now exists or was retracted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum SemanticOperation {
    Upsert,
    Retract,
}

/// The eleven RFC 012C runtime families, named on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ObserverFamily {
    Message,
    ContentBlock,
    Tool,
    UserInputRequest,
    Plan,
    Task,
    NativeMarker,
    EffectiveState,
    ActorRun,
    ActorAffiliation,
    UsageV2,
}

impl From<RuntimeSemanticFamily> for ObserverFamily {
    fn from(value: RuntimeSemanticFamily) -> Self {
        match value {
            RuntimeSemanticFamily::Message => Self::Message,
            RuntimeSemanticFamily::ContentBlock => Self::ContentBlock,
            RuntimeSemanticFamily::Tool => Self::Tool,
            RuntimeSemanticFamily::UserInputRequest => Self::UserInputRequest,
            RuntimeSemanticFamily::Plan => Self::Plan,
            RuntimeSemanticFamily::Task => Self::Task,
            RuntimeSemanticFamily::NativeMarker => Self::NativeMarker,
            RuntimeSemanticFamily::EffectiveState => Self::EffectiveState,
            RuntimeSemanticFamily::ActorRun => Self::ActorRun,
            RuntimeSemanticFamily::ActorAffiliation => Self::ActorAffiliation,
            RuntimeSemanticFamily::UsageV2 => Self::UsageV2,
        }
    }
}

impl ObserverFamily {
    pub(crate) fn as_runtime(self) -> RuntimeSemanticFamily {
        match self {
            Self::Message => RuntimeSemanticFamily::Message,
            Self::ContentBlock => RuntimeSemanticFamily::ContentBlock,
            Self::Tool => RuntimeSemanticFamily::Tool,
            Self::UserInputRequest => RuntimeSemanticFamily::UserInputRequest,
            Self::Plan => RuntimeSemanticFamily::Plan,
            Self::Task => RuntimeSemanticFamily::Task,
            Self::NativeMarker => RuntimeSemanticFamily::NativeMarker,
            Self::EffectiveState => RuntimeSemanticFamily::EffectiveState,
            Self::ActorRun => RuntimeSemanticFamily::ActorRun,
            Self::ActorAffiliation => RuntimeSemanticFamily::ActorAffiliation,
            Self::UsageV2 => RuntimeSemanticFamily::UsageV2,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        self.as_runtime().as_str()
    }

    pub(crate) const ALL: [Self; 11] = [
        Self::Message,
        Self::ContentBlock,
        Self::Tool,
        Self::UserInputRequest,
        Self::Plan,
        Self::Task,
        Self::NativeMarker,
        Self::EffectiveState,
        Self::ActorRun,
        Self::ActorAffiliation,
        Self::UsageV2,
    ];
}

/// Where an event came from, in source coordinates. Native content never
/// travels here: transcript streams are declared `HashOnly`, so an event
/// carries the record's digest and byte range, not its bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SourcePosition {
    /// Declared stream this object belongs to, e.g. `session-transcripts`.
    pub stream_id: String,
    /// Path of the object relative to its declared access root.
    pub object_path: String,
    /// Access root the path is relative to (`projects`, `home`, ...).
    pub root_name: String,
    /// Bumped on truncation, replacement, or native identity change.
    #[ts(type = "number")]
    pub generation: u64,
    /// Byte range of the record inside the current generation, when the driver
    /// is append-delimited.
    #[ts(type = "number | null")]
    pub byte_start: Option<u64>,
    #[ts(type = "number | null")]
    pub byte_end: Option<u64>,
    /// Digest of the native record that produced the event.
    pub record_digest: String,
}

/// One typed RFC 012C semantic revision, delivered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SemanticEvent {
    /// Deterministic idempotency key. Deduplicate by `(scope_epoch, event_id)`.
    pub event_id: ObserverEventId,
    /// Monotonic delivery order inside one attachment. Not comparable across
    /// attachments and never comparable to a durable commit sequence.
    #[ts(type = "number")]
    pub sequence: u64,
    #[ts(type = "number")]
    pub scope_epoch: u64,
    pub family: ObserverFamily,
    pub phase: ObserverPhase,
    pub operation: SemanticOperation,
    /// The durable/live join identity for this revision. A value delivered
    /// here is equal to the one an aggregate-facing durable query returns for
    /// the same revision, which is what makes cross-topology reconciliation
    /// possible without comparing observer event ids.
    #[ts(type = "{ semantic_reference_contract_version: number, fact_revision_id: string }")]
    pub semantic_revision_ref: SemanticRevisionRef,
    /// Canonical fact identity the revision belongs to.
    #[ts(type = "string")]
    pub fact_id: CanonicalFactId,
    pub actor: ActorRef,
    pub source: SourcePosition,
    /// Native timestamp when the adapter proved one, in epoch milliseconds.
    #[ts(type = "number | null")]
    pub native_time: Option<i64>,
    /// Host time the record was read, in epoch milliseconds.
    #[ts(type = "number")]
    pub observed_at: i64,
    /// The reduced typed value, exactly as RFC 012C defines it for this family.
    /// Absent for a retraction.
    #[ts(type = "unknown")]
    pub value: Option<serde_json::Value>,
}

/// Per-family coverage inside a completion barrier. `entity_count` plus
/// `digest` make absence actionable: an entity missing from a complete family
/// is genuinely gone, not merely undelivered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct FamilyManifestEntry {
    pub family: ObserverFamily,
    pub contract_version: u32,
    pub entity_count: u32,
    /// Topology-neutral replacement digest. A clean bootstrap and a completed
    /// resync at equal coverage produce equal digests.
    pub digest: String,
}

/// State of one in-scope source object at a barrier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ObjectCoverage {
    /// The declared relation that admitted this object into the scope.
    pub relation_id: String,
    pub stream_id: String,
    pub object_path: String,
    pub root_name: String,
    #[ts(type = "number")]
    pub generation: u64,
    pub present: bool,
    /// Set when the object could not be read or decoded. Sibling objects keep
    /// being observed.
    pub error: Option<String>,
}

/// Delivered at `bootstrap_complete` and `resync_complete`. Everything a
/// consumer needs to swap its staged epoch atomically.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ObserverBarrier {
    pub event_id: ObserverEventId,
    #[ts(type = "number")]
    pub sequence: u64,
    #[ts(type = "number")]
    pub scope_epoch: u64,
    /// True once the root transcript exists. A named-but-absent root produces
    /// an empty complete bootstrap, not an error.
    pub root_present: bool,
    pub family_manifest: Vec<FamilyManifestEntry>,
    pub coverage: Vec<ObjectCoverage>,
    #[ts(type = "number")]
    pub observed_at: i64,
}

/// Why an epoch lost continuity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum OverflowReason {
    /// The bounded semantic queue filled while live appends kept arriving.
    QueueFull,
    /// The consumer asked for a fresh replacement snapshot.
    ConsumerRequested,
}

/// A source object's generation changed under us.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ResetEvent {
    pub event_id: ObserverEventId,
    #[ts(type = "number")]
    pub sequence: u64,
    #[ts(type = "number")]
    pub scope_epoch: u64,
    pub source: SourcePosition,
    #[ts(type = "number")]
    pub old_generation: u64,
    #[ts(type = "number")]
    pub new_generation: u64,
    /// `truncated`, `identity_changed`, `prefix_mismatch`, `contract_replay`.
    pub reason: String,
    pub actor: ActorRef,
    #[ts(type = "number")]
    pub observed_at: i64,
}

/// Continuity was lost; the named epoch is invalid and its undelivered events
/// were discarded. A full replacement snapshot follows in a new epoch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct OverflowEvent {
    pub event_id: ObserverEventId,
    #[ts(type = "number")]
    pub sequence: u64,
    /// The epoch being abandoned.
    #[ts(type = "number")]
    pub scope_epoch: u64,
    pub reason: OverflowReason,
    /// Last sequence that was contiguous in the invalidated epoch.
    #[ts(type = "number")]
    pub last_contiguous_sequence: u64,
    #[ts(type = "number")]
    pub observed_at: i64,
}

/// One in-scope object failed. Siblings keep being observed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SourceErrorEvent {
    pub event_id: ObserverEventId,
    #[ts(type = "number")]
    pub sequence: u64,
    #[ts(type = "number")]
    pub scope_epoch: u64,
    pub source: SourcePosition,
    pub message: String,
    /// False when the object is expected to recover on a later pass.
    pub terminal: bool,
    #[ts(type = "number")]
    pub observed_at: i64,
}

/// The observer released the scope. Idempotent: only one is ever emitted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ClosedEvent {
    pub event_id: ObserverEventId,
    #[ts(type = "number")]
    pub sequence: u64,
    #[ts(type = "number")]
    pub scope_epoch: u64,
    /// Events discarded because they were never delivered.
    pub discarded_events: u32,
    #[ts(type = "number")]
    pub observed_at: i64,
}

/// The observer failed terminally and makes no further continuity claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ObserverErrorEvent {
    pub event_id: ObserverEventId,
    #[ts(type = "number")]
    pub sequence: u64,
    #[ts(type = "number")]
    pub scope_epoch: u64,
    pub message: String,
    #[ts(type = "number")]
    pub observed_at: i64,
}

/// Everything the observer delivers, on one ordered stream.
///
/// The `type` tag is the discriminator; the eleven semantic variants share the
/// [`SemanticEvent`] payload and name their family both in the tag and in
/// `family`, so a consumer can switch on either.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum ObserverEvent {
    Message(SemanticEvent),
    ContentBlock(SemanticEvent),
    Tool(SemanticEvent),
    UserInputRequest(SemanticEvent),
    Plan(SemanticEvent),
    Task(SemanticEvent),
    NativeMarker(SemanticEvent),
    EffectiveState(SemanticEvent),
    ActorRun(SemanticEvent),
    ActorAffiliation(SemanticEvent),
    UsageV2(SemanticEvent),
    BootstrapComplete(ObserverBarrier),
    Reset(ResetEvent),
    Overflow(OverflowEvent),
    ResyncComplete(ObserverBarrier),
    SourceError(SourceErrorEvent),
    Closed(ClosedEvent),
    Error(ObserverErrorEvent),
}

impl ObserverEvent {
    pub(crate) fn semantic(family: ObserverFamily, event: SemanticEvent) -> Self {
        match family {
            ObserverFamily::Message => Self::Message(event),
            ObserverFamily::ContentBlock => Self::ContentBlock(event),
            ObserverFamily::Tool => Self::Tool(event),
            ObserverFamily::UserInputRequest => Self::UserInputRequest(event),
            ObserverFamily::Plan => Self::Plan(event),
            ObserverFamily::Task => Self::Task(event),
            ObserverFamily::NativeMarker => Self::NativeMarker(event),
            ObserverFamily::EffectiveState => Self::EffectiveState(event),
            ObserverFamily::ActorRun => Self::ActorRun(event),
            ObserverFamily::ActorAffiliation => Self::ActorAffiliation(event),
            ObserverFamily::UsageV2 => Self::UsageV2(event),
        }
    }

    /// Control events ride a lane that ordinary semantic backlog cannot block.
    pub fn is_control(&self) -> bool {
        !matches!(
            self,
            Self::Message(_)
                | Self::ContentBlock(_)
                | Self::Tool(_)
                | Self::UserInputRequest(_)
                | Self::Plan(_)
                | Self::Task(_)
                | Self::NativeMarker(_)
                | Self::EffectiveState(_)
                | Self::ActorRun(_)
                | Self::ActorAffiliation(_)
                | Self::UsageV2(_)
        )
    }

    pub(crate) fn sequence(&self) -> u64 {
        match self {
            Self::Message(event)
            | Self::ContentBlock(event)
            | Self::Tool(event)
            | Self::UserInputRequest(event)
            | Self::Plan(event)
            | Self::Task(event)
            | Self::NativeMarker(event)
            | Self::EffectiveState(event)
            | Self::ActorRun(event)
            | Self::ActorAffiliation(event)
            | Self::UsageV2(event) => event.sequence,
            Self::BootstrapComplete(event) | Self::ResyncComplete(event) => event.sequence,
            Self::Reset(event) => event.sequence,
            Self::Overflow(event) => event.sequence,
            Self::SourceError(event) => event.sequence,
            Self::Closed(event) => event.sequence,
            Self::Error(event) => event.sequence,
        }
    }

    /// The deterministic idempotency key. Deduplicate by
    /// `(scope_epoch, event_id)`, never by `event_id` across epochs.
    pub fn event_id(&self) -> ObserverEventId {
        match self {
            Self::Message(event)
            | Self::ContentBlock(event)
            | Self::Tool(event)
            | Self::UserInputRequest(event)
            | Self::Plan(event)
            | Self::Task(event)
            | Self::NativeMarker(event)
            | Self::EffectiveState(event)
            | Self::ActorRun(event)
            | Self::ActorAffiliation(event)
            | Self::UsageV2(event) => event.event_id,
            Self::BootstrapComplete(event) | Self::ResyncComplete(event) => event.event_id,
            Self::Reset(event) => event.event_id,
            Self::Overflow(event) => event.event_id,
            Self::SourceError(event) => event.event_id,
            Self::Closed(event) => event.event_id,
            Self::Error(event) => event.event_id,
        }
    }

    pub(crate) fn set_sequence(&mut self, sequence: u64) {
        match self {
            Self::Message(event)
            | Self::ContentBlock(event)
            | Self::Tool(event)
            | Self::UserInputRequest(event)
            | Self::Plan(event)
            | Self::Task(event)
            | Self::NativeMarker(event)
            | Self::EffectiveState(event)
            | Self::ActorRun(event)
            | Self::ActorAffiliation(event)
            | Self::UsageV2(event) => event.sequence = sequence,
            Self::BootstrapComplete(event) | Self::ResyncComplete(event) => {
                event.sequence = sequence;
            }
            Self::Reset(event) => event.sequence = sequence,
            Self::Overflow(event) => event.sequence = sequence,
            Self::SourceError(event) => event.sequence = sequence,
            Self::Closed(event) => event.sequence = sequence,
            Self::Error(event) => event.sequence = sequence,
        }
    }

    /// Rough retained size, used only for the queue's byte budget.
    pub(crate) fn retained_bytes(&self) -> usize {
        match self {
            Self::Message(event)
            | Self::ContentBlock(event)
            | Self::Tool(event)
            | Self::UserInputRequest(event)
            | Self::Plan(event)
            | Self::Task(event)
            | Self::NativeMarker(event)
            | Self::EffectiveState(event)
            | Self::ActorRun(event)
            | Self::ActorAffiliation(event)
            | Self::UsageV2(event) => event
                .value
                .as_ref()
                .map_or(128, |value| estimate_json_bytes(value)),
            _ => 256,
        }
    }
}

/// Cheap size estimate; exact serialized length is not worth a second pass.
fn estimate_json_bytes(value: &serde_json::Value) -> usize {
    match value {
        serde_json::Value::Null => 4,
        serde_json::Value::Bool(_) => 5,
        serde_json::Value::Number(_) => 8,
        serde_json::Value::String(text) => text.len() + 2,
        serde_json::Value::Array(items) => {
            2 + items.iter().map(estimate_json_bytes).sum::<usize>() + items.len()
        }
        serde_json::Value::Object(entries) => {
            2 + entries
                .iter()
                .map(|(key, value)| key.len() + 3 + estimate_json_bytes(value))
                .sum::<usize>()
        }
    }
}
