//! Open agent-adapter and typed fact boundary for RFC 011.
//!
//! Adapters discover native sources and decode common-driver records into
//! storage-agnostic facts. They never receive a Spaghetti database handle.

#[cfg(test)]
mod builtin_support;
mod catalog;
mod contract;
mod disposition;
mod facts;
mod probe;
mod registry;
mod runtime_value;
mod scope;
mod semantic;
mod support;

#[cfg(test)]
mod runtime_contract_fixture;

#[cfg(test)]
pub(crate) use builtin_support::{
    verified_builtin_support_catalog, verified_claude_candidate_for_test,
};
pub use catalog::{
    AssociationQuality, CatalogDiscoveryLimits, DiscoveredAssociationConflict, DiscoveredProject,
    DiscoveredSession, ProjectAssociationBasis, SourceCatalogDiscovery,
};
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
pub(crate) use disposition::{BoundedNativeEvidence, RecordMappingDisposition};
pub(crate) use facts::MAX_UNKNOWN_RAW_PAYLOAD_BYTES;
pub use facts::{
    ActorAffiliationDimension, ActorAffiliationRevisionFact, ActorAffiliationState,
    ActorRunRevisionFact, ActorRunRole, ArtifactCapture, ArtifactContentFact,
    ArtifactMetadataEntry, ArtifactMetadataSnapshotFact, ArtifactObservationKind, ContentBlock,
    ContentBlockRevisionFact, ContentBlockRevisionValue, DelegationFact, DelegationKind,
    DelegationMetadataFact, DelegationSpawnFact, EffectiveStateDimension,
    EffectiveStateEvidenceKind, EffectiveStateQualifiedValue, EffectiveStateRevisionFact,
    EffectiveStateValueAuthority, EffectiveStateValueProvenance, EntityKey, EvidenceKind,
    EvidenceStrength, Fact, FactBatch, FactEnvelope, FactId, FactProvenance, FactSemanticContext,
    FactSemanticRevision, HookEventSummary, InterpretationSettingsDocumentStatus,
    InterpretationSettingsFact, InterpretationSettingsLayer, InterpretationSettingsSnapshot,
    MessageFact, MessageRevisionFact, MessageRevisionRole, MessageRole, NativeCompactionPhase,
    NativeProgressState, NativeQueueOperation, NativeRuntimeMarkerProvenance,
    NativeRuntimeMarkerRevisionFact, NativeRuntimeMarkerValue, PersistedToolResultFact,
    PlanRevisionFact, PlanSnapshotFact, PresenceFact, ProjectMemoryDocumentFact, RelationStrength,
    RunEvidenceFact, RunFact, SessionFact, SessionIndexEntrySnapshot, SessionIndexSnapshotFact,
    TaskCollectionKind, TaskItemSnapshot, TaskLifecycleState, TaskRevisionFact,
    TaskSnapshotCoverage, TaskSnapshotFact, TaskStatus, TeamInboxMessageSnapshot,
    TeamInboxSnapshotFact, TeamMemberSnapshot, TeamSnapshotFact, ToolRevisionFact,
    ToolRevisionKind, UsageBucketsV2, UsageQualifiedValue, UsageResponseIdentity,
    UsageRevisionV2Fact, UsageValueAuthority, UsageValueProvenance, UserInputKind,
    UserInputLifecycleState, UserInputOperation, UserInputOption, UserInputQuestion,
    UserInputRequestRevisionFact, WorkflowMemberEventFact, WorkflowMemberEventKind,
    WorkflowSnapshotFact, WorkflowStatus,
};
pub(crate) use probe::{bounded_file_bytes, platform_id, probe_error, sorted_directory_entries};
pub use registry::{AdapterRegistry, AdapterRegistryBuilder};
pub use runtime_value::RuntimeSemanticValue;
pub use scope::{
    ScopeContractError, ScopeDirectoryIdentityAuthority, ScopeJoinEvidence, ScopeJoinIdentityInput,
    ScopeJoinParameterSet, ScopeJoinUpdate, ScopeObservationSourceBinding, ScopeProgramDeclaration,
    ScopeProgramManifest, ScopeProgramStatus, ScopeRelationBounds, ScopeRelationDeclaration,
    ScopeRelationPrimitive, ScopeRelationSourceBinding, ScopeRelationSourcePrimitive,
    ScopeUnavailableBehavior, SCOPE_PROGRAM_SCHEMA_VERSION,
};
pub(crate) use semantic::{
    bound_fact_revision_id, object_scoped_native_revision_key, RevisionBinding,
};
pub use semantic::{
    compare_coverage, CanonicalEntityKey, CanonicalFactId, CanonicalSourceInstanceKey,
    ContractCompleteness, CoverageAbsence, CoverageAbsenceKind, CoverageComparison,
    CoverageDeclarationDigest, CoverageDomain, CoverageError, CoverageMembershipRevision,
    CoverageObjectKey, CoveragePosition, CoveragePositionKind, CoveragePositionRef,
    CoverageProvenance, CoverageScope, CoverageSetCompleteness, CoverageStatus, CoverageStreamKey,
    ExternalEntityRef, FactRevisionId, NativeIdentity, NativeIdentityClaim, QualifiedTimestamp,
    QualifiedUnknownReason, QualifiedValue, QualifiedValueQuality, SemanticContractError,
    SemanticRevisionRef, SourceCoveragePoint, SourceCoverageSet, SourceRecordId, TimestampQuality,
    EXTERNAL_ENTITY_REFERENCE_VERSION, SEMANTIC_REFERENCE_CONTRACT_VERSION,
    SOURCE_COVERAGE_CONTRACT_VERSION, SOURCE_COVERAGE_SET_CONTRACT_VERSION,
};
pub(crate) use semantic::{decode_opaque_reference, encode_opaque_reference};
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
pub(crate) use support::{
    AuthorizedDurableAccess, AuthorizedObservationSourceAuthority,
    AuthorizedObservationSourceContract, AuthorizedObservationSourceDriver,
};
