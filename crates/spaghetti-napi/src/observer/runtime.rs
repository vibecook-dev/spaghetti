//! The source owner: watch first, then scan, then keep reconciling.
//!
//! One thread owns every source read, decode, and reduction for one scope.
//! Filesystem notifications are hints only — loss and coalescing are normal —
//! so a bounded poll interval backs every watcher and reconciliation is the
//! authority.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};

use crate::adapter::{
    AgentAdapter, CanonicalEntityKey, DiscoveryContext, Fact, FactSemanticRevision, SourceInstance,
};
use crate::claude::ClaudeCodeAdapter;

use super::event::{
    ClosedEvent, ObjectCoverage, ObserverBarrier, ObserverErrorEvent, ObserverEvent,
    ObserverFamily, ObserverPhase, OverflowEvent, OverflowReason, ResetEvent, SemanticEvent,
    SemanticOperation, SourceErrorEvent, SourcePosition,
};
use super::identity::{ActorAttribution, ActorRef, EventIdBuilder, ScopeIdentity};
use super::object::{DecodedFact, ObjectReset, ObservedObject};
use super::queue::{Admission, Delivery};
use super::request::ResolvedRequest;
use super::scope::{
    resolve_members, watch_anchors, JoinedLocators, ScopeMemberKey, StreamCatalog,
    ROOT_TRANSCRIPT_STREAM,
};
use super::state::{ScopeState, StateChange};
use super::ObserverError;

/// Reconciliation passes are bounded so one very large append cannot hold the
/// owner thread away from close or control delivery indefinitely.
const MAX_PASSES_PER_WAKE: usize = 64;

/// A wake reason for the owner thread.
pub(crate) enum Wake {
    /// A watcher fired, or a consumer asked for a low-latency pass.
    Hint,
    Close,
}

/// Everything the owner thread needs. Built on the calling thread so that
/// `open()` fails fast on a bad request rather than in the background.
pub(crate) struct ObserverRuntime {
    request: ResolvedRequest,
    adapter: ClaudeCodeAdapter,
    instance: SourceInstance,
    catalog: StreamCatalog,
    scope: ScopeIdentity,
    delivery: Arc<Delivery>,
    objects: BTreeMap<ScopeMemberKey, ObservedObject>,
    known_members: BTreeSet<ScopeMemberKey>,
    object_errors: BTreeMap<ScopeMemberKey, String>,
    joined: JoinedLocators,
    state: ScopeState,
    next_local_object_id: u64,
    root_key: ScopeMemberKey,
    phase: ObserverPhase,
    /// Set when this epoch lost continuity. Ordinary delivery stops, but
    /// reading and reduction continue so the replacement snapshot is built at
    /// the drained watermark rather than at the moment the queue filled.
    replacement_pending: bool,
}

impl ObserverRuntime {
    pub(crate) fn attach(
        request: ResolvedRequest,
        delivery: Arc<Delivery>,
    ) -> Result<Self, ObserverError> {
        let adapter = ClaudeCodeAdapter::new();

        // Evaluate a declared scope program, not an ad-hoc path rule. The
        // adapter's compiled-in manifest is the authority for what a session
        // scope may reach.
        let manifest = adapter.manifest();
        let programs = manifest.scope_programs.as_ref().ok_or_else(|| {
            ObserverError::Unsupported("adapter declares no scope program".to_string())
        })?;
        if !programs
            .programs
            .iter()
            .any(|program| program.root_entity_kind == "session")
        {
            return Err(ObserverError::Unsupported(
                "adapter declares no session-rooted scope program".to_string(),
            ));
        }

        let specs = adapter
            .discover(&DiscoveryContext {
                configured_roots: vec![request.agent_root.clone()],
                observed_at: now_ms(),
            })
            .map_err(|error| ObserverError::Adapter(error.to_string()))?;
        let spec = specs.into_iter().next().ok_or_else(|| {
            ObserverError::InvalidRequest("agent_root is not a usable agent instance".to_string())
        })?;
        let instance = SourceInstance { id: 1, spec };

        // Root identity is final here — before any watch is installed and
        // before any object is opened.
        let scope = ScopeIdentity::derive(
            &manifest.id,
            instance.spec.identity_contract_version,
            instance.spec.stable_key.as_bytes(),
            &request.native_session_id,
        )
        .map_err(|error| ObserverError::Adapter(error.to_string()))?;

        let catalog = StreamCatalog::new(
            adapter
                .streams(&instance)
                .map_err(|error| ObserverError::Adapter(error.to_string()))?,
        );
        let root_key = ScopeMemberKey {
            stream_id: ROOT_TRANSCRIPT_STREAM.to_string(),
            root_name: "projects".to_string(),
            relative_path: request.root_transcript_relative(),
        };

        Ok(Self {
            request,
            adapter,
            instance,
            catalog,
            scope,
            delivery,
            objects: BTreeMap::new(),
            known_members: BTreeSet::new(),
            object_errors: BTreeMap::new(),
            joined: JoinedLocators::default(),
            state: ScopeState::default(),
            next_local_object_id: 1,
            root_key,
            phase: ObserverPhase::Bootstrap,
            replacement_pending: false,
        })
    }

    /// Install watches, then bootstrap, then serve. Runs on the owner thread.
    pub(crate) fn run(mut self, wake: Receiver<Wake>, hint: Sender<Wake>) {
        // Watch before scan. A watcher installed after the scan would miss
        // every append that landed during it.
        let _watcher = install_watcher(&self.request, hint);

        self.phase = ObserverPhase::Bootstrap;
        self.reconcile_until_drained();
        self.emit_barrier(true);
        self.phase = ObserverPhase::Live;

        loop {
            if self.delivery.is_closing() {
                break;
            }
            match wake.recv_timeout(self.request.poll_interval) {
                Ok(Wake::Close) => break,
                Ok(Wake::Hint) | Err(RecvTimeoutError::Timeout) => {
                    self.reconcile_until_drained();
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }
            if let Some(message) = self.delivery.take_terminal_error() {
                self.emit_error(message);
                break;
            }
        }

        let discarded = self.delivery.mark_closed();
        let mut builder = EventIdBuilder::new("observer.closed");
        builder.scope(&self.scope).u64(self.delivery.epoch());
        self.delivery
            .admit_control(ObserverEvent::Closed(ClosedEvent {
                event_id: builder.finish(),
                sequence: 0,
                scope_epoch: self.delivery.epoch(),
                discarded_events: discarded,
                observed_at: now_ms(),
            }));
    }

    /// Reconcile until no object reports more bytes ready, then publish a
    /// replacement epoch if this one lost continuity along the way.
    fn reconcile_until_drained(&mut self) {
        for _ in 0..MAX_PASSES_PER_WAKE {
            if self.delivery.is_closing() {
                break;
            }
            if !self.reconcile_once() {
                break;
            }
        }
        if self.replacement_pending && !self.delivery.is_closing() {
            self.publish_replacement();
        }
    }

    /// One pass over the current member set. Returns true when another pass is
    /// needed because a driver had more bytes ready or the scope grew.
    fn reconcile_once(&mut self) -> bool {
        let members = resolve_members(&self.request, &self.catalog, &self.joined);
        let current: BTreeSet<ScopeMemberKey> =
            members.iter().map(|member| member.key.clone()).collect();

        // Objects that left the declared scope retract the facts they owned.
        let departed: Vec<_> = self.known_members.difference(&current).cloned().collect();
        for key in departed {
            self.objects.remove(&key);
            self.object_errors.remove(&key);
            self.known_members.remove(&key);
            self.retract_owned(&key, None);
        }

        for member in members {
            let key = member.key.clone();
            if !self.objects.contains_key(&key) {
                let local_object_id = self.next_local_object_id;
                self.next_local_object_id += 1;
                match ObservedObject::bind(&self.adapter, &self.instance, member, local_object_id) {
                    Ok(Some(object)) => {
                        self.objects.insert(key.clone(), object);
                    }
                    // Declared relations whose driver is out of the v1 scoped
                    // surface stay unopened; that is not an error.
                    Ok(None) => continue,
                    Err(error) => {
                        self.record_object_error(&key, error.to_string(), true);
                        continue;
                    }
                }
            }
            self.known_members.insert(key);
        }

        let keys: Vec<_> = self.objects.keys().cloned().collect();
        let mut more = false;
        let observed_at = now_ms();
        for key in keys {
            if self.delivery.is_closing() {
                return false;
            }
            let Some(object) = self.objects.get_mut(&key) else {
                continue;
            };
            let pass = object.reconcile(&self.adapter, observed_at);
            let generation = object.generation();
            more |= pass.more_available;

            if let Some(reset) = pass.reset {
                self.emit_reset(&key, reset, generation);
                // Reset before replay: retract what the superseded generation
                // owned before any corrected record is delivered.
                self.retract_owned(&key, Some(reset.new_generation));
            }
            if pass.removed {
                self.retract_owned(&key, None);
            }
            if self.joined.apply(&pass.joins) {
                more = true;
            }
            for fact in &pass.facts {
                // Reduction continues even after continuity is lost: the
                // checkpoint has already advanced past these records, so
                // dropping them here would leave the replacement snapshot
                // short of what the source actually holds.
                self.emit_fact(&key, fact, generation);
            }
            match pass.error {
                Some(message) => self.record_object_error(&key, message, false),
                None => {
                    self.object_errors.remove(&key);
                }
            }
        }
        more
    }

    /// Reduce one decoded fact and deliver whatever the reducer decided.
    fn emit_fact(&mut self, owner: &ScopeMemberKey, fact: &DecodedFact, generation: u64) {
        let Some((family, change)) = self.state.apply(fact, owner) else {
            return;
        };
        let source = self.source_position(owner, generation, fact);
        match change {
            StateChange::Unchanged => {}
            StateChange::Upsert { semantic, value } => {
                let actor = self.actor_for(&value);
                let event = self.build_semantic(
                    family,
                    &semantic,
                    SemanticOperation::Upsert,
                    actor,
                    source,
                    Some(&value),
                    fact.native_time,
                    fact.observed_at,
                );
                self.admit(family, event);
            }
            StateChange::Retract { semantic } => {
                let actor = ActorRef::root(&self.scope, ActorAttribution::ScopeFallback);
                let event = self.build_semantic(
                    family,
                    &semantic,
                    SemanticOperation::Retract,
                    actor,
                    source,
                    None,
                    fact.native_time,
                    fact.observed_at,
                );
                self.admit(family, event);
            }
        }
    }

    /// Deliver retractions for everything one object (or one superseded
    /// generation of it) owned.
    fn retract_owned(&mut self, owner: &ScopeMemberKey, before_generation: Option<u64>) {
        let retracted = self.state.retract_owned(owner, before_generation);
        let observed_at = now_ms();
        for (family, semantic) in retracted {
            let source = SourcePosition {
                stream_id: owner.stream_id.clone(),
                object_path: owner.relative_path.to_string_lossy().into_owned(),
                root_name: owner.root_name.clone(),
                generation: before_generation.unwrap_or_default(),
                byte_start: None,
                byte_end: None,
                record_digest: String::new(),
            };
            let actor = ActorRef::root(&self.scope, ActorAttribution::ScopeFallback);
            let event = self.build_semantic(
                family,
                &semantic,
                SemanticOperation::Retract,
                actor,
                source,
                None,
                None,
                observed_at,
            );
            self.admit(family, event);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn build_semantic(
        &self,
        family: ObserverFamily,
        semantic: &FactSemanticRevision,
        operation: SemanticOperation,
        actor: ActorRef,
        source: SourcePosition,
        value: Option<&Fact>,
        native_time: Option<i64>,
        observed_at: i64,
    ) -> SemanticEvent {
        let mut builder = EventIdBuilder::new(family.as_str());
        builder
            .component(match operation {
                SemanticOperation::Upsert => b"upsert",
                SemanticOperation::Retract => b"retract",
            })
            .semantic(semantic);
        SemanticEvent {
            event_id: builder.finish(),
            sequence: 0,
            scope_epoch: self.delivery.epoch(),
            family,
            phase: self.phase,
            operation,
            semantic_revision_ref: semantic.semantic_revision_ref,
            fact_id: semantic.fact_id,
            actor,
            source,
            native_time,
            observed_at,
            value: value.and_then(|value| serde_json::to_value(value).ok()),
        }
    }

    /// Offer one semantic event under the current phase's backpressure rule.
    fn admit(&mut self, family: ObserverFamily, event: SemanticEvent) {
        if self.replacement_pending {
            // Ordinary delivery is suspended until the replacement epoch opens.
            return;
        }
        let event = ObserverEvent::semantic(family, event);
        match self.phase {
            // Bootstrap and correction may pause the producer. Queue fullness
            // alone must never manufacture continuity loss.
            ObserverPhase::Bootstrap | ObserverPhase::Correction => {
                self.delivery.admit_semantic_blocking(event);
            }
            ObserverPhase::Live => {
                if self.delivery.admit_semantic(event) == Admission::Full {
                    self.begin_overflow(OverflowReason::QueueFull);
                }
            }
        }
    }

    /// Continuity is lost. Mark the epoch invalid and say so immediately; the
    /// replacement snapshot follows once the current pass set is drained.
    fn begin_overflow(&mut self, reason: OverflowReason) {
        if self.replacement_pending {
            return;
        }
        self.replacement_pending = true;
        let (invalid_epoch, last_contiguous, _discarded) = self.delivery.invalidate_epoch();
        let mut builder = EventIdBuilder::new("observer.overflow");
        builder
            .scope(&self.scope)
            .u64(invalid_epoch)
            .component(match reason {
                OverflowReason::QueueFull => b"queue_full",
                OverflowReason::ConsumerRequested => b"consumer_requested",
            });
        self.delivery
            .admit_control(ObserverEvent::Overflow(OverflowEvent {
                event_id: builder.finish(),
                sequence: 0,
                scope_epoch: invalid_epoch,
                reason,
                last_contiguous_sequence: last_contiguous,
                observed_at: now_ms(),
            }));
    }

    /// Open the next epoch and publish a complete replacement snapshot built
    /// from reducer state at the drained watermark. A clean bootstrap at the
    /// same coverage produces identical family digests.
    fn publish_replacement(&mut self) {
        self.replacement_pending = false;
        self.delivery.begin_replacement_epoch();
        let previous_phase = self.phase;
        self.phase = ObserverPhase::Correction;

        // Replay every current reduced entity. Cloning first keeps the state
        // borrow off the emit path; a session tree's reduced state is bounded.
        let snapshot: Vec<_> = self
            .state
            .replacement_snapshot()
            .into_iter()
            .map(|(family, semantic, value)| (family, *semantic, value.clone()))
            .collect();
        let observed_at = now_ms();
        for (family, semantic, value) in snapshot {
            let owner = self
                .state
                .owner_of(family, semantic.fact_id)
                .map(|(key, generation)| (key.clone(), generation));
            let source = owner.map_or_else(
                || SourcePosition {
                    stream_id: self.root_key.stream_id.clone(),
                    object_path: self.root_key.relative_path.to_string_lossy().into_owned(),
                    root_name: self.root_key.root_name.clone(),
                    generation: 0,
                    byte_start: None,
                    byte_end: None,
                    record_digest: String::new(),
                },
                |(key, generation)| SourcePosition {
                    stream_id: key.stream_id.clone(),
                    object_path: key.relative_path.to_string_lossy().into_owned(),
                    root_name: key.root_name.clone(),
                    generation,
                    byte_start: None,
                    byte_end: None,
                    record_digest: String::new(),
                },
            );
            let actor = self.actor_for(&value);
            let event = self.build_semantic(
                family,
                &semantic,
                SemanticOperation::Upsert,
                actor,
                source,
                Some(&value),
                None,
                observed_at,
            );
            self.admit(family, event);
        }
        self.emit_barrier(false);
        self.phase = previous_phase;
    }

    fn emit_barrier(&mut self, bootstrap: bool) {
        let epoch = self.delivery.epoch();
        let manifest = self.state.family_manifest();
        let coverage = self.coverage();
        let root_present = self
            .objects
            .get(&self.root_key)
            .is_some_and(ObservedObject::present);
        let kind = if bootstrap {
            "observer.bootstrap_complete"
        } else {
            "observer.resync_complete"
        };
        let mut builder = EventIdBuilder::new(kind);
        builder.scope(&self.scope).u64(epoch);
        for entry in &manifest {
            builder.component(entry.digest.as_bytes());
        }
        let barrier = ObserverBarrier {
            event_id: builder.finish(),
            sequence: 0,
            scope_epoch: epoch,
            root_present,
            family_manifest: manifest,
            coverage,
            observed_at: now_ms(),
        };
        self.delivery.admit_control(if bootstrap {
            ObserverEvent::BootstrapComplete(barrier)
        } else {
            ObserverEvent::ResyncComplete(barrier)
        });
    }

    fn coverage(&self) -> Vec<ObjectCoverage> {
        self.objects
            .iter()
            .map(|(key, object)| ObjectCoverage {
                relation_id: object.relation_id().to_string(),
                stream_id: key.stream_id.clone(),
                object_path: key.relative_path.to_string_lossy().into_owned(),
                root_name: key.root_name.clone(),
                generation: object.generation(),
                present: object.present(),
                error: self.object_errors.get(key).cloned(),
            })
            .collect()
    }

    fn emit_reset(&mut self, owner: &ScopeMemberKey, reset: ObjectReset, generation: u64) {
        let mut builder = EventIdBuilder::new("source.reset");
        builder
            .scope(&self.scope)
            .component(owner.stream_id.as_bytes())
            .component(owner.relative_path.to_string_lossy().as_bytes())
            .u64(reset.old_generation)
            .u64(reset.new_generation)
            .component(reset.reason.as_bytes());
        self.delivery
            .admit_control(ObserverEvent::Reset(ResetEvent {
                event_id: builder.finish(),
                sequence: 0,
                scope_epoch: self.delivery.epoch(),
                source: SourcePosition {
                    stream_id: owner.stream_id.clone(),
                    object_path: owner.relative_path.to_string_lossy().into_owned(),
                    root_name: owner.root_name.clone(),
                    generation,
                    byte_start: None,
                    byte_end: None,
                    record_digest: String::new(),
                },
                old_generation: reset.old_generation,
                new_generation: reset.new_generation,
                reason: reset.reason.to_string(),
                actor: ActorRef::root(&self.scope, ActorAttribution::ScopeFallback),
                observed_at: now_ms(),
            }));
    }

    /// One object's failure is explicit and never kills its siblings.
    fn record_object_error(&mut self, owner: &ScopeMemberKey, message: String, terminal: bool) {
        if self.object_errors.get(owner) == Some(&message) {
            return;
        }
        self.object_errors.insert(owner.clone(), message.clone());
        let mut builder = EventIdBuilder::new("source.error");
        builder
            .scope(&self.scope)
            .component(owner.stream_id.as_bytes())
            .component(owner.relative_path.to_string_lossy().as_bytes())
            .component(message.as_bytes());
        self.delivery
            .admit_control(ObserverEvent::SourceError(SourceErrorEvent {
                event_id: builder.finish(),
                sequence: 0,
                scope_epoch: self.delivery.epoch(),
                source: SourcePosition {
                    stream_id: owner.stream_id.clone(),
                    object_path: owner.relative_path.to_string_lossy().into_owned(),
                    root_name: owner.root_name.clone(),
                    generation: 0,
                    byte_start: None,
                    byte_end: None,
                    record_digest: String::new(),
                },
                message,
                terminal,
                observed_at: now_ms(),
            }));
    }

    fn emit_error(&mut self, message: String) {
        let mut builder = EventIdBuilder::new("observer.error");
        builder
            .scope(&self.scope)
            .u64(self.delivery.epoch())
            .component(message.as_bytes());
        self.delivery
            .admit_control(ObserverEvent::Error(ObserverErrorEvent {
                event_id: builder.finish(),
                sequence: 0,
                scope_epoch: self.delivery.epoch(),
                message,
                observed_at: now_ms(),
            }));
    }

    fn source_position(
        &self,
        owner: &ScopeMemberKey,
        generation: u64,
        fact: &DecodedFact,
    ) -> SourcePosition {
        SourcePosition {
            stream_id: owner.stream_id.clone(),
            object_path: owner.relative_path.to_string_lossy().into_owned(),
            root_name: owner.root_name.clone(),
            generation,
            byte_start: fact.byte_start,
            byte_end: fact.byte_end,
            record_digest: fact
                .record_digest
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        }
    }

    /// Every RFC 012C runtime family names the actor run it belongs to. A fact
    /// that does not is routed to the root as scope fallback, which is not
    /// semantic attribution.
    fn actor_for(&self, value: &Fact) -> ActorRef {
        match actor_run_of(value) {
            Some(actor_run) => ActorRef::actor(&self.scope, actor_run),
            None => ActorRef::root(&self.scope, ActorAttribution::ScopeFallback),
        }
    }
}

fn actor_run_of(value: &Fact) -> Option<CanonicalEntityKey> {
    Some(match value {
        Fact::ActorRunRevision(revision) => revision.actor_run,
        Fact::ActorAffiliationRevision(revision) => revision.actor_run,
        Fact::UsageRevisionV2(revision) => revision.actor_run,
        Fact::UserInputRequestRevision(revision) => revision.actor_run,
        Fact::MessageRevision(revision) => revision.actor_run,
        Fact::ContentBlockRevision(revision) => revision.actor_run,
        Fact::NativeRuntimeMarkerRevision(revision) => revision.actor_run,
        Fact::TaskRevision(revision) => revision.actor_run,
        Fact::PlanRevision(revision) => revision.actor_run,
        Fact::ToolRevision(revision) => revision.actor_run,
        Fact::EffectiveStateRevision(revision) => revision.actor_run,
        _ => return None,
    })
}

/// Install one recursive watcher per declared anchor, falling back to the
/// nearest existing ancestor so that attaching before the session directory
/// exists still observes its creation.
fn install_watcher(request: &ResolvedRequest, hint: Sender<Wake>) -> Option<RecommendedWatcher> {
    let mut watcher = notify::recommended_watcher(move |_event| {
        // Notifications are hints; a full channel already means a pass is
        // pending, so dropping the extra wake is correct.
        let _unused = hint.try_send(Wake::Hint);
    })
    .ok()?;
    let mut watched: BTreeSet<PathBuf> = BTreeSet::new();
    for anchor in watch_anchors(request) {
        let Some(existing) = nearest_existing_ancestor(&anchor, &request.agent_root) else {
            continue;
        };
        if !watched.insert(existing.clone()) {
            continue;
        }
        let _unused = watcher.watch(&existing, RecursiveMode::Recursive);
    }
    Some(watcher)
}

fn nearest_existing_ancestor(path: &Path, floor: &Path) -> Option<PathBuf> {
    let mut candidate = path.to_path_buf();
    loop {
        if candidate.is_dir() {
            return Some(candidate);
        }
        if !candidate.starts_with(floor) || candidate == floor {
            return floor.is_dir().then(|| floor.to_path_buf());
        }
        candidate = candidate.parent()?.to_path_buf();
    }
}

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}

/// Bounded wake channel between the watcher, the consumer's poll hint, and the
/// owner thread.
pub(crate) fn wake_channel() -> (Sender<Wake>, Receiver<Wake>) {
    crossbeam_channel::bounded(64)
}

/// Ask the owner thread to run a pass now. Used by `poll()` as the
/// low-latency hint RFC 012D describes.
pub(crate) fn nudge(sender: &Sender<Wake>) {
    let _unused = sender.try_send(Wake::Hint);
}

pub(crate) fn signal_close(sender: &Sender<Wake>) {
    let _unused = sender.send_timeout(Wake::Close, Duration::from_millis(100));
}
