use serde_json::{json, Value as JsonValue};

use super::*;

const FIXTURE: &str =
    include_str!("../../../fixtures/contracts/rfc012d-scoped-known-envelope-v1.json");
const SOURCE_FIXTURE: &str =
    include_str!("../../../fixtures/contracts/rfc012d-scoped-source-envelope-v1.json");

#[test]
fn known_outer_wire_is_strictly_named_and_fixture_bound() {
    let source: JsonValue = serde_json::from_str(SOURCE_FIXTURE).unwrap();
    let actual = json!({
        "fixture_contract_version": 1,
        "source_created": known_wire_value(
            ScopedObservationKnownEventFamily::Source,
            source["context"].clone(),
            source["created"].clone(),
        ),
        "expected": {
            "known_family_count": 6,
            "complete_event_union": false,
            "unknown_wire_event": "not_yet_negotiated",
            "attachment_authority_serialized": false,
        }
    });
    let expected: JsonValue = serde_json::from_str(FIXTURE).unwrap();
    if expected != actual {
        panic!("{}", serde_json::to_string_pretty(&actual).unwrap());
    }
}

#[test]
fn all_known_family_discriminators_are_closed_and_canonical() {
    assert_eq!(
        [
            ScopedObservationKnownEventFamily::Usage,
            ScopedObservationKnownEventFamily::Actor,
            ScopedObservationKnownEventFamily::Source,
            ScopedObservationKnownEventFamily::ArtifactAvailability,
            ScopedObservationKnownEventFamily::Completion,
            ScopedObservationKnownEventFamily::Continuity,
        ]
        .map(ScopedObservationKnownEventFamily::wire_name),
        [
            "usage",
            "actor",
            "source",
            "artifact_availability",
            "completion",
            "continuity",
        ]
    );
}
