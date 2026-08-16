//! Open agent-adapter and typed fact boundary for RFC 011.
//!
//! Adapters discover native sources and decode common-driver records into
//! storage-agnostic facts. They never receive a Spaghetti database handle.

mod contract;
mod facts;
mod registry;
mod semantic;

pub use contract::{
    AdapterDiagnostic, AdapterError, AdapterErrorClass, AdapterId, AdapterManifest,
    AdapterObjectContext, AgentAdapter, Availability, CapabilityDeclaration, CapabilityGranularity,
    CapabilityId, CapabilitySupport, ConsistencyPolicy, DecodeContext, DecodeDisposition,
    DecoderId, DeletionPolicy, DependencyRevision, DiscoveryContext, DriverSpec, EntityScope,
    ObjectSelector, RawRetentionPolicy, SourceAccess, SourceInstance, SourceInstanceKey,
    SourceInstanceSpec, SourceListedObject, SourceObjectDescriptor, SourceObjectList,
    SourceObjectListRequest, SourceQuery, SourceQueryBounds, SourceRoot, SourceRows,
    SourceSnapshot, StreamAuthority, StreamId, StreamSpec, SupportLevel,
};
pub use facts::{
    ArtifactCapture, ArtifactContentFact, ArtifactMetadataEntry, ArtifactMetadataSnapshotFact,
    ArtifactObservationKind, ContentBlock, DelegationFact, DelegationKind, DelegationMetadataFact,
    DelegationSpawnFact, EntityKey, EvidenceKind, EvidenceStrength, Fact, FactBatch, FactEnvelope,
    FactId, FactProvenance, HookEventSummary, InterpretationSettingsDocumentStatus,
    InterpretationSettingsFact, InterpretationSettingsLayer, InterpretationSettingsSnapshot,
    MessageFact, MessageRole, PersistedToolResultFact, PlanSnapshotFact, PresenceFact,
    ProjectMemoryDocumentFact, QualifiedTimestamp, RelationStrength, RunEvidenceFact, RunFact,
    SessionFact, SessionIndexEntrySnapshot, SessionIndexSnapshotFact, TaskCollectionKind,
    TaskItemSnapshot, TaskSnapshotCoverage, TaskSnapshotFact, TaskStatus, TeamInboxMessageSnapshot,
    TeamInboxSnapshotFact, TeamMemberSnapshot, TeamSnapshotFact, TimestampQuality, TokenUsage,
    UsageAccounting, UsageFact, UsageScope, ValueQuality, WorkflowMemberEventFact,
    WorkflowMemberEventKind, WorkflowSnapshotFact, WorkflowStatus,
};
pub use registry::{AdapterRegistry, AdapterRegistryBuilder};
pub use semantic::{
    compare_coverage, CanonicalEntityKey, CanonicalFactId, CanonicalSourceInstanceKey,
    ContractCompleteness, CoverageAbsence, CoverageAbsenceKind, CoverageComparison,
    CoverageDeclarationDigest, CoverageDomain, CoverageError, CoverageMembershipRevision,
    CoverageObjectKey, CoveragePosition, CoveragePositionKind, CoveragePositionRef,
    CoverageProvenance, CoverageScope, CoverageSetCompleteness, CoverageStatus, CoverageStreamKey,
    ExternalEntityRef, FactRevisionId, NativeIdentity, NativeIdentityClaim, QualifiedUnknownReason,
    QualifiedValue, QualifiedValueQuality, SemanticContractError, SemanticRevisionRef,
    SourceCoveragePoint, SourceCoverageSet, SourceRecordId, EXTERNAL_ENTITY_REFERENCE_VERSION,
    SEMANTIC_REFERENCE_CONTRACT_VERSION, SOURCE_COVERAGE_CONTRACT_VERSION,
    SOURCE_COVERAGE_SET_CONTRACT_VERSION,
};
