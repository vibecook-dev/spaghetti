//! Redacted interpretation-settings projection and scope reduction.
//!
//! Settings documents are configuration evidence, not transcript or runtime
//! evidence. Each document is independently replaceable. The effective view
//! applies native scalar precedence, stable array union, keyed plugin
//! overrides, and additive hook-event metadata without storing sensitive
//! values or executable hook bodies.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{params, Transaction};

use crate::adapter::{
    Fact, FactBatch, FactEnvelope, HookEventSummary, InterpretationSettingsDocumentStatus,
    InterpretationSettingsFact, InterpretationSettingsLayer, InterpretationSettingsSnapshot,
};

use super::commit::{ChangeEntry, ProjectionCommitContext};
use super::EngineError;

const CHANGE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, PartialEq, Eq)]
struct DocumentReduction {
    scope_key: Option<Vec<u8>>,
    document_status: Option<String>,
    resolution_status: Option<String>,
    assertion_count: usize,
    competing_settings_count: usize,
}

#[derive(Debug)]
struct CanonicalLayer {
    health: &'static str,
    settings: Option<InterpretationSettingsSnapshot>,
    decisive_fact_id: Option<Vec<u8>>,
    assertion_count: usize,
    document_count: usize,
}

#[derive(Debug, PartialEq, Eq)]
struct EffectiveReduction {
    present: bool,
    global_document_status: String,
    local_document_status: String,
    resolution_status: Option<String>,
    document_count: usize,
    assertion_count: usize,
}

pub(super) fn apply_interpretation_settings_facts(
    transaction: &Transaction<'_>,
    context: &ProjectionCommitContext,
    batch: &FactBatch,
) -> Result<Vec<ChangeEntry>, EngineError> {
    let object_id = sqlite_u64(context.source_object_id, "source object id")?;
    let mut affected_documents = source_object_keys(transaction, object_id)?;
    let mut affected_scopes = scopes_for_documents(transaction, &affected_documents)?;

    // One source object is one complete root settings document. A valid edit,
    // malformed replacement, or confirmed deletion all replace its assertion.
    transaction
        .execute(
            "DELETE FROM interpretation_settings_assertions WHERE source_object_id = ?1",
            [object_id],
        )
        .map_err(|error| sqlite_error("replace interpretation settings assertion", error))?;

    for envelope in batch.facts() {
        let Fact::InterpretationSettings(fact) = &envelope.value else {
            continue;
        };
        validate_fact(fact)?;
        write_assertion(transaction, context, envelope, fact)?;
        affected_documents.insert(fact.document.as_bytes().to_vec());
        affected_scopes.insert(fact.scope.as_bytes().to_vec());
    }

    let mut changes = Vec::new();
    for document_key in affected_documents {
        let reduction = reduce_document(transaction, &document_key, context.commit_seq)?;
        if let Some(scope_key) = &reduction.scope_key {
            affected_scopes.insert(scope_key.clone());
        }
        changes.push(document_change(&document_key, &reduction)?);
        changes.push(document_conflict_change(&document_key, &reduction)?);
    }

    for scope_key in affected_scopes {
        let reduction = reduce_effective_scope(transaction, &scope_key, context.commit_seq)?;
        changes.push(effective_change(&scope_key, &reduction)?);
        changes.push(settings_health_change(&scope_key, &reduction)?);
    }

    transaction
        .execute(
            r#"
            DELETE FROM fact_records
            WHERE source_object_id = ?1
              AND fact_kind = 'interpretation_settings'
              AND last_commit_seq <> ?2
            "#,
            params![
                object_id,
                sqlite_u64(context.commit_seq, "commit sequence")?,
            ],
        )
        .map_err(|error| sqlite_error("retract replaced interpretation settings facts", error))?;

    Ok(changes)
}

fn validate_fact(fact: &InterpretationSettingsFact) -> Result<(), EngineError> {
    match fact.document_status {
        InterpretationSettingsDocumentStatus::Valid
            if fact.settings.is_some() && fact.error_code.is_none() =>
        {
            Ok(())
        }
        InterpretationSettingsDocumentStatus::Invalid
            if fact.settings.is_none() && fact.error_code.is_some() =>
        {
            Ok(())
        }
        _ => Err(EngineError::InvalidCommit(
            "interpretation settings fact has inconsistent document status".to_string(),
        )),
    }
}

fn write_assertion(
    transaction: &Transaction<'_>,
    context: &ProjectionCommitContext,
    envelope: &FactEnvelope,
    fact: &InterpretationSettingsFact,
) -> Result<(), EngineError> {
    let settings_json = fact
        .settings
        .as_ref()
        .map(|settings| serialize(settings, "serialize interpretation settings"))
        .transpose()?;
    let digest_input = serde_json::json!({
        "fact": fact,
        "invalid_payload_hash": if fact.document_status == InterpretationSettingsDocumentStatus::Invalid {
            Some(envelope.provenance.record_hash)
        } else {
            None
        },
    });
    let settings_digest = digest(&digest_input, "digest interpretation settings")?;
    transaction
        .execute(
            r#"
            INSERT INTO interpretation_settings_assertions (
                fact_id, document_key, scope_key, layer,
                native_document_path, document_status, settings_json,
                error_code, size_bytes, settings_digest, source_object_id,
                source_generation, cursor_end, last_commit_seq
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                ?13, ?14
            )
            "#,
            params![
                envelope.id.as_bytes().as_slice(),
                fact.document.as_bytes(),
                fact.scope.as_bytes(),
                layer(fact.layer),
                fact.native_document_path,
                document_status(fact.document_status),
                settings_json,
                fact.error_code,
                sqlite_u64(fact.size_bytes, "interpretation settings size")?,
                settings_digest.as_slice(),
                sqlite_u64(context.source_object_id, "source object id")?,
                sqlite_u64(context.generation, "source generation")?,
                envelope.provenance.cursor_end,
                sqlite_u64(context.commit_seq, "commit sequence")?,
            ],
        )
        .map_err(|error| sqlite_error("write interpretation settings assertion", error))?;
    Ok(())
}

fn reduce_document(
    transaction: &Transaction<'_>,
    document_key: &[u8],
    commit_seq: u64,
) -> Result<DocumentReduction, EngineError> {
    let mut statement = transaction
        .prepare(
            r#"
            SELECT fact_id, scope_key, document_status, settings_digest
            FROM interpretation_settings_assertions
            WHERE document_key = ?1
            ORDER BY fact_id
            "#,
        )
        .map_err(|error| sqlite_error("prepare interpretation settings reduction", error))?;
    let assertions = statement
        .query_map([document_key], |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                row.get::<_, Vec<u8>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Vec<u8>>(3)?,
            ))
        })
        .map_err(|error| sqlite_error("read interpretation settings assertions", error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| sqlite_error("collect interpretation settings assertions", error))?;

    let Some((decisive_fact_id, scope_key, decisive_status, _)) = assertions.first() else {
        transaction
            .execute(
                "DELETE FROM canonical_interpretation_settings_documents WHERE document_key = ?1",
                [document_key],
            )
            .map_err(|error| sqlite_error("remove absent interpretation settings", error))?;
        return Ok(DocumentReduction {
            scope_key: None,
            document_status: None,
            resolution_status: None,
            assertion_count: 0,
            competing_settings_count: 0,
        });
    };

    if assertions
        .iter()
        .any(|(_, candidate_scope, _, _)| candidate_scope != scope_key)
    {
        return Err(EngineError::InvalidCommit(
            "one interpretation settings document asserted multiple scope keys".to_string(),
        ));
    }
    let assertion_count = assertions.len();
    let competing_settings_count = assertions
        .iter()
        .map(|(_, _, _, digest)| digest)
        .collect::<BTreeSet<_>>()
        .len()
        .saturating_sub(1);
    let resolution_status = if competing_settings_count > 0 {
        "conflicting"
    } else {
        "resolved"
    };

    transaction
        .execute(
            r#"
            INSERT INTO canonical_interpretation_settings_documents (
                document_key, scope_key, layer, native_document_path,
                document_status, settings_json, error_code, size_bytes,
                resolution_status, decisive_fact_id, assertion_count,
                competing_settings_count, last_commit_seq
            )
            SELECT document_key, scope_key, layer, native_document_path,
                   document_status, settings_json, error_code, size_bytes,
                   ?2, fact_id, ?3, ?4, ?5
            FROM interpretation_settings_assertions WHERE fact_id = ?1
            ON CONFLICT(document_key) DO UPDATE SET
                scope_key = excluded.scope_key,
                layer = excluded.layer,
                native_document_path = excluded.native_document_path,
                document_status = excluded.document_status,
                settings_json = excluded.settings_json,
                error_code = excluded.error_code,
                size_bytes = excluded.size_bytes,
                resolution_status = excluded.resolution_status,
                decisive_fact_id = excluded.decisive_fact_id,
                assertion_count = excluded.assertion_count,
                competing_settings_count = excluded.competing_settings_count,
                last_commit_seq = excluded.last_commit_seq
            "#,
            params![
                decisive_fact_id,
                resolution_status,
                sqlite_usize(assertion_count, "interpretation settings assertion count")?,
                sqlite_usize(
                    competing_settings_count,
                    "interpretation settings competing count",
                )?,
                sqlite_u64(commit_seq, "commit sequence")?,
            ],
        )
        .map_err(|error| sqlite_error("write canonical interpretation settings", error))?;

    Ok(DocumentReduction {
        scope_key: Some(scope_key.clone()),
        document_status: Some(decisive_status.clone()),
        resolution_status: Some(resolution_status.to_string()),
        assertion_count,
        competing_settings_count,
    })
}

fn reduce_effective_scope(
    transaction: &Transaction<'_>,
    scope_key: &[u8],
    commit_seq: u64,
) -> Result<EffectiveReduction, EngineError> {
    let global = read_layer(transaction, scope_key, "global")?;
    let local = read_layer(transaction, scope_key, "local")?;
    let document_count = global.document_count + local.document_count;
    let assertion_count = global.assertion_count + local.assertion_count;
    if document_count == 0 {
        transaction
            .execute(
                "DELETE FROM canonical_effective_interpretation_settings WHERE scope_key = ?1",
                [scope_key],
            )
            .map_err(|error| sqlite_error("remove absent effective settings", error))?;
        return Ok(EffectiveReduction {
            present: false,
            global_document_status: "absent".to_string(),
            local_document_status: "absent".to_string(),
            resolution_status: None,
            document_count: 0,
            assertion_count: 0,
        });
    }

    let resolution_status = if global.health == "conflicting" || local.health == "conflicting" {
        "conflicting"
    } else if global.health == "invalid" || local.health == "invalid" {
        "invalid"
    } else {
        "resolved"
    };
    let effective = merge_settings(global.settings.as_ref(), local.settings.as_ref());
    let effective_json = serialize(&effective, "serialize effective interpretation settings")?;
    transaction
        .execute(
            r#"
            INSERT INTO canonical_effective_interpretation_settings (
                scope_key, effective_settings_json, global_document_status,
                local_document_status, resolution_status,
                global_decisive_fact_id, local_decisive_fact_id,
                document_count, assertion_count, last_commit_seq
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            ON CONFLICT(scope_key) DO UPDATE SET
                effective_settings_json = excluded.effective_settings_json,
                global_document_status = excluded.global_document_status,
                local_document_status = excluded.local_document_status,
                resolution_status = excluded.resolution_status,
                global_decisive_fact_id = excluded.global_decisive_fact_id,
                local_decisive_fact_id = excluded.local_decisive_fact_id,
                document_count = excluded.document_count,
                assertion_count = excluded.assertion_count,
                last_commit_seq = excluded.last_commit_seq
            "#,
            params![
                scope_key,
                effective_json,
                global.health,
                local.health,
                resolution_status,
                global.decisive_fact_id,
                local.decisive_fact_id,
                sqlite_usize(document_count, "interpretation settings document count")?,
                sqlite_usize(assertion_count, "interpretation settings assertion count")?,
                sqlite_u64(commit_seq, "commit sequence")?,
            ],
        )
        .map_err(|error| sqlite_error("write effective interpretation settings", error))?;

    Ok(EffectiveReduction {
        present: true,
        global_document_status: global.health.to_string(),
        local_document_status: local.health.to_string(),
        resolution_status: Some(resolution_status.to_string()),
        document_count,
        assertion_count,
    })
}

fn read_layer(
    transaction: &Transaction<'_>,
    scope_key: &[u8],
    layer: &str,
) -> Result<CanonicalLayer, EngineError> {
    let mut statement = transaction
        .prepare(
            r#"
            SELECT document_status, settings_json, resolution_status,
                   decisive_fact_id, assertion_count
            FROM canonical_interpretation_settings_documents
            WHERE scope_key = ?1 AND layer = ?2
            ORDER BY document_key
            "#,
        )
        .map_err(|error| sqlite_error("prepare interpretation settings layer", error))?;
    let documents = statement
        .query_map(params![scope_key, layer], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<Vec<u8>>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Vec<u8>>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })
        .map_err(|error| sqlite_error("read interpretation settings layer", error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| sqlite_error("collect interpretation settings layer", error))?;
    let assertion_count = documents.iter().try_fold(0usize, |total, document| {
        let count = usize::try_from(document.4).map_err(|_| {
            EngineError::InvalidCommit(
                "stored interpretation settings assertion count is invalid".to_string(),
            )
        })?;
        total.checked_add(count).ok_or_else(|| {
            EngineError::InvalidCommit(
                "interpretation settings assertion total exceeds platform limits".to_string(),
            )
        })
    })?;
    let document_count = documents.len();
    let Some((document_status, settings_json, resolution_status, decisive_fact_id, _)) =
        documents.first()
    else {
        return Ok(CanonicalLayer {
            health: "absent",
            settings: None,
            decisive_fact_id: None,
            assertion_count: 0,
            document_count: 0,
        });
    };
    if document_count > 1 || resolution_status == "conflicting" {
        return Ok(CanonicalLayer {
            health: "conflicting",
            settings: None,
            decisive_fact_id: Some(decisive_fact_id.clone()),
            assertion_count,
            document_count,
        });
    }
    if document_status == "invalid" {
        return Ok(CanonicalLayer {
            health: "invalid",
            settings: None,
            decisive_fact_id: Some(decisive_fact_id.clone()),
            assertion_count,
            document_count,
        });
    }
    if document_status != "valid" {
        return Err(EngineError::InvalidCommit(format!(
            "unknown interpretation settings document status {document_status}"
        )));
    }
    let settings_json = settings_json.as_ref().ok_or_else(|| {
        EngineError::InvalidCommit(
            "valid canonical interpretation settings are missing normalized values".to_string(),
        )
    })?;
    let settings = serde_json::from_slice(settings_json).map_err(|error| {
        EngineError::InvalidCommit(format!("decode canonical interpretation settings: {error}"))
    })?;
    Ok(CanonicalLayer {
        health: "valid",
        settings: Some(settings),
        decisive_fact_id: Some(decisive_fact_id.clone()),
        assertion_count,
        document_count,
    })
}

fn merge_settings(
    global: Option<&InterpretationSettingsSnapshot>,
    local: Option<&InterpretationSettingsSnapshot>,
) -> InterpretationSettingsSnapshot {
    let empty = InterpretationSettingsSnapshot::default();
    let global = global.unwrap_or(&empty);
    let local = local.unwrap_or(&empty);
    InterpretationSettingsSnapshot {
        agent: override_value(&global.agent, &local.agent),
        model: override_value(&global.model, &local.model),
        effort_level: override_value(&global.effort_level, &local.effort_level),
        plans_directory: override_value(&global.plans_directory, &local.plans_directory),
        always_thinking_enabled: local
            .always_thinking_enabled
            .or(global.always_thinking_enabled),
        auto_compact_enabled: local.auto_compact_enabled.or(global.auto_compact_enabled),
        skip_auto_permission_prompt: local
            .skip_auto_permission_prompt
            .or(global.skip_auto_permission_prompt),
        permission_default_mode: override_value(
            &global.permission_default_mode,
            &local.permission_default_mode,
        ),
        disable_bypass_permissions_mode: override_value(
            &global.disable_bypass_permissions_mode,
            &local.disable_bypass_permissions_mode,
        ),
        disable_auto_mode: override_value(&global.disable_auto_mode, &local.disable_auto_mode),
        permission_allow: merge_string_arrays(
            global.permission_allow.as_ref(),
            local.permission_allow.as_ref(),
        ),
        permission_ask: merge_string_arrays(
            global.permission_ask.as_ref(),
            local.permission_ask.as_ref(),
        ),
        permission_deny: merge_string_arrays(
            global.permission_deny.as_ref(),
            local.permission_deny.as_ref(),
        ),
        enabled_plugins: merge_plugin_maps(
            global.enabled_plugins.as_ref(),
            local.enabled_plugins.as_ref(),
        ),
        hook_events: merge_hook_events(global.hook_events.as_ref(), local.hook_events.as_ref()),
    }
}

fn override_value<T: Clone>(global: &Option<T>, local: &Option<T>) -> Option<T> {
    local.clone().or_else(|| global.clone())
}

fn merge_string_arrays(
    global: Option<&Vec<String>>,
    local: Option<&Vec<String>>,
) -> Option<Vec<String>> {
    if global.is_none() && local.is_none() {
        return None;
    }
    let mut seen = BTreeSet::new();
    let mut merged = Vec::new();
    for value in global
        .into_iter()
        .flatten()
        .chain(local.into_iter().flatten())
    {
        if seen.insert(value.clone()) {
            merged.push(value.clone());
        }
    }
    Some(merged)
}

fn merge_plugin_maps(
    global: Option<&BTreeMap<String, bool>>,
    local: Option<&BTreeMap<String, bool>>,
) -> Option<BTreeMap<String, bool>> {
    if global.is_none() && local.is_none() {
        return None;
    }
    let mut merged = global.cloned().unwrap_or_default();
    if let Some(local) = local {
        merged.extend(local.iter().map(|(key, value)| (key.clone(), *value)));
    }
    Some(merged)
}

fn merge_hook_events(
    global: Option<&BTreeMap<String, HookEventSummary>>,
    local: Option<&BTreeMap<String, HookEventSummary>>,
) -> Option<BTreeMap<String, HookEventSummary>> {
    if global.is_none() && local.is_none() {
        return None;
    }
    let mut merged = global.cloned().unwrap_or_default();
    if let Some(local) = local {
        for (event, local_summary) in local {
            let summary = merged.entry(event.clone()).or_insert(HookEventSummary {
                declared_matcher_count: 0,
                declared_hook_count: 0,
            });
            summary.declared_matcher_count = summary
                .declared_matcher_count
                .saturating_add(local_summary.declared_matcher_count);
            summary.declared_hook_count = summary
                .declared_hook_count
                .saturating_add(local_summary.declared_hook_count);
        }
    }
    Some(merged)
}

fn source_object_keys(
    transaction: &Transaction<'_>,
    source_object_id: i64,
) -> Result<BTreeSet<Vec<u8>>, EngineError> {
    let mut statement = transaction
        .prepare(
            "SELECT DISTINCT document_key FROM interpretation_settings_assertions WHERE source_object_id = ?1",
        )
        .map_err(|error| sqlite_error("prepare replaced interpretation settings", error))?;
    let keys = statement
        .query_map([source_object_id], |row| row.get::<_, Vec<u8>>(0))
        .map_err(|error| sqlite_error("read replaced interpretation settings", error))?
        .collect::<Result<BTreeSet<_>, _>>()
        .map_err(|error| sqlite_error("collect replaced interpretation settings", error))?;
    Ok(keys)
}

fn scopes_for_documents(
    transaction: &Transaction<'_>,
    document_keys: &BTreeSet<Vec<u8>>,
) -> Result<BTreeSet<Vec<u8>>, EngineError> {
    let mut scopes = BTreeSet::new();
    for document_key in document_keys {
        let mut statement = transaction
            .prepare(
                "SELECT DISTINCT scope_key FROM canonical_interpretation_settings_documents WHERE document_key = ?1",
            )
            .map_err(|error| sqlite_error("prepare interpretation settings scopes", error))?;
        scopes.extend(
            statement
                .query_map([document_key], |row| row.get::<_, Vec<u8>>(0))
                .map_err(|error| sqlite_error("read interpretation settings scopes", error))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| sqlite_error("collect interpretation settings scopes", error))?,
        );
    }
    Ok(scopes)
}

fn document_change(
    document_key: &[u8],
    reduction: &DocumentReduction,
) -> Result<ChangeEntry, EngineError> {
    change(
        "configuration.interpretation-settings-document.changed",
        document_key,
        reduction.resolution_status.is_some(),
        &serde_json::json!({
            "document_status": reduction.document_status,
            "resolution_status": reduction.resolution_status,
            "assertion_count": reduction.assertion_count,
            "competing_settings_count": reduction.competing_settings_count,
        }),
        "serialize interpretation settings document change",
    )
}

fn document_conflict_change(
    document_key: &[u8],
    reduction: &DocumentReduction,
) -> Result<ChangeEntry, EngineError> {
    change(
        "diagnostic.configuration.interpretation-settings-conflict",
        document_key,
        reduction.competing_settings_count > 0,
        &serde_json::json!({
            "conflicting": reduction.competing_settings_count > 0,
            "competing_settings_count": reduction.competing_settings_count,
        }),
        "serialize interpretation settings conflict change",
    )
}

fn effective_change(
    scope_key: &[u8],
    reduction: &EffectiveReduction,
) -> Result<ChangeEntry, EngineError> {
    change(
        "configuration.interpretation-settings.changed",
        scope_key,
        reduction.present,
        &serde_json::json!({
            "global_document_status": reduction.global_document_status,
            "local_document_status": reduction.local_document_status,
            "resolution_status": reduction.resolution_status,
            "document_count": reduction.document_count,
            "assertion_count": reduction.assertion_count,
        }),
        "serialize effective interpretation settings change",
    )
}

fn settings_health_change(
    scope_key: &[u8],
    reduction: &EffectiveReduction,
) -> Result<ChangeEntry, EngineError> {
    let unhealthy = reduction
        .resolution_status
        .as_deref()
        .is_some_and(|status| status != "resolved");
    change(
        "diagnostic.configuration.interpretation-settings-health",
        scope_key,
        unhealthy,
        &serde_json::json!({
            "unhealthy": unhealthy,
            "global_document_status": reduction.global_document_status,
            "local_document_status": reduction.local_document_status,
            "resolution_status": reduction.resolution_status,
        }),
        "serialize interpretation settings health change",
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

fn layer(value: InterpretationSettingsLayer) -> &'static str {
    match value {
        InterpretationSettingsLayer::Global => "global",
        InterpretationSettingsLayer::Local => "local",
    }
}

fn document_status(value: InterpretationSettingsDocumentStatus) -> &'static str {
    match value {
        InterpretationSettingsDocumentStatus::Valid => "valid",
        InterpretationSettingsDocumentStatus::Invalid => "invalid",
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
