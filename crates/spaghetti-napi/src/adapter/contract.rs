use std::collections::BTreeSet;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use crate::source::{
    AppendDelimitedConfig, DirectorySnapshotConfig, IngestPriority, PresenceObjectConfig,
    ReplaceDocumentConfig, SourceRecord,
};

use super::FactBatch;

macro_rules! string_id {
    ($name:ident, $label:literal) => {
        #[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(Arc<str>);

        impl $name {
            pub fn new(value: impl Into<Arc<str>>) -> Result<Self, AdapterError> {
                let value = value.into();
                if value.trim().is_empty() {
                    return Err(AdapterError::invalid_contract(concat!(
                        $label,
                        " must not be empty"
                    )));
                }
                if value.len() > 128 {
                    return Err(AdapterError::invalid_contract(concat!(
                        $label,
                        " exceeds 128 bytes"
                    )));
                }
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter
                    .debug_tuple(stringify!($name))
                    .field(&self.0)
                    .finish()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

string_id!(AdapterId, "adapter id");
string_id!(StreamId, "stream id");
string_id!(DecoderId, "decoder id");
string_id!(CapabilityId, "capability id");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupportLevel {
    Native,
    Derived,
    Estimated,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapabilityGranularity {
    Record,
    Message,
    Turn,
    Run,
    Session,
    Team,
    Project,
    Instance,
    Custom(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    Live,
    EventuallyLive,
    CompletionOnly,
    BackfillOnly,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilitySupport {
    pub level: SupportLevel,
    pub granularity: CapabilityGranularity,
    pub availability: Availability,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityDeclaration {
    pub id: CapabilityId,
    pub support: CapabilitySupport,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SourceInstanceKey(Vec<u8>);

impl SourceInstanceKey {
    pub fn new(value: Vec<u8>) -> Result<Self, AdapterError> {
        if value.is_empty() {
            return Err(AdapterError::invalid_contract(
                "source instance key must not be empty",
            ));
        }
        if value.len() > 4_096 {
            return Err(AdapterError::invalid_contract(
                "source instance key exceeds 4096 bytes",
            ));
        }
        Ok(Self(value))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterManifest {
    pub id: AdapterId,
    pub display_name: String,
    pub adapter_version: String,
    pub contract_version: u32,
    pub source_schema_versions: Vec<String>,
    pub capabilities: Vec<CapabilityDeclaration>,
}

impl AdapterManifest {
    pub fn validate(&self) -> Result<(), AdapterError> {
        if self.display_name.trim().is_empty() {
            return Err(AdapterError::invalid_contract(
                "adapter display name must not be empty",
            ));
        }
        if self.adapter_version.trim().is_empty() {
            return Err(AdapterError::invalid_contract(
                "adapter version must not be empty",
            ));
        }
        if self.contract_version == 0 {
            return Err(AdapterError::invalid_contract(
                "adapter contract version must be greater than zero",
            ));
        }
        let mut capability_ids = BTreeSet::new();
        for capability in &self.capabilities {
            if !capability_ids.insert(capability.id.clone()) {
                return Err(AdapterError::invalid_contract(format!(
                    "adapter declares capability {} more than once",
                    capability.id
                )));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRoot {
    pub name: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceInstanceSpec {
    pub stable_key: SourceInstanceKey,
    pub display_name: String,
    pub roots: Vec<SourceRoot>,
    pub discovery_reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceInstance {
    pub id: u64,
    pub spec: SourceInstanceSpec,
}

impl SourceInstance {
    pub fn root(&self, name: &str) -> Result<&std::path::Path, AdapterError> {
        self.spec
            .roots
            .iter()
            .find(|root| root.name == name)
            .map(|root| root.path.as_path())
            .ok_or_else(|| {
                AdapterError::invalid_contract(format!(
                    "source instance does not declare root {name}"
                ))
            })
    }
}

#[derive(Debug, Clone)]
pub struct DiscoveryContext {
    pub configured_roots: Vec<PathBuf>,
    pub observed_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriverSpec {
    AppendDelimited(AppendDelimitedConfig),
    ReplaceDocument(ReplaceDocumentConfig),
    DirectorySnapshot(DirectorySnapshotConfig),
    Presence(PresenceObjectConfig),
}

impl DriverSpec {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::AppendDelimited(_) => "append_delimited_file",
            Self::ReplaceDocument(_) => "replace_document",
            Self::DirectorySnapshot(_) => "directory_snapshot",
            Self::Presence(_) => "presence_object",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectSelector {
    pub root_name: String,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamAuthority {
    Canonical,
    Supplemental,
    Diagnostic,
    IgnoredDerived,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntityScope {
    Session,
    Run,
    Project,
    Instance,
    Custom(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsistencyPolicy {
    IncrementalCursor,
    SnapshotReplace,
    SnapshotDiff,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeletionPolicy {
    MirrorSource,
    PreserveHistory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawRetentionPolicy {
    Full,
    HashOnly,
    ProvenanceOnly,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamSpec {
    pub id: StreamId,
    pub driver: DriverSpec,
    pub selector: ObjectSelector,
    pub decoder: DecoderId,
    pub authority: StreamAuthority,
    pub entity_scope: EntityScope,
    pub priority: IngestPriority,
    pub consistency: ConsistencyPolicy,
    pub deletion: DeletionPolicy,
    pub retention: RawRetentionPolicy,
    pub capabilities: Vec<CapabilityId>,
}

impl StreamSpec {
    pub fn validate(&self, instance: &SourceInstance) -> Result<(), AdapterError> {
        if self.selector.include.is_empty() {
            return Err(AdapterError::invalid_contract(format!(
                "stream {} has no include selector",
                self.id
            )));
        }
        instance.root(&self.selector.root_name)?;
        let mut capabilities = BTreeSet::new();
        for capability in &self.capabilities {
            if !capabilities.insert(capability.clone()) {
                return Err(AdapterError::invalid_contract(format!(
                    "stream {} declares capability {capability} more than once",
                    self.id
                )));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceObjectDescriptor {
    pub stream_id: StreamId,
    pub object_key: Vec<u8>,
    pub relative_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterObjectContext {
    version: u32,
    payload: Vec<u8>,
}

impl AdapterObjectContext {
    pub const MAX_BYTES: usize = 64 * 1024;

    pub fn empty() -> Self {
        Self {
            version: 1,
            payload: Vec::new(),
        }
    }

    pub fn new(version: u32, payload: Vec<u8>) -> Result<Self, AdapterError> {
        if version == 0 {
            return Err(AdapterError::invalid_contract(
                "object context version must be greater than zero",
            ));
        }
        if payload.len() > Self::MAX_BYTES {
            return Err(AdapterError::invalid_contract(format!(
                "object context exceeds {} bytes",
                Self::MAX_BYTES
            )));
        }
        Ok(Self { version, payload })
    }

    pub fn version(&self) -> u32 {
        self.version
    }

    pub fn payload(&self) -> &[u8] {
        &self.payload
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeDisposition {
    Applied,
    IgnoredKnown,
    PreservedUnknown,
    RetryTransient,
}

pub struct DecodeContext<'a> {
    pub decoder: &'a DecoderId,
    pub object_context: &'a AdapterObjectContext,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterErrorClass {
    Transient,
    RecordPermanent,
    StreamFatal,
    AdapterFatal,
    InvalidContract,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterDiagnostic {
    pub class: AdapterErrorClass,
    pub code: String,
    pub message: String,
}

#[derive(Debug, thiserror::Error)]
#[error("{class:?} adapter error [{code}]: {message}")]
pub struct AdapterError {
    pub class: AdapterErrorClass,
    pub code: String,
    pub message: String,
}

impl AdapterError {
    pub fn new(
        class: AdapterErrorClass,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            class,
            code: code.into(),
            message: message.into(),
        }
    }

    pub fn invalid_contract(message: impl Into<String>) -> Self {
        Self::new(
            AdapterErrorClass::InvalidContract,
            "invalid_contract",
            message,
        )
    }

    pub fn unknown_decoder(decoder: &DecoderId) -> Self {
        Self::new(
            AdapterErrorClass::InvalidContract,
            "unknown_decoder",
            format!("adapter does not declare decoder {decoder}"),
        )
    }
}

pub trait AgentAdapter: Send + Sync + 'static {
    fn manifest(&self) -> &AdapterManifest;

    fn discover(&self, context: &DiscoveryContext)
        -> Result<Vec<SourceInstanceSpec>, AdapterError>;

    fn streams(&self, instance: &SourceInstance) -> Result<Vec<StreamSpec>, AdapterError>;

    fn bootstrap_object(
        &self,
        _instance: &SourceInstance,
        _object: &SourceObjectDescriptor,
    ) -> Result<AdapterObjectContext, AdapterError> {
        Ok(AdapterObjectContext::empty())
    }

    fn decode(
        &self,
        context: DecodeContext<'_>,
        record: &SourceRecord,
        output: &mut FactBatch,
    ) -> Result<DecodeDisposition, AdapterError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_reject_empty_or_unbounded_values() {
        assert!(AdapterId::new("").is_err());
        assert!(StreamId::new("x".repeat(129)).is_err());
        assert_eq!(DecoderId::new("decoder").unwrap().as_str(), "decoder");
    }

    #[test]
    fn object_context_is_bounded_and_versioned() {
        assert!(AdapterObjectContext::new(0, Vec::new()).is_err());
        assert!(
            AdapterObjectContext::new(1, vec![0; AdapterObjectContext::MAX_BYTES + 1]).is_err()
        );
        let context = AdapterObjectContext::new(2, b"opaque".to_vec()).unwrap();
        assert_eq!(context.version(), 2);
        assert_eq!(context.payload(), b"opaque");
    }

    #[test]
    fn manifest_rejects_duplicate_capability_declarations() {
        let capability = CapabilityDeclaration {
            id: CapabilityId::new("runtime.subagents").unwrap(),
            support: CapabilitySupport {
                level: SupportLevel::Derived,
                granularity: CapabilityGranularity::Run,
                availability: Availability::Live,
                notes: None,
            },
        };
        let manifest = AdapterManifest {
            id: AdapterId::new("fixture").unwrap(),
            display_name: "Fixture".to_string(),
            adapter_version: "1".to_string(),
            contract_version: 1,
            source_schema_versions: Vec::new(),
            capabilities: vec![capability.clone(), capability],
        };
        assert!(manifest.validate().is_err());
    }
}
