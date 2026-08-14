//! Replaceable team configuration and inbox snapshot projections.
//!
//! Team membership is configuration evidence only. This projector deliberately
//! does not write run evidence, interpret tmux pane ids, or infer liveness from
//! inbox traffic. Inbox snapshots also stand alone when their team config is
//! missing so partially-written and orphaned native state remains observable.

use std::collections::BTreeSet;

use rusqlite::{params, OptionalExtension, Transaction};

use crate::adapter::{
    Fact, FactBatch, FactEnvelope, QualifiedTimestamp, TeamInboxMessageSnapshot,
    TeamInboxSnapshotFact, TeamMemberSnapshot, TeamSnapshotFact, TimestampQuality,
};

use super::commit::{ChangeEntry, ProjectionCommitContext};
use super::EngineError;

const CHANGE_SCHEMA_VERSION: u32 = 1;
type DigestRow = (Vec<u8>, Vec<u8>);

#[derive(Debug, PartialEq, Eq)]
struct SnapshotReduction {
    status: Option<String>,
    assertion_count: usize,
    competing_snapshot_count: usize,
}

pub(super) fn apply_team_snapshots(
    transaction: &Transaction<'_>,
    context: &ProjectionCommitContext,
    batch: &FactBatch,
) -> Result<Vec<ChangeEntry>, EngineError> {
    let object_id = sqlite_u64(context.source_object_id, "source object id")?;
    let has_team_fact = batch.facts().iter().any(|envelope| {
        matches!(
            envelope.value,
            Fact::TeamSnapshot(_) | Fact::TeamInboxSnapshot(_)
        )
    });
    if context.skip_unowned_replace_document(has_team_fact) {
        return Ok(Vec::new());
    }
    let mut affected_teams = source_object_keys(
        transaction,
        "SELECT DISTINCT team_key FROM team_snapshot_assertions WHERE source_object_id = ?1",
        object_id,
        "read replaced team snapshots",
    )?;
    let mut affected_inboxes = source_object_keys(
        transaction,
        "SELECT DISTINCT inbox_key FROM team_inbox_snapshot_assertions WHERE source_object_id = ?1",
        object_id,
        "read replaced team inbox snapshots",
    )?;

    if !has_team_fact && affected_teams.is_empty() && affected_inboxes.is_empty() {
        return Ok(Vec::new());
    }

    let mut affected_members = children_for_parents(
        transaction,
        "SELECT member_key FROM canonical_team_members WHERE team_key = ?1",
        &affected_teams,
        "read prior canonical team members",
    )?;
    let mut affected_messages = children_for_parents(
        transaction,
        "SELECT message_key FROM canonical_team_inbox_messages WHERE inbox_key = ?1",
        &affected_inboxes,
        "read prior canonical team inbox messages",
    )?;

    // Replace-document commits own the complete current assertion set for one
    // object, including same-generation edits and empty deletion snapshots.
    transaction
        .execute(
            "DELETE FROM team_snapshot_assertions WHERE source_object_id = ?1",
            [object_id],
        )
        .map_err(|error| sqlite_error("retract replaced team snapshots", error))?;
    transaction
        .execute(
            "DELETE FROM team_inbox_snapshot_assertions WHERE source_object_id = ?1",
            [object_id],
        )
        .map_err(|error| sqlite_error("retract replaced team inbox snapshots", error))?;

    for envelope in batch.facts() {
        match &envelope.value {
            Fact::TeamSnapshot(fact) => {
                write_team_snapshot(transaction, context, envelope, fact)?;
                affected_teams.insert(fact.team.as_bytes().to_vec());
            }
            Fact::TeamInboxSnapshot(fact) => {
                write_inbox_snapshot(transaction, context, envelope, fact)?;
                affected_inboxes.insert(fact.inbox.as_bytes().to_vec());
            }
            _ => {}
        }
    }

    let mut changes = Vec::new();
    for team_key in &affected_teams {
        let reduction = reduce_team(transaction, team_key, context.commit_seq)?;
        changes.push(snapshot_change(
            "runtime.team.changed",
            team_key,
            &reduction,
        )?);
        changes.push(snapshot_conflict_change(
            "diagnostic.runtime.team-config-conflict",
            team_key,
            &reduction,
        )?);
    }
    affected_members.extend(children_for_parents(
        transaction,
        "SELECT member_key FROM canonical_team_members WHERE team_key = ?1",
        &affected_teams,
        "read current canonical team members",
    )?);
    for member_key in affected_members {
        changes.extend(child_changes(
            transaction,
            "canonical_team_members",
            "member_key",
            "membership_status",
            "competing_membership_count",
            "runtime.team-member.changed",
            "diagnostic.runtime.team-member-conflict",
            &member_key,
        )?);
    }

    for inbox_key in &affected_inboxes {
        let reduction = reduce_inbox(transaction, inbox_key, context.commit_seq)?;
        changes.push(snapshot_change(
            "runtime.team-inbox.changed",
            inbox_key,
            &reduction,
        )?);
        changes.push(snapshot_conflict_change(
            "diagnostic.runtime.team-inbox-conflict",
            inbox_key,
            &reduction,
        )?);
    }
    affected_messages.extend(children_for_parents(
        transaction,
        "SELECT message_key FROM canonical_team_inbox_messages WHERE inbox_key = ?1",
        &affected_inboxes,
        "read current canonical team inbox messages",
    )?);
    for message_key in affected_messages {
        changes.extend(child_changes(
            transaction,
            "canonical_team_inbox_messages",
            "message_key",
            "message_status",
            "competing_message_count",
            "runtime.team-inbox-message.changed",
            "diagnostic.runtime.team-inbox-message-conflict",
            &message_key,
        )?);
    }

    // Snapshot assertions are replaced even when the common source generation
    // does not change, so retire the superseded audit rows after every reducer
    // has released its foreign-key references.
    transaction
        .execute(
            r#"
            DELETE FROM fact_records
            WHERE source_object_id = ?1
              AND fact_kind IN ('team_snapshot', 'team_inbox_snapshot')
              AND last_commit_seq <> ?2
            "#,
            params![
                object_id,
                sqlite_u64(context.commit_seq, "commit sequence")?,
            ],
        )
        .map_err(|error| sqlite_error("retract replaced team snapshot facts", error))?;

    Ok(changes)
}

fn write_team_snapshot(
    transaction: &Transaction<'_>,
    context: &ProjectionCommitContext,
    envelope: &FactEnvelope,
    fact: &TeamSnapshotFact,
) -> Result<(), EngineError> {
    let snapshot_digest = digest(fact, "digest team snapshot")?;
    transaction
        .execute(
            r#"
            INSERT INTO team_snapshot_assertions (
                fact_id, team_key, native_team_id, name, description,
                created_at, created_at_quality, lead_member_key,
                native_lead_agent_id, lead_session_key,
                native_lead_session_id, snapshot_digest, source_object_id,
                source_generation, cursor_end, last_commit_seq
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                ?13, ?14, ?15, ?16
            )
            "#,
            params![
                envelope.id.as_bytes().as_slice(),
                fact.team.as_bytes(),
                fact.native_team_id,
                fact.name,
                fact.description,
                fact.created_at.value,
                timestamp_quality(&fact.created_at),
                fact.lead_member.as_ref().map(|key| key.as_bytes()),
                fact.native_lead_agent_id,
                fact.lead_session.as_bytes(),
                fact.native_lead_session_id,
                snapshot_digest.as_slice(),
                sqlite_u64(context.source_object_id, "source object id")?,
                sqlite_u64(context.generation, "source generation")?,
                envelope.provenance.cursor_end,
                sqlite_u64(context.commit_seq, "commit sequence")?,
            ],
        )
        .map_err(|error| sqlite_error("project team snapshot assertion", error))?;

    let mut member_keys = BTreeSet::new();
    for (ordinal, member) in fact.members.iter().enumerate() {
        if !member_keys.insert(member.member.as_bytes().to_vec()) {
            return Err(EngineError::InvalidCommit(
                "team snapshot contains duplicate member keys".to_string(),
            ));
        }
        write_team_member(transaction, envelope, fact, member, ordinal)?;
    }
    Ok(())
}

fn write_team_member(
    transaction: &Transaction<'_>,
    envelope: &FactEnvelope,
    team: &TeamSnapshotFact,
    member: &TeamMemberSnapshot,
    ordinal: usize,
) -> Result<(), EngineError> {
    let subscriptions = serialize(&member.subscriptions, "serialize team subscriptions")?;
    let member_digest = digest(member, "digest team member")?;
    transaction
        .execute(
            r#"
            INSERT INTO team_member_assertions (
                fact_id, member_key, team_key, member_ordinal,
                native_agent_id, native_name, agent_type, model, prompt,
                color, plan_mode_required, joined_at, joined_at_quality,
                tmux_pane_id, cwd, subscriptions_json, backend_type,
                member_digest
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                ?13, ?14, ?15, ?16, ?17, ?18
            )
            "#,
            params![
                envelope.id.as_bytes().as_slice(),
                member.member.as_bytes(),
                team.team.as_bytes(),
                sqlite_usize(ordinal, "team member ordinal")?,
                member.native_agent_id,
                member.native_name,
                member.agent_type,
                member.model,
                member.prompt,
                member.color,
                member.plan_mode_required.map(i64::from),
                member.joined_at.value,
                timestamp_quality(&member.joined_at),
                member.tmux_pane_id,
                member.cwd,
                subscriptions,
                member.backend_type,
                member_digest.as_slice(),
            ],
        )
        .map(|_| ())
        .map_err(|error| sqlite_error("project team member assertion", error))
}

fn write_inbox_snapshot(
    transaction: &Transaction<'_>,
    context: &ProjectionCommitContext,
    envelope: &FactEnvelope,
    fact: &TeamInboxSnapshotFact,
) -> Result<(), EngineError> {
    let snapshot_digest = digest(fact, "digest team inbox snapshot")?;
    transaction
        .execute(
            r#"
            INSERT INTO team_inbox_snapshot_assertions (
                fact_id, inbox_key, team_key, recipient_key, native_team_id,
                native_recipient_name, snapshot_digest, source_object_id,
                source_generation, cursor_end, last_commit_seq
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
            "#,
            params![
                envelope.id.as_bytes().as_slice(),
                fact.inbox.as_bytes(),
                fact.team.as_bytes(),
                fact.recipient.as_bytes(),
                fact.native_team_id,
                fact.native_recipient_name,
                snapshot_digest.as_slice(),
                sqlite_u64(context.source_object_id, "source object id")?,
                sqlite_u64(context.generation, "source generation")?,
                envelope.provenance.cursor_end,
                sqlite_u64(context.commit_seq, "commit sequence")?,
            ],
        )
        .map_err(|error| sqlite_error("project team inbox snapshot assertion", error))?;

    let mut message_keys = BTreeSet::new();
    for (ordinal, message) in fact.messages.iter().enumerate() {
        if !message_keys.insert(message.message.as_bytes().to_vec()) {
            return Err(EngineError::InvalidCommit(
                "team inbox snapshot contains duplicate message keys".to_string(),
            ));
        }
        write_inbox_message(transaction, envelope, fact, message, ordinal)?;
    }
    Ok(())
}

fn write_inbox_message(
    transaction: &Transaction<'_>,
    envelope: &FactEnvelope,
    inbox: &TeamInboxSnapshotFact,
    message: &TeamInboxMessageSnapshot,
    ordinal: usize,
) -> Result<(), EngineError> {
    let message_digest = digest(message, "digest team inbox message")?;
    transaction
        .execute(
            r#"
            INSERT INTO team_inbox_message_assertions (
                fact_id, message_key, inbox_key, message_ordinal, sender_key,
                native_message_id, native_kind, native_version,
                native_sender_name, text, summary, color, source_time,
                source_time_quality, read, message_digest
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                ?13, ?14, ?15, ?16
            )
            "#,
            params![
                envelope.id.as_bytes().as_slice(),
                message.message.as_bytes(),
                inbox.inbox.as_bytes(),
                sqlite_usize(ordinal, "team inbox message ordinal")?,
                message.sender.as_bytes(),
                message.native_message_id,
                message.native_kind,
                message.native_version.map(i64::from),
                message.native_sender_name,
                message.text,
                message.summary,
                message.color,
                message.source_time.value,
                timestamp_quality(&message.source_time),
                i64::from(message.read),
                message_digest.as_slice(),
            ],
        )
        .map(|_| ())
        .map_err(|error| sqlite_error("project team inbox message assertion", error))
}

fn reduce_team(
    transaction: &Transaction<'_>,
    team_key: &[u8],
    commit_seq: u64,
) -> Result<SnapshotReduction, EngineError> {
    let assertions = assertion_digests(
        transaction,
        "SELECT fact_id, snapshot_digest FROM team_snapshot_assertions WHERE team_key = ?1 ORDER BY fact_id",
        team_key,
        "read team snapshot assertions",
    )?;
    transaction
        .execute(
            "DELETE FROM canonical_team_members WHERE team_key = ?1",
            [team_key],
        )
        .map_err(|error| sqlite_error("replace canonical team members", error))?;
    let Some((decisive_fact_id, _)) = assertions.first() else {
        transaction
            .execute(
                "DELETE FROM canonical_teams WHERE team_key = ?1",
                [team_key],
            )
            .map_err(|error| sqlite_error("remove empty canonical team", error))?;
        return Ok(SnapshotReduction {
            status: None,
            assertion_count: 0,
            competing_snapshot_count: 0,
        });
    };
    let competing_snapshot_count = distinct_digest_count(&assertions).saturating_sub(1);
    let status = if competing_snapshot_count > 0 {
        "conflicting"
    } else {
        "resolved"
    };
    let assertion_count = assertions.len();
    let member_count = transaction
        .query_row(
            "SELECT COUNT(*) FROM team_member_assertions WHERE fact_id = ?1",
            [decisive_fact_id],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|error| sqlite_error("count decisive team members", error))?;
    transaction
        .execute(
            r#"
            INSERT INTO canonical_teams (
                team_key, native_team_id, name, description, created_at,
                created_at_quality, lead_member_key, native_lead_agent_id,
                lead_session_key, native_lead_session_id, config_status,
                decisive_fact_id, assertion_count, competing_snapshot_count,
                member_count, last_commit_seq
            )
            SELECT team_key, native_team_id, name, description, created_at,
                   created_at_quality, lead_member_key, native_lead_agent_id,
                   lead_session_key, native_lead_session_id, ?2, fact_id,
                   ?3, ?4, ?5, ?6
            FROM team_snapshot_assertions WHERE fact_id = ?1
            ON CONFLICT(team_key) DO UPDATE SET
                native_team_id = excluded.native_team_id,
                name = excluded.name,
                description = excluded.description,
                created_at = excluded.created_at,
                created_at_quality = excluded.created_at_quality,
                lead_member_key = excluded.lead_member_key,
                native_lead_agent_id = excluded.native_lead_agent_id,
                lead_session_key = excluded.lead_session_key,
                native_lead_session_id = excluded.native_lead_session_id,
                config_status = excluded.config_status,
                decisive_fact_id = excluded.decisive_fact_id,
                assertion_count = excluded.assertion_count,
                competing_snapshot_count = excluded.competing_snapshot_count,
                member_count = excluded.member_count,
                last_commit_seq = excluded.last_commit_seq
            "#,
            params![
                decisive_fact_id,
                status,
                sqlite_usize(assertion_count, "team assertion count")?,
                sqlite_usize(competing_snapshot_count, "team conflict count")?,
                member_count,
                sqlite_u64(commit_seq, "commit sequence")?,
            ],
        )
        .map_err(|error| sqlite_error("write canonical team", error))?;

    let members = keyed_digests(
        transaction,
        "SELECT member_key, member_digest FROM team_member_assertions WHERE fact_id = ?1 ORDER BY member_ordinal",
        decisive_fact_id,
        "read decisive team members",
    )?;
    for (member_key, _) in members {
        let competing = child_competing_count(
            transaction,
            "team_member_assertions",
            "team_key",
            "member_key",
            "member_digest",
            team_key,
            &member_key,
            assertion_count,
            "reduce team member conflict",
        )?;
        let membership_status = if competing > 0 {
            "conflicting"
        } else {
            "resolved"
        };
        transaction
            .execute(
                r#"
                INSERT INTO canonical_team_members (
                    member_key, team_key, member_ordinal, native_agent_id,
                    native_name, agent_type, model, prompt, color,
                    plan_mode_required, joined_at, joined_at_quality,
                    tmux_pane_id, cwd, subscriptions_json, backend_type,
                    membership_status, decisive_fact_id, assertion_count,
                    competing_membership_count, last_commit_seq
                )
                SELECT member_key, team_key, member_ordinal, native_agent_id,
                       native_name, agent_type, model, prompt, color,
                       plan_mode_required, joined_at, joined_at_quality,
                       tmux_pane_id, cwd, subscriptions_json, backend_type,
                       ?3, fact_id, ?4, ?5, ?6
                FROM team_member_assertions
                WHERE fact_id = ?1 AND member_key = ?2
                "#,
                params![
                    decisive_fact_id,
                    member_key,
                    membership_status,
                    sqlite_usize(assertion_count, "team member assertion count")?,
                    sqlite_usize(competing, "team member conflict count")?,
                    sqlite_u64(commit_seq, "commit sequence")?,
                ],
            )
            .map_err(|error| sqlite_error("write canonical team member", error))?;
    }

    Ok(SnapshotReduction {
        status: Some(status.to_string()),
        assertion_count,
        competing_snapshot_count,
    })
}

fn reduce_inbox(
    transaction: &Transaction<'_>,
    inbox_key: &[u8],
    commit_seq: u64,
) -> Result<SnapshotReduction, EngineError> {
    let assertions = assertion_digests(
        transaction,
        "SELECT fact_id, snapshot_digest FROM team_inbox_snapshot_assertions WHERE inbox_key = ?1 ORDER BY fact_id",
        inbox_key,
        "read team inbox snapshot assertions",
    )?;
    transaction
        .execute(
            "DELETE FROM canonical_team_inbox_messages WHERE inbox_key = ?1",
            [inbox_key],
        )
        .map_err(|error| sqlite_error("replace canonical team inbox messages", error))?;
    let Some((decisive_fact_id, _)) = assertions.first() else {
        transaction
            .execute(
                "DELETE FROM canonical_team_inboxes WHERE inbox_key = ?1",
                [inbox_key],
            )
            .map_err(|error| sqlite_error("remove empty canonical team inbox", error))?;
        return Ok(SnapshotReduction {
            status: None,
            assertion_count: 0,
            competing_snapshot_count: 0,
        });
    };
    let competing_snapshot_count = distinct_digest_count(&assertions).saturating_sub(1);
    let status = if competing_snapshot_count > 0 {
        "conflicting"
    } else {
        "resolved"
    };
    let assertion_count = assertions.len();
    let message_count = transaction
        .query_row(
            "SELECT COUNT(*) FROM team_inbox_message_assertions WHERE fact_id = ?1",
            [decisive_fact_id],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|error| sqlite_error("count decisive team inbox messages", error))?;
    transaction
        .execute(
            r#"
            INSERT INTO canonical_team_inboxes (
                inbox_key, team_key, recipient_key, native_team_id,
                native_recipient_name, inbox_status, decisive_fact_id,
                assertion_count, competing_snapshot_count, message_count,
                last_commit_seq
            )
            SELECT inbox_key, team_key, recipient_key, native_team_id,
                   native_recipient_name, ?2, fact_id, ?3, ?4, ?5, ?6
            FROM team_inbox_snapshot_assertions WHERE fact_id = ?1
            ON CONFLICT(inbox_key) DO UPDATE SET
                team_key = excluded.team_key,
                recipient_key = excluded.recipient_key,
                native_team_id = excluded.native_team_id,
                native_recipient_name = excluded.native_recipient_name,
                inbox_status = excluded.inbox_status,
                decisive_fact_id = excluded.decisive_fact_id,
                assertion_count = excluded.assertion_count,
                competing_snapshot_count = excluded.competing_snapshot_count,
                message_count = excluded.message_count,
                last_commit_seq = excluded.last_commit_seq
            "#,
            params![
                decisive_fact_id,
                status,
                sqlite_usize(assertion_count, "team inbox assertion count")?,
                sqlite_usize(competing_snapshot_count, "team inbox conflict count")?,
                message_count,
                sqlite_u64(commit_seq, "commit sequence")?,
            ],
        )
        .map_err(|error| sqlite_error("write canonical team inbox", error))?;

    let messages = keyed_digests(
        transaction,
        "SELECT message_key, message_digest FROM team_inbox_message_assertions WHERE fact_id = ?1 ORDER BY message_ordinal",
        decisive_fact_id,
        "read decisive team inbox messages",
    )?;
    for (message_key, _) in messages {
        let competing = child_competing_count(
            transaction,
            "team_inbox_message_assertions",
            "inbox_key",
            "message_key",
            "message_digest",
            inbox_key,
            &message_key,
            assertion_count,
            "reduce team inbox message conflict",
        )?;
        let message_status = if competing > 0 {
            "conflicting"
        } else {
            "resolved"
        };
        transaction
            .execute(
                r#"
                INSERT INTO canonical_team_inbox_messages (
                    message_key, inbox_key, message_ordinal, sender_key,
                    native_message_id, native_kind, native_version,
                    native_sender_name, text, summary, color, source_time,
                    source_time_quality, read, message_status,
                    decisive_fact_id, assertion_count,
                    competing_message_count, last_commit_seq
                )
                SELECT message_key, inbox_key, message_ordinal, sender_key,
                       native_message_id, native_kind, native_version,
                       native_sender_name, text, summary, color, source_time,
                       source_time_quality, read, ?3, fact_id, ?4, ?5, ?6
                FROM team_inbox_message_assertions
                WHERE fact_id = ?1 AND message_key = ?2
                "#,
                params![
                    decisive_fact_id,
                    message_key,
                    message_status,
                    sqlite_usize(assertion_count, "team inbox message assertion count")?,
                    sqlite_usize(competing, "team inbox message conflict count")?,
                    sqlite_u64(commit_seq, "commit sequence")?,
                ],
            )
            .map_err(|error| sqlite_error("write canonical team inbox message", error))?;
    }

    Ok(SnapshotReduction {
        status: Some(status.to_string()),
        assertion_count,
        competing_snapshot_count,
    })
}

fn assertion_digests(
    transaction: &Transaction<'_>,
    sql: &str,
    entity_key: &[u8],
    operation: &'static str,
) -> Result<Vec<DigestRow>, EngineError> {
    let mut statement = transaction
        .prepare(sql)
        .map_err(|error| sqlite_error(operation, error))?;
    let rows = statement
        .query_map([entity_key], |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(|error| sqlite_error(operation, error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| sqlite_error(operation, error))?;
    Ok(rows)
}

fn keyed_digests(
    transaction: &Transaction<'_>,
    sql: &str,
    fact_id: &[u8],
    operation: &'static str,
) -> Result<Vec<DigestRow>, EngineError> {
    assertion_digests(transaction, sql, fact_id, operation)
}

fn distinct_digest_count(assertions: &[DigestRow]) -> usize {
    assertions
        .iter()
        .map(|(_, digest)| digest.clone())
        .collect::<BTreeSet<_>>()
        .len()
}

#[allow(clippy::too_many_arguments)]
fn child_competing_count(
    transaction: &Transaction<'_>,
    table: &'static str,
    parent_column: &'static str,
    key_column: &'static str,
    digest_column: &'static str,
    parent_key: &[u8],
    child_key: &[u8],
    snapshot_count: usize,
    operation: &'static str,
) -> Result<usize, EngineError> {
    let sql = format!(
        "SELECT COUNT(*), COUNT(DISTINCT {digest_column}) FROM {table} WHERE {parent_column} = ?1 AND {key_column} = ?2"
    );
    let (appearance_count, distinct_count) = transaction
        .query_row(&sql, params![parent_key, child_key], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(|error| sqlite_error(operation, error))?;
    let appearance_count = usize::try_from(appearance_count).map_err(|_| {
        EngineError::InvalidCommit(format!("{operation}: negative appearance count"))
    })?;
    let distinct_count = usize::try_from(distinct_count)
        .map_err(|_| EngineError::InvalidCommit(format!("{operation}: negative distinct count")))?;
    Ok(distinct_count
        .saturating_sub(1)
        .saturating_add(usize::from(appearance_count < snapshot_count)))
}

fn source_object_keys(
    transaction: &Transaction<'_>,
    sql: &str,
    source_object_id: i64,
    operation: &'static str,
) -> Result<BTreeSet<Vec<u8>>, EngineError> {
    let mut statement = transaction
        .prepare(sql)
        .map_err(|error| sqlite_error(operation, error))?;
    let keys = statement
        .query_map([source_object_id], |row| row.get::<_, Vec<u8>>(0))
        .map_err(|error| sqlite_error(operation, error))?
        .collect::<Result<BTreeSet<_>, _>>()
        .map_err(|error| sqlite_error(operation, error))?;
    Ok(keys)
}

fn children_for_parents(
    transaction: &Transaction<'_>,
    sql: &str,
    parent_keys: &BTreeSet<Vec<u8>>,
    operation: &'static str,
) -> Result<BTreeSet<Vec<u8>>, EngineError> {
    let mut children = BTreeSet::new();
    let mut statement = transaction
        .prepare(sql)
        .map_err(|error| sqlite_error(operation, error))?;
    for parent_key in parent_keys {
        children.extend(
            statement
                .query_map([parent_key], |row| row.get::<_, Vec<u8>>(0))
                .map_err(|error| sqlite_error(operation, error))?
                .collect::<Result<BTreeSet<_>, _>>()
                .map_err(|error| sqlite_error(operation, error))?,
        );
    }
    Ok(children)
}

fn snapshot_change(
    topic: &'static str,
    entity_key: &[u8],
    reduction: &SnapshotReduction,
) -> Result<ChangeEntry, EngineError> {
    let payload = serialize(
        &serde_json::json!({
            "status": reduction.status,
            "assertion_count": reduction.assertion_count,
            "competing_snapshot_count": reduction.competing_snapshot_count,
        }),
        "serialize team snapshot change",
    )?;
    Ok(ChangeEntry {
        topic: topic.to_string(),
        schema_version: CHANGE_SCHEMA_VERSION,
        entity_key: entity_key.to_vec(),
        operation: if reduction.status.is_some() {
            "upsert"
        } else {
            "delete"
        }
        .to_string(),
        payload,
    })
}

fn snapshot_conflict_change(
    topic: &'static str,
    entity_key: &[u8],
    reduction: &SnapshotReduction,
) -> Result<ChangeEntry, EngineError> {
    let conflicting = reduction.competing_snapshot_count > 0;
    let payload = serialize(
        &serde_json::json!({
            "conflicting": conflicting,
            "competing_snapshot_count": reduction.competing_snapshot_count,
        }),
        "serialize team snapshot conflict change",
    )?;
    Ok(ChangeEntry {
        topic: topic.to_string(),
        schema_version: CHANGE_SCHEMA_VERSION,
        entity_key: entity_key.to_vec(),
        operation: if conflicting { "upsert" } else { "delete" }.to_string(),
        payload,
    })
}

#[allow(clippy::too_many_arguments)]
fn child_changes(
    transaction: &Transaction<'_>,
    table: &'static str,
    key_column: &'static str,
    status_column: &'static str,
    conflict_count_column: &'static str,
    topic: &'static str,
    conflict_topic: &'static str,
    entity_key: &[u8],
) -> Result<[ChangeEntry; 2], EngineError> {
    let sql = format!(
        "SELECT {status_column}, {conflict_count_column} FROM {table} WHERE {key_column} = ?1"
    );
    let state = transaction
        .query_row(&sql, [entity_key], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .optional()
        .map_err(|error| sqlite_error("read team child change", error))?;
    let status = state.as_ref().map(|(status, _)| status.as_str());
    let competing_count = state.as_ref().map(|(_, count)| *count).unwrap_or(0);
    let conflicting = status == Some("conflicting");
    let change_payload = serialize(
        &serde_json::json!({
            "status": status,
            "competing_count": competing_count,
        }),
        "serialize team child change",
    )?;
    let conflict_payload = serialize(
        &serde_json::json!({
            "conflicting": conflicting,
            "competing_count": competing_count,
        }),
        "serialize team child conflict change",
    )?;
    Ok([
        ChangeEntry {
            topic: topic.to_string(),
            schema_version: CHANGE_SCHEMA_VERSION,
            entity_key: entity_key.to_vec(),
            operation: if state.is_some() { "upsert" } else { "delete" }.to_string(),
            payload: change_payload,
        },
        ChangeEntry {
            topic: conflict_topic.to_string(),
            schema_version: CHANGE_SCHEMA_VERSION,
            entity_key: entity_key.to_vec(),
            operation: if conflicting { "upsert" } else { "delete" }.to_string(),
            payload: conflict_payload,
        },
    ])
}

fn digest<T: serde::Serialize>(
    value: &T,
    operation: &'static str,
) -> Result<[u8; 32], EngineError> {
    let encoded = serialize(value, operation)?;
    Ok(*blake3::hash(&encoded).as_bytes())
}

fn serialize<T: serde::Serialize>(
    value: &T,
    operation: &'static str,
) -> Result<Vec<u8>, EngineError> {
    serde_json::to_vec(value)
        .map_err(|error| EngineError::InvalidCommit(format!("{operation}: {error}")))
}

fn timestamp_quality(timestamp: &QualifiedTimestamp) -> &'static str {
    match timestamp.quality {
        TimestampQuality::NativeExact => "native_exact",
        TimestampQuality::NativeApproximate => "native_approximate",
        TimestampQuality::FileMetadataFallback => "file_metadata_fallback",
        TimestampQuality::Derived => "derived",
    }
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
