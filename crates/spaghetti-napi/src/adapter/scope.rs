//! Declarative RFC 012A session-scope programs.
//!
//! These types describe authorization inputs only. They do not open native
//! objects, construct source-runtime budgets, or deliver observations.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

pub const SCOPE_PROGRAM_SCHEMA_VERSION: u32 = 1;

const MAX_SCOPE_DOCUMENT_BYTES: usize = 1024 * 1024;
const MAX_IDENTIFIER_BYTES: usize = 128;
const MAX_LOCATOR_BYTES: usize = 4 * 1024;
const MAX_BLOCKER_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ScopeContractError {
    message: String,
}

impl ScopeContractError {
    fn invalid(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeProgramStatus {
    Incomplete,
    Candidate,
    Promoted,
    Retired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScopeRelationPrimitive {
    KnownObject,
    SiblingObject,
    ChildDirectoryByNativeId,
    ReferencedObjectFromField,
    BoundedIndexLookup,
    ParameterizedSQLiteRows,
    KeyNamespace,
    ArtifactLocatorFromEvidence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeUnavailableBehavior {
    RecordUnavailable,
    SkipOptional,
    FailScope,
}

/// Exact bounded source contract used by an evidence-derived artifact
/// relation. V1 intentionally permits only the common ReplaceDocument shape:
/// other primitives need their own generation and position law before they
/// can back an ordered artifact-availability occurrence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScopeRelationSourcePrimitive {
    ReplaceDocument,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopeRelationSourceBinding {
    pub stream_id: String,
    pub primitive: ScopeRelationSourcePrimitive,
    pub max_object_bytes: u64,
}

impl ScopeRelationSourceBinding {
    fn validate(&self, relation_bounds: ScopeRelationBounds) -> Result<(), ScopeContractError> {
        validate_identifier("scope relation source stream id", &self.stream_id)?;
        if self.max_object_bytes == 0 || self.max_object_bytes > relation_bounds.max_bytes {
            return Err(invalid(
                "scope relation source object bound must be positive and fit the relation byte budget",
            ));
        }
        Ok(())
    }
}

/// Exact source stream and selection authority for a scoped observation
/// relation. The source pattern must occur in the digest-bound source
/// declaration. Directory relations additionally carry a selector relative
/// to their already-confined rendered locator; neither value is caller input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopeObservationSourceBinding {
    pub stream_id: String,
    pub source_pattern: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relative_selector: Option<String>,
}

impl ScopeObservationSourceBinding {
    fn validate(
        &self,
        relation_primitive: ScopeRelationPrimitive,
        locator: &str,
        identity_inputs: &[String],
    ) -> Result<(), ScopeContractError> {
        validate_identifier("scope observation source stream id", &self.stream_id)?;
        validate_source_pattern("scope observation source pattern", &self.source_pattern)?;
        let locator_pattern = locator_template_pattern(locator, identity_inputs)?;
        match (relation_primitive, self.relative_selector.as_deref()) {
            (ScopeRelationPrimitive::ChildDirectoryByNativeId, Some(selector)) => {
                validate_source_pattern("scope observation relative selector", selector)?;
                if format!("{locator_pattern}/{selector}") != self.source_pattern {
                    return Err(invalid(
                        "child-directory selector does not compose to its declared source pattern",
                    ));
                }
            }
            (ScopeRelationPrimitive::ChildDirectoryByNativeId, None) => {
                return Err(invalid(
                    "child-directory observation binding requires a relative selector",
                ));
            }
            (ScopeRelationPrimitive::SiblingObject, None) => {}
            (ScopeRelationPrimitive::ReferencedObjectFromField, None) => {
                if locator_pattern != self.source_pattern {
                    return Err(invalid(
                        "referenced-object locator does not compose to its declared source pattern",
                    ));
                }
            }
            (
                ScopeRelationPrimitive::SiblingObject
                | ScopeRelationPrimitive::ReferencedObjectFromField,
                Some(_),
            ) => {
                return Err(invalid(
                    "exact-object observation binding cannot declare a relative selector",
                ));
            }
            _ => {
                return Err(invalid(
                    "observation source bindings are limited to child-directory, sibling, and referenced-object relations",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopeRelationBounds {
    pub max_fan_out: u64,
    pub max_depth: u32,
    pub max_objects: u64,
    pub max_bytes: u64,
    pub max_rows: u64,
}

impl ScopeRelationBounds {
    pub fn validate(&self) -> Result<(), ScopeContractError> {
        if self.max_fan_out == 0
            || self.max_depth == 0
            || self.max_objects == 0
            || self.max_bytes == 0
        {
            return Err(invalid(
                "scope relation fan-out, depth, object, and byte bounds must be greater than zero",
            ));
        }
        if self.max_fan_out > self.max_objects {
            return Err(invalid(
                "scope relation max_fan_out cannot exceed max_objects",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopeRelationDeclaration {
    pub relation_id: String,
    pub primitive: ScopeRelationPrimitive,
    pub access_root: String,
    pub locator: String,
    pub identity_inputs: Vec<String>,
    pub bounds: ScopeRelationBounds,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub statement_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameter_names: Option<Vec<String>>,
    /// Exact declared source stream whose canonical object coordinate backs
    /// this evidence-derived artifact read. Incomplete manifests may omit the
    /// binding while a conceptual relation remains unresolved; promoted
    /// manifests may not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_binding: Option<ScopeRelationSourceBinding>,
    /// Digest-bound stream and selector authority for a dynamic or related
    /// observation. This declaration alone cannot open or read a source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation_binding: Option<ScopeObservationSourceBinding>,
    pub unavailable_behavior: ScopeUnavailableBehavior,
    pub claim_refs: Vec<String>,
}

impl ScopeRelationDeclaration {
    fn validate(
        &self,
        roots: &BTreeSet<&str>,
        require_promoted_bindings: bool,
    ) -> Result<(), ScopeContractError> {
        validate_identifier("scope relation id", &self.relation_id)?;
        validate_identifier("scope relation access root", &self.access_root)?;
        if !roots.contains(self.access_root.as_str()) {
            return Err(invalid(format!(
                "scope relation {} names undeclared access root {}",
                self.relation_id, self.access_root
            )));
        }
        validate_locator(&self.locator)?;
        validate_identifier_list("scope relation identity input", &self.identity_inputs, true)?;
        validate_identifier_list("scope relation claim", &self.claim_refs, true)?;
        self.bounds.validate()?;

        match self.primitive {
            ScopeRelationPrimitive::ParameterizedSQLiteRows => {
                let statement_id = self.statement_id.as_deref().ok_or_else(|| {
                    invalid("parameterized SQLite relation requires statement_id")
                })?;
                validate_identifier("scope relation statement id", statement_id)?;
                let parameter_names = self.parameter_names.as_ref().ok_or_else(|| {
                    invalid("parameterized SQLite relation requires parameter_names")
                })?;
                validate_identifier_list("scope relation parameter", parameter_names, true)?;
                if self.bounds.max_rows == 0 {
                    return Err(invalid(
                        "parameterized SQLite relation requires a positive row bound",
                    ));
                }
            }
            _ if self.statement_id.is_some() || self.parameter_names.is_some() => {
                return Err(invalid(
                    "non-SQL scope relation cannot declare statement fields",
                ));
            }
            _ => {}
        }
        match (self.primitive, self.source_binding.as_ref()) {
            (ScopeRelationPrimitive::ArtifactLocatorFromEvidence, Some(binding)) => {
                binding.validate(self.bounds)?;
            }
            (ScopeRelationPrimitive::ArtifactLocatorFromEvidence, None)
                if require_promoted_bindings =>
            {
                return Err(invalid(
                    "promoted evidence-derived artifact relation requires a source binding",
                ));
            }
            (ScopeRelationPrimitive::ArtifactLocatorFromEvidence, None) => {}
            (_, Some(_)) => {
                return Err(invalid(
                    "only an evidence-derived artifact relation may declare a source binding",
                ));
            }
            (_, None) => {}
        }
        match self.observation_binding.as_ref() {
            Some(binding) => {
                binding.validate(self.primitive, &self.locator, &self.identity_inputs)?
            }
            None if require_promoted_bindings
                && matches!(
                    self.primitive,
                    ScopeRelationPrimitive::SiblingObject
                        | ScopeRelationPrimitive::ChildDirectoryByNativeId
                        | ScopeRelationPrimitive::ReferencedObjectFromField
                        | ScopeRelationPrimitive::BoundedIndexLookup
                        | ScopeRelationPrimitive::ParameterizedSQLiteRows
                        | ScopeRelationPrimitive::KeyNamespace
                ) =>
            {
                return Err(invalid(
                    "promoted dynamic observation relation requires an executable source binding",
                ));
            }
            None => {}
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopeProgramDeclaration {
    pub program_id: String,
    pub root_entity_kind: String,
    /// The declared relation whose exact native object represents the
    /// requested root entity. Candidate/incomplete manifests may omit this
    /// while the relation model is still under review; promoted manifests may
    /// not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_relation_id: Option<String>,
    pub relations: Vec<ScopeRelationDeclaration>,
    pub claim_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopeProgramManifest {
    pub schema_version: u32,
    pub declaration_id: String,
    pub adapter_id: String,
    pub ads_id: String,
    pub status: ScopeProgramStatus,
    pub roots: Vec<String>,
    pub programs: Vec<ScopeProgramDeclaration>,
    pub blockers: Vec<String>,
    pub claim_refs: Vec<String>,
}

impl ScopeProgramManifest {
    pub fn from_json(bytes: &[u8]) -> Result<Self, ScopeContractError> {
        if bytes.is_empty() || bytes.len() > MAX_SCOPE_DOCUMENT_BYTES {
            return Err(invalid(format!(
                "scope program document must contain between 1 and {MAX_SCOPE_DOCUMENT_BYTES} bytes"
            )));
        }
        let manifest = serde_json::from_slice::<Self>(bytes)
            .map_err(|error| invalid(format!("scope program JSON is invalid: {error}")))?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<(), ScopeContractError> {
        if self.schema_version != SCOPE_PROGRAM_SCHEMA_VERSION {
            return Err(invalid(format!(
                "unsupported scope-program schema version {}",
                self.schema_version
            )));
        }
        validate_identifier("scope declaration id", &self.declaration_id)?;
        validate_identifier("scope adapter id", &self.adapter_id)?;
        validate_identifier("scope ADS id", &self.ads_id)?;
        validate_identifier_list("scope root", &self.roots, false)?;
        validate_identifier_list("scope claim", &self.claim_refs, true)?;
        for blocker in &self.blockers {
            if blocker.trim().is_empty() || blocker.len() > MAX_BLOCKER_BYTES {
                return Err(invalid(format!(
                    "scope blocker must contain between 1 and {MAX_BLOCKER_BYTES} bytes"
                )));
            }
        }
        match self.status {
            ScopeProgramStatus::Incomplete if self.blockers.is_empty() => {
                return Err(invalid("incomplete scope program requires blockers"));
            }
            ScopeProgramStatus::Candidate | ScopeProgramStatus::Promoted
                if self.programs.is_empty() =>
            {
                return Err(invalid(
                    "candidate or promoted scope manifest requires a program",
                ));
            }
            _ => {}
        }

        let roots = self
            .roots
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let mut program_ids = BTreeSet::new();
        let mut relation_ids = BTreeSet::new();
        for program in &self.programs {
            validate_identifier("scope program id", &program.program_id)?;
            validate_identifier("scope root entity kind", &program.root_entity_kind)?;
            validate_identifier_list("scope program claim", &program.claim_refs, true)?;
            if !program_ids.insert(program.program_id.as_str()) {
                return Err(invalid(format!(
                    "duplicate scope program id {}",
                    program.program_id
                )));
            }
            if program.relations.is_empty() {
                return Err(invalid(format!(
                    "scope program {} has no relations",
                    program.program_id
                )));
            }
            for relation in &program.relations {
                relation.validate(&roots, self.status == ScopeProgramStatus::Promoted)?;
                if !relation_ids.insert(relation.relation_id.as_str()) {
                    return Err(invalid(format!(
                        "duplicate scope relation id {}",
                        relation.relation_id
                    )));
                }
            }
            match program.root_relation_id.as_deref() {
                Some(root_relation_id) => {
                    validate_identifier("scope root relation id", root_relation_id)?;
                    let root_relation = program
                        .relations
                        .iter()
                        .find(|relation| relation.relation_id == root_relation_id)
                        .ok_or_else(|| {
                            invalid(format!(
                                "scope program {} names undeclared root relation {}",
                                program.program_id, root_relation_id
                            ))
                        })?;
                    if root_relation.primitive != ScopeRelationPrimitive::KnownObject {
                        return Err(invalid(format!(
                            "scope program {} root relation {} must use KnownObject",
                            program.program_id, root_relation_id
                        )));
                    }
                }
                None if self.status == ScopeProgramStatus::Promoted => {
                    return Err(invalid(format!(
                        "promoted scope program {} requires a declared root relation",
                        program.program_id
                    )));
                }
                None => {}
            }
        }
        Ok(())
    }

    pub fn program(&self, program_id: &str) -> Option<&ScopeProgramDeclaration> {
        self.programs
            .iter()
            .find(|program| program.program_id == program_id)
    }

    pub fn relation(&self, relation_id: &str) -> Option<&ScopeRelationDeclaration> {
        self.programs
            .iter()
            .flat_map(|program| &program.relations)
            .find(|relation| relation.relation_id == relation_id)
    }

    pub fn relations(&self) -> impl Iterator<Item = &ScopeRelationDeclaration> {
        self.programs
            .iter()
            .flat_map(|program| program.relations.iter())
    }
}

fn validate_identifier(label: &str, value: &str) -> Result<(), ScopeContractError> {
    let valid = !value.is_empty()
        && value.len() <= MAX_IDENTIFIER_BYTES
        && value.trim() == value
        && value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        });
    if !valid {
        return Err(invalid(format!(
            "{label} must match [a-z0-9][a-z0-9._-]{{0,127}}"
        )));
    }
    Ok(())
}

fn validate_source_pattern(label: &str, value: &str) -> Result<(), ScopeContractError> {
    let first_component = value.split('/').next().unwrap_or_default().as_bytes();
    let has_windows_drive_prefix = first_component.len() >= 2
        && first_component[0].is_ascii_alphabetic()
        && first_component[1] == b':';
    let valid = !value.is_empty()
        && value.len() <= MAX_LOCATOR_BYTES
        && !value.starts_with('/')
        && !has_windows_drive_prefix
        && !value.contains('\\')
        && !value.contains("**/**")
        && !value.bytes().any(|byte| byte.is_ascii_control())
        && !value
            .bytes()
            .any(|byte| matches!(byte, b'?' | b'[' | b']' | b'{' | b'}'))
        && value.split('/').all(|component| {
            !component.is_empty()
                && component != "."
                && component != ".."
                && (!component.contains("**") || component == "**")
        });
    if !valid {
        return Err(invalid(format!(
            "{label} must be a bounded canonical confined star-only pattern"
        )));
    }
    Ok(())
}

fn locator_template_pattern(
    locator: &str,
    identity_inputs: &[String],
) -> Result<String, ScopeContractError> {
    let bytes = locator.as_bytes();
    let mut output = String::with_capacity(locator.len());
    let mut cursor = 0;
    let mut literal_start = 0;
    let mut placeholders = BTreeSet::new();
    while cursor < bytes.len() {
        match bytes[cursor] {
            b'{' => {
                output.push_str(&locator[literal_start..cursor]);
                let end = bytes[cursor + 1..]
                    .iter()
                    .position(|byte| *byte == b'}')
                    .map(|offset| cursor + 1 + offset)
                    .ok_or_else(|| invalid("scope observation locator template is invalid"))?;
                let name = &locator[cursor + 1..end];
                if name.as_bytes().contains(&b'{')
                    || validate_identifier("scope observation locator placeholder", name).is_err()
                    || !identity_inputs.iter().any(|input| input == name)
                    || !placeholders.insert(name)
                {
                    return Err(invalid("scope observation locator template is invalid"));
                }
                output.push('*');
                cursor = end + 1;
                literal_start = cursor;
            }
            b'}' => return Err(invalid("scope observation locator template is invalid")),
            _ => cursor += 1,
        }
    }
    if placeholders.is_empty() {
        return Err(invalid("scope observation locator template is invalid"));
    }
    output.push_str(&locator[literal_start..]);
    validate_source_pattern("scope observation locator pattern", &output)?;
    Ok(output)
}

fn validate_identifier_list(
    label: &str,
    values: &[String],
    require_nonempty: bool,
) -> Result<(), ScopeContractError> {
    if require_nonempty && values.is_empty() {
        return Err(invalid(format!("{label} list must not be empty")));
    }
    let mut seen = BTreeSet::new();
    for value in values {
        validate_identifier(label, value)?;
        if !seen.insert(value.as_str()) {
            return Err(invalid(format!(
                "{label} list contains duplicate value {value}"
            )));
        }
    }
    Ok(())
}

fn validate_locator(locator: &str) -> Result<(), ScopeContractError> {
    let first_component = locator.split('/').next().unwrap_or_default().as_bytes();
    let has_windows_drive_prefix = first_component.len() >= 2
        && first_component[0].is_ascii_alphabetic()
        && first_component[1] == b':';
    if locator.is_empty()
        || locator.len() > MAX_LOCATOR_BYTES
        || locator.starts_with('/')
        || has_windows_drive_prefix
        || locator.contains('\\')
        || locator
            .bytes()
            .any(|byte| byte.is_ascii_control() || matches!(byte, b'*' | b'?' | b'[' | b']'))
        || locator
            .split('/')
            .any(|component| component.is_empty() || component == "." || component == "..")
    {
        return Err(invalid(
            "scope locator must be a bounded canonical relative locator without globs or traversal",
        ));
    }
    Ok(())
}

fn invalid(message: impl Into<String>) -> ScopeContractError {
    ScopeContractError::invalid(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_candidate_scope_manifests_are_strictly_parseable() {
        for bytes in [
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../agent-support/claude-code/candidate-2026-08-15/scope-programs.json"
            ))
            .as_slice(),
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../agent-support/codex/candidate-2026-08-15/scope-programs.json"
            ))
            .as_slice(),
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../agent-support/grok/candidate-2026-08-15/scope-programs.json"
            ))
            .as_slice(),
        ] {
            let manifest = ScopeProgramManifest::from_json(bytes).unwrap();
            assert_eq!(manifest.status, ScopeProgramStatus::Incomplete);
            assert!(!manifest.programs.is_empty());
        }
    }

    #[test]
    fn promoted_scope_program_requires_a_known_object_root_relation() {
        let bytes = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../agent-support/grok/candidate-2026-08-15/scope-programs.json"
        ));
        let mut manifest = ScopeProgramManifest::from_json(bytes).unwrap();
        manifest.status = ScopeProgramStatus::Promoted;
        manifest.blockers.clear();
        manifest.programs[0].relations.truncate(1);

        assert!(manifest
            .validate()
            .unwrap_err()
            .to_string()
            .contains("declared root relation"));

        manifest.programs[0].root_relation_id = Some("root-history".to_string());
        manifest.programs[0].relations[0].primitive = ScopeRelationPrimitive::SiblingObject;
        manifest.programs[0].relations[0].locator = "history/{native-session-id}.jsonl".to_string();
        manifest.programs[0].relations[0].observation_binding =
            Some(ScopeObservationSourceBinding {
                stream_id: "history-documents".to_string(),
                source_pattern: "**/history.jsonl".to_string(),
                relative_selector: None,
            });
        assert!(manifest
            .validate()
            .unwrap_err()
            .to_string()
            .contains("must use KnownObject"));

        manifest.programs[0].relations[0].primitive = ScopeRelationPrimitive::KnownObject;
        manifest.programs[0].relations[0].observation_binding = None;
        manifest.validate().unwrap();
    }

    #[test]
    fn scope_validation_rejects_duplicate_relations_and_sql_without_rows() {
        let bytes = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../agent-support/grok/candidate-2026-08-15/scope-programs.json"
        ));
        let mut manifest = ScopeProgramManifest::from_json(bytes).unwrap();
        let duplicate = manifest.programs[0].relations[0].clone();
        manifest.programs[0].relations.push(duplicate);
        assert!(manifest
            .validate()
            .unwrap_err()
            .to_string()
            .contains("duplicate"));

        let relation = &mut manifest.programs[0].relations[0];
        relation.relation_id = "query".to_string();
        relation.primitive = ScopeRelationPrimitive::ParameterizedSQLiteRows;
        relation.statement_id = Some("lookup".to_string());
        relation.parameter_names = Some(vec!["session-id".to_string()]);
        relation.bounds.max_rows = 0;
        manifest.programs[0].relations.truncate(1);
        assert!(manifest
            .validate()
            .unwrap_err()
            .to_string()
            .contains("row bound"));
    }

    #[test]
    fn scope_validation_rejects_path_escape_and_platform_absolute_locators() {
        let bytes = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../agent-support/grok/candidate-2026-08-15/scope-programs.json"
        ));
        for locator in ["../summary.json", "/summary.json", "C:/summary.json"] {
            let mut manifest = ScopeProgramManifest::from_json(bytes).unwrap();
            manifest.programs[0].relations[0].locator = locator.to_string();
            assert!(manifest.validate().is_err(), "accepted {locator:?}");
        }
    }

    #[test]
    fn promoted_artifact_relations_require_an_exact_bounded_source_binding() {
        let bytes = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../agent-support/grok/candidate-2026-08-15/scope-programs.json"
        ));
        let mut manifest = ScopeProgramManifest::from_json(bytes).unwrap();
        manifest.status = ScopeProgramStatus::Promoted;
        manifest.blockers.clear();
        manifest.programs[0].root_relation_id = Some("root-history".to_string());
        let relation = &mut manifest.programs[0].relations[1];
        relation.primitive = ScopeRelationPrimitive::ArtifactLocatorFromEvidence;
        assert!(manifest
            .validate()
            .unwrap_err()
            .to_string()
            .contains("requires a source binding"));

        let relation = &mut manifest.programs[0].relations[1];
        relation.source_binding = Some(ScopeRelationSourceBinding {
            stream_id: "summary-documents".to_string(),
            primitive: ScopeRelationSourcePrimitive::ReplaceDocument,
            max_object_bytes: relation.bounds.max_bytes,
        });
        manifest.programs[0].relations.truncate(2);
        manifest.validate().unwrap();

        manifest.programs[0].relations[1].source_binding = None;
        manifest.programs[0].relations[1].primitive = ScopeRelationPrimitive::SiblingObject;
        manifest.programs[0].relations[0].source_binding = Some(ScopeRelationSourceBinding {
            stream_id: "events-documents".to_string(),
            primitive: ScopeRelationSourcePrimitive::ReplaceDocument,
            max_object_bytes: 1,
        });
        assert!(manifest
            .validate()
            .unwrap_err()
            .to_string()
            .contains("only an evidence-derived artifact relation"));
    }

    #[test]
    fn promoted_dynamic_relations_require_primitive_appropriate_source_bindings() {
        let bytes = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../agent-support/grok/candidate-2026-08-15/scope-programs.json"
        ));
        let mut manifest = ScopeProgramManifest::from_json(bytes).unwrap();
        manifest.status = ScopeProgramStatus::Promoted;
        manifest.blockers.clear();
        manifest.programs[0].root_relation_id = Some("root-history".to_string());
        manifest.programs[0].relations.truncate(2);

        assert!(manifest
            .validate()
            .unwrap_err()
            .to_string()
            .contains("requires an executable source binding"));

        manifest.programs[0].relations[1].observation_binding =
            Some(ScopeObservationSourceBinding {
                stream_id: "summary-documents".to_string(),
                source_pattern: "**/summary.json".to_string(),
                relative_selector: None,
            });
        manifest.programs[0].relations[1].locator = "{history-object}/summary.json".to_string();
        manifest.validate().unwrap();

        manifest.programs[0].relations[1].primitive =
            ScopeRelationPrimitive::ChildDirectoryByNativeId;
        assert!(manifest
            .validate()
            .unwrap_err()
            .to_string()
            .contains("requires a relative selector"));
        manifest.programs[0].relations[1]
            .observation_binding
            .as_mut()
            .unwrap()
            .relative_selector = Some("**/entry-*.json".to_string());
        manifest.programs[0].relations[1]
            .observation_binding
            .as_mut()
            .unwrap()
            .source_pattern = "*/summary.json/**/entry-*.json".to_string();
        manifest.validate().unwrap();

        manifest.programs[0].relations[1].primitive =
            ScopeRelationPrimitive::ReferencedObjectFromField;
        let binding = manifest.programs[0].relations[1]
            .observation_binding
            .as_mut()
            .unwrap();
        binding.relative_selector = None;
        binding.source_pattern = "**/summary.json".to_string();
        assert!(manifest
            .validate()
            .unwrap_err()
            .to_string()
            .contains("referenced-object locator"));
        manifest.programs[0].relations[1]
            .observation_binding
            .as_mut()
            .unwrap()
            .source_pattern = "*/summary.json".to_string();
        manifest.validate().unwrap();

        manifest.programs[0].relations[1]
            .observation_binding
            .as_mut()
            .unwrap()
            .relative_selector = Some("../escape".to_string());
        assert!(manifest.validate().is_err());
    }
}
