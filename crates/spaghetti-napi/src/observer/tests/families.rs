//! Every RFC 012C family survives the reduce-to-event path.
//!
//! The previous observer answered eight of the eleven families with
//! `PortableEventContractUnavailable` and stalled the queue. This test drives
//! all eleven through the same reducer, state, and manifest path the observer
//! uses at runtime, from the committed RFC 012C contract fixtures.
//!
//! Note what it does *not* claim: the Claude adapter today emits only
//! actor-run, actor-affiliation, and usage-v2 from a transcript. Adding the
//! other eight is adapter work (RFC 012A), not observer work — but no family
//! can fail at the observer's wire any more.

use crate::engine::runtime_semantic_merge::tests::all_runtime_family_contributions;
use crate::observer::event::ObserverFamily;
use crate::observer::object::DecodedFact;
use crate::observer::scope::ScopeMemberKey;
use crate::observer::state::{ScopeState, StateChange};

fn owner() -> ScopeMemberKey {
    ScopeMemberKey {
        stream_id: "session-transcripts".to_string(),
        root_name: "projects".to_string(),
        relative_path: std::path::PathBuf::from("fixture/session.jsonl"),
    }
}

fn decoded(
    contribution: &crate::engine::runtime_semantic_merge::DurableRuntimeContribution,
) -> DecodedFact {
    DecodedFact {
        semantic: contribution.semantic,
        value: contribution.revision.clone(),
        native_time: None,
        observed_at: 0,
        byte_start: Some(0),
        byte_end: Some(1),
        record_digest: [0_u8; 32],
        generation: 1,
    }
}

#[test]
fn all_eleven_families_reduce_and_appear_in_the_replacement_manifest() {
    let contributions = all_runtime_family_contributions();
    assert_eq!(
        contributions.len(),
        11,
        "the RFC 012C fixture set should cover every runtime family"
    );

    let mut state = ScopeState::default();
    let owner = owner();
    let mut accepted = Vec::new();
    for (family, contribution) in &contributions {
        let fact = decoded(contribution);
        let (observed, change) = state
            .apply(&fact, &owner)
            .unwrap_or_else(|| panic!("{family:?} was rejected by the observer reducer"));
        assert_eq!(
            observed,
            ObserverFamily::from(*family),
            "family classification disagrees with the shared reducer"
        );
        assert!(
            matches!(change, StateChange::Upsert { .. }),
            "{family:?} did not reduce to an upsert on first sight"
        );
        accepted.push(observed);
    }

    let manifest = state.family_manifest();
    assert_eq!(manifest.len(), 11);
    for entry in &manifest {
        assert_eq!(
            entry.entity_count, 1,
            "{:?} is missing from the replacement manifest",
            entry.family
        );
        assert!(
            !entry.digest.is_empty(),
            "{:?} produced no replacement digest",
            entry.family
        );
    }
}

#[test]
fn an_exact_repeat_of_a_revision_adds_nothing() {
    let contributions = all_runtime_family_contributions();
    let mut state = ScopeState::default();
    let owner = owner();
    for (_, contribution) in &contributions {
        state.apply(&decoded(contribution), &owner);
    }
    let before = state.family_manifest();

    for (family, contribution) in &contributions {
        let (_, change) = state
            .apply(&decoded(contribution), &owner)
            .expect("a repeated revision is still a known family");
        assert!(
            matches!(change, StateChange::Unchanged),
            "{family:?} re-delivered an identical revision instead of suppressing it"
        );
    }
    assert_eq!(
        before,
        state.family_manifest(),
        "a repeated revision changed reduced state"
    );
}

#[test]
fn retracting_an_objects_facts_empties_its_families() {
    let contributions = all_runtime_family_contributions();
    let mut state = ScopeState::default();
    let owner = owner();
    for (_, contribution) in &contributions {
        state.apply(&decoded(contribution), &owner);
    }

    let retracted = state.retract_owned(&owner, None);
    assert_eq!(
        retracted.len(),
        11,
        "every fact owned by a removed object must be retracted"
    );
    for entry in state.family_manifest() {
        assert_eq!(
            entry.entity_count, 0,
            "{:?} kept state after its owning object went away",
            entry.family
        );
    }
}
