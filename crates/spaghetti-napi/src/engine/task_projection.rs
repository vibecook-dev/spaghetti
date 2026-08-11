//! Replaceable task, todo, and plan projections.
//!
//! Complete todo documents and independently replaceable task-item documents
//! share one task model while retaining their different deletion coverage.
//! Task status is never promoted to run lifecycle evidence. Plan documents are
//! standalone until a separate native fact supplies a trustworthy relation.

use std::collections::BTreeSet;

use rusqlite::{params, Transaction};

use crate::adapter::{
    Fact, FactBatch, FactEnvelope, PlanSnapshotFact, QualifiedTimestamp, TaskCollectionKind,
    TaskItemSnapshot, TaskSnapshotCoverage, TaskSnapshotFact, TaskStatus,
};

use super::commit::{ChangeEntry, ProjectionCommitContext};
use super::EngineError;

const CHANGE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, PartialEq, Eq)]
struct CollectionReduction {
    resolution_status: Option<String>,
    assertion_count: usize,
    competing_metadata_count: usize,
    complete_snapshot_count: usize,
    item_document_count: usize,
    item_count: usize,
}

#[derive(Debug, PartialEq, Eq)]
struct TaskReduction {
    resolution_status: Option<String>,
    task_status: Option<String>,
    native_status: Option<String>,
    assertion_count: usize,
    competing_item_count: usize,
}

#[derive(Debug, PartialEq, Eq)]
struct PlanReduction {
    resolution_status: Option<String>,
    assertion_count: usize,
    competing_plan_count: usize,
}

pub(super) fn apply_task_snapshots(
    transaction: &Transaction<'_>,
    context: &ProjectionCommitContext,
    batch: &FactBatch,
) -> Result<Vec<ChangeEntry>, EngineError> {
    let object_id = sqlite_u64(context.source_object_id, "source object id")?;
    let mut affected_collections = source_object_keys(
        transaction,
        "SELECT DISTINCT collection_key FROM task_snapshot_assertions WHERE source_object_id = ?1",
        object_id,
        "read replaced task collections",
    )?;
    let mut affected_tasks = source_object_keys(
        transaction,
        r#"
        SELECT DISTINCT item.task_key
        FROM task_item_assertions AS item
        JOIN task_snapshot_assertions AS snapshot ON snapshot.fact_id = item.fact_id
        WHERE snapshot.source_object_id = ?1
        "#,
        object_id,
        "read replaced task items",
    )?;
    let mut affected_plans = source_object_keys(
        transaction,
        "SELECT DISTINCT plan_key FROM plan_assertions WHERE source_object_id = ?1",
        object_id,
        "read replaced plans",
    )?;

    // Every declared input is a replace-document object. Same-generation
    // edits and confirmed deletion therefore replace the object's complete
    // assertion set; child rows cascade from the snapshot assertion.
    transaction
        .execute(
            "DELETE FROM task_snapshot_assertions WHERE source_object_id = ?1",
            [object_id],
        )
        .map_err(|error| sqlite_error("retract replaced task snapshots", error))?;
    transaction
        .execute(
            "DELETE FROM plan_assertions WHERE source_object_id = ?1",
            [object_id],
        )
        .map_err(|error| sqlite_error("retract replaced plan snapshots", error))?;

    for envelope in batch.facts() {
        match &envelope.value {
            Fact::TaskSnapshot(fact) => {
                write_task_snapshot(transaction, context, envelope, fact)?;
                affected_collections.insert(fact.collection.as_bytes().to_vec());
                affected_tasks.extend(fact.items.iter().map(|item| item.task.as_bytes().to_vec()));
            }
            Fact::PlanSnapshot(fact) => {
                write_plan_snapshot(transaction, context, envelope, fact)?;
                affected_plans.insert(fact.plan.as_bytes().to_vec());
            }
            _ => {}
        }
    }

    let mut changes = Vec::new();
    for task_key in affected_tasks {
        let reduction = reduce_task(transaction, &task_key, context.commit_seq)?;
        changes.push(task_change(&task_key, &reduction)?);
        changes.push(task_conflict_change(&task_key, &reduction)?);
    }
    for collection_key in affected_collections {
        let reduction = reduce_collection(transaction, &collection_key, context.commit_seq)?;
        changes.push(collection_change(&collection_key, &reduction)?);
        changes.push(collection_conflict_change(&collection_key, &reduction)?);
    }
    for plan_key in affected_plans {
        let reduction = reduce_plan(transaction, &plan_key, context.commit_seq)?;
        changes.push(plan_change(&plan_key, &reduction)?);
        changes.push(plan_conflict_change(&plan_key, &reduction)?);
    }

    // Canonical foreign keys now point at current decisive assertions (or the
    // rows have been removed), so superseded replace-document audit facts can
    // be retired without leaving provenance orphans.
    transaction
        .execute(
            r#"
            DELETE FROM fact_records
            WHERE source_object_id = ?1
              AND fact_kind IN ('task_snapshot', 'plan_snapshot')
              AND last_commit_seq <> ?2
            "#,
            params![
                object_id,
                sqlite_u64(context.commit_seq, "commit sequence")?,
            ],
        )
        .map_err(|error| sqlite_error("retract replaced task and plan facts", error))?;

    Ok(changes)
}

fn write_task_snapshot(
    transaction: &Transaction<'_>,
    context: &ProjectionCommitContext,
    envelope: &FactEnvelope,
    fact: &TaskSnapshotFact,
) -> Result<(), EngineError> {
    let (collection_kind, native_collection_kind) = collection_kind(&fact.kind);
    // Item documents intentionally agree on collection metadata so their
    // independently owned children merge. Complete snapshots also contribute
    // their full item set, making two disagreeing authoritative snapshots a
    // collection-level conflict as well as any child-level conflicts.
    let complete_items =
        matches!(fact.coverage, TaskSnapshotCoverage::Complete).then_some(fact.items.as_slice());
    let metadata_digest = digest(
        &(
            &fact.collection,
            &fact.session,
            &fact.run,
            &fact.team,
            &fact.native_collection_id,
            &fact.native_owner_id,
            collection_kind,
            native_collection_kind,
            complete_items,
        ),
        "digest task collection metadata",
    )?;
    transaction
        .execute(
            r#"
            INSERT INTO task_snapshot_assertions (
                fact_id, collection_key, session_key, run_key, team_key,
                native_collection_id, native_owner_id, collection_kind,
                native_collection_kind, coverage, metadata_digest,
                source_object_id, source_generation, cursor_end,
                last_commit_seq
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                ?13, ?14, ?15
            )
            "#,
            params![
                envelope.id.as_bytes().as_slice(),
                fact.collection.as_bytes(),
                fact.session.as_ref().map(|key| key.as_bytes()),
                fact.run.as_ref().map(|key| key.as_bytes()),
                fact.team.as_ref().map(|key| key.as_bytes()),
                fact.native_collection_id,
                fact.native_owner_id,
                collection_kind,
                native_collection_kind,
                coverage(fact.coverage),
                metadata_digest.as_slice(),
                sqlite_u64(context.source_object_id, "source object id")?,
                sqlite_u64(context.generation, "source generation")?,
                envelope.provenance.cursor_end,
                sqlite_u64(context.commit_seq, "commit sequence")?,
            ],
        )
        .map_err(|error| sqlite_error("project task snapshot assertion", error))?;

    for (ordinal, item) in fact.items.iter().enumerate() {
        write_task_item(transaction, envelope, &fact.collection, ordinal, item)?;
    }
    Ok(())
}

fn write_task_item(
    transaction: &Transaction<'_>,
    envelope: &FactEnvelope,
    collection: &crate::adapter::EntityKey,
    ordinal: usize,
    item: &TaskItemSnapshot,
) -> Result<(), EngineError> {
    let (task_status, native_status) = task_status(&item.status);
    let blocks_json = serialize(&item.blocks, "serialize task blockers")?;
    let blocked_by_json = serialize(&item.blocked_by, "serialize task blocked-by references")?;
    let item_digest = digest(item, "digest task item")?;
    transaction
        .execute(
            r#"
            INSERT INTO task_item_assertions (
                fact_id, task_key, collection_key, item_ordinal,
                native_task_id, subject, description, active_form,
                native_owner, task_status, native_status, blocks_json,
                blocked_by_json, item_digest
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                ?13, ?14
            )
            "#,
            params![
                envelope.id.as_bytes().as_slice(),
                item.task.as_bytes(),
                collection.as_bytes(),
                sqlite_usize(ordinal, "task item ordinal")?,
                item.native_task_id,
                item.subject,
                item.description,
                item.active_form,
                item.native_owner,
                task_status,
                native_status,
                blocks_json,
                blocked_by_json,
                item_digest.as_slice(),
            ],
        )
        .map(|_| ())
        .map_err(|error| sqlite_error("project task item assertion", error))
}

fn write_plan_snapshot(
    transaction: &Transaction<'_>,
    context: &ProjectionCommitContext,
    envelope: &FactEnvelope,
    fact: &PlanSnapshotFact,
) -> Result<(), EngineError> {
    let plan_digest = digest(fact, "digest plan snapshot")?;
    transaction
        .execute(
            r#"
            INSERT INTO plan_assertions (
                fact_id, plan_key, native_plan_id, title, content,
                size_bytes, source_time, source_time_quality, plan_digest,
                source_object_id, source_generation, cursor_end,
                last_commit_seq
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                ?13
            )
            "#,
            params![
                envelope.id.as_bytes().as_slice(),
                fact.plan.as_bytes(),
                fact.native_plan_id,
                fact.title,
                fact.content,
                sqlite_u64(fact.size_bytes, "plan size")?,
                timestamp_value(fact.source_time.as_ref()),
                timestamp_quality(fact.source_time.as_ref()),
                plan_digest.as_slice(),
                sqlite_u64(context.source_object_id, "source object id")?,
                sqlite_u64(context.generation, "source generation")?,
                envelope.provenance.cursor_end,
                sqlite_u64(context.commit_seq, "commit sequence")?,
            ],
        )
        .map(|_| ())
        .map_err(|error| sqlite_error("project plan assertion", error))
}

fn reduce_collection(
    transaction: &Transaction<'_>,
    collection_key: &[u8],
    commit_seq: u64,
) -> Result<CollectionReduction, EngineError> {
    let mut statement = transaction
        .prepare(
            r#"
            SELECT fact_id, metadata_digest, coverage
            FROM task_snapshot_assertions
            WHERE collection_key = ?1
            ORDER BY fact_id
            "#,
        )
        .map_err(|error| sqlite_error("prepare task collection reduction", error))?;
    let assertions = statement
        .query_map([collection_key], |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                row.get::<_, Vec<u8>>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|error| sqlite_error("read task collection assertions", error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| sqlite_error("collect task collection assertions", error))?;
    let Some((decisive_fact_id, _, _)) = assertions.first() else {
        transaction
            .execute(
                "DELETE FROM canonical_task_collections WHERE collection_key = ?1",
                [collection_key],
            )
            .map_err(|error| sqlite_error("remove absent task collection", error))?;
        return Ok(CollectionReduction {
            resolution_status: None,
            assertion_count: 0,
            competing_metadata_count: 0,
            complete_snapshot_count: 0,
            item_document_count: 0,
            item_count: 0,
        });
    };
    let assertion_count = assertions.len();
    let competing_metadata_count = assertions
        .iter()
        .map(|(_, digest, _)| digest)
        .collect::<BTreeSet<_>>()
        .len()
        .saturating_sub(1);
    let complete_snapshot_count = assertions
        .iter()
        .filter(|(_, _, coverage)| coverage == "complete")
        .count();
    let item_document_count = assertions
        .iter()
        .filter(|(_, _, coverage)| coverage == "item_document")
        .count();
    let item_count = transaction
        .query_row(
            "SELECT COUNT(DISTINCT task_key) FROM task_item_assertions WHERE collection_key = ?1",
            [collection_key],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|error| sqlite_error("count canonical task collection items", error))?;
    let item_count = usize::try_from(item_count).map_err(|_| {
        EngineError::InvalidCommit("task item count is outside usize range".to_string())
    })?;
    let resolution_status = if competing_metadata_count > 0 {
        "conflicting"
    } else {
        "resolved"
    };

    transaction
        .execute(
            r#"
            INSERT INTO canonical_task_collections (
                collection_key, session_key, run_key, team_key,
                native_collection_id, native_owner_id, collection_kind,
                native_collection_kind, resolution_status,
                decisive_fact_id, assertion_count,
                competing_metadata_count, complete_snapshot_count,
                item_document_count, item_count, last_commit_seq
            )
            SELECT collection_key, session_key, run_key, team_key,
                   native_collection_id, native_owner_id, collection_kind,
                   native_collection_kind, ?2, fact_id, ?3, ?4, ?5, ?6,
                   ?7, ?8
            FROM task_snapshot_assertions WHERE fact_id = ?1
            ON CONFLICT(collection_key) DO UPDATE SET
                session_key = excluded.session_key,
                run_key = excluded.run_key,
                team_key = excluded.team_key,
                native_collection_id = excluded.native_collection_id,
                native_owner_id = excluded.native_owner_id,
                collection_kind = excluded.collection_kind,
                native_collection_kind = excluded.native_collection_kind,
                resolution_status = excluded.resolution_status,
                decisive_fact_id = excluded.decisive_fact_id,
                assertion_count = excluded.assertion_count,
                competing_metadata_count = excluded.competing_metadata_count,
                complete_snapshot_count = excluded.complete_snapshot_count,
                item_document_count = excluded.item_document_count,
                item_count = excluded.item_count,
                last_commit_seq = excluded.last_commit_seq
            "#,
            params![
                decisive_fact_id,
                resolution_status,
                sqlite_usize(assertion_count, "task collection assertion count")?,
                sqlite_usize(
                    competing_metadata_count,
                    "task collection competing metadata count"
                )?,
                sqlite_usize(
                    complete_snapshot_count,
                    "task collection complete snapshot count"
                )?,
                sqlite_usize(item_document_count, "task item document count")?,
                sqlite_usize(item_count, "task collection item count")?,
                sqlite_u64(commit_seq, "commit sequence")?,
            ],
        )
        .map_err(|error| sqlite_error("write canonical task collection", error))?;

    Ok(CollectionReduction {
        resolution_status: Some(resolution_status.to_string()),
        assertion_count,
        competing_metadata_count,
        complete_snapshot_count,
        item_document_count,
        item_count,
    })
}

fn reduce_task(
    transaction: &Transaction<'_>,
    task_key: &[u8],
    commit_seq: u64,
) -> Result<TaskReduction, EngineError> {
    let mut statement = transaction
        .prepare(
            r#"
            SELECT fact_id, item_digest, task_status, native_status
            FROM task_item_assertions
            WHERE task_key = ?1
            ORDER BY fact_id
            "#,
        )
        .map_err(|error| sqlite_error("prepare task item reduction", error))?;
    let assertions = statement
        .query_map([task_key], |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                row.get::<_, Vec<u8>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .map_err(|error| sqlite_error("read task item assertions", error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| sqlite_error("collect task item assertions", error))?;
    let Some((decisive_fact_id, _, task_status, native_status)) = assertions.first() else {
        transaction
            .execute(
                "DELETE FROM canonical_tasks WHERE task_key = ?1",
                [task_key],
            )
            .map_err(|error| sqlite_error("remove absent canonical task", error))?;
        return Ok(TaskReduction {
            resolution_status: None,
            task_status: None,
            native_status: None,
            assertion_count: 0,
            competing_item_count: 0,
        });
    };
    let assertion_count = assertions.len();
    let competing_item_count = assertions
        .iter()
        .map(|(_, digest, _, _)| digest)
        .collect::<BTreeSet<_>>()
        .len()
        .saturating_sub(1);
    let resolution_status = if competing_item_count > 0 {
        "conflicting"
    } else {
        "resolved"
    };

    transaction
        .execute(
            r#"
            INSERT INTO canonical_tasks (
                task_key, collection_key, item_ordinal, native_task_id,
                subject, description, active_form, native_owner,
                task_status, native_status, blocks_json, blocked_by_json,
                resolution_status, decisive_fact_id, assertion_count,
                competing_item_count, last_commit_seq
            )
            SELECT task_key, collection_key, item_ordinal, native_task_id,
                   subject, description, active_form, native_owner,
                   task_status, native_status, blocks_json, blocked_by_json,
                   ?2, fact_id, ?3, ?4, ?5
            FROM task_item_assertions
            WHERE fact_id = ?1 AND task_key = ?6
            ON CONFLICT(task_key) DO UPDATE SET
                collection_key = excluded.collection_key,
                item_ordinal = excluded.item_ordinal,
                native_task_id = excluded.native_task_id,
                subject = excluded.subject,
                description = excluded.description,
                active_form = excluded.active_form,
                native_owner = excluded.native_owner,
                task_status = excluded.task_status,
                native_status = excluded.native_status,
                blocks_json = excluded.blocks_json,
                blocked_by_json = excluded.blocked_by_json,
                resolution_status = excluded.resolution_status,
                decisive_fact_id = excluded.decisive_fact_id,
                assertion_count = excluded.assertion_count,
                competing_item_count = excluded.competing_item_count,
                last_commit_seq = excluded.last_commit_seq
            "#,
            params![
                decisive_fact_id,
                resolution_status,
                sqlite_usize(assertion_count, "task assertion count")?,
                sqlite_usize(competing_item_count, "task competing item count")?,
                sqlite_u64(commit_seq, "commit sequence")?,
                task_key,
            ],
        )
        .map_err(|error| sqlite_error("write canonical task", error))?;

    Ok(TaskReduction {
        resolution_status: Some(resolution_status.to_string()),
        task_status: Some(task_status.clone()),
        native_status: Some(native_status.clone()),
        assertion_count,
        competing_item_count,
    })
}

fn reduce_plan(
    transaction: &Transaction<'_>,
    plan_key: &[u8],
    commit_seq: u64,
) -> Result<PlanReduction, EngineError> {
    let mut statement = transaction
        .prepare(
            "SELECT fact_id, plan_digest FROM plan_assertions WHERE plan_key = ?1 ORDER BY fact_id",
        )
        .map_err(|error| sqlite_error("prepare plan reduction", error))?;
    let assertions = statement
        .query_map([plan_key], |row| {
            Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?))
        })
        .map_err(|error| sqlite_error("read plan assertions", error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| sqlite_error("collect plan assertions", error))?;
    let Some((decisive_fact_id, _)) = assertions.first() else {
        transaction
            .execute(
                "DELETE FROM canonical_plans WHERE plan_key = ?1",
                [plan_key],
            )
            .map_err(|error| sqlite_error("remove absent canonical plan", error))?;
        return Ok(PlanReduction {
            resolution_status: None,
            assertion_count: 0,
            competing_plan_count: 0,
        });
    };
    let assertion_count = assertions.len();
    let competing_plan_count = assertions
        .iter()
        .map(|(_, digest)| digest)
        .collect::<BTreeSet<_>>()
        .len()
        .saturating_sub(1);
    let resolution_status = if competing_plan_count > 0 {
        "conflicting"
    } else {
        "resolved"
    };

    transaction
        .execute(
            r#"
            INSERT INTO canonical_plans (
                plan_key, native_plan_id, title, content, size_bytes,
                source_time, source_time_quality, resolution_status,
                decisive_fact_id, assertion_count, competing_plan_count,
                last_commit_seq
            )
            SELECT plan_key, native_plan_id, title, content, size_bytes,
                   source_time, source_time_quality, ?2, fact_id, ?3, ?4, ?5
            FROM plan_assertions WHERE fact_id = ?1
            ON CONFLICT(plan_key) DO UPDATE SET
                native_plan_id = excluded.native_plan_id,
                title = excluded.title,
                content = excluded.content,
                size_bytes = excluded.size_bytes,
                source_time = excluded.source_time,
                source_time_quality = excluded.source_time_quality,
                resolution_status = excluded.resolution_status,
                decisive_fact_id = excluded.decisive_fact_id,
                assertion_count = excluded.assertion_count,
                competing_plan_count = excluded.competing_plan_count,
                last_commit_seq = excluded.last_commit_seq
            "#,
            params![
                decisive_fact_id,
                resolution_status,
                sqlite_usize(assertion_count, "plan assertion count")?,
                sqlite_usize(competing_plan_count, "plan competing assertion count")?,
                sqlite_u64(commit_seq, "commit sequence")?,
            ],
        )
        .map_err(|error| sqlite_error("write canonical plan", error))?;

    Ok(PlanReduction {
        resolution_status: Some(resolution_status.to_string()),
        assertion_count,
        competing_plan_count,
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

fn collection_change(
    collection_key: &[u8],
    reduction: &CollectionReduction,
) -> Result<ChangeEntry, EngineError> {
    change(
        "runtime.task-collection.changed",
        collection_key,
        reduction.resolution_status.is_some(),
        &serde_json::json!({
            "resolution_status": reduction.resolution_status,
            "assertion_count": reduction.assertion_count,
            "competing_metadata_count": reduction.competing_metadata_count,
            "complete_snapshot_count": reduction.complete_snapshot_count,
            "item_document_count": reduction.item_document_count,
            "item_count": reduction.item_count,
        }),
        "serialize task collection change",
    )
}

fn collection_conflict_change(
    collection_key: &[u8],
    reduction: &CollectionReduction,
) -> Result<ChangeEntry, EngineError> {
    conflict_change(
        "diagnostic.runtime.task-collection-conflict",
        collection_key,
        reduction.competing_metadata_count,
        "serialize task collection conflict",
    )
}

fn task_change(task_key: &[u8], reduction: &TaskReduction) -> Result<ChangeEntry, EngineError> {
    change(
        "runtime.task.changed",
        task_key,
        reduction.resolution_status.is_some(),
        &serde_json::json!({
            "resolution_status": reduction.resolution_status,
            "task_status": reduction.task_status,
            "native_status": reduction.native_status,
            "assertion_count": reduction.assertion_count,
            "competing_item_count": reduction.competing_item_count,
        }),
        "serialize task change",
    )
}

fn task_conflict_change(
    task_key: &[u8],
    reduction: &TaskReduction,
) -> Result<ChangeEntry, EngineError> {
    conflict_change(
        "diagnostic.runtime.task-conflict",
        task_key,
        reduction.competing_item_count,
        "serialize task conflict",
    )
}

fn plan_change(plan_key: &[u8], reduction: &PlanReduction) -> Result<ChangeEntry, EngineError> {
    change(
        "runtime.plan.changed",
        plan_key,
        reduction.resolution_status.is_some(),
        &serde_json::json!({
            "resolution_status": reduction.resolution_status,
            "assertion_count": reduction.assertion_count,
            "competing_plan_count": reduction.competing_plan_count,
        }),
        "serialize plan change",
    )
}

fn plan_conflict_change(
    plan_key: &[u8],
    reduction: &PlanReduction,
) -> Result<ChangeEntry, EngineError> {
    conflict_change(
        "diagnostic.runtime.plan-conflict",
        plan_key,
        reduction.competing_plan_count,
        "serialize plan conflict",
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

fn conflict_change(
    topic: &str,
    entity_key: &[u8],
    competing_count: usize,
    operation: &'static str,
) -> Result<ChangeEntry, EngineError> {
    let conflicting = competing_count > 0;
    change(
        topic,
        entity_key,
        conflicting,
        &serde_json::json!({
            "conflicting": conflicting,
            "competing_count": competing_count,
        }),
        operation,
    )
}

fn collection_kind(kind: &TaskCollectionKind) -> (&'static str, &str) {
    match kind {
        TaskCollectionKind::TodoList => ("todo_list", "todo_list"),
        TaskCollectionKind::NativeTaskList => ("native_task_list", "native_task_list"),
        TaskCollectionKind::Other(native) => ("other", native),
    }
}

fn coverage(coverage: TaskSnapshotCoverage) -> &'static str {
    match coverage {
        TaskSnapshotCoverage::Complete => "complete",
        TaskSnapshotCoverage::ItemDocument => "item_document",
    }
}

fn task_status(status: &TaskStatus) -> (&'static str, &str) {
    match status {
        TaskStatus::Pending => ("pending", "pending"),
        TaskStatus::InProgress => ("in_progress", "in_progress"),
        TaskStatus::Completed => ("completed", "completed"),
        TaskStatus::Other(native) => ("other", native),
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

fn timestamp_value(timestamp: Option<&QualifiedTimestamp>) -> Option<&str> {
    timestamp.map(|timestamp| timestamp.value.as_str())
}

fn timestamp_quality(timestamp: Option<&QualifiedTimestamp>) -> Option<&'static str> {
    timestamp.map(|timestamp| match timestamp.quality {
        crate::adapter::TimestampQuality::NativeExact => "native_exact",
        crate::adapter::TimestampQuality::NativeApproximate => "native_approximate",
        crate::adapter::TimestampQuality::FileMetadataFallback => "file_metadata_fallback",
        crate::adapter::TimestampQuality::Derived => "derived",
    })
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
