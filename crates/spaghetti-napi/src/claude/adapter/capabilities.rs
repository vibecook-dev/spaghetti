//! What the Claude support release declares this adapter can observe.
//!
//! Split out of `adapter.rs` to keep that file inside the landing plan's
//! 3,000-line production cap. These are declarations, not logic: each entry
//! names a capability, its granularity, and the support level the release
//! claims for it.

use super::*;

pub(super) fn claude_capabilities() -> Vec<CapabilityDeclaration> {
    let live_native = |id, granularity| {
        capability(
            id,
            SupportLevel::Native,
            granularity,
            Availability::Live,
            None,
        )
    };
    vec![
        live_native(HISTORY_SESSIONS, CapabilityGranularity::Session),
        live_native(HISTORY_MESSAGES, CapabilityGranularity::Message),
        live_native(HISTORY_CONTENT_BLOCKS, CapabilityGranularity::Message),
        live_native(HISTORY_TIMESTAMPS, CapabilityGranularity::Message),
        live_native(HISTORY_MODEL_IDENTITY, CapabilityGranularity::Message),
        live_native(RUNTIME_SESSION_ACTIVITY, CapabilityGranularity::Run),
        live_native(RUNTIME_USAGE_V2, CapabilityGranularity::Message),
        capability(
            RUNTIME_SUBAGENTS,
            SupportLevel::Derived,
            CapabilityGranularity::Run,
            Availability::Live,
            Some(
                "child identity is native; layout lineage remains durable and matching native spawn/metadata tool-use IDs strengthen it explicitly; silence never implies completion",
            ),
        ),
        live_native(RUNTIME_TEAMS, CapabilityGranularity::Team),
        live_native(RUNTIME_TEAM_INBOX, CapabilityGranularity::Message),
        capability(
            RUNTIME_PRESENCE,
            SupportLevel::Native,
            CapabilityGranularity::Custom("process_presence".to_string()),
            Availability::Live,
            Some(
                "agent-owned registry presence is durable; host PID liveness and time-based freshness remain transient assessments",
            ),
        ),
        capability(
            RUNTIME_TASKS,
            SupportLevel::Native,
            CapabilityGranularity::Custom("task".to_string()),
            Availability::Live,
            Some(
                "todo files are complete snapshots, numbered task files are item documents, and task status never implies run completion",
            ),
        ),
        capability(
            RUNTIME_ARTIFACTS,
            SupportLevel::Native,
            CapabilityGranularity::Custom("artifact".to_string()),
            Availability::Live,
            Some(
                "file-history metadata and backup blobs are joined by native session and backup name; capture is session-attributed and never implies that a run produced the tracked file",
            ),
        ),
        capability(
            RUNTIME_WORKFLOWS,
            SupportLevel::Native,
            CapabilityGranularity::Custom("workflow".to_string()),
            Availability::EventuallyLive,
            Some(
                "workflow summaries and append journals preserve native workflow/member state; workflow terminal status never implies terminal child-run state",
            ),
        ),
        capability(
            CONTEXT_PROJECT_MEMORY,
            SupportLevel::Native,
            CapabilityGranularity::Custom("memory_document".to_string()),
            Availability::Live,
            Some(
                "project memory is a set of independently replaceable Markdown documents; MEMORY.md is the native index and links do not assert relations",
            ),
        ),
        capability(
            HISTORY_PERSISTED_TOOL_RESULTS,
            SupportLevel::Native,
            CapabilityGranularity::Custom("persisted_tool_result".to_string()),
            Availability::Live,
            Some(
                "immediate UTF-8 tool-results/*.txt documents supplement transcript content; filename stems are native identifiers but do not always denote a model tool call",
            ),
        ),
        capability(
            CONFIGURATION_INTERPRETATION_SETTINGS,
            SupportLevel::Native,
            CapabilityGranularity::Instance,
            Availability::Live,
            Some(
                "global and local root settings are reduced with native scalar precedence and array merging; sensitive values and command bodies are excluded",
            ),
        ),
        live_native(USAGE_INPUT_TOKENS, CapabilityGranularity::Message),
        live_native(USAGE_OUTPUT_TOKENS, CapabilityGranularity::Message),
        live_native(USAGE_CACHE_TOKENS, CapabilityGranularity::Message),
        live_native(SOURCE_LIVE, CapabilityGranularity::Instance),
        live_native(SOURCE_RECONCILE, CapabilityGranularity::Instance),
        live_native(SOURCE_RESUME_CURSOR, CapabilityGranularity::Record),
    ]
}

pub(super) fn capability(
    id: &'static str,
    level: SupportLevel,
    granularity: CapabilityGranularity,
    availability: Availability,
    notes: Option<&'static str>,
) -> CapabilityDeclaration {
    CapabilityDeclaration {
        id: CapabilityId::new(id).expect("static Claude capability id is valid"),
        support: CapabilitySupport {
            level,
            granularity,
            availability,
            notes: notes.map(str::to_owned),
        },
    }
}

pub(super) fn transcript_capabilities() -> Vec<CapabilityId> {
    [
        HISTORY_SESSIONS,
        HISTORY_MESSAGES,
        HISTORY_CONTENT_BLOCKS,
        HISTORY_TIMESTAMPS,
        HISTORY_MODEL_IDENTITY,
        RUNTIME_SESSION_ACTIVITY,
        RUNTIME_USAGE_V2,
        USAGE_INPUT_TOKENS,
        USAGE_OUTPUT_TOKENS,
        USAGE_CACHE_TOKENS,
        RUNTIME_SUBAGENTS,
        RUNTIME_ARTIFACTS,
        SOURCE_LIVE,
        SOURCE_RECONCILE,
        SOURCE_RESUME_CURSOR,
    ]
    .into_iter()
    .map(|id| CapabilityId::new(id).expect("static Claude stream capability id is valid"))
    .collect()
}

pub(super) fn capability_ids(ids: &[&str]) -> Vec<CapabilityId> {
    ids.iter()
        .map(|id| CapabilityId::new(*id).expect("static Claude capability id is valid"))
        .collect()
}

pub(super) fn subagent_metadata_capabilities() -> Vec<CapabilityId> {
    [
        RUNTIME_SUBAGENTS,
        RUNTIME_TEAMS,
        SOURCE_LIVE,
        SOURCE_RECONCILE,
        SOURCE_RESUME_CURSOR,
    ]
    .into_iter()
    .map(|id| CapabilityId::new(id).expect("static Claude stream capability id is valid"))
    .collect()
}

pub(super) fn presence_capabilities() -> Vec<CapabilityId> {
    [
        RUNTIME_PRESENCE,
        SOURCE_LIVE,
        SOURCE_RECONCILE,
        SOURCE_RESUME_CURSOR,
    ]
    .into_iter()
    .map(|id| CapabilityId::new(id).expect("static Claude stream capability id is valid"))
    .collect()
}

pub(super) fn task_capabilities() -> Vec<CapabilityId> {
    [
        RUNTIME_TASKS,
        SOURCE_LIVE,
        SOURCE_RECONCILE,
        SOURCE_RESUME_CURSOR,
    ]
    .into_iter()
    .map(|id| CapabilityId::new(id).expect("static Claude stream capability id is valid"))
    .collect()
}

pub(super) fn artifact_capabilities() -> Vec<CapabilityId> {
    [
        RUNTIME_ARTIFACTS,
        SOURCE_LIVE,
        SOURCE_RECONCILE,
        SOURCE_RESUME_CURSOR,
    ]
    .into_iter()
    .map(|id| CapabilityId::new(id).expect("static Claude stream capability id is valid"))
    .collect()
}

pub(super) fn workflow_capabilities() -> Vec<CapabilityId> {
    [
        RUNTIME_WORKFLOWS,
        SOURCE_LIVE,
        SOURCE_RECONCILE,
        SOURCE_RESUME_CURSOR,
    ]
    .into_iter()
    .map(|id| CapabilityId::new(id).expect("static Claude stream capability id is valid"))
    .collect()
}

pub(super) fn session_index_capabilities() -> Vec<CapabilityId> {
    [
        HISTORY_SESSIONS,
        HISTORY_TIMESTAMPS,
        SOURCE_LIVE,
        SOURCE_RECONCILE,
        SOURCE_RESUME_CURSOR,
    ]
    .into_iter()
    .map(|id| CapabilityId::new(id).expect("static Claude stream capability id is valid"))
    .collect()
}

pub(super) fn project_memory_capabilities() -> Vec<CapabilityId> {
    [
        CONTEXT_PROJECT_MEMORY,
        SOURCE_LIVE,
        SOURCE_RECONCILE,
        SOURCE_RESUME_CURSOR,
    ]
    .into_iter()
    .map(|id| CapabilityId::new(id).expect("static Claude stream capability id is valid"))
    .collect()
}

pub(super) fn persisted_tool_result_capabilities() -> Vec<CapabilityId> {
    [
        HISTORY_PERSISTED_TOOL_RESULTS,
        SOURCE_LIVE,
        SOURCE_RECONCILE,
        SOURCE_RESUME_CURSOR,
    ]
    .into_iter()
    .map(|id| CapabilityId::new(id).expect("static Claude stream capability id is valid"))
    .collect()
}

pub(super) fn interpretation_settings_capabilities() -> Vec<CapabilityId> {
    [
        CONFIGURATION_INTERPRETATION_SETTINGS,
        SOURCE_LIVE,
        SOURCE_RECONCILE,
        SOURCE_RESUME_CURSOR,
    ]
    .into_iter()
    .map(|id| CapabilityId::new(id).expect("static Claude stream capability id is valid"))
    .collect()
}
