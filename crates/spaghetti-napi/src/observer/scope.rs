//! Which objects belong to one session scope.
//!
//! RFC 012D section 5 forbids enumerating a global agent root to attach one
//! scope. Every object here is reached through a declared relation: the root
//! transcript from the request locator, child transcripts and workflow objects
//! from the declared locator templates anchored at that root, and sidecars only
//! after adapter decode emits scope-join evidence naming them.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::adapter::{DecoderId, DriverSpec, RawRetentionPolicy, ScopeJoinUpdate, StreamSpec};

use super::request::ResolvedRequest;

/// Declared stream ids this observer follows. They match
/// `agent-support/claude-code/.../source-declarations.json`.
pub(crate) const ROOT_TRANSCRIPT_STREAM: &str = "session-transcripts";
pub(crate) const SUBAGENT_TRANSCRIPT_STREAM: &str = "subagent-transcripts";
pub(crate) const SUBAGENT_METADATA_STREAM: &str = "subagent-metadata";
pub(crate) const WORKFLOW_JOURNAL_STREAM: &str = "workflow-journals";
pub(crate) const WORKFLOW_RUN_STREAM: &str = "workflow-runs";
pub(crate) const TODO_STREAM: &str = "todo-snapshots";
pub(crate) const TEAM_CONFIG_STREAM: &str = "team-configs";
pub(crate) const TEAM_INBOX_STREAM: &str = "team-inboxes";

/// Declared relation ids, as the adapter emits them in scope-join evidence.
const TODO_RELATION: &str = "todo-snapshot-from-evidence";
const TEAM_CONFIG_RELATION: &str = "team-config-from-evidence";
const TEAM_INBOX_RELATION: &str = "team-inbox-from-evidence";

/// Declared bounds for the child-directory relation.
const MAX_DESCENDANT_DEPTH: usize = 5;
const MAX_DESCENDANT_OBJECTS: usize = 512;
/// Declared bound for evidence-derived sidecars, which fan out per actor.
const MAX_EVIDENCE_OBJECTS: usize = 512;

/// One object the observer follows, with the declared relation that admitted it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ScopeMemberKey {
    pub stream_id: String,
    pub root_name: String,
    pub relative_path: PathBuf,
}

#[derive(Debug, Clone)]
pub(crate) struct ScopeMember {
    pub key: ScopeMemberKey,
    pub relation_id: &'static str,
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
        relation_id: &'static str,
        relative_path: PathBuf,
    ) -> Option<ScopeMember> {
        let stream = self.streams.get(stream_id)?;
        Some(ScopeMember {
            key: ScopeMemberKey {
                stream_id: stream_id.to_string(),
                root_name: stream.selector.root_name.clone(),
                relative_path,
            },
            relation_id,
            driver: stream.driver.clone(),
            decoder: stream.decoder.clone(),
            retention: stream.retention,
        })
    }
}

/// Evidence-derived sidecar locators accumulated from adapter scope joins.
#[derive(Debug, Default, Clone)]
pub(crate) struct JoinedLocators {
    /// `todos/<session>-agent-<actor>.json`
    todo_actors: BTreeSet<String>,
    /// `<team>/config.json` and `<team>/inboxes/<member>.json`
    teams: BTreeSet<String>,
    team_inboxes: BTreeSet<(String, String)>,
}

impl JoinedLocators {
    /// Apply one decode's scope-join updates. Returns true when the scope grew.
    pub(crate) fn apply(&mut self, updates: &[ScopeJoinUpdate]) -> bool {
        let mut changed = false;
        for update in updates {
            for parameters in update.parameters() {
                let named = |name: &str| -> Option<String> {
                    parameters
                        .identity_inputs()
                        .iter()
                        .find(|input| input.name() == name)
                        .and_then(|input| std::str::from_utf8(input.value()).ok())
                        .map(str::to_string)
                };
                match update.relation_id() {
                    TODO_RELATION => {
                        if let Some(actor) = named("native-actor-id") {
                            changed |= self.todo_actors.len() < MAX_EVIDENCE_OBJECTS
                                && self.todo_actors.insert(actor);
                        }
                    }
                    TEAM_CONFIG_RELATION => {
                        if let Some(team) = named("observed-team-name") {
                            changed |=
                                self.teams.len() < MAX_EVIDENCE_OBJECTS && self.teams.insert(team);
                        }
                    }
                    TEAM_INBOX_RELATION => {
                        if let (Some(team), Some(member)) =
                            (named("observed-team-name"), named("observed-recipient"))
                        {
                            changed |= self.team_inboxes.len() < MAX_EVIDENCE_OBJECTS
                                && self.team_inboxes.insert((team, member));
                        }
                    }
                    // Child/workflow relations are anchored at the root locator
                    // and are already resolved by the directory walk below.
                    _ => {}
                }
            }
        }
        changed
    }
}

/// Resolve the current member set. Called at bootstrap and on every
/// reconciliation pass, because children appear and disappear while attached.
pub(crate) fn resolve_members(
    request: &ResolvedRequest,
    catalog: &StreamCatalog,
    joined: &JoinedLocators,
) -> Vec<ScopeMember> {
    let mut members = Vec::new();
    if let Some(member) = catalog.member(
        ROOT_TRANSCRIPT_STREAM,
        "root-transcript",
        request.root_transcript_relative(),
    ) {
        members.push(member);
    }
    if !request.include_descendants {
        return members;
    }

    let projects_root = request.agent_root.join("projects");
    let subtree = request.session_subtree_relative();

    collect_descendants(
        &projects_root,
        &subtree.join("subagents"),
        catalog,
        &mut members,
    );
    collect_workflow_runs(
        &projects_root,
        &subtree.join("workflows"),
        catalog,
        &mut members,
    );

    for actor in &joined.todo_actors {
        if let Some(member) = catalog.member(
            TODO_STREAM,
            TODO_RELATION,
            PathBuf::from("todos").join(format!(
                "{}-agent-{}.json",
                request.native_session_id, actor
            )),
        ) {
            members.push(member);
        }
    }
    for team in &joined.teams {
        if let Some(member) = catalog.member(
            TEAM_CONFIG_STREAM,
            TEAM_CONFIG_RELATION,
            PathBuf::from(team).join("config.json"),
        ) {
            members.push(member);
        }
    }
    for (team, recipient) in &joined.team_inboxes {
        if let Some(member) = catalog.member(
            TEAM_INBOX_STREAM,
            TEAM_INBOX_RELATION,
            PathBuf::from(team)
                .join("inboxes")
                .join(format!("{recipient}.json")),
        ) {
            members.push(member);
        }
    }
    members
}

/// Bounded walk of the declared `{project-key}/{native-session-id}/subagents`
/// directory. Depth and object count come from the declared relation bounds;
/// nothing outside this subtree is opened.
fn collect_descendants(
    projects_root: &Path,
    relative_dir: &Path,
    catalog: &StreamCatalog,
    members: &mut Vec<ScopeMember>,
) {
    let mut frontier = vec![(relative_dir.to_path_buf(), 0_usize)];
    let mut found = 0_usize;
    while let Some((relative, depth)) = frontier.pop() {
        if depth > MAX_DESCENDANT_DEPTH || found >= MAX_DESCENDANT_OBJECTS {
            return;
        }
        let Ok(entries) = std::fs::read_dir(projects_root.join(&relative)) else {
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
            let stream = if name.starts_with("agent-") && name.ends_with(".meta.json") {
                Some((SUBAGENT_METADATA_STREAM, "descendant-metadata"))
            } else if name.starts_with("agent-") && name.ends_with(".jsonl") {
                Some((SUBAGENT_TRANSCRIPT_STREAM, "descendant-transcripts"))
            } else if name == "journal.jsonl" {
                Some((WORKFLOW_JOURNAL_STREAM, "workflow-journals"))
            } else {
                None
            };
            let Some((stream_id, relation_id)) = stream else {
                continue;
            };
            if let Some(member) = catalog.member(stream_id, relation_id, child) {
                members.push(member);
                found += 1;
                if found >= MAX_DESCENDANT_OBJECTS {
                    return;
                }
            }
        }
    }
}

/// Declared `{project-key}/{native-session-id}/workflows/wf_*.json`.
fn collect_workflow_runs(
    projects_root: &Path,
    relative_dir: &Path,
    catalog: &StreamCatalog,
    members: &mut Vec<ScopeMember>,
) {
    let Ok(entries) = std::fs::read_dir(projects_root.join(relative_dir)) else {
        return;
    };
    let mut found = 0_usize;
    for entry in entries.flatten() {
        if found >= MAX_DESCENDANT_OBJECTS {
            return;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if !name.starts_with("wf_") || !name.ends_with(".json") {
            continue;
        }
        if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        if let Some(member) = catalog.member(
            WORKFLOW_RUN_STREAM,
            "workflow-records",
            relative_dir.join(name),
        ) {
            members.push(member);
            found += 1;
        }
    }
}

/// Directories the watcher anchors on. RFC 012D requires watches to exist
/// before the bootstrap scan, so these are computed from the request alone and
/// are stable for the lifetime of the attachment.
pub(crate) fn watch_anchors(request: &ResolvedRequest) -> Vec<PathBuf> {
    let projects_root = request.agent_root.join("projects");
    let mut anchors = vec![projects_root.join(&request.project_slug)];
    if request.include_descendants {
        let subtree = projects_root.join(request.session_subtree_relative());
        anchors.push(subtree);
        anchors.push(request.agent_root.join("todos"));
        anchors.push(request.agent_root.join("teams"));
    }
    anchors
}
