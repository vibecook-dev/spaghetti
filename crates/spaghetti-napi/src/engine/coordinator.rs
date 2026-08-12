//! Adapter-neutral declared-object reconciliation for RFC 011.
//!
//! A reconcile is intentionally deterministic and synchronous at this layer:
//! native watcher hints and bounded schedulers can call the same operation
//! without gaining a second framing, decode, or commit path.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs::Metadata;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use walkdir::WalkDir;

use crate::adapter::{
    AdapterError, AdapterErrorClass, AdapterObjectContext, AgentAdapter, DecodeContext,
    DecodeDisposition, DeletionPolicy, DiscoveryContext, DriverSpec, FactBatch, RawRetentionPolicy,
    SourceInstance, SourceInstanceSpec as AdapterSourceInstanceSpec, SourceObjectDescriptor,
    StreamAuthority, StreamSpec,
};
use crate::source::{
    confined_relative_path_key, AppendCheckpoint, AppendDelimitedFile, AppendItem, AppendRead,
    PresenceCheckpoint, PresenceObject, PresenceRead, RecordOrigin, ReplaceCheckpoint,
    ReplaceDocument, ReplaceRead, Revision, SourceCursor, SourceDriverError, SourceMediaType,
    SourceRecord,
};

use super::commit::{
    CommitReceipt, ExpectedSourceCursor, ObservationCommit, SourceInstanceSpec, SourceObjectUpdate,
    SourceRecordError, SourceStreamSpec,
};
use super::query_pool::{SourceCatalogObject, SourceCatalogSnapshot};
use super::{EngineError, SpaghettiEngineCore};

const FACT_BATCH_LIMIT: usize = 4_096;
const DIAGNOSTIC_LIMIT: usize = 256;
const DISCOVERY_MAX_DEPTH: usize = 64;
const DISCOVERY_MAX_ENTRIES: usize = 250_000;
const MAX_APPEND_RECORDS_PER_RECONCILE: usize = 4_096;

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
    pub commits: u32,
    pub last_commit_seq: Option<u64>,
}

pub struct ObservationCoordinator {
    engine: Arc<SpaghettiEngineCore>,
}

impl ObservationCoordinator {
    pub fn new(engine: Arc<SpaghettiEngineCore>) -> Self {
        Self { engine }
    }

    pub fn reconcile<A: AgentAdapter + ?Sized>(
        &self,
        adapter: &A,
        request: ReconcileRequest,
    ) -> Result<ReconcileOutcome, EngineError> {
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
            let instances = adapter
                .discover(&DiscoveryContext {
                    configured_roots: request.configured_roots,
                    observed_at: started_at,
                })
                .map_err(|error| adapter_error("discover source instances", error))?;
            let mut outcome = ReconcileOutcome {
                instances_discovered: bounded_u32(instances.len()),
                ..ReconcileOutcome::default()
            };
            let mut instance_keys = BTreeSet::new();
            lease.begin_reconciling();
            for spec in instances {
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
            let mut outcome = ReconcileOutcome {
                instances_discovered: 1,
                ..ReconcileOutcome::default()
            };
            self.reconcile_instance(adapter, spec, &reason, started_at, &mut outcome)?;
            Ok(outcome)
        })();
        self.finish_reconcile(lease, result, started_at)
    }

    fn finish_reconcile(
        &self,
        lease: super::ObservationLease,
        result: Result<ReconcileOutcome, EngineError>,
        started_at: i64,
    ) -> Result<ReconcileOutcome, EngineError> {
        let finished_at = now_unix_ms().unwrap_or(started_at);
        match result {
            Ok(outcome) => match self.engine.overview() {
                Ok(overview) => {
                    lease.complete(&outcome, overview.commit_seq, finished_at);
                    Ok(outcome)
                }
                Err(error) => {
                    lease.fail(&error, finished_at);
                    Err(error)
                }
            },
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
        let manifest = adapter.manifest();
        let catalog = self
            .engine
            .source_catalog(manifest.id.as_str(), spec.stable_key.as_bytes())?;
        // Entity keys emitted by adapters include the durable source-instance
        // ID. Reserve it before decoding and refresh discovery metadata even
        // when this instance currently declares no matching objects.
        let source_instance_id = self.reserve_instance(adapter, &spec, started_at)?;
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
        let streams = adapter
            .streams(&instance)
            .map_err(|error| adapter_error("declare source streams", error))?;
        let mut stream_ids = BTreeSet::new();
        for stream in streams {
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
            self.reconcile_stream(
                adapter, &instance, &stream, &catalog, reason, started_at, outcome,
            )?;
        }
        Ok(())
    }

    fn reserve_instance<A: AgentAdapter + ?Sized>(
        &self,
        adapter: &A,
        spec: &AdapterSourceInstanceSpec,
        started_at: i64,
    ) -> Result<u64, EngineError> {
        self.engine.reserve_source_instance(SourceInstanceSpec {
            adapter_id: adapter.manifest().id.as_str().to_string(),
            stable_key: spec.stable_key.as_bytes().to_vec(),
            display_name: spec.display_name.clone(),
            adapter_contract_version: adapter.manifest().contract_version,
            discovered_at: started_at,
            last_seen_at: started_at,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn reconcile_stream<A: AgentAdapter + ?Sized>(
        &self,
        adapter: &A,
        instance: &SourceInstance,
        stream: &StreamSpec,
        catalog: &SourceCatalogSnapshot,
        reason: &str,
        started_at: i64,
        outcome: &mut ReconcileOutcome,
    ) -> Result<(), EngineError> {
        if matches!(stream.driver, DriverSpec::DirectorySnapshot(_)) {
            return Err(observation_error(
                "dispatch source stream",
                format!(
                    "stream {} declares DirectorySnapshot, which produces membership changes rather than adapter records",
                    stream.id
                ),
            ));
        }
        if stream.authority == StreamAuthority::IgnoredDerived {
            return Ok(());
        }
        let root = instance
            .root(&stream.selector.root_name)
            .map_err(|error| adapter_error("resolve source root", error))?;
        let discovered = discover_objects(root, stream)?;
        if !discovered.available {
            outcome.streams_unavailable = outcome.streams_unavailable.saturating_add(1);
            return Ok(());
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

        for object in discovered.objects.values() {
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
            self.reconcile_object(
                adapter,
                instance,
                stream,
                object,
                previous.as_ref(),
                reason,
                started_at,
                outcome,
            )?;
        }

        if stream.deletion == DeletionPolicy::MirrorSource {
            for previous in stored_by_key
                .values()
                .filter(|object| object.state != "absent")
            {
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
                self.reconcile_object(
                    adapter,
                    instance,
                    stream,
                    &object,
                    Some(previous),
                    reason,
                    started_at,
                    outcome,
                )?;
            }
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
    ) -> Result<(), EngineError> {
        let object_context = adapter
            .bootstrap_object(instance, &object.descriptor)
            .map_err(|error| adapter_error("bootstrap source object", error))?;
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

        let origin = RecordOrigin {
            source_instance_id: durable.source_instance_id,
            stream_id: durable.source_stream_id,
            object_id: durable.source_object_id,
            observed_at: now_unix_ms()?,
            source_timestamp_hint: None,
            media_type: media_type(&object.path)?,
        };
        match &stream.driver {
            DriverSpec::AppendDelimited(config) => self.reconcile_append(
                adapter,
                instance,
                stream,
                object,
                &object_context,
                durable,
                &origin,
                config,
                reason,
                started_at,
                outcome,
            ),
            DriverSpec::ReplaceDocument(config) => self.reconcile_replace(
                adapter,
                instance,
                stream,
                object,
                &object_context,
                durable,
                &origin,
                config,
                reason,
                started_at,
                outcome,
            ),
            DriverSpec::Presence(config) => self.reconcile_presence(
                adapter,
                instance,
                stream,
                object,
                &object_context,
                durable,
                &origin,
                config,
                reason,
                started_at,
                outcome,
            ),
            DriverSpec::DirectorySnapshot(_) => unreachable!("rejected before object discovery"),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn register_object<A: AgentAdapter + ?Sized>(
        &self,
        adapter: &A,
        instance: &SourceInstance,
        stream: &StreamSpec,
        object: &DeclaredObject,
        object_context: &AdapterObjectContext,
        reason: &str,
        started_at: i64,
        outcome: &mut ReconcileOutcome,
    ) -> Result<DurableObject, EngineError> {
        let initial_cursor = match stream.driver {
            DriverSpec::AppendDelimited(_) => SourceCursor::append_offset(0),
            DriverSpec::ReplaceDocument(_) => SourceCursor::snapshot(Revision::ZERO),
            DriverSpec::Presence(_) => SourceCursor::presence(Revision::ZERO),
            DriverSpec::DirectorySnapshot(_) => unreachable!("not an adapter record stream"),
        };
        let request = commit_request(
            adapter,
            instance,
            stream,
            object,
            object_context,
            ExpectedSourceCursor::Absent,
            1,
            initial_cursor.into_bytes(),
            None,
            None,
            None,
            "pending",
            Vec::new(),
            reason,
            started_at,
        )?;
        let receipt = self.engine.commit_observation(request)?;
        record_commit(outcome, &receipt);
        outcome.objects_registered = outcome.objects_registered.saturating_add(1);
        Ok(DurableObject {
            source_instance_id: receipt.source_instance_id,
            source_stream_id: receipt.source_stream_id,
            source_object_id: receipt.source_object_id,
            generation: 1,
            committed_cursor: initial_cursor_bytes(stream),
            driver_checkpoint: None,
            driver_checkpoint_version: None,
            decoder_state: None,
            decoder_state_version: None,
            decoder_contract_version: adapter.manifest().contract_version,
            state: "pending".to_string(),
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn reconcile_append<A: AgentAdapter + ?Sized>(
        &self,
        adapter: &A,
        instance: &SourceInstance,
        stream: &StreamSpec,
        object: &DeclaredObject,
        object_context: &AdapterObjectContext,
        mut durable: DurableObject,
        origin: &RecordOrigin,
        config: &crate::source::AppendDelimitedConfig,
        reason: &str,
        started_at: i64,
        outcome: &mut ReconcileOutcome,
    ) -> Result<(), EngineError> {
        let mut config = config.clone();
        config.max_records_per_batch = 1;
        let driver = AppendDelimitedFile::new(config).map_err(source_error)?;
        let mut previous = durable
            .driver_checkpoint
            .as_deref()
            .map(AppendCheckpoint::decode)
            .transpose()
            .map_err(source_error)?;
        let mut force_contract_replay =
            durable.driver_checkpoint.is_some() && durable.decoder_contract_changed(adapter);
        let mut records_seen = 0_usize;
        loop {
            if records_seen == MAX_APPEND_RECORDS_PER_RECONCILE {
                outcome.retries_required = outcome.retries_required.saturating_add(1);
                return Ok(());
            }
            match driver
                .read(
                    &object.path,
                    previous.as_ref(),
                    origin,
                    force_contract_replay,
                )
                .map_err(source_error)?
            {
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
                            let receipt = if generation_changed {
                                self.engine.commit_facts(
                                    request,
                                    FactBatch::new(FACT_BATCH_LIMIT, DIAGNOSTIC_LIMIT).map_err(
                                        |error| {
                                            adapter_error(
                                                "create generation replacement fact batch",
                                                error,
                                            )
                                        },
                                    )?,
                                )?
                            } else {
                                self.engine.commit_observation(request)?
                            };
                            record_commit(outcome, &receipt);
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
                        debug_assert_eq!(items.len(), 1);
                        match &items[0] {
                            AppendItem::Record(record) => {
                                let decoded = decode_record(
                                    adapter,
                                    stream,
                                    object_context,
                                    record,
                                    prior_decoder_state.as_deref(),
                                )?;
                                if decoded.disposition == DecodeDisposition::RetryTransient {
                                    outcome.retries_required =
                                        outcome.retries_required.saturating_add(1);
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
                                    Some(checkpoint_bytes.clone()),
                                    decoded
                                        .next_decoder_state
                                        .clone()
                                        .or_else(|| prior_decoder_state.clone()),
                                    decoded
                                        .next_decoder_state
                                        .as_ref()
                                        .map(|_| adapter.manifest().contract_version)
                                        .or(prior_decoder_state_version),
                                    "active",
                                    decoded.errors,
                                    reason,
                                    started_at,
                                )?;
                                let receipt =
                                    if decoded.batch.facts().is_empty() && !generation_changed {
                                        self.engine.commit_observation(request)?
                                    } else {
                                        self.engine.commit_facts(request, decoded.batch)?
                                    };
                                record_commit(outcome, &receipt);
                                outcome.records_decoded = outcome.records_decoded.saturating_add(1);
                                if decoded.quarantined {
                                    outcome.records_quarantined =
                                        outcome.records_quarantined.saturating_add(1);
                                }
                                durable.decoder_state = decoded
                                    .next_decoder_state
                                    .or_else(|| prior_decoder_state.clone());
                                if durable.decoder_state.is_some() {
                                    durable.decoder_state_version =
                                        Some(adapter.manifest().contract_version);
                                }
                            }
                            AppendItem::Quarantined(quarantine) => {
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
                                    prior_decoder_state.clone(),
                                    prior_decoder_state_version,
                                    "active",
                                    vec![quarantine_error(adapter, origin, quarantine)],
                                    reason,
                                    started_at,
                                )?;
                                let receipt = if generation_changed {
                                    self.engine.commit_facts(
                                        request,
                                        FactBatch::new(FACT_BATCH_LIMIT, DIAGNOSTIC_LIMIT)
                                            .map_err(|error| {
                                                adapter_error(
                                                    "create generation quarantine fact batch",
                                                    error,
                                                )
                                            })?,
                                    )?
                                } else {
                                    self.engine.commit_observation(request)?
                                };
                                record_commit(outcome, &receipt);
                                durable.decoder_state = prior_decoder_state;
                                durable.decoder_state_version = prior_decoder_state_version;
                                outcome.records_quarantined =
                                    outcome.records_quarantined.saturating_add(1);
                            }
                        }
                        durable.advance(&checkpoint, checkpoint_bytes.clone());
                        durable.state = "active".to_string();
                        outcome.objects_changed = outcome.objects_changed.saturating_add(1);
                    }
                    previous = Some(checkpoint);
                    force_contract_replay = false;
                    if !more_available {
                        if needs_retry {
                            outcome.retries_required = outcome.retries_required.saturating_add(1);
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
        let receipt = self.engine.commit_facts(request, batch)?;
        record_commit(outcome, &receipt);
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
        object_context: &AdapterObjectContext,
        durable: DurableObject,
        origin: &RecordOrigin,
        config: &crate::source::ReplaceDocumentConfig,
        reason: &str,
        started_at: i64,
        outcome: &mut ReconcileOutcome,
    ) -> Result<(), EngineError> {
        let previous = durable
            .driver_checkpoint
            .as_deref()
            .map(ReplaceCheckpoint::decode)
            .transpose()
            .map_err(source_error)?;
        let generation_reset = durable.decoder_contract_changed(adapter);
        match ReplaceDocument::new(config.clone())
            .map_err(source_error)?
            .read(&object.path, previous.as_ref(), origin, generation_reset)
            .map_err(source_error)?
        {
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
                let receipt = self.engine.commit_observation(request)?;
                record_commit(outcome, &receipt);
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
                    &durable,
                    record,
                    checkpoint,
                    false,
                    reason,
                    started_at,
                    outcome,
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
                        &durable,
                        record,
                        checkpoint,
                        true,
                        reason,
                        started_at,
                        outcome,
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
                let receipt = self.engine.commit_facts(
                    request,
                    FactBatch::new(FACT_BATCH_LIMIT, DIAGNOSTIC_LIMIT).map_err(|error| {
                        adapter_error("create snapshot quarantine fact batch", error)
                    })?,
                )?;
                record_commit(outcome, &receipt);
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
        durable: &DurableObject,
        record: SourceRecord,
        checkpoint: ReplaceCheckpoint,
        removed: bool,
        reason: &str,
        started_at: i64,
        outcome: &mut ReconcileOutcome,
    ) -> Result<(), EngineError> {
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
        let receipt = self.engine.commit_facts(request, decoded.batch)?;
        record_commit(outcome, &receipt);
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
    fn reconcile_presence<A: AgentAdapter + ?Sized>(
        &self,
        adapter: &A,
        instance: &SourceInstance,
        stream: &StreamSpec,
        object: &DeclaredObject,
        object_context: &AdapterObjectContext,
        durable: DurableObject,
        origin: &RecordOrigin,
        config: &crate::source::PresenceObjectConfig,
        reason: &str,
        started_at: i64,
        outcome: &mut ReconcileOutcome,
    ) -> Result<(), EngineError> {
        let previous = durable
            .driver_checkpoint
            .as_deref()
            .map(PresenceCheckpoint::decode)
            .transpose()
            .map_err(source_error)?;
        let contract_replay = durable.decoder_contract_changed(adapter);
        match PresenceObject::new(config.clone())
            .map_err(source_error)?
            .read(
                &object.path,
                (!contract_replay).then_some(previous.as_ref()).flatten(),
                origin,
            )
            .map_err(source_error)?
        {
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
                let receipt = self.engine.commit_facts(request, decoded.batch)?;
                record_commit(outcome, &receipt);
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

struct DeclaredObject {
    path: PathBuf,
    descriptor: SourceObjectDescriptor,
    metadata: Option<Metadata>,
}

fn discover_objects(root: &Path, stream: &StreamSpec) -> Result<DiscoveredObjects, EngineError> {
    let root_metadata = match std::fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(DiscoveredObjects {
                available: false,
                objects: BTreeMap::new(),
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
    let patterns = SelectorPatterns::new(stream)?;
    let mut objects = BTreeMap::new();
    let mut entries = 0_usize;
    for entry in WalkDir::new(root)
        .min_depth(1)
        .max_depth(DISCOVERY_MAX_DEPTH + 1)
        .follow_links(false)
    {
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
        let relative = entry.path().strip_prefix(root).map_err(|_| {
            observation_error(
                "confine source object",
                entry.path().to_string_lossy().into_owned(),
            )
        })?;
        if !patterns.matches(relative) {
            continue;
        }
        let object_key = confined_relative_path_key(relative).map_err(source_error)?;
        let descriptor = SourceObjectDescriptor {
            stream_id: stream.id.clone(),
            object_key: object_key.clone(),
            relative_path: relative.to_path_buf(),
        };
        if objects
            .insert(
                object_key,
                DeclaredObject {
                    path: entry.path().to_path_buf(),
                    descriptor,
                    metadata: entry.metadata().ok(),
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

struct SelectorPatterns {
    include: Vec<GlobPattern>,
    exclude: Vec<GlobPattern>,
}

impl SelectorPatterns {
    fn new(stream: &StreamSpec) -> Result<Self, EngineError> {
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

    fn matches(&self, path: &Path) -> bool {
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
    driver_checkpoint: Option<Vec<u8>>,
    driver_checkpoint_version: Option<u32>,
    decoder_state: Option<Vec<u8>>,
    decoder_state_version: Option<u32>,
    decoder_contract_version: u32,
    state: String,
}

impl DurableObject {
    fn from_catalog(source_instance_id: u64, object: &SourceCatalogObject) -> Self {
        Self {
            source_instance_id,
            source_stream_id: object.source_stream_id,
            source_object_id: object.source_object_id,
            generation: object.generation,
            committed_cursor: object.committed_cursor.clone(),
            driver_checkpoint: object.driver_checkpoint.clone(),
            driver_checkpoint_version: object.driver_checkpoint_version,
            decoder_state: object.decoder_state.clone(),
            decoder_state_version: object.decoder_state_version,
            decoder_contract_version: object.decoder_contract_version,
            state: object.state.clone(),
        }
    }

    fn expected(&self) -> ExpectedSourceCursor {
        ExpectedSourceCursor::At {
            generation: self.generation,
            committed_cursor: self.committed_cursor.clone(),
        }
    }

    fn advance(&mut self, checkpoint: &AppendCheckpoint, encoded: Vec<u8>) {
        self.generation = checkpoint.generation;
        self.committed_cursor = checkpoint.cursor().into_bytes();
        self.driver_checkpoint = Some(encoded);
    }

    fn decoder_contract_changed<A: AgentAdapter + ?Sized>(&self, adapter: &A) -> bool {
        self.decoder_contract_version != adapter.manifest().contract_version
    }

    fn validate_driver_checkpoint(&self, stream: &StreamSpec) -> Result<(), EngineError> {
        let Some(version) = self.driver_checkpoint_version else {
            return Ok(());
        };
        let supported = match stream.driver {
            DriverSpec::AppendDelimited(_) | DriverSpec::Presence(_) => version == 1,
            DriverSpec::ReplaceDocument(_) => matches!(version, 1 | 2),
            DriverSpec::DirectorySnapshot(_) => false,
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

fn decode_record<A: AgentAdapter + ?Sized>(
    adapter: &A,
    stream: &StreamSpec,
    object_context: &AdapterObjectContext,
    record: &SourceRecord,
    decoder_state: Option<&[u8]>,
) -> Result<DecodedRecord, EngineError> {
    let mut batch = FactBatch::new(FACT_BATCH_LIMIT, DIAGNOSTIC_LIMIT)
        .map_err(|error| adapter_error("create fact batch", error))?;
    let disposition = adapter
        .decode(
            DecodeContext {
                decoder: &stream.decoder,
                object_context,
                decoder_state,
            },
            record,
            &mut batch,
        )
        .map_err(|error| adapter_error("decode source record", error))?;
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
        DecodeDisposition::IgnoredKnown | DecodeDisposition::RetryTransient if fact_count != 0 => {
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
    if stream.retention != RawRetentionPolicy::Full {
        batch.redact_unknown_record_payloads();
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
            raw_payload: (stream.retention == RawRetentionPolicy::Full)
                .then(|| record.payload.clone()),
            error_class: adapter_error_class(diagnostic.class).to_string(),
            error_message: format!("{}: {}", diagnostic.code, diagnostic.message),
            adapter_version: adapter.manifest().adapter_version.clone(),
            contract_version: adapter.manifest().contract_version,
            last_retry_at: None,
        })
        .collect();
    let next_decoder_state = batch.next_decoder_state().map(ToOwned::to_owned);
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
    let fallback_metadata = object
        .metadata
        .is_none()
        .then(|| std::fs::metadata(&object.path).ok())
        .flatten();
    let metadata = object.metadata.as_ref().or(fallback_metadata.as_ref());
    Ok(ObservationCommit {
        source: SourceInstanceSpec {
            adapter_id: adapter.manifest().id.as_str().to_string(),
            stable_key: instance.spec.stable_key.as_bytes().to_vec(),
            display_name: instance.spec.display_name.clone(),
            adapter_contract_version: adapter.manifest().contract_version,
            discovered_at: started_at,
            last_seen_at: committed_at,
        },
        stream: SourceStreamSpec {
            stream_key: stream.id.as_str().to_string(),
            driver_kind: stream.driver.kind().to_string(),
            decoder_key: stream.decoder.as_str().to_string(),
            stream_state: "available".to_string(),
            last_reconciled_at: Some(committed_at),
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

fn initial_cursor_bytes(stream: &StreamSpec) -> Vec<u8> {
    match stream.driver {
        DriverSpec::AppendDelimited(_) => SourceCursor::append_offset(0).into_bytes(),
        DriverSpec::ReplaceDocument(_) => SourceCursor::snapshot(Revision::ZERO).into_bytes(),
        DriverSpec::Presence(_) => SourceCursor::presence(Revision::ZERO).into_bytes(),
        DriverSpec::DirectorySnapshot(_) => unreachable!("not an adapter record stream"),
    }
}

fn driver_checkpoint_version(driver: &DriverSpec) -> u32 {
    match driver {
        DriverSpec::AppendDelimited(_) | DriverSpec::Presence(_) => 1,
        DriverSpec::ReplaceDocument(_) => 2,
        DriverSpec::DirectorySnapshot(_) => unreachable!("not an adapter record stream"),
    }
}

fn media_type(path: &Path) -> Result<SourceMediaType, EngineError> {
    let value = match path.extension().and_then(OsStr::to_str) {
        Some("jsonl") => "application/x-ndjson",
        Some("json") => "application/json",
        Some("md") => "text/markdown",
        Some("txt") => "text/plain",
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

    use rusqlite::{Connection, OpenFlags};
    use tempfile::TempDir;

    use crate::claude::ClaudeCodeAdapter;
    use crate::engine::EngineOptions;

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
    fn claude_reconcile_resumes_append_checkpoints_across_engine_restart() {
        let fixture = ClaudeFixture::new();
        let transcript = fixture.transcript_path();
        std::fs::write(&transcript, transcript_line("m1", "first")).unwrap();

        let first = fixture.open_engine();
        let initial = fixture.reconcile(&first);
        assert_eq!(initial.objects_registered, 1);
        assert_eq!(initial.records_decoded, 1);
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
        let degraded = engine.status().observation;
        assert_eq!(degraded.state, "degraded");
        assert!(degraded.full_reconcile_required);
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
            "canonical_interpretation_settings_documents" => {
                "SELECT COUNT(*) FROM canonical_interpretation_settings_documents"
            }
            _ => panic!("unsupported coordinator test table"),
        };
        connection.query_row(sql, [], |row| row.get(0)).unwrap()
    }

    fn catalog_object(
        engine: &SpaghettiEngineCore,
        root: &Path,
        stream: &str,
    ) -> SourceCatalogObject {
        let adapter = ClaudeCodeAdapter::new();
        engine
            .source_catalog(adapter.manifest().id.as_str(), &canonical_root_key(root))
            .unwrap()
            .objects
            .into_iter()
            .find(|object| object.stream_key == stream)
            .unwrap()
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

    fn first_blob(database: &Path, query: &str) -> Vec<u8> {
        let connection =
            Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        connection.query_row(query, [], |row| row.get(0)).unwrap()
    }

    fn canonical_root_key(root: &Path) -> Vec<u8> {
        crate::source::platform_path_key(&std::fs::canonicalize(root).unwrap())
    }
}
