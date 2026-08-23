//! Reduced RFC 012C state for one scope.
//!
//! The observer keeps exactly one current revision per canonical fact, decided
//! by the shared reducer in `runtime_semantic_reducer`. Nothing here re-decides
//! a family law: durable ingestion and the observer must reach the same reduced
//! state, and the per-family replacement digest is how that gets tested.

use std::collections::BTreeMap;

use crate::adapter::{CanonicalFactId, Fact, FactSemanticRevision};
use crate::runtime_semantic_reducer::{
    reduce_runtime_fact_revision, runtime_replacement_state_digest, validate_runtime_fact_revision,
    RuntimeFactReduction, RuntimeReplacementDigestEntity, RuntimeSemanticFamily,
};

use super::event::{FamilyManifestEntry, ObserverFamily};
use super::object::DecodedFact;
use super::scope::ScopeMemberKey;

/// What the reducer decided for one incoming revision.
pub(crate) enum StateChange {
    /// The revision is an exact repeat of the current one. RFC 012C suppresses
    /// it before any event is built: a repeated usage row adds nothing.
    Unchanged,
    Upsert {
        semantic: FactSemanticRevision,
        value: Box<Fact>,
    },
    Retract {
        semantic: FactSemanticRevision,
    },
}

struct Entry {
    semantic: FactSemanticRevision,
    value: Fact,
    owner: ScopeMemberKey,
    generation: u64,
}

/// The observer's whole in-memory model. Bounded by the number of distinct
/// canonical facts in one session tree.
#[derive(Default)]
pub(crate) struct ScopeState {
    entries: BTreeMap<(RuntimeSemanticFamily, CanonicalFactId), Entry>,
}

impl ScopeState {
    /// Run one decoded fact through the shared family law.
    ///
    /// Returns `None` for facts that are not RFC 012C runtime revisions (the
    /// RFC 011 durable families), which the observer does not deliver.
    pub(crate) fn apply(
        &mut self,
        fact: &DecodedFact,
        owner: &ScopeMemberKey,
    ) -> Option<(ObserverFamily, StateChange)> {
        let family = validate_runtime_fact_revision(&fact.semantic, &fact.value).ok()?;
        let key = (family, fact.semantic.fact_id);
        let current = self
            .entries
            .get(&key)
            .map(|entry| (&entry.semantic, &entry.value));
        let reduction =
            reduce_runtime_fact_revision(current, (&fact.semantic, &fact.value)).ok()?;
        let change = match reduction {
            RuntimeFactReduction::Unchanged => StateChange::Unchanged,
            RuntimeFactReduction::Upsert { semantic, revision } => {
                self.entries.insert(
                    key,
                    Entry {
                        semantic,
                        value: (*revision).clone(),
                        owner: owner.clone(),
                        generation: fact.generation,
                    },
                );
                StateChange::Upsert {
                    semantic,
                    value: revision,
                }
            }
            RuntimeFactReduction::Retract => {
                let removed = self.entries.remove(&key);
                StateChange::Retract {
                    semantic: removed.map_or(fact.semantic, |entry| entry.semantic),
                }
            }
        };
        Some((family.into(), change))
    }

    /// Drop every fact owned by one object, or by one superseded generation of
    /// it. RFC 012D section 11: deletion and reset retract old object-owned
    /// state through the common reducers rather than leaving it stale.
    pub(crate) fn retract_owned(
        &mut self,
        owner: &ScopeMemberKey,
        before_generation: Option<u64>,
    ) -> Vec<(ObserverFamily, FactSemanticRevision)> {
        let doomed: Vec<_> = self
            .entries
            .iter()
            .filter(|(_, entry)| {
                &entry.owner == owner
                    && before_generation.is_none_or(|generation| entry.generation < generation)
            })
            .map(|((family, fact_id), _)| (*family, *fact_id))
            .collect();
        doomed
            .into_iter()
            .filter_map(|key| {
                self.entries
                    .remove(&key)
                    .map(|entry| (key.0.into(), entry.semantic))
            })
            .collect()
    }

    /// Every current revision, in a deterministic order, for snapshot replay.
    pub(crate) fn replacement_snapshot(
        &self,
    ) -> Vec<(ObserverFamily, &FactSemanticRevision, &Fact)> {
        self.entries
            .iter()
            .map(|((family, _), entry)| ((*family).into(), &entry.semantic, &entry.value))
            .collect()
    }

    /// The object each current revision came from, for source coordinates on
    /// replayed snapshot events.
    pub(crate) fn owner_of(
        &self,
        family: ObserverFamily,
        fact_id: CanonicalFactId,
    ) -> Option<(&ScopeMemberKey, u64)> {
        self.entries
            .get(&(family.as_runtime(), fact_id))
            .map(|entry| (&entry.owner, entry.generation))
    }

    /// Per-family manifest with the topology-neutral replacement digest.
    ///
    /// A clean bootstrap and a completed resync at the same coverage produce
    /// identical digests; that equality is the observer's proof that a
    /// replacement epoch is a faithful rebuild and not a partial merge.
    pub(crate) fn family_manifest(&self) -> Vec<FamilyManifestEntry> {
        ObserverFamily::ALL
            .iter()
            .map(|family| {
                let runtime = family.as_runtime();
                let entities: Vec<_> = self
                    .entries
                    .iter()
                    .filter(|((entry_family, _), _)| *entry_family == runtime)
                    .map(|(_, entry)| RuntimeReplacementDigestEntity {
                        semantic: &entry.semantic,
                        revision: &entry.value,
                    })
                    .collect();
                let entity_count = entities.len() as u32;
                let digest = runtime_replacement_state_digest(runtime, entities)
                    .map(|digest| {
                        digest
                            .iter()
                            .map(|byte| format!("{byte:02x}"))
                            .collect::<String>()
                    })
                    .unwrap_or_default();
                FamilyManifestEntry {
                    family: *family,
                    contract_version: RuntimeSemanticFamily::VERSION,
                    entity_count,
                    digest,
                }
            })
            .collect()
    }
}
