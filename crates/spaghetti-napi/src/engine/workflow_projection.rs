//! Workflow run-summary and append-journal projection.
//!
//! Run documents are independently replaceable snapshots. Journal records
//! accumulate within one append generation and establish workflow membership
//! plus native started/result observations. Workflow terminal state is never
//! copied to a child run: completed workflows in the native corpus can retain
//! members without result records and can contain member-level errors.

use std::collections::BTreeSet;

use rusqlite::{params, OptionalExtension, Transaction};

use crate::adapter::{
    Fact, FactBatch, FactEnvelope, QualifiedTimestamp, WorkflowMemberEventFact,
    WorkflowMemberEventKind, WorkflowSnapshotFact, WorkflowStatus,
};

use super::commit::{ChangeEntry, ProjectionCommitContext};
use super::EngineError;

const CHANGE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug)]
struct SnapshotAssertion {
    fact_id: Vec<u8>,
    session_key: Vec<u8>,
    project_key: Vec<u8>,
    native_workflow_id: String,
    native_task_id: String,
    name: String,
    native_status: String,
    workflow_status: String,
    default_model: String,
    script: String,
    script_path: String,
    args: Option<String>,
    summary: String,
    error: Option<String>,
    started_at: String,
    started_at_quality: String,
    finished_at: String,
    finished_at_quality: String,
    duration_ms: i64,
    agent_count: i64,
    total_tokens: i64,
    total_tool_calls: i64,
    native_snapshot_json: Vec<u8>,
    snapshot_digest: Vec<u8>,
}

#[derive(Debug)]
struct MemberEventAssertion {
    fact_id: Vec<u8>,
    workflow_key: Vec<u8>,
    child_run_key: Vec<u8>,
    session_key: Vec<u8>,
    project_key: Vec<u8>,
    native_workflow_id: String,
    native_agent_id: String,
    native_event_key: String,
    event_kind: String,
    result_json: Option<Vec<u8>>,
    event_digest: Vec<u8>,
}

#[derive(Debug, Default)]
struct MemberSummary {
    observed_count: usize,
    started_count: usize,
    result_count: usize,
    unresolved_count: usize,
    conflicting_count: usize,
    session_key: Option<Vec<u8>>,
    project_key: Option<Vec<u8>>,
    native_workflow_id: Option<String>,
    identity_conflict: bool,
}

#[derive(Debug, PartialEq, Eq)]
struct MemberReduction {
    member_status: Option<String>,
    resolution_status: Option<String>,
    started_assertion_count: usize,
    competing_started_count: usize,
    result_assertion_count: usize,
    competing_result_count: usize,
    event_key_conflict: bool,
    identity_conflict: bool,
}

#[derive(Debug, PartialEq, Eq)]
struct WorkflowReduction {
    snapshot_status: Option<String>,
    resolution_status: Option<String>,
    snapshot_assertion_count: usize,
    competing_snapshot_count: usize,
    observed_member_count: usize,
    started_member_count: usize,
    result_member_count: usize,
    unresolved_member_count: usize,
    conflicting_member_count: usize,
    membership_count_status: Option<String>,
    join_conflict: bool,
}

pub(super) fn apply_workflow_facts(
    transaction: &Transaction<'_>,
    context: &ProjectionCommitContext,
    batch: &FactBatch,
) -> Result<Vec<ChangeEntry>, EngineError> {
    let object_id = sqlite_u64(context.source_object_id, "source object id")?;
    let generation = sqlite_u64(context.generation, "source generation")?;
    let mut affected_workflows = source_object_keys(
        transaction,
        "SELECT DISTINCT workflow_key FROM workflow_snapshot_assertions WHERE source_object_id = ?1",
        object_id,
        "read source-owned workflow snapshots",
    )?;
    affected_workflows.extend(source_object_keys(
        transaction,
        "SELECT DISTINCT workflow_key FROM workflow_member_event_assertions WHERE source_object_id = ?1",
        object_id,
        "read source-owned workflow journal events",
    )?);
    let mut affected_members = source_object_keys(
        transaction,
        "SELECT DISTINCT member_key FROM workflow_member_event_assertions WHERE source_object_id = ?1",
        object_id,
        "read source-owned workflow members",
    )?;

    // A run summary is a whole replaceable document. Journal records append
    // within a generation and retract only when that append history rewrites.
    transaction
        .execute(
            "DELETE FROM workflow_snapshot_assertions WHERE source_object_id = ?1",
            [object_id],
        )
        .map_err(|error| sqlite_error("replace workflow run snapshot", error))?;
    transaction
        .execute(
            r#"
            DELETE FROM workflow_member_event_assertions
            WHERE source_object_id = ?1 AND source_generation <> ?2
            "#,
            params![object_id, generation],
        )
        .map_err(|error| sqlite_error("retract replaced workflow journal generation", error))?;

    for envelope in batch.facts() {
        match &envelope.value {
            Fact::WorkflowSnapshot(fact) => {
                write_snapshot_assertion(transaction, context, envelope, fact)?;
                affected_workflows.insert(fact.workflow.as_bytes().to_vec());
            }
            Fact::WorkflowMemberEvent(fact) => {
                write_member_event_assertion(transaction, context, envelope, fact)?;
                affected_workflows.insert(fact.workflow.as_bytes().to_vec());
                affected_members.insert(fact.member.as_bytes().to_vec());
            }
            _ => {}
        }
    }

    // A malformed or competing assertion can map one stable member key to
    // more than one workflow. Recompute every side currently asserted for an
    // affected member, not only the workflow owned by the committing object;
    // otherwise changing or retracting the decisive assertion can leave the
    // other workflow's aggregate counts stale.
    for member_key in &affected_members {
        affected_workflows.extend(member_workflow_keys(transaction, member_key)?);
    }

    let mut changes = Vec::new();
    for member_key in affected_members {
        let reduction = reduce_member(transaction, &member_key, context.commit_seq)?;
        changes.push(member_change(&member_key, &reduction)?);
        changes.push(member_conflict_change(&member_key, &reduction)?);
    }
    for workflow_key in affected_workflows {
        let reduction = reduce_workflow(transaction, &workflow_key, context.commit_seq)?;
        changes.push(workflow_change(&workflow_key, &reduction)?);
        changes.push(workflow_conflict_change(&workflow_key, &reduction)?);
    }

    // Canonical foreign keys now point at surviving decisive assertions.
    transaction
        .execute(
            r#"
            DELETE FROM fact_records
            WHERE source_object_id = ?1
              AND fact_kind = 'workflow_snapshot'
              AND last_commit_seq <> ?2
            "#,
            params![
                object_id,
                sqlite_u64(context.commit_seq, "commit sequence")?,
            ],
        )
        .map_err(|error| sqlite_error("retract replaced workflow snapshot facts", error))?;
    transaction
        .execute(
            r#"
            DELETE FROM fact_records
            WHERE source_object_id = ?1
              AND fact_kind = 'workflow_member_event'
              AND source_generation <> ?2
            "#,
            params![object_id, generation],
        )
        .map_err(|error| sqlite_error("retract replaced workflow event facts", error))?;

    Ok(changes)
}

fn write_snapshot_assertion(
    transaction: &Transaction<'_>,
    context: &ProjectionCommitContext,
    envelope: &FactEnvelope,
    fact: &WorkflowSnapshotFact,
) -> Result<(), EngineError> {
    for (field, value) in [
        ("native workflow id", fact.native_workflow_id.as_str()),
        ("native task id", fact.native_task_id.as_str()),
        ("workflow name", fact.name.as_str()),
        ("native workflow status", fact.native_status.as_str()),
        ("default model", fact.default_model.as_str()),
        ("workflow script", fact.script.as_str()),
        ("workflow script path", fact.script_path.as_str()),
        ("workflow summary", fact.summary.as_str()),
        ("workflow start time", fact.started_at.value.as_str()),
        ("workflow finish time", fact.finished_at.value.as_str()),
    ] {
        if value.trim().is_empty() {
            return Err(EngineError::InvalidCommit(format!(
                "{field} must not be empty"
            )));
        }
    }
    let workflow_status = workflow_status(&fact.status);
    let native_snapshot_json = serialize(&fact.native_snapshot, "serialize workflow snapshot")?;
    let snapshot_digest = digest(fact, "digest workflow snapshot")?;
    transaction
        .execute(
            r#"
            INSERT INTO workflow_snapshot_assertions (
                fact_id, workflow_key, session_key, project_key,
                native_workflow_id, native_task_id, name, native_status,
                workflow_status, default_model, script, script_path, args,
                summary, error, started_at, started_at_quality, finished_at,
                finished_at_quality, duration_ms, agent_count, total_tokens,
                total_tool_calls, native_snapshot_json, snapshot_digest,
                source_object_id, source_generation, cursor_end,
                last_commit_seq
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23,
                ?24, ?25, ?26, ?27, ?28, ?29
            )
            "#,
            params![
                envelope.id.as_bytes().as_slice(),
                fact.workflow.as_bytes(),
                fact.session.as_bytes(),
                fact.project.as_bytes(),
                fact.native_workflow_id,
                fact.native_task_id,
                fact.name,
                fact.native_status,
                workflow_status,
                fact.default_model,
                fact.script,
                fact.script_path,
                fact.args,
                fact.summary,
                fact.error,
                fact.started_at.value,
                timestamp_quality(&fact.started_at),
                fact.finished_at.value,
                timestamp_quality(&fact.finished_at),
                sqlite_u64(fact.duration_ms, "workflow duration")?,
                sqlite_u64(fact.agent_count, "workflow agent count")?,
                sqlite_u64(fact.total_tokens, "workflow total tokens")?,
                sqlite_u64(fact.total_tool_calls, "workflow total tool calls")?,
                native_snapshot_json,
                snapshot_digest.as_slice(),
                sqlite_u64(context.source_object_id, "source object id")?,
                sqlite_u64(context.generation, "source generation")?,
                envelope.provenance.cursor_end,
                sqlite_u64(context.commit_seq, "commit sequence")?,
            ],
        )
        .map(|_| ())
        .map_err(|error| sqlite_error("project workflow snapshot assertion", error))
}

fn write_member_event_assertion(
    transaction: &Transaction<'_>,
    context: &ProjectionCommitContext,
    envelope: &FactEnvelope,
    fact: &WorkflowMemberEventFact,
) -> Result<(), EngineError> {
    for (field, value) in [
        ("native workflow id", fact.native_workflow_id.as_str()),
        ("native workflow agent id", fact.native_agent_id.as_str()),
        ("native workflow event key", fact.native_event_key.as_str()),
    ] {
        if value.trim().is_empty() {
            return Err(EngineError::InvalidCommit(format!(
                "{field} must not be empty"
            )));
        }
    }
    match (fact.kind, fact.result.as_ref()) {
        (WorkflowMemberEventKind::Started, None) | (WorkflowMemberEventKind::Result, Some(_)) => {}
        _ => {
            return Err(EngineError::InvalidCommit(
                "workflow event kind and result payload disagree".to_string(),
            ));
        }
    }
    let event_kind = member_event_kind(fact.kind);
    let result_json = fact
        .result
        .as_ref()
        .map(|result| serialize(result, "serialize workflow member result"))
        .transpose()?;
    let event_digest = digest(
        &(
            &fact.workflow,
            &fact.member,
            &fact.child_run,
            &fact.session,
            &fact.project,
            &fact.native_workflow_id,
            &fact.native_agent_id,
            &fact.native_event_key,
            event_kind,
            &fact.result,
        ),
        "digest workflow member event",
    )?;
    transaction
        .execute(
            r#"
            INSERT INTO workflow_member_event_assertions (
                fact_id, workflow_key, member_key, child_run_key,
                session_key, project_key, native_workflow_id,
                native_agent_id, native_event_key, event_kind, result_json,
                event_digest, source_object_id, source_generation,
                cursor_end, last_commit_seq
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                ?13, ?14, ?15, ?16
            )
            "#,
            params![
                envelope.id.as_bytes().as_slice(),
                fact.workflow.as_bytes(),
                fact.member.as_bytes(),
                fact.child_run.as_bytes(),
                fact.session.as_bytes(),
                fact.project.as_bytes(),
                fact.native_workflow_id,
                fact.native_agent_id,
                fact.native_event_key,
                event_kind,
                result_json,
                event_digest.as_slice(),
                sqlite_u64(context.source_object_id, "source object id")?,
                sqlite_u64(context.generation, "source generation")?,
                envelope.provenance.cursor_end,
                sqlite_u64(context.commit_seq, "commit sequence")?,
            ],
        )
        .map(|_| ())
        .map_err(|error| sqlite_error("project workflow member event", error))
}

fn reduce_member(
    transaction: &Transaction<'_>,
    member_key: &[u8],
    commit_seq: u64,
) -> Result<MemberReduction, EngineError> {
    let assertions = read_member_assertions(transaction, member_key)?;
    if assertions.is_empty() {
        transaction
            .execute(
                "DELETE FROM canonical_workflow_members WHERE member_key = ?1",
                [member_key],
            )
            .map_err(|error| sqlite_error("remove absent canonical workflow member", error))?;
        return Ok(MemberReduction {
            member_status: None,
            resolution_status: None,
            started_assertion_count: 0,
            competing_started_count: 0,
            result_assertion_count: 0,
            competing_result_count: 0,
            event_key_conflict: false,
            identity_conflict: false,
        });
    }

    let started = assertions
        .iter()
        .filter(|assertion| assertion.event_kind == "started")
        .collect::<Vec<_>>();
    let results = assertions
        .iter()
        .filter(|assertion| assertion.event_kind == "result")
        .collect::<Vec<_>>();
    let started_assertion_count = started.len();
    let competing_started_count = distinct_digest_count(&started).saturating_sub(1);
    let result_assertion_count = results.len();
    let competing_result_count = distinct_digest_count(&results).saturating_sub(1);
    let decisive = assertions.first().expect("non-empty assertion set");
    let identity_conflict = assertions.iter().any(|assertion| {
        assertion.workflow_key != decisive.workflow_key
            || assertion.child_run_key != decisive.child_run_key
            || assertion.session_key != decisive.session_key
            || assertion.project_key != decisive.project_key
            || assertion.native_workflow_id != decisive.native_workflow_id
            || assertion.native_agent_id != decisive.native_agent_id
    });
    let event_key_conflict = assertions
        .iter()
        .map(|assertion| assertion.native_event_key.as_str())
        .collect::<BTreeSet<_>>()
        .len()
        > 1;
    let decisive_started = started.first().copied();
    let decisive_result = results.first().copied();
    let member_status = match (decisive_started, decisive_result) {
        (Some(_), Some(_)) => "result_observed",
        (Some(_), None) => "started",
        (None, Some(_)) => "orphan_result",
        (None, None) => {
            return Err(EngineError::InvalidCommit(
                "workflow member contains no supported event assertions".to_string(),
            ));
        }
    };
    let resolution_status = if competing_started_count > 0
        || competing_result_count > 0
        || event_key_conflict
        || identity_conflict
    {
        "conflicting"
    } else {
        "resolved"
    };
    let native_event_key = decisive_started
        .or(decisive_result)
        .map(|assertion| assertion.native_event_key.as_str())
        .expect("one supported assertion exists");

    transaction
        .execute(
            r#"
            INSERT INTO canonical_workflow_members (
                member_key, workflow_key, child_run_key, session_key,
                project_key, native_workflow_id, native_agent_id,
                native_event_key, member_status, result_json,
                resolution_status, decisive_started_fact_id,
                decisive_result_fact_id, started_assertion_count,
                competing_started_count, result_assertion_count,
                competing_result_count, event_key_conflict,
                identity_conflict, last_commit_seq
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20
            )
            ON CONFLICT(member_key) DO UPDATE SET
                workflow_key = excluded.workflow_key,
                child_run_key = excluded.child_run_key,
                session_key = excluded.session_key,
                project_key = excluded.project_key,
                native_workflow_id = excluded.native_workflow_id,
                native_agent_id = excluded.native_agent_id,
                native_event_key = excluded.native_event_key,
                member_status = excluded.member_status,
                result_json = excluded.result_json,
                resolution_status = excluded.resolution_status,
                decisive_started_fact_id = excluded.decisive_started_fact_id,
                decisive_result_fact_id = excluded.decisive_result_fact_id,
                started_assertion_count = excluded.started_assertion_count,
                competing_started_count = excluded.competing_started_count,
                result_assertion_count = excluded.result_assertion_count,
                competing_result_count = excluded.competing_result_count,
                event_key_conflict = excluded.event_key_conflict,
                identity_conflict = excluded.identity_conflict,
                last_commit_seq = excluded.last_commit_seq
            "#,
            params![
                member_key,
                decisive.workflow_key,
                decisive.child_run_key,
                decisive.session_key,
                decisive.project_key,
                decisive.native_workflow_id,
                decisive.native_agent_id,
                native_event_key,
                member_status,
                decisive_result.and_then(|assertion| assertion.result_json.as_deref()),
                resolution_status,
                decisive_started.map(|assertion| assertion.fact_id.as_slice()),
                decisive_result.map(|assertion| assertion.fact_id.as_slice()),
                sqlite_usize(started_assertion_count, "workflow started assertion count")?,
                sqlite_usize(competing_started_count, "workflow competing started count")?,
                sqlite_usize(result_assertion_count, "workflow result assertion count")?,
                sqlite_usize(competing_result_count, "workflow competing result count")?,
                i64::from(event_key_conflict),
                i64::from(identity_conflict),
                sqlite_u64(commit_seq, "commit sequence")?,
            ],
        )
        .map_err(|error| sqlite_error("write canonical workflow member", error))?;

    Ok(MemberReduction {
        member_status: Some(member_status.to_string()),
        resolution_status: Some(resolution_status.to_string()),
        started_assertion_count,
        competing_started_count,
        result_assertion_count,
        competing_result_count,
        event_key_conflict,
        identity_conflict,
    })
}

fn reduce_workflow(
    transaction: &Transaction<'_>,
    workflow_key: &[u8],
    commit_seq: u64,
) -> Result<WorkflowReduction, EngineError> {
    let snapshots = read_snapshot_assertions(transaction, workflow_key)?;
    let members = read_member_summary(transaction, workflow_key)?;
    if snapshots.is_empty() && members.observed_count == 0 {
        transaction
            .execute(
                "DELETE FROM canonical_workflows WHERE workflow_key = ?1",
                [workflow_key],
            )
            .map_err(|error| sqlite_error("remove absent canonical workflow", error))?;
        return Ok(WorkflowReduction {
            snapshot_status: None,
            resolution_status: None,
            snapshot_assertion_count: 0,
            competing_snapshot_count: 0,
            observed_member_count: 0,
            started_member_count: 0,
            result_member_count: 0,
            unresolved_member_count: 0,
            conflicting_member_count: 0,
            membership_count_status: None,
            join_conflict: false,
        });
    }

    let snapshot_assertion_count = snapshots.len();
    let competing_snapshot_count = snapshots
        .iter()
        .map(|assertion| &assertion.snapshot_digest)
        .collect::<BTreeSet<_>>()
        .len()
        .saturating_sub(1);
    let decisive_snapshot = snapshots.first();
    let join_conflict = members.identity_conflict
        || decisive_snapshot.is_some_and(|snapshot| {
            members
                .session_key
                .as_deref()
                .is_some_and(|session| session != snapshot.session_key)
                || members
                    .project_key
                    .as_deref()
                    .is_some_and(|project| project != snapshot.project_key)
                || members
                    .native_workflow_id
                    .as_deref()
                    .is_some_and(|native_id| native_id != snapshot.native_workflow_id)
        });
    let snapshot_status = if decisive_snapshot.is_some() {
        "present"
    } else {
        "missing"
    };
    let resolution_status = if competing_snapshot_count > 0 || join_conflict {
        "conflicting"
    } else if decisive_snapshot.is_none() {
        "incomplete"
    } else {
        "resolved"
    };
    let membership_count_status = match (decisive_snapshot, members.observed_count) {
        (_, 0) => "unobserved",
        (None, _) => "snapshot_missing",
        (Some(snapshot), count) if snapshot.agent_count == sqlite_usize(count, "member count")? => {
            "matched"
        }
        (Some(_), _) => "different",
    };
    let session_key = decisive_snapshot
        .map(|snapshot| snapshot.session_key.as_slice())
        .or(members.session_key.as_deref())
        .expect("snapshot or member supplies workflow session");
    let project_key = decisive_snapshot
        .map(|snapshot| snapshot.project_key.as_slice())
        .or(members.project_key.as_deref())
        .expect("snapshot or member supplies workflow project");
    let native_workflow_id = decisive_snapshot
        .map(|snapshot| snapshot.native_workflow_id.as_str())
        .or(members.native_workflow_id.as_deref())
        .expect("snapshot or member supplies native workflow id");

    transaction
        .execute(
            r#"
            INSERT INTO canonical_workflows (
                workflow_key, session_key, project_key, native_workflow_id,
                native_task_id, name, native_status, workflow_status,
                default_model, script, script_path, args, summary, error,
                started_at, started_at_quality, finished_at,
                finished_at_quality, duration_ms, agent_count, total_tokens,
                total_tool_calls, native_snapshot_json, snapshot_status,
                resolution_status, decisive_snapshot_fact_id,
                snapshot_assertion_count, competing_snapshot_count,
                observed_member_count, started_member_count,
                result_member_count, unresolved_member_count,
                conflicting_member_count, membership_count_status,
                join_conflict, last_commit_seq
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23,
                ?24, ?25, ?26, ?27, ?28, ?29, ?30, ?31, ?32, ?33, ?34,
                ?35, ?36
            )
            ON CONFLICT(workflow_key) DO UPDATE SET
                session_key = excluded.session_key,
                project_key = excluded.project_key,
                native_workflow_id = excluded.native_workflow_id,
                native_task_id = excluded.native_task_id,
                name = excluded.name,
                native_status = excluded.native_status,
                workflow_status = excluded.workflow_status,
                default_model = excluded.default_model,
                script = excluded.script,
                script_path = excluded.script_path,
                args = excluded.args,
                summary = excluded.summary,
                error = excluded.error,
                started_at = excluded.started_at,
                started_at_quality = excluded.started_at_quality,
                finished_at = excluded.finished_at,
                finished_at_quality = excluded.finished_at_quality,
                duration_ms = excluded.duration_ms,
                agent_count = excluded.agent_count,
                total_tokens = excluded.total_tokens,
                total_tool_calls = excluded.total_tool_calls,
                native_snapshot_json = excluded.native_snapshot_json,
                snapshot_status = excluded.snapshot_status,
                resolution_status = excluded.resolution_status,
                decisive_snapshot_fact_id = excluded.decisive_snapshot_fact_id,
                snapshot_assertion_count = excluded.snapshot_assertion_count,
                competing_snapshot_count = excluded.competing_snapshot_count,
                observed_member_count = excluded.observed_member_count,
                started_member_count = excluded.started_member_count,
                result_member_count = excluded.result_member_count,
                unresolved_member_count = excluded.unresolved_member_count,
                conflicting_member_count = excluded.conflicting_member_count,
                membership_count_status = excluded.membership_count_status,
                join_conflict = excluded.join_conflict,
                last_commit_seq = excluded.last_commit_seq
            "#,
            params![
                workflow_key,
                session_key,
                project_key,
                native_workflow_id,
                decisive_snapshot.map(|snapshot| snapshot.native_task_id.as_str()),
                decisive_snapshot.map(|snapshot| snapshot.name.as_str()),
                decisive_snapshot.map(|snapshot| snapshot.native_status.as_str()),
                decisive_snapshot.map(|snapshot| snapshot.workflow_status.as_str()),
                decisive_snapshot.map(|snapshot| snapshot.default_model.as_str()),
                decisive_snapshot.map(|snapshot| snapshot.script.as_str()),
                decisive_snapshot.map(|snapshot| snapshot.script_path.as_str()),
                decisive_snapshot.and_then(|snapshot| snapshot.args.as_deref()),
                decisive_snapshot.map(|snapshot| snapshot.summary.as_str()),
                decisive_snapshot.and_then(|snapshot| snapshot.error.as_deref()),
                decisive_snapshot.map(|snapshot| snapshot.started_at.as_str()),
                decisive_snapshot.map(|snapshot| snapshot.started_at_quality.as_str()),
                decisive_snapshot.map(|snapshot| snapshot.finished_at.as_str()),
                decisive_snapshot.map(|snapshot| snapshot.finished_at_quality.as_str()),
                decisive_snapshot.map(|snapshot| snapshot.duration_ms),
                decisive_snapshot.map(|snapshot| snapshot.agent_count),
                decisive_snapshot.map(|snapshot| snapshot.total_tokens),
                decisive_snapshot.map(|snapshot| snapshot.total_tool_calls),
                decisive_snapshot.map(|snapshot| snapshot.native_snapshot_json.as_slice()),
                snapshot_status,
                resolution_status,
                decisive_snapshot.map(|snapshot| snapshot.fact_id.as_slice()),
                sqlite_usize(
                    snapshot_assertion_count,
                    "workflow snapshot assertion count"
                )?,
                sqlite_usize(
                    competing_snapshot_count,
                    "workflow competing snapshot count"
                )?,
                sqlite_usize(members.observed_count, "workflow observed member count")?,
                sqlite_usize(members.started_count, "workflow started member count")?,
                sqlite_usize(members.result_count, "workflow result member count")?,
                sqlite_usize(members.unresolved_count, "workflow unresolved member count")?,
                sqlite_usize(
                    members.conflicting_count,
                    "workflow conflicting member count"
                )?,
                membership_count_status,
                i64::from(join_conflict),
                sqlite_u64(commit_seq, "commit sequence")?,
            ],
        )
        .map_err(|error| sqlite_error("write canonical workflow", error))?;

    Ok(WorkflowReduction {
        snapshot_status: Some(snapshot_status.to_string()),
        resolution_status: Some(resolution_status.to_string()),
        snapshot_assertion_count,
        competing_snapshot_count,
        observed_member_count: members.observed_count,
        started_member_count: members.started_count,
        result_member_count: members.result_count,
        unresolved_member_count: members.unresolved_count,
        conflicting_member_count: members.conflicting_count,
        membership_count_status: Some(membership_count_status.to_string()),
        join_conflict,
    })
}

fn read_snapshot_assertions(
    transaction: &Transaction<'_>,
    workflow_key: &[u8],
) -> Result<Vec<SnapshotAssertion>, EngineError> {
    let mut statement = transaction
        .prepare(
            r#"
            SELECT fact_id, session_key, project_key, native_workflow_id,
                   native_task_id, name, native_status, workflow_status,
                   default_model, script, script_path, args, summary, error,
                   started_at, started_at_quality, finished_at,
                   finished_at_quality, duration_ms, agent_count, total_tokens,
                   total_tool_calls, native_snapshot_json, snapshot_digest
            FROM workflow_snapshot_assertions
            WHERE workflow_key = ?1
            ORDER BY fact_id
            "#,
        )
        .map_err(|error| sqlite_error("prepare workflow snapshot reduction", error))?;
    let assertions = statement
        .query_map([workflow_key], |row| {
            Ok(SnapshotAssertion {
                fact_id: row.get(0)?,
                session_key: row.get(1)?,
                project_key: row.get(2)?,
                native_workflow_id: row.get(3)?,
                native_task_id: row.get(4)?,
                name: row.get(5)?,
                native_status: row.get(6)?,
                workflow_status: row.get(7)?,
                default_model: row.get(8)?,
                script: row.get(9)?,
                script_path: row.get(10)?,
                args: row.get(11)?,
                summary: row.get(12)?,
                error: row.get(13)?,
                started_at: row.get(14)?,
                started_at_quality: row.get(15)?,
                finished_at: row.get(16)?,
                finished_at_quality: row.get(17)?,
                duration_ms: row.get(18)?,
                agent_count: row.get(19)?,
                total_tokens: row.get(20)?,
                total_tool_calls: row.get(21)?,
                native_snapshot_json: row.get(22)?,
                snapshot_digest: row.get(23)?,
            })
        })
        .map_err(|error| sqlite_error("read workflow snapshot reduction", error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| sqlite_error("collect workflow snapshot reduction", error))?;
    Ok(assertions)
}

fn read_member_assertions(
    transaction: &Transaction<'_>,
    member_key: &[u8],
) -> Result<Vec<MemberEventAssertion>, EngineError> {
    let mut statement = transaction
        .prepare(
            r#"
            SELECT fact_id, workflow_key, child_run_key, session_key,
                   project_key, native_workflow_id, native_agent_id,
                   native_event_key, event_kind, result_json, event_digest
            FROM workflow_member_event_assertions
            WHERE member_key = ?1
            ORDER BY fact_id
            "#,
        )
        .map_err(|error| sqlite_error("prepare workflow member reduction", error))?;
    let assertions = statement
        .query_map([member_key], |row| {
            Ok(MemberEventAssertion {
                fact_id: row.get(0)?,
                workflow_key: row.get(1)?,
                child_run_key: row.get(2)?,
                session_key: row.get(3)?,
                project_key: row.get(4)?,
                native_workflow_id: row.get(5)?,
                native_agent_id: row.get(6)?,
                native_event_key: row.get(7)?,
                event_kind: row.get(8)?,
                result_json: row.get(9)?,
                event_digest: row.get(10)?,
            })
        })
        .map_err(|error| sqlite_error("read workflow member reduction", error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| sqlite_error("collect workflow member reduction", error))?;
    Ok(assertions)
}

fn read_member_summary(
    transaction: &Transaction<'_>,
    workflow_key: &[u8],
) -> Result<MemberSummary, EngineError> {
    let counts = transaction
        .query_row(
            r#"
            SELECT COUNT(*),
                   COALESCE(SUM(decisive_started_fact_id IS NOT NULL), 0),
                   COALESCE(SUM(decisive_result_fact_id IS NOT NULL), 0),
                   COALESCE(SUM(member_status <> 'result_observed'), 0),
                   COALESCE(SUM(resolution_status = 'conflicting'), 0),
                   COUNT(DISTINCT session_key),
                   COUNT(DISTINCT project_key),
                   COUNT(DISTINCT native_workflow_id)
            FROM canonical_workflow_members
            WHERE workflow_key = ?1
            "#,
            [workflow_key],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                ))
            },
        )
        .map_err(|error| sqlite_error("read workflow member counts", error))?;
    let identity = transaction
        .query_row(
            r#"
            SELECT session_key, project_key, native_workflow_id
            FROM canonical_workflow_members
            WHERE workflow_key = ?1
            ORDER BY member_key
            LIMIT 1
            "#,
            [workflow_key],
            |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()
        .map_err(|error| sqlite_error("read workflow member identity", error))?;
    let (session_key, project_key, native_workflow_id) = match identity {
        Some((session, project, native_id)) => (Some(session), Some(project), Some(native_id)),
        None => (None, None, None),
    };
    Ok(MemberSummary {
        observed_count: sqlite_count(counts.0, "workflow observed member count")?,
        started_count: sqlite_count(counts.1, "workflow started member count")?,
        result_count: sqlite_count(counts.2, "workflow result member count")?,
        unresolved_count: sqlite_count(counts.3, "workflow unresolved member count")?,
        conflicting_count: sqlite_count(counts.4, "workflow conflicting member count")?,
        session_key,
        project_key,
        native_workflow_id,
        identity_conflict: counts.5 > 1 || counts.6 > 1 || counts.7 > 1,
    })
}

fn source_object_keys(
    transaction: &Transaction<'_>,
    query: &'static str,
    source_object_id: i64,
    operation: &'static str,
) -> Result<BTreeSet<Vec<u8>>, EngineError> {
    let mut statement = transaction
        .prepare(query)
        .map_err(|error| sqlite_error(operation, error))?;
    let keys = statement
        .query_map([source_object_id], |row| row.get::<_, Vec<u8>>(0))
        .map_err(|error| sqlite_error(operation, error))?
        .collect::<Result<BTreeSet<_>, _>>()
        .map_err(|error| sqlite_error(operation, error))?;
    Ok(keys)
}

fn member_workflow_keys(
    transaction: &Transaction<'_>,
    member_key: &[u8],
) -> Result<BTreeSet<Vec<u8>>, EngineError> {
    let mut statement = transaction
        .prepare(
            r#"
            SELECT DISTINCT workflow_key
            FROM workflow_member_event_assertions
            WHERE member_key = ?1
            "#,
        )
        .map_err(|error| sqlite_error("prepare workflow member dependencies", error))?;
    let keys = statement
        .query_map([member_key], |row| row.get::<_, Vec<u8>>(0))
        .map_err(|error| sqlite_error("read workflow member dependencies", error))?
        .collect::<Result<BTreeSet<_>, _>>()
        .map_err(|error| sqlite_error("collect workflow member dependencies", error))?;
    Ok(keys)
}

fn distinct_digest_count(assertions: &[&MemberEventAssertion]) -> usize {
    assertions
        .iter()
        .map(|assertion| &assertion.event_digest)
        .collect::<BTreeSet<_>>()
        .len()
}

fn workflow_change(
    workflow_key: &[u8],
    reduction: &WorkflowReduction,
) -> Result<ChangeEntry, EngineError> {
    change(
        "runtime.workflow.changed",
        workflow_key,
        reduction.resolution_status.is_some(),
        &serde_json::json!({
            "snapshot_status": reduction.snapshot_status,
            "resolution_status": reduction.resolution_status,
            "snapshot_assertion_count": reduction.snapshot_assertion_count,
            "competing_snapshot_count": reduction.competing_snapshot_count,
            "observed_member_count": reduction.observed_member_count,
            "started_member_count": reduction.started_member_count,
            "result_member_count": reduction.result_member_count,
            "unresolved_member_count": reduction.unresolved_member_count,
            "conflicting_member_count": reduction.conflicting_member_count,
            "membership_count_status": reduction.membership_count_status,
            "join_conflict": reduction.join_conflict,
        }),
        "serialize workflow change",
    )
}

fn workflow_conflict_change(
    workflow_key: &[u8],
    reduction: &WorkflowReduction,
) -> Result<ChangeEntry, EngineError> {
    let competing_count = reduction
        .competing_snapshot_count
        .saturating_add(usize::from(reduction.join_conflict));
    change(
        "diagnostic.runtime.workflow-conflict",
        workflow_key,
        competing_count > 0,
        &serde_json::json!({
            "conflicting": competing_count > 0,
            "competing_count": competing_count,
            "competing_snapshot_count": reduction.competing_snapshot_count,
            "join_conflict": reduction.join_conflict,
        }),
        "serialize workflow conflict",
    )
}

fn member_change(
    member_key: &[u8],
    reduction: &MemberReduction,
) -> Result<ChangeEntry, EngineError> {
    change(
        "runtime.workflow-member.changed",
        member_key,
        reduction.resolution_status.is_some(),
        &serde_json::json!({
            "member_status": reduction.member_status,
            "resolution_status": reduction.resolution_status,
            "started_assertion_count": reduction.started_assertion_count,
            "competing_started_count": reduction.competing_started_count,
            "result_assertion_count": reduction.result_assertion_count,
            "competing_result_count": reduction.competing_result_count,
            "event_key_conflict": reduction.event_key_conflict,
            "identity_conflict": reduction.identity_conflict,
        }),
        "serialize workflow member change",
    )
}

fn member_conflict_change(
    member_key: &[u8],
    reduction: &MemberReduction,
) -> Result<ChangeEntry, EngineError> {
    let competing_count = reduction
        .competing_started_count
        .saturating_add(reduction.competing_result_count)
        .saturating_add(usize::from(reduction.event_key_conflict))
        .saturating_add(usize::from(reduction.identity_conflict));
    change(
        "diagnostic.runtime.workflow-member-conflict",
        member_key,
        competing_count > 0,
        &serde_json::json!({
            "conflicting": competing_count > 0,
            "competing_count": competing_count,
            "competing_started_count": reduction.competing_started_count,
            "competing_result_count": reduction.competing_result_count,
            "event_key_conflict": reduction.event_key_conflict,
            "identity_conflict": reduction.identity_conflict,
        }),
        "serialize workflow member conflict",
    )
}

fn change<T: serde::Serialize>(
    topic: &str,
    entity_key: &[u8],
    present: bool,
    payload: &T,
    operation: &'static str,
) -> Result<ChangeEntry, EngineError> {
    Ok(ChangeEntry {
        topic: topic.to_string(),
        schema_version: CHANGE_SCHEMA_VERSION,
        entity_key: entity_key.to_vec(),
        operation: if present { "upsert" } else { "delete" }.to_string(),
        payload: serialize(payload, operation)?,
    })
}

fn workflow_status(status: &WorkflowStatus) -> &str {
    match status {
        WorkflowStatus::Pending => "pending",
        WorkflowStatus::Running => "running",
        WorkflowStatus::Succeeded => "succeeded",
        WorkflowStatus::Failed => "failed",
        WorkflowStatus::Cancelled => "cancelled",
        WorkflowStatus::Other(_) => "other",
    }
}

fn member_event_kind(kind: WorkflowMemberEventKind) -> &'static str {
    match kind {
        WorkflowMemberEventKind::Started => "started",
        WorkflowMemberEventKind::Result => "result",
    }
}

fn timestamp_quality(timestamp: &QualifiedTimestamp) -> &'static str {
    match timestamp.quality {
        crate::adapter::TimestampQuality::NativeExact => "native_exact",
        crate::adapter::TimestampQuality::NativeApproximate => "native_approximate",
        crate::adapter::TimestampQuality::FileMetadataFallback => "file_metadata_fallback",
        crate::adapter::TimestampQuality::Derived => "derived",
    }
}

fn digest<T: serde::Serialize>(
    value: &T,
    operation: &'static str,
) -> Result<[u8; 32], EngineError> {
    serialize(value, operation).map(|encoded| *blake3::hash(&encoded).as_bytes())
}

fn serialize<T: serde::Serialize>(
    value: &T,
    operation: &'static str,
) -> Result<Vec<u8>, EngineError> {
    serde_json::to_vec(value)
        .map_err(|error| EngineError::InvalidCommit(format!("{operation}: {error}")))
}

fn sqlite_count(value: i64, field: &'static str) -> Result<usize, EngineError> {
    usize::try_from(value)
        .map_err(|_| EngineError::InvalidCommit(format!("{field} is outside usize range")))
}

fn sqlite_usize(value: usize, field: &'static str) -> Result<i64, EngineError> {
    i64::try_from(value)
        .map_err(|_| EngineError::InvalidCommit(format!("{field} exceeds SQLite integer range")))
}

fn sqlite_u64(value: u64, field: &'static str) -> Result<i64, EngineError> {
    i64::try_from(value)
        .map_err(|_| EngineError::InvalidCommit(format!("{field} exceeds SQLite integer range")))
}

fn sqlite_error(operation: &'static str, error: rusqlite::Error) -> EngineError {
    EngineError::Sqlite {
        operation,
        detail: error.to_string(),
    }
}
