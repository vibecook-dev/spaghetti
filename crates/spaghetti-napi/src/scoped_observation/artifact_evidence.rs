//! Admission-bound, path-free artifact metadata evidence.
//!
//! This reducer deliberately stops before `ArtifactLocatorFromEvidence`
//! authorization. It retains only metadata facts already admitted by the
//! scoped projection, derives canonical evidence revisions, and follows the
//! source object's reset/deletion lifecycle. Native locator construction and
//! artifact reads remain separate future authority boundaries.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

use crate::adapter::{
    ArtifactCapture, ArtifactMetadataSnapshotFact, CanonicalEntityKey, CanonicalFactId,
    FactEnvelope, FactRevisionId,
};

use super::{
    scoped_context_semantic_source, ScopedDecodedRecordEvidence, ScopedProjectionError,
    ScopedSourceObjectIdentity, ScopedUsageV2Source,
};

pub(super) const SCOPED_ARTIFACT_EVIDENCE_CONTRACT_VERSION: u32 = 1;
const MAX_SCOPED_ARTIFACT_EVIDENCE_ASSERTIONS: usize = 4_096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum ScopedArtifactEvidenceDisposition {
    ContentExpected,
    NotCaptured,
    Conflicting,
}

#[derive(Clone, PartialEq, Eq)]
struct ScopedArtifactAssertion {
    artifact_key: CanonicalEntityKey,
    native_artifact_id: Option<Arc<str>>,
    version: u64,
    capture: ArtifactCapture,
}

#[derive(Clone)]
pub(super) struct ScopedArtifactEvidenceFactState {
    object_token: u64,
    generation: u64,
    semantic: crate::adapter::FactSemanticRevision,
    source: ScopedUsageV2Source,
    assertions: Vec<ScopedArtifactAssertion>,
}

#[derive(Default)]
pub(super) struct ScopedArtifactEvidenceMutation {
    upserts: BTreeMap<CanonicalFactId, ScopedArtifactEvidenceFactState>,
    retractions: BTreeSet<CanonicalFactId>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct ScopedArtifactEvidenceRevision([u8; 32]);

impl ScopedArtifactEvidenceRevision {
    pub(super) fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for ScopedArtifactEvidenceRevision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("ScopedArtifactEvidenceRevision")
            .field(&"v1:<redacted>")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct ScopedArtifactEvidenceEntry {
    artifact_key: CanonicalEntityKey,
    disposition: ScopedArtifactEvidenceDisposition,
    evidence_count: u64,
    revision: ScopedArtifactEvidenceRevision,
}

impl fmt::Debug for ScopedArtifactEvidenceEntry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScopedArtifactEvidenceEntry")
            .field("artifact_key", &self.artifact_key)
            .field("disposition", &self.disposition)
            .field("evidence_count", &self.evidence_count)
            .field("revision", &self.revision)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct ScopedArtifactEvidenceSnapshot {
    contract_version: u32,
    root_session: CanonicalEntityKey,
    entry_count: u64,
    semantic_digest: ScopedArtifactEvidenceRevision,
    entries: Vec<ScopedArtifactEvidenceEntry>,
}

impl ScopedArtifactEvidenceSnapshot {
    pub(super) fn semantic_digest(&self) -> ScopedArtifactEvidenceRevision {
        self.semantic_digest
    }
}

impl fmt::Debug for ScopedArtifactEvidenceSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScopedArtifactEvidenceSnapshot")
            .field("contract_version", &self.contract_version)
            .field("root_session", &self.root_session)
            .field("entry_count", &self.entry_count)
            .field("semantic_digest", &self.semantic_digest)
            .field("entries", &self.entries)
            .finish()
    }
}

pub(super) struct ScopedArtifactEvidenceReducer {
    root_session: Option<CanonicalEntityKey>,
    facts: BTreeMap<CanonicalFactId, ScopedArtifactEvidenceFactState>,
}

impl ScopedArtifactEvidenceReducer {
    pub(super) fn new(root_session: Option<CanonicalEntityKey>) -> Self {
        Self {
            root_session,
            facts: BTreeMap::new(),
        }
    }

    pub(super) fn prepare_metadata(
        &self,
        mutation: &mut ScopedArtifactEvidenceMutation,
        object_token: u64,
        source: &ScopedSourceObjectIdentity,
        evidence: &ScopedDecodedRecordEvidence,
        envelope: &FactEnvelope,
        fact: &ArtifactMetadataSnapshotFact,
    ) -> Result<(), ScopedProjectionError> {
        let Some(root_session) = self.root_session else {
            return Ok(());
        };
        let Some(canonical_session) = fact.canonical_session else {
            if fact
                .artifacts
                .iter()
                .any(|artifact| artifact.canonical_artifact.is_some())
            {
                return Err(ScopedProjectionError::InvalidArtifactEvidence);
            }
            return Ok(());
        };
        if canonical_session != root_session {
            return Err(ScopedProjectionError::InvalidArtifactEvidence);
        }

        let (semantic, source) = scoped_context_semantic_source(source, evidence, envelope)?;
        let revision_key = fact
            .semantic_revision_key()
            .map_err(|_| ScopedProjectionError::InvalidArtifactEvidence)?
            .ok_or(ScopedProjectionError::InvalidArtifactEvidence)?;
        if FactRevisionId::derive(&semantic.fact_id, 1, &revision_key)
            .map_err(|_| ScopedProjectionError::InvalidArtifactEvidence)?
            != semantic.fact_revision_id
        {
            return Err(ScopedProjectionError::InvalidArtifactEvidence);
        }

        let mut assertions = Vec::new();
        assertions
            .try_reserve_exact(fact.artifacts.len())
            .map_err(|_| ScopedProjectionError::ArtifactEvidenceCapacityFull)?;
        for artifact in &fact.artifacts {
            let artifact_key = artifact
                .canonical_artifact
                .ok_or(ScopedProjectionError::InvalidArtifactEvidence)?;
            let native_artifact_id =
                match (artifact.capture, artifact.native_artifact_id.as_deref()) {
                    (ArtifactCapture::ContentExpected, Some(value)) if !value.trim().is_empty() => {
                        Some(Arc::from(value))
                    }
                    (ArtifactCapture::NotCaptured, None) => None,
                    _ => return Err(ScopedProjectionError::InvalidArtifactEvidence),
                };
            if artifact.version == 0 {
                return Err(ScopedProjectionError::InvalidArtifactEvidence);
            }
            assertions.push(ScopedArtifactAssertion {
                artifact_key,
                native_artifact_id,
                version: artifact.version,
                capture: artifact.capture,
            });
        }
        assertions.sort_by_key(|assertion| assertion.artifact_key);
        if assertions
            .windows(2)
            .any(|window| window[0].artifact_key == window[1].artifact_key)
        {
            return Err(ScopedProjectionError::InvalidArtifactEvidence);
        }

        let next = ScopedArtifactEvidenceFactState {
            object_token,
            generation: evidence.generation,
            semantic,
            source,
            assertions,
        };
        let current = mutation
            .upserts
            .get(&next.semantic.fact_id)
            .or_else(|| self.facts.get(&next.semantic.fact_id));
        if let Some(current) = current {
            if current.object_token != next.object_token
                || current.generation != next.generation
                || current.source.object != next.source.object
            {
                return Err(ScopedProjectionError::ConflictingOwnership);
            }
            if current.semantic.fact_revision_id == next.semantic.fact_revision_id {
                return if current.semantic == next.semantic && current.assertions == next.assertions
                {
                    Ok(())
                } else {
                    Err(ScopedProjectionError::InvalidArtifactEvidence)
                };
            }
            let old_cursor = current.source.cursor_end.append_offset_value();
            let next_cursor = next.source.cursor_end.append_offset_value();
            if old_cursor
                .zip(next_cursor)
                .is_none_or(|(old, next)| next <= old)
            {
                return Err(ScopedProjectionError::StaleRevision);
            }
        }
        mutation.upserts.insert(next.semantic.fact_id, next);
        self.validate_capacity(mutation)
    }

    pub(super) fn prepare_object_retractions(
        &self,
        object_token: u64,
        generation: u64,
        mismatch: ScopedProjectionError,
    ) -> Result<ScopedArtifactEvidenceMutation, ScopedProjectionError> {
        if self
            .facts
            .values()
            .any(|state| state.object_token == object_token && state.generation != generation)
        {
            return Err(mismatch);
        }
        Ok(ScopedArtifactEvidenceMutation {
            upserts: BTreeMap::new(),
            retractions: self
                .facts
                .iter()
                .filter_map(|(fact_id, state)| {
                    (state.object_token == object_token && state.generation == generation)
                        .then_some(*fact_id)
                })
                .collect(),
        })
    }

    pub(super) fn has_object(&self, object_token: u64) -> bool {
        self.facts
            .values()
            .any(|state| state.object_token == object_token)
    }

    pub(super) fn validate_capacity(
        &self,
        mutation: &ScopedArtifactEvidenceMutation,
    ) -> Result<(), ScopedProjectionError> {
        let retained_fact_count = self
            .facts
            .keys()
            .filter(|fact_id| {
                !mutation.retractions.contains(fact_id) && !mutation.upserts.contains_key(fact_id)
            })
            .count();
        if retained_fact_count
            .checked_add(mutation.upserts.len())
            .is_none_or(|count| count > MAX_SCOPED_ARTIFACT_EVIDENCE_ASSERTIONS)
        {
            return Err(ScopedProjectionError::ArtifactEvidenceCapacityFull);
        }
        let retained_assertion_count = self
            .facts
            .iter()
            .filter(|(fact_id, _)| {
                !mutation.retractions.contains(fact_id) && !mutation.upserts.contains_key(fact_id)
            })
            .try_fold(0_usize, |count, (_, state)| {
                count.checked_add(state.assertions.len())
            });
        let total_assertion_count = mutation
            .upserts
            .values()
            .fold(retained_assertion_count, |count, state| {
                count.and_then(|count| count.checked_add(state.assertions.len()))
            });
        if total_assertion_count.is_none_or(|count| count > MAX_SCOPED_ARTIFACT_EVIDENCE_ASSERTIONS)
        {
            return Err(ScopedProjectionError::ArtifactEvidenceCapacityFull);
        }
        Ok(())
    }

    pub(super) fn commit(&mut self, mutation: ScopedArtifactEvidenceMutation) {
        for fact_id in mutation.retractions {
            self.facts.remove(&fact_id);
        }
        self.facts.extend(mutation.upserts);
    }

    pub(super) fn rollback_replacement_object(&mut self, object_token: u64) {
        self.facts
            .retain(|_, state| state.object_token != object_token);
    }

    pub(super) fn snapshot(&self) -> Result<ScopedArtifactEvidenceSnapshot, ScopedProjectionError> {
        let root_session = self
            .root_session
            .ok_or(ScopedProjectionError::InvalidArtifactEvidence)?;
        let mut grouped = BTreeMap::<
            CanonicalEntityKey,
            Vec<(&ScopedArtifactEvidenceFactState, &ScopedArtifactAssertion)>,
        >::new();
        for state in self.facts.values() {
            for assertion in &state.assertions {
                grouped
                    .entry(assertion.artifact_key)
                    .or_default()
                    .push((state, assertion));
            }
        }
        if grouped.len() > MAX_SCOPED_ARTIFACT_EVIDENCE_ASSERTIONS {
            return Err(ScopedProjectionError::ArtifactEvidenceCapacityFull);
        }

        let mut entries = Vec::new();
        entries
            .try_reserve_exact(grouped.len())
            .map_err(|_| ScopedProjectionError::ArtifactEvidenceCapacityFull)?;
        for (artifact_key, assertions) in grouped {
            let first = assertions
                .first()
                .ok_or(ScopedProjectionError::InvalidArtifactEvidence)?;
            let agrees = assertions.iter().all(|(_, assertion)| {
                assertion.capture == first.1.capture
                    && assertion.version == first.1.version
                    && assertion.native_artifact_id == first.1.native_artifact_id
            });
            let disposition = if !agrees {
                ScopedArtifactEvidenceDisposition::Conflicting
            } else {
                match first.1.capture {
                    ArtifactCapture::ContentExpected => {
                        ScopedArtifactEvidenceDisposition::ContentExpected
                    }
                    ArtifactCapture::NotCaptured => ScopedArtifactEvidenceDisposition::NotCaptured,
                }
            };
            let evidence_count = u64::try_from(assertions.len())
                .map_err(|_| ScopedProjectionError::ArtifactEvidenceCapacityFull)?;
            let revision = derive_entry_revision(
                root_session,
                artifact_key,
                disposition,
                first.1,
                &assertions,
            );
            entries.push(ScopedArtifactEvidenceEntry {
                artifact_key,
                disposition,
                evidence_count,
                revision,
            });
        }
        let entry_count = u64::try_from(entries.len())
            .map_err(|_| ScopedProjectionError::ArtifactEvidenceCapacityFull)?;
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"spaghetti/rfc012d/artifact-evidence-snapshot\0");
        hasher.update(&SCOPED_ARTIFACT_EVIDENCE_CONTRACT_VERSION.to_be_bytes());
        hash_component(&mut hasher, root_session.as_bytes());
        hasher.update(&entry_count.to_be_bytes());
        for entry in &entries {
            hash_component(&mut hasher, entry.artifact_key.as_bytes());
            hasher.update(&[disposition_tag(entry.disposition)]);
            hasher.update(&entry.evidence_count.to_be_bytes());
            hash_component(&mut hasher, entry.revision.as_bytes());
        }
        Ok(ScopedArtifactEvidenceSnapshot {
            contract_version: SCOPED_ARTIFACT_EVIDENCE_CONTRACT_VERSION,
            root_session,
            entry_count,
            semantic_digest: ScopedArtifactEvidenceRevision(*hasher.finalize().as_bytes()),
            entries,
        })
    }
}

fn derive_entry_revision(
    root_session: CanonicalEntityKey,
    artifact_key: CanonicalEntityKey,
    disposition: ScopedArtifactEvidenceDisposition,
    representative: &ScopedArtifactAssertion,
    assertions: &[(&ScopedArtifactEvidenceFactState, &ScopedArtifactAssertion)],
) -> ScopedArtifactEvidenceRevision {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"spaghetti/rfc012d/artifact-evidence-revision\0");
    hasher.update(&SCOPED_ARTIFACT_EVIDENCE_CONTRACT_VERSION.to_be_bytes());
    hash_component(&mut hasher, root_session.as_bytes());
    hash_component(&mut hasher, artifact_key.as_bytes());
    hasher.update(&[disposition_tag(disposition)]);
    hasher.update(&representative.version.to_be_bytes());
    hash_optional_component(
        &mut hasher,
        representative
            .native_artifact_id
            .as_deref()
            .map(str::as_bytes),
    );
    hasher.update(&(assertions.len() as u64).to_be_bytes());
    for (state, _) in assertions {
        hash_component(&mut hasher, state.semantic.fact_id.as_bytes());
        hash_component(&mut hasher, state.semantic.fact_revision_id.as_bytes());
        hash_component(&mut hasher, state.semantic.source_record_id.as_bytes());
    }
    ScopedArtifactEvidenceRevision(*hasher.finalize().as_bytes())
}

const fn disposition_tag(disposition: ScopedArtifactEvidenceDisposition) -> u8 {
    match disposition {
        ScopedArtifactEvidenceDisposition::ContentExpected => 1,
        ScopedArtifactEvidenceDisposition::NotCaptured => 2,
        ScopedArtifactEvidenceDisposition::Conflicting => 3,
    }
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

#[cfg(test)]
mod tests;
