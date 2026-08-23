//! Real temp directories, real JSONL, real decoding.
//!
//! Every record here is synthetic: fixed fixture ids and empty content. The
//! shapes match `agent-support/claude-code/.../fixtures`, which is what makes
//! the Claude decoder accept them.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tempfile::TempDir;

use crate::claude::ClaudeCodeAdapter;
use crate::observer::{ObserveSessionRequest, ObserverEvent, ObserverHandle};

pub(crate) const SESSION: &str = "01234567-89ab-cdef-0123-456789abcdef";
pub(crate) const PROJECT: &str = "-fixture-project";

/// A `.claude`-shaped tree with one session in it.
pub(crate) struct SessionFixture {
    pub root: TempDir,
}

impl SessionFixture {
    pub(crate) fn new() -> Self {
        let root = TempDir::new().expect("temp dir");
        std::fs::create_dir_all(root.path().join("projects").join(PROJECT)).expect("project dir");
        Self { root }
    }

    /// A fixture whose project directory does not exist yet, for the
    /// attach-before-create case.
    pub(crate) fn empty() -> Self {
        let root = TempDir::new().expect("temp dir");
        std::fs::create_dir_all(root.path().join("projects")).expect("projects dir");
        Self { root }
    }

    pub(crate) fn transcript(&self) -> PathBuf {
        self.root
            .path()
            .join("projects")
            .join(PROJECT)
            .join(format!("{SESSION}.jsonl"))
    }

    pub(crate) fn subagent(&self, agent_id: &str) -> PathBuf {
        self.root
            .path()
            .join("projects")
            .join(PROJECT)
            .join(SESSION)
            .join("subagents")
            .join(format!("agent-{agent_id}.jsonl"))
    }

    pub(crate) fn project(&self) -> &'static str {
        PROJECT
    }

    /// The source instance the adapter discovers for this fixture root.
    pub(crate) fn source_instance(
        &self,
        adapter: &ClaudeCodeAdapter,
    ) -> crate::adapter::SourceInstance {
        use crate::adapter::{AgentAdapter, DiscoveryContext, SourceInstance};
        let spec = adapter
            .discover(&DiscoveryContext {
                configured_roots: vec![self.root.path().to_path_buf()],
                observed_at: 0,
            })
            .expect("discovery")
            .into_iter()
            .next()
            .expect("one instance");
        SourceInstance { id: 1, spec }
    }

    fn session_dir(&self) -> PathBuf {
        self.root
            .path()
            .join("projects")
            .join(PROJECT)
            .join(SESSION)
    }

    pub(crate) fn workflow_child(&self, workflow: &str, agent_id: &str) -> PathBuf {
        self.session_dir()
            .join("subagents")
            .join("workflows")
            .join(workflow)
            .join(format!("agent-{agent_id}.jsonl"))
    }

    pub(crate) fn workflow_journal(&self, workflow: &str) -> PathBuf {
        self.session_dir()
            .join("subagents")
            .join("workflows")
            .join(workflow)
            .join("journal.jsonl")
    }

    pub(crate) fn write_workflow_run(&self, workflow: &str) {
        let path = self
            .session_dir()
            .join("workflows")
            .join(format!("wf_{workflow}.json"));
        self.append_once(
            &path,
            &[format!(r#"{{"id":"{workflow}","status":"running"}}"#)],
        );
    }

    /// The sibling metadata object the declared sibling relation names: the
    /// transcript path with the declared suffix appended.
    pub(crate) fn write_subagent_metadata(&self, agent_id: &str) {
        let transcript = self.subagent(agent_id);
        let path = transcript.with_file_name(format!(
            "{}.meta.json",
            transcript
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
        ));
        self.append_once(&path, &[format!(r#"{{"agentId":"{agent_id}"}}"#)]);
    }

    pub(crate) fn write_todo(&self, actor_id: &str) {
        self.append_once(
            &self.todo_sidecar(actor_id),
            &[r#"[{"content":"f","status":"pending","activeForm":"f"}]"#.to_string()],
        );
    }

    pub(crate) fn write_team(&self, team: &str, member: &str) {
        let teams = self.root.path().join("teams").join(team);
        self.append_once(
            &teams.join("config.json"),
            &[format!(r#"{{"name":"{team}","leadAgentId":"{member}"}}"#)],
        );
        self.append_once(
            &teams.join("inboxes").join(format!("{member}.json")),
            &[r#"{"messages":[]}"#.to_string()],
        );
    }

    pub(crate) fn write_plan(&self, slug: &str) {
        self.append_once(
            &self.root.path().join("plans").join(format!("{slug}.md")),
            &["# plan".to_string()],
        );
    }

    pub(crate) fn todo_sidecar(&self, actor_id: &str) -> PathBuf {
        self.root
            .path()
            .join("todos")
            .join(format!("{SESSION}-agent-{actor_id}.json"))
    }

    pub(crate) fn append(&self, path: &Path, lines: &[String]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("parent dir");
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .expect("open for append");
        for line in lines {
            file.write_all(line.as_bytes()).expect("write record");
            file.write_all(b"\n").expect("write delimiter");
        }
        file.flush().expect("flush");
    }

    /// Append every line in a single write, so a concurrently reading observer
    /// sees either none of them or all of them. Tests that assert on a snapshot
    /// at an exact watermark need that; a per-line append does not give it.
    pub(crate) fn append_once(&self, path: &Path, lines: &[String]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("parent dir");
        }
        let body = lines
            .iter()
            .map(|line| format!("{line}\n"))
            .collect::<String>();
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .expect("open for append");
        file.write_all(body.as_bytes()).expect("write batch");
        file.flush().expect("flush");
    }

    /// Append bytes with no trailing delimiter, leaving a partial record.
    pub(crate) fn append_partial(&self, path: &Path, partial: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("parent dir");
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .expect("open for append");
        file.write_all(partial.as_bytes()).expect("write partial");
        file.flush().expect("flush");
    }

    pub(crate) fn truncate(&self, path: &Path, len: u64) {
        let file = OpenOptions::new()
            .write(true)
            .open(path)
            .expect("open for truncate");
        file.set_len(len).expect("truncate");
    }

    /// Replace the file wholesale, which is what a rotation looks like on disk.
    pub(crate) fn rewrite(&self, path: &Path, lines: &[String]) {
        let body = lines
            .iter()
            .map(|line| format!("{line}\n"))
            .collect::<String>();
        std::fs::write(path, body).expect("rewrite");
    }

    pub(crate) fn request(&self) -> ObserveSessionRequest {
        ObserveSessionRequest {
            adapter_id: "claude-code".to_string(),
            agent_root: self.root.path().to_string_lossy().into_owned(),
            transcript_path: self.transcript().to_string_lossy().into_owned(),
            native_session_id: Some(SESSION.to_string()),
            include_descendants: Some(true),
            max_queued_events: None,
            max_queued_bytes: None,
            // Notifications are hints; a short reconciliation interval keeps
            // the tests fast without depending on watcher timing.
            poll_interval_ms: Some(15),
        }
    }

    pub(crate) fn open(&self) -> ObserverHandle {
        open_observer(&self.request()).expect("observer attaches")
    }
}

/// The binding layer injects the adapter at runtime; tests do it directly.
pub(crate) fn open_observer(
    request: &ObserveSessionRequest,
) -> Result<ObserverHandle, crate::observer::ObserverError> {
    ObserverHandle::open(request, std::sync::Arc::new(ClaudeCodeAdapter::new()))
}

/// A minimal user record. `sessionId` must match the transcript locator or the
/// decoder rejects the record, which is the identity rule under test elsewhere.
pub(crate) fn user_record(uuid: &str) -> String {
    format!(
        r#"{{"type":"user","uuid":"{uuid}","sessionId":"{SESSION}","timestamp":"2026-08-11T00:00:00Z","cwd":"/fixture","version":"1","gitBranch":"main","isSidechain":false,"userType":"external","message":{{"role":"user","content":[]}}}}"#
    )
}

/// An assistant record carrying content, a tool call, and response usage: it
/// exercises the message, content-block, tool, usage-v2, effective-state, and
/// actor-run families from one line.
pub(crate) fn assistant_record(uuid: &str, response_id: &str, input_tokens: u64) -> String {
    format!(
        r#"{{"type":"assistant","uuid":"{uuid}","parentUuid":"u1","timestamp":"2026-08-11T00:00:00Z","sessionId":"{SESSION}","cwd":"/fixture","version":"1","gitBranch":"main","isSidechain":false,"userType":"external","requestId":"r-{uuid}","message":{{"model":"fixture-model","id":"{response_id}","type":"message","role":"assistant","content":[{{"type":"text","text":"fixture"}},{{"type":"tool_use","id":"tool-{uuid}","name":"Read","input":{{}}}}],"usage":{{"input_tokens":{input_tokens},"output_tokens":5,"cache_creation_input_tokens":2,"cache_read_input_tokens":3}}}}}}"#
    )
}

/// The smallest assistant record that still reduces to one usage revision.
/// Used where a test needs many revisions and the file size matters.
pub(crate) fn compact_assistant_record(index: usize) -> String {
    format!(
        r#"{{"type":"assistant","uuid":"m{index}","timestamp":"2026-08-11T00:00:00Z","sessionId":"{SESSION}","cwd":"/f","version":"1","isSidechain":false,"userType":"external","requestId":"r{index}","message":{{"model":"m","id":"p{index}","type":"message","role":"assistant","content":[],"usage":{{"input_tokens":1,"output_tokens":1,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}}}}"#
    )
}

/// A workflow journal record.
pub(crate) fn workflow_journal_record(workflow: &str) -> String {
    format!(r#"{{"type":"started","agentId":"a1","key":"step-1","workflowId":"{workflow}"}}"#)
}

/// A subagent transcript record. The child declares its own actor id.
pub(crate) fn subagent_record(agent_id: &str, uuid: &str) -> String {
    format!(
        r#"{{"type":"assistant","uuid":"{uuid}","parentUuid":"u1","timestamp":"2026-08-11T00:00:00Z","sessionId":"{SESSION}","cwd":"/fixture","version":"1","gitBranch":"main","isSidechain":true,"userType":"external","agentId":"{agent_id}","requestId":"r-{uuid}","message":{{"model":"fixture-model","id":"resp-{uuid}","type":"message","role":"assistant","content":[{{"type":"text","text":"child"}}],"usage":{{"input_tokens":1,"output_tokens":1,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}}}}"#
    )
}

/// Drain events until `predicate` accepts the accumulated set, or give up.
pub(crate) fn collect_until(
    handle: &ObserverHandle,
    timeout: Duration,
    mut predicate: impl FnMut(&[ObserverEvent]) -> bool,
) -> Vec<ObserverEvent> {
    let deadline = Instant::now() + timeout;
    let mut collected = Vec::new();
    loop {
        if predicate(&collected) {
            return collected;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return collected;
        }
        collected.extend(handle.wait_for_events(remaining.min(Duration::from_millis(50)), 4096));
    }
}

/// Everything delivered through the first bootstrap barrier.
pub(crate) fn drain_bootstrap(handle: &ObserverHandle) -> Vec<ObserverEvent> {
    drain_bootstrap_within(handle, Duration::from_secs(10))
}

pub(crate) fn drain_bootstrap_within(
    handle: &ObserverHandle,
    timeout: Duration,
) -> Vec<ObserverEvent> {
    collect_until(handle, timeout, |events| {
        events
            .iter()
            .any(|event| matches!(event, ObserverEvent::BootstrapComplete(_)))
    })
}

pub(crate) fn semantic_ids(events: &[ObserverEvent]) -> Vec<String> {
    events
        .iter()
        .filter(|event| !event.is_control())
        .map(|event| event.event_id().to_string())
        .collect()
}

/// `(family, entity_count, digest)` from the first barrier in the batch.
pub(crate) fn barrier_manifest(events: &[ObserverEvent]) -> Vec<(String, u32, String)> {
    events
        .iter()
        .find_map(|event| match event {
            ObserverEvent::BootstrapComplete(barrier) | ObserverEvent::ResyncComplete(barrier) => {
                Some(
                    barrier
                        .family_manifest
                        .iter()
                        .map(|entry| {
                            (
                                entry.family.as_str().to_string(),
                                entry.entity_count,
                                entry.digest.clone(),
                            )
                        })
                        .collect(),
                )
            }
            _ => None,
        })
        .unwrap_or_default()
}
