//! Open agent-adapter and typed fact boundary for RFC 011.
//!
//! Adapters discover native sources and decode common-driver records into
//! storage-agnostic facts. They never receive a Spaghetti database handle.

mod contract;
mod facts;
mod registry;

pub use contract::{
    AdapterDiagnostic, AdapterError, AdapterErrorClass, AdapterId, AdapterManifest,
    AdapterObjectContext, AgentAdapter, Availability, CapabilityDeclaration, CapabilityGranularity,
    CapabilityId, CapabilitySupport, ConsistencyPolicy, DecodeContext, DecodeDisposition,
    DecoderId, DeletionPolicy, DiscoveryContext, DriverSpec, EntityScope, ObjectSelector,
    RawRetentionPolicy, SourceInstance, SourceInstanceKey, SourceInstanceSpec,
    SourceObjectDescriptor, SourceRoot, StreamAuthority, StreamId, StreamSpec, SupportLevel,
};
pub use facts::{
    ContentBlock, DelegationFact, DelegationKind, EntityKey, EvidenceKind, EvidenceStrength, Fact,
    FactBatch, FactEnvelope, FactId, FactProvenance, MessageFact, MessageRole, QualifiedTimestamp,
    RelationStrength, RunEvidenceFact, RunFact, SessionFact, TimestampQuality, TokenUsage,
    UsageAccounting, UsageFact, UsageScope, ValueQuality,
};
pub use registry::{AdapterRegistry, AdapterRegistryBuilder};
