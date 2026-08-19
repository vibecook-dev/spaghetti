//! Open agent-adapter and typed fact boundary for RFC 011.
//!
//! Adapters discover native sources and decode common-driver records into
//! storage-agnostic facts. They never receive a Spaghetti database handle.

mod contract;
mod facts;
mod registry;
mod scope;
mod semantic;
mod support;

#[cfg(test)]
mod runtime_contract_fixture;

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
    ActorAffiliationDimension, ActorAffiliationRevisionFact, ActorAffiliationState,
    ActorRunRevisionFact, ActorRunRole, ArtifactCapture, ArtifactContentFact,
    ArtifactMetadataEntry, ArtifactMetadataSnapshotFact, ArtifactObservationKind, ContentBlock,
    DelegationFact, DelegationKind, DelegationMetadataFact, DelegationSpawnFact, EntityKey,
    EvidenceKind, EvidenceStrength, Fact, FactBatch, FactEnvelope, FactId, FactProvenance,
    FactSemanticContext, FactSemanticRevision, HookEventSummary,
    InterpretationSettingsDocumentStatus, InterpretationSettingsFact, InterpretationSettingsLayer,
    InterpretationSettingsSnapshot, MessageFact, MessageRole, PersistedToolResultFact,
    PlanSnapshotFact, PresenceFact, ProjectMemoryDocumentFact, QualifiedTimestamp,
    RelationStrength, RunEvidenceFact, RunFact, SessionFact, SessionIndexEntrySnapshot,
    SessionIndexSnapshotFact, TaskCollectionKind, TaskItemSnapshot, TaskSnapshotCoverage,
    TaskSnapshotFact, TaskStatus, TeamInboxMessageSnapshot, TeamInboxSnapshotFact,
    TeamMemberSnapshot, TeamSnapshotFact, TimestampQuality, TokenUsage, UsageAccounting,
    UsageBucketsV2, UsageFact, UsageQualifiedValue, UsageResponseIdentity, UsageRevisionV2Fact,
    UsageScope, UsageValueAuthority, UsageValueProvenance, ValueQuality, WorkflowMemberEventFact,
    WorkflowMemberEventKind, WorkflowSnapshotFact, WorkflowStatus,
};
#[cfg(test)]
pub(crate) use registry::tests::{
    scoped_access_request as fixture_scoped_access_request, supported_fixture_registry_with_scope,
};
pub use registry::{AdapterRegistry, AdapterRegistryBuilder};
pub use scope::{
    ScopeContractError, ScopeProgramDeclaration, ScopeProgramManifest, ScopeProgramStatus,
    ScopeRelationBounds, ScopeRelationDeclaration, ScopeRelationPrimitive,
    ScopeRelationSourceBinding, ScopeRelationSourcePrimitive, ScopeUnavailableBehavior,
    SCOPE_PROGRAM_SCHEMA_VERSION,
};
pub(crate) use semantic::CoverageMembershipRevisionBuilder;
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
pub use support::{
    classify_runtime_support, select_contract_versions, verify_support_release_bundle,
    AdapterSupportBinding, AdapterSupportRegistration, ArtifactCompatibilityDeclaration,
    ArtifactVersionRange, AuthorizedCatalogAccess, AuthorizedScopeProgram, CompatibilityClass,
    CompatibilityDecision, CompatibilityReason, ContractVersionOffer, ContractVersionRequest,
    ContractVersionSelection, NativeArtifactProbe, OperationAuthorization, OperationPermissions,
    Sha256Digest, SupportBundleDocument, SupportCapabilityDeclaration, SupportCapabilityLevel,
    SupportCapabilityTopology, SupportCatalog, SupportContractError, SupportOperation,
    SupportReleaseDescriptor, SupportReleaseStatus, TypedAccessAuthorization,
    VerifiedSupportRelease, CONTRACT_VERSION_SELECTION_VERSION, SUPPORT_RELEASE_SCHEMA_VERSION,
    SUPPORT_SELECTION_CONTRACT_VERSION,
};
