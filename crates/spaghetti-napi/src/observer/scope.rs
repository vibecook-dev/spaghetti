//! Which objects belong to one session scope.
//!
//! RFC 012D section 5 forbids enumerating a global agent root to attach one
//! scope, and section 9 makes the adapter's scope program the authority on
//! where a scope may reach. This module evaluates that program rather than
//! restating it: every path comes from a declared relation's locator template,
//! rendered against identity inputs through the common confinement law in
//! `source::access`, and bounded by the relation's own declared bounds.
//!
//! Nothing here names a vendor or a path shape. Relations, locators, stream
//! bindings, selectors, and bounds all arrive from the manifest.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::adapter::{
    DecoderId, DriverSpec, RawRetentionPolicy, ScopeJoinUpdate, ScopeProgramDeclaration,
    ScopeRelationDeclaration, ScopeRelationPrimitive, StreamSpec,
};
use crate::source::{
    confined_relative_path_key, render_confined_locator, validate_bound_locator_template,
    GlobPattern, ScopeIdentityInput,
};

use super::request::ResolvedRequest;
use super::ObserverError;

/// Identity inputs bindable from the request alone. Every other input has to be
/// named by decoded evidence before its relation can be evaluated.
const INPUT_PROJECT_KEY: &str = "project-key";
const INPUT_NATIVE_SESSION_ID: &str = "native-session-id";
/// Ceilings the observer will not exceed even if a declaration asks for more.
const HARD_MAX_OBJECTS: usize = 4_096;
const HARD_MAX_DEPTH: usize = 16;
/// Distinct evidence bindings retained per relation, so a decoder cannot grow
/// the scope without limit.
const MAX_BINDINGS_PER_RELATION: usize = 512;

/// One object the observer follows.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ScopeMemberKey {
    pub stream_id: String,
    pub root_name: String,
    pub relative_path: PathBuf,
}

#[derive(Debug, Clone)]
pub(crate) struct ScopeMember {
    pub key: ScopeMemberKey,
    /// The declared relation that admitted this object. RFC 012D section 5
    /// requires every opened object to be attributable to exactly one.
    pub relation_id: String,
    pub driver: DriverSpec,
    pub decoder: DecoderId,
    pub retention: RawRetentionPolicy,
}

/// The adapter's declared streams, indexed for lookup.
pub(crate) struct StreamCatalog {
    streams: BTreeMap<String, StreamSpec>,
}

impl StreamCatalog {
    pub(crate) fn new(streams: Vec<StreamSpec>) -> Self {
        Self {
            streams: streams
                .into_iter()
                .map(|stream| (stream.id.as_str().to_string(), stream))
                .collect(),
        }
    }

    fn member(
        &self,
        stream_id: &str,
        relation_id: &str,
        relative_path: PathBuf,
    ) -> Option<ScopeMember> {
        let stream = self.streams.get(stream_id)?;
        Some(ScopeMember {
            key: ScopeMemberKey {
                stream_id: stream_id.to_string(),
                root_name: stream.selector.root_name.clone(),
                relative_path,
            },
            relation_id: relation_id.to_string(),
            driver: stream.driver.clone(),
            decoder: stream.decoder.clone(),
            retention: stream.retention,
        })
    }

    /// The declared stream whose selector claims this exact object, used for
    /// the root relation — which names an object rather than a stream.
    fn stream_claiming(&self, root_name: &str, relative: &Path) -> Option<&str> {
        self.streams.values().find_map(|stream| {
            if stream.selector.root_name != root_name {
                return None;
            }
            let matches = |patterns: &[String]| {
                patterns
                    .iter()
                    .filter_map(|pattern| GlobPattern::new(pattern).ok())
                    .any(|pattern| pattern.matches_path(relative))
            };
            (matches(&stream.selector.include) && !matches(&stream.selector.exclude))
                .then(|| stream.id.as_str())
        })
    }
}

/// Identity values decoded evidence has named, keyed by relation.
///
/// A relation is evaluated only once every one of its declared identity inputs
/// has a value, which is what keeps an evidence-derived relation from being
/// followed before anything pointed at it.
#[derive(Debug, Default, Clone)]
pub(crate) struct JoinedLocators {
    bindings: BTreeMap<String, BTreeSet<Vec<(String, String)>>>,
}

impl JoinedLocators {
    /// Apply one decode's scope-join updates. Returns true when the scope grew.
    pub(crate) fn apply(&mut self, updates: &[ScopeJoinUpdate]) -> bool {
        let mut changed = false;
        for update in updates {
            for parameters in update.parameters() {
                let mut binding: Vec<(String, String)> = Vec::new();
                let mut usable = true;
                for input in parameters.identity_inputs() {
                    match std::str::from_utf8(input.value()) {
                        Ok(value) => binding.push((input.name().to_string(), value.to_string())),
                        // A non-text value cannot be rendered into a locator;
                        // the confinement law would reject it anyway.
                        Err(_) => usable = false,
                    }
                }
                if !usable || binding.is_empty() {
                    continue;
                }
                binding.sort();
                let slot = self
                    .bindings
                    .entry(update.relation_id().to_string())
                    .or_default();
                if slot.len() < MAX_BINDINGS_PER_RELATION {
                    changed |= slot.insert(binding);
                }
            }
        }
        changed
    }

    fn for_relation(&self, relation_id: &str) -> impl Iterator<Item = &Vec<(String, String)>> {
        self.bindings.get(relation_id).into_iter().flatten()
    }
}

/// The declared program this observer evaluates.
pub(crate) struct ScopeProgram {
    relations: Vec<ScopeRelationDeclaration>,
    root_relation_id: String,
}

impl ScopeProgram {
    /// Adopt one declared session-rooted program.
    pub(crate) fn select(program: &ScopeProgramDeclaration) -> Result<Self, ObserverError> {
        let root_relation_id = program.root_relation_id.clone().ok_or_else(|| {
            ObserverError::Unsupported(
                "scope program declares no root relation, so a session root cannot be bound"
                    .to_string(),
            )
        })?;
        if !program
            .relations
            .iter()
            .any(|relation| relation.relation_id == root_relation_id)
        {
            return Err(ObserverError::Unsupported(
                "scope program names a root relation it does not declare".to_string(),
            ));
        }
        Ok(Self {
            relations: program.relations.clone(),
            root_relation_id,
        })
    }

    /// Directories the observer may watch, derived from the declared relations
    /// so a relation the adapter does not declare is never watched either.
    pub(crate) fn watch_anchors(&self, request: &ResolvedRequest) -> Vec<PathBuf> {
        let mut anchors = BTreeSet::new();
        let fixed = request_bindings(request);
        for relation in &self.relations {
            let root = request.agent_root.join(&relation.access_root);
            if relation.relation_id == self.root_relation_id {
                if let Some(parent) = request.root_transcript_relative().parent() {
                    anchors.insert(root.join(parent));
                }
                continue;
            }
            if !request.include_descendants {
                continue;
            }
            match relation.primitive {
                // A directory relation bindable from the request alone is
                // watched precisely; an evidence-bound one is watched through
                // its access root, since its locator is not known yet.
                ScopeRelationPrimitive::ChildDirectoryByNativeId => {
                    match self.render(relation, &fixed) {
                        Some(relative) => anchors.insert(root.join(relative)),
                        None => anchors.insert(root),
                    };
                }
                ScopeRelationPrimitive::ReferencedObjectFromField
                | ScopeRelationPrimitive::SiblingObject => {
                    anchors.insert(root);
                }
                _ => {}
            }
        }
        anchors.into_iter().collect()
    }

    /// Render one relation's locator, or `None` when a declared identity input
    /// has no value yet or the result would leave the access root.
    fn render(
        &self,
        relation: &ScopeRelationDeclaration,
        bindings: &BTreeMap<String, String>,
    ) -> Option<PathBuf> {
        let values: Vec<(&str, &str)> = relation
            .identity_inputs
            .iter()
            .filter_map(|name| {
                bindings
                    .get(name)
                    .map(|value| (name.as_str(), value.as_str()))
            })
            .collect();
        if values.len() != relation.identity_inputs.len() {
            return None;
        }
        let inputs: Vec<ScopeIdentityInput<'_>> = values
            .iter()
            .map(|(name, value)| ScopeIdentityInput {
                name,
                value: value.as_bytes(),
            })
            .collect();
        // The confinement law lives in `source::locator` and is applied in two
        // steps: the template must be the primitive this evaluator thinks it is
        // and name only declared identity inputs, then the render refuses values
        // carrying separators or control bytes and any result that is absolute
        // or contains `.`, `..`, or empty components.
        validate_bound_locator_template(relation, relation.primitive).ok()?;
        render_confined_locator(&relation.locator, &inputs).ok()
    }
}

fn request_bindings(request: &ResolvedRequest) -> BTreeMap<String, String> {
    BTreeMap::from([
        (INPUT_PROJECT_KEY.to_string(), request.project_slug.clone()),
        (
            INPUT_NATIVE_SESSION_ID.to_string(),
            request.native_session_id.clone(),
        ),
    ])
}

/// Resolve the current member set by evaluating every declared relation.
pub(crate) fn resolve_members(
    request: &ResolvedRequest,
    program: &ScopeProgram,
    catalog: &StreamCatalog,
    joined: &JoinedLocators,
) -> Vec<ScopeMember> {
    let mut members = Vec::new();
    let fixed = request_bindings(request);

    for relation in &program.relations {
        // The root is bound by the request locator rather than by rendering:
        // the caller named it, and the request already proved it lives under
        // the declared access root.
        if relation.relation_id == program.root_relation_id {
            let relative = request.root_transcript_relative();
            if let Some(stream_id) = catalog.stream_claiming(&relation.access_root, &relative) {
                if let Some(member) = catalog.member(stream_id, &relation.relation_id, relative) {
                    members.push(member);
                }
            }
            continue;
        }
        if !request.include_descendants {
            continue;
        }
        // A relation with no observation binding names no stream to read.
        let Some(binding) = relation.observation_binding.as_ref() else {
            continue;
        };
        let root = request.agent_root.join(&relation.access_root);

        // Every identity binding this relation can take: the request's own,
        // plus each distinct set evidence has named.
        let mut candidates = vec![fixed.clone()];
        for evidence in joined.for_relation(&relation.relation_id) {
            let mut bound = fixed.clone();
            for (name, value) in evidence {
                bound.insert(name.clone(), value.clone());
            }
            candidates.push(bound);
        }

        for bound in candidates {
            let Some(relative) = program.render(relation, &bound) else {
                continue;
            };
            match relation.primitive {
                ScopeRelationPrimitive::ChildDirectoryByNativeId => collect_directory(
                    &root,
                    &relative,
                    relation,
                    binding.relative_selector.as_deref(),
                    catalog,
                    &binding.stream_id,
                    &mut members,
                ),
                ScopeRelationPrimitive::ReferencedObjectFromField => {
                    if let Some(member) =
                        catalog.member(&binding.stream_id, &relation.relation_id, relative)
                    {
                        members.push(member);
                    }
                }
                // Sibling relations bind to objects another relation resolved,
                // so they run after this loop. Primitives outside the v1 scoped
                // surface stay unevaluated rather than guessed at.
                _ => {}
            }
        }
    }

    if request.include_descendants {
        collect_siblings(request, program, catalog, &mut members);
    }

    members.sort_by(|left, right| left.key.cmp(&right.key));
    members.dedup_by(|left, right| left.key == right.key);
    members
}

/// The literal suffix a `SiblingObject` relation appends to the object it is a
/// sibling of.
///
/// This primitive is not rendered like the others. Its identity input *is* a
/// path — `{actor-transcript-object}.meta.json` — and the confinement law
/// rightly refuses a separator inside an identity value, because for every
/// other primitive that would be a traversal. So the declaration is read for
/// what it says: one placeholder naming the object, followed by a literal
/// suffix. The base path is one the observer already resolved and confined,
/// and the joined result is confined again below.
fn sibling_suffix(relation: &ScopeRelationDeclaration) -> Option<&str> {
    let placeholder = relation.identity_inputs.first()?;
    if relation.identity_inputs.len() != 1 {
        return None;
    }
    let opening = format!("{{{placeholder}}}");
    let suffix = relation.locator.strip_prefix(&opening)?;
    // A literal remainder only: anything else is a shape this evaluator does
    // not claim to understand.
    (!suffix.is_empty() && !suffix.contains(['{', '}', '/', '\\'])).then_some(suffix)
}

/// Attach sibling relations to the objects the main pass resolved.
fn collect_siblings(
    request: &ResolvedRequest,
    program: &ScopeProgram,
    catalog: &StreamCatalog,
    members: &mut Vec<ScopeMember>,
) {
    for relation in &program.relations {
        if relation.primitive != ScopeRelationPrimitive::SiblingObject {
            continue;
        }
        let (Some(binding), Some(suffix)) = (
            relation.observation_binding.as_ref(),
            sibling_suffix(relation),
        ) else {
            continue;
        };
        let root = request.agent_root.join(&relation.access_root);
        let siblings: Vec<ScopeMember> = members
            .iter()
            .filter(|member| member.key.root_name == relation.access_root)
            .filter_map(|member| {
                let name = member.key.relative_path.file_name()?.to_str()?;
                let relative = member
                    .key
                    .relative_path
                    .with_file_name(format!("{name}{suffix}"));
                // Confine the joined path the same way every other locator is.
                confined_relative_path_key(&relative).ok()?;
                // A sibling is followed only where one exists: the declaration
                // says where it would be, not that there is one.
                root.join(&relative)
                    .is_file()
                    .then(|| catalog.member(&binding.stream_id, &relation.relation_id, relative))
                    .flatten()
            })
            .collect();
        members.extend(siblings);
    }
}

/// Bounded walk of a declared child directory, admitting the objects its
/// selector matches. Depth and count come from the relation's declared bounds,
/// clamped to the observer's own ceilings.
fn collect_directory(
    root: &Path,
    relative_dir: &Path,
    relation: &ScopeRelationDeclaration,
    selector: Option<&str>,
    catalog: &StreamCatalog,
    stream_id: &str,
    members: &mut Vec<ScopeMember>,
) {
    let Some(pattern) = selector.and_then(|selector| GlobPattern::new(selector).ok()) else {
        return;
    };
    let max_objects = usize::try_from(relation.bounds.max_objects)
        .unwrap_or(HARD_MAX_OBJECTS)
        .min(HARD_MAX_OBJECTS);
    let max_depth = usize::try_from(relation.bounds.max_depth)
        .unwrap_or(HARD_MAX_DEPTH)
        .min(HARD_MAX_DEPTH);

    let mut frontier = vec![(relative_dir.to_path_buf(), 0_usize)];
    let mut found = 0_usize;
    while let Some((relative, depth)) = frontier.pop() {
        if depth > max_depth || found >= max_objects {
            return;
        }
        let Ok(entries) = std::fs::read_dir(root.join(&relative)) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            let child = relative.join(&name);
            if file_type.is_dir() {
                frontier.push((child, depth + 1));
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            // The declared selector is relative to the declared directory.
            let Ok(within) = child.strip_prefix(relative_dir) else {
                continue;
            };
            if !pattern.matches_path(within) {
                continue;
            }
            if let Some(member) = catalog.member(stream_id, &relation.relation_id, child) {
                members.push(member);
                found += 1;
                if found >= max_objects {
                    return;
                }
            }
        }
    }
}
