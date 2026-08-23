//! Deterministic observer identity: scope identity, event ids, actor refs.
//!
//! RFC 012D section 8 requires that an `event_id` binds the event contract
//! version, the event kind, the semantic revision reference, and the source
//! *occurrence* — never delivery sequence, wall clock, phase, or epoch. The
//! occurrence component is what keeps an `A -> B -> A` transition from being
//! collapsed into one delivery: both `A` revisions share a semantic revision
//! reference, but the second one arrives from a different source record.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::adapter::{
    AdapterId, CanonicalEntityKey, CanonicalSourceInstanceKey, FactSemanticRevision,
    SemanticContractError,
};

/// Version of the observer event contract. It participates in every derived
/// event id so a contract change cannot silently reuse an old idempotency key.
pub(crate) const OBSERVER_EVENT_CONTRACT_VERSION: u32 = 1;

const EVENT_ID_DOMAIN: &[u8] = b"spaghetti/rfc012d/observation-event-id\0";

/// Opaque 32-byte deterministic delivery identity, serialized as lowercase hex.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, TS)]
#[ts(export, type = "string")]
pub struct ObserverEventId(#[serde(with = "hex_digest")] [u8; 32]);

impl ObserverEventId {
    /// Raw digest bytes, for a consumer that keys its own dedup table.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl std::fmt::Display for ObserverEventId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

mod hex_digest {
    use serde::de::Error as _;
    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(
        value: &[u8; 32],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let mut encoded = String::with_capacity(64);
        for byte in value {
            encoded.push_str(&format!("{byte:02x}"));
        }
        serializer.serialize_str(&encoded)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<[u8; 32], D::Error> {
        let encoded = String::deserialize(deserializer)?;
        if encoded.len() != 64 {
            return Err(D::Error::custom("observer event id must be 64 hex digits"));
        }
        let mut bytes = [0_u8; 32];
        for (index, slot) in bytes.iter_mut().enumerate() {
            *slot = u8::from_str_radix(&encoded[index * 2..index * 2 + 2], 16)
                .map_err(|_| D::Error::custom("observer event id must be hex"))?;
        }
        Ok(bytes)
    }
}

/// Length-framed component hashing so no concatenation of two components can
/// collide with a different split of the same bytes.
pub(crate) struct EventIdBuilder {
    hasher: blake3::Hasher,
}

impl EventIdBuilder {
    pub(crate) fn new(event_kind: &str) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(EVENT_ID_DOMAIN);
        hasher.update(&OBSERVER_EVENT_CONTRACT_VERSION.to_be_bytes());
        let mut builder = Self { hasher };
        builder.component(event_kind.as_bytes());
        builder
    }

    pub(crate) fn component(&mut self, value: &[u8]) -> &mut Self {
        self.hasher.update(&(value.len() as u64).to_be_bytes());
        self.hasher.update(value);
        self
    }

    pub(crate) fn u64(&mut self, value: u64) -> &mut Self {
        self.hasher.update(&value.to_be_bytes());
        self
    }

    pub(crate) fn scope(&mut self, scope: &ScopeIdentity) -> &mut Self {
        self.component(scope.adapter_id.as_str().as_bytes());
        self.component(scope.source_instance_key.as_bytes());
        self.component(scope.session_key.as_bytes());
        self
    }

    /// Bind one typed semantic revision plus its source occurrence.
    pub(crate) fn semantic(&mut self, semantic: &FactSemanticRevision) -> &mut Self {
        self.component(semantic.fact_id.as_bytes());
        self.u64(u64::from(
            semantic
                .semantic_revision_ref
                .semantic_reference_contract_version,
        ));
        self.component(semantic.fact_revision_id.as_bytes());
        // The occurrence reference. Two identical accepted revisions arriving
        // from two different records are two deliveries, not one.
        self.component(semantic.source_record_id.as_bytes());
        self
    }

    pub(crate) fn finish(&self) -> ObserverEventId {
        ObserverEventId(*self.hasher.finalize().as_bytes())
    }
}

/// Final root identity for one observer attachment. RFC 012D section 2.13
/// requires this to be settled before any watch is installed or event emitted,
/// including when the root transcript does not exist yet.
#[derive(Debug, Clone)]
pub(crate) struct ScopeIdentity {
    pub adapter_id: AdapterId,
    pub source_instance_key: CanonicalSourceInstanceKey,
    pub native_session_id: String,
    pub session_key: CanonicalEntityKey,
    pub root_actor_run_key: CanonicalEntityKey,
}

impl ScopeIdentity {
    pub(crate) fn derive(
        adapter_id: &AdapterId,
        identity_contract_version: u32,
        stable_instance_discriminator: &[u8],
        native_session_id: &str,
    ) -> Result<Self, SemanticContractError> {
        let source_instance_key = CanonicalSourceInstanceKey::derive(
            identity_contract_version,
            stable_instance_discriminator,
        )?;
        let session_key = CanonicalEntityKey::derive(
            adapter_id.as_str(),
            &source_instance_key,
            "session",
            native_session_id.as_bytes(),
        )?;
        let root_actor_run_key = CanonicalEntityKey::derive_root_actor_run(
            adapter_id.as_str(),
            &source_instance_key,
            &session_key,
            None,
        )?;
        Ok(Self {
            adapter_id: adapter_id.clone(),
            source_instance_key,
            native_session_id: native_session_id.to_string(),
            session_key,
            root_actor_run_key,
        })
    }
}

/// How an event's routing actor was established. Only `Native` and `Derived`
/// are semantic attribution; `ScopeFallback` routes a control or unattributed
/// evidence envelope to the root without claiming the root produced it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ActorAttribution {
    Native,
    Derived,
    ScopeFallback,
}

/// Root + actor identity carried by every event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ActorRef {
    /// RFC 012A canonical session key of the observed root.
    #[ts(type = "string")]
    pub session_key: CanonicalEntityKey,
    /// Native session id as the adapter names it.
    pub native_session_id: String,
    /// Canonical actor-run key. Equals the root run for root-produced events.
    #[ts(type = "string")]
    pub actor_run_key: CanonicalEntityKey,
    pub attribution: ActorAttribution,
}

impl ActorRef {
    pub(crate) fn root(scope: &ScopeIdentity, attribution: ActorAttribution) -> Self {
        Self {
            session_key: scope.session_key,
            native_session_id: scope.native_session_id.clone(),
            actor_run_key: scope.root_actor_run_key,
            attribution,
        }
    }

    pub(crate) fn actor(scope: &ScopeIdentity, actor_run_key: CanonicalEntityKey) -> Self {
        Self {
            session_key: scope.session_key,
            native_session_id: scope.native_session_id.clone(),
            actor_run_key,
            attribution: if actor_run_key == scope.root_actor_run_key {
                ActorAttribution::Native
            } else {
                ActorAttribution::Derived
            },
        }
    }
}
