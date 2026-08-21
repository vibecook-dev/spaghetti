//! Adapter-neutral declared-object reconciliation for RFC 011.
//!
//! A reconcile is intentionally deterministic and synchronous at this layer:
//! native watcher hints and bounded schedulers can call the same operation
//! without gaining a second framing, decode, or commit path.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs::Metadata;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use walkdir::WalkDir;

use crate::adapter::{
    AdapterError, AdapterErrorClass, AdapterManifest, AdapterObjectContext, AgentAdapter,
    Availability, CanonicalSourceInstanceKey, CapabilityGranularity, CapabilityId, CoverageAbsence,
    CoverageAbsenceKind, CoverageDeclarationDigest, CoverageDomain, CoverageError,
    CoverageObjectKey, CoveragePosition, CoveragePositionKind, CoverageProvenance, CoverageScope,
    CoverageSetCompleteness, CoverageStatus, CoverageStreamKey, DecodeDisposition, DeletionPolicy,
    DependencyRevision, DiscoveryContext, DriverSpec, FactBatch, FactSemanticContext,
    RawRetentionPolicy, SourceAccess, SourceCoveragePoint, SourceCoverageSet, SourceInstance,
    SourceInstanceSpec as AdapterSourceInstanceSpec, SourceListedObject, SourceObjectDescriptor,
    SourceObjectList, SourceObjectListRequest, SourceQuery, SourceRows, SourceSnapshot,
    StreamAuthority, StreamSpec, SupportLevel,
};
use crate::coverage_runtime::{
    derive_coverage_membership_revision, source_membership_prefix, CoverageMembershipObject,
};
use crate::decode_runtime::{
    decode_record as decode_adapter_record, diagnostic_excerpt, DecodeRuntimeLimits,
    DecodeRuntimeRequest,
};
use crate::source::{
    confined_relative_path_key, AccessBudget, AccessBudgetError, AccessBudgetSnapshot,
    AccessObjectToken, AccessOperation, AccessOutcome, AccessPhase, AccessReservation,
    AccessReservationRequest, AppendCheckpoint, AppendDelimitedFile, AppendItem, AppendRead,
    BoundedScheduler, DirectoryCheckpoint, DirectoryEntryKind, DirectoryScan, DirectorySelection,
    DirectorySnapshot, DirtyReason, GlobPattern, KeyValueCheckpoint, KeyValueRead,
    KeyValueSnapshot, MalformedRevisionGuard, MalformedRevisionPolicy, ParseFailureDecision,
    PresenceCheckpoint, PresenceObject, PresenceRead, RecordOrigin, ReplaceCheckpoint,
    ReplaceDocument, ReplaceRead, Revision, ScheduleOutcome, ScheduledWork, ScopeAccessBounds,
    SourceCursor, SourceDriverError, SourceMediaType, SourceRecord, SqliteCheckpoint, SqliteRead,
    SqliteSnapshot, WorkKey,
};

use super::commit::{
    source_object_catalog_id, source_stream_catalog_id, CommitReceipt, ExpectedSourceCursor,
    ObservationCommit, ProjectionReadiness, ProjectionVersionCommit, ProjectionVersionUpdate,
    SourceCapabilitySpec, SourceInstanceSpec, SourceObjectUpdate, SourceRecordError,
    SourceStreamSpec,
};
use super::performance::{SourceDecodeObservation, SourceDecodeOutcome, SourcePerformanceRecorder};
use super::query_pool::{
    QueryCancellationToken, SourceCatalogObject, SourceCatalogSnapshot,
    SourceCoverageReplayBaseline,
};
use super::runtime_semantic_projection::{USAGE_V2_PROJECTION_ID, USAGE_V2_PROJECTION_VERSION};
use super::source_coverage::{DurableCoverageSetPrecondition, DurableCoverageSetUpdate};
use super::{EngineError, SpaghettiEngineCore};

// One default append-driver batch can contain 1,024 records. Built-in
// transcript decoders emit several normalized facts per record, so keep the
// fact envelope bound independently large enough for that byte-bounded input
// transaction without falling back to tiny commits.
const FACT_BATCH_LIMIT: usize = 8_192;
const DIAGNOSTIC_LIMIT: usize = 256;
const DISCOVERY_MAX_DEPTH: usize = 64;
const DISCOVERY_MAX_ENTRIES: usize = 250_000;
const MAX_APPEND_RECORDS_PER_RECONCILE: usize = 4_096;
const MAX_APPEND_RECORDS_PER_COMMIT: usize = 1_024;
const SCHEDULER_CAPACITY: usize = 1_024;
const MAX_OBJECTS_IN_FLIGHT: usize = 16;
const MAX_UNACKED_COMMITS: usize = 256;
const USAGE_V2_REPLAY_PENDING_DETAIL: &str =
    "runtime.usage-v2 explicit replay in progress; replacement coverage not established";
const USAGE_V2_REPLAY_COMMIT_REASON: &str = "projection.runtime.usage-v2.explicit_replay";

struct CommitLane {
    engine: Arc<SpaghettiEngineCore>,
    pending: Option<crossbeam_channel::Receiver<Result<CommitReceipt, EngineError>>>,
    commits: u32,
    last_commit_seq: Option<u64>,
}

impl CommitLane {
    fn new(engine: Arc<SpaghettiEngineCore>) -> Self {
        Self {
            engine,
            pending: None,
            commits: 0,
            last_commit_seq: None,
        }
    }

    fn submit_observation(&mut self, request: ObservationCommit) -> Result<(), EngineError> {
        self.flush()?;
        self.pending = Some(self.engine.submit_observation(request)?);
        Ok(())
    }

    fn submit_facts(
        &mut self,
        request: ObservationCommit,
        batch: FactBatch,
    ) -> Result<(), EngineError> {
        self.flush()?;
        self.pending = Some(self.engine.submit_facts(request, batch)?);
        Ok(())
    }

    fn flush(&mut self) -> Result<(), EngineError> {
        let Some(pending) = self.pending.take() else {
            return Ok(());
        };
        let receipt = recv_commit_receipt(pending)?;
        self.note_receipt(&receipt);
        Ok(())
    }

    fn note_receipt(&mut self, receipt: &CommitReceipt) {
        self.engine.accept_commit_receipt(receipt);
        self.commits = self.commits.saturating_add(1);
        self.last_commit_seq = Some(receipt.commit_seq);
    }

    fn apply_to(&self, outcome: &mut ReconcileOutcome) {
        outcome.commits = outcome.commits.saturating_add(self.commits);
        if self.last_commit_seq.is_some() {
            outcome.last_commit_seq = self.last_commit_seq;
        }
    }
}

fn recv_commit_receipt(
    pending: crossbeam_channel::Receiver<Result<CommitReceipt, EngineError>>,
) -> Result<CommitReceipt, EngineError> {
    pending
        .recv()
        .map_err(|_| EngineError::WorkerUnavailable { worker: "writer" })?
}

#[derive(Debug, Clone)]
pub struct ReconcileRequest {
    pub configured_roots: Vec<PathBuf>,
    pub reason: String,
}

impl ReconcileRequest {
    pub fn manual(configured_roots: Vec<PathBuf>) -> Self {
        Self {
            configured_roots,
            reason: "manual_reconcile".to_string(),
        }
    }
}

/// Requests an explicit, source-instance-scoped replay of every stream that
/// declares one fact-family capability. The current coordinator supports the
/// usage-v2 family; the shape is family-neutral so later projection packs use
/// the same lifecycle instead of an adapter-private reset path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactFamilyReplayRequest {
    pub owner_id: String,
    pub family: String,
    pub version: u32,
    pub reason: String,
    authorization: Option<FactFamilyReplayAuthorization>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FactFamilyReplayAuthorization {
    adapter_id: String,
    canonical_source_instance_key: Vec<u8>,
    content_digest: Vec<u8>,
    coverage_last_commit_seq: u64,
}

impl FactFamilyReplayRequest {
    pub fn usage_v2(reason: impl Into<String>) -> Self {
        Self {
            owner_id: USAGE_V2_PROJECTION_ID.to_string(),
            family: USAGE_V2_PROJECTION_ID.to_string(),
            version: USAGE_V2_PROJECTION_VERSION,
            reason: reason.into(),
            authorization: None,
        }
    }

    pub(crate) fn authorized(
        mut self,
        adapter_id: String,
        canonical_source_instance_key: Vec<u8>,
        content_digest: Vec<u8>,
        coverage_last_commit_seq: u64,
    ) -> Self {
        self.authorization = Some(FactFamilyReplayAuthorization {
            adapter_id,
            canonical_source_instance_key,
            content_digest,
            coverage_last_commit_seq,
        });
        self
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReconcileOutcome {
    pub instances_discovered: u32,
    pub streams_reconciled: u32,
    pub streams_unavailable: u32,
    pub objects_discovered: u32,
    pub objects_registered: u32,
    pub objects_changed: u32,
    pub objects_unchanged: u32,
    pub objects_removed: u32,
    pub records_decoded: u32,
    pub records_quarantined: u32,
    pub(crate) unscoped_records_quarantined: u32,
    pub(crate) capability_records_quarantined: BTreeMap<String, u32>,
    pub retries_required: u32,
    pub incomplete_tail_retries: u32,
    pub dependency_access_attempts: u64,
    pub dependency_access_denials: u64,
    pub dependency_access_abandoned: u64,
    pub dependency_objects_accessed: u64,
    pub dependency_bytes_read: u64,
    pub dependency_rows_read: u64,
    pub dependency_max_depth: u32,
    pub dependency_trace_entries_dropped: u64,
    /// Retry work caused only by the deliberate per-pass record bound. The
    /// supervisor may resume this immediately without treating it like an
    /// unstable or incomplete source.
    pub backlog_remaining: u32,
    pub retry_targets: Vec<ReconcileRetryTarget>,
    pub commits: u32,
    pub last_commit_seq: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ReconcileRetryTarget {
    pub stable_key: Vec<u8>,
    pub stream_key: String,
    pub object_key: Vec<u8>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ProjectionBlockers {
    streams_unavailable: u32,
    records_quarantined: u32,
    retries_required: u32,
    incomplete_tail_retries: u32,
    backlog_remaining: u32,
    dependency_access_denials: u64,
}

impl ProjectionBlockers {
    fn capture_for(outcome: &ReconcileOutcome, capability: &str) -> Self {
        Self {
            streams_unavailable: outcome.streams_unavailable,
            records_quarantined: outcome.unscoped_records_quarantined.saturating_add(
                outcome
                    .capability_records_quarantined
                    .get(capability)
                    .copied()
                    .unwrap_or_default(),
            ),
            retries_required: outcome.retries_required,
            incomplete_tail_retries: outcome.incomplete_tail_retries,
            backlog_remaining: outcome.backlog_remaining,
            dependency_access_denials: outcome.dependency_access_denials,
        }
    }

    fn increases_since(self, previous: Self) -> Vec<&'static str> {
        let mut blockers = Vec::new();
        if self.streams_unavailable > previous.streams_unavailable {
            blockers.push("streams_unavailable");
        }
        if self.records_quarantined > previous.records_quarantined {
            blockers.push("records_quarantined");
        }
        if self.retries_required > previous.retries_required {
            blockers.push("retries_required");
        }
        if self.incomplete_tail_retries > previous.incomplete_tail_retries {
            blockers.push("incomplete_tail_retries");
        }
        if self.backlog_remaining > previous.backlog_remaining {
            blockers.push("backlog_remaining");
        }
        if self.dependency_access_denials > previous.dependency_access_denials {
            blockers.push("dependency_access_denials");
        }
        blockers
    }
}

#[derive(Debug, Default)]
struct ProjectionCoverageAttempt {
    required_streams: BTreeMap<String, CoveragePositionKind>,
    blockers: BTreeMap<String, BTreeSet<&'static str>>,
}

impl ProjectionCoverageAttempt {
    fn require_stream(&mut self, stream: &StreamSpec) {
        self.required_streams.insert(
            stream.id.as_str().to_string(),
            coverage_position_kind(&stream.driver),
        );
    }

    fn note_outcome(
        &mut self,
        stream_key: &str,
        previous: ProjectionBlockers,
        current: ProjectionBlockers,
    ) {
        for blocker in current.increases_since(previous) {
            self.blockers
                .entry(stream_key.to_string())
                .or_default()
                .insert(blocker);
        }
    }

    fn block(&mut self, stream_key: &str, blocker: &'static str) {
        self.blockers
            .entry(stream_key.to_string())
            .or_default()
            .insert(blocker);
    }

    fn has_quarantine(&self) -> bool {
        self.blockers.values().any(|blockers| {
            blockers.contains("records_quarantined") || blockers.contains("durable_quarantine")
        })
    }

    fn detail(&self) -> Option<String> {
        if self.required_streams.is_empty() {
            return Some(
                "adapter declares runtime.usage-v2 but no source stream provides it".to_string(),
            );
        }
        if self.blockers.is_empty() {
            return None;
        }
        let streams = self
            .blockers
            .iter()
            .map(|(stream, blockers)| {
                format!(
                    "{stream}={}",
                    blockers.iter().copied().collect::<Vec<_>>().join("+")
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        Some(format!("runtime.usage-v2 coverage incomplete: {streams}"))
    }
}

#[derive(Debug, Clone)]
struct FactFamilyReplayContext {
    family: String,
    version: u32,
    baseline: BTreeMap<(Vec<u8>, Vec<u8>), (u64, bool)>,
    precondition: DurableCoverageSetPrecondition,
}

impl FactFamilyReplayContext {
    fn from_durable(
        request: &FactFamilyReplayRequest,
        expected_source_instance_id: u64,
        owner_scope_key: &[u8],
        baseline: SourceCoverageReplayBaseline,
    ) -> Result<Self, EngineError> {
        if baseline.source_instance_id != expected_source_instance_id {
            return Err(observation_error(
                "start fact-family replay",
                "coverage baseline belongs to another source instance",
            ));
        }
        if baseline.last_commit_seq == 0
            || !matches!(
                baseline.completeness.as_str(),
                "complete" | "partial" | "unavailable"
            )
        {
            return Err(observation_error(
                "start fact-family replay",
                "coverage baseline has invalid completeness or commit provenance",
            ));
        }
        if let Some(authorization) = &request.authorization {
            if authorization.adapter_id != baseline.adapter_id
                || authorization.canonical_source_instance_key
                    != baseline.canonical_source_instance_key
                || authorization.content_digest != baseline.content_digest
                || authorization.coverage_last_commit_seq != baseline.last_commit_seq
            {
                return Err(EngineError::InvalidConfig(
                    "fact-family replay authorization is stale or belongs to another scope"
                        .to_string(),
                ));
            }
        }
        let mut members = BTreeMap::new();
        for member in baseline.members {
            if members
                .insert(
                    (member.stream_key, member.object_key),
                    (member.generation, member.absent),
                )
                .is_some()
            {
                return Err(observation_error(
                    "start fact-family replay",
                    "coverage baseline repeats one stream/object member",
                ));
            }
        }
        Ok(Self {
            family: request.family.clone(),
            version: request.version,
            baseline: members,
            precondition: DurableCoverageSetPrecondition {
                owner_id: request.owner_id.clone(),
                owner_scope_key: owner_scope_key.to_vec(),
                family: request.family.clone(),
                family_version: request.version,
                adapter_id: baseline.adapter_id,
                canonical_source_instance_key: baseline.canonical_source_instance_key,
                expected_content_digest: baseline.content_digest,
                expected_last_commit_seq: baseline.last_commit_seq,
            },
        })
    }

    fn stream_is_provider(&self, stream: &StreamSpec) -> bool {
        stream
            .capabilities
            .iter()
            .any(|capability| capability.as_str() == self.family)
    }

    fn object_identity(
        &self,
        manifest: &AdapterManifest,
        stream_key: &str,
        object_key: &[u8],
    ) -> Result<(Vec<u8>, Vec<u8>), EngineError> {
        let stream = CoverageStreamKey::derive(manifest.id.as_str(), stream_key.as_bytes())
            .map_err(|error| {
                observation_error("match replay stream coverage", error.to_string())
            })?;
        let object = CoverageObjectKey::derive(stream_key, object_key).map_err(|error| {
            observation_error("match replay object coverage", error.to_string())
        })?;
        Ok((stream.as_bytes().to_vec(), object.as_bytes().to_vec()))
    }

    fn force_generation_replay(
        &self,
        manifest: &AdapterManifest,
        stream: &StreamSpec,
        previous: Option<&SourceCatalogObject>,
    ) -> Result<bool, EngineError> {
        if !self.stream_is_provider(stream) {
            return Ok(false);
        }
        let Some(previous) = previous else {
            // An object created after the durable baseline is already read
            // from its beginning in a fresh generation.
            return Ok(false);
        };
        let identity =
            self.object_identity(manifest, previous.stream_key.as_str(), &previous.object_key)?;
        let Some((baseline_generation, baseline_absent)) = self.baseline.get(&identity).copied()
        else {
            // New membership is likewise ingested from its beginning.
            return Ok(false);
        };
        if previous.generation < baseline_generation {
            return Err(observation_error(
                "resume fact-family replay",
                "source generation moved behind its durable coverage baseline",
            ));
        }
        Ok(previous.generation == baseline_generation
            && !(baseline_absent && previous.state == "absent"))
    }

    fn verify_replacement(
        &self,
        manifest: &AdapterManifest,
        catalog: &SourceCatalogSnapshot,
        coverage: &mut ProjectionCoverageAttempt,
    ) -> Result<(), EngineError> {
        let mut current = BTreeMap::new();
        for object in catalog
            .objects
            .iter()
            .filter(|object| coverage.required_streams.contains_key(&object.stream_key))
        {
            let identity =
                self.object_identity(manifest, &object.stream_key, &object.object_key)?;
            current.insert(identity, (object.generation, object.state.as_str()));
        }
        for (identity, (baseline_generation, baseline_absent)) in &self.baseline {
            let Some((generation, state)) = current.get(identity).copied() else {
                coverage.block("replay-baseline", "replay_member_missing");
                continue;
            };
            if generation < *baseline_generation {
                return Err(observation_error(
                    "verify fact-family replay",
                    "source generation moved behind its durable coverage baseline",
                ));
            }
            let unchanged_absence =
                *baseline_absent && state == "absent" && generation == *baseline_generation;
            if generation == *baseline_generation && !unchanged_absence {
                coverage.block("replay-baseline", "replay_generation_not_advanced");
            }
        }
        Ok(())
    }
}

pub struct ObservationCoordinator {
    engine: Arc<SpaghettiEngineCore>,
    cancellations: Vec<QueryCancellationToken>,
    max_append_records_per_reconcile: usize,
}

#[derive(Debug, Clone)]
struct TrackedDependency {
    revision: DependencyRevision,
    root: PathBuf,
    kind: TrackedDependencyKind,
}

#[derive(Debug, Clone)]
enum TrackedDependencyKind {
    Object {
        relative_path: PathBuf,
        max_bytes: usize,
    },
    Query(SourceQuery),
    Listing(SourceObjectListRequest),
}

struct ConfinedSourceAccess<'a> {
    instance: &'a SourceInstance,
    cancellations: &'a [QueryCancellationToken],
    reads: Mutex<Vec<TrackedDependency>>,
    budget: AccessBudget,
}

impl<'a> ConfinedSourceAccess<'a> {
    const MAX_READ_BYTES: usize = 64 * 1024 * 1024;
    const MAX_QUERY_ROWS: usize = 16_384;
    const MAX_LISTING_ENTRIES: usize = 10_000;
    const MAX_LISTING_ACCOUNTED_BYTES_PER_OBJECT: usize = 16 * 1024;
    const MAX_LISTING_RETURN_BYTES: usize =
        Self::MAX_LISTING_ENTRIES * Self::MAX_LISTING_ACCOUNTED_BYTES_PER_OBJECT;
    const ACCESS_RELATION_ID: &'static str = "adapter-dependency";

    fn new(instance: &'a SourceInstance, cancellations: &'a [QueryCancellationToken]) -> Self {
        Self {
            instance,
            cancellations,
            reads: Mutex::new(Vec::new()),
            budget: AccessBudget::new(
                Self::ACCESS_RELATION_ID,
                ScopeAccessBounds {
                    max_fan_out: Self::MAX_LISTING_ENTRIES as u64,
                    max_depth: 1,
                    max_objects: Self::MAX_LISTING_ENTRIES as u64,
                    max_bytes: (2 * Self::MAX_LISTING_RETURN_BYTES) as u64,
                    max_rows: (2 * Self::MAX_QUERY_ROWS) as u64,
                },
            )
            .expect("fixed adapter-dependency access bounds must be valid"),
        }
    }

    fn access_snapshot(&self) -> AccessBudgetSnapshot {
        self.budget.snapshot()
    }

    fn reserve_access(
        &self,
        operation: AccessOperation,
        phase: AccessPhase,
        root_name: &str,
        object_key: &[u8],
        max_bytes: usize,
        max_rows: usize,
    ) -> Result<AccessReservation, AccessBudgetError> {
        let operation_tag = match operation {
            AccessOperation::ObjectRead => b"object".as_slice(),
            AccessOperation::ParameterizedQuery => b"query".as_slice(),
            AccessOperation::ObjectListing => b"listing".as_slice(),
        };
        let object_token = AccessObjectToken::derive(
            self.budget.relation_id(),
            &[operation_tag, root_name.as_bytes(), object_key],
        )?;
        self.budget.reserve(AccessReservationRequest {
            operation,
            phase,
            parent_token: None,
            object_token,
            depth: 1,
            max_bytes: u64::try_from(max_bytes).unwrap_or(u64::MAX),
            max_rows: u64::try_from(max_rows).unwrap_or(u64::MAX),
        })
    }

    fn revisions(&self) -> Result<Vec<DependencyRevision>, EngineError> {
        self.reads
            .lock()
            .map(|reads| reads.iter().map(|read| read.revision.clone()).collect())
            .map_err(|_| observation_error("read source dependencies", "dependency lock poisoned"))
    }

    fn changed_since_read(&self) -> Result<bool, EngineError> {
        let reads = self
            .reads
            .lock()
            .map_err(|_| {
                observation_error("validate source dependencies", "dependency lock poisoned")
            })?
            .clone();
        for read in reads {
            check_cancellations(self.cancellations)?;
            let (operation, max_bytes, max_rows) = match &read.kind {
                TrackedDependencyKind::Object { max_bytes, .. } => {
                    (AccessOperation::ObjectRead, *max_bytes, 0)
                }
                TrackedDependencyKind::Query(query) => (
                    AccessOperation::ParameterizedQuery,
                    query.bounds.max_snapshot_bytes,
                    query.bounds.max_rows,
                ),
                TrackedDependencyKind::Listing(request) => (
                    AccessOperation::ObjectListing,
                    listing_reservation_bytes(request.max_entries),
                    request.max_entries,
                ),
            };
            let reservation = self
                .reserve_access(
                    operation,
                    AccessPhase::Revalidation,
                    &read.revision.root_name,
                    &read.revision.object_key,
                    max_bytes,
                    max_rows,
                )
                .map_err(access_budget_engine_error)?;
            let current = match &read.kind {
                TrackedDependencyKind::Object {
                    relative_path,
                    max_bytes,
                } => dependency_snapshot(
                    self.instance.id,
                    &read.revision.root_name,
                    &read.root,
                    relative_path,
                    *max_bytes,
                )
                .map(|snapshot| {
                    let measurement = snapshot_access_measurement(&snapshot);
                    (snapshot.revision, measurement)
                }),
                TrackedDependencyKind::Query(query) => {
                    source_query_snapshot(self.instance.id, &read.root, query, self.cancellations)
                        .map(|rows| {
                            let measurement = rows_access_measurement(&rows);
                            (rows.revision, measurement)
                        })
                }
                TrackedDependencyKind::Listing(request) => {
                    source_object_listing(self.instance.id, &read.root, request, self.cancellations)
                        .map(|listing| {
                            let measurement = listing_access_measurement(&listing);
                            (listing.revision, measurement)
                        })
                }
            };
            let (current, measurement) = match current {
                Ok(value) => value,
                Err(SourceDriverError::Unstable(_)) => {
                    reservation.fail_conservative();
                    return Ok(true);
                }
                Err(error) => {
                    reservation.fail_conservative();
                    return Err(source_error(error));
                }
            };
            reservation
                .complete(measurement.bytes, measurement.rows, measurement.outcome)
                .map_err(access_budget_engine_error)?;
            if current != read.revision {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn track(&self, tracked: TrackedDependency) -> Result<(), AdapterError> {
        let mut reads = self.reads.lock().map_err(|_| {
            AdapterError::new(
                AdapterErrorClass::AdapterFatal,
                "source_access_lock",
                "source dependency lock was poisoned",
            )
        })?;
        if let Some(existing) = reads.iter_mut().find(|read| {
            read.revision.root_name == tracked.revision.root_name
                && read.revision.object_key == tracked.revision.object_key
        }) {
            *existing = tracked;
        } else {
            reads.push(tracked);
        }
        Ok(())
    }
}

impl SourceAccess for ConfinedSourceAccess<'_> {
    fn read_object(
        &self,
        root_name: &str,
        relative_path: &Path,
        max_bytes: usize,
    ) -> Result<SourceSnapshot, AdapterError> {
        check_cancellations(self.cancellations).map_err(|_| {
            AdapterError::new(
                AdapterErrorClass::Transient,
                "source_access_cancelled",
                "source dependency read was cancelled",
            )
        })?;
        if max_bytes == 0 || max_bytes > Self::MAX_READ_BYTES {
            return Err(AdapterError::invalid_contract(format!(
                "source dependency byte limit must be between 1 and {}",
                Self::MAX_READ_BYTES
            )));
        }
        let root = self.instance.root(root_name)?.to_path_buf();
        let object_key = confined_relative_path_key(relative_path).map_err(source_access_error)?;
        let reservation = self
            .reserve_access(
                AccessOperation::ObjectRead,
                AccessPhase::Initial,
                root_name,
                &object_key,
                max_bytes,
                0,
            )
            .map_err(source_access_budget_error)?;
        let snapshot =
            match dependency_snapshot(self.instance.id, root_name, &root, relative_path, max_bytes)
            {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    reservation.fail_conservative();
                    return Err(source_access_error(error));
                }
            };
        let measurement = snapshot_access_measurement(&snapshot);
        reservation
            .complete(measurement.bytes, measurement.rows, measurement.outcome)
            .map_err(source_access_budget_error)?;
        let tracked = TrackedDependency {
            revision: snapshot.revision.clone(),
            root,
            kind: TrackedDependencyKind::Object {
                relative_path: relative_path.to_path_buf(),
                max_bytes,
            },
        };
        self.track(tracked)?;
        Ok(snapshot)
    }

    fn query_source_db(&self, query: &SourceQuery) -> Result<SourceRows, AdapterError> {
        check_cancellations(self.cancellations).map_err(|_| source_access_cancelled())?;
        validate_source_query_bounds(query)?;
        let root = self.instance.root(&query.root_name)?.to_path_buf();
        let object_key = source_query_dependency_key(query).map_err(source_access_error)?;
        let reservation = self
            .reserve_access(
                AccessOperation::ParameterizedQuery,
                AccessPhase::Initial,
                &query.root_name,
                &object_key,
                query.bounds.max_snapshot_bytes,
                query.bounds.max_rows,
            )
            .map_err(source_access_budget_error)?;
        let rows = match source_query_snapshot(self.instance.id, &root, query, self.cancellations) {
            Ok(rows) => rows,
            Err(error) => {
                reservation.fail_conservative();
                return Err(source_access_error(error));
            }
        };
        let measurement = rows_access_measurement(&rows);
        reservation
            .complete(measurement.bytes, measurement.rows, measurement.outcome)
            .map_err(source_access_budget_error)?;
        self.track(TrackedDependency {
            revision: rows.revision.clone(),
            root,
            kind: TrackedDependencyKind::Query(query.clone()),
        })?;
        Ok(rows)
    }

    fn list_objects(
        &self,
        request: &SourceObjectListRequest,
    ) -> Result<SourceObjectList, AdapterError> {
        check_cancellations(self.cancellations).map_err(|_| source_access_cancelled())?;
        if request.max_entries == 0
            || request.max_entries > Self::MAX_LISTING_ENTRIES
            || request.include.is_empty()
        {
            return Err(AdapterError::invalid_contract(
                "source object listing requires include patterns and a 1..=10000 entry bound",
            ));
        }
        let root = self.instance.root(&request.root_name)?.to_path_buf();
        let object_key = source_listing_dependency_key(request).map_err(source_access_error)?;
        let reservation = self
            .reserve_access(
                AccessOperation::ObjectListing,
                AccessPhase::Initial,
                &request.root_name,
                &object_key,
                listing_reservation_bytes(request.max_entries),
                request.max_entries,
            )
            .map_err(source_access_budget_error)?;
        let listing =
            match source_object_listing(self.instance.id, &root, request, self.cancellations) {
                Ok(listing) => listing,
                Err(error) => {
                    reservation.fail_conservative();
                    return Err(source_access_error(error));
                }
            };
        let measurement = listing_access_measurement(&listing);
        reservation
            .complete(measurement.bytes, measurement.rows, measurement.outcome)
            .map_err(source_access_budget_error)?;
        self.track(TrackedDependency {
            revision: listing.revision.clone(),
            root,
            kind: TrackedDependencyKind::Listing(request.clone()),
        })?;
        Ok(listing)
    }
}

#[derive(Debug, Clone, Copy)]
struct AccessMeasurement {
    bytes: u64,
    rows: u64,
    outcome: AccessOutcome,
}

fn snapshot_access_measurement(snapshot: &SourceSnapshot) -> AccessMeasurement {
    AccessMeasurement {
        bytes: snapshot
            .payload
            .as_ref()
            .map_or(0, |payload| payload.len() as u64),
        rows: 0,
        outcome: if snapshot.oversized {
            AccessOutcome::Oversized
        } else if snapshot.payload.is_some() {
            AccessOutcome::Available
        } else {
            AccessOutcome::Unavailable
        },
    }
}

fn rows_access_measurement(rows: &SourceRows) -> AccessMeasurement {
    AccessMeasurement {
        bytes: rows.rows.iter().fold(0_u64, |total, row| {
            total.saturating_add(row.encode().len() as u64)
        }),
        rows: rows.rows.len() as u64,
        outcome: if rows.available {
            AccessOutcome::Available
        } else {
            AccessOutcome::Unavailable
        },
    }
}

fn listing_access_measurement(listing: &SourceObjectList) -> AccessMeasurement {
    AccessMeasurement {
        bytes: listing.objects.iter().fold(0_u64, |total, object| {
            // The binary-safe object key encodes the relative path. Account a
            // second copy plus fixed metadata because the returned structure
            // carries both the path and key alongside size/mtime fields.
            total.saturating_add(
                (object.object_key.len() as u64)
                    .saturating_mul(2)
                    .saturating_add(24),
            )
        }),
        rows: listing.objects.len() as u64,
        outcome: if listing.available {
            AccessOutcome::Available
        } else {
            AccessOutcome::Unavailable
        },
    }
}

fn listing_reservation_bytes(max_entries: usize) -> usize {
    max_entries.saturating_mul(ConfinedSourceAccess::MAX_LISTING_ACCOUNTED_BYTES_PER_OBJECT)
}

fn source_access_budget_error(error: AccessBudgetError) -> AdapterError {
    AdapterError::new(
        AdapterErrorClass::InvalidContract,
        "source_access_budget",
        error.to_string(),
    )
}

fn access_budget_engine_error(error: AccessBudgetError) -> EngineError {
    observation_error("enforce source access budget", error.to_string())
}

impl ObservationCoordinator {
    pub fn new(engine: Arc<SpaghettiEngineCore>) -> Self {
        Self {
            engine,
            cancellations: Vec::new(),
            max_append_records_per_reconcile: MAX_APPEND_RECORDS_PER_RECONCILE,
        }
    }

    pub fn with_cancellation(
        engine: Arc<SpaghettiEngineCore>,
        cancellation: QueryCancellationToken,
    ) -> Self {
        Self {
            engine,
            cancellations: vec![cancellation],
            max_append_records_per_reconcile: MAX_APPEND_RECORDS_PER_RECONCILE,
        }
    }

    pub(crate) fn with_cancellations(
        engine: Arc<SpaghettiEngineCore>,
        cancellations: Vec<QueryCancellationToken>,
    ) -> Self {
        Self {
            engine,
            cancellations,
            max_append_records_per_reconcile: MAX_APPEND_RECORDS_PER_RECONCILE,
        }
    }

    #[cfg(test)]
    fn with_append_record_limit(engine: Arc<SpaghettiEngineCore>, limit: usize) -> Self {
        assert!(limit > 0 && limit <= MAX_APPEND_RECORDS_PER_RECONCILE);
        Self {
            engine,
            cancellations: Vec::new(),
            max_append_records_per_reconcile: limit,
        }
    }

    pub fn reconcile<A: AgentAdapter + ?Sized>(
        &self,
        adapter: &A,
        request: ReconcileRequest,
    ) -> Result<ReconcileOutcome, EngineError> {
        self.check_cancelled()?;
        validate_request(&request)?;
        let manifest = adapter.manifest();
        manifest
            .validate()
            .map_err(|error| adapter_error("validate adapter manifest", error))?;
        let started_at = now_unix_ms()?;
        let lease = self
            .engine
            .begin_full_reconcile(manifest.id.as_str(), started_at)?;
        let result = (|| {
            self.check_cancelled()?;
            let instances = catch_adapter_panic("discover source instances", || {
                adapter.discover(&DiscoveryContext {
                    configured_roots: request.configured_roots,
                    observed_at: started_at,
                })
            })?
            .map_err(|error| adapter_error("discover source instances", error))?;
            self.check_cancelled()?;
            let mut outcome = ReconcileOutcome {
                instances_discovered: bounded_u32(instances.len()),
                ..ReconcileOutcome::default()
            };
            let mut instance_keys = BTreeSet::new();
            lease.begin_reconciling();
            for spec in instances {
                self.check_cancelled()?;
                if !instance_keys.insert(spec.stable_key.as_bytes().to_vec()) {
                    return Err(observation_error(
                        "validate discovered source instances",
                        "adapter discovered the same stable instance key more than once",
                    ));
                }
                self.reconcile_instance(
                    adapter,
                    spec,
                    &request.reason,
                    started_at,
                    &mut outcome,
                    None,
                )?;
            }
            Ok(outcome)
        })();
        self.finish_reconcile(lease, result, started_at)
    }

    /// Reconcile one already-discovered source instance. Watcher and polling
    /// schedulers use this narrower entry point so one dirty instance does not
    /// force discovery and scanning of every configured root.
    pub fn reconcile_declared_instance<A: AgentAdapter + ?Sized>(
        &self,
        adapter: &A,
        spec: AdapterSourceInstanceSpec,
        reason: impl Into<String>,
    ) -> Result<ReconcileOutcome, EngineError> {
        self.check_cancelled()?;
        let manifest = adapter.manifest();
        manifest
            .validate()
            .map_err(|error| adapter_error("validate adapter manifest", error))?;
        let reason = reason.into();
        if reason.trim().is_empty() {
            return Err(EngineError::InvalidConfig(
                "instance reconcile requires a reason".to_string(),
            ));
        }
        let started_at = now_unix_ms()?;
        let lease = self.engine.begin_instance_reconcile(
            manifest.id.as_str(),
            spec.stable_key.as_bytes(),
            started_at,
        )?;
        let result = (|| {
            self.check_cancelled()?;
            let mut outcome = ReconcileOutcome {
                instances_discovered: 1,
                ..ReconcileOutcome::default()
            };
            self.reconcile_instance(adapter, spec, &reason, started_at, &mut outcome, None)?;
            Ok(outcome)
        })();
        self.finish_reconcile(lease, result, started_at)
    }

    /// Replay one fact family's declared provider streams for an already
    /// discovered source instance. The durable coverage set is the baseline:
    /// every present baseline object must enter a later generation before the
    /// replacement barrier may become Ready.
    pub(crate) fn replay_discovered_fact_family<A: AgentAdapter + ?Sized>(
        &self,
        adapter: &A,
        configured_roots: Vec<PathBuf>,
        target_stable_key: &[u8],
        request: FactFamilyReplayRequest,
    ) -> Result<ReconcileOutcome, EngineError> {
        self.check_cancelled()?;
        validate_request(&ReconcileRequest {
            configured_roots: configured_roots.clone(),
            reason: request.reason.clone(),
        })?;
        validate_fact_family_replay_request(&request)?;
        let manifest = adapter.manifest();
        manifest
            .validate()
            .map_err(|error| adapter_error("validate adapter manifest", error))?;
        let observed_at = now_unix_ms()?;
        let instances = catch_adapter_panic("discover replay source instance", || {
            adapter.discover(&DiscoveryContext {
                configured_roots,
                observed_at,
            })
        })?
        .map_err(|error| adapter_error("discover replay source instance", error))?;
        let mut instance_keys = BTreeSet::new();
        let mut selected = None;
        for spec in instances {
            let key = spec.stable_key.as_bytes().to_vec();
            if !instance_keys.insert(key.clone()) {
                return Err(observation_error(
                    "validate replay source discovery",
                    "adapter discovered the same stable instance key more than once",
                ));
            }
            if key == target_stable_key {
                selected = Some(spec);
            }
        }
        let selected = selected.ok_or_else(|| {
            EngineError::InvalidConfig(
                "configured roots do not resolve the authorized replay source instance".to_string(),
            )
        })?;
        self.replay_declared_instance_fact_family(adapter, selected, request)
    }

    pub fn replay_declared_instance_fact_family<A: AgentAdapter + ?Sized>(
        &self,
        adapter: &A,
        spec: AdapterSourceInstanceSpec,
        request: FactFamilyReplayRequest,
    ) -> Result<ReconcileOutcome, EngineError> {
        self.check_cancelled()?;
        validate_fact_family_replay_request(&request)?;
        let manifest = adapter.manifest();
        manifest
            .validate()
            .map_err(|error| adapter_error("validate adapter manifest", error))?;
        spec.validate()
            .map_err(|error| adapter_error("validate source instance identity", error))?;
        if request.owner_id != USAGE_V2_PROJECTION_ID
            || request.family != USAGE_V2_PROJECTION_ID
            || request.version != USAGE_V2_PROJECTION_VERSION
        {
            return Err(EngineError::InvalidConfig(format!(
                "fact-family replay is not implemented for owner {} family {} version {}",
                request.owner_id, request.family, request.version
            )));
        }
        if !declares_usage_v2_projection(manifest) {
            return Err(EngineError::InvalidConfig(
                "adapter does not declare supported runtime.usage-v2 evidence".to_string(),
            ));
        }

        let started_at = now_unix_ms()?;
        let lease = self.engine.begin_instance_reconcile(
            manifest.id.as_str(),
            spec.stable_key.as_bytes(),
            started_at,
        )?;
        let result = (|| {
            let catalog = self
                .engine
                .source_catalog(manifest.id.as_str(), spec.stable_key.as_bytes())?;
            let source_instance_id = catalog.source_instance_id.ok_or_else(|| {
                observation_error(
                    "start fact-family replay",
                    "source instance has no durable coverage baseline",
                )
            })?;
            let replay = self.load_fact_family_replay_context(
                source_instance_id,
                spec.stable_key.as_bytes(),
                &request,
            )?;
            let committed_at = now_unix_ms()?.max(started_at);
            let marker_commit =
                self.engine
                    .commit_projection_versions(ProjectionVersionCommit {
                        source_instance_id,
                        reason: request.reason.clone(),
                        started_at,
                        committed_at,
                        projection_versions: vec![usage_v2_projection_update(
                            &SourceInstance {
                                id: source_instance_id,
                                spec: spec.clone(),
                            },
                            ProjectionReadiness::Pending,
                            Some(USAGE_V2_REPLAY_PENDING_DETAIL),
                        )],
                        coverage_sets: Vec::new(),
                        coverage_preconditions: vec![replay.precondition.clone()],
                        query_pack_selections: Vec::new(),
                    })?;
            let mut outcome = ReconcileOutcome {
                instances_discovered: 1,
                ..ReconcileOutcome::default()
            };
            if let Some(commit_seq) = marker_commit {
                outcome.commits = outcome.commits.saturating_add(1);
                outcome.last_commit_seq = Some(commit_seq);
            }
            lease.begin_reconciling();
            self.reconcile_instance(
                adapter,
                spec,
                USAGE_V2_REPLAY_COMMIT_REASON,
                started_at,
                &mut outcome,
                Some(replay),
            )?;
            Ok(outcome)
        })();
        self.finish_reconcile(lease, result, started_at)
    }

    pub(crate) fn reconcile_declared_object<A: AgentAdapter + ?Sized>(
        &self,
        adapter: &A,
        spec: AdapterSourceInstanceSpec,
        target: &ReconcileRetryTarget,
        reason: impl Into<String>,
    ) -> Result<ReconcileOutcome, EngineError> {
        self.check_cancelled()?;
        let manifest = adapter.manifest();
        manifest
            .validate()
            .map_err(|error| adapter_error("validate adapter manifest", error))?;
        if spec.stable_key.as_bytes() != target.stable_key {
            return Err(EngineError::InvalidConfig(
                "retry target does not belong to the declared source instance".to_string(),
            ));
        }
        let reason = reason.into();
        if reason.trim().is_empty() {
            return Err(EngineError::InvalidConfig(
                "object reconcile requires a reason".to_string(),
            ));
        }
        let started_at = now_unix_ms()?;
        let lease = self.engine.begin_object_reconcile(
            manifest.id.as_str(),
            spec.stable_key.as_bytes(),
            &target.stream_key,
            &target.object_key,
            started_at,
        )?;
        let result = (|| {
            let catalog = self
                .engine
                .source_catalog(manifest.id.as_str(), spec.stable_key.as_bytes())?;
            let source_instance_id = catalog.source_instance_id.ok_or_else(|| {
                observation_error("resume retry object", "source instance is not registered")
            })?;
            let instance = SourceInstance {
                id: source_instance_id,
                spec,
            };
            let streams =
                catch_adapter_panic("declare retry source stream", || adapter.streams(&instance))?
                    .map_err(|error| adapter_error("declare retry source stream", error))?;
            let mut usage_v2_coverage = ProjectionCoverageAttempt::default();
            if declares_usage_v2_projection(manifest) {
                for candidate in &streams {
                    if stream_declares_usage_v2_projection(candidate) {
                        usage_v2_coverage.require_stream(candidate);
                    }
                }
            }
            let stream = streams
                .into_iter()
                .find(|stream| stream.id.as_str() == target.stream_key)
                .ok_or_else(|| {
                    observation_error(
                        "resume retry object",
                        format!("adapter no longer declares stream {}", target.stream_key),
                    )
                })?;
            stream
                .validate(&instance)
                .map_err(|error| adapter_error("validate retry source stream", error))?;
            if matches!(stream.driver, DriverSpec::DirectorySnapshot(_)) {
                return Err(observation_error(
                    "resume retry object",
                    "directory membership retries require an instance reconcile",
                ));
            }
            let previous = catalog.objects.iter().find(|object| {
                object.stream_key == target.stream_key && object.object_key == target.object_key
            });
            let Some(previous) = previous else {
                // A native backend can report a newly created file as a
                // content modification. The object route has no durable
                // display path yet, so escalate once to membership discovery
                // instead of turning that normal race into a recovery loop.
                self.engine.mark_observation_instance_dirty(
                    manifest.id.as_str(),
                    instance.spec.stable_key.as_bytes(),
                    crate::source::DirtyReason::IdentityChanged,
                )?;
                return Ok(ReconcileOutcome {
                    instances_discovered: 1,
                    ..ReconcileOutcome::default()
                });
            };
            let root = instance
                .root(&stream.selector.root_name)
                .map_err(|error| adapter_error("resolve retry source root", error))?;
            let relative_path = previous_display_path(previous)?;
            let object = DeclaredObject {
                path: root.join(&relative_path),
                descriptor: SourceObjectDescriptor {
                    stream_id: stream.id.clone(),
                    object_key: target.object_key.clone(),
                    relative_path,
                },
                metadata: confined_metadata(root, previous.display_path.as_deref()),
            };
            let mut outcome = ReconcileOutcome {
                instances_discovered: 1,
                streams_reconciled: 1,
                objects_discovered: 1,
                ..ReconcileOutcome::default()
            };
            let usage_v2_stream = declares_usage_v2_projection(manifest)
                && stream_declares_usage_v2_projection(&stream);
            let replay = usage_v2_stream
                .then(|| self.resume_fact_family_replay(&instance, &catalog))
                .transpose()?
                .flatten();
            lease.begin_reconciling();
            let mut lane = CommitLane::new(Arc::clone(&self.engine));
            self.reconcile_object(
                adapter,
                &instance,
                &stream,
                &object,
                Some(previous),
                false,
                &reason,
                started_at,
                &mut outcome,
                &mut lane,
            )?;
            lane.flush()?;
            lane.apply_to(&mut outcome);
            if usage_v2_stream {
                usage_v2_coverage.note_outcome(
                    stream.id.as_str(),
                    ProjectionBlockers::default(),
                    ProjectionBlockers::capture_for(&outcome, USAGE_V2_PROJECTION_ID),
                );
                // A provider commit publishes Pending in the same source
                // transaction. Object-scoped watcher and recovery work must
                // close that transition with the same instance-wide coverage
                // barrier as a full reconcile; otherwise a successfully
                // drained retry leaves truthful data behind stale Pending.
                self.finish_instance_projection_readiness(
                    manifest,
                    &instance,
                    &catalog,
                    started_at,
                    usage_v2_coverage,
                    &mut outcome,
                    replay.as_ref(),
                )?;
            }
            Ok(outcome)
        })();
        self.finish_reconcile(lease, result, started_at)
    }

    fn load_fact_family_replay_context(
        &self,
        source_instance_id: u64,
        scope_key: &[u8],
        request: &FactFamilyReplayRequest,
    ) -> Result<FactFamilyReplayContext, EngineError> {
        let baseline = self
            .engine
            .source_coverage_replay_baseline(
                source_instance_id,
                &request.owner_id,
                scope_key,
                &request.family,
                request.version,
            )?
            .ok_or_else(|| {
                observation_error(
                    "start fact-family replay",
                    "no durable source-coverage baseline exists for the requested family",
                )
            })?;
        FactFamilyReplayContext::from_durable(request, source_instance_id, scope_key, baseline)
    }

    fn resume_fact_family_replay(
        &self,
        instance: &SourceInstance,
        catalog: &SourceCatalogSnapshot,
    ) -> Result<Option<FactFamilyReplayContext>, EngineError> {
        let active = catalog.projection_versions.iter().find(|projection| {
            projection.projection_id == USAGE_V2_PROJECTION_ID
                && projection.readiness == "pending"
                && projection.detail.as_deref() == Some(USAGE_V2_REPLAY_PENDING_DETAIL)
        });
        let Some(active) = active else {
            return Ok(None);
        };
        if active.desired_version != USAGE_V2_PROJECTION_VERSION {
            return Err(observation_error(
                "resume fact-family replay",
                "active replay desired version no longer matches the engine contract",
            ));
        }
        self.load_fact_family_replay_context(
            instance.id,
            instance.spec.stable_key.as_bytes(),
            &FactFamilyReplayRequest::usage_v2("resume durable explicit replay"),
        )
        .map(Some)
    }

    fn check_cancelled(&self) -> Result<(), EngineError> {
        check_cancellations(&self.cancellations)
    }

    fn source_performance<A: AgentAdapter + ?Sized>(
        &self,
        adapter: &A,
        stream: &StreamSpec,
    ) -> SourcePerformanceRecorder {
        self.engine.source_performance_recorder(
            adapter.manifest().id.as_str(),
            stream.id.as_str(),
            stream.driver.kind(),
        )
    }

    fn finish_reconcile(
        &self,
        lease: super::ObservationLease,
        result: Result<ReconcileOutcome, EngineError>,
        started_at: i64,
    ) -> Result<ReconcileOutcome, EngineError> {
        let finished_at = now_unix_ms().unwrap_or(started_at);
        match result {
            Ok(outcome) => {
                lease.complete(&outcome, self.engine.latest_commit_seq(), finished_at);
                Ok(outcome)
            }
            Err(error) => {
                lease.fail(&error, finished_at);
                Err(error)
            }
        }
    }

    fn reconcile_instance<A: AgentAdapter + ?Sized>(
        &self,
        adapter: &A,
        spec: AdapterSourceInstanceSpec,
        reason: &str,
        started_at: i64,
        outcome: &mut ReconcileOutcome,
        requested_replay: Option<FactFamilyReplayContext>,
    ) -> Result<(), EngineError> {
        self.check_cancelled()?;
        spec.validate()
            .map_err(|error| adapter_error("validate source instance identity", error))?;
        let manifest = adapter.manifest();
        let catalog = self
            .engine
            .source_catalog(manifest.id.as_str(), spec.stable_key.as_bytes())?;
        // Entity keys emitted by adapters include the durable source-instance
        // ID. Reserve it before first decode or when the adapter contract
        // changes. An ordinary unchanged poll reuses the catalog identity so it
        // does not create an otherwise empty SQLite write transaction.
        let source_instance_id =
            match (catalog.source_instance_id, catalog.adapter_contract_version) {
                (Some(source_instance_id), Some(contract_version))
                    if contract_version == manifest.contract_version =>
                {
                    source_instance_id
                }
                _ => self.reserve_instance(adapter, &spec, started_at)?,
            };
        if catalog
            .source_instance_id
            .is_some_and(|catalog_id| catalog_id != source_instance_id)
        {
            return Err(observation_error(
                "reserve source instance",
                "catalog identity changed during reconcile",
            ));
        }
        let instance = SourceInstance {
            id: source_instance_id,
            spec,
        };
        let replay = match requested_replay {
            Some(replay) => Some(replay),
            None => self.resume_fact_family_replay(&instance, &catalog)?,
        };
        let reason = if replay.is_some() {
            USAGE_V2_REPLAY_COMMIT_REASON
        } else {
            reason
        };
        let mut usage_v2_coverage = ProjectionCoverageAttempt::default();
        let mut discovery = DiscoveryIndex::default();
        let streams = catch_adapter_panic("declare source streams", || adapter.streams(&instance))?
            .map_err(|error| adapter_error("declare source streams", error))?;
        discovery.preload(&instance, &streams, &self.cancellations)?;
        let mut stream_ids = BTreeSet::new();
        let mut scheduled_objects = Vec::new();
        for stream in streams {
            self.check_cancelled()?;
            stream
                .validate(&instance)
                .map_err(|error| adapter_error("validate source stream", error))?;
            if !stream_ids.insert(stream.id.as_str().to_string()) {
                return Err(observation_error(
                    "validate source streams",
                    format!("adapter declared stream {} more than once", stream.id),
                ));
            }
            let usage_v2_stream = declares_usage_v2_projection(manifest)
                && stream_declares_usage_v2_projection(&stream);
            if usage_v2_stream {
                usage_v2_coverage.require_stream(&stream);
            }
            let projection_blockers =
                ProjectionBlockers::capture_for(outcome, USAGE_V2_PROJECTION_ID);
            outcome.streams_reconciled = outcome.streams_reconciled.saturating_add(1);
            scheduled_objects.extend(self.collect_stream_work(
                adapter,
                &instance,
                &stream,
                &catalog,
                &mut discovery,
                replay.as_ref(),
                reason,
                started_at,
                outcome,
            )?);
            if usage_v2_stream {
                usage_v2_coverage.note_outcome(
                    stream.id.as_str(),
                    projection_blockers,
                    ProjectionBlockers::capture_for(outcome, USAGE_V2_PROJECTION_ID),
                );
            }
        }
        self.execute_scheduled_objects(
            adapter,
            &instance,
            scheduled_objects,
            reason,
            started_at,
            outcome,
            &mut usage_v2_coverage,
        )?;
        self.finish_instance_projection_readiness(
            manifest,
            &instance,
            &catalog,
            started_at,
            usage_v2_coverage,
            outcome,
            replay.as_ref(),
        )?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_instance_projection_readiness(
        &self,
        manifest: &AdapterManifest,
        instance: &SourceInstance,
        prior_catalog: &SourceCatalogSnapshot,
        started_at: i64,
        mut coverage: ProjectionCoverageAttempt,
        outcome: &mut ReconcileOutcome,
        replay: Option<&FactFamilyReplayContext>,
    ) -> Result<(), EngineError> {
        if !declares_usage_v2_projection(manifest) {
            return Ok(());
        }
        let current_catalog = self
            .engine
            .source_catalog(manifest.id.as_str(), instance.spec.stable_key.as_bytes())?;
        if current_catalog.source_instance_id != Some(instance.id) {
            return Err(observation_error(
                "finish projection readiness",
                "source catalog identity changed before the readiness barrier",
            ));
        }
        for object in &current_catalog.objects {
            if !coverage.required_streams.contains_key(&object.stream_key) {
                continue;
            }
            match object.state.as_str() {
                "quarantined" => coverage.block(&object.stream_key, "durable_quarantine"),
                "retrying" => coverage.block(&object.stream_key, "durable_retry"),
                _ => {}
            }
        }
        if let Some(replay) = replay {
            if replay.family != USAGE_V2_PROJECTION_ID
                || replay.version != USAGE_V2_PROJECTION_VERSION
            {
                return Err(observation_error(
                    "finish projection replay",
                    "replay family/version does not match the usage-v2 projection",
                ));
            }
            replay.verify_replacement(manifest, &current_catalog, &mut coverage)?;
        }
        let prior = prior_catalog
            .projection_versions
            .iter()
            .find(|projection| projection.projection_id == USAGE_V2_PROJECTION_ID);
        let sticky_quarantine = prior.is_some_and(|projection| {
            projection.readiness == "unavailable"
                && projection
                    .detail
                    .as_deref()
                    .is_some_and(|detail| detail.contains("records_quarantined"))
        });
        let mut detail = coverage.detail();
        if sticky_quarantine && replay.is_none() {
            detail = match detail {
                Some(current) if coverage_detail_only_quarantine(&current) => prior
                    .and_then(|projection| projection.detail.clone())
                    .or(Some(current)),
                Some(current) => Some(format!(
                    "{current}; retained=records_quarantined_requires_explicit_replay"
                )),
                None => prior
                    .and_then(|projection| projection.detail.clone())
                    .or_else(|| {
                        Some(
                            "runtime.usage-v2 coverage incomplete: records_quarantined requires explicit replay"
                                .to_string(),
                        )
                    }),
            };
        }
        let committed_at = now_unix_ms()?.max(started_at);
        if replay.is_some() && detail.is_some() && !coverage.has_quarantine() {
            // Preserve the old normalized coverage set as the durable replay
            // baseline. A later pass resumes only baseline objects whose
            // generation has not advanced yet; bounded append continuation
            // therefore makes progress instead of restarting from byte zero.
            let commit_seq = self
                .engine
                .commit_projection_versions(ProjectionVersionCommit {
                    source_instance_id: instance.id,
                    reason: format!("projection.{USAGE_V2_PROJECTION_ID}.replay_pending"),
                    started_at,
                    committed_at,
                    projection_versions: vec![usage_v2_projection_update(
                        instance,
                        ProjectionReadiness::Pending,
                        Some(USAGE_V2_REPLAY_PENDING_DETAIL),
                    )],
                    coverage_sets: Vec::new(),
                    coverage_preconditions: Vec::new(),
                    query_pack_selections: Vec::new(),
                })?;
            if let Some(commit_seq) = commit_seq {
                outcome.commits = outcome.commits.saturating_add(1);
                outcome.last_commit_seq = Some(commit_seq);
            }
            return Ok(());
        }
        let (readiness, state) = if detail.is_none() {
            (ProjectionReadiness::Ready, "ready")
        } else {
            (ProjectionReadiness::Unavailable, "unavailable")
        };
        let coverage_set = usage_v2_coverage_set_update(
            manifest,
            instance,
            &current_catalog,
            &coverage,
            detail.as_deref(),
        )?;
        let commit_seq = self
            .engine
            .commit_projection_versions(ProjectionVersionCommit {
                source_instance_id: instance.id,
                reason: format!("projection.{USAGE_V2_PROJECTION_ID}.{state}"),
                started_at,
                committed_at,
                projection_versions: vec![usage_v2_projection_update(
                    instance,
                    readiness,
                    detail.as_deref(),
                )],
                coverage_sets: vec![coverage_set],
                coverage_preconditions: Vec::new(),
                query_pack_selections: Vec::new(),
            })?;
        if let Some(commit_seq) = commit_seq {
            outcome.commits = outcome.commits.saturating_add(1);
            outcome.last_commit_seq = Some(commit_seq);
        }
        Ok(())
    }

    fn reserve_instance<A: AgentAdapter + ?Sized>(
        &self,
        adapter: &A,
        spec: &AdapterSourceInstanceSpec,
        started_at: i64,
    ) -> Result<u64, EngineError> {
        self.check_cancelled()?;
        self.engine.reserve_source_instance(source_instance_spec(
            adapter.manifest(),
            spec,
            started_at,
            started_at,
        ))
    }

    fn commit_observation_checked(
        &self,
        request: ObservationCommit,
    ) -> Result<CommitReceipt, EngineError> {
        self.check_cancelled()?;
        self.engine.commit_observation(request)
    }

    fn commit_facts_checked(
        &self,
        request: ObservationCommit,
        batch: FactBatch,
    ) -> Result<CommitReceipt, EngineError> {
        self.check_cancelled()?;
        self.engine.commit_facts(request, batch)
    }

    fn commit_observation_on(
        &self,
        request: ObservationCommit,
        lane: &mut CommitLane,
    ) -> Result<(), EngineError> {
        self.check_cancelled()?;
        lane.submit_observation(request)
    }

    fn commit_facts_on(
        &self,
        request: ObservationCommit,
        batch: FactBatch,
        lane: &mut CommitLane,
    ) -> Result<(), EngineError> {
        self.check_cancelled()?;
        lane.submit_facts(request, batch)
    }

    #[allow(clippy::too_many_arguments)]
    fn commit_append_slice<A: AgentAdapter + ?Sized>(
        &self,
        adapter: &A,
        instance: &SourceInstance,
        stream: &StreamSpec,
        object: &DeclaredObject,
        object_context: &AdapterObjectContext,
        durable: &mut DurableObject,
        lane: &mut CommitLane,
        checkpoint: &AppendCheckpoint,
        batch: FactBatch,
        errors: Vec<SourceRecordError>,
        decoder_state: Option<Vec<u8>>,
        decoder_state_version: Option<u32>,
        force_facts: bool,
        reason: &str,
        started_at: i64,
    ) -> Result<(), EngineError> {
        let checkpoint_bytes = checkpoint.encode();
        let request = commit_request(
            adapter,
            instance,
            stream,
            object,
            object_context,
            durable.expected(),
            checkpoint.generation,
            checkpoint.cursor().into_bytes(),
            Some(checkpoint_bytes.clone()),
            decoder_state,
            decoder_state_version,
            "active",
            errors,
            reason,
            started_at,
        )?;
        if batch.facts().is_empty() && !force_facts {
            self.commit_observation_on(request, lane)?;
        } else {
            self.commit_facts_on(request, batch, lane)?;
        }
        durable.advance(checkpoint, checkpoint_bytes);
        durable.state = "active".to_string();
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn collect_stream_work<A: AgentAdapter + ?Sized>(
        &self,
        adapter: &A,
        instance: &SourceInstance,
        stream: &StreamSpec,
        catalog: &SourceCatalogSnapshot,
        discovery: &mut DiscoveryIndex,
        replay: Option<&FactFamilyReplayContext>,
        reason: &str,
        started_at: i64,
        outcome: &mut ReconcileOutcome,
    ) -> Result<Vec<ObjectWork>, EngineError> {
        self.check_cancelled()?;
        if stream.authority == StreamAuthority::IgnoredDerived {
            return Ok(Vec::new());
        }
        let root = instance
            .root(&stream.selector.root_name)
            .map_err(|error| adapter_error("resolve source root", error))?;
        if let DriverSpec::DirectorySnapshot(config) = &stream.driver {
            if replay.is_some_and(|replay| replay.stream_is_provider(stream)) {
                return Err(observation_error(
                    "replay fact-family provider stream",
                    format!(
                        "directory snapshot stream {} cannot directly provide replayable facts",
                        stream.id
                    ),
                ));
            }
            self.reconcile_directory_snapshot(
                adapter, instance, stream, root, catalog, config, reason, started_at, outcome,
            )?;
            return Ok(Vec::new());
        }
        let discovered = discovery.discover_objects(root, stream, &self.cancellations)?;
        if !discovered.available {
            outcome.streams_unavailable = outcome.streams_unavailable.saturating_add(1);
            return Ok(Vec::new());
        }
        outcome.objects_discovered = outcome
            .objects_discovered
            .saturating_add(bounded_u32(discovered.objects.len()));

        let mut stored_by_key = BTreeMap::new();
        let mut stored_by_path = BTreeMap::new();
        for object in catalog
            .objects
            .iter()
            .filter(|object| object.stream_key == stream.id.as_str())
        {
            stored_by_key.insert(object.object_key.clone(), object.clone());
            if let Some(display_path) = &object.display_path {
                stored_by_path.insert(display_path.clone(), object.object_key.clone());
            }
        }

        let mut work = Vec::new();
        for object in discovered.objects.values() {
            self.check_cancelled()?;
            let previous = stored_by_key
                .remove(&object.descriptor.object_key)
                .or_else(|| {
                    let display = object
                        .descriptor
                        .relative_path
                        .to_string_lossy()
                        .into_owned();
                    let prior_key = stored_by_path.get(&display)?;
                    stored_by_key.remove(prior_key)
                });
            let force_replay = replay
                .map(|replay| {
                    replay.force_generation_replay(adapter.manifest(), stream, previous.as_ref())
                })
                .transpose()?
                .unwrap_or(false);
            work.push(ObjectWork {
                stream: stream.clone(),
                object: object.clone(),
                previous,
                force_replay,
            });
        }

        if stream.deletion == DeletionPolicy::MirrorSource {
            for previous in stored_by_key
                .values()
                .filter(|object| object.state != "absent")
            {
                self.check_cancelled()?;
                let Some(relative_path) = previous_display_relative(previous, root) else {
                    outcome.retries_required = outcome.retries_required.saturating_add(1);
                    continue;
                };
                let object = DeclaredObject {
                    path: root.join(&relative_path),
                    descriptor: SourceObjectDescriptor {
                        stream_id: stream.id.clone(),
                        object_key: previous.object_key.clone(),
                        relative_path,
                    },
                    metadata: None,
                };
                if previous.state == "quarantined" {
                    let durable = DurableObject::from_catalog(instance.id, previous);
                    self.commit_missing_object_absence(
                        adapter,
                        instance,
                        stream,
                        &object,
                        &AdapterObjectContext::empty(),
                        &durable,
                        reason,
                        started_at,
                        outcome,
                        None,
                    )?;
                    continue;
                }
                work.push(ObjectWork {
                    stream: stream.clone(),
                    object,
                    previous: Some(previous.clone()),
                    force_replay: replay
                        .map(|replay| {
                            replay.force_generation_replay(
                                adapter.manifest(),
                                stream,
                                Some(previous),
                            )
                        })
                        .transpose()?
                        .unwrap_or(false),
                });
            }
        }
        Ok(work)
    }

    #[allow(clippy::too_many_arguments)]
    fn reconcile_directory_snapshot<A: AgentAdapter + ?Sized>(
        &self,
        adapter: &A,
        instance: &SourceInstance,
        stream: &StreamSpec,
        root: &Path,
        catalog: &SourceCatalogSnapshot,
        config: &crate::source::DirectorySnapshotConfig,
        reason: &str,
        started_at: i64,
        outcome: &mut ReconcileOutcome,
    ) -> Result<(), EngineError> {
        const DIRECTORY_OBJECT_KEY: &[u8] = b"\x01directory-snapshot-root";
        let previous = catalog.objects.iter().find(|object| {
            object.stream_key == stream.id.as_str()
                && object.object_key.as_slice() == DIRECTORY_OBJECT_KEY
        });
        let previous_checkpoint = previous
            .and_then(|object| object.driver_checkpoint.as_deref())
            .map(|bytes| DirectoryCheckpoint::decode_for_config(bytes, config))
            .transpose()
            .map_err(source_error)?;
        if previous.is_some_and(|object| object.driver_checkpoint_version != Some(1)) {
            return Err(observation_error(
                "resume directory snapshot",
                format!("stream {} has an unsupported checkpoint version", stream.id),
            ));
        }
        let patterns = SelectorPatterns::new(stream)?;
        let driver = DirectorySnapshot::new(config.clone()).map_err(source_error)?;
        let performance = self.source_performance(adapter, stream);
        let read_started = Instant::now();
        let scan = driver.scan(
            root,
            previous_checkpoint.as_ref(),
            &|relative: &Path, kind| match kind {
                DirectoryEntryKind::Directory => DirectorySelection::Recurse,
                DirectoryEntryKind::File if patterns.matches(relative) => {
                    DirectorySelection::Include
                }
                DirectoryEntryKind::File => DirectorySelection::Ignore,
            },
        );
        performance.record_read(
            read_started.elapsed(),
            scan.is_err(),
            matches!(&scan, Ok(DirectoryScan::RetryTransient)),
            false,
            0,
            0,
        );
        let scan = scan.map_err(source_error)?;
        match scan {
            DirectoryScan::Unavailable => {
                outcome.streams_unavailable = outcome.streams_unavailable.saturating_add(1);
                Ok(())
            }
            DirectoryScan::RetryTransient => {
                outcome.retries_required = outcome.retries_required.saturating_add(1);
                Ok(())
            }
            DirectoryScan::Snapshot {
                changes,
                checkpoint,
                ..
            } => {
                outcome.objects_discovered = outcome.objects_discovered.saturating_add(1);
                if previous_checkpoint.as_ref() == Some(&checkpoint) && changes.is_empty() {
                    outcome.objects_unchanged = outcome.objects_unchanged.saturating_add(1);
                    return Ok(());
                }
                let object = DeclaredObject {
                    path: root.to_path_buf(),
                    descriptor: SourceObjectDescriptor {
                        stream_id: stream.id.clone(),
                        object_key: DIRECTORY_OBJECT_KEY.to_vec(),
                        relative_path: PathBuf::from("."),
                    },
                    metadata: std::fs::symlink_metadata(root).ok(),
                };
                let expected = previous.map_or(ExpectedSourceCursor::Absent, |object| {
                    ExpectedSourceCursor::At {
                        generation: object.generation,
                        committed_cursor: object.committed_cursor.clone(),
                    }
                });
                let request = commit_request(
                    adapter,
                    instance,
                    stream,
                    &object,
                    &AdapterObjectContext::empty(),
                    expected,
                    checkpoint.generation,
                    checkpoint.cursor().into_bytes(),
                    Some(checkpoint.encode()),
                    None,
                    None,
                    "active",
                    Vec::new(),
                    reason,
                    started_at,
                )?;
                let receipt = self.commit_observation_checked(request)?;
                record_commit(outcome, &receipt);
                if previous.is_none() {
                    outcome.objects_registered = outcome.objects_registered.saturating_add(1);
                }
                outcome.objects_changed = outcome.objects_changed.saturating_add(1);
                Ok(())
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn execute_scheduled_objects<A: AgentAdapter + ?Sized>(
        &self,
        adapter: &A,
        instance: &SourceInstance,
        work: Vec<ObjectWork>,
        reason: &str,
        started_at: i64,
        outcome: &mut ReconcileOutcome,
        projection_coverage: &mut ProjectionCoverageAttempt,
    ) -> Result<(), EngineError> {
        if work.is_empty() {
            return Ok(());
        }
        let mut scheduler = BoundedScheduler::new(SCHEDULER_CAPACITY, MAX_OBJECTS_IN_FLIGHT)
            .map_err(source_error)?;
        let mut pending = work.into_iter();
        let mut exhausted = false;
        let mut admitted = BTreeMap::<WorkKey, ObjectWork>::new();
        let workers = self.engine.observation_workers()?;
        let (result_tx, result_rx) = crossbeam_channel::bounded(MAX_OBJECTS_IN_FLIGHT);
        let mut in_flight = 0_usize;
        let mut unacked =
            Vec::<crossbeam_channel::Receiver<Result<CommitReceipt, EngineError>>>::new();
        let mut first_error = None;

        workers.scope(|scope| {
            loop {
                if first_error.is_none() {
                    if let Err(error) = self.check_cancelled() {
                        first_error = Some(error);
                    }
                }
                while first_error.is_none()
                    && in_flight < MAX_OBJECTS_IN_FLIGHT
                    && (!exhausted || scheduler.queued_len() > 0)
                {
                    while !exhausted && scheduler.queued_len() < SCHEDULER_CAPACITY {
                        let Some(next) = pending.next() else {
                            exhausted = true;
                            break;
                        };
                        let generation = next
                            .previous
                            .as_ref()
                            .map_or(1, |previous| previous.generation);
                        let key = match scheduled_work_key(&next.stream, &next.object, generation)
                        {
                            Ok(key) => key,
                            Err(error) => {
                                first_error.get_or_insert(error);
                                exhausted = true;
                                break;
                            }
                        };
                        let scheduled = ScheduledWork {
                            key: key.clone(),
                            priority: next.stream.priority,
                            reason: DirtyReason::Recovery,
                        };
                        match scheduler.enqueue(scheduled) {
                            ScheduleOutcome::Enqueued => {
                                if admitted.insert(key, next).is_some() {
                                    first_error.get_or_insert(observation_error(
                                        "schedule source objects",
                                        "duplicate object/generation work key",
                                    ));
                                    exhausted = true;
                                    break;
                                }
                            }
                            ScheduleOutcome::Coalesced | ScheduleOutcome::PriorityEscalated => {
                                first_error.get_or_insert(observation_error(
                                    "schedule source objects",
                                    "discovery produced duplicate object/generation work",
                                ));
                                exhausted = true;
                                break;
                            }
                            ScheduleOutcome::FullNeedsReconcile => {
                                first_error.get_or_insert(observation_error(
                                    "schedule source objects",
                                    "bounded scheduler rejected work before reaching its declared capacity",
                                ));
                                exhausted = true;
                                break;
                            }
                        }
                    }
                    let Some(scheduled) = scheduler.dispatch() else {
                        break;
                    };
                    let task = match admitted.remove(&scheduled.key) {
                        Some(task) => task,
                        None => {
                            first_error.get_or_insert(observation_error(
                                "dispatch source objects",
                                "scheduler returned work without an admitted object",
                            ));
                            break;
                        }
                    };
                    let key = scheduled.key;
                    let stream_key = task.stream.id.as_str().to_string();
                    let result_tx = result_tx.clone();
                    let engine = Arc::clone(&self.engine);
                    scope.spawn(move |_| {
                        let mut local = ReconcileOutcome::default();
                        let mut lane = CommitLane::new(engine);
                        let result = self.reconcile_object(
                            adapter,
                            instance,
                            &task.stream,
                            &task.object,
                            task.previous.as_ref(),
                            task.force_replay,
                            reason,
                            started_at,
                            &mut local,
                            &mut lane,
                        );
                        if result.is_err() {
                            let _ = lane.flush();
                        }
                        lane.apply_to(&mut local);
                        let _ = result_tx.send((
                            key,
                            stream_key,
                            result,
                            local,
                            lane.pending.take(),
                        ));
                    });
                    in_flight = in_flight.saturating_add(1);
                }
                if in_flight == 0 {
                    if first_error.is_none()
                        && !exhausted
                        && scheduler.queued_len() == 0
                        && scheduler.in_flight_len() == 0
                    {
                        first_error = Some(observation_error(
                            "dispatch source objects",
                            "bounded scheduler made no progress",
                        ));
                    }
                    break;
                }
                match result_rx.recv() {
                    Ok((key, stream_key, result, local, pending)) => {
                        in_flight = in_flight.saturating_sub(1);
                        if !scheduler.complete(&key) {
                            first_error.get_or_insert(observation_error(
                                "complete source object work",
                                "scheduler lost an in-flight object key",
                            ));
                        }
                        if projection_coverage
                            .required_streams
                            .contains_key(&stream_key)
                        {
                            projection_coverage.note_outcome(
                                &stream_key,
                                ProjectionBlockers::default(),
                                ProjectionBlockers::capture_for(
                                    &local,
                                    USAGE_V2_PROJECTION_ID,
                                ),
                            );
                        }
                        merge_outcome(outcome, local);
                        if let Err(error) = result {
                            first_error.get_or_insert(error);
                        }
                        if let Some(pending) = pending {
                            unacked.push(pending);
                        }
                        let block = unacked.len() >= MAX_UNACKED_COMMITS;
                        if let Err(error) = settle_ready_commits(
                            &self.engine,
                            &mut unacked,
                            outcome,
                            block,
                        ) {
                            first_error.get_or_insert(error);
                        }
                    }
                    Err(_) => {
                        first_error.get_or_insert(observation_error(
                            "complete source object work",
                            "object worker channel closed before draining in-flight work",
                        ));
                        break;
                    }
                }
            }
        });
        if let Err(error) = settle_ready_commits(&self.engine, &mut unacked, outcome, true) {
            first_error.get_or_insert(error);
        }
        if let Some(error) = first_error {
            return Err(error);
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn reconcile_object<A: AgentAdapter + ?Sized>(
        &self,
        adapter: &A,
        instance: &SourceInstance,
        stream: &StreamSpec,
        object: &DeclaredObject,
        previous: Option<&SourceCatalogObject>,
        force_replay: bool,
        reason: &str,
        started_at: i64,
        outcome: &mut ReconcileOutcome,
        lane: &mut CommitLane,
    ) -> Result<(), EngineError> {
        self.check_cancelled()?;
        let source_access = ConfinedSourceAccess::new(instance, &self.cancellations);
        let object_context = match catch_adapter_panic("bootstrap source object", || {
            adapter.bootstrap_object_with_access(instance, &object.descriptor, &source_access)
        })? {
            Ok(context) => context,
            Err(error) if error.class == AdapterErrorClass::RecordPermanent => {
                let result = self.quarantine_object_path(
                    adapter, instance, stream, object, previous, error, reason, started_at,
                    outcome, lane,
                );
                apply_access_snapshot(outcome, &source_access.access_snapshot());
                return result;
            }
            Err(error) => return Err(adapter_error("bootstrap source object", error)),
        };
        let durable = match previous {
            Some(previous) => DurableObject::from_catalog(instance.id, previous),
            None => self.register_object(
                adapter,
                instance,
                stream,
                object,
                &object_context,
                reason,
                started_at,
                outcome,
            )?,
        };
        durable.validate_driver_checkpoint(stream)?;
        let root = instance
            .root(&stream.selector.root_name)
            .map_err(|error| adapter_error("resolve source root for confined read", error))?;

        let origin = RecordOrigin {
            source_instance_id: durable.source_instance_id,
            stream_id: durable.source_stream_id,
            object_id: durable.source_object_id,
            observed_at: now_unix_ms()?,
            source_timestamp_hint: None,
            media_type: media_type(&object.path)?,
        };
        let result = match &stream.driver {
            DriverSpec::AppendDelimited(config) => self.reconcile_append(
                adapter,
                instance,
                stream,
                object,
                root,
                &object_context,
                &source_access,
                durable,
                &origin,
                config,
                force_replay,
                reason,
                started_at,
                outcome,
                lane,
            ),
            DriverSpec::ReplaceDocument(config) => self.reconcile_replace(
                adapter,
                instance,
                stream,
                object,
                root,
                &object_context,
                &source_access,
                durable,
                &origin,
                config,
                force_replay,
                reason,
                started_at,
                outcome,
                lane,
            ),
            DriverSpec::Presence(config) => self.reconcile_presence(
                adapter,
                instance,
                stream,
                object,
                root,
                &object_context,
                &source_access,
                durable,
                &origin,
                config,
                force_replay,
                reason,
                started_at,
                outcome,
                lane,
            ),
            DriverSpec::SqliteSnapshot(config) => self.reconcile_sqlite_snapshot(
                adapter,
                instance,
                stream,
                object,
                root,
                &object_context,
                &source_access,
                durable,
                &origin,
                config,
                force_replay,
                reason,
                started_at,
                outcome,
                lane,
            ),
            DriverSpec::KeyValueSnapshot(config) => self.reconcile_key_value_snapshot(
                adapter,
                instance,
                stream,
                object,
                root,
                &object_context,
                &source_access,
                durable,
                &origin,
                config,
                force_replay,
                reason,
                started_at,
                outcome,
                lane,
            ),
            DriverSpec::DirectorySnapshot(_) => unreachable!("rejected before object discovery"),
        };
        let dependency_changed = if result.is_ok() {
            source_access.changed_since_read()
        } else {
            Ok(false)
        };
        apply_access_snapshot(outcome, &source_access.access_snapshot());
        if dependency_changed? {
            outcome.retries_required = outcome.retries_required.saturating_add(1);
            record_retry_target(outcome, instance, stream, object);
        }
        result
    }

    #[allow(clippy::too_many_arguments)]
    fn quarantine_object_path<A: AgentAdapter + ?Sized>(
        &self,
        adapter: &A,
        instance: &SourceInstance,
        stream: &StreamSpec,
        object: &DeclaredObject,
        previous: Option<&SourceCatalogObject>,
        error: AdapterError,
        reason: &str,
        started_at: i64,
        outcome: &mut ReconcileOutcome,
        lane: &mut CommitLane,
    ) -> Result<(), EngineError> {
        if previous.is_some_and(|object| object.state == "quarantined") {
            outcome.objects_unchanged = outcome.objects_unchanged.saturating_add(1);
            return Ok(());
        }

        let (expected, generation) = match previous {
            Some(previous) => {
                let durable = DurableObject::from_catalog(instance.id, previous);
                (
                    durable.expected(),
                    next_object_generation(&durable, "quarantine invalid source object path")?,
                )
            }
            None => (ExpectedSourceCursor::Absent, 1),
        };
        let cursor = initial_cursor_bytes(stream);
        let request = commit_request(
            adapter,
            instance,
            stream,
            object,
            &AdapterObjectContext::empty(),
            expected,
            generation,
            cursor.clone(),
            None,
            None,
            None,
            "quarantined",
            vec![SourceRecordError {
                generation,
                cursor_start: cursor.clone(),
                cursor_end: cursor,
                payload_hash: blake3::hash(&object.descriptor.object_key)
                    .as_bytes()
                    .to_vec(),
                media_type: media_type(&object.path)?.as_str().to_string(),
                raw_payload: None,
                error_class: adapter_error_class(error.class).to_string(),
                error_message: format!("{}: {}", error.code, error.message),
                adapter_version: adapter.manifest().adapter_version.clone(),
                contract_version: adapter.manifest().contract_version,
                last_retry_at: None,
            }],
            reason,
            started_at,
        )?;
        self.commit_facts_on(
            request,
            FactBatch::new(FACT_BATCH_LIMIT, DIAGNOSTIC_LIMIT)
                .map_err(|error| adapter_error("create path quarantine fact batch", error))?,
            lane,
        )?;
        record_unscoped_quarantines(outcome, 1);
        outcome.objects_changed = outcome.objects_changed.saturating_add(1);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn register_object<A: AgentAdapter + ?Sized>(
        &self,
        adapter: &A,
        instance: &SourceInstance,
        stream: &StreamSpec,
        object: &DeclaredObject,
        object_context: &AdapterObjectContext,
        _reason: &str,
        _started_at: i64,
        outcome: &mut ReconcileOutcome,
    ) -> Result<DurableObject, EngineError> {
        let source_stream_id = source_stream_catalog_id(instance.id, stream.id.as_str());
        let source_object_id =
            source_object_catalog_id(source_stream_id, &object.descriptor.object_key);
        outcome.objects_registered = outcome.objects_registered.saturating_add(1);
        Ok(DurableObject {
            source_instance_id: instance.id,
            source_stream_id,
            source_object_id,
            generation: 1,
            committed_cursor: initial_cursor_bytes(stream),
            adapter_object_context: Some(object_context.payload().to_vec()),
            driver_checkpoint: None,
            driver_checkpoint_version: None,
            decoder_state: None,
            decoder_state_version: None,
            retry_state: None,
            decoder_contract_version: adapter.manifest().contract_version,
            state: "pending".to_string(),
            unpersisted: true,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn reconcile_append<A: AgentAdapter + ?Sized>(
        &self,
        adapter: &A,
        instance: &SourceInstance,
        stream: &StreamSpec,
        object: &DeclaredObject,
        root: &Path,
        object_context: &AdapterObjectContext,
        source_access: &ConfinedSourceAccess<'_>,
        mut durable: DurableObject,
        origin: &RecordOrigin,
        config: &crate::source::AppendDelimitedConfig,
        force_replay: bool,
        reason: &str,
        started_at: i64,
        outcome: &mut ReconcileOutcome,
        lane: &mut CommitLane,
    ) -> Result<(), EngineError> {
        let mut config = config.clone();
        config.max_records_per_batch = config
            .max_records_per_batch
            .min(MAX_APPEND_RECORDS_PER_COMMIT)
            .min(self.max_append_records_per_reconcile);
        let driver = AppendDelimitedFile::new(config).map_err(source_error)?;
        let performance = self.source_performance(adapter, stream);
        let semantic_context = fact_semantic_context(adapter, instance, stream, object)?;
        let decode_binding = DurableDecodeBinding {
            stream,
            object_context,
            semantic_context: &semantic_context,
        };
        let mut previous = durable
            .driver_checkpoint
            .as_deref()
            .map(AppendCheckpoint::decode)
            .transpose()
            .map_err(source_error)?;
        let mut force_contract_replay = durable.driver_checkpoint.is_some()
            && (force_replay
                || durable.decoder_contract_changed(adapter)
                || durable.object_context_changed(object_context));
        let mut records_seen = 0_usize;
        loop {
            self.check_cancelled()?;
            if records_seen >= self.max_append_records_per_reconcile {
                outcome.retries_required = outcome.retries_required.saturating_add(1);
                outcome.backlog_remaining = outcome.backlog_remaining.saturating_add(1);
                record_retry_target(outcome, instance, stream, object);
                return Ok(());
            }
            let read_started = Instant::now();
            let read = driver.read_confined(
                root,
                &object.descriptor.relative_path,
                previous.as_ref(),
                origin,
                force_contract_replay,
            );
            let (read_retry, read_continuation, records_read, payload_bytes_read) = read
                .as_ref()
                .map(append_read_volume)
                .unwrap_or((false, false, 0, 0));
            performance.record_read(
                read_started.elapsed(),
                read.is_err(),
                read_retry,
                read_continuation,
                records_read,
                payload_bytes_read,
            );
            match read.map_err(source_error)? {
                AppendRead::Missing => {
                    if stream.deletion == DeletionPolicy::MirrorSource {
                        self.commit_missing_object_absence(
                            adapter,
                            instance,
                            stream,
                            object,
                            object_context,
                            &durable,
                            reason,
                            started_at,
                            outcome,
                            Some(lane),
                        )?;
                    } else {
                        outcome.objects_unchanged = outcome.objects_unchanged.saturating_add(1);
                    }
                    return Ok(());
                }
                AppendRead::RetryTransient => {
                    outcome.retries_required = outcome.retries_required.saturating_add(1);
                    return Ok(());
                }
                AppendRead::Batch {
                    mut items,
                    mut checkpoint,
                    needs_retry,
                    more_available,
                    ..
                } => {
                    let rebased_recreation = durable.state == "absent" && previous.is_none();
                    if rebased_recreation {
                        let recreated_generation =
                            next_object_generation(&durable, "resume recreated append object")?;
                        checkpoint.generation = recreated_generation;
                        for item in &mut items {
                            match item {
                                AppendItem::Record(record) => {
                                    record.generation = recreated_generation;
                                }
                                AppendItem::Quarantined(quarantine) => {
                                    quarantine.generation = recreated_generation;
                                }
                            }
                        }
                    }
                    let checkpoint_bytes = checkpoint.encode();
                    let generation_changed = checkpoint.generation != durable.generation;
                    let prior_decoder_state = (!generation_changed)
                        .then(|| durable.decoder_state.clone())
                        .flatten();
                    let prior_decoder_state_version = (!generation_changed)
                        .then_some(durable.decoder_state_version)
                        .flatten();
                    if items.is_empty() {
                        let made_progress = previous
                            .as_ref()
                            .is_none_or(|old| old.cursor() != checkpoint.cursor());
                        if previous.as_ref() != Some(&checkpoint) {
                            let retained_decoder_state = prior_decoder_state.clone();
                            let request = commit_request(
                                adapter,
                                instance,
                                stream,
                                object,
                                object_context,
                                durable.expected(),
                                checkpoint.generation,
                                checkpoint.cursor().into_bytes(),
                                Some(checkpoint_bytes.clone()),
                                prior_decoder_state,
                                prior_decoder_state_version,
                                "active",
                                Vec::new(),
                                reason,
                                started_at,
                            )?;
                            if generation_changed {
                                self.commit_facts_on(
                                    request,
                                    FactBatch::new(FACT_BATCH_LIMIT, DIAGNOSTIC_LIMIT).map_err(
                                        |error| {
                                            adapter_error(
                                                "create generation replacement fact batch",
                                                error,
                                            )
                                        },
                                    )?,
                                    lane,
                                )?;
                            } else {
                                self.commit_observation_on(request, lane)?;
                            }
                            durable.advance(&checkpoint, checkpoint_bytes.clone());
                            durable.state = "active".to_string();
                            durable.decoder_state = retained_decoder_state;
                            durable.decoder_state_version = prior_decoder_state_version;
                            outcome.objects_changed = outcome.objects_changed.saturating_add(1);
                        } else {
                            outcome.objects_unchanged = outcome.objects_unchanged.saturating_add(1);
                        }
                        if more_available && !made_progress {
                            outcome.retries_required = outcome.retries_required.saturating_add(1);
                            return Ok(());
                        }
                    } else {
                        records_seen = records_seen.saturating_add(items.len());
                        let mut batch = FactBatch::new(FACT_BATCH_LIMIT, DIAGNOSTIC_LIMIT)
                            .map_err(|error| {
                                adapter_error("create append commit fact batch", error)
                            })?;
                        let mut errors = Vec::new();
                        let mut next_decoder_state = prior_decoder_state.clone();
                        let mut next_decoder_state_version = prior_decoder_state_version;
                        let mut decoded_count = 0_u32;
                        let mut quarantined_count = 0_u32;
                        let mut unscoped_quarantined_count = 0_u32;
                        let mut capability_quarantined_counts = BTreeMap::new();
                        let mut remaining_generation_change = generation_changed;
                        let mut last_included: Option<AppendCheckpoint> = None;
                        for item in &items {
                            self.check_cancelled()?;
                            match item {
                                AppendItem::Record(record) => {
                                    // If this record overflows the bounded commit batch, the
                                    // preceding slice must persist the decoder state that
                                    // produced that slice—not the state produced by the record
                                    // that will be committed next. Advancing state ahead of its
                                    // cursor can skip state-dependent facts after a crash.
                                    let decoder_state_before_record = next_decoder_state.clone();
                                    let decoder_state_version_before_record =
                                        next_decoder_state_version;
                                    let decoded = decode_record(
                                        adapter,
                                        &decode_binding,
                                        source_access,
                                        &performance,
                                        record,
                                        next_decoder_state.as_deref(),
                                    )?;
                                    if decoded.disposition == DecodeDisposition::RetryTransient {
                                        outcome.retries_required =
                                            outcome.retries_required.saturating_add(1);
                                        return Ok(());
                                    }
                                    decoded_count = decoded_count.saturating_add(1);
                                    if decoded.quarantined {
                                        quarantined_count = quarantined_count.saturating_add(1);
                                    }
                                    if decoded.unscoped_permanent_diagnostic {
                                        unscoped_quarantined_count =
                                            unscoped_quarantined_count.saturating_add(1);
                                    }
                                    increment_capability_quarantines(
                                        &mut capability_quarantined_counts,
                                        &decoded.diagnostic_coverage_gaps,
                                    );
                                    if !batch.can_append(&decoded.batch)
                                        || errors.len().saturating_add(decoded.errors.len())
                                            > DIAGNOSTIC_LIMIT
                                    {
                                        let Some(slice) = last_included.as_ref() else {
                                            return Err(adapter_error(
                                                "merge append record fact batches",
                                                AdapterError::invalid_contract(
                                                    "single append record exceeded the commit diagnostic bound",
                                                ),
                                            ));
                                        };
                                        self.commit_append_slice(
                                            adapter,
                                            instance,
                                            stream,
                                            object,
                                            object_context,
                                            &mut durable,
                                            lane,
                                            slice,
                                            std::mem::replace(
                                                &mut batch,
                                                FactBatch::new(FACT_BATCH_LIMIT, DIAGNOSTIC_LIMIT)
                                                    .map_err(|error| {
                                                        adapter_error(
                                                            "create overflow append fact batch",
                                                            error,
                                                        )
                                                    })?,
                                            ),
                                            std::mem::take(&mut errors),
                                            decoder_state_before_record.clone(),
                                            decoder_state_version_before_record,
                                            remaining_generation_change,
                                            reason,
                                            started_at,
                                        )?;
                                        remaining_generation_change = false;
                                        durable.decoder_state = decoder_state_before_record;
                                        durable.decoder_state_version =
                                            decoder_state_version_before_record;
                                    }
                                    if let Some(state) = decoded.next_decoder_state.clone() {
                                        next_decoder_state = Some(state);
                                        next_decoder_state_version =
                                            Some(adapter.manifest().contract_version);
                                    }
                                    errors.extend(decoded.errors);
                                    batch.append(decoded.batch).map_err(|error| {
                                        adapter_error("merge append record fact batches", error)
                                    })?;
                                    last_included = Some(append_checkpoint_through(
                                        &checkpoint,
                                        &record.cursor_end,
                                    ));
                                }
                                AppendItem::Quarantined(quarantine) => {
                                    quarantined_count = quarantined_count.saturating_add(1);
                                    unscoped_quarantined_count =
                                        unscoped_quarantined_count.saturating_add(1);
                                    if errors.len() >= DIAGNOSTIC_LIMIT {
                                        let Some(slice) = last_included.as_ref() else {
                                            return Err(observation_error(
                                                "merge append record fact batches",
                                                "quarantine exceeded the commit diagnostic bound",
                                            ));
                                        };
                                        self.commit_append_slice(
                                            adapter,
                                            instance,
                                            stream,
                                            object,
                                            object_context,
                                            &mut durable,
                                            lane,
                                            slice,
                                            std::mem::replace(
                                                &mut batch,
                                                FactBatch::new(FACT_BATCH_LIMIT, DIAGNOSTIC_LIMIT)
                                                    .map_err(|error| {
                                                        adapter_error(
                                                            "create overflow append fact batch",
                                                            error,
                                                        )
                                                    })?,
                                            ),
                                            std::mem::take(&mut errors),
                                            next_decoder_state.clone(),
                                            next_decoder_state_version,
                                            remaining_generation_change,
                                            reason,
                                            started_at,
                                        )?;
                                        remaining_generation_change = false;
                                        durable.decoder_state = next_decoder_state.clone();
                                        durable.decoder_state_version = next_decoder_state_version;
                                    }
                                    errors.push(quarantine_error(adapter, origin, quarantine));
                                    last_included = Some(append_checkpoint_through(
                                        &checkpoint,
                                        &quarantine.cursor_end,
                                    ));
                                }
                            }
                        }
                        self.commit_append_slice(
                            adapter,
                            instance,
                            stream,
                            object,
                            object_context,
                            &mut durable,
                            lane,
                            &checkpoint,
                            batch,
                            errors,
                            next_decoder_state.clone(),
                            next_decoder_state_version,
                            remaining_generation_change,
                            reason,
                            started_at,
                        )?;
                        durable.decoder_state = next_decoder_state;
                        durable.decoder_state_version = next_decoder_state_version;
                        outcome.records_decoded =
                            outcome.records_decoded.saturating_add(decoded_count);
                        record_quarantines(
                            outcome,
                            quarantined_count,
                            unscoped_quarantined_count,
                            capability_quarantined_counts,
                        );
                        outcome.objects_changed = outcome.objects_changed.saturating_add(1);
                    }
                    previous = Some(checkpoint);
                    force_contract_replay = false;
                    if !more_available {
                        if needs_retry {
                            outcome.retries_required = outcome.retries_required.saturating_add(1);
                            outcome.incomplete_tail_retries =
                                outcome.incomplete_tail_retries.saturating_add(1);
                            record_retry_target(outcome, instance, stream, object);
                        }
                        return Ok(());
                    }
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn commit_missing_object_absence<A: AgentAdapter + ?Sized>(
        &self,
        adapter: &A,
        instance: &SourceInstance,
        stream: &StreamSpec,
        object: &DeclaredObject,
        object_context: &AdapterObjectContext,
        durable: &DurableObject,
        reason: &str,
        started_at: i64,
        outcome: &mut ReconcileOutcome,
        lane: Option<&mut CommitLane>,
    ) -> Result<(), EngineError> {
        if durable.state == "absent" {
            outcome.objects_unchanged = outcome.objects_unchanged.saturating_add(1);
            return Ok(());
        }
        let generation = next_object_generation(durable, "record source deletion")?;
        let cursor = initial_cursor_bytes(stream);
        let request = commit_request(
            adapter,
            instance,
            stream,
            object,
            object_context,
            durable.expected(),
            generation,
            cursor,
            None,
            None,
            None,
            "absent",
            Vec::new(),
            reason,
            started_at,
        )?;
        let batch = FactBatch::new(FACT_BATCH_LIMIT, DIAGNOSTIC_LIMIT)
            .map_err(|error| adapter_error("create deletion fact batch", error))?;
        if let Some(lane) = lane {
            self.commit_facts_on(request, batch, lane)?;
        } else {
            let receipt = self.commit_facts_checked(request, batch)?;
            record_commit(outcome, &receipt);
        }
        outcome.objects_removed = outcome.objects_removed.saturating_add(1);
        outcome.objects_changed = outcome.objects_changed.saturating_add(1);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn reconcile_replace<A: AgentAdapter + ?Sized>(
        &self,
        adapter: &A,
        instance: &SourceInstance,
        stream: &StreamSpec,
        object: &DeclaredObject,
        root: &Path,
        object_context: &AdapterObjectContext,
        source_access: &ConfinedSourceAccess<'_>,
        durable: DurableObject,
        origin: &RecordOrigin,
        config: &crate::source::ReplaceDocumentConfig,
        force_replay: bool,
        reason: &str,
        started_at: i64,
        outcome: &mut ReconcileOutcome,
        lane: &mut CommitLane,
    ) -> Result<(), EngineError> {
        self.check_cancelled()?;
        let previous = durable
            .driver_checkpoint
            .as_deref()
            .map(ReplaceCheckpoint::decode)
            .transpose()
            .map_err(source_error)?;
        let generation_reset = force_replay
            || durable.decoder_contract_changed(adapter)
            || durable.object_context_changed(object_context);
        let performance = self.source_performance(adapter, stream);
        let driver = ReplaceDocument::new(config.clone()).map_err(source_error)?;
        let read_started = Instant::now();
        let read = driver.read_confined(
            root,
            &object.descriptor.relative_path,
            previous.as_ref(),
            origin,
            generation_reset,
        );
        let (read_retry, read_continuation, records_read, payload_bytes_read) = read
            .as_ref()
            .map(replace_read_volume)
            .unwrap_or((false, false, 0, 0));
        performance.record_read(
            read_started.elapsed(),
            read.is_err(),
            read_retry,
            read_continuation,
            records_read,
            payload_bytes_read,
        );
        match read.map_err(source_error)? {
            ReplaceRead::Missing => {
                if stream.deletion == DeletionPolicy::MirrorSource {
                    self.commit_missing_object_absence(
                        adapter,
                        instance,
                        stream,
                        object,
                        object_context,
                        &durable,
                        reason,
                        started_at,
                        outcome,
                        Some(lane),
                    )
                } else {
                    outcome.objects_unchanged = outcome.objects_unchanged.saturating_add(1);
                    Ok(())
                }
            }
            ReplaceRead::Unchanged { checkpoint } => {
                if previous.as_ref() == Some(&checkpoint) {
                    outcome.objects_unchanged = outcome.objects_unchanged.saturating_add(1);
                    return Ok(());
                }
                let request = commit_request(
                    adapter,
                    instance,
                    stream,
                    object,
                    object_context,
                    durable.expected(),
                    checkpoint.generation,
                    checkpoint.cursor().into_bytes(),
                    Some(checkpoint.encode()),
                    durable.decoder_state,
                    durable.decoder_state_version,
                    "active",
                    Vec::new(),
                    reason,
                    started_at,
                )?;
                self.commit_observation_on(request, lane)?;
                outcome.objects_changed = outcome.objects_changed.saturating_add(1);
                Ok(())
            }
            ReplaceRead::RetryTransient => {
                outcome.retries_required = outcome.retries_required.saturating_add(1);
                Ok(())
            }
            ReplaceRead::Record {
                mut record,
                mut checkpoint,
                ..
            } => {
                rebase_checkpointless_recreation(
                    &durable,
                    previous.as_ref(),
                    &mut checkpoint.generation,
                    &mut record.generation,
                )?;
                self.commit_snapshot_record(
                    adapter,
                    instance,
                    stream,
                    object,
                    object_context,
                    source_access,
                    &performance,
                    &durable,
                    record,
                    checkpoint,
                    false,
                    reason,
                    started_at,
                    outcome,
                    lane,
                )
            }
            ReplaceRead::Removed { record, checkpoint } => {
                if stream.deletion == DeletionPolicy::MirrorSource {
                    self.commit_snapshot_record(
                        adapter,
                        instance,
                        stream,
                        object,
                        object_context,
                        source_access,
                        &performance,
                        &durable,
                        record,
                        checkpoint,
                        true,
                        reason,
                        started_at,
                        outcome,
                        lane,
                    )
                } else {
                    outcome.objects_unchanged = outcome.objects_unchanged.saturating_add(1);
                    Ok(())
                }
            }
            ReplaceRead::Quarantined {
                mut quarantine,
                mut checkpoint,
                ..
            } => {
                rebase_checkpointless_recreation(
                    &durable,
                    previous.as_ref(),
                    &mut checkpoint.generation,
                    &mut quarantine.generation,
                )?;
                let generation_changed = checkpoint.generation != durable.generation;
                let request = commit_request(
                    adapter,
                    instance,
                    stream,
                    object,
                    object_context,
                    durable.expected(),
                    checkpoint.generation,
                    checkpoint.cursor().into_bytes(),
                    Some(checkpoint.encode()),
                    (!generation_changed)
                        .then(|| durable.decoder_state.clone())
                        .flatten(),
                    (!generation_changed)
                        .then_some(durable.decoder_state_version)
                        .flatten(),
                    "quarantined",
                    vec![quarantine_error(adapter, origin, &quarantine)],
                    reason,
                    started_at,
                )?;
                // A replace-document quarantine describes the current whole
                // document, so its former typed snapshot is no longer current
                // even when the driver generation itself did not change.
                self.commit_facts_on(
                    request,
                    FactBatch::new(FACT_BATCH_LIMIT, DIAGNOSTIC_LIMIT).map_err(|error| {
                        adapter_error("create snapshot quarantine fact batch", error)
                    })?,
                    lane,
                )?;
                record_unscoped_quarantines(outcome, 1);
                outcome.objects_changed = outcome.objects_changed.saturating_add(1);
                Ok(())
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn commit_snapshot_record<A: AgentAdapter + ?Sized>(
        &self,
        adapter: &A,
        instance: &SourceInstance,
        stream: &StreamSpec,
        object: &DeclaredObject,
        object_context: &AdapterObjectContext,
        source_access: &ConfinedSourceAccess<'_>,
        performance: &SourcePerformanceRecorder,
        durable: &DurableObject,
        record: SourceRecord,
        checkpoint: ReplaceCheckpoint,
        removed: bool,
        reason: &str,
        started_at: i64,
        outcome: &mut ReconcileOutcome,
        lane: &mut CommitLane,
    ) -> Result<(), EngineError> {
        self.check_cancelled()?;
        let generation_changed = checkpoint.generation != durable.generation;
        let prior_decoder_state = (!generation_changed)
            .then(|| durable.decoder_state.clone())
            .flatten();
        let prior_decoder_state_version = (!generation_changed)
            .then_some(durable.decoder_state_version)
            .flatten();
        let semantic_context = fact_semantic_context(adapter, instance, stream, object)?;
        let decode_binding = DurableDecodeBinding {
            stream,
            object_context,
            semantic_context: &semantic_context,
        };
        let decoded = decode_record(
            adapter,
            &decode_binding,
            source_access,
            performance,
            &record,
            prior_decoder_state.as_deref(),
        )?;
        if decoded.disposition == DecodeDisposition::RetryTransient {
            let now = now_unix_ms()?;
            let mut guard = MalformedRevisionGuard::from_checkpoint(
                MalformedRevisionPolicy::default(),
                durable.retry_state.as_deref(),
            )
            .map_err(source_error)?;
            return match guard.classify_failure(checkpoint.revision, now) {
                ParseFailureDecision::RetryTransient { attempt } => {
                    let mut request = commit_request(
                        adapter,
                        instance,
                        stream,
                        object,
                        object_context,
                        durable.expected(),
                        durable.generation,
                        durable.committed_cursor.clone(),
                        durable.driver_checkpoint.clone(),
                        durable.decoder_state.clone(),
                        durable.decoder_state_version,
                        "retrying",
                        vec![snapshot_decode_error(
                            adapter,
                            stream,
                            &record,
                            "transient",
                            format!(
                                "complete snapshot decode requested retry; stable revision attempt {attempt}"
                            ),
                            Some(now),
                        )],
                        reason,
                        started_at,
                    )?;
                    request.object.observed_revision =
                        Some(checkpoint.revision.as_bytes().to_vec());
                    request.object.retry_state = guard.checkpoint();
                    self.commit_observation_on(request, lane)?;
                    outcome.retries_required = outcome.retries_required.saturating_add(1);
                    outcome.objects_changed = outcome.objects_changed.saturating_add(1);
                    Ok(())
                }
                ParseFailureDecision::Quarantine { attempts } => {
                    let request = commit_request(
                        adapter,
                        instance,
                        stream,
                        object,
                        object_context,
                        durable.expected(),
                        checkpoint.generation,
                        checkpoint.cursor().into_bytes(),
                        Some(checkpoint.encode()),
                        prior_decoder_state,
                        prior_decoder_state_version,
                        "quarantined",
                        vec![snapshot_decode_error(
                            adapter,
                            stream,
                            &record,
                            "record_permanent",
                            format!(
                                "complete snapshot remained malformed after {attempts} stable revision attempts"
                            ),
                            Some(now),
                        )],
                        reason,
                        started_at,
                    )?;
                    self.commit_facts_on(
                        request,
                        FactBatch::new(FACT_BATCH_LIMIT, DIAGNOSTIC_LIMIT).map_err(|error| {
                            adapter_error("create malformed snapshot quarantine batch", error)
                        })?,
                        lane,
                    )?;
                    record_unscoped_quarantines(outcome, 1);
                    outcome.objects_changed = outcome.objects_changed.saturating_add(1);
                    Ok(())
                }
            };
        }
        let mut capability_quarantined_records = BTreeMap::new();
        increment_capability_quarantines(
            &mut capability_quarantined_records,
            &decoded.diagnostic_coverage_gaps,
        );
        let quarantined_records = u32::from(decoded.quarantined);
        let unscoped_quarantined_records = u32::from(decoded.unscoped_permanent_diagnostic);
        let request = commit_request(
            adapter,
            instance,
            stream,
            object,
            object_context,
            durable.expected(),
            checkpoint.generation,
            checkpoint.cursor().into_bytes(),
            Some(checkpoint.encode()),
            decoded.next_decoder_state.clone().or(prior_decoder_state),
            decoded
                .next_decoder_state
                .as_ref()
                .map(|_| adapter.manifest().contract_version)
                .or(prior_decoder_state_version),
            if removed { "absent" } else { "active" },
            decoded.errors,
            reason,
            started_at,
        )?;
        self.commit_facts_on(request, decoded.batch, lane)?;
        outcome.records_decoded = outcome.records_decoded.saturating_add(1);
        record_quarantines(
            outcome,
            quarantined_records,
            unscoped_quarantined_records,
            capability_quarantined_records,
        );
        outcome.objects_changed = outcome.objects_changed.saturating_add(1);
        if removed {
            outcome.objects_removed = outcome.objects_removed.saturating_add(1);
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn reconcile_sqlite_snapshot<A: AgentAdapter + ?Sized>(
        &self,
        adapter: &A,
        instance: &SourceInstance,
        stream: &StreamSpec,
        object: &DeclaredObject,
        root: &Path,
        object_context: &AdapterObjectContext,
        source_access: &ConfinedSourceAccess<'_>,
        durable: DurableObject,
        origin: &RecordOrigin,
        config: &crate::source::SqliteSnapshotConfig,
        force_replay: bool,
        reason: &str,
        started_at: i64,
        outcome: &mut ReconcileOutcome,
        lane: &mut CommitLane,
    ) -> Result<(), EngineError> {
        let previous = durable
            .driver_checkpoint
            .as_deref()
            .map(SqliteCheckpoint::decode)
            .transpose()
            .map_err(source_error)?;
        let force_replay = force_replay
            || durable.decoder_contract_changed(adapter)
            || durable.object_context_changed(object_context);
        let cancellations = self.cancellations.clone();
        let performance = self.source_performance(adapter, stream);
        let driver = SqliteSnapshot::new(config.clone()).map_err(source_error)?;
        let read_started = Instant::now();
        let read = driver.read_confined(
            root,
            &object.descriptor.relative_path,
            previous.as_ref(),
            origin,
            force_replay,
            move || {
                cancellations
                    .iter()
                    .any(QueryCancellationToken::is_cancelled)
            },
        );
        let (read_retry, read_continuation, records_read, payload_bytes_read) = read
            .as_ref()
            .map(sqlite_read_volume)
            .unwrap_or((false, false, 0, 0));
        performance.record_read(
            read_started.elapsed(),
            read.is_err(),
            read_retry,
            read_continuation,
            records_read,
            payload_bytes_read,
        );
        match read.map_err(source_error)? {
            SqliteRead::Missing => {
                if stream.deletion == DeletionPolicy::MirrorSource {
                    self.commit_missing_object_absence(
                        adapter,
                        instance,
                        stream,
                        object,
                        object_context,
                        &durable,
                        reason,
                        started_at,
                        outcome,
                        Some(lane),
                    )
                } else {
                    outcome.objects_unchanged = outcome.objects_unchanged.saturating_add(1);
                    Ok(())
                }
            }
            SqliteRead::RetryTransient => {
                self.check_cancelled()?;
                outcome.retries_required = outcome.retries_required.saturating_add(1);
                record_retry_target(outcome, instance, stream, object);
                Ok(())
            }
            SqliteRead::Unchanged { .. } => {
                outcome.objects_unchanged = outcome.objects_unchanged.saturating_add(1);
                Ok(())
            }
            SqliteRead::Snapshot {
                mut records,
                mut checkpoint,
                ..
            } => {
                rebase_database_recreation(
                    &durable,
                    previous.is_none(),
                    &mut checkpoint.generation,
                    &mut records,
                )?;
                self.commit_database_snapshot(
                    adapter,
                    instance,
                    stream,
                    object,
                    object_context,
                    source_access,
                    &performance,
                    &durable,
                    records,
                    checkpoint.generation,
                    checkpoint.cursor().into_bytes(),
                    checkpoint.encode(),
                    reason,
                    started_at,
                    outcome,
                    lane,
                )
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn reconcile_key_value_snapshot<A: AgentAdapter + ?Sized>(
        &self,
        adapter: &A,
        instance: &SourceInstance,
        stream: &StreamSpec,
        object: &DeclaredObject,
        root: &Path,
        object_context: &AdapterObjectContext,
        source_access: &ConfinedSourceAccess<'_>,
        durable: DurableObject,
        origin: &RecordOrigin,
        config: &crate::source::KeyValueSnapshotConfig,
        force_replay: bool,
        reason: &str,
        started_at: i64,
        outcome: &mut ReconcileOutcome,
        lane: &mut CommitLane,
    ) -> Result<(), EngineError> {
        let previous = durable
            .driver_checkpoint
            .as_deref()
            .map(KeyValueCheckpoint::decode)
            .transpose()
            .map_err(source_error)?;
        let force_replay = force_replay
            || durable.decoder_contract_changed(adapter)
            || durable.object_context_changed(object_context);
        let cancellations = self.cancellations.clone();
        let performance = self.source_performance(adapter, stream);
        let driver = KeyValueSnapshot::new(config.clone()).map_err(source_error)?;
        let read_started = Instant::now();
        let read = driver.read_confined(
            root,
            &object.descriptor.relative_path,
            previous.as_ref(),
            origin,
            force_replay,
            move || {
                cancellations
                    .iter()
                    .any(QueryCancellationToken::is_cancelled)
            },
        );
        let (read_retry, read_continuation, records_read, payload_bytes_read) = read
            .as_ref()
            .map(key_value_read_volume)
            .unwrap_or((false, false, 0, 0));
        performance.record_read(
            read_started.elapsed(),
            read.is_err(),
            read_retry,
            read_continuation,
            records_read,
            payload_bytes_read,
        );
        match read.map_err(source_error)? {
            KeyValueRead::Missing => {
                if stream.deletion == DeletionPolicy::MirrorSource {
                    self.commit_missing_object_absence(
                        adapter,
                        instance,
                        stream,
                        object,
                        object_context,
                        &durable,
                        reason,
                        started_at,
                        outcome,
                        Some(lane),
                    )
                } else {
                    outcome.objects_unchanged = outcome.objects_unchanged.saturating_add(1);
                    Ok(())
                }
            }
            KeyValueRead::RetryTransient => {
                self.check_cancelled()?;
                outcome.retries_required = outcome.retries_required.saturating_add(1);
                record_retry_target(outcome, instance, stream, object);
                Ok(())
            }
            KeyValueRead::Unchanged { .. } => {
                outcome.objects_unchanged = outcome.objects_unchanged.saturating_add(1);
                Ok(())
            }
            KeyValueRead::Snapshot {
                mut records,
                mut checkpoint,
                ..
            } => {
                rebase_database_recreation(
                    &durable,
                    previous.is_none(),
                    &mut checkpoint.generation,
                    &mut records,
                )?;
                self.commit_database_snapshot(
                    adapter,
                    instance,
                    stream,
                    object,
                    object_context,
                    source_access,
                    &performance,
                    &durable,
                    records,
                    checkpoint.generation,
                    checkpoint.cursor().into_bytes(),
                    checkpoint.encode(),
                    reason,
                    started_at,
                    outcome,
                    lane,
                )
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn commit_database_snapshot<A: AgentAdapter + ?Sized>(
        &self,
        adapter: &A,
        instance: &SourceInstance,
        stream: &StreamSpec,
        object: &DeclaredObject,
        object_context: &AdapterObjectContext,
        source_access: &ConfinedSourceAccess<'_>,
        performance: &SourcePerformanceRecorder,
        durable: &DurableObject,
        records: Vec<SourceRecord>,
        generation: u64,
        cursor: Vec<u8>,
        checkpoint: Vec<u8>,
        reason: &str,
        started_at: i64,
        outcome: &mut ReconcileOutcome,
        lane: &mut CommitLane,
    ) -> Result<(), EngineError> {
        self.check_cancelled()?;
        let generation_changed = generation != durable.generation;
        let prior_decoder_state = (!generation_changed)
            .then(|| durable.decoder_state.clone())
            .flatten();
        let semantic_context = fact_semantic_context(adapter, instance, stream, object)?;
        let decode_binding = DurableDecodeBinding {
            stream,
            object_context,
            semantic_context: &semantic_context,
        };
        let decoded = decode_snapshot_records(
            adapter,
            &decode_binding,
            source_access,
            performance,
            &records,
            prior_decoder_state,
        )?;
        let Some(decoded) = decoded else {
            outcome.retries_required = outcome.retries_required.saturating_add(1);
            record_retry_target(outcome, instance, stream, object);
            return Ok(());
        };
        let decoder_state_version = decoded
            .next_decoder_state
            .as_ref()
            .map(|_| adapter.manifest().contract_version);
        let request = commit_request(
            adapter,
            instance,
            stream,
            object,
            object_context,
            durable.expected(),
            generation,
            cursor,
            Some(checkpoint),
            decoded.next_decoder_state,
            decoder_state_version,
            "active",
            decoded.errors,
            reason,
            started_at,
        )?;
        self.commit_facts_on(request, decoded.batch, lane)?;
        outcome.records_decoded = outcome
            .records_decoded
            .saturating_add(bounded_u32(records.len()));
        record_quarantines(
            outcome,
            decoded.quarantined_records,
            decoded.unscoped_quarantined_records,
            decoded.capability_quarantined_records,
        );
        outcome.objects_changed = outcome.objects_changed.saturating_add(1);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn reconcile_presence<A: AgentAdapter + ?Sized>(
        &self,
        adapter: &A,
        instance: &SourceInstance,
        stream: &StreamSpec,
        object: &DeclaredObject,
        root: &Path,
        object_context: &AdapterObjectContext,
        source_access: &ConfinedSourceAccess<'_>,
        durable: DurableObject,
        origin: &RecordOrigin,
        config: &crate::source::PresenceObjectConfig,
        force_replay: bool,
        reason: &str,
        started_at: i64,
        outcome: &mut ReconcileOutcome,
        lane: &mut CommitLane,
    ) -> Result<(), EngineError> {
        self.check_cancelled()?;
        let previous = durable
            .driver_checkpoint
            .as_deref()
            .map(PresenceCheckpoint::decode)
            .transpose()
            .map_err(source_error)?;
        let contract_replay = force_replay || durable.decoder_contract_changed(adapter);
        let performance = self.source_performance(adapter, stream);
        let driver = PresenceObject::new(config.clone()).map_err(source_error)?;
        let read_started = Instant::now();
        let read = driver.read_confined(
            root,
            &object.descriptor.relative_path,
            (!contract_replay).then_some(previous.as_ref()).flatten(),
            origin,
        );
        let (read_retry, read_continuation, records_read, payload_bytes_read) = read
            .as_ref()
            .map(presence_read_volume)
            .unwrap_or((false, false, 0, 0));
        performance.record_read(
            read_started.elapsed(),
            read.is_err(),
            read_retry,
            read_continuation,
            records_read,
            payload_bytes_read,
        );
        match read.map_err(source_error)? {
            PresenceRead::RetryTransient => {
                outcome.retries_required = outcome.retries_required.saturating_add(1);
                Ok(())
            }
            PresenceRead::Unchanged { .. } => {
                outcome.objects_unchanged = outcome.objects_unchanged.saturating_add(1);
                Ok(())
            }
            PresenceRead::Observation {
                mut record,
                mut checkpoint,
                ..
            } => {
                if contract_replay {
                    let generation = next_object_generation(&durable, "replay presence contract")?;
                    checkpoint.generation = generation;
                    record.generation = generation;
                }
                let removed = !checkpoint.present;
                if removed && stream.deletion != DeletionPolicy::MirrorSource {
                    outcome.objects_unchanged = outcome.objects_unchanged.saturating_add(1);
                    return Ok(());
                }
                let generation_changed = checkpoint.generation != durable.generation;
                let prior_decoder_state = (!generation_changed)
                    .then(|| durable.decoder_state.clone())
                    .flatten();
                let prior_decoder_state_version = (!generation_changed)
                    .then_some(durable.decoder_state_version)
                    .flatten();
                let semantic_context = fact_semantic_context(adapter, instance, stream, object)?;
                let decode_binding = DurableDecodeBinding {
                    stream,
                    object_context,
                    semantic_context: &semantic_context,
                };
                let decoded = decode_record(
                    adapter,
                    &decode_binding,
                    source_access,
                    &performance,
                    &record,
                    prior_decoder_state.as_deref(),
                )?;
                if decoded.disposition == DecodeDisposition::RetryTransient {
                    outcome.retries_required = outcome.retries_required.saturating_add(1);
                    return Ok(());
                }
                let mut capability_quarantined_records = BTreeMap::new();
                increment_capability_quarantines(
                    &mut capability_quarantined_records,
                    &decoded.diagnostic_coverage_gaps,
                );
                let quarantined_records = u32::from(decoded.quarantined);
                let unscoped_quarantined_records = u32::from(decoded.unscoped_permanent_diagnostic);
                let request = commit_request(
                    adapter,
                    instance,
                    stream,
                    object,
                    object_context,
                    durable.expected(),
                    checkpoint.generation,
                    checkpoint.cursor().into_bytes(),
                    Some(checkpoint.encode()),
                    decoded.next_decoder_state.clone().or(prior_decoder_state),
                    decoded
                        .next_decoder_state
                        .as_ref()
                        .map(|_| adapter.manifest().contract_version)
                        .or(prior_decoder_state_version),
                    if removed { "absent" } else { "active" },
                    decoded.errors,
                    reason,
                    started_at,
                )?;
                self.commit_facts_on(request, decoded.batch, lane)?;
                outcome.records_decoded = outcome.records_decoded.saturating_add(1);
                record_quarantines(
                    outcome,
                    quarantined_records,
                    unscoped_quarantined_records,
                    capability_quarantined_records,
                );
                outcome.objects_changed = outcome.objects_changed.saturating_add(1);
                if removed {
                    outcome.objects_removed = outcome.objects_removed.saturating_add(1);
                }
                Ok(())
            }
        }
    }
}

struct DiscoveredObjects {
    available: bool,
    objects: BTreeMap<Vec<u8>, DeclaredObject>,
}

#[derive(Clone)]
struct DeclaredObject {
    path: PathBuf,
    descriptor: SourceObjectDescriptor,
    metadata: Option<Metadata>,
}

struct ObjectWork {
    stream: StreamSpec,
    object: DeclaredObject,
    previous: Option<SourceCatalogObject>,
    force_replay: bool,
}

#[derive(Default)]
struct DiscoveryIndex {
    roots: BTreeMap<PathBuf, RootDiscovery>,
}

struct RootDiscovery {
    available: bool,
    files: Vec<DiscoveredFile>,
}

struct DiscoveredFile {
    path: PathBuf,
    metadata: Option<Metadata>,
}

impl DiscoveryIndex {
    fn preload(
        &mut self,
        instance: &SourceInstance,
        streams: &[StreamSpec],
        cancellations: &[QueryCancellationToken],
    ) -> Result<(), EngineError> {
        let mut roots = streams
            .iter()
            .filter(|stream| !matches!(stream.driver, DriverSpec::DirectorySnapshot(_)))
            .map(|stream| {
                instance
                    .root(&stream.selector.root_name)
                    .map(Path::to_path_buf)
                    .map_err(|error| adapter_error("resolve discovery root", error))
            })
            .collect::<Result<Vec<_>, _>>()?;
        roots.sort_by_key(|root| root.components().count());
        roots.dedup();
        let mut consolidated = Vec::<PathBuf>::new();
        for root in roots {
            if consolidated
                .iter()
                .any(|ancestor| root.starts_with(ancestor))
            {
                continue;
            }
            consolidated.push(root);
        }
        for root in consolidated {
            check_cancellations(cancellations)?;
            self.roots
                .insert(root.clone(), scan_root(&root, cancellations)?);
        }
        Ok(())
    }

    fn discover_objects(
        &mut self,
        root: &Path,
        stream: &StreamSpec,
        cancellations: &[QueryCancellationToken],
    ) -> Result<DiscoveredObjects, EngineError> {
        let mut discovery_root = self
            .roots
            .keys()
            .filter(|candidate| root.starts_with(candidate.as_path()))
            .max_by_key(|candidate| candidate.components().count())
            .cloned();
        if discovery_root.is_none() {
            self.roots
                .insert(root.to_path_buf(), scan_root(root, cancellations)?);
            discovery_root = Some(root.to_path_buf());
        }
        let discovery_root = discovery_root.expect("discovery root was resolved above");
        let root_discovery = self
            .roots
            .get(&discovery_root)
            .expect("root discovery was inserted above");
        if !root_discovery.available {
            return Ok(DiscoveredObjects {
                available: false,
                objects: BTreeMap::new(),
            });
        }

        let patterns = SelectorPatterns::new(stream)?;
        let mut objects = BTreeMap::new();
        for file in &root_discovery.files {
            check_cancellations(cancellations)?;
            let Ok(relative_path) = file.path.strip_prefix(root) else {
                continue;
            };
            if !patterns.matches(relative_path) {
                continue;
            }
            let object_key = confined_relative_path_key(relative_path).map_err(source_error)?;
            let descriptor = SourceObjectDescriptor {
                stream_id: stream.id.clone(),
                object_key: object_key.clone(),
                relative_path: relative_path.to_path_buf(),
            };
            if objects
                .insert(
                    object_key,
                    DeclaredObject {
                        path: file.path.clone(),
                        descriptor,
                        metadata: file.metadata.clone(),
                    },
                )
                .is_some()
            {
                return Err(observation_error(
                    "enumerate source objects",
                    "two paths resolved to the same binary object key",
                ));
            }
        }
        Ok(DiscoveredObjects {
            available: true,
            objects,
        })
    }
}

fn scan_root(
    root: &Path,
    cancellations: &[QueryCancellationToken],
) -> Result<RootDiscovery, EngineError> {
    check_cancellations(cancellations)?;
    let root_metadata = match std::fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RootDiscovery {
                available: false,
                files: Vec::new(),
            });
        }
        Err(error) => {
            return Err(io_observation_error(
                "read source root metadata",
                root,
                error,
            ))
        }
    };
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err(observation_error(
            "validate source root",
            format!("{} is not a real directory", root.to_string_lossy()),
        ));
    }
    let mut files = Vec::new();
    let mut entries = 0_usize;
    for entry in WalkDir::new(root)
        .min_depth(1)
        .max_depth(DISCOVERY_MAX_DEPTH + 1)
        .follow_links(false)
    {
        check_cancellations(cancellations)?;
        let entry = entry.map_err(|error| {
            observation_error(
                "enumerate source root",
                format!("{}: {error}", root.to_string_lossy()),
            )
        })?;
        if entry.depth() > DISCOVERY_MAX_DEPTH {
            return Err(observation_error(
                "enumerate source root",
                format!(
                    "{} exceeds the {DISCOVERY_MAX_DEPTH}-component discovery depth",
                    root.display()
                ),
            ));
        }
        entries = entries.saturating_add(1);
        if entries > DISCOVERY_MAX_ENTRIES {
            return Err(observation_error(
                "enumerate source root",
                format!("{} exceeds {DISCOVERY_MAX_ENTRIES} entries", root.display()),
            ));
        }
        if !entry.file_type().is_file() {
            continue;
        }
        entry.path().strip_prefix(root).map_err(|_| {
            observation_error(
                "confine source object",
                entry.path().to_string_lossy().into_owned(),
            )
        })?;
        files.push(DiscoveredFile {
            path: entry.path().to_path_buf(),
            metadata: entry.metadata().ok(),
        });
    }
    Ok(RootDiscovery {
        available: true,
        files,
    })
}

fn check_cancellations(cancellations: &[QueryCancellationToken]) -> Result<(), EngineError> {
    if cancellations
        .iter()
        .any(QueryCancellationToken::is_cancelled)
    {
        Err(EngineError::QueryCancelled)
    } else {
        Ok(())
    }
}

#[derive(Debug)]
pub(super) struct SelectorPatterns {
    include: Vec<GlobPattern>,
    exclude: Vec<GlobPattern>,
}

impl SelectorPatterns {
    pub(super) fn new(stream: &StreamSpec) -> Result<Self, EngineError> {
        let compile = |pattern: &String| {
            GlobPattern::new(pattern).map_err(|detail| {
                observation_error(
                    "compile object selector",
                    format!("stream {} pattern {pattern:?}: {detail}", stream.id),
                )
            })
        };
        Ok(Self {
            include: stream
                .selector
                .include
                .iter()
                .map(compile)
                .collect::<Result<_, _>>()?,
            exclude: stream
                .selector
                .exclude
                .iter()
                .map(compile)
                .collect::<Result<_, _>>()?,
        })
    }

    pub(super) fn matches(&self, path: &Path) -> bool {
        self.include
            .iter()
            .any(|pattern| pattern.matches_path(path))
            && !self
                .exclude
                .iter()
                .any(|pattern| pattern.matches_path(path))
    }
}

struct DurableObject {
    source_instance_id: u64,
    source_stream_id: u64,
    source_object_id: u64,
    generation: u64,
    committed_cursor: Vec<u8>,
    adapter_object_context: Option<Vec<u8>>,
    driver_checkpoint: Option<Vec<u8>>,
    driver_checkpoint_version: Option<u32>,
    decoder_state: Option<Vec<u8>>,
    decoder_state_version: Option<u32>,
    retry_state: Option<Vec<u8>>,
    decoder_contract_version: u32,
    state: String,
    unpersisted: bool,
}

impl DurableObject {
    fn from_catalog(source_instance_id: u64, object: &SourceCatalogObject) -> Self {
        Self {
            source_instance_id,
            source_stream_id: object.source_stream_id,
            source_object_id: object.source_object_id,
            generation: object.generation,
            committed_cursor: object.committed_cursor.clone(),
            adapter_object_context: object.adapter_object_context.clone(),
            driver_checkpoint: object.driver_checkpoint.clone(),
            driver_checkpoint_version: object.driver_checkpoint_version,
            decoder_state: object.decoder_state.clone(),
            decoder_state_version: object.decoder_state_version,
            retry_state: object.retry_state.clone(),
            decoder_contract_version: object.decoder_contract_version,
            state: object.state.clone(),
            unpersisted: false,
        }
    }

    fn expected(&self) -> ExpectedSourceCursor {
        if self.unpersisted {
            ExpectedSourceCursor::Absent
        } else {
            ExpectedSourceCursor::At {
                generation: self.generation,
                committed_cursor: self.committed_cursor.clone(),
            }
        }
    }

    fn mark_persisted(&mut self) {
        self.unpersisted = false;
    }

    fn advance(&mut self, checkpoint: &AppendCheckpoint, encoded: Vec<u8>) {
        self.generation = checkpoint.generation;
        self.committed_cursor = checkpoint.cursor().into_bytes();
        self.driver_checkpoint = Some(encoded);
        self.unpersisted = false;
    }

    fn decoder_contract_changed<A: AgentAdapter + ?Sized>(&self, adapter: &A) -> bool {
        self.decoder_contract_version != adapter.manifest().contract_version
    }

    fn object_context_changed(&self, context: &AdapterObjectContext) -> bool {
        self.adapter_object_context.as_deref() != Some(context.payload())
    }

    fn validate_driver_checkpoint(&self, stream: &StreamSpec) -> Result<(), EngineError> {
        let Some(version) = self.driver_checkpoint_version else {
            return Ok(());
        };
        let supported = match stream.driver {
            DriverSpec::AppendDelimited(_) | DriverSpec::Presence(_) => version == 1,
            DriverSpec::ReplaceDocument(_) => matches!(version, 1 | 2),
            DriverSpec::DirectorySnapshot(_)
            | DriverSpec::SqliteSnapshot(_)
            | DriverSpec::KeyValueSnapshot(_) => version == 1,
        };
        if supported {
            Ok(())
        } else {
            Err(observation_error(
                "hydrate driver checkpoint",
                format!(
                    "stream {} has unsupported {} checkpoint version {version}",
                    stream.id,
                    stream.driver.kind()
                ),
            ))
        }
    }
}

fn rebase_checkpointless_recreation(
    durable: &DurableObject,
    previous: Option<&ReplaceCheckpoint>,
    checkpoint_generation: &mut u64,
    record_generation: &mut u64,
) -> Result<(), EngineError> {
    if durable.state == "absent" && previous.is_none() {
        let generation = next_object_generation(durable, "resume recreated replace document")?;
        *checkpoint_generation = generation;
        *record_generation = generation;
    }
    Ok(())
}

fn rebase_database_recreation(
    durable: &DurableObject,
    checkpointless: bool,
    checkpoint_generation: &mut u64,
    records: &mut [SourceRecord],
) -> Result<(), EngineError> {
    if durable.state == "absent" && checkpointless {
        let generation = next_object_generation(durable, "resume recreated database snapshot")?;
        *checkpoint_generation = generation;
        for record in records {
            record.generation = generation;
        }
    }
    Ok(())
}

fn next_object_generation(
    durable: &DurableObject,
    operation: &'static str,
) -> Result<u64, EngineError> {
    durable
        .generation
        .checked_add(1)
        .ok_or_else(|| observation_error(operation, "generation overflow"))
}

struct DecodedRecord {
    disposition: DecodeDisposition,
    batch: FactBatch,
    errors: Vec<SourceRecordError>,
    next_decoder_state: Option<Vec<u8>>,
    quarantined: bool,
    unscoped_permanent_diagnostic: bool,
    diagnostic_coverage_gaps: Vec<CapabilityId>,
}

struct DecodedSnapshot {
    batch: FactBatch,
    errors: Vec<SourceRecordError>,
    next_decoder_state: Option<Vec<u8>>,
    quarantined_records: u32,
    unscoped_quarantined_records: u32,
    capability_quarantined_records: BTreeMap<String, u32>,
}

fn source_record_volume(record: &SourceRecord) -> (u64, u64) {
    (1, u64::try_from(record.payload.len()).unwrap_or(u64::MAX))
}

fn source_records_volume(records: &[SourceRecord]) -> (u64, u64) {
    records.iter().fold((0_u64, 0_u64), |total, record| {
        let current = source_record_volume(record);
        (
            total.0.saturating_add(current.0),
            total.1.saturating_add(current.1),
        )
    })
}

fn append_read_volume(read: &AppendRead) -> (bool, bool, u64, u64) {
    match read {
        AppendRead::RetryTransient => (true, false, 0, 0),
        AppendRead::Batch {
            items,
            needs_retry,
            more_available,
            ..
        } => {
            let (records, bytes) = items.iter().fold((0_u64, 0_u64), |total, item| {
                let current = match item {
                    AppendItem::Record(record) => source_record_volume(record),
                    AppendItem::Quarantined(quarantine) => (1, quarantine.payload_len),
                };
                (
                    total.0.saturating_add(current.0),
                    total.1.saturating_add(current.1),
                )
            });
            (
                *needs_retry && !*more_available,
                *more_available,
                records,
                bytes,
            )
        }
        AppendRead::Missing => (false, false, 0, 0),
    }
}

fn replace_read_volume(read: &ReplaceRead) -> (bool, bool, u64, u64) {
    match read {
        ReplaceRead::RetryTransient => (true, false, 0, 0),
        ReplaceRead::Record { record, .. } | ReplaceRead::Removed { record, .. } => {
            let (records, bytes) = source_record_volume(record);
            (false, false, records, bytes)
        }
        ReplaceRead::Quarantined { quarantine, .. } => (false, false, 1, quarantine.payload_len),
        ReplaceRead::Missing | ReplaceRead::Unchanged { .. } => (false, false, 0, 0),
    }
}

fn sqlite_read_volume(read: &SqliteRead) -> (bool, bool, u64, u64) {
    match read {
        SqliteRead::RetryTransient => (true, false, 0, 0),
        SqliteRead::Snapshot { records, .. } => {
            let (records, bytes) = source_records_volume(records);
            (false, false, records, bytes)
        }
        SqliteRead::Missing | SqliteRead::Unchanged { .. } => (false, false, 0, 0),
    }
}

fn key_value_read_volume(read: &KeyValueRead) -> (bool, bool, u64, u64) {
    match read {
        KeyValueRead::RetryTransient => (true, false, 0, 0),
        KeyValueRead::Snapshot { records, .. } => {
            let (records, bytes) = source_records_volume(records);
            (false, false, records, bytes)
        }
        KeyValueRead::Missing | KeyValueRead::Unchanged { .. } => (false, false, 0, 0),
    }
}

fn presence_read_volume(read: &PresenceRead) -> (bool, bool, u64, u64) {
    match read {
        PresenceRead::RetryTransient => (true, false, 0, 0),
        PresenceRead::Observation { record, .. } => {
            let (records, bytes) = source_record_volume(record);
            (false, false, records, bytes)
        }
        PresenceRead::Unchanged { .. } => (false, false, 0, 0),
    }
}

struct DurableDecodeBinding<'a> {
    stream: &'a StreamSpec,
    object_context: &'a AdapterObjectContext,
    semantic_context: &'a FactSemanticContext,
}

fn decode_snapshot_records<A: AgentAdapter + ?Sized>(
    adapter: &A,
    binding: &DurableDecodeBinding<'_>,
    source_access: &ConfinedSourceAccess<'_>,
    performance: &SourcePerformanceRecorder,
    records: &[SourceRecord],
    mut decoder_state: Option<Vec<u8>>,
) -> Result<Option<DecodedSnapshot>, EngineError> {
    let mut batch = FactBatch::new(FACT_BATCH_LIMIT, DIAGNOSTIC_LIMIT)
        .map_err(|error| adapter_error("create database snapshot fact batch", error))?;
    let mut errors = Vec::new();
    let mut quarantined_records = 0_u32;
    let mut unscoped_quarantined_records = 0_u32;
    let mut capability_quarantined_records = BTreeMap::new();
    for record in records {
        let decoded = decode_record(
            adapter,
            binding,
            source_access,
            performance,
            record,
            decoder_state.as_deref(),
        )?;
        if decoded.disposition == DecodeDisposition::RetryTransient {
            return Ok(None);
        }
        if let Some(next) = decoded.next_decoder_state {
            decoder_state = Some(next);
        }
        errors.extend(decoded.errors);
        if decoded.quarantined {
            quarantined_records = quarantined_records.saturating_add(1);
        }
        if decoded.unscoped_permanent_diagnostic {
            unscoped_quarantined_records = unscoped_quarantined_records.saturating_add(1);
        }
        increment_capability_quarantines(
            &mut capability_quarantined_records,
            &decoded.diagnostic_coverage_gaps,
        );
        batch
            .append(decoded.batch)
            .map_err(|error| adapter_error("merge database snapshot facts", error))?;
    }
    Ok(Some(DecodedSnapshot {
        batch,
        errors,
        next_decoder_state: decoder_state,
        quarantined_records,
        unscoped_quarantined_records,
        capability_quarantined_records,
    }))
}

fn source_instance_spec(
    manifest: &AdapterManifest,
    spec: &AdapterSourceInstanceSpec,
    discovered_at: i64,
    last_seen_at: i64,
) -> SourceInstanceSpec {
    SourceInstanceSpec {
        adapter_id: manifest.id.as_str().to_string(),
        stable_key: spec.stable_key.as_bytes().to_vec(),
        display_name: spec.display_name.clone(),
        adapter_version: manifest.adapter_version.clone(),
        adapter_contract_version: manifest.contract_version,
        source_schema_versions: manifest.source_schema_versions.clone(),
        capabilities: manifest
            .capabilities
            .iter()
            .map(|capability| SourceCapabilitySpec {
                id: capability.id.as_str().to_string(),
                support_level: support_level_name(capability.support.level).to_string(),
                granularity: capability_granularity_name(&capability.support.granularity),
                availability: availability_name(capability.support.availability).to_string(),
                notes: capability.support.notes.clone(),
            })
            .collect(),
        discovered_at,
        last_seen_at,
    }
}

fn support_level_name(level: SupportLevel) -> &'static str {
    match level {
        SupportLevel::Native => "native",
        SupportLevel::Derived => "derived",
        SupportLevel::Estimated => "estimated",
        SupportLevel::Unsupported => "unsupported",
    }
}

fn capability_granularity_name(granularity: &CapabilityGranularity) -> String {
    match granularity {
        CapabilityGranularity::Record => "record".to_string(),
        CapabilityGranularity::Message => "message".to_string(),
        CapabilityGranularity::Turn => "turn".to_string(),
        CapabilityGranularity::Run => "run".to_string(),
        CapabilityGranularity::Session => "session".to_string(),
        CapabilityGranularity::Team => "team".to_string(),
        CapabilityGranularity::Project => "project".to_string(),
        CapabilityGranularity::Instance => "instance".to_string(),
        CapabilityGranularity::Custom(value) => format!("custom:{value}"),
    }
}

fn availability_name(availability: Availability) -> &'static str {
    match availability {
        Availability::Live => "live",
        Availability::EventuallyLive => "eventually_live",
        Availability::CompletionOnly => "completion_only",
        Availability::BackfillOnly => "backfill_only",
    }
}

fn decode_record<A: AgentAdapter + ?Sized>(
    adapter: &A,
    binding: &DurableDecodeBinding<'_>,
    source_access: &ConfinedSourceAccess<'_>,
    performance: &SourcePerformanceRecorder,
    record: &SourceRecord,
    decoder_state: Option<&[u8]>,
) -> Result<DecodedRecord, EngineError> {
    let started = Instant::now();
    let attempt = decode_adapter_record(DecodeRuntimeRequest {
        adapter,
        decoder: &binding.stream.decoder,
        object_context: binding.object_context,
        source_access,
        record,
        semantic_context: binding.semantic_context,
        decoder_state,
        retention: binding.stream.retention,
        limits: DecodeRuntimeLimits {
            max_facts: FACT_BATCH_LIMIT,
            max_diagnostics: DIAGNOSTIC_LIMIT,
        },
    });
    let adapter_elapsed = attempt.adapter_elapsed;
    let fact_build_time = attempt.fact_build_time;
    let decoded = (|| {
        let mut decoded = attempt
            .result
            .map_err(|error| adapter_error("decode source record", error))?;
        for dependency in source_access.revisions()? {
            decoded
                .batch
                .add_dependency_read(dependency)
                .map_err(|error| adapter_error("record source dependency", error))?;
        }
        let errors = decoded
            .batch
            .diagnostics()
            .iter()
            .map(|diagnostic| SourceRecordError {
                generation: record.generation,
                cursor_start: record.cursor_start.as_bytes().to_vec(),
                cursor_end: record.cursor_end.as_bytes().to_vec(),
                payload_hash: record.payload_hash.as_bytes().to_vec(),
                media_type: record.media_type.as_str().to_string(),
                raw_payload: retained_diagnostic_payload(binding.stream.retention, &record.payload),
                error_class: adapter_error_class(diagnostic.class).to_string(),
                error_message: format!("{}: {}", diagnostic.code, diagnostic.message),
                adapter_version: adapter.manifest().adapter_version.clone(),
                contract_version: adapter.manifest().contract_version,
                last_retry_at: None,
            })
            .collect();
        Ok((decoded, errors))
    })();
    let outcome = match &decoded {
        Ok((decoded, _)) if decoded.disposition == DecodeDisposition::RetryTransient => {
            SourceDecodeOutcome::Retry
        }
        Ok((decoded, _)) => SourceDecodeOutcome::Decoded {
            facts: u64::try_from(decoded.batch.facts().len()).unwrap_or(u64::MAX),
            quarantined: decoded.quarantined,
        },
        Err(_) => SourceDecodeOutcome::Failed,
    };
    performance.record_decode(&SourceDecodeObservation {
        elapsed: started.elapsed(),
        adapter_elapsed,
        fact_build: fact_build_time,
        outcome,
    });
    let (decoded, errors) = decoded?;
    Ok(DecodedRecord {
        disposition: decoded.disposition,
        batch: decoded.batch,
        errors,
        next_decoder_state: decoded.next_decoder_state,
        quarantined: decoded.quarantined,
        unscoped_permanent_diagnostic: decoded.unscoped_permanent_diagnostic,
        diagnostic_coverage_gaps: decoded.diagnostic_coverage_gaps,
    })
}

#[allow(clippy::too_many_arguments)]
fn commit_request<A: AgentAdapter + ?Sized>(
    adapter: &A,
    instance: &SourceInstance,
    stream: &StreamSpec,
    object: &DeclaredObject,
    object_context: &AdapterObjectContext,
    expected: ExpectedSourceCursor,
    generation: u64,
    committed_cursor: Vec<u8>,
    driver_checkpoint: Option<Vec<u8>>,
    decoder_state: Option<Vec<u8>>,
    decoder_state_version: Option<u32>,
    state: &str,
    record_errors: Vec<SourceRecordError>,
    reason: &str,
    started_at: i64,
) -> Result<ObservationCommit, EngineError> {
    let committed_at = now_unix_ms()?.max(started_at);
    // Metadata captured during no-follow discovery is advisory. Never reopen
    // the path here: it may have been replaced after the confined content read.
    let metadata = object.metadata.as_ref();
    Ok(ObservationCommit {
        source: source_instance_spec(adapter.manifest(), &instance.spec, started_at, committed_at),
        stream: SourceStreamSpec {
            stream_key: stream.id.as_str().to_string(),
            driver_kind: stream.driver.kind().to_string(),
            decoder_key: stream.decoder.as_str().to_string(),
            stream_state: "available".to_string(),
            last_reconciled_at: Some(committed_at),
            consistency: stream.consistency,
            retention: stream.retention,
        },
        object: SourceObjectUpdate {
            object_key: object.descriptor.object_key.clone(),
            expected,
            display_path: Some(
                object
                    .descriptor
                    .relative_path
                    .to_string_lossy()
                    .into_owned(),
            ),
            native_identity: None,
            generation,
            committed_cursor,
            observed_revision: None,
            adapter_object_context: Some(object_context.payload().to_vec()),
            driver_checkpoint_version: driver_checkpoint
                .as_ref()
                .map(|_| driver_checkpoint_version(&stream.driver)),
            driver_checkpoint,
            decoder_state,
            decoder_state_version,
            retry_state: None,
            size_bytes: metadata.map(Metadata::len),
            mtime_ns: metadata.and_then(modified_ns),
            decoder_contract_version: adapter.manifest().contract_version,
            state: state.to_string(),
        },
        reason: reason.to_string(),
        started_at,
        committed_at,
        fact_count: 0,
        projection_versions: pending_projection_updates(
            adapter.manifest(),
            stream,
            instance,
            reason,
        ),
        record_errors,
        changes: Vec::new(),
    })
}

fn declares_usage_v2_projection(manifest: &AdapterManifest) -> bool {
    manifest.capabilities.iter().any(|capability| {
        capability.id.as_str() == USAGE_V2_PROJECTION_ID
            && capability.support.level != SupportLevel::Unsupported
    })
}

fn stream_declares_usage_v2_projection(stream: &StreamSpec) -> bool {
    stream
        .capabilities
        .iter()
        .any(|capability| capability.as_str() == USAGE_V2_PROJECTION_ID)
}

fn usage_v2_projection_update(
    instance: &SourceInstance,
    readiness: ProjectionReadiness,
    detail: Option<&str>,
) -> ProjectionVersionUpdate {
    ProjectionVersionUpdate {
        projection_id: USAGE_V2_PROJECTION_ID.to_string(),
        scope_key: instance.spec.stable_key.as_bytes().to_vec(),
        desired_version: USAGE_V2_PROJECTION_VERSION,
        completed_version: (readiness == ProjectionReadiness::Ready)
            .then_some(USAGE_V2_PROJECTION_VERSION),
        readiness,
        detail: detail.map(str::to_string),
    }
}

fn pending_projection_updates(
    manifest: &AdapterManifest,
    stream: &StreamSpec,
    instance: &SourceInstance,
    reason: &str,
) -> Vec<ProjectionVersionUpdate> {
    (declares_usage_v2_projection(manifest) && stream_declares_usage_v2_projection(stream))
        .then(|| {
            usage_v2_projection_update(
                instance,
                ProjectionReadiness::Pending,
                Some(if reason == USAGE_V2_REPLAY_COMMIT_REASON {
                    USAGE_V2_REPLAY_PENDING_DETAIL
                } else {
                    "source reconciliation in progress"
                }),
            )
        })
        .into_iter()
        .collect()
}

fn coverage_position_kind(driver: &DriverSpec) -> CoveragePositionKind {
    match driver {
        DriverSpec::AppendDelimited(_) => CoveragePositionKind::AppendCursor,
        DriverSpec::ReplaceDocument(_) | DriverSpec::Presence(_) => {
            CoveragePositionKind::DocumentRevision
        }
        DriverSpec::DirectorySnapshot(_) => CoveragePositionKind::SnapshotRevision,
        DriverSpec::SqliteSnapshot(_) => CoveragePositionKind::DatabaseWatermark,
        DriverSpec::KeyValueSnapshot(_) => CoveragePositionKind::KeyRangeToken,
    }
}

fn usage_v2_coverage_set_update(
    manifest: &AdapterManifest,
    instance: &SourceInstance,
    catalog: &SourceCatalogSnapshot,
    attempt: &ProjectionCoverageAttempt,
    incomplete_detail: Option<&str>,
) -> Result<DurableCoverageSetUpdate, EngineError> {
    let support = manifest.support_binding.as_ref().ok_or_else(|| {
        observation_error(
            "build usage-v2 coverage",
            "a typed fact-family coverage set requires a digest-bound support release",
        )
    })?;
    let source_instance_key = CanonicalSourceInstanceKey::derive(
        instance.spec.identity_contract_version,
        instance.spec.stable_key.as_bytes(),
    )
    .map_err(|error| observation_error("build usage-v2 coverage", error.to_string()))?;
    // The support binding contains the verified SHA-256 declaration digest,
    // not a second copy of the declaration bytes. RFC 012A's opaque coverage
    // digest therefore binds that verified digest as its stable input.
    let declaration_digest =
        CoverageDeclarationDigest::derive(support.source_declaration_digest().as_bytes())
            .map_err(|error| observation_error("build usage-v2 coverage", error.to_string()))?;

    let domain = CoverageDomain::FactFamily {
        family: USAGE_V2_PROJECTION_ID.to_string(),
        version: USAGE_V2_PROJECTION_VERSION,
    };
    // Once a decode gap has quarantined evidence, later cursor progress cannot
    // prove the missing fact-family interval. Keep the normalized set stable
    // until the explicit replay path starts a replacement attempt. In
    // particular, the first quarantine and the following unchanged poll must
    // not alternate between a per-record error and a generic sticky error.
    let replay_gap = incomplete_detail.is_some_and(|detail| detail.contains("records_quarantined"));
    let membership_streams = attempt
        .required_streams
        .keys()
        .map(|stream| stream.as_bytes())
        .collect::<Vec<_>>();
    let mut membership_objects = catalog
        .objects
        .iter()
        .filter(|object| attempt.required_streams.contains_key(&object.stream_key))
        .map(|object| CoverageMembershipObject {
            stream_key: object.stream_key.as_bytes(),
            object_key: &object.object_key,
            generation: object.generation,
            absent: object.state == "absent",
        })
        .collect::<Vec<_>>();
    membership_objects.sort_by(|left, right| {
        (left.stream_key, left.object_key, left.generation).cmp(&(
            right.stream_key,
            right.object_key,
            right.generation,
        ))
    });
    let membership_prefix = source_membership_prefix(&domain)
        .map_err(|error| observation_error("build usage-v2 coverage", error.to_string()))?;
    let membership_revision = derive_coverage_membership_revision(
        &membership_prefix,
        &membership_streams,
        &membership_objects,
    )
    .map_err(|error| observation_error("build usage-v2 coverage", error.to_string()))?;
    let mut points = Vec::new();
    let mut absences = Vec::new();
    for object in catalog
        .objects
        .iter()
        .filter(|object| attempt.required_streams.contains_key(&object.stream_key))
    {
        let stream_key =
            CoverageStreamKey::derive(manifest.id.as_str(), object.stream_key.as_bytes())
                .map_err(|error| observation_error("build usage-v2 coverage", error.to_string()))?;
        let object_key = CoverageObjectKey::derive(&object.stream_key, &object.object_key)
            .map_err(|error| observation_error("build usage-v2 coverage", error.to_string()))?;
        if object.state == "absent" {
            absences.push(CoverageAbsence {
                stream_key,
                object_key,
                generation: object.generation,
                kind: CoverageAbsenceKind::Absent,
            });
            continue;
        }

        let kind = *attempt
            .required_streams
            .get(&object.stream_key)
            .expect("provider object was filtered by the required stream map");
        let cursor =
            SourceCursor::from_opaque(object.committed_cursor.clone()).map_err(source_error)?;
        let position = CoveragePosition::derive(
            kind,
            &object.committed_cursor,
            (kind == CoveragePositionKind::AppendCursor)
                .then(|| cursor.append_offset_value())
                .flatten(),
        )
        .map_err(|error| observation_error("build usage-v2 coverage", error.to_string()))?;
        let status = if replay_gap {
            CoverageStatus::Unavailable {
                reason: "coverage_gap_requires_explicit_replay".to_string(),
            }
        } else {
            match object.state.as_str() {
                "active" if kind == CoveragePositionKind::AppendCursor => {
                    CoverageStatus::CompleteThrough
                }
                "active" => CoverageStatus::ExactSnapshot,
                "retrying" | "pending" => CoverageStatus::Partial,
                "quarantined" => CoverageStatus::Unavailable {
                    reason: "durable_quarantine".to_string(),
                },
                other => CoverageStatus::Unavailable {
                    reason: format!("unknown_source_state_{other}"),
                },
            }
        };
        points.push(
            SourceCoveragePoint::new(
                domain.clone(),
                manifest.id.as_str(),
                source_instance_key,
                stream_key,
                object_key,
                object.generation,
                Some(position),
                status,
                CoverageProvenance::default(),
            )
            .map_err(|error| observation_error("build usage-v2 coverage", error.to_string()))?,
        );
    }
    points.sort_by_key(|point| (point.stream_key, point.object_key, point.generation));
    absences.sort();

    let mut errors = Vec::new();
    for (stream, blockers) in &attempt.blockers {
        let stream_key = CoverageStreamKey::derive(manifest.id.as_str(), stream.as_bytes())
            .map_err(|error| observation_error("build usage-v2 coverage", error.to_string()))?;
        for blocker in blockers {
            errors.push(CoverageError {
                stream_key: Some(stream_key),
                object_key: None,
                code: normalized_coverage_blocker(blocker).to_string(),
            });
        }
    }
    if replay_gap {
        errors.retain(|error| error.code != "durable_quarantine");
        errors.push(CoverageError {
            stream_key: None,
            object_key: None,
            code: "coverage_gap_requires_explicit_replay".to_string(),
        });
    } else if incomplete_detail.is_some() && errors.is_empty() {
        errors.push(CoverageError {
            stream_key: None,
            object_key: None,
            code: "coverage_incomplete".to_string(),
        });
    }
    errors.sort();
    errors.dedup();

    let completeness = if incomplete_detail.is_none() {
        CoverageSetCompleteness::Complete
    } else {
        CoverageSetCompleteness::Unavailable
    };
    let set = SourceCoverageSet::new(
        domain,
        CoverageScope {
            adapter_id: manifest.id.as_str().to_string(),
            source_instance_key,
            root_entity_key: None,
            support_release_id: support.support_release_id().to_string(),
            source_or_scope_declaration_digest: declaration_digest,
        },
        membership_revision,
        points,
        absences,
        errors,
        completeness,
    )
    .map_err(|error| observation_error("build usage-v2 coverage", error.to_string()))?;
    Ok(DurableCoverageSetUpdate {
        owner_id: USAGE_V2_PROJECTION_ID.to_string(),
        owner_scope_key: instance.spec.stable_key.as_bytes().to_vec(),
        set,
    })
}

fn normalized_coverage_blocker(blocker: &str) -> &str {
    match blocker {
        "records_quarantined" => "durable_quarantine",
        "retries_required" => "durable_retry",
        other => other,
    }
}

fn coverage_detail_only_quarantine(detail: &str) -> bool {
    let Some((_, streams)) = detail.split_once(':') else {
        return false;
    };
    let mut found = false;
    for blocker in streams.split(',').flat_map(|stream| {
        stream
            .split_once('=')
            .map_or("", |(_, blockers)| blockers)
            .split('+')
    }) {
        if !matches!(blocker.trim(), "durable_quarantine" | "records_quarantined") {
            return false;
        }
        found = true;
    }
    found
}

fn previous_display_relative(object: &SourceCatalogObject, root: &Path) -> Option<PathBuf> {
    let display = object.display_path.as_deref()?;
    let relative = PathBuf::from(display);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::RootDir))
    {
        return None;
    }
    let path = root.join(&relative);
    (confined_relative_path_key(&relative).ok().as_deref() == Some(&object.object_key)
        && !path.exists())
    .then_some(relative)
}

fn previous_display_path(object: &SourceCatalogObject) -> Result<PathBuf, EngineError> {
    let display = object.display_path.as_deref().ok_or_else(|| {
        observation_error("resume retry object", "source object has no display path")
    })?;
    let relative = PathBuf::from(display);
    let path_key = confined_relative_path_key(&relative).map_err(source_error)?;
    if path_key != object.object_key {
        return Err(observation_error(
            "resume retry object",
            "source object display path no longer matches its binary key",
        ));
    }
    Ok(relative)
}

fn confined_metadata(root: &Path, display_path: Option<&str>) -> Option<Metadata> {
    let relative = PathBuf::from(display_path?);
    let metadata = std::fs::symlink_metadata(root.join(relative)).ok()?;
    (!metadata.file_type().is_symlink() && metadata.is_file()).then_some(metadata)
}

fn initial_cursor_bytes(stream: &StreamSpec) -> Vec<u8> {
    match stream.driver {
        DriverSpec::AppendDelimited(_) => SourceCursor::append_offset(0).into_bytes(),
        DriverSpec::ReplaceDocument(_) => SourceCursor::snapshot(Revision::ZERO).into_bytes(),
        DriverSpec::Presence(_) => SourceCursor::presence(Revision::ZERO).into_bytes(),
        DriverSpec::DirectorySnapshot(_) => SourceCursor::directory(Revision::ZERO).into_bytes(),
        DriverSpec::SqliteSnapshot(_) => SourceCursor::sqlite_snapshot(Revision::ZERO).into_bytes(),
        DriverSpec::KeyValueSnapshot(_) => {
            SourceCursor::key_value_snapshot(Revision::ZERO).into_bytes()
        }
    }
}

fn driver_checkpoint_version(driver: &DriverSpec) -> u32 {
    match driver {
        DriverSpec::AppendDelimited(_) | DriverSpec::Presence(_) => 1,
        DriverSpec::ReplaceDocument(_) => 2,
        DriverSpec::DirectorySnapshot(_)
        | DriverSpec::SqliteSnapshot(_)
        | DriverSpec::KeyValueSnapshot(_) => 1,
    }
}

fn fact_semantic_context<A: AgentAdapter + ?Sized>(
    adapter: &A,
    instance: &SourceInstance,
    stream: &StreamSpec,
    object: &DeclaredObject,
) -> Result<FactSemanticContext, EngineError> {
    FactSemanticContext::new(
        &adapter.manifest().id,
        instance.spec.identity_contract_version,
        instance.spec.stable_key.as_bytes(),
        stream.id.as_str().as_bytes(),
        &object.descriptor.object_key,
        stream.driver.framing_contract_version(),
    )
    .map_err(|error| adapter_error("bind canonical fact identity", error))
}

fn media_type(path: &Path) -> Result<SourceMediaType, EngineError> {
    let value = match path.extension().and_then(OsStr::to_str) {
        Some("jsonl") => "application/x-ndjson",
        Some("json") => "application/json",
        Some("md") => "text/markdown",
        Some("txt") => "text/plain",
        Some("db" | "db3" | "sqlite" | "sqlite3") => "application/vnd.sqlite3",
        _ => "application/octet-stream",
    };
    SourceMediaType::new(value).map_err(source_error)
}

fn quarantine_error<A: AgentAdapter + ?Sized>(
    adapter: &A,
    origin: &RecordOrigin,
    quarantine: &crate::source::DriverQuarantine,
) -> SourceRecordError {
    SourceRecordError {
        generation: quarantine.generation,
        cursor_start: quarantine.cursor_start.as_bytes().to_vec(),
        cursor_end: quarantine.cursor_end.as_bytes().to_vec(),
        payload_hash: quarantine.payload_hash.as_bytes().to_vec(),
        media_type: origin.media_type.as_str().to_string(),
        raw_payload: None,
        error_class: "record_permanent".to_string(),
        error_message: quarantine.reason.clone(),
        adapter_version: adapter.manifest().adapter_version.clone(),
        contract_version: adapter.manifest().contract_version,
        last_retry_at: None,
    }
}

fn snapshot_decode_error<A: AgentAdapter + ?Sized>(
    adapter: &A,
    stream: &StreamSpec,
    record: &SourceRecord,
    error_class: &str,
    error_message: String,
    last_retry_at: Option<i64>,
) -> SourceRecordError {
    SourceRecordError {
        generation: record.generation,
        cursor_start: record.cursor_start.as_bytes().to_vec(),
        cursor_end: record.cursor_end.as_bytes().to_vec(),
        payload_hash: record.payload_hash.as_bytes().to_vec(),
        media_type: record.media_type.as_str().to_string(),
        raw_payload: retained_diagnostic_payload(stream.retention, &record.payload),
        error_class: error_class.to_string(),
        error_message,
        adapter_version: adapter.manifest().adapter_version.clone(),
        contract_version: adapter.manifest().contract_version,
        last_retry_at,
    }
}

fn retained_diagnostic_payload(retention: RawRetentionPolicy, payload: &[u8]) -> Option<Vec<u8>> {
    match retention {
        RawRetentionPolicy::None | RawRetentionPolicy::HashOnly => None,
        RawRetentionPolicy::DiagnosticExcerpt => Some(diagnostic_excerpt(payload)),
        RawRetentionPolicy::Full => Some(payload.to_vec()),
    }
}

fn dependency_snapshot(
    source_instance_id: u64,
    root_name: &str,
    root: &Path,
    relative_path: &Path,
    max_bytes: usize,
) -> Result<SourceSnapshot, SourceDriverError> {
    let object_key = confined_relative_path_key(relative_path)?;
    let (payload, revision, oversized) =
        match crate::source::read_stable_file_confined(root, relative_path, max_bytes)? {
            crate::source::StableRead::Missing => {
                (None, *Revision::missing_dependency().as_bytes(), false)
            }
            crate::source::StableRead::Unstable => {
                return Err(SourceDriverError::Unstable(
                    relative_path.to_string_lossy().into_owned(),
                ))
            }
            crate::source::StableRead::Oversized(stamp) => {
                let revision = Revision::oversized_dependency(stamp.len, stamp.modified_ns);
                (None, *revision.as_bytes(), true)
            }
            crate::source::StableRead::Stable {
                bytes, revision, ..
            } => (Some(bytes), *revision.as_bytes(), false),
        };
    Ok(SourceSnapshot {
        payload,
        revision: DependencyRevision {
            source_instance_id,
            root_name: root_name.to_string(),
            object_key,
            revision,
        },
        oversized,
    })
}

fn source_query_snapshot(
    source_instance_id: u64,
    root: &Path,
    query: &SourceQuery,
    cancellations: &[QueryCancellationToken],
) -> Result<SourceRows, SourceDriverError> {
    let dependency_key = source_query_dependency_key(query)?;
    let config = crate::source::SqliteSnapshotConfig {
        queries: vec![query.query.clone()],
        max_database_bytes: query.bounds.max_database_bytes,
        max_sidecar_bytes: query.bounds.max_sidecar_bytes,
        max_rows: query.bounds.max_rows,
        max_value_bytes: query.bounds.max_value_bytes,
        max_snapshot_bytes: query.bounds.max_snapshot_bytes,
        busy_timeout_ms: query.bounds.busy_timeout_ms,
    };
    let origin = RecordOrigin {
        source_instance_id,
        stream_id: 0,
        object_id: 0,
        observed_at: 0,
        source_timestamp_hint: None,
        media_type: SourceMediaType::new("application/vnd.sqlite3")?,
    };
    let cancellation_tokens = cancellations.to_vec();
    match SqliteSnapshot::new(config)?.read_confined(
        root,
        &query.relative_path,
        None,
        &origin,
        false,
        move || {
            cancellation_tokens
                .iter()
                .any(QueryCancellationToken::is_cancelled)
        },
    )? {
        SqliteRead::Missing => Ok(SourceRows {
            available: false,
            schema_version: None,
            rows: Vec::new(),
            revision: DependencyRevision {
                source_instance_id,
                root_name: query.root_name.clone(),
                object_key: dependency_key,
                revision: *blake3::hash(b"spaghetti/source-query/missing/v1").as_bytes(),
            },
        }),
        SqliteRead::RetryTransient => Err(SourceDriverError::Unstable(format!(
            "source query {} must be retried",
            query.query.name
        ))),
        SqliteRead::Unchanged { .. } => unreachable!("checkpointless source query"),
        SqliteRead::Snapshot {
            records,
            checkpoint,
            ..
        } => Ok(SourceRows {
            available: true,
            schema_version: Some(checkpoint.schema_version),
            rows: records
                .iter()
                .map(|record| crate::source::SqliteRowRecord::decode(&record.payload))
                .collect::<Result<Vec<_>, _>>()?,
            revision: DependencyRevision {
                source_instance_id,
                root_name: query.root_name.clone(),
                object_key: dependency_key,
                revision: *checkpoint.revision.as_bytes(),
            },
        }),
    }
}

fn source_object_listing(
    source_instance_id: u64,
    root: &Path,
    request: &SourceObjectListRequest,
    cancellations: &[QueryCancellationToken],
) -> Result<SourceObjectList, SourceDriverError> {
    let dependency_key = source_listing_dependency_key(request)?;
    let root_metadata = match std::fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(SourceObjectList {
                available: false,
                objects: Vec::new(),
                revision: DependencyRevision {
                    source_instance_id,
                    root_name: request.root_name.clone(),
                    object_key: dependency_key,
                    revision: *blake3::hash(b"spaghetti/source-listing/missing/v1").as_bytes(),
                },
            });
        }
        Err(error) => {
            return Err(SourceDriverError::Unstable(format!(
                "cannot inspect source listing root {}: {error}",
                root.display()
            )))
        }
    };
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err(SourceDriverError::PathEscape(
            root.to_string_lossy().into_owned(),
        ));
    }
    let compile = |pattern: &String| {
        GlobPattern::new(pattern).map_err(|detail| {
            SourceDriverError::InvalidConfig(format!(
                "source listing pattern {pattern:?}: {detail}"
            ))
        })
    };
    let patterns = SelectorPatterns {
        include: request
            .include
            .iter()
            .map(compile)
            .collect::<Result<_, _>>()?,
        exclude: request
            .exclude
            .iter()
            .map(compile)
            .collect::<Result<_, _>>()?,
    };
    let mut objects = Vec::new();
    let mut scanned = 0_usize;
    for entry in WalkDir::new(root)
        .min_depth(1)
        .max_depth(DISCOVERY_MAX_DEPTH + 1)
        .follow_links(false)
    {
        if cancellations
            .iter()
            .any(QueryCancellationToken::is_cancelled)
        {
            return Err(SourceDriverError::Unstable(
                "source listing was cancelled".to_string(),
            ));
        }
        let entry = entry.map_err(|error| SourceDriverError::Unstable(error.to_string()))?;
        if entry.depth() > DISCOVERY_MAX_DEPTH {
            return Err(SourceDriverError::LimitExceeded(format!(
                "source listing exceeds {DISCOVERY_MAX_DEPTH} path components"
            )));
        }
        scanned = scanned.saturating_add(1);
        if scanned > DISCOVERY_MAX_ENTRIES {
            return Err(SourceDriverError::LimitExceeded(format!(
                "source listing exceeds {DISCOVERY_MAX_ENTRIES} scanned entries"
            )));
        }
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = entry.path().strip_prefix(root).map_err(|_| {
            SourceDriverError::PathEscape(entry.path().to_string_lossy().into_owned())
        })?;
        if !patterns.matches(relative) {
            continue;
        }
        if objects.len() == request.max_entries {
            return Err(SourceDriverError::LimitExceeded(format!(
                "source listing exceeds {} selected entries",
                request.max_entries
            )));
        }
        let metadata = entry.metadata().map_err(|error| {
            SourceDriverError::Unstable(format!(
                "cannot inspect source listing entry {}: {error}",
                entry.path().display()
            ))
        })?;
        objects.push(SourceListedObject {
            object_key: confined_relative_path_key(relative)?,
            relative_path: relative.to_path_buf(),
            size_bytes: metadata.len(),
            modified_ns: modified_ns(&metadata),
        });
    }
    objects.sort_by(|left, right| left.object_key.cmp(&right.object_key));
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"spaghetti/source-listing/revision/v1\0");
    for object in &objects {
        hasher.update(&(object.object_key.len() as u64).to_be_bytes());
        hasher.update(&object.object_key);
        hasher.update(&object.size_bytes.to_be_bytes());
        hasher.update(&object.modified_ns.unwrap_or(i64::MIN).to_be_bytes());
    }
    Ok(SourceObjectList {
        available: true,
        objects,
        revision: DependencyRevision {
            source_instance_id,
            root_name: request.root_name.clone(),
            object_key: dependency_key,
            revision: *hasher.finalize().as_bytes(),
        },
    })
}

fn source_query_dependency_key(query: &SourceQuery) -> Result<Vec<u8>, SourceDriverError> {
    let path_key = confined_relative_path_key(&query.relative_path)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"spaghetti/source-query/key/v1\0");
    hasher.update(&(path_key.len() as u64).to_be_bytes());
    hasher.update(&path_key);
    hasher.update(query.query.name.as_bytes());
    hasher.update(query.query.sql.as_bytes());
    for key in &query.query.key_columns {
        hasher.update(key.as_bytes());
        hasher.update(&[0]);
    }
    Ok(hasher.finalize().as_bytes().to_vec())
}

fn source_listing_dependency_key(
    request: &SourceObjectListRequest,
) -> Result<Vec<u8>, SourceDriverError> {
    let mut include = request.include.clone();
    let mut exclude = request.exclude.clone();
    include.sort();
    exclude.sort();
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"spaghetti/source-listing/key/v1\0");
    for pattern in include {
        GlobPattern::new(&pattern).map_err(SourceDriverError::InvalidConfig)?;
        hasher.update(pattern.as_bytes());
        hasher.update(&[0]);
    }
    hasher.update(&[0xff]);
    for pattern in exclude {
        GlobPattern::new(&pattern).map_err(SourceDriverError::InvalidConfig)?;
        hasher.update(pattern.as_bytes());
        hasher.update(&[0]);
    }
    Ok(hasher.finalize().as_bytes().to_vec())
}

fn validate_source_query_bounds(query: &SourceQuery) -> Result<(), AdapterError> {
    const MAX_DATABASE_BYTES: usize = 4 * 1024 * 1024 * 1024;
    if query.root_name.trim().is_empty()
        || query.bounds.max_database_bytes == 0
        || query.bounds.max_database_bytes > MAX_DATABASE_BYTES
        || query.bounds.max_sidecar_bytes == 0
        || query.bounds.max_sidecar_bytes > MAX_DATABASE_BYTES
        || query.bounds.max_rows == 0
        || query.bounds.max_rows > 16_384
        || query.bounds.max_value_bytes == 0
        || query.bounds.max_value_bytes > ConfinedSourceAccess::MAX_READ_BYTES
        || query.bounds.max_snapshot_bytes == 0
        || query.bounds.max_snapshot_bytes > ConfinedSourceAccess::MAX_READ_BYTES
        || query.bounds.busy_timeout_ms > 30_000
    {
        return Err(AdapterError::invalid_contract(
            "source database query bounds are zero or outside engine limits",
        ));
    }
    Ok(())
}

fn source_access_cancelled() -> AdapterError {
    AdapterError::new(
        AdapterErrorClass::Transient,
        "source_access_cancelled",
        "source dependency read was cancelled",
    )
}

fn source_access_error(error: SourceDriverError) -> AdapterError {
    let class = match error {
        SourceDriverError::InvalidConfig(_)
        | SourceDriverError::InvalidCursor(_)
        | SourceDriverError::LimitExceeded(_)
        | SourceDriverError::PathEscape(_) => AdapterErrorClass::InvalidContract,
        SourceDriverError::Unstable(_)
        | SourceDriverError::Database(_)
        | SourceDriverError::Io { .. } => AdapterErrorClass::Transient,
    };
    AdapterError::new(class, "source_access_read", error.to_string())
}

fn modified_ns(metadata: &Metadata) -> Option<i64> {
    metadata
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| {
            let nanos = i128::from(duration.as_secs()) * 1_000_000_000
                + i128::from(duration.subsec_nanos());
            i64::try_from(nanos).ok()
        })
}

fn adapter_error_class(class: AdapterErrorClass) -> &'static str {
    match class {
        AdapterErrorClass::Transient => "transient",
        AdapterErrorClass::RecordPermanent => "record_permanent",
        AdapterErrorClass::StreamFatal => "stream_fatal",
        AdapterErrorClass::AdapterFatal => "adapter_fatal",
        AdapterErrorClass::InvalidContract => "invalid_contract",
    }
}

fn validate_request(request: &ReconcileRequest) -> Result<(), EngineError> {
    if request.configured_roots.is_empty() || request.reason.trim().is_empty() {
        return Err(EngineError::InvalidConfig(
            "reconcile requires at least one configured root and a reason".to_string(),
        ));
    }
    Ok(())
}

fn validate_fact_family_replay_request(
    request: &FactFamilyReplayRequest,
) -> Result<(), EngineError> {
    if request.owner_id.trim().is_empty()
        || request.owner_id.len() > 256
        || request.family.trim().is_empty()
        || request.family.len() > 256
        || request.version == 0
        || request.reason.trim().is_empty()
        || request.reason.len() > 4 * 1024
    {
        return Err(EngineError::InvalidConfig(
            "fact-family replay requires a bounded owner/family, positive version, and bounded reason"
                .to_string(),
        ));
    }
    if let Some(authorization) = &request.authorization {
        if authorization.adapter_id.trim().is_empty()
            || authorization.adapter_id.len() > 256
            || authorization.canonical_source_instance_key.len() != 32
            || authorization.content_digest.len() != 32
            || authorization.coverage_last_commit_seq == 0
        {
            return Err(EngineError::InvalidConfig(
                "fact-family replay authorization has an empty, unbounded, or invalid field"
                    .to_string(),
            ));
        }
    }
    Ok(())
}

fn record_commit(outcome: &mut ReconcileOutcome, receipt: &CommitReceipt) {
    outcome.commits = outcome.commits.saturating_add(1);
    outcome.last_commit_seq = Some(receipt.commit_seq);
}

fn settle_ready_commits(
    engine: &SpaghettiEngineCore,
    unacked: &mut Vec<crossbeam_channel::Receiver<Result<CommitReceipt, EngineError>>>,
    outcome: &mut ReconcileOutcome,
    block: bool,
) -> Result<(), EngineError> {
    if unacked.is_empty() {
        return Ok(());
    }
    if block {
        while let Some(pending) = unacked.pop() {
            let receipt = recv_commit_receipt(pending)?;
            engine.accept_commit_receipt(&receipt);
            record_commit(outcome, &receipt);
        }
        return Ok(());
    }
    let mut index = 0;
    while index < unacked.len() {
        match unacked[index].try_recv() {
            Ok(result) => {
                let receipt = result?;
                engine.accept_commit_receipt(&receipt);
                record_commit(outcome, &receipt);
                unacked.swap_remove(index);
            }
            Err(crossbeam_channel::TryRecvError::Empty) => index += 1,
            Err(crossbeam_channel::TryRecvError::Disconnected) => {
                unacked.swap_remove(index);
                return Err(EngineError::WorkerUnavailable { worker: "writer" });
            }
        }
    }
    if unacked.len() >= MAX_UNACKED_COMMITS {
        let pending = unacked.remove(0);
        let receipt = recv_commit_receipt(pending)?;
        engine.accept_commit_receipt(&receipt);
        record_commit(outcome, &receipt);
    }
    Ok(())
}

fn scheduled_work_key(
    stream: &StreamSpec,
    object: &DeclaredObject,
    generation: u64,
) -> Result<WorkKey, EngineError> {
    let stream_id = stream.id.as_str().as_bytes();
    let mut object_key =
        Vec::with_capacity(8 + stream_id.len() + object.descriptor.object_key.len());
    object_key.extend_from_slice(&(stream_id.len() as u64).to_be_bytes());
    object_key.extend_from_slice(stream_id);
    object_key.extend_from_slice(&object.descriptor.object_key);
    WorkKey::new(object_key, generation).map_err(source_error)
}

fn append_checkpoint_through(
    checkpoint: &AppendCheckpoint,
    cursor_end: &SourceCursor,
) -> AppendCheckpoint {
    let mut next = checkpoint.clone();
    if let Some(offset) = cursor_end.append_offset_value() {
        next.committed_offset = offset;
    }
    next
}

fn increment_capability_quarantines(
    counts: &mut BTreeMap<String, u32>,
    capabilities: &[CapabilityId],
) {
    for capability in capabilities {
        let count = counts.entry(capability.as_str().to_string()).or_default();
        *count = count.saturating_add(1);
    }
}

fn record_quarantines(
    outcome: &mut ReconcileOutcome,
    records: u32,
    unscoped_records: u32,
    capability_records: BTreeMap<String, u32>,
) {
    outcome.records_quarantined = outcome.records_quarantined.saturating_add(records);
    outcome.unscoped_records_quarantined = outcome
        .unscoped_records_quarantined
        .saturating_add(unscoped_records);
    for (capability, records) in capability_records {
        let count = outcome
            .capability_records_quarantined
            .entry(capability)
            .or_default();
        *count = count.saturating_add(records);
    }
}

fn record_unscoped_quarantines(outcome: &mut ReconcileOutcome, records: u32) {
    record_quarantines(outcome, records, records, BTreeMap::new());
}

fn merge_outcome(target: &mut ReconcileOutcome, source: ReconcileOutcome) {
    target.instances_discovered = target
        .instances_discovered
        .saturating_add(source.instances_discovered);
    target.streams_reconciled = target
        .streams_reconciled
        .saturating_add(source.streams_reconciled);
    target.streams_unavailable = target
        .streams_unavailable
        .saturating_add(source.streams_unavailable);
    target.objects_discovered = target
        .objects_discovered
        .saturating_add(source.objects_discovered);
    target.objects_registered = target
        .objects_registered
        .saturating_add(source.objects_registered);
    target.objects_changed = target
        .objects_changed
        .saturating_add(source.objects_changed);
    target.objects_unchanged = target
        .objects_unchanged
        .saturating_add(source.objects_unchanged);
    target.objects_removed = target
        .objects_removed
        .saturating_add(source.objects_removed);
    target.records_decoded = target
        .records_decoded
        .saturating_add(source.records_decoded);
    target.records_quarantined = target
        .records_quarantined
        .saturating_add(source.records_quarantined);
    target.unscoped_records_quarantined = target
        .unscoped_records_quarantined
        .saturating_add(source.unscoped_records_quarantined);
    for (capability, records) in source.capability_records_quarantined {
        let count = target
            .capability_records_quarantined
            .entry(capability)
            .or_default();
        *count = count.saturating_add(records);
    }
    target.retries_required = target
        .retries_required
        .saturating_add(source.retries_required);
    target.incomplete_tail_retries = target
        .incomplete_tail_retries
        .saturating_add(source.incomplete_tail_retries);
    target.dependency_access_attempts = target
        .dependency_access_attempts
        .saturating_add(source.dependency_access_attempts);
    target.dependency_access_denials = target
        .dependency_access_denials
        .saturating_add(source.dependency_access_denials);
    target.dependency_access_abandoned = target
        .dependency_access_abandoned
        .saturating_add(source.dependency_access_abandoned);
    target.dependency_objects_accessed = target
        .dependency_objects_accessed
        .saturating_add(source.dependency_objects_accessed);
    target.dependency_bytes_read = target
        .dependency_bytes_read
        .saturating_add(source.dependency_bytes_read);
    target.dependency_rows_read = target
        .dependency_rows_read
        .saturating_add(source.dependency_rows_read);
    target.dependency_max_depth = target.dependency_max_depth.max(source.dependency_max_depth);
    target.dependency_trace_entries_dropped = target
        .dependency_trace_entries_dropped
        .saturating_add(source.dependency_trace_entries_dropped);
    target.backlog_remaining = target
        .backlog_remaining
        .saturating_add(source.backlog_remaining);
    for retry in source.retry_targets {
        if !target.retry_targets.contains(&retry) {
            target.retry_targets.push(retry);
        }
    }
    target.commits = target.commits.saturating_add(source.commits);
    target.last_commit_seq = target.last_commit_seq.max(source.last_commit_seq);
}

fn apply_access_snapshot(outcome: &mut ReconcileOutcome, snapshot: &AccessBudgetSnapshot) {
    outcome.dependency_access_attempts = outcome
        .dependency_access_attempts
        .saturating_add(snapshot.attempts);
    outcome.dependency_access_denials = outcome
        .dependency_access_denials
        .saturating_add(snapshot.denied);
    outcome.dependency_access_abandoned = outcome
        .dependency_access_abandoned
        .saturating_add(snapshot.abandoned);
    outcome.dependency_objects_accessed = outcome
        .dependency_objects_accessed
        .saturating_add(snapshot.objects_accessed);
    outcome.dependency_bytes_read = outcome
        .dependency_bytes_read
        .saturating_add(snapshot.bytes_read);
    outcome.dependency_rows_read = outcome
        .dependency_rows_read
        .saturating_add(snapshot.rows_read);
    outcome.dependency_max_depth = outcome
        .dependency_max_depth
        .max(snapshot.max_depth_observed);
    outcome.dependency_trace_entries_dropped = outcome
        .dependency_trace_entries_dropped
        .saturating_add(snapshot.trace_entries_dropped);
}

fn record_retry_target(
    outcome: &mut ReconcileOutcome,
    instance: &SourceInstance,
    stream: &StreamSpec,
    object: &DeclaredObject,
) {
    let target = ReconcileRetryTarget {
        stable_key: instance.spec.stable_key.as_bytes().to_vec(),
        stream_key: stream.id.as_str().to_string(),
        object_key: object.descriptor.object_key.clone(),
    };
    if !outcome.retry_targets.contains(&target) {
        outcome.retry_targets.push(target);
    }
}

fn now_unix_ms() -> Result<i64, EngineError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| observation_error("read system time", error.to_string()))?;
    i64::try_from(duration.as_millis())
        .map_err(|_| observation_error("read system time", "epoch milliseconds overflowed"))
}

fn bounded_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn catch_adapter_panic<T>(
    operation: &'static str,
    call: impl FnOnce() -> Result<T, AdapterError>,
) -> Result<Result<T, AdapterError>, EngineError> {
    catch_unwind(AssertUnwindSafe(call)).map_err(|_| {
        // Panic payloads are deliberately omitted: adapters parse private
        // source content and a formatted payload is not safe public telemetry.
        observation_error(operation, "adapter panicked at the controlled boundary")
    })
}

fn adapter_error(operation: &'static str, error: AdapterError) -> EngineError {
    observation_error(operation, error.to_string())
}

fn source_error(error: SourceDriverError) -> EngineError {
    observation_error("run common source driver", error.to_string())
}

fn io_observation_error(
    operation: &'static str,
    path: &Path,
    error: std::io::Error,
) -> EngineError {
    observation_error(operation, format!("{}: {error}", path.to_string_lossy()))
}

fn observation_error(operation: &'static str, detail: impl Into<String>) -> EngineError {
    EngineError::Observation {
        operation,
        detail: detail.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use rusqlite::{Connection, OpenFlags, OptionalExtension};
    use tempfile::TempDir;

    use crate::adapter::{
        AdapterId, AdapterManifest, AdapterRegistry, ConsistencyPolicy, DecodeContext, DecoderId,
        EntityScope, Fact, ObjectSelector, SourceInstanceKey, SourceRoot, StreamId,
    };
    use crate::claude::ClaudeCodeAdapter;
    use crate::decode_runtime::MAX_DIAGNOSTIC_EXCERPT_BYTES;
    use crate::engine::EngineOptions;
    use crate::source::{
        platform_path_key, DirectorySnapshotConfig, IngestPriority, KeyValueRecord,
        KeyValueSnapshotConfig, ReplaceDocumentConfig, SqliteQuerySpec, SqliteRowRecord,
        SqliteSnapshotConfig,
    };

    use super::*;

    fn glob(pattern: &str) -> GlobPattern {
        GlobPattern::new(pattern).unwrap()
    }

    fn path(value: &str) -> Vec<Vec<u8>> {
        value
            .split('/')
            .map(|component| component.as_bytes().to_vec())
            .collect()
    }

    #[test]
    fn component_globs_cover_declared_nested_shapes() {
        assert!(glob("*/*.jsonl").matches(&path("project/session.jsonl")));
        assert!(!glob("*/*.jsonl").matches(&path("project/a/session.jsonl")));
        assert!(glob("*/*/subagents/**/agent-*.jsonl").matches(&path(
            "project/session/subagents/workflows/wf/member/agent-child.jsonl"
        )));
        assert!(glob("*/*/subagents/**/agent-*.jsonl")
            .matches(&path("project/session/subagents/agent-child.jsonl")));
        assert!(!glob("*/*/subagents/**/agent-*.jsonl")
            .matches(&path("project/session/subagents/agent-child.meta.json")));
    }

    #[test]
    fn invalid_recursive_wildcards_are_rejected() {
        assert!(GlobPattern::new("root/a**b/file").is_err());
        assert!(GlobPattern::new("../escape").is_err());
        assert!(GlobPattern::new("/absolute").is_err());
    }

    #[test]
    fn diagnostic_retention_is_bounded_and_never_copies_secret_keys_or_values() {
        let payload = br#"{"authorization":"Bearer private-token","nested":{"password":"hunter2"},"count":7}"#;
        let excerpt = retained_diagnostic_payload(RawRetentionPolicy::DiagnosticExcerpt, payload)
            .expect("diagnostic excerpt");
        assert!(excerpt.len() <= MAX_DIAGNOSTIC_EXCERPT_BYTES);
        serde_json::from_slice::<serde_json::Value>(&excerpt).expect("valid diagnostic JSON");
        let text = String::from_utf8(excerpt).unwrap();
        for secret in ["authorization", "private-token", "password", "hunter2"] {
            assert!(!text.contains(secret), "diagnostic leaked {secret}");
        }
        assert_eq!(
            retained_diagnostic_payload(RawRetentionPolicy::Full, payload),
            Some(payload.to_vec())
        );
        assert_eq!(
            retained_diagnostic_payload(RawRetentionPolicy::HashOnly, payload),
            None
        );
        assert_eq!(
            retained_diagnostic_payload(RawRetentionPolicy::None, payload),
            None
        );
    }

    #[test]
    fn source_access_confines_stamps_and_revalidates_dependency_reads() {
        let root = TempDir::new().unwrap();
        std::fs::write(root.path().join("summary.json"), br#"{"title":"one"}"#).unwrap();
        let source_database = root.path().join("source.db");
        Connection::open(&source_database)
            .unwrap()
            .execute_batch(
                "CREATE TABLE items(id INTEGER PRIMARY KEY, value TEXT);\n\
                 INSERT INTO items VALUES (1, 'one');",
            )
            .unwrap();
        let instance = SourceInstance {
            id: 41,
            spec: AdapterSourceInstanceSpec {
                identity_contract_version: 1,
                stable_key: SourceInstanceKey::new(b"source-access-root".to_vec()).unwrap(),
                display_name: "source access".to_string(),
                roots: vec![crate::adapter::SourceRoot {
                    name: "sessions".to_string(),
                    path: root.path().to_path_buf(),
                }],
                discovery_reason: "test".to_string(),
            },
        };
        let cancellations = Vec::new();
        let access = ConfinedSourceAccess::new(&instance, &cancellations);
        let snapshot = access
            .read_object("sessions", Path::new("summary.json"), 1_024)
            .unwrap();
        assert_eq!(snapshot.payload.unwrap(), br#"{"title":"one"}"#);
        let rows = access
            .query_source_db(&SourceQuery {
                root_name: "sessions".to_string(),
                relative_path: PathBuf::from("source.db"),
                query: SqliteQuerySpec {
                    name: "items".to_string(),
                    sql: "SELECT id, value FROM items".to_string(),
                    key_columns: vec!["id".to_string()],
                },
                bounds: crate::adapter::SourceQueryBounds::default(),
            })
            .unwrap();
        assert!(rows.available);
        assert_eq!(rows.rows.len(), 1);
        let listing = access
            .list_objects(&SourceObjectListRequest {
                root_name: "sessions".to_string(),
                include: vec!["*.json".to_string()],
                exclude: Vec::new(),
                max_entries: 8,
            })
            .unwrap();
        assert_eq!(listing.objects.len(), 1);
        assert_eq!(access.revisions().unwrap().len(), 3);
        assert!(!access.changed_since_read().unwrap());

        Connection::open(&source_database)
            .unwrap()
            .execute("UPDATE items SET value = 'two' WHERE id = 1", [])
            .unwrap();
        assert!(access.changed_since_read().unwrap());
        let access_snapshot = access.access_snapshot();
        assert_eq!(access_snapshot.attempts, 8);
        assert_eq!(access_snapshot.completed, 8);
        assert_eq!(access_snapshot.denied, 0);
        assert_eq!(access_snapshot.abandoned, 0);
        assert_eq!(access_snapshot.objects_accessed, 3);
        assert!(access_snapshot.bytes_read > 0);
        assert_eq!(access_snapshot.rows_read, 5);
        assert_eq!(access_snapshot.max_depth_observed, 1);
        assert_eq!(
            access_snapshot
                .trace
                .iter()
                .filter(|entry| entry.phase == AccessPhase::Revalidation)
                .count(),
            5
        );
        assert!(access
            .read_object("sessions", Path::new("../escape"), 1_024)
            .is_err());
        assert_eq!(access.access_snapshot().attempts, 8);
    }

    #[test]
    fn claude_reconcile_resumes_append_checkpoints_across_engine_restart() {
        let fixture = ClaudeFixture::new();
        let transcript = fixture.transcript_path();
        std::fs::write(&transcript, transcript_line("m1", "first")).unwrap();

        let first = fixture.open_engine();
        let initial = fixture.reconcile(&first);
        assert_eq!(initial.objects_registered, 1);
        assert_eq!(initial.records_decoded, 1);
        assert_eq!(
            count_rows(&fixture.database, "ingest_commits"),
            2,
            "the first data commit owns object identity and pending readiness; one following administrative commit establishes ready"
        );
        assert_eq!(
            projection_version_state(&fixture.database, "runtime.usage-v2"),
            (1, Some(1), "ready".to_string(), 2, None)
        );
        assert_eq!(count_rows(&fixture.database, "canonical_messages"), 1);
        let first_instance_id = initial_source_instance_id(&fixture.database);
        let first_message_key = first_blob(
            &fixture.database,
            "SELECT message_key FROM canonical_messages LIMIT 1",
        );
        first.shutdown().unwrap();

        let mut append = std::fs::OpenOptions::new()
            .append(true)
            .open(&transcript)
            .unwrap();
        append.write_all(&transcript_line("m2", "second")).unwrap();
        append.flush().unwrap();

        let restarted = fixture.open_engine();
        let resumed = fixture.reconcile(&restarted);
        assert_eq!(resumed.objects_registered, 0);
        assert_eq!(resumed.records_decoded, 1);
        assert_eq!(resumed.commits, 2);
        assert_eq!(count_rows(&fixture.database, "ingest_commits"), 4);
        assert_eq!(
            projection_version_state(&fixture.database, "runtime.usage-v2"),
            (1, Some(1), "ready".to_string(), 4, None)
        );
        assert_eq!(count_rows(&fixture.database, "canonical_messages"), 2);
        assert_ne!(first_instance_id, 0);
        assert_eq!(
            initial_source_instance_id(&fixture.database),
            first_instance_id
        );
        assert!(count_with_instance_id(&fixture.database, first_instance_id) > 0);
        assert_eq!(count_with_instance_id(&fixture.database, 0), 0);
        assert_eq!(
            first_blob(
                &fixture.database,
                "SELECT message_key FROM canonical_messages ORDER BY cursor_end LIMIT 1",
            ),
            first_message_key
        );
        let catalog = restarted
            .source_catalog(
                ClaudeCodeAdapter::new().manifest().id.as_str(),
                &canonical_root_key(&fixture.root),
            )
            .unwrap();
        let transcript_object = catalog
            .objects
            .iter()
            .find(|object| object.stream_key == "session-transcripts")
            .unwrap();
        assert_eq!(transcript_object.generation, 1);
        assert!(transcript_object.driver_checkpoint.is_some());
        assert_eq!(
            AppendCheckpoint::decode(transcript_object.driver_checkpoint.as_deref().unwrap())
                .unwrap()
                .committed_offset,
            std::fs::metadata(&transcript).unwrap().len()
        );
    }

    #[test]
    fn append_backfill_commits_multiple_records_as_one_bounded_batch() {
        let fixture = ClaudeFixture::new();
        let transcript = fixture.transcript_path();
        let mut contents = Vec::new();
        for index in 0..10 {
            contents.extend(transcript_line(&format!("m{index}"), "batched"));
        }
        std::fs::write(transcript, contents).unwrap();
        let engine = fixture.open_engine();

        let outcome = fixture.reconcile(&engine);
        assert_eq!(outcome.records_decoded, 10);
        assert_eq!(
            outcome.commits, 2,
            "one bounded data commit is followed by one projection completion barrier"
        );
        assert_eq!(count_rows(&fixture.database, "canonical_messages"), 10);
    }

    #[test]
    fn usage_v2_coverage_is_durable_and_restart_stable() {
        let fixture = ClaudeFixture::new();
        let transcript = fixture.transcript_path();
        std::fs::write(&transcript, transcript_line("m1", "covered")).unwrap();

        let first = fixture.open_engine();
        let initial = fixture.reconcile(&first);
        assert_eq!(initial.commits, 2);
        let covered = usage_v2_coverage_state(&fixture.database);
        assert_eq!(covered.set_contract_version, 1);
        assert_eq!(covered.coverage_contract_version, 1);
        assert_eq!(covered.domain_kind, "fact_family");
        assert_eq!(covered.domain_name, "runtime.usage-v2");
        assert_eq!(covered.domain_version, 1);
        assert_eq!(covered.adapter_id, "claude-code");
        assert_eq!(
            covered.support_release_id,
            "claude-code-support-2026-08-21-promoted"
        );
        assert_eq!(covered.completeness, "complete");
        assert_eq!(covered.last_commit_seq, 2);
        assert_eq!(covered.point_count, 1);
        assert_eq!(covered.absence_count, 0);
        assert_eq!(covered.point_status.as_deref(), Some("complete_through"));
        assert_eq!(covered.position_kind.as_deref(), Some("append_cursor"));
        assert_eq!(
            covered.monotonic_order,
            Some(std::fs::metadata(&transcript).unwrap().len())
        );
        assert_eq!(covered.unavailable_reason, None);
        assert!(covered.error_codes.is_empty());

        let (project_id, session_id) = canonical_project_session_ids(&fixture.database);
        let public = first
            .fact_family_coverage_cancellable(
                crate::engine::FactFamilyCoveragePageRequest {
                    project_id: project_id.clone(),
                    session_id: session_id.clone(),
                    owner_id: USAGE_V2_PROJECTION_ID.to_string(),
                    family: USAGE_V2_PROJECTION_ID.to_string(),
                    family_version: USAGE_V2_PROJECTION_VERSION,
                    cursor: None,
                    limit: 1,
                },
                QueryCancellationToken::default(),
            )
            .unwrap();
        assert_eq!(public.contract_version, 1);
        assert_eq!(public.at_commit_seq, 2);
        assert_eq!(public.status, "materialized");
        assert_eq!(public.items.len(), 1);
        assert_eq!(public.items[0].kind, "point");
        assert_eq!(public.items[0].status.as_deref(), Some("complete_through"));
        assert_eq!(public.items[0].monotonic_order, covered.monotonic_order);
        assert!(public.next_cursor.is_none());
        let public_set = public.coverage.unwrap();
        assert_eq!(public_set.completeness, "complete");
        assert_eq!(public_set.last_commit_seq, 2);
        assert_eq!(public_set.adapter_id, "claude-code");
        for opaque in [
            public_set.source_instance_ref,
            public_set.declaration_ref,
            public_set.membership_revision_ref,
            public_set.content_digest_ref,
            public.items[0].stream_ref.clone().unwrap(),
            public.items[0].object_ref.clone().unwrap(),
        ] {
            assert!(opaque.starts_with("v1:"));
            assert!(!opaque.contains(PROJECT));
            assert!(!opaque.contains(SESSION));
        }
        first.shutdown().unwrap();

        let restarted = fixture.open_engine();
        let unchanged = fixture.reconcile(&restarted);
        assert_eq!(unchanged.commits, 0);
        assert_eq!(usage_v2_coverage_state(&fixture.database), covered);
    }

    #[test]
    fn unrelated_stream_commits_do_not_churn_usage_v2_readiness() {
        let fixture = ClaudeFixture::new();
        std::fs::write(
            fixture.transcript_path(),
            transcript_line("m1", "usage source"),
        )
        .unwrap();
        let engine = fixture.open_engine();
        let initial = fixture.reconcile(&engine);
        assert_eq!(initial.commits, 2);
        assert_eq!(
            projection_version_state(&fixture.database, "runtime.usage-v2"),
            (1, Some(1), "ready".to_string(), 2, None)
        );

        std::fs::write(
            fixture.root.join("settings.json"),
            br#"{"model":"claude-opus"}"#,
        )
        .unwrap();
        let settings_only = fixture.reconcile(&engine);
        assert_eq!(settings_only.records_decoded, 1);
        assert_eq!(settings_only.commits, 1);
        assert_eq!(count_rows(&fixture.database, "ingest_commits"), 3);
        assert_eq!(
            projection_version_state(&fixture.database, "runtime.usage-v2"),
            (1, Some(1), "ready".to_string(), 2, None),
            "a non-provider stream must not move the usage-v2 readiness clock"
        );
    }

    #[test]
    fn provider_object_reconcile_closes_pending_with_instance_coverage_barrier() {
        let fixture = ClaudeFixture::new();
        let transcript = fixture.transcript_path();
        std::fs::write(&transcript, transcript_line("m1", "initial usage")).unwrap();
        let engine = fixture.open_engine();
        assert_eq!(fixture.reconcile(&engine).commits, 2);

        let mut append = std::fs::OpenOptions::new()
            .append(true)
            .open(&transcript)
            .unwrap();
        append
            .write_all(&transcript_line("m2", "targeted usage"))
            .unwrap();
        append.flush().unwrap();

        let adapter = ClaudeCodeAdapter::new();
        let spec = adapter
            .discover(&DiscoveryContext {
                configured_roots: vec![fixture.root.clone()],
                observed_at: now_unix_ms().unwrap(),
            })
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let object = catalog_object(&engine, &fixture.root, "session-transcripts");
        let target = ReconcileRetryTarget {
            stable_key: spec.stable_key.as_bytes().to_vec(),
            stream_key: object.stream_key,
            object_key: object.object_key,
        };
        let update = ObservationCoordinator::new(Arc::clone(&engine))
            .reconcile_declared_object(&adapter, spec, &target, "test targeted provider update")
            .unwrap();

        assert_eq!(update.records_decoded, 1);
        assert_eq!(
            update.commits, 2,
            "the provider data commit must be followed by its coverage barrier"
        );
        assert_eq!(
            projection_version_state(&fixture.database, "runtime.usage-v2"),
            (1, Some(1), "ready".to_string(), 4, None)
        );
        let coverage = usage_v2_coverage_state(&fixture.database);
        assert_eq!(coverage.completeness, "complete");
        assert_eq!(coverage.last_commit_seq, 4);
        assert_eq!(coverage.point_count, 1);
        assert_eq!(
            coverage.monotonic_order,
            Some(std::fs::metadata(transcript).unwrap().len())
        );
        assert_eq!(
            count_rows(&fixture.database, "usage_v2_response_contributions"),
            2
        );
    }

    #[test]
    fn non_usage_diagnostic_does_not_create_a_usage_v2_coverage_gap() {
        let fixture = ClaudeFixture::new();
        let mut transcript = transcript_line("m1", "usage remains covered");
        transcript.extend_from_slice(
            br#"{"type":"file-history-delta","messageId":"delta-bad","snapshotMessageId":"checkpoint","trackingPath":"src/lib.rs","backup":{"backupFileName":"71f902cd51ee4c6e@v2","version":3,"backupTime":"2026-08-11T20:01:00.000Z"}}"#,
        );
        transcript.push(b'\n');
        std::fs::write(fixture.transcript_path(), transcript).unwrap();
        let engine = fixture.open_engine();

        let outcome = fixture.reconcile(&engine);
        assert_eq!(outcome.records_quarantined, 1);
        assert_eq!(outcome.unscoped_records_quarantined, 0);
        assert_eq!(
            outcome
                .capability_records_quarantined
                .get("runtime.artifacts"),
            Some(&1)
        );
        assert!(!outcome
            .capability_records_quarantined
            .contains_key(USAGE_V2_PROJECTION_ID));
        assert_eq!(
            projection_version_state(&fixture.database, USAGE_V2_PROJECTION_ID).2,
            "ready"
        );
        assert_eq!(
            usage_v2_coverage_state(&fixture.database).completeness,
            "complete"
        );
        assert_eq!(
            count_rows(&fixture.database, "usage_v2_response_contributions"),
            1
        );
        assert_eq!(count_rows(&fixture.database, "source_record_errors"), 1);
    }

    #[test]
    fn rfc012_x2_dump_engine_source_record_errors() {
        let fixture = ClaudeFixture::new();
        let mut transcript = transcript_line("m1", "covered");
        for uuid in ["row-bad-a", "row-bad-b", "row-bad-a2"] {
            transcript.extend_from_slice(
                format!(
                    r#"{{"type":"assistant","uuid":"{uuid}","timestamp":"2026-08-11T00:00:01Z","sessionId":"{SESSION}","cwd":"/repo","message":{{"model":"claude-sonnet","id":"api-bad","type":"message","role":"assistant","content":[],"usage":{{"input_tokens":"bad"}}}}}}"#
                )
                .as_bytes(),
            );
            transcript.push(b'\n');
        }
        transcript.extend_from_slice(
            br#"{"type":"file-history-delta","messageId":"delta-bad","snapshotMessageId":"checkpoint","trackingPath":"src/lib.rs","backup":{"backupFileName":"71f902cd51ee4c6e@v2","version":3,"backupTime":"2026-08-11T20:01:00.000Z"}}"#,
        );
        transcript.push(b'\n');
        std::fs::write(fixture.transcript_path(), transcript).unwrap();
        let engine = fixture.open_engine();

        let outcome = fixture.reconcile(&engine);
        assert!(outcome.records_quarantined >= 2);
        assert!(count_rows(&fixture.database, "source_record_errors") >= 2);

        engine.shutdown().unwrap();
        drop(engine);

        let source = Connection::open(&fixture.database).unwrap();
        source
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .unwrap();
        let dump = std::env::var_os("RFC012_X2_DUMP")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                Path::new(env!("CARGO_MANIFEST_DIR")).join(
                    "../../scripts/rfc012_experiments/fixtures/source-record-errors.sqlite",
                )
            });
        if dump.exists() {
            std::fs::remove_file(&dump).unwrap();
        }
        if let Some(parent) = dump.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        {
            let dest = Connection::open(&dump).unwrap();
            dest.execute_batch(
                r#"
                CREATE TABLE source_instances (
                  source_instance_id INTEGER PRIMARY KEY,
                  adapter_id TEXT NOT NULL
                );
                CREATE TABLE source_streams (
                  source_stream_id INTEGER PRIMARY KEY,
                  source_instance_id INTEGER NOT NULL,
                  stream_key TEXT NOT NULL
                );
                CREATE TABLE source_objects (
                  source_object_id INTEGER PRIMARY KEY,
                  source_stream_id INTEGER NOT NULL
                );
                CREATE TABLE source_record_errors (
                  source_object_id INTEGER NOT NULL,
                  generation INTEGER NOT NULL,
                  error_class TEXT NOT NULL,
                  first_commit_seq INTEGER NOT NULL,
                  payload_hash BLOB NOT NULL
                );
                "#,
            )
            .unwrap();
        }
        source
            .execute("ATTACH DATABASE ?1 AS dump", [dump.to_str().unwrap()])
            .unwrap();
        source
            .execute_batch(
                r#"
                DELETE FROM dump.source_instances;
                INSERT INTO dump.source_instances(source_instance_id, adapter_id)
                  SELECT source_instance_id, adapter_id FROM source_instances;
                INSERT INTO dump.source_streams(source_stream_id, source_instance_id, stream_key)
                  SELECT source_stream_id, source_instance_id, stream_key FROM source_streams;
                INSERT INTO dump.source_objects(source_object_id, source_stream_id)
                  SELECT source_object_id, source_stream_id FROM source_objects;
                INSERT INTO dump.source_record_errors(
                  source_object_id, generation, error_class, first_commit_seq, payload_hash
                )
                  SELECT source_object_id, generation, error_class, first_commit_seq, payload_hash
                    FROM source_record_errors;
                "#,
            )
            .unwrap();
        source.execute_batch("DETACH DATABASE dump;").unwrap();

        let dumped =
            Connection::open_with_flags(&dump, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let dump_errors: i64 = dumped
            .query_row("SELECT COUNT(*) FROM source_record_errors", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(dump_errors >= 2);
        let native_columns = dumped
            .prepare("PRAGMA table_info(source_record_errors)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .map(Result::unwrap)
            .filter(|name| name == "error_message" || name == "raw_payload")
            .count();
        assert_eq!(native_columns, 0);
    }

    #[test]
    fn usage_scoped_diagnostic_blocks_usage_v2_coverage() {
        let fixture = ClaudeFixture::new();
        let malformed = format!(
            r#"{{"type":"assistant","uuid":"row-bad","timestamp":"2026-08-11T00:00:01Z","sessionId":"{SESSION}","cwd":"/repo","message":{{"model":"claude-sonnet","id":"api-bad","type":"message","role":"assistant","content":[],"usage":{{"input_tokens":"bad"}}}}}}"#
        );
        std::fs::write(fixture.transcript_path(), format!("{malformed}\n")).unwrap();
        let engine = fixture.open_engine();

        let outcome = fixture.reconcile(&engine);
        assert_eq!(outcome.records_quarantined, 1);
        assert_eq!(outcome.unscoped_records_quarantined, 0);
        assert_eq!(
            outcome
                .capability_records_quarantined
                .get(USAGE_V2_PROJECTION_ID),
            Some(&1)
        );
        assert_eq!(
            projection_version_state(&fixture.database, USAGE_V2_PROJECTION_ID).2,
            "unavailable"
        );
        assert_eq!(
            usage_v2_coverage_state(&fixture.database).error_codes,
            vec!["coverage_gap_requires_explicit_replay".to_string()]
        );
        assert_eq!(
            count_rows(&fixture.database, "usage_v2_response_contributions"),
            0
        );
    }

    #[test]
    fn quarantined_usage_v2_coverage_stays_unavailable_until_explicit_replay() {
        let fixture = ClaudeFixture::new();
        let transcript = fixture.transcript_path();
        let mut initial_transcript = transcript_line("m1", "usage before coverage gap");
        initial_transcript.extend_from_slice(b"{not-json}\n");
        std::fs::write(&transcript, initial_transcript).unwrap();
        let engine = fixture.open_engine();

        let initial = fixture.reconcile(&engine);
        assert_eq!(initial.records_quarantined, 1);
        let unavailable = projection_version_state(&fixture.database, "runtime.usage-v2");
        assert_eq!(unavailable.0, 1);
        assert_eq!(unavailable.1, None);
        assert_eq!(unavailable.2, "unavailable");
        assert!(unavailable
            .4
            .as_deref()
            .is_some_and(|detail| detail.contains("session-transcripts=records_quarantined")));
        let unavailable_coverage = usage_v2_coverage_state(&fixture.database);
        assert_eq!(unavailable_coverage.completeness, "unavailable");
        assert_eq!(unavailable_coverage.last_commit_seq, 2);
        assert_eq!(
            unavailable_coverage.point_status.as_deref(),
            Some("unavailable")
        );
        assert_eq!(
            unavailable_coverage.unavailable_reason.as_deref(),
            Some("coverage_gap_requires_explicit_replay")
        );
        assert_eq!(
            unavailable_coverage.error_codes,
            vec!["coverage_gap_requires_explicit_replay".to_string()]
        );
        let (project_id, session_id) = canonical_project_session_ids(&fixture.database);
        let coverage_request = |cursor| crate::engine::FactFamilyCoveragePageRequest {
            project_id: project_id.clone(),
            session_id: session_id.clone(),
            owner_id: USAGE_V2_PROJECTION_ID.to_string(),
            family: USAGE_V2_PROJECTION_ID.to_string(),
            family_version: USAGE_V2_PROJECTION_VERSION,
            cursor,
            limit: 1,
        };
        let first_coverage_page = engine
            .fact_family_coverage_cancellable(
                coverage_request(None),
                QueryCancellationToken::default(),
            )
            .unwrap();
        let first_coverage_authorization = first_coverage_page.coverage.clone().unwrap();
        assert_eq!(first_coverage_page.items.len(), 1);
        assert_eq!(first_coverage_page.items[0].kind, "point");
        let first_coverage_cursor = first_coverage_page.next_cursor.unwrap();
        let second_coverage_page = engine
            .fact_family_coverage_cancellable(
                coverage_request(Some(first_coverage_cursor.clone())),
                QueryCancellationToken::default(),
            )
            .unwrap();
        assert_eq!(second_coverage_page.items.len(), 1);
        assert_eq!(second_coverage_page.items[0].kind, "error");
        assert_eq!(
            second_coverage_page.items[0].error_code.as_deref(),
            Some("coverage_gap_requires_explicit_replay")
        );
        assert!(second_coverage_page.next_cursor.is_none());

        let unchanged = fixture.reconcile(&engine);
        assert_eq!(unchanged.records_quarantined, 0);
        assert_eq!(unchanged.commits, 0);
        assert_eq!(
            projection_version_state(&fixture.database, "runtime.usage-v2"),
            unavailable,
            "passing an already-quarantined cursor cannot prove the missing family coverage"
        );
        assert_eq!(
            usage_v2_coverage_state(&fixture.database),
            unavailable_coverage,
            "the sticky gap must not rewrite its normalized coverage snapshot"
        );

        let mut append = std::fs::OpenOptions::new()
            .append(true)
            .open(&transcript)
            .unwrap();
        append
            .write_all(&transcript_line("m2", "later valid response"))
            .unwrap();
        append.flush().unwrap();
        let appended = fixture.reconcile(&engine);
        assert_eq!(appended.records_decoded, 1);
        assert_eq!(appended.commits, 2);
        let still_unavailable = projection_version_state(&fixture.database, "runtime.usage-v2");
        assert_eq!(still_unavailable.2, "unavailable");
        assert_eq!(still_unavailable.4, unavailable.4);
        let advanced_coverage = usage_v2_coverage_state(&fixture.database);
        assert_eq!(advanced_coverage.completeness, "unavailable");
        assert_eq!(advanced_coverage.last_commit_seq, 4);
        assert_eq!(
            advanced_coverage.error_codes,
            unavailable_coverage.error_codes
        );
        assert_ne!(
            advanced_coverage.content_digest,
            unavailable_coverage.content_digest
        );
        assert!(matches!(
            engine.fact_family_coverage_cancellable(
                coverage_request(Some(first_coverage_cursor)),
                QueryCancellationToken::default(),
            ),
            Err(EngineError::InvalidQuery(detail)) if detail.contains("cursor expired")
        ));
        assert!(matches!(
            engine.replay_fact_family_cancellable(
                crate::engine::FactFamilyReplayCommand {
                    adapter_id: first_coverage_authorization.adapter_id.clone(),
                    configured_roots: vec![fixture.root.clone()],
                    project_id: project_id.clone(),
                    session_id: session_id.clone(),
                    owner_id: USAGE_V2_PROJECTION_ID.to_string(),
                    family: USAGE_V2_PROJECTION_ID.to_string(),
                    family_version: USAGE_V2_PROJECTION_VERSION,
                    expected_source_instance_ref: first_coverage_authorization
                        .source_instance_ref
                        .clone(),
                    expected_content_digest_ref: first_coverage_authorization
                        .content_digest_ref
                        .clone(),
                    expected_coverage_last_commit_seq: first_coverage_authorization
                        .last_commit_seq,
                    reason: "stale authorization must fail".to_string(),
                },
                QueryCancellationToken::default(),
            ),
            Err(EngineError::InvalidQuery(detail)) if detail.contains("authorization is stale")
        ));

        // Repairing/replacing the native file through an ordinary reconcile
        // is not sufficient proof: the sticky gap remains unavailable even
        // though the new generation now decodes cleanly.
        std::fs::write(&transcript, transcript_line("m2", "corrected response")).unwrap();
        let repaired = fixture.reconcile(&engine);
        assert_eq!(repaired.records_decoded, 1);
        let repaired_object = catalog_object(&engine, &fixture.root, "session-transcripts");
        assert_eq!(
            projection_version_state(&fixture.database, "runtime.usage-v2").2,
            "unavailable"
        );
        std::fs::write(
            fixture.root.join("settings.json"),
            br#"{"model":"claude-sonnet"}"#,
        )
        .unwrap();
        assert_eq!(fixture.reconcile(&engine).records_decoded, 1);
        let settings_before =
            catalog_object(&engine, &fixture.root, "interpretation-settings").generation;

        let replay_authorization = engine
            .fact_family_coverage_cancellable(
                coverage_request(None),
                QueryCancellationToken::default(),
            )
            .unwrap()
            .coverage
            .unwrap();
        let wrong_root_path = fixture.root.parent().unwrap().join("other-source");
        std::fs::create_dir_all(&wrong_root_path).unwrap();
        let wrong_root = engine.replay_fact_family_cancellable(
            crate::engine::FactFamilyReplayCommand {
                adapter_id: replay_authorization.adapter_id.clone(),
                configured_roots: vec![wrong_root_path],
                project_id: project_id.clone(),
                session_id: session_id.clone(),
                owner_id: USAGE_V2_PROJECTION_ID.to_string(),
                family: USAGE_V2_PROJECTION_ID.to_string(),
                family_version: USAGE_V2_PROJECTION_VERSION,
                expected_source_instance_ref: replay_authorization.source_instance_ref.clone(),
                expected_content_digest_ref: replay_authorization.content_digest_ref.clone(),
                expected_coverage_last_commit_seq: replay_authorization.last_commit_seq,
                reason: "wrong configured root must fail".to_string(),
            },
            QueryCancellationToken::default(),
        );
        assert!(
            matches!(
                &wrong_root,
                Err(EngineError::InvalidConfig(detail)) if detail.contains("configured roots")
            ),
            "unexpected wrong-root result: {wrong_root:?}"
        );
        let connection =
            Connection::open_with_flags(&fixture.database, OpenFlags::SQLITE_OPEN_READ_ONLY)
                .unwrap();
        let (source_instance_id, owner_scope_key, canonical_source_instance_key) = connection
            .query_row(
                r#"
                SELECT source_instance_id, owner_scope_key,
                       canonical_source_instance_key
                FROM source_coverage_sets
                WHERE owner_id = 'runtime.usage-v2'
                "#,
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Vec<u8>>(1)?,
                        row.get::<_, Vec<u8>>(2)?,
                    ))
                },
            )
            .unwrap();
        drop(connection);
        let commits_before_stale_writer = count_rows(&fixture.database, "ingest_commits");
        let now = now_unix_ms().unwrap();
        let stale_writer = engine.commit_projection_versions(ProjectionVersionCommit {
            source_instance_id: u64::try_from(source_instance_id).unwrap(),
            reason: "test stale replay writer authorization".to_string(),
            started_at: now,
            committed_at: now,
            projection_versions: vec![ProjectionVersionUpdate {
                projection_id: USAGE_V2_PROJECTION_ID.to_string(),
                scope_key: owner_scope_key.clone(),
                desired_version: USAGE_V2_PROJECTION_VERSION,
                completed_version: None,
                readiness: ProjectionReadiness::Pending,
                detail: Some(USAGE_V2_REPLAY_PENDING_DETAIL.to_string()),
            }],
            coverage_sets: Vec::new(),
            coverage_preconditions: vec![
                crate::engine::source_coverage::DurableCoverageSetPrecondition {
                    owner_id: USAGE_V2_PROJECTION_ID.to_string(),
                    owner_scope_key,
                    family: USAGE_V2_PROJECTION_ID.to_string(),
                    family_version: USAGE_V2_PROJECTION_VERSION,
                    adapter_id: "claude-code".to_string(),
                    canonical_source_instance_key,
                    expected_content_digest: vec![0; 32],
                    expected_last_commit_seq: replay_authorization.last_commit_seq,
                },
            ],
            query_pack_selections: Vec::new(),
        });
        assert!(matches!(
            stale_writer,
            Err(EngineError::InvalidCommit(detail)) if detail.contains("authorization is stale")
        ));
        assert_eq!(
            count_rows(&fixture.database, "ingest_commits"),
            commits_before_stale_writer,
            "writer-side replay authorization failure must roll back the transition"
        );
        let replayed = engine
            .replay_fact_family_cancellable(
                crate::engine::FactFamilyReplayCommand {
                    adapter_id: replay_authorization.adapter_id.clone(),
                    configured_roots: vec![fixture.root.clone()],
                    project_id: project_id.clone(),
                    session_id: session_id.clone(),
                    owner_id: USAGE_V2_PROJECTION_ID.to_string(),
                    family: USAGE_V2_PROJECTION_ID.to_string(),
                    family_version: USAGE_V2_PROJECTION_VERSION,
                    expected_source_instance_ref: replay_authorization.source_instance_ref,
                    expected_content_digest_ref: replay_authorization.content_digest_ref,
                    expected_coverage_last_commit_seq: replay_authorization.last_commit_seq,
                    reason: "test corrected coverage replay".to_string(),
                },
                QueryCancellationToken::default(),
            )
            .unwrap();
        assert_eq!(replayed.contract_version, 1);
        assert_eq!(replayed.outcome.records_decoded, 1);
        let replayed_object = catalog_object(&engine, &fixture.root, "session-transcripts");
        assert_eq!(replayed_object.generation, repaired_object.generation + 1);
        assert_eq!(
            catalog_object(&engine, &fixture.root, "interpretation-settings").generation,
            settings_before,
            "fact-family replay must not reset a stream that does not declare the family"
        );
        let ready = projection_version_state(&fixture.database, "runtime.usage-v2");
        assert_eq!(ready.1, Some(1));
        assert_eq!(ready.2, "ready");
        assert_eq!(ready.4, None);
        let replacement = usage_v2_coverage_state(&fixture.database);
        assert_eq!(replacement.completeness, "complete");
        assert_eq!(
            replacement.point_status.as_deref(),
            Some("complete_through")
        );
        assert_eq!(replacement.error_codes, Vec::<String>::new());
        assert_eq!(
            count_rows(&fixture.database, "usage_v2_response_contributions"),
            1
        );
        assert_eq!(
            count_rows(&fixture.database, "source_record_errors"),
            1,
            "historical quarantine diagnostics remain auditable after replacement coverage"
        );

        let unchanged_after_replay = fixture.reconcile(&engine);
        assert_eq!(unchanged_after_replay.records_decoded, 0);
        assert_eq!(unchanged_after_replay.commits, 0);
    }

    #[test]
    fn bounded_usage_v2_replay_resumes_after_restart_without_replaying_new_generation() {
        const TEST_REPLAY_LIMIT: usize = 4;
        let fixture = ClaudeFixture::new();
        let transcript = fixture.transcript_path();
        std::fs::write(&transcript, b"{not-json}\n").unwrap();
        let engine = fixture.open_engine();
        assert_eq!(fixture.reconcile(&engine).records_quarantined, 1);

        let mut corrected = Vec::new();
        for index in 0..=TEST_REPLAY_LIMIT {
            corrected.extend(transcript_line(
                &format!("m{index}"),
                "bounded replay response",
            ));
        }
        std::fs::write(&transcript, corrected).unwrap();
        let repaired = fixture.reconcile(&engine);
        assert_eq!(repaired.records_decoded, (TEST_REPLAY_LIMIT + 1) as u32);
        assert_eq!(
            projection_version_state(&fixture.database, "runtime.usage-v2").2,
            "unavailable",
            "ordinary corrected ingestion cannot clear the old quarantine gap"
        );
        let baseline_generation =
            catalog_object(&engine, &fixture.root, "session-transcripts").generation;

        let adapter = ClaudeCodeAdapter::new();
        let spec = adapter
            .discover(&DiscoveryContext {
                configured_roots: vec![fixture.root.clone()],
                observed_at: now_unix_ms().unwrap(),
            })
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let partial = ObservationCoordinator::with_append_record_limit(
            Arc::clone(&engine),
            TEST_REPLAY_LIMIT,
        )
        .replay_declared_instance_fact_family(
            &adapter,
            spec,
            FactFamilyReplayRequest::usage_v2("test bounded restart replay"),
        )
        .unwrap();
        assert_eq!(partial.backlog_remaining, 1);
        let partial_projection = projection_version_state(&fixture.database, "runtime.usage-v2");
        assert_eq!(partial_projection.2, "pending");
        assert_eq!(
            partial_projection.4.as_deref(),
            Some(USAGE_V2_REPLAY_PENDING_DETAIL)
        );
        let partial_generation =
            catalog_object(&engine, &fixture.root, "session-transcripts").generation;
        assert_eq!(partial_generation, baseline_generation + 1);
        assert_eq!(
            count_rows(&fixture.database, "usage_v2_response_contributions"),
            TEST_REPLAY_LIMIT as i64,
            "the first new-generation slice atomically retracts the old generation"
        );
        engine.shutdown().unwrap();
        drop(engine);

        let restarted = fixture.open_engine();
        let resumed = fixture.reconcile(&restarted);
        assert_eq!(resumed.records_decoded, 1);
        assert_eq!(resumed.backlog_remaining, 0);
        assert_eq!(
            catalog_object(&restarted, &fixture.root, "session-transcripts").generation,
            partial_generation,
            "restart must continue the replay generation instead of restarting it"
        );
        let ready = projection_version_state(&fixture.database, "runtime.usage-v2");
        assert_eq!(ready.1, Some(USAGE_V2_PROJECTION_VERSION as i64));
        assert_eq!(ready.2, "ready");
        assert_eq!(ready.4, None);
        assert_eq!(
            count_rows(&fixture.database, "usage_v2_response_contributions"),
            (TEST_REPLAY_LIMIT + 1) as i64,
            "resumed replay must neither retain old-generation rows nor duplicate new ones"
        );
        assert_eq!(
            usage_v2_coverage_state(&fixture.database).completeness,
            "complete"
        );
    }

    #[test]
    fn cancellation_during_decode_prevents_the_pending_record_commit() {
        let fixture = ClaudeFixture::new();
        std::fs::write(
            fixture.transcript_path(),
            transcript_line("m1", "cancelled"),
        )
        .unwrap();
        let engine = fixture.open_engine();
        let cancellation = QueryCancellationToken::default();
        let adapter = CancelOnDecodeAdapter {
            inner: ClaudeCodeAdapter::new(),
            cancellation: cancellation.clone(),
        };

        let error = ObservationCoordinator::with_cancellation(Arc::clone(&engine), cancellation)
            .reconcile(
                &adapter,
                ReconcileRequest::manual(vec![fixture.root.clone()]),
            )
            .unwrap_err();

        assert!(matches!(error, EngineError::QueryCancelled));
        assert_eq!(count_rows(&fixture.database, "canonical_messages"), 0);
        assert_eq!(
            count_rows(&fixture.database, "ingest_commits"),
            0,
            "cancelled first decode must not persist a pending catalog row"
        );
        assert!(
            catalog_object_opt(&engine, &fixture.root, "session-transcripts").is_none(),
            "an unpersisted object must stay absent until its first data commit"
        );
    }

    #[test]
    fn adapter_decode_panic_is_contained_without_committing_the_record() {
        let fixture = ClaudeFixture::new();
        std::fs::write(fixture.transcript_path(), transcript_line("m1", "private")).unwrap();
        let engine = fixture.open_engine();
        let adapter = PanicOnDecodeAdapter {
            inner: ClaudeCodeAdapter::new(),
        };

        let error = ObservationCoordinator::new(Arc::clone(&engine))
            .reconcile(
                &adapter,
                ReconcileRequest::manual(vec![fixture.root.clone()]),
            )
            .unwrap_err();

        assert!(matches!(
            error,
            EngineError::Observation {
                operation: "decode source record",
                ..
            }
        ));
        assert_eq!(count_rows(&fixture.database, "canonical_messages"), 0);
        assert_eq!(count_rows(&fixture.database, "ingest_commits"), 0);
        assert!(
            catalog_object_opt(&engine, &fixture.root, "session-transcripts").is_none(),
            "a panicked first decode must not leave a pending catalog row"
        );
    }

    #[test]
    fn stable_snapshot_retry_state_survives_restart_and_becomes_quarantine() {
        let fixture = ClaudeFixture::new();
        std::fs::write(
            fixture.root.join("settings.json"),
            br#"{"model":"malformed-to-adapter"}"#,
        )
        .unwrap();
        let adapter = RetryEverySnapshotAdapter {
            inner: ClaudeCodeAdapter::new(),
        };

        let first = fixture.open_engine();
        let initial = ObservationCoordinator::new(Arc::clone(&first))
            .reconcile(
                &adapter,
                ReconcileRequest::manual(vec![fixture.root.clone()]),
            )
            .unwrap();
        assert_eq!(initial.retries_required, 1);
        let retrying = catalog_object(&first, &fixture.root, "interpretation-settings");
        assert_eq!(retrying.state, "retrying");
        assert!(retrying.retry_state.is_some());
        first.shutdown().unwrap();

        std::thread::sleep(Duration::from_millis(110));
        let restarted = fixture.open_engine();
        let settled = ObservationCoordinator::new(Arc::clone(&restarted))
            .reconcile(
                &adapter,
                ReconcileRequest::manual(vec![fixture.root.clone()]),
            )
            .unwrap();
        assert_eq!(settled.retries_required, 0);
        assert_eq!(settled.records_quarantined, 1);
        let quarantined = catalog_object(&restarted, &fixture.root, "interpretation-settings");
        assert_eq!(quarantined.state, "quarantined");
        assert!(quarantined.retry_state.is_none());
        assert_eq!(count_rows(&fixture.database, "source_record_errors"), 1);
        assert_eq!(
            record_error_state(&fixture.database),
            ("record_permanent".to_string(), 1)
        );
    }

    #[test]
    fn directory_snapshot_stream_persists_membership_and_reconciles_changes() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("source");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("first.json"), b"one").unwrap();
        std::fs::write(root.join("ignored.txt"), b"ignored").unwrap();
        let database = temp.path().join("directory.db");
        let engine = open_test_engine(database);
        let adapter = DirectoryOnlyAdapter::new();

        let initial = ObservationCoordinator::new(Arc::clone(&engine))
            .reconcile(&adapter, ReconcileRequest::manual(vec![root.clone()]))
            .unwrap();
        assert_eq!(initial.objects_registered, 1);
        assert_eq!(initial.objects_changed, 1);
        let first = directory_catalog_object(&engine, &root);
        let first_checkpoint = DirectoryCheckpoint::decode_for_config(
            first.driver_checkpoint.as_deref().unwrap(),
            &DirectorySnapshotConfig {
                max_entries: 100,
                max_entries_per_directory: 100,
                max_depth: 4,
            },
        )
        .unwrap();
        assert_eq!(first_checkpoint.entries.len(), 1);
        assert!(first_checkpoint
            .entries
            .values()
            .any(|entry| entry.display_path == "first.json"));

        std::fs::remove_file(root.join("first.json")).unwrap();
        std::fs::write(root.join("second.json"), b"two").unwrap();
        let changed = ObservationCoordinator::new(Arc::clone(&engine))
            .reconcile(&adapter, ReconcileRequest::manual(vec![root.clone()]))
            .unwrap();
        assert_eq!(changed.objects_changed, 1);
        let second = directory_catalog_object(&engine, &root);
        let second_checkpoint = DirectoryCheckpoint::decode_for_config(
            second.driver_checkpoint.as_deref().unwrap(),
            &DirectorySnapshotConfig {
                max_entries: 100,
                max_entries_per_directory: 100,
                max_depth: 4,
            },
        )
        .unwrap();
        assert_eq!(second_checkpoint.entries.len(), 1);
        assert!(second_checkpoint
            .entries
            .values()
            .any(|entry| entry.display_path == "second.json"));

        let unchanged = ObservationCoordinator::new(Arc::clone(&engine))
            .reconcile(&adapter, ReconcileRequest::manual(vec![root]))
            .unwrap();
        assert_eq!(unchanged.commits, 0);
        assert_eq!(unchanged.objects_unchanged, 1);
    }

    #[test]
    fn directory_snapshot_resume_rejects_a_checkpoint_above_the_current_stream_bound() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("source");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("first.json"), b"one").unwrap();
        std::fs::write(root.join("second.json"), b"two").unwrap();
        let engine = open_test_engine(temp.path().join("directory-bound.db"));

        ObservationCoordinator::new(Arc::clone(&engine))
            .reconcile(
                &DirectoryOnlyAdapter::with_max_entries(2),
                ReconcileRequest::manual(vec![root.clone()]),
            )
            .unwrap();
        let before = directory_catalog_object(&engine, &root)
            .driver_checkpoint
            .unwrap();

        // Leave a source tree that fits the new declaration. Resume must still
        // reject the two-entry stored checkpoint before scanning under a
        // narrower authority than the one that created it.
        std::fs::remove_file(root.join("second.json")).unwrap();
        let result = ObservationCoordinator::new(Arc::clone(&engine)).reconcile(
            &DirectoryOnlyAdapter::with_max_entries(1),
            ReconcileRequest::manual(vec![root.clone()]),
        );
        assert!(
            matches!(
                &result,
                Err(EngineError::Observation { detail, .. })
                    if detail.contains("configured limit 1")
            ),
            "unexpected narrowed-bound resume result: {result:?}"
        );
        assert_eq!(
            directory_catalog_object(&engine, &root).driver_checkpoint,
            Some(before)
        );
    }

    #[test]
    fn database_snapshot_streams_commit_atomic_replacements_through_the_common_runtime() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("source");
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("source.db");
        Connection::open(&source)
            .unwrap()
            .execute_batch(
                "CREATE TABLE items(id INTEGER PRIMARY KEY, value TEXT);\n\
                 INSERT INTO items VALUES (1, 'one'), (2, 'two');\n\
                 CREATE TABLE state(key TEXT PRIMARY KEY, value BLOB);\n\
                 INSERT INTO state VALUES ('agent.one', x'31'), ('other', x'32');",
            )
            .unwrap();
        let database = temp.path().join("engine.db");
        let engine = open_test_engine(database.clone());
        let adapter = DatabaseSnapshotAdapter::new();

        let initial = ObservationCoordinator::new(Arc::clone(&engine))
            .reconcile(&adapter, ReconcileRequest::manual(vec![root.clone()]))
            .unwrap();
        assert_eq!(initial.records_decoded, 3);
        assert_eq!(stream_fact_count(&database, "sqlite-items"), 2);
        assert_eq!(stream_fact_count(&database, "key-values"), 1);

        Connection::open(&source)
            .unwrap()
            .execute_batch(
                "DELETE FROM items WHERE id = 2;\n\
                 UPDATE state SET value = x'39' WHERE key = 'other';",
            )
            .unwrap();
        let changed = ObservationCoordinator::new(Arc::clone(&engine))
            .reconcile(&adapter, ReconcileRequest::manual(vec![root.clone()]))
            .unwrap();
        assert_eq!(changed.records_decoded, 1);
        assert_eq!(stream_fact_count(&database, "sqlite-items"), 1);
        assert_eq!(stream_fact_count(&database, "key-values"), 1);

        Connection::open(&source)
            .unwrap()
            .execute("DELETE FROM state WHERE key = 'agent.one'", [])
            .unwrap();
        let removed = ObservationCoordinator::new(Arc::clone(&engine))
            .reconcile(&adapter, ReconcileRequest::manual(vec![root]))
            .unwrap();
        assert_eq!(removed.records_decoded, 0);
        assert_eq!(stream_fact_count(&database, "key-values"), 0);
        engine.shutdown().unwrap();
    }

    #[test]
    fn production_scheduler_bounds_parallel_object_decode() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("source");
        std::fs::create_dir_all(&root).unwrap();
        for index in 0..8 {
            std::fs::write(root.join(format!("{index}.json")), b"{}").unwrap();
        }
        let engine = open_test_engine(temp.path().join("parallel.db"));
        let adapter = ParallelSnapshotAdapter::new();

        let outcome = ObservationCoordinator::new(Arc::clone(&engine))
            .reconcile(&adapter, ReconcileRequest::manual(vec![root]))
            .unwrap();

        assert_eq!(outcome.records_decoded, 8);
        assert_eq!(
            outcome.objects_registered, 8,
            "each new object is allocated locally without a pending catalog commit"
        );
        assert_eq!(
            count_rows(&temp.path().join("parallel.db"), "ingest_commits"),
            8,
            "pipelined new objects must not pay a second pending-only transaction"
        );
        let maximum = adapter.maximum.load(Ordering::Acquire);
        assert!(
            maximum >= 2,
            "independent objects did not decode in parallel"
        );
        assert!(
            maximum <= MAX_OBJECTS_IN_FLIGHT,
            "decode concurrency exceeded the common bound: {maximum}"
        );
    }

    #[test]
    fn replace_delete_and_recreate_retract_and_restore_settings() {
        let fixture = ClaudeFixture::new();
        let settings = fixture.root.join("settings.json");
        std::fs::write(
            &settings,
            br#"{"model":"claude-sonnet","permissions":{"allow":["Read"]}}"#,
        )
        .unwrap();
        let engine = fixture.open_engine();

        let initial = fixture.reconcile(&engine);
        assert_eq!(initial.records_decoded, 1);
        assert_eq!(
            count_rows(
                &fixture.database,
                "canonical_interpretation_settings_documents"
            ),
            1
        );

        std::fs::remove_file(&settings).unwrap();
        let removed = fixture.reconcile(&engine);
        assert_eq!(removed.objects_removed, 1);
        assert_eq!(
            count_rows(
                &fixture.database,
                "canonical_interpretation_settings_documents"
            ),
            0
        );
        let absent = catalog_object(&engine, &fixture.root, "interpretation-settings");
        assert_eq!(absent.state, "absent");
        let absent_generation = absent.generation;

        std::fs::write(&settings, br#"{"model":"claude-opus"}"#).unwrap();
        let recreated = fixture.reconcile(&engine);
        assert_eq!(recreated.records_decoded, 1);
        assert_eq!(
            count_rows(
                &fixture.database,
                "canonical_interpretation_settings_documents"
            ),
            1
        );
        let present = catalog_object(&engine, &fixture.root, "interpretation-settings");
        assert_eq!(present.state, "active");
        assert_eq!(present.generation, absent_generation + 1);
    }

    #[test]
    fn append_delete_and_recreate_stays_generation_monotonic() {
        let fixture = ClaudeFixture::new();
        let transcript = fixture.transcript_path();
        std::fs::write(&transcript, transcript_line("m1", "first")).unwrap();
        let engine = fixture.open_engine();
        fixture.reconcile(&engine);
        let first = catalog_object(&engine, &fixture.root, "session-transcripts");

        std::fs::remove_file(&transcript).unwrap();
        let removed = fixture.reconcile(&engine);
        assert_eq!(removed.objects_removed, 1);
        assert_eq!(count_rows(&fixture.database, "canonical_messages"), 0);
        let absent = catalog_object(&engine, &fixture.root, "session-transcripts");
        assert_eq!(absent.generation, first.generation + 1);
        assert_eq!(absent.state, "absent");
        assert!(absent.driver_checkpoint.is_none());

        std::fs::write(&transcript, b"").unwrap();
        let empty_recreated = fixture.reconcile(&engine);
        assert_eq!(empty_recreated.records_decoded, 0);
        assert_eq!(empty_recreated.objects_changed, 1);
        let empty = catalog_object(&engine, &fixture.root, "session-transcripts");
        assert_eq!(empty.generation, absent.generation + 1);
        assert_eq!(empty.state, "active");
        assert!(empty.driver_checkpoint.is_some());

        std::fs::write(&transcript, transcript_line("m2", "recreated")).unwrap();
        let recreated = fixture.reconcile(&engine);
        assert_eq!(recreated.records_decoded, 1);
        assert_eq!(count_rows(&fixture.database, "canonical_messages"), 1);
        let present = catalog_object(&engine, &fixture.root, "session-transcripts");
        assert_eq!(present.generation, empty.generation);
        assert_eq!(present.state, "active");
    }

    #[test]
    fn invalid_matching_object_is_quarantined_without_stalling_other_streams() {
        let fixture = ClaudeFixture::new();
        let transcript = fixture.transcript_path();
        std::fs::write(&transcript, transcript_line("m1", "valid")).unwrap();
        let sessions = fixture.root.join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let lookalike = sessions.join("notes.json");
        std::fs::write(&lookalike, br#"{"note":"not a process presence"}"#).unwrap();
        let engine = fixture.open_engine();

        let initial = fixture.reconcile(&engine);
        assert_eq!(initial.records_decoded, 1);
        assert_eq!(initial.records_quarantined, 1);
        assert_eq!(count_rows(&fixture.database, "canonical_messages"), 1);
        assert_eq!(count_rows(&fixture.database, "source_record_errors"), 1);
        assert_eq!(
            projection_version_state(&fixture.database, "runtime.usage-v2").2,
            "ready",
            "an unrelated presence-sidecar quarantine must not block transcript usage coverage"
        );
        let quarantined = catalog_object(&engine, &fixture.root, "active-sessions");
        assert_eq!(quarantined.state, "quarantined");

        let unchanged = fixture.reconcile(&engine);
        assert_eq!(unchanged.records_quarantined, 0);
        assert_eq!(count_rows(&fixture.database, "source_record_errors"), 1);

        std::fs::remove_file(lookalike).unwrap();
        let removed = fixture.reconcile(&engine);
        assert_eq!(removed.objects_removed, 1);
        let absent = catalog_object(&engine, &fixture.root, "active-sessions");
        assert_eq!(absent.state, "absent");
    }

    #[test]
    fn reconcile_lifecycle_reports_live_retry_and_failure_states() {
        let fixture = ClaudeFixture::new();
        let transcript = fixture.transcript_path();
        std::fs::write(&transcript, transcript_line("m1", "complete")).unwrap();
        let engine = fixture.open_engine();

        assert_eq!(engine.status().observation.state, "idle");
        let live = fixture.reconcile(&engine);
        let live_status = engine.status().observation;
        assert_eq!(live_status.state, "live");
        assert_eq!(live_status.reconciles_total, 1);
        assert_eq!(live_status.last_commit_seq, live.last_commit_seq);

        let mut append = std::fs::OpenOptions::new()
            .append(true)
            .open(&transcript)
            .unwrap();
        append.write_all(br#"{"type":"assistant"}"#).unwrap();
        append.flush().unwrap();
        let retry = fixture.reconcile(&engine);
        assert_eq!(retry.retries_required, 1);
        assert_eq!(retry.incomplete_tail_retries, 1);
        let degraded = engine.status().observation;
        assert_eq!(degraded.state, "degraded");
        assert!(!degraded.full_reconcile_required);
        assert_eq!(degraded.dirty_instances, 1);
        assert_eq!(degraded.retry_signals_total, 1);
        assert!(!engine.health().healthy);

        let missing = fixture.root.join("missing-root");
        let error = ObservationCoordinator::new(Arc::clone(&engine))
            .reconcile(
                &ClaudeCodeAdapter::new(),
                ReconcileRequest::manual(vec![missing]),
            )
            .unwrap_err();
        assert!(matches!(error, EngineError::Observation { .. }));
        let failed = engine.status().observation;
        assert_eq!(failed.state, "degraded");
        assert_eq!(failed.failed_reconciles_total, 1);
        assert!(failed.last_error.is_some());
    }

    struct ClaudeFixture {
        _temp: TempDir,
        root: PathBuf,
        database: PathBuf,
    }

    struct CancelOnDecodeAdapter {
        inner: ClaudeCodeAdapter,
        cancellation: QueryCancellationToken,
    }

    struct PanicOnDecodeAdapter {
        inner: ClaudeCodeAdapter,
    }

    impl AgentAdapter for PanicOnDecodeAdapter {
        fn manifest(&self) -> &crate::adapter::AdapterManifest {
            self.inner.manifest()
        }

        fn discover(
            &self,
            context: &DiscoveryContext,
        ) -> Result<Vec<AdapterSourceInstanceSpec>, AdapterError> {
            self.inner.discover(context)
        }

        fn streams(&self, instance: &SourceInstance) -> Result<Vec<StreamSpec>, AdapterError> {
            self.inner.streams(instance)
        }

        fn bootstrap_object(
            &self,
            instance: &SourceInstance,
            object: &SourceObjectDescriptor,
        ) -> Result<AdapterObjectContext, AdapterError> {
            self.inner.bootstrap_object(instance, object)
        }

        fn decode(
            &self,
            _context: DecodeContext<'_>,
            _record: &SourceRecord,
            _output: &mut FactBatch,
        ) -> Result<DecodeDisposition, AdapterError> {
            panic!("private panic payload must not cross the boundary")
        }
    }

    struct RetryEverySnapshotAdapter {
        inner: ClaudeCodeAdapter,
    }

    impl AgentAdapter for RetryEverySnapshotAdapter {
        fn manifest(&self) -> &crate::adapter::AdapterManifest {
            self.inner.manifest()
        }

        fn discover(
            &self,
            context: &DiscoveryContext,
        ) -> Result<Vec<AdapterSourceInstanceSpec>, AdapterError> {
            self.inner.discover(context)
        }

        fn streams(&self, instance: &SourceInstance) -> Result<Vec<StreamSpec>, AdapterError> {
            self.inner.streams(instance)
        }

        fn bootstrap_object(
            &self,
            instance: &SourceInstance,
            object: &SourceObjectDescriptor,
        ) -> Result<AdapterObjectContext, AdapterError> {
            self.inner.bootstrap_object(instance, object)
        }

        fn decode(
            &self,
            _context: DecodeContext<'_>,
            _record: &SourceRecord,
            _output: &mut FactBatch,
        ) -> Result<DecodeDisposition, AdapterError> {
            Ok(DecodeDisposition::RetryTransient)
        }
    }

    struct DirectoryOnlyAdapter {
        manifest: AdapterManifest,
        max_entries: usize,
    }

    impl DirectoryOnlyAdapter {
        fn new() -> Self {
            Self::with_max_entries(100)
        }

        fn with_max_entries(max_entries: usize) -> Self {
            Self {
                manifest: synthetic_manifest("synthetic-directory"),
                max_entries,
            }
        }
    }

    impl AgentAdapter for DirectoryOnlyAdapter {
        fn manifest(&self) -> &AdapterManifest {
            &self.manifest
        }

        fn discover(
            &self,
            context: &DiscoveryContext,
        ) -> Result<Vec<AdapterSourceInstanceSpec>, AdapterError> {
            synthetic_discovery(context)
        }

        fn streams(&self, _instance: &SourceInstance) -> Result<Vec<StreamSpec>, AdapterError> {
            Ok(vec![StreamSpec {
                id: StreamId::new("membership")?,
                driver: DriverSpec::DirectorySnapshot(DirectorySnapshotConfig {
                    max_entries: self.max_entries,
                    max_entries_per_directory: self.max_entries,
                    max_depth: 4,
                }),
                selector: ObjectSelector {
                    root_name: "root".to_string(),
                    include: vec!["*.json".to_string()],
                    exclude: Vec::new(),
                },
                decoder: DecoderId::new("membership-v1")?,
                authority: StreamAuthority::Supplemental,
                entity_scope: EntityScope::Instance,
                priority: IngestPriority::Maintenance,
                consistency: ConsistencyPolicy::SnapshotDiff,
                deletion: DeletionPolicy::MirrorSource,
                retention: RawRetentionPolicy::None,
                capabilities: Vec::new(),
            }])
        }

        fn decode(
            &self,
            _context: DecodeContext<'_>,
            _record: &SourceRecord,
            _output: &mut FactBatch,
        ) -> Result<DecodeDisposition, AdapterError> {
            panic!("directory membership streams do not emit adapter records")
        }
    }

    struct DatabaseSnapshotAdapter {
        manifest: AdapterManifest,
    }

    impl DatabaseSnapshotAdapter {
        fn new() -> Self {
            Self {
                manifest: synthetic_manifest("synthetic-database"),
            }
        }
    }

    impl AgentAdapter for DatabaseSnapshotAdapter {
        fn manifest(&self) -> &AdapterManifest {
            &self.manifest
        }

        fn discover(
            &self,
            context: &DiscoveryContext,
        ) -> Result<Vec<AdapterSourceInstanceSpec>, AdapterError> {
            synthetic_discovery(context)
        }

        fn streams(&self, _instance: &SourceInstance) -> Result<Vec<StreamSpec>, AdapterError> {
            let sqlite = SqliteSnapshotConfig::bounded(vec![SqliteQuerySpec {
                name: "items".to_string(),
                sql: "SELECT id, value FROM items".to_string(),
                key_columns: vec!["id".to_string()],
            }]);
            let mut key_values = KeyValueSnapshotConfig::bounded(
                "state",
                "SELECT key, value FROM state",
                "key",
                "value",
            );
            key_values.key_prefixes = vec![b"agent.".to_vec()];
            Ok(vec![
                database_stream(
                    "sqlite-items",
                    "sqlite-row-v1",
                    DriverSpec::SqliteSnapshot(sqlite),
                )?,
                database_stream(
                    "key-values",
                    "key-value-v1",
                    DriverSpec::KeyValueSnapshot(key_values),
                )?,
            ])
        }

        fn decode(
            &self,
            context: DecodeContext<'_>,
            record: &SourceRecord,
            output: &mut FactBatch,
        ) -> Result<DecodeDisposition, AdapterError> {
            let native_kind = match context.decoder.as_str() {
                "sqlite-row-v1" => {
                    let row = SqliteRowRecord::decode(&record.payload).map_err(|error| {
                        AdapterError::new(
                            AdapterErrorClass::RecordPermanent,
                            "invalid_sqlite_row",
                            error.to_string(),
                        )
                    })?;
                    format!("sqlite:{}", hex_key(&row.row_key))
                }
                "key-value-v1" => {
                    let entry = KeyValueRecord::decode(&record.payload).map_err(|error| {
                        AdapterError::new(
                            AdapterErrorClass::RecordPermanent,
                            "invalid_key_value_record",
                            error.to_string(),
                        )
                    })?;
                    format!("key-value:{}", String::from_utf8_lossy(&entry.key))
                }
                _ => return Err(AdapterError::unknown_decoder(context.decoder)),
            };
            output.push(
                record,
                Fact::UnknownRecord {
                    native_kind: Some(native_kind),
                    raw_payload: record.payload.clone(),
                    reason: "synthetic database conformance".to_string(),
                },
            )?;
            Ok(DecodeDisposition::PreservedUnknown)
        }
    }

    fn database_stream(
        stream: &str,
        decoder: &str,
        driver: DriverSpec,
    ) -> Result<StreamSpec, AdapterError> {
        Ok(StreamSpec {
            id: StreamId::new(stream)?,
            driver,
            selector: ObjectSelector {
                root_name: "root".to_string(),
                include: vec!["source.db".to_string()],
                exclude: Vec::new(),
            },
            decoder: DecoderId::new(decoder)?,
            authority: StreamAuthority::Canonical,
            entity_scope: EntityScope::Instance,
            priority: IngestPriority::Maintenance,
            consistency: ConsistencyPolicy::SnapshotDiff,
            deletion: DeletionPolicy::MirrorSource,
            retention: RawRetentionPolicy::HashOnly,
            capabilities: Vec::new(),
        })
    }

    fn hex_key(value: &[u8]) -> String {
        value.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    struct ParallelSnapshotAdapter {
        manifest: AdapterManifest,
        active: AtomicUsize,
        maximum: AtomicUsize,
    }

    impl ParallelSnapshotAdapter {
        fn new() -> Self {
            Self {
                manifest: synthetic_manifest("synthetic-parallel"),
                active: AtomicUsize::new(0),
                maximum: AtomicUsize::new(0),
            }
        }
    }

    impl AgentAdapter for ParallelSnapshotAdapter {
        fn manifest(&self) -> &AdapterManifest {
            &self.manifest
        }

        fn discover(
            &self,
            context: &DiscoveryContext,
        ) -> Result<Vec<AdapterSourceInstanceSpec>, AdapterError> {
            synthetic_discovery(context)
        }

        fn streams(&self, _instance: &SourceInstance) -> Result<Vec<StreamSpec>, AdapterError> {
            Ok(vec![StreamSpec {
                id: StreamId::new("snapshots")?,
                driver: DriverSpec::ReplaceDocument(ReplaceDocumentConfig {
                    max_document_bytes: 1_024,
                }),
                selector: ObjectSelector {
                    root_name: "root".to_string(),
                    include: vec!["*.json".to_string()],
                    exclude: Vec::new(),
                },
                decoder: DecoderId::new("snapshot-v1")?,
                authority: StreamAuthority::Canonical,
                entity_scope: EntityScope::Instance,
                priority: IngestPriority::Interactive,
                consistency: ConsistencyPolicy::SnapshotReplace,
                deletion: DeletionPolicy::MirrorSource,
                retention: RawRetentionPolicy::HashOnly,
                capabilities: Vec::new(),
            }])
        }

        fn decode(
            &self,
            _context: DecodeContext<'_>,
            _record: &SourceRecord,
            _output: &mut FactBatch,
        ) -> Result<DecodeDisposition, AdapterError> {
            let active = self.active.fetch_add(1, Ordering::AcqRel) + 1;
            self.maximum.fetch_max(active, Ordering::AcqRel);
            std::thread::sleep(Duration::from_millis(25));
            self.active.fetch_sub(1, Ordering::AcqRel);
            Ok(DecodeDisposition::IgnoredKnown)
        }
    }

    fn synthetic_manifest(id: &str) -> AdapterManifest {
        AdapterManifest {
            id: AdapterId::new(id).unwrap(),
            display_name: id.to_string(),
            adapter_version: "1.0.0".to_string(),
            contract_version: 1,
            support_binding: None,
            scope_programs: None,
            source_schema_versions: Vec::new(),
            capabilities: Vec::new(),
        }
    }

    fn synthetic_discovery(
        context: &DiscoveryContext,
    ) -> Result<Vec<AdapterSourceInstanceSpec>, AdapterError> {
        context
            .configured_roots
            .iter()
            .map(|root| {
                let canonical = root.canonicalize().map_err(|error| {
                    AdapterError::new(
                        AdapterErrorClass::Transient,
                        "root_unavailable",
                        error.to_string(),
                    )
                })?;
                Ok(AdapterSourceInstanceSpec {
                    identity_contract_version: 1,
                    stable_key: SourceInstanceKey::new(platform_path_key(&canonical))?,
                    display_name: canonical.to_string_lossy().into_owned(),
                    roots: vec![SourceRoot {
                        name: "root".to_string(),
                        path: canonical,
                    }],
                    discovery_reason: "synthetic coordinator test".to_string(),
                })
            })
            .collect()
    }

    impl AgentAdapter for CancelOnDecodeAdapter {
        fn manifest(&self) -> &crate::adapter::AdapterManifest {
            self.inner.manifest()
        }

        fn discover(
            &self,
            context: &DiscoveryContext,
        ) -> Result<Vec<AdapterSourceInstanceSpec>, AdapterError> {
            self.inner.discover(context)
        }

        fn streams(&self, instance: &SourceInstance) -> Result<Vec<StreamSpec>, AdapterError> {
            self.inner.streams(instance)
        }

        fn bootstrap_object(
            &self,
            instance: &SourceInstance,
            object: &SourceObjectDescriptor,
        ) -> Result<AdapterObjectContext, AdapterError> {
            self.inner.bootstrap_object(instance, object)
        }

        fn decode(
            &self,
            context: DecodeContext<'_>,
            record: &SourceRecord,
            output: &mut FactBatch,
        ) -> Result<DecodeDisposition, AdapterError> {
            self.cancellation.cancel();
            self.inner.decode(context, record, output)
        }
    }

    impl ClaudeFixture {
        fn new() -> Self {
            let temp = TempDir::new().unwrap();
            let root = temp.path().join("source");
            std::fs::create_dir_all(root.join(format!("projects/{PROJECT}"))).unwrap();
            Self {
                database: temp.path().join("engine.db"),
                _temp: temp,
                root,
            }
        }

        fn transcript_path(&self) -> PathBuf {
            self.root
                .join(format!("projects/{PROJECT}/{SESSION}.jsonl"))
        }

        fn open_engine(&self) -> Arc<SpaghettiEngineCore> {
            SpaghettiEngineCore::open_with_registry(
                EngineOptions {
                    database_path: self.database.clone(),
                    query_workers: Some(1),
                    owner_label: Some("coordinator-test".to_string()),
                    defer_query_structures: false,
                },
                AdapterRegistry::builder()
                    .register(ClaudeCodeAdapter::new())
                    .build_legacy()
                    .unwrap(),
            )
            .unwrap()
        }

        fn reconcile(&self, engine: &Arc<SpaghettiEngineCore>) -> ReconcileOutcome {
            ObservationCoordinator::new(Arc::clone(engine))
                .reconcile(
                    &ClaudeCodeAdapter::new(),
                    ReconcileRequest::manual(vec![self.root.clone()]),
                )
                .unwrap()
        }
    }

    fn open_test_engine(database_path: PathBuf) -> Arc<SpaghettiEngineCore> {
        SpaghettiEngineCore::open(EngineOptions {
            database_path,
            query_workers: Some(1),
            owner_label: Some("synthetic-coordinator-test".to_string()),
            defer_query_structures: false,
        })
        .unwrap()
    }

    fn directory_catalog_object(engine: &SpaghettiEngineCore, root: &Path) -> SourceCatalogObject {
        engine
            .source_catalog(
                "synthetic-directory",
                &platform_path_key(&root.canonicalize().unwrap()),
            )
            .unwrap()
            .objects
            .into_iter()
            .find(|object| object.stream_key == "membership")
            .unwrap()
    }

    const PROJECT: &str = "-Users-fixture-project";
    const SESSION: &str = "01234567-89ab-cdef-0123-456789abcdef";

    fn transcript_line(message_id: &str, text: &str) -> Vec<u8> {
        let mut line = format!(
            r#"{{"type":"assistant","uuid":"{message_id}","timestamp":"2026-08-12T00:00:00Z","sessionId":"{SESSION}","cwd":"/fixture/project","version":"1","gitBranch":"main","isSidechain":false,"userType":"external","requestId":"request-{message_id}","message":{{"model":"claude-sonnet","id":"api-{message_id}","type":"message","role":"assistant","content":[{{"type":"text","text":"{text}"}}],"usage":{{"input_tokens":1,"output_tokens":1}}}}}}"#
        )
        .into_bytes();
        line.push(b'\n');
        line
    }

    fn count_rows(database: &Path, table: &str) -> i64 {
        let connection =
            Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let sql = match table {
            "canonical_messages" => "SELECT COUNT(*) FROM canonical_messages",
            "ingest_commits" => "SELECT COUNT(*) FROM ingest_commits",
            "source_record_errors" => "SELECT COUNT(*) FROM source_record_errors",
            "canonical_interpretation_settings_documents" => {
                "SELECT COUNT(*) FROM canonical_interpretation_settings_documents"
            }
            "usage_v2_response_contributions" => {
                "SELECT COUNT(*) FROM usage_v2_response_contributions"
            }
            _ => panic!("unsupported coordinator test table"),
        };
        connection.query_row(sql, [], |row| row.get(0)).unwrap()
    }

    fn canonical_project_session_ids(database: &Path) -> (String, String) {
        let connection =
            Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let (project_key, session_key) = connection
            .query_row(
                "SELECT project_key, session_key FROM canonical_sessions LIMIT 1",
                [],
                |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?)),
            )
            .unwrap();
        (
            super::super::query_identity::encode_entity_id(
                super::super::query_identity::PROJECT_ID_PREFIX,
                &project_key,
            ),
            super::super::query_identity::encode_entity_id(
                super::super::query_identity::SESSION_ID_PREFIX,
                &session_key,
            ),
        )
    }

    #[derive(Debug, PartialEq, Eq)]
    struct UsageV2CoverageState {
        set_contract_version: i64,
        coverage_contract_version: i64,
        domain_kind: String,
        domain_name: String,
        domain_version: i64,
        adapter_id: String,
        support_release_id: String,
        completeness: String,
        content_digest: Vec<u8>,
        last_commit_seq: i64,
        point_count: i64,
        absence_count: i64,
        point_status: Option<String>,
        unavailable_reason: Option<String>,
        position_kind: Option<String>,
        monotonic_order: Option<u64>,
        error_codes: Vec<String>,
    }

    fn usage_v2_coverage_state(database: &Path) -> UsageV2CoverageState {
        let connection =
            Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let (
            coverage_set_id,
            set_contract_version,
            coverage_contract_version,
            domain_kind,
            domain_name,
            domain_version,
            adapter_id,
            support_release_id,
            completeness,
            content_digest,
            last_commit_seq,
        ) = connection
            .query_row(
                r#"
                SELECT coverage_set_id, coverage_set_contract_version,
                       coverage_contract_version, domain_kind, domain_name,
                       domain_version, adapter_id, support_release_id,
                       completeness, content_digest, last_commit_seq
                FROM source_coverage_sets
                WHERE owner_id = 'runtime.usage-v2'
                "#,
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                        row.get(9)?,
                        row.get(10)?,
                    ))
                },
            )
            .unwrap();
        let point = connection
            .query_row(
                r#"
                SELECT status, unavailable_reason, position_kind, monotonic_order
                FROM source_coverage_points
                WHERE coverage_set_id = ?1
                ORDER BY stream_key, object_key, generation
                LIMIT 1
                "#,
                [coverage_set_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<u64>>(3)?,
                    ))
                },
            )
            .optional()
            .unwrap();
        let point_count = connection
            .query_row(
                "SELECT COUNT(*) FROM source_coverage_points WHERE coverage_set_id = ?1",
                [coverage_set_id],
                |row| row.get(0),
            )
            .unwrap();
        let absence_count = connection
            .query_row(
                "SELECT COUNT(*) FROM source_coverage_absences WHERE coverage_set_id = ?1",
                [coverage_set_id],
                |row| row.get(0),
            )
            .unwrap();
        let mut error_statement = connection
            .prepare(
                "SELECT error_code FROM source_coverage_errors WHERE coverage_set_id = ?1 ORDER BY error_ordinal",
            )
            .unwrap();
        let error_codes = error_statement
            .query_map([coverage_set_id], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<String>, _>>()
            .unwrap();
        let (point_status, unavailable_reason, position_kind, monotonic_order) = point
            .map(|(status, reason, kind, order)| (Some(status), reason, kind, order))
            .unwrap_or((None, None, None, None));
        UsageV2CoverageState {
            set_contract_version,
            coverage_contract_version,
            domain_kind,
            domain_name,
            domain_version,
            adapter_id,
            support_release_id,
            completeness,
            content_digest,
            last_commit_seq,
            point_count,
            absence_count,
            point_status,
            unavailable_reason,
            position_kind,
            monotonic_order,
            error_codes,
        }
    }

    fn record_error_state(database: &Path) -> (String, i64) {
        let connection =
            Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        connection
            .query_row(
                "SELECT error_class, retry_count FROM source_record_errors LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap()
    }

    fn projection_version_state(
        database: &Path,
        projection_id: &str,
    ) -> (i64, Option<i64>, String, i64, Option<String>) {
        let connection =
            Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        connection
            .query_row(
                r#"
                SELECT desired_version, completed_version, readiness,
                       last_commit_seq, detail
                FROM projection_versions
                WHERE projection_id = ?1
                "#,
                [projection_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap()
    }

    fn catalog_object(
        engine: &SpaghettiEngineCore,
        root: &Path,
        stream: &str,
    ) -> SourceCatalogObject {
        catalog_object_opt(engine, root, stream).unwrap()
    }

    fn catalog_object_opt(
        engine: &SpaghettiEngineCore,
        root: &Path,
        stream: &str,
    ) -> Option<SourceCatalogObject> {
        let adapter = ClaudeCodeAdapter::new();
        engine
            .source_catalog(adapter.manifest().id.as_str(), &canonical_root_key(root))
            .unwrap()
            .objects
            .into_iter()
            .find(|object| object.stream_key == stream)
    }

    fn initial_source_instance_id(database: &Path) -> u64 {
        let connection =
            Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        connection
            .query_row(
                "SELECT source_instance_id FROM source_instances LIMIT 1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map(|value| u64::try_from(value).unwrap())
            .unwrap()
    }

    fn count_with_instance_id(database: &Path, instance_id: u64) -> i64 {
        let connection =
            Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        connection
            .query_row(
                "SELECT COUNT(*) FROM fact_records WHERE source_instance_id = ?1",
                [i64::try_from(instance_id).unwrap()],
                |row| row.get(0),
            )
            .unwrap()
    }

    fn stream_fact_count(database: &Path, stream_key: &str) -> i64 {
        let connection =
            Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        connection
            .query_row(
                r#"
                SELECT COUNT(*)
                FROM fact_records AS fact
                JOIN source_streams AS stream
                  ON stream.source_stream_id = fact.source_stream_id
                WHERE stream.stream_key = ?1
                "#,
                [stream_key],
                |row| row.get(0),
            )
            .unwrap()
    }

    fn first_blob(database: &Path, query: &str) -> Vec<u8> {
        let connection =
            Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        connection.query_row(query, [], |row| row.get(0)).unwrap()
    }

    fn canonical_root_key(root: &Path) -> Vec<u8> {
        crate::source::platform_path_key(&std::fs::canonicalize(root).unwrap())
    }
}
