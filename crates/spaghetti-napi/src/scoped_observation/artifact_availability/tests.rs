use std::collections::BTreeSet;
use std::sync::Arc;

use crate::adapter::{
    AdapterId, CanonicalEntityKey, CanonicalSourceInstanceKey, CoverageObjectKey, CoverageStreamKey,
};
use crate::source::AccessObjectToken;

use super::*;

fn identity(seed: &[u8], kind: &str) -> CanonicalEntityKey {
    let adapter = AdapterId::new("fixture").unwrap();
    let source = CanonicalSourceInstanceKey::derive(1, b"availability-source").unwrap();
    CanonicalEntityKey::derive(adapter.as_str(), &source, kind, seed).unwrap()
}

fn selection(
    root: CanonicalEntityKey,
    artifact: CanonicalEntityKey,
    version: u64,
    revision_byte: u8,
) -> ScopedArtifactEvidenceSelection {
    ScopedArtifactEvidenceSelection::fixture(
        root,
        artifact,
        format!("native-{version}"),
        version,
        [revision_byte; 32],
    )
}

fn observation(
    selection: ScopedArtifactEvidenceSelection,
    kind: &str,
    relation: &str,
    state: ScopedArtifactAvailabilityState,
) -> ScopedArtifactAvailabilityObservation {
    let token = AccessObjectToken::derive(
        relation,
        &[
            b"artifact".as_slice(),
            selection.artifact_key().as_bytes(),
            b"version".as_slice(),
            selection.version().to_string().as_bytes(),
        ],
    )
    .unwrap();
    let source_generation = match state {
        ScopedArtifactAvailabilityState::Available { generation, .. }
        | ScopedArtifactAvailabilityState::OverLimit { generation, .. } => generation,
        ScopedArtifactAvailabilityState::Missing {
            observed_generation,
            ..
        } => observed_generation.unwrap_or(1),
        ScopedArtifactAvailabilityState::Unstable => 1,
    };
    let source_instance_key =
        CanonicalSourceInstanceKey::derive(1, b"availability-source").unwrap();
    let stream_key = CoverageStreamKey::derive("fixture", relation.as_bytes()).unwrap();
    let object_key = CoverageObjectKey::derive(relation, token.as_bytes()).unwrap();
    ScopedArtifactAvailabilityObservation::new(
        selection,
        Arc::from(kind),
        Arc::from(relation),
        token,
        ScopedArtifactAvailabilitySourceOccurrence::new(
            [9; 32],
            ScopedSourceObjectIdentity {
                adapter_id: AdapterId::new("fixture").unwrap(),
                source_instance_key,
                stream_key,
                object_key,
            },
            source_generation,
        ),
        state,
    )
}

fn snapshot_with(
    reducer: &ScopedArtifactAvailabilityReducer,
    root: CanonicalEntityKey,
    current: &BTreeSet<(CanonicalEntityKey, [u8; 32])>,
) -> ScopedArtifactAvailabilitySnapshot {
    reducer
        .snapshot_with_current(root, |selection| {
            Ok(current.contains(&(selection.artifact_key(), *selection.revision().as_bytes())))
        })
        .unwrap()
}

#[test]
fn snapshot_is_canonical_current_evidence_bound_and_path_free() {
    let root = identity(b"root", "session");
    let first_key = identity(b"artifact-a", "artifact");
    let second_key = identity(b"artifact-b", "artifact");
    let first = selection(root, first_key, 7, 1);
    let second = selection(root, second_key, 8, 2);
    let first_observation = observation(
        first.clone(),
        "workflow_definition",
        "file-artifact",
        ScopedArtifactAvailabilityState::Available {
            generation: 3,
            provenance_ref: [3; 32],
            size_bytes: 91,
        },
    );
    let second_observation = observation(
        second.clone(),
        "workflow_journal",
        "journal-artifact",
        ScopedArtifactAvailabilityState::Missing {
            observed_generation: Some(4),
            provenance_ref: Some([4; 32]),
        },
    );

    let mut forward = ScopedArtifactAvailabilityReducer::new();
    forward.observe(first_observation.clone()).unwrap();
    forward.observe(second_observation.clone()).unwrap();
    let mut reversed = ScopedArtifactAvailabilityReducer::new();
    reversed.observe(second_observation).unwrap();
    reversed.observe(first_observation).unwrap();
    let current = BTreeSet::from([
        (first_key, *first.revision().as_bytes()),
        (second_key, *second.revision().as_bytes()),
    ]);
    let forward_snapshot = snapshot_with(&forward, root, &current);
    let reversed_snapshot = snapshot_with(&reversed, root, &current);
    assert_eq!(forward_snapshot, reversed_snapshot);
    assert_eq!(forward_snapshot.entry_count(), 2);
    assert!(forward_snapshot.validate_for_root(root));
    assert!(!forward_snapshot.validate_for_root(identity(b"foreign", "session")));
    let mut malformed = forward_snapshot.clone();
    malformed.entries.reverse();
    assert!(!malformed.validate_for_root(root));
    let mut wrong_count = forward_snapshot.clone();
    wrong_count.entry_count += 1;
    assert!(!wrong_count.validate_for_root(root));
    let mut zero_revision = forward_snapshot.clone();
    zero_revision.entries[0].revision = ScopedArtifactAvailabilityRevision([0; 32]);
    assert!(!zero_revision.validate_for_root(root));
    let mut invalid_state = forward_snapshot.clone();
    invalid_state.entries[0].state = ScopedArtifactAvailabilityState::Available {
        generation: 0,
        provenance_ref: [3; 32],
        size_bytes: 1,
    };
    assert!(!invalid_state.validate_for_root(root));

    let stale = BTreeSet::from([(first_key, [9; 32])]);
    let stale_snapshot = snapshot_with(&forward, root, &stale);
    assert_eq!(stale_snapshot.entry_count(), 0);
    assert_ne!(
        stale_snapshot.semantic_digest(),
        forward_snapshot.semantic_digest()
    );

    let debug = format!("{forward_snapshot:?}");
    assert!(debug.contains("artifact_keys: \"<redacted>\""));
    for secret in ["native-7", "native-8", "file-artifact", "journal-artifact"] {
        assert!(!debug.contains(secret));
    }
}

#[test]
fn native_state_and_probe_bound_change_the_availability_revision() {
    let root = identity(b"root", "session");
    let artifact = identity(b"artifact", "artifact");
    let evidence = selection(root, artifact, 7, 1);
    let current = BTreeSet::from([(artifact, *evidence.revision().as_bytes())]);
    let states = [
        ScopedArtifactAvailabilityState::Available {
            generation: 1,
            provenance_ref: [1; 32],
            size_bytes: 9,
        },
        ScopedArtifactAvailabilityState::Missing {
            observed_generation: Some(2),
            provenance_ref: Some([2; 32]),
        },
        ScopedArtifactAvailabilityState::OverLimit {
            generation: 3,
            provenance_ref: [3; 32],
            observed_bytes: 10,
            request_max_bytes: 9,
        },
        ScopedArtifactAvailabilityState::OverLimit {
            generation: 3,
            provenance_ref: [3; 32],
            observed_bytes: 10,
            request_max_bytes: 8,
        },
        ScopedArtifactAvailabilityState::Unstable,
    ];
    let mut digests = BTreeSet::new();
    for state in states {
        let mut reducer = ScopedArtifactAvailabilityReducer::new();
        reducer
            .observe(observation(
                evidence.clone(),
                "workflow_definition",
                "file-artifact",
                state,
            ))
            .unwrap();
        digests.insert(
            *snapshot_with(&reducer, root, &current)
                .semantic_digest()
                .as_bytes(),
        );
    }
    assert_eq!(digests.len(), 5);
}

#[test]
fn latest_observation_replaces_one_key_and_capacity_is_exact() {
    let root = identity(b"root", "session");
    let artifact = identity(b"artifact", "artifact");
    let evidence = selection(root, artifact, 7, 1);
    let mut reducer = ScopedArtifactAvailabilityReducer::new();
    reducer
        .observe(observation(
            evidence.clone(),
            "workflow_definition",
            "file-artifact",
            ScopedArtifactAvailabilityState::Unstable,
        ))
        .unwrap();
    reducer
        .observe(observation(
            evidence,
            "workflow_definition",
            "file-artifact",
            ScopedArtifactAvailabilityState::Available {
                generation: 1,
                provenance_ref: [1; 32],
                size_bytes: 4,
            },
        ))
        .unwrap();
    assert_eq!(reducer.observations.len(), 1);

    for index in 1..MAX_SCOPED_ARTIFACT_EVIDENCE_ASSERTIONS {
        let key = identity(format!("artifact-{index}").as_bytes(), "artifact");
        reducer
            .observe(observation(
                selection(root, key, 1, 1),
                "workflow_definition",
                &format!("relation-{index}"),
                ScopedArtifactAvailabilityState::Unstable,
            ))
            .unwrap();
    }
    assert_eq!(
        reducer.observations.len(),
        MAX_SCOPED_ARTIFACT_EVIDENCE_ASSERTIONS
    );
    let overflow_key = identity(b"overflow", "artifact");
    assert!(reducer
        .observe(observation(
            selection(root, overflow_key, 1, 1),
            "workflow_definition",
            "overflow-relation",
            ScopedArtifactAvailabilityState::Unstable,
        ))
        .is_err());
    assert_eq!(
        reducer.observations.len(),
        MAX_SCOPED_ARTIFACT_EVIDENCE_ASSERTIONS
    );
}

#[test]
fn prepare_offer_commit_is_atomic_replay_safe_and_source_bound() {
    let root = identity(b"root", "session");
    let artifact = identity(b"artifact", "artifact");
    let evidence = selection(root, artifact, 7, 1);
    let observation = observation(
        evidence,
        "workflow_definition",
        "file-artifact",
        ScopedArtifactAvailabilityState::Unstable,
    );
    let mut reducer = ScopedArtifactAvailabilityReducer::new();
    let prepared = reducer
        .prepare_observe(root, observation.clone())
        .unwrap()
        .unwrap();
    assert!(reducer.observations.is_empty());
    assert!(prepared.occurrence().validate_for_root(root));
    let first_event_id = super::super::artifact_availability_event_id(prepared.occurrence());
    let first_revision = prepared.occurrence().entry().revision();
    reducer.commit_observe(prepared);
    assert_eq!(reducer.observations.len(), 1);
    assert!(reducer
        .prepare_observe(root, observation.clone())
        .unwrap()
        .is_none());

    let mut declaration_drift = observation.clone();
    declaration_drift.source.source_declaration_digest = [10; 32];
    let prepared = reducer
        .prepare_observe(root, declaration_drift)
        .unwrap()
        .unwrap();
    assert_eq!(prepared.occurrence().entry().revision(), first_revision);
    assert_ne!(
        super::super::artifact_availability_event_id(prepared.occurrence()),
        first_event_id
    );
    assert_eq!(reducer.observations.len(), 1);

    let mut generation_drift = observation.clone();
    generation_drift.source.generation = 2;
    let prepared = reducer
        .prepare_observe(root, generation_drift)
        .unwrap()
        .unwrap();
    assert_eq!(prepared.occurrence().entry().revision(), first_revision);
    assert_ne!(
        super::super::artifact_availability_event_id(prepared.occurrence()),
        first_event_id
    );

    let mut invalid_generation = observation;
    invalid_generation.state = ScopedArtifactAvailabilityState::Available {
        generation: 2,
        provenance_ref: [3; 32],
        size_bytes: 1,
    };
    assert!(reducer.prepare_observe(root, invalid_generation).is_err());
    assert_eq!(reducer.observations.len(), 1);
}
