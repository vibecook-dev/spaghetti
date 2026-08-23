//! Declared locator templates resolved into confined relative paths.
//!
//! RFC 012A section 7 lets a support release declare *where* a related object
//! lives as a template — `todos/{native-session-id}-agent-{native-actor-id}.json`
//! — with the placeholder names bound to that relation's declared identity
//! inputs. Turning such a template plus a set of evidence-derived values into a
//! path is the last step before the engine touches the filesystem, so it is
//! also the last place confinement can be enforced.
//!
//! **Confinement is enforced twice, and both halves are load-bearing.**
//!
//! 1. *At emission*, on the identity values themselves: an adapter that emits
//!    scope-join evidence runs each value through its own confinement rule
//!    (`claude/adapter.rs::is_confined_scope_component`), which rejects `.`,
//!    `..`, control bytes, `/`, `\`, and drive-letter prefixes. A value that
//!    never becomes a path separator cannot escape a directory later.
//! 2. *At resolution*, here, on the rendered path: even with clean inputs a
//!    template can be malformed, name a placeholder the relation never
//!    declared, or render to something absolute or empty. [`render_confined_locator`]
//!    re-checks the inputs and then checks the *result*, ending at
//!    [`confined_relative_path_key`](super::file::confined_relative_path_key).
//!
//! Checking only the inputs would trust the template; checking only the output
//! would trust values that a template could have concatenated into a separator.
//!
//! The rules below were extracted from `source/access.rs`, unchanged. Only
//! their reachability is new: they were `#[cfg(test)]` there because the only
//! caller was a test — no evaluator existed yet. `source/access.rs` keeps its
//! access-budget machinery, which the durable coordinator's
//! `ConfinedSourceAccess` uses to bound adapter dependency reads, and now
//! calls into this module instead of carrying its own copy.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use crate::adapter::{ScopeRelationDeclaration, ScopeRelationPrimitive};

/// Bound on a rendered locator, applied before any allocation so a hostile
/// template cannot make the engine reserve memory proportional to its input.
pub const MAX_RENDERED_SCOPE_LOCATOR_BYTES: usize = 4 * 1024;
const MAX_RELATION_ID_BYTES: usize = 128;

/// One evidence-derived value bound to a declared identity input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScopeIdentityInput<'a> {
    pub name: &'a str,
    pub value: &'a [u8],
}

/// A template, a placeholder, or a bound value that cannot produce a confined
/// path. Deliberately opaque: the reason names no native value, because the
/// values are the sensitive part.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("scope locator template or bound identity input is invalid")]
pub struct LocatorError;

/// A relation id, and therefore a placeholder name: `[a-z0-9][a-z0-9._-]{0,127}`.
pub fn validate_relation_id(value: &str) -> Result<(), LocatorError> {
    let valid = !value.is_empty()
        && value.len() <= MAX_RELATION_ID_BYTES
        && value.trim() == value
        && value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        });
    valid.then_some(()).ok_or(LocatorError)
}

/// Whether an artifact relation's locator template is well formed and names
/// only identity inputs the relation declared.
pub fn validate_evidence_locator_template(
    declaration: &ScopeRelationDeclaration,
) -> Result<(), LocatorError> {
    validate_bound_locator_template(
        declaration,
        ScopeRelationPrimitive::ArtifactLocatorFromEvidence,
    )
}

/// The same check for any relation primitive that resolves a bound template.
pub fn validate_bound_locator_template(
    declaration: &ScopeRelationDeclaration,
    expected_primitive: ScopeRelationPrimitive,
) -> Result<(), LocatorError> {
    if declaration.primitive != expected_primitive {
        return Err(LocatorError);
    }
    validate_locator_identity_placeholders(declaration)
}

/// Every placeholder must be declared, and a template with none is not a
/// locator: it would resolve to one fixed path regardless of evidence.
fn validate_locator_identity_placeholders(
    declaration: &ScopeRelationDeclaration,
) -> Result<(), LocatorError> {
    let placeholders = locator_placeholders(&declaration.locator)?;
    if placeholders.is_empty()
        || placeholders.iter().any(|(_, _, name)| {
            !declaration
                .identity_inputs
                .iter()
                .any(|declared| declared == name)
        })
    {
        return Err(LocatorError);
    }
    Ok(())
}

/// Render one template against the identity inputs it names.
///
/// A placeholder with no supplied value, or two values for one name, is an
/// error rather than a silent substitution. An input the template does not name
/// is *not* — a caller may hold one identity set and render several relations
/// from it, each naming a subset. Requiring the set to match a relation's
/// declaration exactly is [`validate_evidence_locator_template`]'s job, against
/// the declaration.
///
/// The result is a relative path with no empty, `.`, or `..` component, no
/// leading `/`, no backslash, and no drive-letter prefix.
pub fn render_confined_locator(
    template: &str,
    identity_inputs: &[ScopeIdentityInput<'_>],
) -> Result<PathBuf, LocatorError> {
    let placeholders = locator_placeholders(template)?;
    let values = identity_inputs
        .iter()
        .map(|input| {
            let value = std::str::from_utf8(input.value).map_err(|_| LocatorError)?;
            if value.is_empty()
                || value
                    .bytes()
                    .any(|byte| byte.is_ascii_control() || matches!(byte, b'/' | b'\\'))
            {
                return Err(LocatorError);
            }
            Ok((input.name, value))
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    if values.len() != identity_inputs.len() {
        return Err(LocatorError);
    }

    // Size the result before building it, so an oversized render is refused
    // rather than allocated.
    let mut output_len = template.len();
    for (start, end, name) in &placeholders {
        let value = values.get(name).ok_or(LocatorError)?;
        output_len = output_len
            .checked_sub(end - start)
            .and_then(|length| length.checked_add(value.len()))
            .ok_or(LocatorError)?;
    }
    if output_len == 0 || output_len > MAX_RENDERED_SCOPE_LOCATOR_BYTES {
        return Err(LocatorError);
    }
    let mut output = String::new();
    output
        .try_reserve_exact(output_len)
        .map_err(|_| LocatorError)?;
    let mut cursor = 0;
    for (start, end, name) in placeholders {
        output.push_str(&template[cursor..start]);
        output.push_str(values.get(name).ok_or(LocatorError)?);
        cursor = end;
    }
    output.push_str(&template[cursor..]);

    let first_component = output.split('/').next().unwrap_or_default().as_bytes();
    let has_windows_drive_prefix = first_component.len() >= 2
        && first_component[0].is_ascii_alphabetic()
        && first_component[1] == b':';
    if output.len() != output_len
        || output.starts_with('/')
        || has_windows_drive_prefix
        || output.contains('\\')
        || output
            .split('/')
            .any(|component| component.is_empty() || component == "." || component == "..")
    {
        return Err(LocatorError);
    }
    let path = PathBuf::from(output);
    super::file::confined_relative_path_key(&path).map_err(|_| LocatorError)?;
    Ok(path)
}

/// Placeholder spans and names, left to right. A nested or unbalanced brace, a
/// name that is not a relation id, or a repeated name is an error.
fn locator_placeholders(locator: &str) -> Result<Vec<(usize, usize, &str)>, LocatorError> {
    let bytes = locator.as_bytes();
    let mut placeholders = Vec::new();
    let mut names = BTreeSet::new();
    let mut cursor = 0;
    while cursor < bytes.len() {
        match bytes[cursor] {
            b'{' => {
                let end = bytes[cursor + 1..]
                    .iter()
                    .position(|byte| *byte == b'}')
                    .map(|offset| cursor + 1 + offset)
                    .ok_or(LocatorError)?;
                let name = &locator[cursor + 1..end];
                if name.as_bytes().contains(&b'{')
                    || validate_relation_id(name).is_err()
                    || !names.insert(name)
                {
                    return Err(LocatorError);
                }
                placeholders.push((cursor, end + 1, name));
                cursor = end + 1;
            }
            b'}' => return Err(LocatorError),
            _ => cursor += 1,
        }
    }
    Ok(placeholders)
}

#[cfg(test)]
mod tests;
