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
    Availability, CapabilityGranularity, DecodeContext, DecodeDisposition, DeletionPolicy,
    DependencyRevision, DiscoveryContext, DriverSpec, FactBatch, RawRetentionPolicy, SourceAccess,
    SourceInstance, SourceInstanceSpec as AdapterSourceInstanceSpec, SourceListedObject,
    SourceObjectDescriptor, SourceObjectList, SourceObjectListRequest, SourceQuery, SourceRows,
    SourceSnapshot, StreamAuthority, StreamSpec, SupportLevel,
};
use crate::source::{
    confined_relative_path_key, AccessBudget, AccessBudgetError, AccessBudgetSnapshot,
    AccessObjectToken, AccessOperation, AccessOutcome, AccessPhase, AccessReservation,
    AccessReservationRequest, AppendCheckpoint, AppendDelimitedFile, AppendItem, AppendRead,
    BoundedScheduler, DirectoryCheckpoint, DirectoryEntryKind, DirectoryScan, DirectorySelection,
    DirectorySnapshot, DirtyReason, KeyValueCheckpoint, KeyValueRead, KeyValueSnapshot,
    MalformedRevisionGuard, MalformedRevisionPolicy, ParseFailureDecision, PresenceCheckpoint,
    PresenceObject, PresenceRead, RecordOrigin, ReplaceCheckpoint, ReplaceDocument, ReplaceRead,
    Revision, ScheduleOutcome, ScheduledWork, ScopeAccessBounds, SourceCursor, SourceDriverError,
    SourceMediaType, SourceRecord, SqliteCheckpoint, SqliteRead, SqliteSnapshot, WorkKey,
};

use super::commit::{
    source_object_catalog_id, source_stream_catalog_id, CommitReceipt, ExpectedSourceCursor,
    ObservationCommit, SourceCapabilitySpec, SourceInstanceSpec, SourceObjectUpdate,
    SourceRecordError, SourceStreamSpec,
};
use super::performance::{SourceDecodeObservation, SourceDecodeOutcome, SourcePerformanceRecorder};
use super::query_pool::{QueryCancellationToken, SourceCatalogObject, SourceCatalogSnapshot};
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

pub struct ObservationCoordinator {
    engine: Arc<SpaghettiEngineCore>,
    cancellations: Vec<QueryCancellationToken>,
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
        }
    }

    pub fn with_cancellation(
        engine: Arc<SpaghettiEngineCore>,
        cancellation: QueryCancellationToken,
    ) -> Self {
        Self {
            engine,
            cancellations: vec![cancellation],
        }
    }

    pub(crate) fn with_cancellations(
        engine: Arc<SpaghettiEngineCore>,
        cancellations: Vec<QueryCancellationToken>,
    ) -> Self {
        Self {
            engine,
            cancellations,
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
                self.reconcile_instance(adapter, spec, &request.reason, started_at, &mut outcome)?;
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
            self.reconcile_instance(adapter, spec, &reason, started_at, &mut outcome)?;
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
            let stream =
                catch_adapter_panic("declare retry source stream", || adapter.streams(&instance))?
                    .map_err(|error| adapter_error("declare retry source stream", error))?
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
            lease.begin_reconciling();
            let mut lane = CommitLane::new(Arc::clone(&self.engine));
            self.reconcile_object(
                adapter,
                &instance,
                &stream,
                &object,
                Some(previous),
                &reason,
                started_at,
                &mut outcome,
                &mut lane,
            )?;
            lane.flush()?;
            lane.apply_to(&mut outcome);
            Ok(outcome)
        })();
        self.finish_reconcile(lease, result, started_at)
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
    ) -> Result<(), EngineError> {
        self.check_cancelled()?;
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
            outcome.streams_reconciled = outcome.streams_reconciled.saturating_add(1);
            scheduled_objects.extend(self.collect_stream_work(
                adapter,
                &instance,
                &stream,
                &catalog,
                &mut discovery,
                reason,
                started_at,
                outcome,
            )?);
        }
        self.execute_scheduled_objects(
            adapter,
            &instance,
            scheduled_objects,
            reason,
            started_at,
            outcome,
        )?;
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
            work.push(ObjectWork {
                stream: stream.clone(),
                object: object.clone(),
                previous,
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
            .map(DirectoryCheckpoint::decode)
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
                            reason,
                            started_at,
                            &mut local,
                            &mut lane,
                        );
                        if result.is_err() {
                            let _ = lane.flush();
                        }
                        lane.apply_to(&mut local);
                        let _ = result_tx.send((key, result, local, lane.pending.take()));
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
                    Ok((key, result, local, pending)) => {
                        in_flight = in_flight.saturating_sub(1);
                        if !scheduler.complete(&key) {
                            first_error.get_or_insert(observation_error(
                                "complete source object work",
                                "scheduler lost an in-flight object key",
                            ));
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
        outcome.records_quarantined = outcome.records_quarantined.saturating_add(1);
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
        reason: &str,
        started_at: i64,
        outcome: &mut ReconcileOutcome,
        lane: &mut CommitLane,
    ) -> Result<(), EngineError> {
        let mut config = config.clone();
        config.max_records_per_batch = config
            .max_records_per_batch
            .min(MAX_APPEND_RECORDS_PER_COMMIT);
        let driver = AppendDelimitedFile::new(config).map_err(source_error)?;
        let performance = self.source_performance(adapter, stream);
        let mut previous = durable
            .driver_checkpoint
            .as_deref()
            .map(AppendCheckpoint::decode)
            .transpose()
            .map_err(source_error)?;
        let mut force_contract_replay = durable.driver_checkpoint.is_some()
            && (durable.decoder_contract_changed(adapter)
                || durable.object_context_changed(object_context));
        let mut records_seen = 0_usize;
        loop {
            self.check_cancelled()?;
            if records_seen >= MAX_APPEND_RECORDS_PER_RECONCILE {
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
                                        stream,
                                        object_context,
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
                        outcome.records_quarantined = outcome
                            .records_quarantined
                            .saturating_add(quarantined_count);
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
        let generation_reset = durable.decoder_contract_changed(adapter)
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
                outcome.records_quarantined = outcome.records_quarantined.saturating_add(1);
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
        let decoded = decode_record(
            adapter,
            stream,
            object_context,
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
                    outcome.records_quarantined = outcome.records_quarantined.saturating_add(1);
                    outcome.objects_changed = outcome.objects_changed.saturating_add(1);
                    Ok(())
                }
            };
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
        if decoded.quarantined {
            outcome.records_quarantined = outcome.records_quarantined.saturating_add(1);
        }
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
        let force_replay = durable.decoder_contract_changed(adapter)
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
        let force_replay = durable.decoder_contract_changed(adapter)
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
        let decoded = decode_snapshot_records(
            adapter,
            stream,
            object_context,
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
        outcome.records_quarantined = outcome
            .records_quarantined
            .saturating_add(decoded.quarantined_records);
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
        let contract_replay = durable.decoder_contract_changed(adapter);
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
                let decoded = decode_record(
                    adapter,
                    stream,
                    object_context,
                    source_access,
                    &performance,
                    &record,
                    prior_decoder_state.as_deref(),
                )?;
                if decoded.disposition == DecodeDisposition::RetryTransient {
                    outcome.retries_required = outcome.retries_required.saturating_add(1);
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
                if decoded.quarantined {
                    outcome.records_quarantined = outcome.records_quarantined.saturating_add(1);
                }
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
        let components = normal_components(path);
        let Some(components) = components else {
            return false;
        };
        self.include
            .iter()
            .any(|pattern| pattern.matches(&components))
            && !self
                .exclude
                .iter()
                .any(|pattern| pattern.matches(&components))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GlobPattern(Vec<GlobComponent>);

#[derive(Debug, Clone, PartialEq, Eq)]
enum GlobComponent {
    Recursive,
    Segment(Vec<u8>),
}

impl GlobPattern {
    fn new(pattern: &str) -> Result<Self, String> {
        if pattern.is_empty() || pattern.starts_with('/') || pattern.ends_with('/') {
            return Err("selector must be a non-empty relative path".to_string());
        }
        let mut components = Vec::new();
        for component in pattern.split('/') {
            if component.is_empty() || component == "." || component == ".." {
                return Err("selector contains an invalid path component".to_string());
            }
            if component == "**" {
                if !matches!(components.last(), Some(GlobComponent::Recursive)) {
                    components.push(GlobComponent::Recursive);
                }
            } else if component.contains("**") {
                return Err("recursive wildcard must occupy a whole component".to_string());
            } else {
                components.push(GlobComponent::Segment(component.as_bytes().to_vec()));
            }
        }
        Ok(Self(components))
    }

    fn matches(&self, path: &[Vec<u8>]) -> bool {
        matches_components(&self.0, path)
    }
}

fn matches_components(pattern: &[GlobComponent], path: &[Vec<u8>]) -> bool {
    match (pattern.first(), path.first()) {
        (None, None) => true,
        (None, Some(_)) => false,
        (Some(GlobComponent::Recursive), _) => {
            matches_components(&pattern[1..], path)
                || (!path.is_empty() && matches_components(pattern, &path[1..]))
        }
        (Some(GlobComponent::Segment(segment)), Some(component)) => {
            matches_segment(segment, component) && matches_components(&pattern[1..], &path[1..])
        }
        (Some(GlobComponent::Segment(_)), None) => false,
    }
}

fn matches_segment(pattern: &[u8], value: &[u8]) -> bool {
    let mut states = vec![false; value.len() + 1];
    states[0] = true;
    for token in pattern {
        if *token == b'*' {
            for index in 1..=value.len() {
                states[index] = states[index] || states[index - 1];
            }
        } else {
            for index in (1..=value.len()).rev() {
                states[index] = states[index - 1] && (*token == b'?' || *token == value[index - 1]);
            }
            states[0] = false;
        }
    }
    states[value.len()]
}

fn normal_components(path: &Path) -> Option<Vec<Vec<u8>>> {
    path.components()
        .map(|component| match component {
            Component::Normal(value) => Some(os_bytes(value)),
            Component::CurDir => Some(Vec::new()),
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => None,
        })
        .filter(|component| component.as_ref().is_none_or(|value| !value.is_empty()))
        .collect()
}

#[cfg(unix)]
fn os_bytes(value: &OsStr) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    value.as_bytes().to_vec()
}

#[cfg(windows)]
fn os_bytes(value: &OsStr) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;
    value.encode_wide().flat_map(u16::to_be_bytes).collect()
}

#[cfg(not(any(unix, windows)))]
fn os_bytes(value: &OsStr) -> Vec<u8> {
    value.to_string_lossy().as_bytes().to_vec()
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
}

struct DecodedSnapshot {
    batch: FactBatch,
    errors: Vec<SourceRecordError>,
    next_decoder_state: Option<Vec<u8>>,
    quarantined_records: u32,
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

fn decode_snapshot_records<A: AgentAdapter + ?Sized>(
    adapter: &A,
    stream: &StreamSpec,
    object_context: &AdapterObjectContext,
    source_access: &ConfinedSourceAccess<'_>,
    performance: &SourcePerformanceRecorder,
    records: &[SourceRecord],
    mut decoder_state: Option<Vec<u8>>,
) -> Result<Option<DecodedSnapshot>, EngineError> {
    let mut batch = FactBatch::new(FACT_BATCH_LIMIT, DIAGNOSTIC_LIMIT)
        .map_err(|error| adapter_error("create database snapshot fact batch", error))?;
    let mut errors = Vec::new();
    let mut quarantined_records = 0_u32;
    for record in records {
        let decoded = decode_record(
            adapter,
            stream,
            object_context,
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
        batch
            .append(decoded.batch)
            .map_err(|error| adapter_error("merge database snapshot facts", error))?;
    }
    Ok(Some(DecodedSnapshot {
        batch,
        errors,
        next_decoder_state: decoder_state,
        quarantined_records,
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
    stream: &StreamSpec,
    object_context: &AdapterObjectContext,
    source_access: &ConfinedSourceAccess<'_>,
    performance: &SourcePerformanceRecorder,
    record: &SourceRecord,
    decoder_state: Option<&[u8]>,
) -> Result<DecodedRecord, EngineError> {
    let started = Instant::now();
    let mut adapter_elapsed = std::time::Duration::ZERO;
    let mut batch = match FactBatch::new(FACT_BATCH_LIMIT, DIAGNOSTIC_LIMIT) {
        Ok(batch) => batch,
        Err(error) => {
            performance.record_decode(&SourceDecodeObservation {
                elapsed: started.elapsed(),
                adapter_elapsed: std::time::Duration::ZERO,
                fact_build: std::time::Duration::ZERO,
                outcome: SourceDecodeOutcome::Failed,
            });
            return Err(adapter_error("create fact batch", error));
        }
    };
    let decoded = (|| {
        let adapter_started = Instant::now();
        let adapter_result = catch_adapter_panic("decode source record", || {
            adapter.decode_with_access(
                DecodeContext {
                    decoder: &stream.decoder,
                    object_context,
                    decoder_state,
                },
                record,
                &mut batch,
                source_access,
            )
        });
        adapter_elapsed = adapter_started.elapsed();
        let disposition =
            adapter_result?.map_err(|error| adapter_error("decode source record", error))?;
        let fact_count = batch.facts().len();
        match disposition {
            DecodeDisposition::Applied if fact_count == 0 => {
                return Err(observation_error(
                    "validate decode disposition",
                    format!(
                        "adapter returned Applied without facts for stream {}",
                        stream.id
                    ),
                ));
            }
            DecodeDisposition::IgnoredKnown | DecodeDisposition::RetryTransient
                if fact_count != 0 =>
            {
                return Err(observation_error(
                    "validate decode disposition",
                    format!(
                        "adapter returned {disposition:?} with {fact_count} facts for stream {}",
                        stream.id
                    ),
                ));
            }
            _ => {}
        }
        for dependency in source_access.revisions()? {
            batch
                .add_dependency_read(dependency)
                .map_err(|error| adapter_error("record source dependency", error))?;
        }
        match stream.retention {
            RawRetentionPolicy::Full => {}
            RawRetentionPolicy::DiagnosticExcerpt => {
                let excerpt = diagnostic_excerpt(&record.payload);
                batch.replace_unknown_record_payloads(&excerpt);
            }
            RawRetentionPolicy::HashOnly | RawRetentionPolicy::None => {
                batch.redact_unknown_record_payloads();
            }
        }
        let quarantined = batch
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.class == AdapterErrorClass::RecordPermanent);
        let errors = batch
            .diagnostics()
            .iter()
            .map(|diagnostic| SourceRecordError {
                generation: record.generation,
                cursor_start: record.cursor_start.as_bytes().to_vec(),
                cursor_end: record.cursor_end.as_bytes().to_vec(),
                payload_hash: record.payload_hash.as_bytes().to_vec(),
                media_type: record.media_type.as_str().to_string(),
                raw_payload: retained_diagnostic_payload(stream.retention, &record.payload),
                error_class: adapter_error_class(diagnostic.class).to_string(),
                error_message: format!("{}: {}", diagnostic.code, diagnostic.message),
                adapter_version: adapter.manifest().adapter_version.clone(),
                contract_version: adapter.manifest().contract_version,
                last_retry_at: None,
            })
            .collect();
        let next_decoder_state = batch.next_decoder_state().map(ToOwned::to_owned);
        Ok((disposition, errors, next_decoder_state, quarantined))
    })();
    let fact_build_time = batch.fact_build_time();
    let fact_count = u64::try_from(batch.facts().len()).unwrap_or(u64::MAX);
    let outcome = match &decoded {
        Ok((DecodeDisposition::RetryTransient, _, _, _)) => SourceDecodeOutcome::Retry,
        Ok((_, _, _, quarantined)) => SourceDecodeOutcome::Decoded {
            facts: fact_count,
            quarantined: *quarantined,
        },
        Err(_) => SourceDecodeOutcome::Failed,
    };
    performance.record_decode(&SourceDecodeObservation {
        elapsed: started.elapsed(),
        adapter_elapsed,
        fact_build: fact_build_time,
        outcome,
    });
    let (disposition, errors, next_decoder_state, quarantined) = decoded?;
    Ok(DecodedRecord {
        disposition,
        batch,
        errors,
        next_decoder_state,
        quarantined,
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
        projection_versions: Vec::new(),
        record_errors,
        changes: Vec::new(),
    })
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

const MAX_DIAGNOSTIC_EXCERPT_BYTES: usize = 1_024;
const MAX_DIAGNOSTIC_SHAPE_ITEMS: usize = 16;

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
            crate::source::StableRead::Missing => (
                None,
                *blake3::hash(b"spaghetti/source-dependency/missing/v1").as_bytes(),
                false,
            ),
            crate::source::StableRead::Unstable => {
                return Err(SourceDriverError::Unstable(
                    relative_path.to_string_lossy().into_owned(),
                ))
            }
            crate::source::StableRead::Oversized(stamp) => {
                let mut hasher = blake3::Hasher::new();
                hasher.update(b"spaghetti/source-dependency/oversized/v1");
                hasher.update(&stamp.len.to_be_bytes());
                hasher.update(&stamp.modified_ns.to_be_bytes());
                (None, *hasher.finalize().as_bytes(), true)
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

/// Produce useful quarantine context without retaining native values or even
/// native JSON property names. Dynamic property names can themselves contain
/// secrets, so only their hashes and value kinds are exposed.
fn diagnostic_excerpt(payload: &[u8]) -> Vec<u8> {
    let payload_hash = blake3::hash(payload).to_hex().to_string();
    let shape = match serde_json::from_slice::<serde_json::Value>(payload) {
        Ok(serde_json::Value::Object(object)) => {
            let keys = object
                .iter()
                .take(MAX_DIAGNOSTIC_SHAPE_ITEMS)
                .map(|(key, value)| {
                    let key_hash = blake3::hash(key.as_bytes()).to_hex().to_string();
                    serde_json::json!({
                        "key_hash": &key_hash[..12],
                        "value_kind": json_value_kind(value),
                    })
                })
                .collect::<Vec<_>>();
            serde_json::json!({
                "kind": "json_object",
                "bytes": payload.len(),
                "hash": payload_hash,
                "members": object.len(),
                "shape": keys,
                "truncated": object.len() > MAX_DIAGNOSTIC_SHAPE_ITEMS,
            })
        }
        Ok(serde_json::Value::Array(array)) => {
            let items = array
                .iter()
                .take(MAX_DIAGNOSTIC_SHAPE_ITEMS)
                .map(json_value_kind)
                .collect::<Vec<_>>();
            serde_json::json!({
                "kind": "json_array",
                "bytes": payload.len(),
                "hash": payload_hash,
                "items": array.len(),
                "item_kinds": items,
                "truncated": array.len() > MAX_DIAGNOSTIC_SHAPE_ITEMS,
            })
        }
        Ok(value) => serde_json::json!({
            "kind": json_value_kind(&value),
            "bytes": payload.len(),
            "hash": payload_hash,
        }),
        Err(_) => serde_json::json!({
            "kind": "opaque",
            "bytes": payload.len(),
            "hash": payload_hash,
        }),
    };
    let encoded = serde_json::to_vec(&shape).unwrap_or_else(|_| {
        format!(r#"{{"kind":"redacted","bytes":{}}}"#, payload.len()).into_bytes()
    });
    debug_assert!(encoded.len() <= MAX_DIAGNOSTIC_EXCERPT_BYTES);
    if encoded.len() <= MAX_DIAGNOSTIC_EXCERPT_BYTES {
        encoded
    } else {
        encoded[..MAX_DIAGNOSTIC_EXCERPT_BYTES].to_vec()
    }
}

fn json_value_kind(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
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

    use rusqlite::{Connection, OpenFlags};
    use tempfile::TempDir;

    use crate::adapter::{
        AdapterId, AdapterManifest, ConsistencyPolicy, DecoderId, EntityScope, Fact,
        ObjectSelector, SourceInstanceKey, SourceRoot, StreamId,
    };
    use crate::claude::ClaudeCodeAdapter;
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
            1,
            "new objects must persist identity in the first data commit, not a pending-only catalog transaction"
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
            outcome.commits, 1,
            "a new append object persists identity in the same data commit as its first records"
        );
        assert_eq!(count_rows(&fixture.database, "canonical_messages"), 10);
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
        let first_checkpoint =
            DirectoryCheckpoint::decode(first.driver_checkpoint.as_deref().unwrap()).unwrap();
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
        let second_checkpoint =
            DirectoryCheckpoint::decode(second.driver_checkpoint.as_deref().unwrap()).unwrap();
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
    }

    impl DirectoryOnlyAdapter {
        fn new() -> Self {
            Self {
                manifest: synthetic_manifest("synthetic-directory"),
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
                    max_entries: 100,
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
            SpaghettiEngineCore::open(EngineOptions {
                database_path: self.database.clone(),
                query_workers: Some(1),
                owner_label: Some("coordinator-test".to_string()),
                defer_query_structures: false,
            })
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
            _ => panic!("unsupported coordinator test table"),
        };
        connection.query_row(sql, [], |row| row.get(0)).unwrap()
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
