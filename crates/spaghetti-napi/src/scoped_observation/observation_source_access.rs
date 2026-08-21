//! Attachment-owned binding for authorized dynamic observation sources.
//!
//! This module owns attachment-bound confined directory listing and member
//! reads. It joins declaration/runtime stream proof to the exact source
//! instance approved by the scoped attachment, prepares nonconstructible
//! decoder inputs, and runs the declaration-owned ReplaceDocument driver plus
//! store-agnostic decode without granting adapters any additional source access.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;

use crate::adapter::{
    AdapterError, AdapterErrorClass, AdapterId, AdapterObjectContext, AgentAdapter,
    CanonicalSourceInstanceKey, CoverageAbsenceKind, CoverageObjectKey, CoverageStreamKey,
    DecodeDisposition, DriverSpec, Fact, FactBatch, FactSemanticContext, RawRetentionPolicy,
    ScopeRelationPrimitive, SourceAccess, SourceInstance, SourceInstanceKey,
    SourceObjectDescriptor, SourceObjectList, SourceObjectListRequest, SourceQuery, SourceRows,
    SourceSnapshot, StreamSpec,
};
use crate::decode_runtime::{
    bootstrap_object_without_source_access, decode_record, diagnostic_excerpt, DecodeRuntimeLimits,
    DecodeRuntimeRequest,
};
use crate::source::{
    confined_relative_path_from_key, confined_relative_path_key, read_stable_file_confined,
    AccessBudgetError, AccessObjectToken, AccessOperation, AccessOutcome,
    AuditedDirectoryScanError, AuthorizedObservationDirectoryEntryReservation,
    AuthorizedObservationDirectoryReadAuthority, AuthorizedObservationDirectoryRootAuthority,
    AuthorizedObservationRuntimeStreamReservation, DirectoryChange, DirectoryCheckpoint,
    DirectoryEntryAuditReservation, DirectoryEntryAuditor, DirectoryEntryKind, DirectoryEntryState,
    DirectoryScan, DirectorySelection, DirectorySnapshot, DirectorySnapshotConfig, FileStamp,
    GlobPattern, RecordOrigin, ReplaceCheckpoint, ReplaceDocument, ReplaceRead, Revision,
    ScopeAccessRequest, SourceRecord, StableRead,
};

use super::{
    ScopedAccessRootGrant, ScopedDecodeFailureClass, ScopedObservationAccessPass,
    ScopedObservationAttachmentAuthority, ScopedSourceFailureClass, ScopedSourceObjectIdentity,
};

const MEMBERSHIP_STREAM_DOMAIN: &[u8] = b"spaghetti/rfc012d/scope-relation-membership-stream/v1\0";
const MEMBERSHIP_OBJECT_NAMESPACE: &str = "spaghetti.scope-relation-membership-v1";
const DIRECTORY_MEMBER_MAX_FACTS: usize = 8_192;
const DIRECTORY_MEMBER_MAX_DIAGNOSTICS: usize = 256;

#[derive(Debug, thiserror::Error)]
pub(crate) enum ScopedObservationRuntimeSourceError {
    #[error("scoped observation source binding does not match the active attachment")]
    InvalidBinding,
    #[error("scoped observation host closed before source binding completed")]
    Closed,
    #[error(transparent)]
    Access(#[from] AccessBudgetError),
    #[error("scoped observation directory scan failed")]
    DirectoryScan,
}

/// Exact source-instance/root join underneath one runtime stream reservation.
/// Native root and relative locator remain separate so no unconfined joined
/// path can escape this private boundary.
pub(crate) struct ScopedObservationRuntimeSourceBinding {
    runtime: AuthorizedObservationRuntimeStreamReservation,
    adapter: Arc<dyn AgentAdapter>,
    source_instance: Arc<SourceInstance>,
    root: PathBuf,
    canonical_source_instance_key: CanonicalSourceInstanceKey,
}

impl fmt::Debug for ScopedObservationRuntimeSourceBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScopedObservationRuntimeSourceBinding")
            .field("has_adapter", &true)
            .field("has_source_instance", &true)
            .field("has_native_root", &true)
            .field(
                "has_relative_selector",
                &self.runtime.relative_selector().is_some(),
            )
            .finish_non_exhaustive()
    }
}

impl ScopedObservationRuntimeSourceBinding {
    pub(crate) fn bind(
        runtime: AuthorizedObservationRuntimeStreamReservation,
        adapter: Arc<dyn AgentAdapter>,
        instance: Arc<SourceInstance>,
        approved_root: &ScopedAccessRootGrant,
        expected_source_instance_key: &CanonicalSourceInstanceKey,
    ) -> Result<Self, ScopedObservationRuntimeSourceError> {
        let canonical_source_instance_key = CanonicalSourceInstanceKey::derive(
            instance.spec.identity_contract_version,
            instance.spec.stable_key.as_bytes(),
        );
        let matches = canonical_source_instance_key
            .as_ref()
            .is_ok_and(|actual| actual == expected_source_instance_key)
            && adapter.manifest().id.as_str() == runtime.adapter_id()
            && instance.id == runtime.source_instance_id()
            && instance.spec.identity_contract_version
                == runtime.source_instance_identity_contract_version()
            && &instance.spec.stable_key == runtime.source_instance_key()
            && approved_root.access_root == runtime.access_root()
            && !approved_root.root.as_os_str().is_empty()
            && approved_root.root.is_absolute()
            && instance
                .spec
                .roots
                .iter()
                .filter(|root| root.name == runtime.access_root())
                .count()
                == 1
            && instance
                .root(runtime.access_root())
                .is_ok_and(|root| root == approved_root.root);
        if !matches {
            runtime.fail_conservative();
            return Err(ScopedObservationRuntimeSourceError::InvalidBinding);
        }
        Ok(Self {
            runtime,
            adapter,
            source_instance: instance,
            root: approved_root.root.clone(),
            canonical_source_instance_key: *expected_source_instance_key,
        })
    }

    pub(crate) fn relation_id(&self) -> &str {
        self.runtime.relation_id()
    }

    pub(crate) fn access_root(&self) -> &str {
        self.runtime.access_root()
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn locator(&self) -> &Path {
        self.runtime.locator()
    }

    pub(crate) fn relative_selector(&self) -> Option<&str> {
        self.runtime.relative_selector()
    }

    pub(crate) fn object_token(&self) -> AccessObjectToken {
        self.runtime.object_token()
    }

    pub(crate) fn support_release_digest(&self) -> &[u8; 32] {
        self.runtime.support_release_digest()
    }

    pub(crate) fn source_declaration_digest(&self) -> &[u8; 32] {
        self.runtime.source_declaration_digest()
    }

    pub(crate) fn scope_program_digest(&self) -> &[u8; 32] {
        self.runtime.scope_program_digest()
    }

    pub(crate) fn source_instance_id(&self) -> u64 {
        self.runtime.source_instance_id()
    }

    pub(crate) fn source_instance_key(&self) -> &SourceInstanceKey {
        self.runtime.source_instance_key()
    }

    pub(crate) fn canonical_source_instance_key(&self) -> CanonicalSourceInstanceKey {
        self.canonical_source_instance_key
    }

    pub(crate) fn stream(&self) -> &StreamSpec {
        self.runtime.stream()
    }

    fn adapter(&self) -> &Arc<dyn AgentAdapter> {
        &self.adapter
    }

    fn source_instance(&self) -> &Arc<SourceInstance> {
        &self.source_instance
    }

    pub(crate) fn complete(
        self,
        bytes_read: u64,
        outcome: AccessOutcome,
    ) -> Result<(), AccessBudgetError> {
        self.runtime.complete(bytes_read, outcome)
    }

    fn complete_directory_listing(
        self,
        authority: &AuthorizedObservationDirectoryRootAuthority,
        outcome: AccessOutcome,
    ) -> Result<Option<AuthorizedObservationDirectoryReadAuthority>, AccessBudgetError> {
        self.runtime.complete_directory_listing(authority, outcome)
    }

    pub(crate) fn fail_conservative(self) {
        self.runtime.fail_conservative();
    }
}

/// Pass-borrowed owner of one exact dynamic source binding. The attachment's
/// approved root cannot be replaced after this value is created, and dropping
/// the value still conservatively consumes its common reservation.
pub(crate) struct ScopedObservationRuntimeSourceReservation<'pass> {
    _pass: &'pass ScopedObservationAccessPass,
    binding: ScopedObservationRuntimeSourceBinding,
}

/// Pre-I/O directory-membership authority compiled only from one exact
/// attachment-owned child-directory reservation. The selector and scan bounds
/// are no longer caller inputs, and the membership source identity is derived
/// without retaining a native path.
pub(crate) struct ScopedObservationDirectoryMembershipContract<'pass> {
    reservation: ScopedObservationRuntimeSourceReservation<'pass>,
    proof: ScopedObservationDirectoryMembershipProof,
}

struct ScopedObservationDirectoryScanAuthority {
    binding: ScopedObservationRuntimeSourceBinding,
    proof: ScopedObservationDirectoryMembershipProof,
}

pub(crate) struct ScopedObservationDirectoryMembershipProof {
    identity: ScopedObservationDirectoryContractIdentity,
    authority: AuthorizedObservationDirectoryRootAuthority,
    // Every yielded entry is retained only as an opaque token, verified kind,
    // and declaration-selection bit, including ignored entries.
    entries: BTreeMap<AccessObjectToken, ScopedObservationDirectoryAccountedEntry>,
    failed: bool,
}

#[derive(Clone)]
struct ScopedObservationDirectoryContractIdentity {
    attachment_authority: Arc<ScopedObservationAttachmentAuthority>,
    relation_id: String,
    source: ScopedSourceObjectIdentity,
    support_release_digest: [u8; 32],
    source_declaration_digest: [u8; 32],
    scope_program_digest: [u8; 32],
    config: DirectorySnapshotConfig,
    selector: GlobPattern,
    root_object_token: AccessObjectToken,
}

impl PartialEq for ScopedObservationDirectoryContractIdentity {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.attachment_authority, &other.attachment_authority)
            && self.relation_id == other.relation_id
            && self.source == other.source
            && self.support_release_digest == other.support_release_digest
            && self.source_declaration_digest == other.source_declaration_digest
            && self.scope_program_digest == other.scope_program_digest
            && self.config == other.config
            && self.selector == other.selector
            && self.root_object_token == other.root_object_token
    }
}

impl Eq for ScopedObservationDirectoryContractIdentity {}

#[derive(Clone, Copy, PartialEq, Eq)]
struct ScopedObservationDirectoryAccountedEntry {
    kind: DirectoryEntryKind,
    selected: bool,
    parent_token: AccessObjectToken,
    depth: u32,
}

struct ScopedObservationDirectoryMemberCoordinate {
    binding: ScopedObservationDirectoryMemberBinding,
    parent_token: AccessObjectToken,
    depth: u32,
    relative_path: PathBuf,
    expected: DirectoryEntryState,
}

#[derive(Clone)]
pub(crate) struct ScopedObservationDirectoryMemberIdentity {
    attachment_authority: Arc<ScopedObservationAttachmentAuthority>,
    relation_id: Arc<str>,
    object_token: AccessObjectToken,
    source: ScopedSourceObjectIdentity,
    semantic_context: FactSemanticContext,
    listing_generation: u64,
    listing_revision: Revision,
    entry_generation: u64,
    entry_revision: Revision,
}

/// Native, non-serializable decoder coordinates for one exact selected child.
/// The descriptor path remains private while the path-free member identity is
/// safe to retain separately as coverage membership.
#[derive(Clone)]
pub(crate) struct ScopedObservationDirectoryMemberBinding {
    identity: ScopedObservationDirectoryMemberIdentity,
    adapter: Arc<dyn AgentAdapter>,
    source_instance: Arc<SourceInstance>,
    runtime_stream: Arc<StreamSpec>,
    descriptor: SourceObjectDescriptor,
}

/// One completed, non-serializable listing proof. Native relative names remain
/// private inside the common checkpoint; Debug and later coverage projection
/// expose only stable source identity, revision, generation, and counts.
pub(crate) struct ScopedObservationDirectoryListing {
    identity: ScopedObservationDirectoryContractIdentity,
    checkpoint: DirectoryCheckpoint,
    changes: Vec<DirectoryChange>,
    root_moved: bool,
    accounted_entries: BTreeMap<AccessObjectToken, ScopedObservationDirectoryAccountedEntry>,
    read_authority: Option<AuthorizedObservationDirectoryReadAuthority>,
    root: PathBuf,
    members: Vec<ScopedObservationDirectoryMemberCoordinate>,
    completed_members: Vec<ScopedObservationDirectoryMemberIdentity>,
    next_member_read: usize,
    member_read_failed: bool,
}

#[derive(Debug)]
pub(crate) enum ScopedObservationDirectoryScan {
    Unavailable,
    RetryTransient,
    Snapshot(Box<ScopedObservationDirectoryListing>),
}

/// One exact selected member read under its listing-derived authority. Retry
/// carries no bytes because the membership checkpoint became stale. Oversized
/// retains only opaque identity/revision and never enters decode or projection.
/// Stable content remains crate-private until the declaration-owned
/// ReplaceDocument driver and decode runtime consume it.
pub(crate) enum ScopedObservationDirectoryMemberRead {
    RetryTransient,
    Oversized {
        binding: ScopedObservationDirectoryMemberBinding,
    },
    Stable(ScopedObservationDirectoryMemberContent),
}

pub(crate) struct ScopedObservationDirectoryMemberContent {
    binding: ScopedObservationDirectoryMemberBinding,
    stamp: FileStamp,
    content_revision: Revision,
    bytes: Vec<u8>,
}

/// One successfully bootstrapped whole-document member. Callers cannot supply
/// or replace its adapter object context, stream, source instance, descriptor,
/// semantic context, revision, or payload.
pub(crate) struct ScopedObservationDirectoryMemberDecodeInput {
    binding: ScopedObservationDirectoryMemberBinding,
    object_context: AdapterObjectContext,
    stamp: FileStamp,
    content_revision: Revision,
    bytes: Vec<u8>,
}

pub(crate) struct ScopedObservationDirectoryMemberBootstrapFailure {
    class: ScopedDecodeFailureClass,
    content: Box<ScopedObservationDirectoryMemberContent>,
}

/// Initial `ReplaceDocument` framing of one exact retained member. The record
/// remains paired with the same nonconstructible decoder binding and object
/// context; numeric origin coordinates cannot replace semantic identity.
pub(crate) struct ScopedObservationDirectoryMemberRecordInput {
    binding: ScopedObservationDirectoryMemberBinding,
    object_context: AdapterObjectContext,
    checkpoint: ReplaceCheckpoint,
    record: SourceRecord,
}

pub(crate) struct ScopedObservationDirectoryMemberFrameFailure {
    class: ScopedSourceFailureClass,
    input: Box<ScopedObservationDirectoryMemberDecodeInput>,
}

/// Whole-document child after one confined ReplaceDocument read and one
/// store-agnostic decode. Facts remain crate-private until admission.
pub(crate) struct ScopedObservationDirectoryMemberDecodedSnapshot {
    binding: ScopedObservationDirectoryMemberBinding,
    object_context: AdapterObjectContext,
    checkpoint: ReplaceCheckpoint,
    record: SourceRecord,
    disposition: DecodeDisposition,
    batch: FactBatch,
    next_decoder_state: Option<Vec<u8>>,
    quarantined: bool,
}

pub(crate) struct ScopedObservationDirectoryMemberObserveFailure {
    kind: ScopedObservationDirectoryMemberObserveFailureKind,
    input: Box<ScopedObservationDirectoryMemberDecodeInput>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScopedObservationDirectoryMemberObserveFailureKind {
    Source(ScopedSourceFailureClass),
    Decode(ScopedDecodeFailureClass),
}

/// Path-free child lifecycle after the declaration-owned ReplaceDocument pass.
/// Present carries decoded facts; missing is explicit absence; oversized never
/// enters decode or projection.
pub(crate) enum ScopedObservationDirectoryMemberLifecycle {
    Present(ScopedObservationDirectoryMemberDecodedSnapshot),
    Absent {
        binding: ScopedObservationDirectoryMemberBinding,
        generation: u64,
        kind: CoverageAbsenceKind,
    },
    Oversized {
        binding: ScopedObservationDirectoryMemberBinding,
    },
}

struct DirectoryMemberSourceAccessDenied;

/// Completed, path-free evidence that one exact native directory entry was
/// accounted beneath the listing root before it can enter membership.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScopedObservationDirectoryEntry {
    object_token: AccessObjectToken,
    kind: DirectoryEntryKind,
    depth: u32,
}

pub(crate) struct ScopedObservationDirectoryEntryReservation<'audit> {
    proof: &'audit mut ScopedObservationDirectoryMembershipProof,
    reservation: Option<AuthorizedObservationDirectoryEntryReservation>,
    object_token: AccessObjectToken,
    parent_token: AccessObjectToken,
    depth: u32,
    file_selected: bool,
}

impl fmt::Debug for ScopedObservationDirectoryMembershipContract<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.proof.fmt(formatter)
    }
}

impl fmt::Debug for ScopedObservationDirectoryMembershipProof {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScopedObservationDirectoryMembershipContract")
            .field("max_entries", &self.identity.config.max_entries)
            .field(
                "max_entries_per_directory",
                &self.identity.config.max_entries_per_directory,
            )
            .field("max_depth", &self.identity.config.max_depth)
            .field("has_relative_selector", &true)
            .field("has_membership_source", &true)
            .field("accounted_entries", &self.entries.len())
            .field("failed", &self.failed)
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for ScopedObservationDirectoryListing {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScopedObservationDirectoryListing")
            .field("generation", &self.checkpoint.generation)
            .field("selected_entries", &self.checkpoint.entries.len())
            .field("accounted_entries", &self.accounted_entries.len())
            .field("changes", &self.changes.len())
            .field("root_moved", &self.root_moved)
            .field("member_reads", &self.next_member_read)
            .field("member_read_count", &self.members.len())
            .field("completed_member_reads", &self.completed_members.len())
            .field("member_read_failed", &self.member_read_failed)
            .field("has_membership_source", &true)
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for ScopedObservationDirectoryMemberIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScopedObservationDirectoryMemberIdentity")
            .field("has_attachment_authority", &true)
            .field("has_relation", &true)
            .field("has_object_token", &true)
            .field("has_source_identity", &true)
            .field("listing_generation", &self.listing_generation)
            .field("has_listing_revision", &true)
            .field("entry_generation", &self.entry_generation)
            .field("has_entry_revision", &true)
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for ScopedObservationDirectoryMemberBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScopedObservationDirectoryMemberBinding")
            .field("identity", &self.identity)
            .field("has_adapter", &true)
            .field("has_source_instance", &true)
            .field("has_runtime_stream", &true)
            .field("has_source_descriptor", &true)
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for ScopedObservationDirectoryMemberRead {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RetryTransient => formatter.write_str("RetryTransient"),
            Self::Oversized { .. } => formatter
                .debug_struct("Oversized")
                .field("has_object_token", &true)
                .field("has_listing_revision", &true)
                .finish(),
            Self::Stable(content) => content.fmt(formatter),
        }
    }
}

impl fmt::Debug for ScopedObservationDirectoryMemberContent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScopedObservationDirectoryMemberContent")
            .field("has_object_token", &true)
            .field("has_listing_revision", &true)
            .field("has_content_revision", &true)
            .field("byte_count", &self.bytes.len())
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for ScopedObservationDirectoryMemberDecodeInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScopedObservationDirectoryMemberDecodeInput")
            .field("binding", &self.binding)
            .field("object_context_version", &self.object_context.version())
            .field("object_context_bytes", &self.object_context.payload().len())
            .field("has_content_revision", &true)
            .field("byte_count", &self.bytes.len())
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for ScopedObservationDirectoryMemberBootstrapFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScopedObservationDirectoryMemberBootstrapFailure")
            .field("class", &self.class)
            .field("content", &self.content)
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for ScopedObservationDirectoryMemberRecordInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScopedObservationDirectoryMemberRecordInput")
            .field("binding", &self.binding)
            .field("object_context_version", &self.object_context.version())
            .field("object_context_bytes", &self.object_context.payload().len())
            .field("generation", &self.checkpoint.generation)
            .field("has_checkpoint_revision", &true)
            .field("record_state", &self.record.state)
            .field("record_bytes", &self.record.payload.len())
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for ScopedObservationDirectoryMemberFrameFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScopedObservationDirectoryMemberFrameFailure")
            .field("class", &self.class)
            .field("input", &self.input)
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for ScopedObservationDirectoryMemberDecodedSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScopedObservationDirectoryMemberDecodedSnapshot")
            .field("binding", &self.binding)
            .field("object_context_version", &self.object_context.version())
            .field("object_context_bytes", &self.object_context.payload().len())
            .field("generation", &self.checkpoint.generation)
            .field("has_checkpoint_revision", &true)
            .field("record_state", &self.record.state)
            .field("record_bytes", &self.record.payload.len())
            .field("disposition", &self.disposition)
            .field("fact_count", &self.batch.facts().len())
            .field("diagnostic_count", &self.batch.diagnostics().len())
            .field("has_next_decoder_state", &self.next_decoder_state.is_some())
            .field("quarantined", &self.quarantined)
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for ScopedObservationDirectoryMemberObserveFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScopedObservationDirectoryMemberObserveFailure")
            .field("kind", &self.kind)
            .field("input", &self.input)
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for ScopedObservationDirectoryMemberLifecycle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Present(snapshot) => formatter.debug_tuple("Present").field(snapshot).finish(),
            Self::Absent {
                generation, kind, ..
            } => formatter
                .debug_struct("Absent")
                .field("generation", generation)
                .field("kind", kind)
                .field("has_binding", &true)
                .finish_non_exhaustive(),
            Self::Oversized { .. } => formatter
                .debug_struct("Oversized")
                .field("has_binding", &true)
                .finish_non_exhaustive(),
        }
    }
}

impl fmt::Debug for ScopedObservationDirectoryEntry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScopedObservationDirectoryEntry")
            .field("kind", &self.kind)
            .field("depth", &self.depth)
            .field("has_object_token", &true)
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for ScopedObservationDirectoryEntryReservation<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScopedObservationDirectoryEntryReservation")
            .field("depth", &self.depth)
            .field("has_object_token", &true)
            .finish_non_exhaustive()
    }
}

impl ScopedObservationDirectoryMembershipContract<'_> {
    pub(crate) fn config(&self) -> &DirectorySnapshotConfig {
        self.proof.config()
    }

    pub(crate) fn source(&self) -> &ScopedSourceObjectIdentity {
        self.proof.source()
    }

    pub(crate) fn reserve_entry(
        &mut self,
        relative_path: &Path,
    ) -> Result<ScopedObservationDirectoryEntryReservation<'_>, ScopedObservationRuntimeSourceError>
    {
        self.proof
            .reserve_entry(&self.reservation.binding, relative_path)
    }

    pub(crate) fn fail_conservative(self) {
        self.reservation.fail_conservative();
    }

    pub(crate) fn scan(
        self,
        previous: Option<&ScopedObservationDirectoryListing>,
    ) -> Result<ScopedObservationDirectoryScan, ScopedObservationRuntimeSourceError> {
        let ScopedObservationDirectoryMembershipContract { reservation, proof } = self;
        let ScopedObservationRuntimeSourceReservation {
            _pass: pass_guard,
            binding,
        } = reservation;
        let _pass_guard = pass_guard;
        ScopedObservationDirectoryScanAuthority { binding, proof }.scan(previous)
    }
}

impl ScopedObservationDirectoryScanAuthority {
    fn fail_conservative(self) {
        self.binding.fail_conservative();
    }

    fn scan(
        mut self,
        previous: Option<&ScopedObservationDirectoryListing>,
    ) -> Result<ScopedObservationDirectoryScan, ScopedObservationRuntimeSourceError> {
        if previous.is_some_and(|previous| previous.identity != self.proof.identity) {
            self.fail_conservative();
            return Err(ScopedObservationRuntimeSourceError::InvalidBinding);
        }
        let root = self.binding.root().to_path_buf();
        let locator = self.binding.locator().to_path_buf();
        let previous_checkpoint = previous.map(|previous| &previous.checkpoint);
        let driver = DirectorySnapshot::new(self.proof.identity.config.clone())
            .map_err(|_| ScopedObservationRuntimeSourceError::InvalidBinding)?;
        let scan =
            match driver.scan_confined_audited(&root, &locator, previous_checkpoint, &mut self) {
                Ok(scan) => scan,
                Err(AuditedDirectoryScanError::Audit(error)) => {
                    self.fail_conservative();
                    return Err(error);
                }
                Err(AuditedDirectoryScanError::Driver(_)) => {
                    self.fail_conservative();
                    return Err(ScopedObservationRuntimeSourceError::DirectoryScan);
                }
            };
        match scan {
            DirectoryScan::Unavailable => {
                let authority = &self.proof.authority;
                if self
                    .binding
                    .complete_directory_listing(authority, AccessOutcome::Unavailable)?
                    .is_some()
                {
                    return Err(ScopedObservationRuntimeSourceError::InvalidBinding);
                }
                Ok(ScopedObservationDirectoryScan::Unavailable)
            }
            DirectoryScan::RetryTransient => {
                self.fail_conservative();
                Ok(ScopedObservationDirectoryScan::RetryTransient)
            }
            DirectoryScan::Snapshot {
                changes,
                checkpoint,
                root_moved,
            } => {
                if !self.proof.matches_checkpoint(&self.binding, &checkpoint) {
                    self.fail_conservative();
                    return Err(ScopedObservationRuntimeSourceError::InvalidBinding);
                }
                let ScopedObservationDirectoryScanAuthority { binding, proof } = self;
                let members = match proof.read_members(&binding, &locator, &checkpoint) {
                    Ok(members) => members,
                    Err(error) => {
                        binding.fail_conservative();
                        return Err(error);
                    }
                };
                let read_authority = binding
                    .complete_directory_listing(&proof.authority, AccessOutcome::Available)?
                    .ok_or(ScopedObservationRuntimeSourceError::InvalidBinding)?;
                Ok(ScopedObservationDirectoryScan::Snapshot(Box::new(
                    ScopedObservationDirectoryListing {
                        identity: proof.identity,
                        checkpoint,
                        changes,
                        root_moved,
                        accounted_entries: proof.entries,
                        read_authority: Some(read_authority),
                        root,
                        members,
                        completed_members: Vec::new(),
                        next_member_read: 0,
                        member_read_failed: false,
                    },
                )))
            }
        }
    }
}

impl ScopedObservationDirectoryMembershipProof {
    pub(crate) fn config(&self) -> &DirectorySnapshotConfig {
        &self.identity.config
    }

    pub(crate) fn source(&self) -> &ScopedSourceObjectIdentity {
        &self.identity.source
    }

    pub(crate) fn accounted_entries(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn is_failed(&self) -> bool {
        self.failed
    }

    pub(crate) fn select(
        &self,
        relative_path: &Path,
        kind: DirectoryEntryKind,
    ) -> DirectorySelection {
        if !GlobPattern::accepts_relative_path(relative_path) {
            return DirectorySelection::Ignore;
        }
        match kind {
            DirectoryEntryKind::Directory => DirectorySelection::Recurse,
            DirectoryEntryKind::File if self.identity.selector.matches_path(relative_path) => {
                DirectorySelection::Include
            }
            DirectoryEntryKind::File => DirectorySelection::Ignore,
        }
    }

    pub(crate) fn reserve_entry<'audit>(
        &'audit mut self,
        binding: &ScopedObservationRuntimeSourceBinding,
        relative_path: &Path,
    ) -> Result<
        ScopedObservationDirectoryEntryReservation<'audit>,
        ScopedObservationRuntimeSourceError,
    > {
        if self.failed || !GlobPattern::accepts_relative_path(relative_path) {
            self.failed = true;
            return Err(ScopedObservationRuntimeSourceError::InvalidBinding);
        }
        let relative_path_key = confined_relative_path_key(relative_path)
            .map_err(|_| ScopedObservationRuntimeSourceError::InvalidBinding)?;
        let file_selected = self.identity.selector.matches_path(relative_path);
        let parent = relative_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty());
        let parent_path_key = parent
            .map(confined_relative_path_key)
            .transpose()
            .map_err(|_| ScopedObservationRuntimeSourceError::InvalidBinding)?;
        let component_count = relative_path
            .components()
            .filter(|component| matches!(component, std::path::Component::Normal(_)))
            .count();
        let depth = u32::try_from(component_count)
            .ok()
            .filter(|depth| *depth > 0)
            .ok_or(ScopedObservationRuntimeSourceError::InvalidBinding)?;
        let parent_token = match parent_path_key.as_deref() {
            Some(parent_key) => {
                let token = binding
                    .runtime
                    .directory_entry_token(&self.authority, parent_key)
                    .map_err(ScopedObservationRuntimeSourceError::Access)?;
                if self.entries.get(&token).map(|entry| entry.kind)
                    != Some(DirectoryEntryKind::Directory)
                {
                    self.failed = true;
                    return Err(ScopedObservationRuntimeSourceError::InvalidBinding);
                }
                token
            }
            None => self.identity.root_object_token,
        };
        let reservation = match binding.runtime.reserve_directory_entry(
            &self.authority,
            &relative_path_key,
            parent_path_key.as_deref(),
            depth,
        ) {
            Ok(reservation) => reservation,
            Err(error) => {
                self.failed = true;
                return Err(ScopedObservationRuntimeSourceError::Access(error));
            }
        };
        let object_token = reservation.object_token();
        if object_token == parent_token || self.entries.contains_key(&object_token) {
            reservation.fail_conservative();
            self.failed = true;
            return Err(ScopedObservationRuntimeSourceError::InvalidBinding);
        }
        Ok(ScopedObservationDirectoryEntryReservation {
            proof: self,
            reservation: Some(reservation),
            object_token,
            parent_token,
            depth,
            file_selected,
        })
    }

    fn matches_checkpoint(
        &self,
        binding: &ScopedObservationRuntimeSourceBinding,
        checkpoint: &DirectoryCheckpoint,
    ) -> bool {
        if self.failed
            || checkpoint.generation == 0
            || checkpoint.entries.len()
                != self.entries.values().filter(|entry| entry.selected).count()
        {
            return false;
        }
        checkpoint.entries.values().all(|state| {
            binding
                .runtime
                .directory_entry_token(&self.authority, &state.path_key)
                .ok()
                .and_then(|token| self.entries.get(&token))
                .is_some_and(|entry| entry.selected && entry.kind == state.kind)
        })
    }

    fn read_members(
        &self,
        binding: &ScopedObservationRuntimeSourceBinding,
        locator: &Path,
        checkpoint: &DirectoryCheckpoint,
    ) -> Result<Vec<ScopedObservationDirectoryMemberCoordinate>, ScopedObservationRuntimeSourceError>
    {
        let mut members = Vec::with_capacity(checkpoint.entries.len());
        let runtime_stream = Arc::new(binding.stream().clone());
        for state in checkpoint.entries.values() {
            if state.kind != DirectoryEntryKind::File {
                return Err(ScopedObservationRuntimeSourceError::InvalidBinding);
            }
            let object_token = binding
                .runtime
                .directory_entry_token(&self.authority, &state.path_key)
                .map_err(ScopedObservationRuntimeSourceError::Access)?;
            let accounted = self
                .entries
                .get(&object_token)
                .filter(|entry| entry.selected && entry.kind == DirectoryEntryKind::File)
                .ok_or(ScopedObservationRuntimeSourceError::InvalidBinding)?;
            let member_path = confined_relative_path_from_key(&state.path_key)
                .map_err(|_| ScopedObservationRuntimeSourceError::InvalidBinding)?;
            let relative_path = locator.join(member_path);
            let canonical_object_key = confined_relative_path_key(&relative_path)
                .map_err(|_| ScopedObservationRuntimeSourceError::InvalidBinding)?;
            let adapter_id = self.identity.source.adapter_id.clone();
            let stream_namespace = runtime_stream.id.as_str();
            let stream_key =
                CoverageStreamKey::derive(adapter_id.as_str(), stream_namespace.as_bytes())
                    .map_err(|_| ScopedObservationRuntimeSourceError::InvalidBinding)?;
            let coverage_object_key =
                CoverageObjectKey::derive(stream_namespace, &canonical_object_key)
                    .map_err(|_| ScopedObservationRuntimeSourceError::InvalidBinding)?;
            let semantic_context = FactSemanticContext::new(
                &adapter_id,
                binding.runtime.source_instance_identity_contract_version(),
                binding.runtime.source_instance_key().as_bytes(),
                stream_namespace.as_bytes(),
                &canonical_object_key,
                runtime_stream.driver.framing_contract_version(),
            )
            .map_err(|_| ScopedObservationRuntimeSourceError::InvalidBinding)?;
            let source = ScopedSourceObjectIdentity {
                adapter_id,
                source_instance_key: semantic_context.source_instance_key(),
                stream_key,
                object_key: coverage_object_key,
            };
            if source.source_instance_key != self.identity.source.source_instance_key {
                return Err(ScopedObservationRuntimeSourceError::InvalidBinding);
            }
            let identity = ScopedObservationDirectoryMemberIdentity {
                attachment_authority: Arc::clone(&self.identity.attachment_authority),
                relation_id: Arc::from(self.identity.relation_id.as_str()),
                object_token,
                source,
                semantic_context,
                listing_generation: checkpoint.generation,
                listing_revision: checkpoint.revision,
                entry_generation: state.generation,
                entry_revision: state.revision,
            };
            members.push(ScopedObservationDirectoryMemberCoordinate {
                binding: ScopedObservationDirectoryMemberBinding {
                    identity,
                    adapter: Arc::clone(binding.adapter()),
                    source_instance: Arc::clone(binding.source_instance()),
                    runtime_stream: Arc::clone(&runtime_stream),
                    descriptor: SourceObjectDescriptor {
                        stream_id: runtime_stream.id.clone(),
                        object_key: canonical_object_key,
                        relative_path: relative_path.clone(),
                    },
                },
                parent_token: accounted.parent_token,
                depth: accounted.depth,
                relative_path,
                expected: state.clone(),
            });
        }
        Ok(members)
    }
}

impl ScopedObservationDirectoryListing {
    pub(crate) fn relation_id(&self) -> &str {
        &self.identity.relation_id
    }

    pub(crate) fn source(&self) -> &ScopedSourceObjectIdentity {
        &self.identity.source
    }

    pub(crate) fn checkpoint(&self) -> &DirectoryCheckpoint {
        &self.checkpoint
    }

    pub(crate) fn change_count(&self) -> usize {
        self.changes.len()
    }

    pub(crate) fn accounted_entry_count(&self) -> usize {
        self.accounted_entries.len()
    }

    pub(crate) fn selected_entry_count(&self) -> usize {
        self.checkpoint.entries.len()
    }

    pub(crate) fn root_moved(&self) -> bool {
        self.root_moved
    }

    pub(crate) fn member_reads_complete(&self) -> bool {
        !self.member_read_failed
            && self.next_member_read == self.members.len()
            && self.completed_members.len() == self.members.len()
    }

    /// Observe one already-bootstrapped selected child through the declaration-
    /// owned ReplaceDocument confined read and the store-agnostic decode
    /// runtime. The listing still owns the approved native root; callers cannot
    /// supply a path.
    pub(crate) fn observe_bootstrapped_member(
        &self,
        input: ScopedObservationDirectoryMemberDecodeInput,
        origin: &RecordOrigin,
    ) -> Result<
        ScopedObservationDirectoryMemberLifecycle,
        ScopedObservationDirectoryMemberObserveFailure,
    > {
        let identity = input.binding.identity();
        if !self.member_reads_complete()
            || self.root.as_os_str().is_empty()
            || !self.root.is_absolute()
            || !identity.matches_attachment(&self.identity.attachment_authority)
            || identity.relation_id.as_ref() != self.identity.relation_id
            || !self
                .completed_members
                .iter()
                .any(|completed| completed.source == identity.source)
        {
            return Err(ScopedObservationDirectoryMemberObserveFailure {
                kind: ScopedObservationDirectoryMemberObserveFailureKind::Source(
                    ScopedSourceFailureClass::InvalidCursor,
                ),
                input: Box::new(input),
            });
        }
        input.observe_replace_confined(&self.root, origin)
    }

    pub(super) fn finalize_for_membership(
        &mut self,
    ) -> Option<BTreeSet<ScopedSourceObjectIdentity>> {
        if !self.member_reads_complete() {
            return None;
        }
        let mut sources = BTreeSet::new();
        for identity in &self.completed_members {
            let expected_source =
                ScopedSourceObjectIdentity::from_semantic_context(&identity.semantic_context)
                    .ok()?;
            if !identity.matches_attachment(&self.identity.attachment_authority)
                || identity.relation_id.as_ref() != self.identity.relation_id
                || identity.listing_generation != self.checkpoint.generation
                || identity.listing_revision != self.checkpoint.revision
                || identity.source != expected_source
                || identity.source.adapter_id != self.identity.source.adapter_id
                || identity.source.source_instance_key != self.identity.source.source_instance_key
                || !sources.insert(identity.source.clone())
            {
                return None;
            }
        }
        self.read_authority = None;
        self.root = PathBuf::new();
        self.members.clear();
        self.next_member_read = 0;
        Some(sources)
    }

    /// Read the next selected member in canonical checkpoint order. No caller
    /// supplies a path or token, and the common reservation is acquired before
    /// the no-follow open. Missing/replaced members invalidate this listing;
    /// a stable oversized member remains an explicit bounded outcome.
    pub(crate) fn read_next_member(
        &mut self,
    ) -> Result<Option<ScopedObservationDirectoryMemberRead>, ScopedObservationRuntimeSourceError>
    {
        if self.member_read_failed {
            return Err(ScopedObservationRuntimeSourceError::InvalidBinding);
        }
        let Some(member) = self.members.get(self.next_member_read) else {
            return Ok(None);
        };
        let authority = self
            .read_authority
            .as_ref()
            .ok_or(ScopedObservationRuntimeSourceError::InvalidBinding)?;
        let reservation = match authority.reserve_member_read(
            member.binding.identity.object_token,
            member.parent_token,
            member.depth,
        ) {
            Ok(reservation) => reservation,
            Err(error) => {
                self.member_read_failed = true;
                return Err(ScopedObservationRuntimeSourceError::Access(error));
            }
        };
        let max_bytes = match usize::try_from(reservation.max_object_bytes()) {
            Ok(max_bytes) if max_bytes > 0 => max_bytes,
            _ => {
                reservation.fail_conservative();
                self.member_read_failed = true;
                return Err(ScopedObservationRuntimeSourceError::InvalidBinding);
            }
        };
        self.next_member_read += 1;
        let read = match read_stable_file_confined(&self.root, &member.relative_path, max_bytes) {
            Ok(read) => read,
            Err(_) => {
                reservation.fail_conservative();
                self.member_read_failed = true;
                return Err(ScopedObservationRuntimeSourceError::DirectoryScan);
            }
        };
        match read {
            StableRead::Missing => {
                reservation.complete(0, AccessOutcome::Unavailable)?;
                self.member_read_failed = true;
                Ok(Some(ScopedObservationDirectoryMemberRead::RetryTransient))
            }
            StableRead::Unstable => {
                reservation.fail_conservative();
                self.member_read_failed = true;
                Ok(Some(ScopedObservationDirectoryMemberRead::RetryTransient))
            }
            StableRead::Oversized(stamp) => {
                if !directory_member_stamp_matches(&stamp, &member.expected) {
                    reservation.fail_conservative();
                    self.member_read_failed = true;
                    return Ok(Some(ScopedObservationDirectoryMemberRead::RetryTransient));
                }
                reservation.complete(0, AccessOutcome::Oversized)?;
                self.completed_members.push(member.binding.identity.clone());
                Ok(Some(ScopedObservationDirectoryMemberRead::Oversized {
                    binding: member.binding.clone(),
                }))
            }
            StableRead::Stable {
                stamp,
                bytes,
                revision,
            } => {
                if !directory_member_stamp_matches(&stamp, &member.expected) {
                    reservation.fail_conservative();
                    self.member_read_failed = true;
                    return Ok(Some(ScopedObservationDirectoryMemberRead::RetryTransient));
                }
                reservation.complete(bytes.len() as u64, AccessOutcome::Available)?;
                self.completed_members.push(member.binding.identity.clone());
                Ok(Some(ScopedObservationDirectoryMemberRead::Stable(
                    ScopedObservationDirectoryMemberContent {
                        binding: member.binding.clone(),
                        stamp,
                        content_revision: revision,
                        bytes,
                    },
                )))
            }
        }
    }
}

fn directory_member_stamp_matches(stamp: &FileStamp, expected: &DirectoryEntryState) -> bool {
    stamp.identity == expected.identity
        && stamp.len == expected.size_bytes
        && stamp.modified_ns == expected.modified_ns
}

impl ScopedObservationDirectoryMemberContent {
    pub(crate) fn object_token(&self) -> AccessObjectToken {
        self.binding.identity().object_token
    }

    pub(crate) fn listing_revision(&self) -> Revision {
        self.binding.identity().listing_revision
    }

    pub(crate) fn content_revision(&self) -> Revision {
        self.content_revision
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub(crate) fn identity(&self) -> &ScopedObservationDirectoryMemberIdentity {
        self.binding.identity()
    }

    pub(super) fn runtime_stream(&self) -> &StreamSpec {
        self.binding.runtime_stream()
    }

    pub(super) fn adapter(&self) -> &Arc<dyn AgentAdapter> {
        self.binding.adapter()
    }

    pub(super) fn source_instance(&self) -> &Arc<SourceInstance> {
        self.binding.source_instance()
    }

    pub(super) fn descriptor(&self) -> &SourceObjectDescriptor {
        self.binding.descriptor()
    }

    pub(super) fn bootstrap(
        self,
    ) -> Result<
        ScopedObservationDirectoryMemberDecodeInput,
        ScopedObservationDirectoryMemberBootstrapFailure,
    > {
        if !self.binding.valid_for_dependency_free_bootstrap() {
            return Err(ScopedObservationDirectoryMemberBootstrapFailure {
                class: ScopedDecodeFailureClass::InvalidContract,
                content: Box::new(self),
            });
        }
        let object_context = match bootstrap_object_without_source_access(
            self.binding.adapter().as_ref(),
            self.binding.source_instance().as_ref(),
            self.binding.descriptor(),
        ) {
            Ok(context) => context,
            Err(error) => {
                return Err(ScopedObservationDirectoryMemberBootstrapFailure {
                    class: super::decode_failure_class(&error),
                    content: Box::new(self),
                });
            }
        };
        Ok(ScopedObservationDirectoryMemberDecodeInput {
            binding: self.binding,
            object_context,
            stamp: self.stamp,
            content_revision: self.content_revision,
            bytes: self.bytes,
        })
    }

    #[cfg(test)]
    pub(crate) fn runtime_stream_for_test(&self) -> &StreamSpec {
        self.runtime_stream()
    }

    #[cfg(test)]
    pub(crate) fn descriptor_for_test(&self) -> &SourceObjectDescriptor {
        self.descriptor()
    }

    #[cfg(test)]
    pub(crate) fn adapter_for_test(&self) -> &Arc<dyn AgentAdapter> {
        self.adapter()
    }

    #[cfg(test)]
    pub(crate) fn source_instance_for_test(&self) -> &Arc<SourceInstance> {
        self.source_instance()
    }

    #[cfg(test)]
    pub(crate) fn bootstrap_for_test(
        self,
    ) -> Result<
        ScopedObservationDirectoryMemberDecodeInput,
        ScopedObservationDirectoryMemberBootstrapFailure,
    > {
        self.bootstrap()
    }
}

impl ScopedObservationDirectoryMemberBinding {
    pub(crate) fn identity(&self) -> &ScopedObservationDirectoryMemberIdentity {
        &self.identity
    }

    pub(super) fn runtime_stream(&self) -> &StreamSpec {
        &self.runtime_stream
    }

    pub(super) fn adapter(&self) -> &Arc<dyn AgentAdapter> {
        &self.adapter
    }

    pub(super) fn source_instance(&self) -> &Arc<SourceInstance> {
        &self.source_instance
    }

    pub(super) fn descriptor(&self) -> &SourceObjectDescriptor {
        &self.descriptor
    }

    pub(crate) fn oversized_lifecycle(self) -> ScopedObservationDirectoryMemberLifecycle {
        ScopedObservationDirectoryMemberLifecycle::Oversized { binding: self }
    }

    fn valid_for_dependency_free_bootstrap(&self) -> bool {
        let identity = self.identity();
        let stream = self.runtime_stream();
        let instance = self.source_instance();
        let descriptor = self.descriptor();
        let canonical_source_instance = CanonicalSourceInstanceKey::derive(
            instance.spec.identity_contract_version,
            instance.spec.stable_key.as_bytes(),
        );
        let include_matches = stream.selector.include.iter().any(|pattern| {
            GlobPattern::new(pattern)
                .ok()
                .is_some_and(|pattern| pattern.matches_path(&descriptor.relative_path))
        });
        let excluded = stream.selector.exclude.iter().any(|pattern| {
            GlobPattern::new(pattern)
                .ok()
                .is_some_and(|pattern| pattern.matches_path(&descriptor.relative_path))
        });
        // Binding creation already checked the exact retained adapter manifest.
        // Keep every later adapter invocation inside the common panic boundary.
        instance.id != 0
            && stream.validate(instance).is_ok()
            && canonical_source_instance.as_ref().ok() == Some(&identity.source.source_instance_key)
            && matches!(stream.driver, DriverSpec::ReplaceDocument(_))
            && stream.id == descriptor.stream_id
            && stream.id.as_str().as_bytes() == identity.semantic_context.stream_key()
            && stream.driver.framing_contract_version()
                == identity.semantic_context.framing_contract_version()
            && descriptor.object_key == identity.semantic_context.object_key()
            && confined_relative_path_key(&descriptor.relative_path)
                .is_ok_and(|key| key == descriptor.object_key)
            && ScopedSourceObjectIdentity::from_semantic_context(&identity.semantic_context)
                .is_ok_and(|source| source == identity.source)
            && include_matches
            && !excluded
    }

    #[cfg(test)]
    pub(crate) fn runtime_stream_for_test(&self) -> &StreamSpec {
        self.runtime_stream()
    }

    #[cfg(test)]
    pub(crate) fn descriptor_for_test(&self) -> &SourceObjectDescriptor {
        self.descriptor()
    }

    #[cfg(test)]
    pub(crate) fn adapter_for_test(&self) -> &Arc<dyn AgentAdapter> {
        self.adapter()
    }

    #[cfg(test)]
    pub(crate) fn source_instance_for_test(&self) -> &Arc<SourceInstance> {
        self.source_instance()
    }
}

impl ScopedObservationDirectoryMemberDecodeInput {
    pub(super) fn frame_initial_replace(
        self,
        origin: RecordOrigin,
    ) -> Result<
        ScopedObservationDirectoryMemberRecordInput,
        ScopedObservationDirectoryMemberFrameFailure,
    > {
        if !self.binding.valid_for_dependency_free_bootstrap()
            || origin.source_instance_id != self.binding.source_instance().id
        {
            return Err(ScopedObservationDirectoryMemberFrameFailure {
                class: ScopedSourceFailureClass::InvalidCursor,
                input: Box::new(self),
            });
        }
        let DriverSpec::ReplaceDocument(config) = &self.binding.runtime_stream().driver else {
            return Err(ScopedObservationDirectoryMemberFrameFailure {
                class: ScopedSourceFailureClass::InvalidConfiguration,
                input: Box::new(self),
            });
        };
        let driver = match ReplaceDocument::new(config.clone()) {
            Ok(driver) => driver,
            Err(error) => {
                return Err(ScopedObservationDirectoryMemberFrameFailure {
                    class: super::source_failure_class(&error),
                    input: Box::new(self),
                });
            }
        };
        let read = match driver.frame_retained_stable(
            &self.stamp,
            &self.bytes,
            self.content_revision,
            None,
            &origin,
            false,
        ) {
            Ok(read) => read,
            Err(error) => {
                return Err(ScopedObservationDirectoryMemberFrameFailure {
                    class: super::source_failure_class(&error),
                    input: Box::new(self),
                });
            }
        };
        let ReplaceRead::Record {
            record,
            checkpoint,
            generation_changed: true,
        } = read
        else {
            return Err(ScopedObservationDirectoryMemberFrameFailure {
                class: ScopedSourceFailureClass::InvalidCursor,
                input: Box::new(self),
            });
        };
        let Self {
            binding,
            object_context,
            ..
        } = self;
        Ok(ScopedObservationDirectoryMemberRecordInput {
            binding,
            object_context,
            checkpoint,
            record,
        })
    }

    /// Re-open the completed child through the retained ReplaceDocument bound
    /// and descriptor, then decode any present record without extra source
    /// access. Missing and oversized outcomes never enter decode.
    pub(super) fn observe_replace_confined(
        self,
        root: &Path,
        origin: &RecordOrigin,
    ) -> Result<
        ScopedObservationDirectoryMemberLifecycle,
        ScopedObservationDirectoryMemberObserveFailure,
    > {
        if !self.binding.valid_for_dependency_free_bootstrap()
            || origin.source_instance_id != self.binding.source_instance().id
            || root.as_os_str().is_empty()
            || !root.is_absolute()
        {
            return Err(ScopedObservationDirectoryMemberObserveFailure {
                kind: ScopedObservationDirectoryMemberObserveFailureKind::Source(
                    ScopedSourceFailureClass::InvalidCursor,
                ),
                input: Box::new(self),
            });
        }
        let DriverSpec::ReplaceDocument(config) = &self.binding.runtime_stream().driver else {
            return Err(ScopedObservationDirectoryMemberObserveFailure {
                kind: ScopedObservationDirectoryMemberObserveFailureKind::Source(
                    ScopedSourceFailureClass::InvalidConfiguration,
                ),
                input: Box::new(self),
            });
        };
        let driver = match ReplaceDocument::new(config.clone()) {
            Ok(driver) => driver,
            Err(error) => {
                return Err(ScopedObservationDirectoryMemberObserveFailure {
                    kind: ScopedObservationDirectoryMemberObserveFailureKind::Source(
                        super::source_failure_class(&error),
                    ),
                    input: Box::new(self),
                });
            }
        };
        let read = match driver.read_confined(
            root,
            &self.binding.descriptor().relative_path,
            None,
            origin,
            false,
        ) {
            Ok(read) => read,
            Err(error) => {
                return Err(ScopedObservationDirectoryMemberObserveFailure {
                    kind: ScopedObservationDirectoryMemberObserveFailureKind::Source(
                        super::source_failure_class(&error),
                    ),
                    input: Box::new(self),
                });
            }
        };
        match read {
            ReplaceRead::Missing => Ok(ScopedObservationDirectoryMemberLifecycle::Absent {
                binding: self.binding,
                generation: 1,
                kind: CoverageAbsenceKind::Absent,
            }),
            ReplaceRead::RetryTransient => Err(ScopedObservationDirectoryMemberObserveFailure {
                kind: ScopedObservationDirectoryMemberObserveFailureKind::Source(
                    ScopedSourceFailureClass::Unstable,
                ),
                input: Box::new(self),
            }),
            ReplaceRead::Unchanged { .. } => Err(ScopedObservationDirectoryMemberObserveFailure {
                kind: ScopedObservationDirectoryMemberObserveFailureKind::Source(
                    ScopedSourceFailureClass::InvalidCursor,
                ),
                input: Box::new(self),
            }),
            ReplaceRead::Removed { checkpoint, .. } => {
                Ok(ScopedObservationDirectoryMemberLifecycle::Absent {
                    binding: self.binding,
                    generation: checkpoint.generation,
                    kind: CoverageAbsenceKind::Deleted,
                })
            }
            ReplaceRead::Quarantined { .. } => {
                Ok(ScopedObservationDirectoryMemberLifecycle::Oversized {
                    binding: self.binding,
                })
            }
            ReplaceRead::Record {
                record,
                checkpoint,
                generation_changed: true,
            } => self.decode_replace_record(record, checkpoint),
            ReplaceRead::Record {
                generation_changed: false,
                ..
            } => Err(ScopedObservationDirectoryMemberObserveFailure {
                kind: ScopedObservationDirectoryMemberObserveFailureKind::Source(
                    ScopedSourceFailureClass::InvalidCursor,
                ),
                input: Box::new(self),
            }),
        }
    }

    fn decode_replace_record(
        self,
        record: SourceRecord,
        checkpoint: ReplaceCheckpoint,
    ) -> Result<
        ScopedObservationDirectoryMemberLifecycle,
        ScopedObservationDirectoryMemberObserveFailure,
    > {
        let attempt = decode_record(DecodeRuntimeRequest {
            adapter: self.binding.adapter().as_ref(),
            decoder: &self.binding.runtime_stream().decoder,
            object_context: &self.object_context,
            source_access: &DirectoryMemberSourceAccessDenied,
            record: &record,
            semantic_context: self.binding.identity().semantic_context(),
            decoder_state: None,
            retention: self.binding.runtime_stream().retention,
            limits: DecodeRuntimeLimits {
                max_facts: DIRECTORY_MEMBER_MAX_FACTS,
                max_diagnostics: DIRECTORY_MEMBER_MAX_DIAGNOSTICS,
            },
        });
        let decoded = match attempt.result {
            Ok(decoded) if decoded.disposition != DecodeDisposition::RetryTransient => decoded,
            Ok(_) => {
                return Err(ScopedObservationDirectoryMemberObserveFailure {
                    kind: ScopedObservationDirectoryMemberObserveFailureKind::Decode(
                        ScopedDecodeFailureClass::Transient,
                    ),
                    input: Box::new(self),
                });
            }
            Err(error) => {
                return Err(ScopedObservationDirectoryMemberObserveFailure {
                    kind: ScopedObservationDirectoryMemberObserveFailureKind::Decode(
                        super::decode_failure_class(&error),
                    ),
                    input: Box::new(self),
                });
            }
        };
        Ok(ScopedObservationDirectoryMemberLifecycle::Present(
            ScopedObservationDirectoryMemberDecodedSnapshot {
                binding: self.binding,
                object_context: self.object_context,
                checkpoint,
                record,
                disposition: decoded.disposition,
                batch: decoded.batch,
                next_decoder_state: decoded.next_decoder_state,
                quarantined: decoded.quarantined,
            },
        ))
    }

    #[cfg(test)]
    pub(crate) fn identity_for_test(&self) -> &ScopedObservationDirectoryMemberIdentity {
        self.binding.identity()
    }

    #[cfg(test)]
    pub(crate) fn runtime_stream_for_test(&self) -> &StreamSpec {
        self.binding.runtime_stream()
    }

    #[cfg(test)]
    pub(crate) fn descriptor_for_test(&self) -> &SourceObjectDescriptor {
        self.binding.descriptor()
    }

    #[cfg(test)]
    pub(crate) fn object_context_for_test(&self) -> &AdapterObjectContext {
        &self.object_context
    }

    #[cfg(test)]
    pub(crate) fn content_revision_for_test(&self) -> Revision {
        self.content_revision
    }

    #[cfg(test)]
    pub(crate) fn bytes_for_test(&self) -> &[u8] {
        &self.bytes
    }

    #[cfg(test)]
    pub(crate) fn frame_initial_replace_for_test(
        self,
        origin: RecordOrigin,
    ) -> Result<
        ScopedObservationDirectoryMemberRecordInput,
        ScopedObservationDirectoryMemberFrameFailure,
    > {
        self.frame_initial_replace(origin)
    }
}

impl ScopedObservationDirectoryMemberBootstrapFailure {
    #[cfg(test)]
    pub(crate) fn class_for_test(&self) -> ScopedDecodeFailureClass {
        self.class
    }

    #[cfg(test)]
    pub(crate) fn into_content_for_test(self) -> ScopedObservationDirectoryMemberContent {
        *self.content
    }
}

impl ScopedObservationDirectoryMemberRecordInput {
    #[cfg(test)]
    pub(crate) fn identity_for_test(&self) -> &ScopedObservationDirectoryMemberIdentity {
        self.binding.identity()
    }

    #[cfg(test)]
    pub(crate) fn runtime_stream_for_test(&self) -> &StreamSpec {
        self.binding.runtime_stream()
    }

    #[cfg(test)]
    pub(crate) fn descriptor_for_test(&self) -> &SourceObjectDescriptor {
        self.binding.descriptor()
    }

    #[cfg(test)]
    pub(crate) fn object_context_for_test(&self) -> &AdapterObjectContext {
        &self.object_context
    }

    #[cfg(test)]
    pub(crate) fn checkpoint_for_test(&self) -> &ReplaceCheckpoint {
        &self.checkpoint
    }

    #[cfg(test)]
    pub(crate) fn record_for_test(&self) -> &SourceRecord {
        &self.record
    }
}

impl ScopedObservationDirectoryMemberFrameFailure {
    #[cfg(test)]
    pub(crate) fn class_for_test(&self) -> ScopedSourceFailureClass {
        self.class
    }

    #[cfg(test)]
    pub(crate) fn into_input_for_test(self) -> ScopedObservationDirectoryMemberDecodeInput {
        *self.input
    }
}

impl ScopedObservationDirectoryMemberDecodedSnapshot {
    pub(super) fn runtime_stream(&self) -> &StreamSpec {
        self.binding.runtime_stream()
    }

    pub(super) fn generation(&self) -> u64 {
        self.checkpoint.generation
    }

    pub(super) fn revision(&self) -> Revision {
        self.checkpoint.revision
    }

    pub(super) fn admission_measurement(&self) -> Option<(u64, u64)> {
        let semantic_items = self
            .batch
            .facts()
            .len()
            .checked_add(self.batch.diagnostics().len())?
            .max(1);
        let data_events = u64::try_from(semantic_items).ok()?;
        let mut retained_native_bytes = match self.binding.runtime_stream().retention {
            RawRetentionPolicy::None | RawRetentionPolicy::HashOnly => 0,
            RawRetentionPolicy::DiagnosticExcerpt => {
                u64::try_from(diagnostic_excerpt(&self.record.payload).len()).ok()?
            }
            RawRetentionPolicy::Full => u64::try_from(self.record.payload.len()).ok()?,
        };
        for fact in self.batch.facts() {
            if let Fact::UnknownRecord { raw_payload, .. } = &fact.value {
                retained_native_bytes =
                    retained_native_bytes.checked_add(u64::try_from(raw_payload.len()).ok()?)?;
            }
        }
        Some((data_events, retained_native_bytes))
    }

    pub(super) fn into_admission_parts(
        self,
    ) -> (
        ScopedObservationDirectoryMemberBinding,
        AdapterObjectContext,
        ReplaceCheckpoint,
        SourceRecord,
        DecodeDisposition,
        FactBatch,
        Option<Vec<u8>>,
        bool,
    ) {
        (
            self.binding,
            self.object_context,
            self.checkpoint,
            self.record,
            self.disposition,
            self.batch,
            self.next_decoder_state,
            self.quarantined,
        )
    }

    #[cfg(test)]
    pub(crate) fn disposition_for_test(&self) -> DecodeDisposition {
        self.disposition
    }

    #[cfg(test)]
    pub(crate) fn fact_count_for_test(&self) -> usize {
        self.batch.facts().len()
    }

    #[cfg(test)]
    pub(crate) fn facts_for_test(&self) -> &[crate::adapter::FactEnvelope] {
        self.batch.facts()
    }

    #[cfg(test)]
    pub(crate) fn record_payload_for_test(&self) -> &[u8] {
        &self.record.payload
    }
}

impl ScopedObservationDirectoryMemberObserveFailure {
    #[cfg(test)]
    pub(crate) fn kind_for_test(&self) -> ScopedObservationDirectoryMemberObserveFailureKind {
        self.kind
    }

    #[cfg(test)]
    pub(crate) fn into_input_for_test(self) -> ScopedObservationDirectoryMemberDecodeInput {
        *self.input
    }
}

impl ScopedObservationDirectoryMemberLifecycle {
    pub(crate) fn source(&self) -> &ScopedSourceObjectIdentity {
        match self {
            Self::Present(snapshot) => snapshot.binding.identity().source(),
            Self::Absent { binding, .. } | Self::Oversized { binding } => {
                binding.identity().source()
            }
        }
    }

    pub(crate) fn relation_id(&self) -> &str {
        match self {
            Self::Present(snapshot) => snapshot.binding.identity().relation_id(),
            Self::Absent { binding, .. } | Self::Oversized { binding } => {
                binding.identity().relation_id()
            }
        }
    }

    pub(super) fn present_snapshot(
        &self,
    ) -> Option<&ScopedObservationDirectoryMemberDecodedSnapshot> {
        match self {
            Self::Present(snapshot) => Some(snapshot),
            Self::Absent { .. } | Self::Oversized { .. } => None,
        }
    }

    pub(super) fn absence(&self) -> Option<(u64, CoverageAbsenceKind)> {
        match self {
            Self::Absent {
                generation, kind, ..
            } => Some((*generation, *kind)),
            Self::Present(_) | Self::Oversized { .. } => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn present_snapshot_for_test(
        &self,
    ) -> Option<&ScopedObservationDirectoryMemberDecodedSnapshot> {
        self.present_snapshot()
    }

    #[cfg(test)]
    pub(crate) fn absence_for_test(&self) -> Option<(u64, CoverageAbsenceKind)> {
        self.absence()
    }
}

impl SourceAccess for DirectoryMemberSourceAccessDenied {
    fn read_object(
        &self,
        _root_name: &str,
        _relative_path: &Path,
        _max_bytes: usize,
    ) -> Result<SourceSnapshot, AdapterError> {
        Err(directory_member_dependency_access_error())
    }

    fn query_source_db(&self, _query: &SourceQuery) -> Result<SourceRows, AdapterError> {
        Err(directory_member_dependency_access_error())
    }

    fn list_objects(
        &self,
        _request: &SourceObjectListRequest,
    ) -> Result<SourceObjectList, AdapterError> {
        Err(directory_member_dependency_access_error())
    }
}

fn directory_member_dependency_access_error() -> AdapterError {
    AdapterError::new(
        AdapterErrorClass::InvalidContract,
        "scoped_dependency_access_undeclared",
        "decoder requested dependency access without a scoped relation-backed grant",
    )
}

impl ScopedObservationDirectoryMemberIdentity {
    pub(crate) fn relation_id(&self) -> &str {
        &self.relation_id
    }

    pub(crate) fn source(&self) -> &ScopedSourceObjectIdentity {
        &self.source
    }

    pub(crate) fn semantic_context(&self) -> &FactSemanticContext {
        &self.semantic_context
    }

    pub(crate) fn listing_generation(&self) -> u64 {
        self.listing_generation
    }

    pub(crate) fn listing_revision(&self) -> Revision {
        self.listing_revision
    }

    pub(crate) fn entry_generation(&self) -> u64 {
        self.entry_generation
    }

    pub(crate) fn entry_revision(&self) -> Revision {
        self.entry_revision
    }

    pub(super) fn matches_attachment(
        &self,
        authority: &Arc<ScopedObservationAttachmentAuthority>,
    ) -> bool {
        Arc::ptr_eq(&self.attachment_authority, authority)
    }
}

impl ScopedObservationDirectoryEntry {
    pub(crate) fn object_token(&self) -> AccessObjectToken {
        self.object_token
    }

    pub(crate) fn kind(&self) -> DirectoryEntryKind {
        self.kind
    }

    pub(crate) fn depth(&self) -> u32 {
        self.depth
    }
}

impl ScopedObservationDirectoryEntryReservation<'_> {
    pub(crate) fn object_token(&self) -> AccessObjectToken {
        self.object_token
    }

    pub(crate) fn selection(&self, kind: DirectoryEntryKind) -> DirectorySelection {
        match kind {
            DirectoryEntryKind::Directory => DirectorySelection::Recurse,
            DirectoryEntryKind::File if self.file_selected => DirectorySelection::Include,
            DirectoryEntryKind::File => DirectorySelection::Ignore,
        }
    }

    pub(crate) fn complete(
        mut self,
        kind: DirectoryEntryKind,
    ) -> Result<ScopedObservationDirectoryEntry, ScopedObservationRuntimeSourceError> {
        let reservation = self
            .reservation
            .take()
            .expect("directory entry reservation is consumed only once");
        let selected = kind == DirectoryEntryKind::File && self.file_selected;
        if let Err(error) = reservation.complete(selected) {
            self.proof.failed = true;
            return Err(ScopedObservationRuntimeSourceError::Access(error));
        }
        if self
            .proof
            .entries
            .insert(
                self.object_token,
                ScopedObservationDirectoryAccountedEntry {
                    kind,
                    selected,
                    parent_token: self.parent_token,
                    depth: self.depth,
                },
            )
            .is_some()
        {
            self.proof.failed = true;
            return Err(ScopedObservationRuntimeSourceError::InvalidBinding);
        }
        Ok(ScopedObservationDirectoryEntry {
            object_token: self.object_token,
            kind,
            depth: self.depth,
        })
    }
}

impl DirectoryEntryAuditor for ScopedObservationDirectoryMembershipContract<'_> {
    type Error = ScopedObservationRuntimeSourceError;
    type Reservation<'audit>
        = ScopedObservationDirectoryEntryReservation<'audit>
    where
        Self: 'audit;

    fn reserve_entry<'audit>(
        &'audit mut self,
        relative_path: &Path,
    ) -> Result<Self::Reservation<'audit>, Self::Error> {
        ScopedObservationDirectoryMembershipContract::reserve_entry(self, relative_path)
    }
}

impl DirectoryEntryAuditor for ScopedObservationDirectoryScanAuthority {
    type Error = ScopedObservationRuntimeSourceError;
    type Reservation<'audit>
        = ScopedObservationDirectoryEntryReservation<'audit>
    where
        Self: 'audit;

    fn reserve_entry<'audit>(
        &'audit mut self,
        relative_path: &Path,
    ) -> Result<Self::Reservation<'audit>, Self::Error> {
        self.proof.reserve_entry(&self.binding, relative_path)
    }
}

impl DirectoryEntryAuditReservation for ScopedObservationDirectoryEntryReservation<'_> {
    type Error = ScopedObservationRuntimeSourceError;

    fn selection(&self, kind: DirectoryEntryKind) -> DirectorySelection {
        ScopedObservationDirectoryEntryReservation::selection(self, kind)
    }

    fn complete(self, kind: DirectoryEntryKind) -> Result<(), Self::Error> {
        ScopedObservationDirectoryEntryReservation::complete(self, kind).map(|_| ())
    }
}

impl Drop for ScopedObservationDirectoryEntryReservation<'_> {
    fn drop(&mut self) {
        let Some(reservation) = self.reservation.take() else {
            return;
        };
        reservation.fail_conservative();
        self.proof.failed = true;
    }
}

impl fmt::Debug for ScopedObservationRuntimeSourceReservation<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.binding.fmt(formatter)
    }
}

impl<'pass> ScopedObservationRuntimeSourceReservation<'pass> {
    pub(crate) fn binding(&self) -> &ScopedObservationRuntimeSourceBinding {
        &self.binding
    }

    pub(crate) fn complete(
        self,
        bytes_read: u64,
        outcome: AccessOutcome,
    ) -> Result<(), AccessBudgetError> {
        self.binding.complete(bytes_read, outcome)
    }

    pub(crate) fn fail_conservative(self) {
        self.binding.fail_conservative();
    }

    pub(crate) fn into_directory_membership_contract(
        self,
    ) -> Result<
        ScopedObservationDirectoryMembershipContract<'pass>,
        ScopedObservationRuntimeSourceError,
    > {
        let prepared = prepare_directory_membership_contract(
            self.binding(),
            Arc::clone(&self._pass.attachment_authority),
        );
        match prepared {
            Ok(proof) => Ok(ScopedObservationDirectoryMembershipContract {
                reservation: self,
                proof,
            }),
            Err(error) => {
                self.fail_conservative();
                Err(error)
            }
        }
    }
}

fn prepare_directory_membership_contract(
    binding: &ScopedObservationRuntimeSourceBinding,
    attachment_authority: Arc<ScopedObservationAttachmentAuthority>,
) -> Result<ScopedObservationDirectoryMembershipProof, ScopedObservationRuntimeSourceError> {
    if binding.runtime.primitive() != ScopeRelationPrimitive::ChildDirectoryByNativeId
        || binding.runtime.operation() != AccessOperation::ObjectListing
    {
        return Err(ScopedObservationRuntimeSourceError::InvalidBinding);
    }
    let bounds = binding.runtime.bounds();
    // The listing root is already one reserved object at depth one. The
    // confined driver therefore receives only the remaining child-object
    // capacity and one fewer relative recursion level. Per-directory fan-out
    // is additionally capped by that remaining aggregate capacity.
    let child_capacity = bounds
        .max_objects
        .checked_sub(1)
        .filter(|capacity| *capacity > 0)
        .ok_or(ScopedObservationRuntimeSourceError::InvalidBinding)?;
    let config = DirectorySnapshotConfig {
        max_entries: usize::try_from(child_capacity)
            .map_err(|_| ScopedObservationRuntimeSourceError::InvalidBinding)?,
        max_entries_per_directory: usize::try_from(bounds.max_fan_out.min(child_capacity))
            .map_err(|_| ScopedObservationRuntimeSourceError::InvalidBinding)?,
        max_depth: usize::try_from(bounds.max_depth.saturating_sub(1))
            .map_err(|_| ScopedObservationRuntimeSourceError::InvalidBinding)?,
    };
    DirectorySnapshot::new(config.clone())
        .map_err(|_| ScopedObservationRuntimeSourceError::InvalidBinding)?;
    let selector = binding
        .relative_selector()
        .ok_or(ScopedObservationRuntimeSourceError::InvalidBinding)
        .and_then(|selector| {
            GlobPattern::new(selector)
                .map_err(|_| ScopedObservationRuntimeSourceError::InvalidBinding)
        })?;
    let adapter_id = AdapterId::new(binding.runtime.adapter_id())
        .map_err(|_| ScopedObservationRuntimeSourceError::InvalidBinding)?;
    let mut membership_stream_key =
        Vec::with_capacity(MEMBERSHIP_STREAM_DOMAIN.len() + 8 + binding.runtime.program_id().len());
    membership_stream_key.extend_from_slice(MEMBERSHIP_STREAM_DOMAIN);
    membership_stream_key
        .extend_from_slice(&(binding.runtime.program_id().len() as u64).to_be_bytes());
    membership_stream_key.extend_from_slice(binding.runtime.program_id().as_bytes());
    let stream_key = CoverageStreamKey::derive(adapter_id.as_str(), &membership_stream_key)
        .map_err(|_| ScopedObservationRuntimeSourceError::InvalidBinding)?;
    let object_key = CoverageObjectKey::derive(
        MEMBERSHIP_OBJECT_NAMESPACE,
        binding.object_token().as_bytes(),
    )
    .map_err(|_| ScopedObservationRuntimeSourceError::InvalidBinding)?;
    let source = ScopedSourceObjectIdentity {
        adapter_id,
        source_instance_key: binding.canonical_source_instance_key(),
        stream_key,
        object_key,
    };
    Ok(ScopedObservationDirectoryMembershipProof {
        identity: ScopedObservationDirectoryContractIdentity {
            attachment_authority,
            relation_id: binding.relation_id().to_string(),
            source,
            support_release_digest: *binding.support_release_digest(),
            source_declaration_digest: *binding.source_declaration_digest(),
            scope_program_digest: *binding.scope_program_digest(),
            config,
            selector,
            root_object_token: binding.object_token(),
        },
        authority: binding.runtime.directory_root_authority()?,
        entries: BTreeMap::new(),
        failed: false,
    })
}

impl ScopedObservationAccessPass {
    pub(crate) fn reserve_observation_runtime_source<'pass>(
        &'pass self,
        request: ScopeAccessRequest<'_>,
    ) -> Result<ScopedObservationRuntimeSourceReservation<'pass>, ScopedObservationRuntimeSourceError>
    {
        if self.state.closed.load(Ordering::Acquire) {
            return Err(ScopedObservationRuntimeSourceError::Closed);
        }
        let runtime = self
            .plan
            .reserve_observation_source(request)?
            .bind_runtime_stream(self.adapter.as_ref(), self.source_instance.as_ref())?;
        let Some(approved_root) = self.access_roots.get(runtime.access_root()) else {
            runtime.fail_conservative();
            return Err(ScopedObservationRuntimeSourceError::InvalidBinding);
        };
        let binding = ScopedObservationRuntimeSourceBinding::bind(
            runtime,
            Arc::clone(&self.adapter),
            Arc::clone(&self.source_instance),
            approved_root,
            &self.root_identity.source_instance_key,
        )?;
        if self.state.closed.load(Ordering::Acquire) {
            binding.fail_conservative();
            return Err(ScopedObservationRuntimeSourceError::Closed);
        }
        Ok(ScopedObservationRuntimeSourceReservation {
            _pass: self,
            binding,
        })
    }
}

#[cfg(test)]
impl ScopedObservationDirectoryListing {
    pub(crate) fn from_checkpoint_for_test(
        relation_id: &str,
        source: ScopedSourceObjectIdentity,
        checkpoint: DirectoryCheckpoint,
    ) -> Self {
        let root_object_token = AccessObjectToken::derive(
            "fixture-directory-membership",
            &[source.object_key.as_bytes().as_slice()],
        )
        .expect("the test membership token uses bounded stable identity");
        Self {
            identity: ScopedObservationDirectoryContractIdentity {
                attachment_authority: super::next_scoped_attachment_authority()
                    .expect("the test attachment authority remains available"),
                relation_id: relation_id.to_string(),
                source,
                support_release_digest: [1; 32],
                source_declaration_digest: [2; 32],
                scope_program_digest: [3; 32],
                config: DirectorySnapshotConfig::default(),
                selector: GlobPattern::new("**").expect("the test selector is valid"),
                root_object_token,
            },
            checkpoint,
            changes: Vec::new(),
            root_moved: false,
            accounted_entries: BTreeMap::new(),
            read_authority: None,
            root: PathBuf::new(),
            members: Vec::new(),
            completed_members: Vec::new(),
            next_member_read: 0,
            member_read_failed: false,
        }
    }
}

#[cfg(test)]
pub(crate) fn bind_observation_runtime_source_for_test(
    runtime: AuthorizedObservationRuntimeStreamReservation,
    adapter: Arc<dyn AgentAdapter>,
    instance: Arc<SourceInstance>,
    approved_root: &ScopedAccessRootGrant,
    expected_source_instance_key: &CanonicalSourceInstanceKey,
) -> Result<ScopedObservationRuntimeSourceBinding, ScopedObservationRuntimeSourceError> {
    ScopedObservationRuntimeSourceBinding::bind(
        runtime,
        adapter,
        instance,
        approved_root,
        expected_source_instance_key,
    )
}

#[cfg(test)]
pub(crate) fn prepare_observation_directory_membership_for_test(
    binding: &ScopedObservationRuntimeSourceBinding,
) -> Result<ScopedObservationDirectoryMembershipProof, ScopedObservationRuntimeSourceError> {
    let attachment_authority = super::next_scoped_attachment_authority()
        .map_err(|_| ScopedObservationRuntimeSourceError::InvalidBinding)?;
    prepare_directory_membership_contract(binding, attachment_authority)
}

pub(crate) fn scan_directory_membership_with_authority(
    binding: ScopedObservationRuntimeSourceBinding,
    attachment_authority: Arc<super::ScopedObservationAttachmentAuthority>,
    previous: Option<&ScopedObservationDirectoryListing>,
) -> Result<ScopedObservationDirectoryScan, ScopedObservationRuntimeSourceError> {
    let proof = prepare_directory_membership_contract(&binding, attachment_authority)?;
    ScopedObservationDirectoryScanAuthority { binding, proof }.scan(previous)
}

#[cfg(test)]
pub(crate) fn scan_observation_directory_membership_for_test(
    binding: ScopedObservationRuntimeSourceBinding,
    previous: Option<&ScopedObservationDirectoryListing>,
) -> Result<ScopedObservationDirectoryScan, ScopedObservationRuntimeSourceError> {
    let attachment_authority = previous
        .map(|previous| Arc::clone(&previous.identity.attachment_authority))
        .map(Ok)
        .unwrap_or_else(super::next_scoped_attachment_authority)
        .map_err(|_| ScopedObservationRuntimeSourceError::InvalidBinding)?;
    let proof = prepare_directory_membership_contract(&binding, attachment_authority)?;
    ScopedObservationDirectoryScanAuthority { binding, proof }.scan(previous)
}

#[cfg(test)]
pub(crate) fn scan_observation_directory_membership_with_foreign_attachment_for_test(
    binding: ScopedObservationRuntimeSourceBinding,
    previous: &ScopedObservationDirectoryListing,
) -> Result<ScopedObservationDirectoryScan, ScopedObservationRuntimeSourceError> {
    let attachment_authority = super::next_scoped_attachment_authority()
        .map_err(|_| ScopedObservationRuntimeSourceError::InvalidBinding)?;
    let proof = prepare_directory_membership_contract(&binding, attachment_authority)?;
    ScopedObservationDirectoryScanAuthority { binding, proof }.scan(Some(previous))
}
