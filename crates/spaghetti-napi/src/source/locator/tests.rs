//! Confinement at resolution: what a declared template may render to.
//!
//! Carried over from `source/access.rs`. The cases are that module's,
//! unchanged, but they now exercise the rules directly instead of through an
//! access reservation — which is also how the observer's ScopeProgram
//! evaluator will call them. The reservation-bound binding check stays in
//! `source/access.rs` with the reservation it belongs to.

use super::*;
use crate::adapter::{ScopeProgramManifest, ScopeRelationDeclaration};

/// A real declared manifest, so the fixture cannot drift from the shape the
/// support bundles actually carry.
fn artifact_declaration(locator: &str) -> ScopeRelationDeclaration {
    let mut manifest = ScopeProgramManifest::from_json(include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../agent-support/grok/2026-08-15/scope-programs.json"
    )))
    .expect("the committed Grok manifest parses");
    let relation = manifest.programs[0]
        .relations
        .iter_mut()
        .find(|relation| relation.relation_id == "summary-sidecar")
        .expect("the Grok program declares a summary sidecar");
    relation.primitive = ScopeRelationPrimitive::ArtifactLocatorFromEvidence;
    relation.locator = locator.to_string();
    relation.identity_inputs = vec![
        "native-session-id".to_string(),
        "backup-name".to_string(),
        "artifact-version".to_string(),
    ];
    relation.clone()
}

fn render(
    locator: &str,
    native_session_id: &[u8],
    backup_name: &[u8],
    artifact_version: &[u8],
) -> Result<PathBuf, LocatorError> {
    validate_evidence_locator_template(&artifact_declaration(locator))?;
    render_confined_locator(
        locator,
        &[
            ScopeIdentityInput {
                name: "native-session-id",
                value: native_session_id,
            },
            ScopeIdentityInput {
                name: "backup-name",
                value: backup_name,
            },
            ScopeIdentityInput {
                name: "artifact-version",
                value: artifact_version,
            },
        ],
    )
}

#[test]
fn a_template_renders_only_the_identity_inputs_it_names() {
    assert_eq!(
        render(
            "file-history/{native-session-id}/{backup-name}.{artifact-version}",
            b"session-7",
            b"backup-a",
            b"9",
        )
        .expect("a well-formed template with bound values renders"),
        PathBuf::from("file-history/session-7/backup-a.9")
    );
}

#[test]
fn a_placeholder_with_no_supplied_value_is_an_error() {
    assert!(render_confined_locator(
        "file-history/{native-session-id}/{backup-name}",
        &[ScopeIdentityInput {
            name: "native-session-id",
            value: b"session-7",
        }],
    )
    .is_err());
}

#[test]
fn a_repeated_input_name_is_an_error_but_an_unused_one_is_not() {
    let repeated = render_confined_locator(
        "file-history/{backup-name}",
        &[
            ScopeIdentityInput {
                name: "backup-name",
                value: b"backup-a",
            },
            ScopeIdentityInput {
                name: "backup-name",
                value: b"backup-b",
            },
        ],
    );
    assert!(
        repeated.is_err(),
        "two values for one placeholder is ambiguous"
    );

    // An input the template does not name is tolerated here on purpose: a
    // caller may hold one identity set and render several relations from it,
    // each naming a subset. Requiring the set to match the declaration exactly
    // is a separate check against the declaration — see
    // `validate_evidence_locator_template` and the reservation binding in
    // `source/access.rs`.
    assert_eq!(
        render_confined_locator(
            "file-history/{backup-name}",
            &[
                ScopeIdentityInput {
                    name: "backup-name",
                    value: b"backup-a",
                },
                ScopeIdentityInput {
                    name: "artifact-version",
                    value: b"9",
                },
            ],
        )
        .expect("an unused input does not invalidate the render"),
        PathBuf::from("file-history/backup-a")
    );
}

#[test]
fn a_conceptual_or_ambiguous_template_is_not_a_locator() {
    for locator in [
        // No placeholder at all: one fixed path whatever the evidence says.
        "declared-artifact-locator",
        // Names an input the relation never declared.
        "file-history/{unknown-input}",
        // The same input twice.
        "file-history/{backup-name}/{backup-name}",
        // Unbalanced and nested braces.
        "file-history/{backup-name",
        "file-history/backup-name}",
        "file-history/{{backup-name}}",
    ] {
        assert!(
            validate_evidence_locator_template(&artifact_declaration(locator)).is_err(),
            "accepted {locator:?}"
        );
    }
}

#[test]
fn a_bound_value_cannot_escape_its_directory() {
    for native_session_id in [
        b"..".as_slice(),
        b".".as_slice(),
        b"C:".as_slice(),
        b"nested/session".as_slice(),
        b"nested\\session".as_slice(),
        b"line\nbreak".as_slice(),
        b"\xff".as_slice(),
    ] {
        assert!(
            render(
                "{native-session-id}/{backup-name}",
                native_session_id,
                b"backup-a",
                b"9",
            )
            .is_err(),
            "accepted {native_session_id:?}"
        );
    }
}

#[test]
fn the_rendered_length_is_bounded_before_it_is_allocated() {
    let exact = vec![b'a'; MAX_RENDERED_SCOPE_LOCATOR_BYTES];
    assert_eq!(
        render("{backup-name}", b"session-7", &exact, b"9")
            .expect("a locator at the bound renders")
            .as_os_str()
            .len(),
        MAX_RENDERED_SCOPE_LOCATOR_BYTES
    );
    let oversized = vec![b'a'; MAX_RENDERED_SCOPE_LOCATOR_BYTES + 1];
    assert!(render("{backup-name}", b"session-7", &oversized, b"9").is_err());
}

#[test]
fn a_placeholder_name_must_be_a_relation_id() {
    assert!(validate_relation_id("todo-snapshot-from-evidence").is_ok());
    assert!(validate_relation_id("native-session-id").is_ok());
    for invalid in [
        "",
        "Upper",
        " leading",
        "-leading",
        "with/slash",
        "with space",
    ] {
        assert!(
            validate_relation_id(invalid).is_err(),
            "accepted {invalid:?}"
        );
    }
    assert!(validate_relation_id(&"a".repeat(129)).is_err());
}

#[test]
fn a_template_bound_to_the_wrong_primitive_is_refused() {
    let mut declaration = artifact_declaration("file-history/{backup-name}");
    declaration.primitive = ScopeRelationPrimitive::KnownObject;
    assert!(
        validate_evidence_locator_template(&declaration).is_err(),
        "an artifact locator check must not accept another primitive's relation"
    );
    assert!(
        validate_bound_locator_template(&declaration, ScopeRelationPrimitive::KnownObject).is_ok()
    );
}
