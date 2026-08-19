use std::collections::BTreeSet;
use std::sync::Arc;

use crate::adapter::{AdapterId, CanonicalEntityKey, CanonicalSourceInstanceKey};
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
    ScopedArtifactAvailabilityObservation::new(
        selection,
        Arc::from(kind),
        Arc::from(relation),
        token,
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
