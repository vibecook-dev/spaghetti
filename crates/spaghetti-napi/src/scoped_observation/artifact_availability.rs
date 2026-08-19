//! Attachment-owned, path-free artifact availability state.
//!
//! A completed confined read may update this reducer only after it has passed
//! the strict artifact-result contract. The reducer retains no native locator,
//! file identity, content, or content hash. Changed observations are prepared
//! without mutation, offered through the attachment's ordered lane, and
//! committed only after that offer succeeds. Its snapshot filters observations
//! through the exact current metadata-evidence selection, so a correction or
//! retraction makes an old native observation unobservable rather than
//! relabeling it as missing.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use crate::adapter::CanonicalEntityKey;
use crate::source::{validate_relation_id, AccessObjectToken};

use super::artifact_evidence::{
    ScopedArtifactEvidenceSelection, MAX_SCOPED_ARTIFACT_EVIDENCE_ASSERTIONS,
};
use super::{ScopedObservationProjectionSink, ScopedProjectionError, ScopedSourceObjectIdentity};

pub(super) const SCOPED_ARTIFACT_AVAILABILITY_CONTRACT_VERSION: u32 = 1;
const JS_SAFE_INTEGER_MAX: u64 = 9_007_199_254_740_991;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ScopedArtifactAvailabilityState {
    Available {
        generation: u64,
        provenance_ref: [u8; 32],
        size_bytes: u64,
    },
    Missing {
        observed_generation: Option<u64>,
        provenance_ref: Option<[u8; 32]>,
    },
    OverLimit {
        generation: u64,
        provenance_ref: [u8; 32],
        observed_bytes: u64,
        request_max_bytes: u64,
    },
    Unstable,
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct ScopedArtifactAvailabilitySourceOccurrence {
    source_declaration_digest: [u8; 32],
    source: ScopedSourceObjectIdentity,
    generation: u64,
}

impl ScopedArtifactAvailabilitySourceOccurrence {
    pub(super) fn new(
        source_declaration_digest: [u8; 32],
        source: ScopedSourceObjectIdentity,
        generation: u64,
    ) -> Self {
        Self {
            source_declaration_digest,
            source,
            generation,
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct ScopedArtifactAvailabilityObservation {
    evidence: ScopedArtifactEvidenceSelection,
    artifact_kind: Arc<str>,
    relation_id: Arc<str>,
    object_token: AccessObjectToken,
    source: ScopedArtifactAvailabilitySourceOccurrence,
    state: ScopedArtifactAvailabilityState,
}

impl ScopedArtifactAvailabilityObservation {
    pub(super) fn new(
        evidence: ScopedArtifactEvidenceSelection,
        artifact_kind: Arc<str>,
        relation_id: Arc<str>,
        object_token: AccessObjectToken,
        source: ScopedArtifactAvailabilitySourceOccurrence,
        state: ScopedArtifactAvailabilityState,
    ) -> Self {
        Self {
            evidence,
            artifact_kind,
            relation_id,
            object_token,
            source,
            state,
        }
    }

    fn validate_for_root(&self, root_session: CanonicalEntityKey) -> bool {
        self.evidence.root_session() == root_session
            && root_session.as_bytes().iter().any(|byte| *byte != 0)
            && self
                .evidence
                .artifact_key()
                .as_bytes()
                .iter()
                .any(|byte| *byte != 0)
            && self.evidence.version() > 0
            && self.evidence.version() <= JS_SAFE_INTEGER_MAX
            && self
                .evidence
                .revision()
                .as_bytes()
                .iter()
                .any(|byte| *byte != 0)
            && validate_artifact_kind(&self.artifact_kind)
            && validate_relation_id(&self.relation_id).is_ok()
            && self.object_token.as_bytes().iter().any(|byte| *byte != 0)
            && self
                .source
                .source_declaration_digest
                .iter()
                .any(|byte| *byte != 0)
            && self
                .source
                .source
                .source_instance_key
                .as_bytes()
                .iter()
                .any(|byte| *byte != 0)
            && self
                .source
                .source
                .stream_key
                .as_bytes()
                .iter()
                .any(|byte| *byte != 0)
            && self
                .source
                .source
                .object_key
                .as_bytes()
                .iter()
                .any(|byte| *byte != 0)
            && self.source.generation > 0
            && self.source.generation <= JS_SAFE_INTEGER_MAX
            && validate_availability_state(self.state)
            && match self.state {
                ScopedArtifactAvailabilityState::Available { generation, .. }
                | ScopedArtifactAvailabilityState::OverLimit { generation, .. } => {
                    generation == self.source.generation
                }
                ScopedArtifactAvailabilityState::Missing {
                    observed_generation,
                    ..
                } => observed_generation.unwrap_or(1) == self.source.generation,
                ScopedArtifactAvailabilityState::Unstable => true,
            }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ScopedArtifactAvailabilityRevision([u8; 32]);

impl ScopedArtifactAvailabilityRevision {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    #[cfg(test)]
    pub(super) fn fixture(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl fmt::Debug for ScopedArtifactAvailabilityRevision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("ScopedArtifactAvailabilityRevision")
            .field(&"v1:<redacted>")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct ScopedArtifactAvailabilityEntry {
    artifact_key: CanonicalEntityKey,
    artifact_kind: Arc<str>,
    revision: ScopedArtifactAvailabilityRevision,
    state: ScopedArtifactAvailabilityState,
}

impl ScopedArtifactAvailabilityEntry {
    pub fn artifact_key(&self) -> CanonicalEntityKey {
        self.artifact_key
    }

    pub fn artifact_kind(&self) -> &str {
        &self.artifact_kind
    }

    pub fn revision(&self) -> ScopedArtifactAvailabilityRevision {
        self.revision
    }

    pub(super) fn state(&self) -> ScopedArtifactAvailabilityState {
        self.state
    }
}

impl fmt::Debug for ScopedArtifactAvailabilityEntry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = match self.state {
            ScopedArtifactAvailabilityState::Available { .. } => "available",
            ScopedArtifactAvailabilityState::Missing { .. } => "missing",
            ScopedArtifactAvailabilityState::OverLimit { .. } => "over_limit",
            ScopedArtifactAvailabilityState::Unstable => "unstable",
        };
        formatter
            .debug_struct("ScopedArtifactAvailabilityEntry")
            .field("artifact_key", &"<redacted>")
            .field("artifact_kind", &self.artifact_kind)
            .field("revision", &self.revision)
            .field("state", &state)
            .finish()
    }
}

/// One checked, path-free source occurrence for an availability change. The
/// exact source declaration digest and common source coordinate remain
/// private; they bind event identity without becoming locator disclosure.
#[derive(Clone, PartialEq, Eq)]
pub struct ScopedArtifactAvailabilityOccurrence {
    root_session: CanonicalEntityKey,
    observation: ScopedArtifactAvailabilityObservation,
    entry: ScopedArtifactAvailabilityEntry,
}

impl ScopedArtifactAvailabilityOccurrence {
    pub(super) fn root_session(&self) -> CanonicalEntityKey {
        self.root_session
    }

    pub(super) fn source(&self) -> &ScopedSourceObjectIdentity {
        &self.observation.source.source
    }

    pub(super) fn source_generation(&self) -> u64 {
        self.observation.source.generation
    }

    pub(super) fn source_declaration_digest(&self) -> &[u8; 32] {
        &self.observation.source.source_declaration_digest
    }

    pub(super) fn entry(&self) -> &ScopedArtifactAvailabilityEntry {
        &self.entry
    }

    pub(super) fn validate_for_root(&self, root_session: CanonicalEntityKey) -> bool {
        self.root_session == root_session
            && self.observation.validate_for_root(root_session)
            && self.entry == entry_for_observation(root_session, &self.observation)
    }
}

impl fmt::Debug for ScopedArtifactAvailabilityOccurrence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScopedArtifactAvailabilityOccurrence")
            .field("source_declaration_digest", &"sha256:<redacted>")
            .field("source_generation", &self.observation.source.generation)
            .field("source_coordinate", &"<redacted>")
            .field("entry", &self.entry)
            .finish()
    }
}

#[must_use = "a prepared availability change must be offered before it is committed"]
pub(super) struct ScopedArtifactAvailabilityPreparedObservation {
    key: (CanonicalEntityKey, Arc<str>),
    observation: ScopedArtifactAvailabilityObservation,
    occurrence: ScopedArtifactAvailabilityOccurrence,
}

impl ScopedArtifactAvailabilityPreparedObservation {
    pub(super) fn occurrence(&self) -> &ScopedArtifactAvailabilityOccurrence {
        &self.occurrence
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct ScopedArtifactAvailabilitySnapshot {
    contract_version: u32,
    root_session: CanonicalEntityKey,
    entry_count: u64,
    semantic_digest: ScopedArtifactAvailabilityRevision,
    entries: Vec<ScopedArtifactAvailabilityEntry>,
}

impl ScopedArtifactAvailabilitySnapshot {
    pub fn entry_count(&self) -> u64 {
        self.entry_count
    }

    pub fn semantic_digest(&self) -> ScopedArtifactAvailabilityRevision {
        self.semantic_digest
    }

    pub fn entries(&self) -> &[ScopedArtifactAvailabilityEntry] {
        &self.entries
    }

    pub(super) fn validate_for_root(&self, root_session: CanonicalEntityKey) -> bool {
        self.contract_version == SCOPED_ARTIFACT_AVAILABILITY_CONTRACT_VERSION
            && self.root_session == root_session
            && u64::try_from(self.entries.len()) == Ok(self.entry_count)
            && self.entries.windows(2).all(|window| {
                (&window[0].artifact_key, window[0].artifact_kind.as_ref())
                    < (&window[1].artifact_key, window[1].artifact_kind.as_ref())
            })
            && self.entries.iter().all(|entry| {
                entry.revision.0.iter().any(|byte| *byte != 0)
                    && validate_artifact_kind(&entry.artifact_kind)
                    && validate_availability_state(entry.state)
            })
            && self.semantic_digest
                == derive_snapshot_digest(root_session, self.entry_count, &self.entries)
            && self.semantic_digest.0.iter().any(|byte| *byte != 0)
    }

    #[cfg(test)]
    pub(super) fn empty_fixture(root_session: CanonicalEntityKey) -> Self {
        let entries = Vec::new();
        Self {
            contract_version: SCOPED_ARTIFACT_AVAILABILITY_CONTRACT_VERSION,
            root_session,
            entry_count: 0,
            semantic_digest: derive_snapshot_digest(root_session, 0, &entries),
            entries,
        }
    }

    #[cfg(test)]
    pub(super) fn fixture(
        root_session: CanonicalEntityKey,
        entries: Vec<(
            CanonicalEntityKey,
            impl Into<Arc<str>>,
            ScopedArtifactAvailabilityRevision,
            ScopedArtifactAvailabilityState,
        )>,
    ) -> Self {
        let mut entries = entries
            .into_iter()
            .map(
                |(artifact_key, artifact_kind, revision, state)| ScopedArtifactAvailabilityEntry {
                    artifact_key,
                    artifact_kind: artifact_kind.into(),
                    revision,
                    state,
                },
            )
            .collect::<Vec<_>>();
        entries.sort_by(|left, right| {
            (&left.artifact_key, left.artifact_kind.as_ref())
                .cmp(&(&right.artifact_key, right.artifact_kind.as_ref()))
        });
        let entry_count = entries.len() as u64;
        Self {
            contract_version: SCOPED_ARTIFACT_AVAILABILITY_CONTRACT_VERSION,
            root_session,
            entry_count,
            semantic_digest: derive_snapshot_digest(root_session, entry_count, &entries),
            entries,
        }
    }
}

impl fmt::Debug for ScopedArtifactAvailabilitySnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScopedArtifactAvailabilitySnapshot")
            .field("contract_version", &self.contract_version)
            .field("entry_count", &self.entry_count)
            .field("semantic_digest", &self.semantic_digest)
            .field("artifact_keys", &"<redacted>")
            .finish_non_exhaustive()
    }
}

pub(super) struct ScopedArtifactAvailabilityReducer {
    observations: BTreeMap<(CanonicalEntityKey, Arc<str>), ScopedArtifactAvailabilityObservation>,
}

impl ScopedArtifactAvailabilityReducer {
    pub(super) fn new() -> Self {
        Self {
            observations: BTreeMap::new(),
        }
    }

    #[cfg(test)]
    pub(super) fn observe(
        &mut self,
        observation: ScopedArtifactAvailabilityObservation,
    ) -> Result<(), ()> {
        let Some(prepared) =
            self.prepare_observe(observation.evidence.root_session(), observation)?
        else {
            return Ok(());
        };
        self.commit_observe(prepared);
        Ok(())
    }

    pub(super) fn prepare_observe(
        &self,
        root_session: CanonicalEntityKey,
        observation: ScopedArtifactAvailabilityObservation,
    ) -> Result<Option<ScopedArtifactAvailabilityPreparedObservation>, ()> {
        if !observation.validate_for_root(root_session) {
            return Err(());
        }
        let key = (
            observation.evidence.artifact_key(),
            Arc::clone(&observation.artifact_kind),
        );
        if self.observations.get(&key) == Some(&observation) {
            return Ok(None);
        }
        if !self.observations.contains_key(&key)
            && self.observations.len() >= MAX_SCOPED_ARTIFACT_EVIDENCE_ASSERTIONS
        {
            return Err(());
        }
        let entry = entry_for_observation(root_session, &observation);
        Ok(Some(ScopedArtifactAvailabilityPreparedObservation {
            key,
            occurrence: ScopedArtifactAvailabilityOccurrence {
                root_session,
                observation: observation.clone(),
                entry,
            },
            observation,
        }))
    }

    pub(super) fn commit_observe(
        &mut self,
        prepared: ScopedArtifactAvailabilityPreparedObservation,
    ) {
        self.observations.insert(prepared.key, prepared.observation);
    }

    pub(super) fn snapshot(
        &self,
        root_session: CanonicalEntityKey,
        projection: &ScopedObservationProjectionSink,
    ) -> Result<ScopedArtifactAvailabilitySnapshot, ScopedProjectionError> {
        self.snapshot_with_current(root_session, |selection| {
            projection.artifact_evidence.selection_is_current(selection)
        })
    }

    pub(super) fn snapshot_with_current<F>(
        &self,
        root_session: CanonicalEntityKey,
        mut is_current: F,
    ) -> Result<ScopedArtifactAvailabilitySnapshot, ScopedProjectionError>
    where
        F: FnMut(&ScopedArtifactEvidenceSelection) -> Result<bool, ScopedProjectionError>,
    {
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(self.observations.len())
            .map_err(|_| ScopedProjectionError::ArtifactEvidenceCapacityFull)?;
        for ((artifact_key, artifact_kind), observation) in &self.observations {
            if observation.evidence.root_session() != root_session
                || !is_current(&observation.evidence)?
            {
                continue;
            }
            debug_assert_eq!(*artifact_key, observation.evidence.artifact_key());
            debug_assert_eq!(artifact_kind.as_ref(), observation.artifact_kind.as_ref());
            entries.push(entry_for_observation(root_session, observation));
        }
        let entry_count = u64::try_from(entries.len())
            .map_err(|_| ScopedProjectionError::ArtifactEvidenceCapacityFull)?;
        Ok(ScopedArtifactAvailabilitySnapshot {
            contract_version: SCOPED_ARTIFACT_AVAILABILITY_CONTRACT_VERSION,
            root_session,
            entry_count,
            semantic_digest: derive_snapshot_digest(root_session, entry_count, &entries),
            entries,
        })
    }
}

fn entry_for_observation(
    root_session: CanonicalEntityKey,
    observation: &ScopedArtifactAvailabilityObservation,
) -> ScopedArtifactAvailabilityEntry {
    ScopedArtifactAvailabilityEntry {
        artifact_key: observation.evidence.artifact_key(),
        artifact_kind: Arc::clone(&observation.artifact_kind),
        revision: derive_entry_revision(root_session, observation),
        state: observation.state,
    }
}

fn validate_artifact_kind(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.as_bytes()[0].is_ascii_lowercase()
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte)
        })
}

fn validate_availability_state(state: ScopedArtifactAvailabilityState) -> bool {
    match state {
        ScopedArtifactAvailabilityState::Available {
            generation,
            provenance_ref,
            size_bytes,
        } => {
            generation > 0
                && generation <= JS_SAFE_INTEGER_MAX
                && provenance_ref.iter().any(|byte| *byte != 0)
                && size_bytes <= JS_SAFE_INTEGER_MAX
        }
        ScopedArtifactAvailabilityState::Missing {
            observed_generation,
            provenance_ref,
        } => {
            observed_generation.is_some() == provenance_ref.is_some()
                && observed_generation
                    .is_none_or(|generation| generation > 0 && generation <= JS_SAFE_INTEGER_MAX)
                && provenance_ref.is_none_or(|reference| reference.iter().any(|byte| *byte != 0))
        }
        ScopedArtifactAvailabilityState::OverLimit {
            generation,
            provenance_ref,
            observed_bytes,
            request_max_bytes,
        } => {
            generation > 0
                && generation <= JS_SAFE_INTEGER_MAX
                && provenance_ref.iter().any(|byte| *byte != 0)
                && request_max_bytes > 0
                && request_max_bytes <= JS_SAFE_INTEGER_MAX
                && observed_bytes <= JS_SAFE_INTEGER_MAX
                && observed_bytes > request_max_bytes
        }
        ScopedArtifactAvailabilityState::Unstable => true,
    }
}

fn derive_snapshot_digest(
    root_session: CanonicalEntityKey,
    entry_count: u64,
    entries: &[ScopedArtifactAvailabilityEntry],
) -> ScopedArtifactAvailabilityRevision {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"spaghetti/rfc012d/artifact-availability-snapshot/v1\0");
    hasher.update(&SCOPED_ARTIFACT_AVAILABILITY_CONTRACT_VERSION.to_be_bytes());
    hash_component(&mut hasher, root_session.as_bytes());
    hasher.update(&entry_count.to_be_bytes());
    for entry in entries {
        hash_component(&mut hasher, entry.artifact_key.as_bytes());
        hash_component(&mut hasher, entry.artifact_kind.as_bytes());
        hash_component(&mut hasher, entry.revision.as_bytes());
    }
    ScopedArtifactAvailabilityRevision(*hasher.finalize().as_bytes())
}

fn derive_entry_revision(
    root_session: CanonicalEntityKey,
    observation: &ScopedArtifactAvailabilityObservation,
) -> ScopedArtifactAvailabilityRevision {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"spaghetti/rfc012d/artifact-availability-revision/v1\0");
    hasher.update(&SCOPED_ARTIFACT_AVAILABILITY_CONTRACT_VERSION.to_be_bytes());
    hash_component(&mut hasher, root_session.as_bytes());
    hash_component(&mut hasher, observation.evidence.artifact_key().as_bytes());
    hash_component(&mut hasher, observation.artifact_kind.as_bytes());
    hash_component(&mut hasher, observation.relation_id.as_bytes());
    hash_component(&mut hasher, observation.object_token.as_bytes());
    hash_component(&mut hasher, observation.evidence.revision().as_bytes());
    hasher.update(&observation.evidence.version().to_be_bytes());
    match observation.state {
        ScopedArtifactAvailabilityState::Available {
            generation,
            provenance_ref,
            size_bytes,
        } => {
            hasher.update(&[1]);
            hasher.update(&generation.to_be_bytes());
            hash_component(&mut hasher, &provenance_ref);
            hasher.update(&size_bytes.to_be_bytes());
        }
        ScopedArtifactAvailabilityState::Missing {
            observed_generation,
            provenance_ref,
        } => {
            hasher.update(&[2]);
            hash_optional_u64(&mut hasher, observed_generation);
            hash_optional_component(
                &mut hasher,
                provenance_ref.as_ref().map(<[_; 32]>::as_slice),
            );
        }
        ScopedArtifactAvailabilityState::OverLimit {
            generation,
            provenance_ref,
            observed_bytes,
            request_max_bytes,
        } => {
            hasher.update(&[3]);
            hasher.update(&generation.to_be_bytes());
            hash_component(&mut hasher, &provenance_ref);
            hasher.update(&observed_bytes.to_be_bytes());
            hasher.update(&request_max_bytes.to_be_bytes());
        }
        ScopedArtifactAvailabilityState::Unstable => {
            hasher.update(&[4]);
        }
    }
    ScopedArtifactAvailabilityRevision(*hasher.finalize().as_bytes())
}

fn hash_component(hasher: &mut blake3::Hasher, value: &[u8]) {
    hasher.update(&(value.len() as u64).to_be_bytes());
    hasher.update(value);
}

fn hash_optional_component(hasher: &mut blake3::Hasher, value: Option<&[u8]>) {
    match value {
        Some(value) => {
            hasher.update(&[1]);
            hash_component(hasher, value);
        }
        None => {
            hasher.update(&[0]);
        }
    }
}

fn hash_optional_u64(hasher: &mut blake3::Hasher, value: Option<u64>) {
    match value {
        Some(value) => {
            hasher.update(&[1]);
            hasher.update(&value.to_be_bytes());
        }
        None => {
            hasher.update(&[0]);
        }
    }
}

#[cfg(test)]
mod tests;
