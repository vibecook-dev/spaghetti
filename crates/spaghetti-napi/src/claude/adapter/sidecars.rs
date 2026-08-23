//! Decoders for every Claude object that is not a transcript.
//!
//! Team configs and inboxes, active-session presence, todo and task documents,
//! plans, artifacts, workflow runs and journals, the session index, project
//! memory, persisted tool results, and interpretation settings. Each one is a
//! declared object in the ADS with its own native document shape.
//!
//! Split out of `adapter.rs` to keep that file inside the landing plan's
//! 3,000-line production cap; the transcript spine stays there.

use super::*;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ClaudeSubagentMetadataDocument {
    pub(super) agent_type: String,
    #[serde(default)]
    pub(super) description: Option<String>,
    #[serde(default)]
    pub(super) name: Option<String>,
    #[serde(default)]
    pub(super) team_name: Option<String>,
    #[serde(default)]
    pub(super) spawn_depth: Option<u32>,
    #[serde(default)]
    pub(super) worktree_path: Option<String>,
    #[serde(default)]
    pub(super) tool_use_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ClaudeTeamConfigDocument {
    pub(super) name: String,
    #[serde(default)]
    pub(super) description: Option<String>,
    pub(super) created_at: i64,
    pub(super) lead_agent_id: String,
    pub(super) lead_session_id: String,
    pub(super) members: Vec<ClaudeTeamMemberDocument>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ClaudeTeamMemberDocument {
    pub(super) agent_id: String,
    pub(super) name: String,
    #[serde(default)]
    pub(super) agent_type: Option<String>,
    #[serde(default)]
    pub(super) model: Option<String>,
    #[serde(default)]
    pub(super) prompt: Option<String>,
    #[serde(default)]
    pub(super) color: Option<String>,
    #[serde(default)]
    pub(super) plan_mode_required: Option<bool>,
    pub(super) joined_at: i64,
    pub(super) tmux_pane_id: String,
    pub(super) cwd: String,
    pub(super) subscriptions: Vec<String>,
    #[serde(default)]
    pub(super) backend_type: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct ClaudeTeamInboxMessageDocument {
    pub(super) from: String,
    pub(super) text: String,
    #[serde(default)]
    pub(super) summary: Option<String>,
    pub(super) timestamp: String,
    #[serde(default)]
    pub(super) color: Option<String>,
    pub(super) read: bool,
    #[serde(default, rename = "msg_id")]
    pub(super) message_id: Option<String>,
    #[serde(default, rename = "msgV")]
    pub(super) message_version: Option<u32>,
    #[serde(default, rename = "type")]
    pub(super) kind: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ClaudeActiveSessionDocument {
    pub(super) pid: u32,
    pub(super) session_id: String,
    pub(super) cwd: String,
    pub(super) started_at: i64,
    #[serde(default)]
    pub(super) kind: Option<String>,
    #[serde(default)]
    pub(super) entrypoint: Option<String>,
    #[serde(default)]
    pub(super) name: Option<String>,
    /// Deliberately decoded but not projected: observed values look like epoch
    /// milliseconds, but native transition semantics are not fixture-proven.
    #[serde(default, rename = "nameSince")]
    pub(super) _name_since: Option<i64>,
    #[serde(default)]
    pub(super) status: Option<String>,
    #[serde(default)]
    pub(super) updated_at: Option<i64>,
    #[serde(default)]
    pub(super) status_updated_at: Option<i64>,
    #[serde(default)]
    pub(super) proc_start: Option<String>,
    #[serde(default)]
    pub(super) version: Option<String>,
    #[serde(default)]
    pub(super) peer_protocol: Option<u32>,
    #[serde(default)]
    pub(super) name_source: Option<String>,
    #[serde(default)]
    pub(super) bridge_session_id: Option<String>,
    #[serde(default)]
    pub(super) messaging_socket_path: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ClaudeTodoItemDocument {
    pub(super) content: String,
    pub(super) status: String,
    #[serde(default)]
    pub(super) active_form: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ClaudeTaskItemDocument {
    pub(super) id: String,
    pub(super) subject: String,
    pub(super) description: String,
    #[serde(default)]
    pub(super) active_form: Option<String>,
    #[serde(default)]
    pub(super) owner: Option<String>,
    pub(super) status: String,
    #[serde(default)]
    pub(super) blocks: Vec<String>,
    #[serde(default)]
    pub(super) blocked_by: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ClaudeArtifactCheckpointDocument {
    pub(super) message_id: String,
    pub(super) timestamp: String,
    #[serde(default)]
    pub(super) tracked_file_backups: BTreeMap<String, ClaudeArtifactBackupDocument>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ClaudeArtifactSnapshotDocument {
    pub(super) message_id: String,
    #[serde(default)]
    pub(super) is_snapshot_update: bool,
    pub(super) snapshot: ClaudeArtifactCheckpointDocument,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ClaudeArtifactDeltaDocument {
    pub(super) message_id: String,
    pub(super) snapshot_message_id: String,
    #[serde(default)]
    pub(super) timestamp: Option<String>,
    pub(super) tracking_path: String,
    pub(super) backup: ClaudeArtifactBackupDocument,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ClaudeArtifactBackupDocument {
    pub(super) backup_file_name: Value,
    pub(super) version: u64,
    pub(super) backup_time: String,
    #[serde(default)]
    pub(super) real_parent_dir: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ClaudeWorkflowRunDocument {
    pub(super) run_id: String,
    pub(super) timestamp: String,
    pub(super) task_id: String,
    pub(super) script: String,
    pub(super) script_path: String,
    #[serde(default)]
    pub(super) args: Option<String>,
    pub(super) agent_count: u64,
    pub(super) duration_ms: u64,
    pub(super) summary: String,
    pub(super) workflow_name: String,
    pub(super) status: String,
    pub(super) start_time: u64,
    pub(super) default_model: String,
    pub(super) total_tokens: u64,
    pub(super) total_tool_calls: u64,
    #[serde(default)]
    pub(super) error: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ClaudeWorkflowJournalDocument {
    #[serde(rename = "type")]
    pub(super) kind: String,
    pub(super) agent_id: String,
    pub(super) key: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ClaudeSessionIndexDocument {
    pub(super) version: u64,
    #[serde(default)]
    pub(super) original_path: Option<String>,
    pub(super) entries: Vec<ClaudeSessionIndexEntryDocument>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ClaudeSessionIndexEntryDocument {
    pub(super) session_id: String,
    pub(super) full_path: String,
    pub(super) file_mtime: u64,
    pub(super) first_prompt: String,
    #[serde(default)]
    pub(super) summary: Option<String>,
    pub(super) message_count: u64,
    pub(super) created: String,
    pub(super) modified: String,
    pub(super) git_branch: String,
    pub(super) project_path: String,
    pub(super) is_sidechain: bool,
}

pub(super) fn decode_team_config(
    adapter_id: &AdapterId,
    context: &ClaudeTeamConfigContext,
    record: &SourceRecord,
    output: &mut FactBatch,
) -> Result<DecodeDisposition, AdapterError> {
    let document: ClaudeTeamConfigDocument = match serde_json::from_slice(&record.payload) {
        Ok(document) => document,
        Err(error) => {
            preserve_unknown(
                record,
                output,
                Some("team_config".to_string()),
                format!("Claude team config is not a supported JSON object: {error}"),
            )?;
            return Ok(DecodeDisposition::PreservedUnknown);
        }
    };
    if document.members.len() > TEAM_MEMBER_LIMIT {
        preserve_unknown(
            record,
            output,
            Some("team_config".to_string()),
            format!("Claude team config exceeds the {TEAM_MEMBER_LIMIT} member bound"),
        )?;
        return Ok(DecodeDisposition::PreservedUnknown);
    }
    let Some(name) = nonempty(&document.name) else {
        return preserve_team_config_contract_loss(record, output, "team name is empty");
    };
    let Some(native_lead_agent_id) = nonempty(&document.lead_agent_id) else {
        return preserve_team_config_contract_loss(record, output, "lead agent id is empty");
    };
    let Some(native_lead_session_id) = nonempty(&document.lead_session_id) else {
        return preserve_team_config_contract_loss(record, output, "lead session id is empty");
    };
    let team = EntityKey::native(
        adapter_id,
        record.source_instance_id,
        "team",
        context.native_team_id.as_bytes(),
    )?;
    let mut member_names = BTreeSet::new();
    let mut members = Vec::with_capacity(document.members.len());
    for member in document.members {
        let Some(native_name) = nonempty(&member.name) else {
            return preserve_team_config_contract_loss(record, output, "member name is empty");
        };
        let Some(native_agent_id) = nonempty(&member.agent_id) else {
            return preserve_team_config_contract_loss(record, output, "member agent id is empty");
        };
        if !member_names.insert(native_name.clone()) {
            return preserve_team_config_contract_loss(
                record,
                output,
                "member names are not unique",
            );
        }
        members.push(TeamMemberSnapshot {
            member: team_member_key(
                adapter_id,
                record.source_instance_id,
                &context.native_team_id,
                &native_name,
            )?,
            native_agent_id,
            native_name,
            agent_type: member.agent_type.as_deref().and_then(nonempty),
            model: member.model.as_deref().and_then(nonempty),
            prompt: member.prompt.as_deref().and_then(nonempty),
            color: member.color.as_deref().and_then(nonempty),
            plan_mode_required: member.plan_mode_required,
            joined_at: epoch_millis_timestamp(member.joined_at),
            tmux_pane_id: member.tmux_pane_id,
            cwd: member.cwd,
            subscriptions: member.subscriptions,
            backend_type: member.backend_type.as_deref().and_then(nonempty),
        });
    }
    let mut lead_member_matches = members
        .iter()
        .filter(|member| member.native_agent_id == native_lead_agent_id);
    let Some(lead_member_snapshot) = lead_member_matches.next() else {
        return preserve_team_config_contract_loss(
            record,
            output,
            "does not contain its declared lead member",
        );
    };
    if lead_member_matches.next().is_some() {
        return preserve_team_config_contract_loss(
            record,
            output,
            "contains an ambiguous declared lead member",
        );
    }
    let lead_member = Some(lead_member_snapshot.member.clone());
    let lead_member_name = Some(lead_member_snapshot.native_name.clone());
    let created_at = epoch_millis_timestamp(document.created_at);
    output.push(
        record,
        Fact::TeamSnapshot(TeamSnapshotFact {
            team,
            native_team_id: context.native_team_id.clone(),
            name,
            description: document.description.as_deref().and_then(nonempty),
            created_at: created_at.clone(),
            lead_member,
            native_lead_agent_id: native_lead_agent_id.clone(),
            lead_session: EntityKey::native(
                adapter_id,
                record.source_instance_id,
                "session",
                native_lead_session_id.as_bytes(),
            )?,
            native_lead_session_id: native_lead_session_id.clone(),
            members,
        }),
    )?;
    emit_team_affiliation(
        record,
        output,
        ClaudeTeamAffiliationActor::Root {
            native_session_id: &native_lead_session_id,
        },
        &context.native_team_id,
        lead_member_name.as_deref(),
        Some(created_at),
    )?;
    Ok(DecodeDisposition::Applied)
}

pub(super) fn preserve_team_config_contract_loss(
    record: &SourceRecord,
    output: &mut FactBatch,
    detail: &str,
) -> Result<DecodeDisposition, AdapterError> {
    preserve_unknown(
        record,
        output,
        Some("team_config".to_string()),
        format!("Claude team config {detail}"),
    )?;
    Ok(DecodeDisposition::PreservedUnknown)
}

pub(super) fn decode_team_inbox(
    adapter_id: &AdapterId,
    context: &ClaudeTeamInboxContext,
    record: &SourceRecord,
    output: &mut FactBatch,
) -> Result<DecodeDisposition, AdapterError> {
    let documents: Vec<ClaudeTeamInboxMessageDocument> =
        match serde_json::from_slice(&record.payload) {
            Ok(documents) => documents,
            Err(error) => {
                preserve_unknown(
                    record,
                    output,
                    Some("team_inbox".to_string()),
                    format!("Claude team inbox is not a supported JSON array: {error}"),
                )?;
                return Ok(DecodeDisposition::PreservedUnknown);
            }
        };
    if documents.len() > TEAM_INBOX_MESSAGE_LIMIT {
        preserve_unknown(
            record,
            output,
            Some("team_inbox".to_string()),
            format!("Claude team inbox exceeds the {TEAM_INBOX_MESSAGE_LIMIT} message bound"),
        )?;
        return Ok(DecodeDisposition::PreservedUnknown);
    }
    let team = EntityKey::native(
        adapter_id,
        record.source_instance_id,
        "team",
        context.native_team_id.as_bytes(),
    )?;
    let recipient = team_member_key(
        adapter_id,
        record.source_instance_id,
        &context.native_team_id,
        &context.native_recipient_name,
    )?;
    let mut native_ids = BTreeSet::new();
    let mut legacy_occurrences = BTreeMap::<[u8; 32], u32>::new();
    let mut messages = Vec::with_capacity(documents.len());
    for document in documents {
        let Some(native_sender_name) = nonempty(&document.from) else {
            return preserve_team_inbox_contract_loss(record, output, "sender is empty");
        };
        let Some(timestamp) = nonempty(&document.timestamp) else {
            return preserve_team_inbox_contract_loss(record, output, "timestamp is empty");
        };
        let native_message_id = document.message_id.as_deref().and_then(nonempty);
        let mut native_message_key = Vec::new();
        push_key_component(&mut native_message_key, context.native_team_id.as_bytes());
        push_key_component(
            &mut native_message_key,
            context.native_recipient_name.as_bytes(),
        );
        if let Some(message_id) = &native_message_id {
            if !native_ids.insert(message_id.clone()) {
                return preserve_team_inbox_contract_loss(
                    record,
                    output,
                    "contains duplicate native message ids",
                );
            }
            push_key_component(&mut native_message_key, b"native-id");
            push_key_component(&mut native_message_key, message_id.as_bytes());
        } else {
            let mut hasher = blake3::Hasher::new();
            hash_component(&mut hasher, native_sender_name.as_bytes());
            hash_component(&mut hasher, timestamp.as_bytes());
            hash_component(&mut hasher, document.text.as_bytes());
            let digest = *hasher.finalize().as_bytes();
            let occurrence = legacy_occurrences.entry(digest).or_default();
            push_key_component(&mut native_message_key, b"legacy-fingerprint");
            push_key_component(&mut native_message_key, &digest);
            push_key_component(&mut native_message_key, &occurrence.to_be_bytes());
            *occurrence = occurrence.saturating_add(1);
        }
        messages.push(TeamInboxMessageSnapshot {
            message: EntityKey::native(
                adapter_id,
                record.source_instance_id,
                "team_inbox_message",
                &native_message_key,
            )?,
            sender: team_member_key(
                adapter_id,
                record.source_instance_id,
                &context.native_team_id,
                &native_sender_name,
            )?,
            native_message_id,
            native_kind: document.kind.as_deref().and_then(nonempty),
            native_version: document.message_version,
            native_sender_name,
            text: document.text,
            summary: document.summary.as_deref().and_then(nonempty),
            color: document.color.as_deref().and_then(nonempty),
            source_time: native_timestamp(&timestamp),
            read: document.read,
        });
    }
    let mut native_inbox_key = Vec::new();
    push_key_component(&mut native_inbox_key, context.native_team_id.as_bytes());
    push_key_component(
        &mut native_inbox_key,
        context.native_recipient_name.as_bytes(),
    );
    output.push(
        record,
        Fact::TeamInboxSnapshot(TeamInboxSnapshotFact {
            inbox: EntityKey::native(
                adapter_id,
                record.source_instance_id,
                "team_inbox",
                &native_inbox_key,
            )?,
            team,
            recipient,
            native_team_id: context.native_team_id.clone(),
            native_recipient_name: context.native_recipient_name.clone(),
            messages,
        }),
    )?;
    Ok(DecodeDisposition::Applied)
}

pub(super) fn preserve_team_inbox_contract_loss(
    record: &SourceRecord,
    output: &mut FactBatch,
    detail: &str,
) -> Result<DecodeDisposition, AdapterError> {
    preserve_unknown(
        record,
        output,
        Some("team_inbox".to_string()),
        format!("Claude team inbox {detail}"),
    )?;
    Ok(DecodeDisposition::PreservedUnknown)
}

pub(super) fn decode_active_session(
    adapter_id: &AdapterId,
    context: &ClaudeActiveSessionContext,
    record: &SourceRecord,
    output: &mut FactBatch,
) -> Result<DecodeDisposition, AdapterError> {
    if record.state == SourceRecordState::Absent {
        return Ok(DecodeDisposition::IgnoredKnown);
    }
    let document: ClaudeActiveSessionDocument = match serde_json::from_slice(&record.payload) {
        Ok(document) => document,
        Err(error) => {
            preserve_unknown(
                record,
                output,
                Some("active_session".to_string()),
                format!("Claude active session is not a supported JSON object: {error}"),
            )?;
            return Ok(DecodeDisposition::PreservedUnknown);
        }
    };
    if document.pid == 0 || document.pid != context.native_pid {
        return preserve_active_session_contract_loss(
            record,
            output,
            "payload pid does not match the source file name",
        );
    }
    let Some(native_session_id) = nonempty(&document.session_id) else {
        return preserve_active_session_contract_loss(record, output, "session id is empty");
    };
    let Some(cwd) = nonempty(&document.cwd) else {
        return preserve_active_session_contract_loss(record, output, "cwd is empty");
    };
    if document.started_at < 0
        || document.updated_at.is_some_and(|value| value < 0)
        || document.status_updated_at.is_some_and(|value| value < 0)
    {
        return preserve_active_session_contract_loss(
            record,
            output,
            "contains a negative epoch-millisecond timestamp",
        );
    }

    let native_process_started_at = document.proc_start.as_deref().and_then(nonempty);
    let mut native_presence_key = Vec::new();
    push_key_component(&mut native_presence_key, &document.pid.to_be_bytes());
    push_key_component(&mut native_presence_key, native_session_id.as_bytes());
    match &native_process_started_at {
        Some(process_start) => {
            push_key_component(&mut native_presence_key, b"proc-start");
            push_key_component(&mut native_presence_key, process_start.as_bytes());
        }
        None => {
            push_key_component(&mut native_presence_key, b"session-start");
            push_key_component(&mut native_presence_key, &document.started_at.to_be_bytes());
        }
    }

    output.push(
        record,
        Fact::Presence(PresenceFact {
            presence: EntityKey::native(
                adapter_id,
                record.source_instance_id,
                "presence",
                &native_presence_key,
            )?,
            session: EntityKey::native(
                adapter_id,
                record.source_instance_id,
                "session",
                native_session_id.as_bytes(),
            )?,
            run: EntityKey::native(
                adapter_id,
                record.source_instance_id,
                "run",
                native_session_id.as_bytes(),
            )?,
            native_session_id,
            native_pid: document.pid,
            cwd,
            started_at: epoch_millis_timestamp(document.started_at),
            native_kind: document.kind.as_deref().and_then(nonempty),
            entrypoint: document.entrypoint.as_deref().and_then(nonempty),
            name: document.name.as_deref().and_then(nonempty),
            native_status: document.status.as_deref().and_then(nonempty),
            updated_at: document.updated_at.map(epoch_millis_timestamp),
            status_updated_at: document.status_updated_at.map(epoch_millis_timestamp),
            native_process_started_at,
            version: document.version.as_deref().and_then(nonempty),
            peer_protocol: document.peer_protocol,
            name_source: document.name_source.as_deref().and_then(nonempty),
            bridge_session_id: document.bridge_session_id.as_deref().and_then(nonempty),
            messaging_socket_path: document.messaging_socket_path.as_deref().and_then(nonempty),
        }),
    )?;
    Ok(DecodeDisposition::Applied)
}

pub(super) fn preserve_active_session_contract_loss(
    record: &SourceRecord,
    output: &mut FactBatch,
    detail: &str,
) -> Result<DecodeDisposition, AdapterError> {
    preserve_unknown(
        record,
        output,
        Some("active_session".to_string()),
        format!("Claude active session {detail}"),
    )?;
    Ok(DecodeDisposition::PreservedUnknown)
}

pub(super) fn decode_todo_snapshot(
    adapter_id: &AdapterId,
    context: &ClaudeTodoContext,
    record: &SourceRecord,
    output: &mut FactBatch,
) -> Result<DecodeDisposition, AdapterError> {
    let documents: Vec<ClaudeTodoItemDocument> = match serde_json::from_slice(&record.payload) {
        Ok(documents) => documents,
        Err(error) => {
            preserve_unknown(
                record,
                output,
                Some("todo_snapshot".to_string()),
                format!("Claude todo snapshot is not a supported JSON array: {error}"),
            )?;
            return Ok(DecodeDisposition::PreservedUnknown);
        }
    };
    if documents.len() > TODO_ITEM_LIMIT {
        return preserve_task_contract_loss(
            record,
            output,
            "todo_snapshot",
            &format!("exceeds the {TODO_ITEM_LIMIT} item bound"),
        );
    }

    let mut native_collection_key = Vec::new();
    push_key_component(&mut native_collection_key, b"todo");
    push_key_component(
        &mut native_collection_key,
        context.native_session_id.as_bytes(),
    );
    push_key_component(
        &mut native_collection_key,
        context.native_agent_id.as_bytes(),
    );
    let collection = EntityKey::native(
        adapter_id,
        record.source_instance_id,
        "task_collection",
        &native_collection_key,
    )?;

    let mut occurrences: BTreeMap<[u8; 32], u32> = BTreeMap::new();
    let mut items = Vec::with_capacity(documents.len());
    for document in documents {
        let Some(subject) = nonempty(&document.content) else {
            return preserve_task_contract_loss(
                record,
                output,
                "todo_snapshot",
                "contains an item with empty content",
            );
        };
        let Some(native_status) = nonempty(&document.status) else {
            return preserve_task_contract_loss(
                record,
                output,
                "todo_snapshot",
                "contains an item with empty status",
            );
        };
        let digest = *blake3::hash(subject.as_bytes()).as_bytes();
        let occurrence = occurrences.entry(digest).or_default();
        let mut native_task_key = native_collection_key.clone();
        push_key_component(&mut native_task_key, b"content-fingerprint");
        push_key_component(&mut native_task_key, &digest);
        push_key_component(&mut native_task_key, &occurrence.to_be_bytes());
        *occurrence = occurrence.saturating_add(1);
        items.push(TaskItemSnapshot {
            task: EntityKey::native(
                adapter_id,
                record.source_instance_id,
                "task",
                &native_task_key,
            )?,
            native_task_id: None,
            subject,
            description: None,
            active_form: document.active_form.as_deref().and_then(nonempty),
            native_owner: None,
            status: task_status(&native_status),
            blocks: Vec::new(),
            blocked_by: Vec::new(),
        });
    }

    let session = EntityKey::native(
        adapter_id,
        record.source_instance_id,
        "session",
        context.native_session_id.as_bytes(),
    )?;
    let run = (context.native_agent_id == context.native_session_id)
        .then(|| {
            EntityKey::native(
                adapter_id,
                record.source_instance_id,
                "run",
                context.native_session_id.as_bytes(),
            )
        })
        .transpose()?;
    output.push(
        record,
        Fact::TaskSnapshot(TaskSnapshotFact {
            collection,
            session: Some(session),
            run,
            team: None,
            native_collection_id: format!(
                "{}-agent-{}",
                context.native_session_id, context.native_agent_id
            ),
            native_owner_id: Some(context.native_agent_id.clone()),
            kind: TaskCollectionKind::TodoList,
            coverage: TaskSnapshotCoverage::Complete,
            items: items.clone(),
        }),
    )?;
    emit_task_snapshot_runtime_facts(
        record,
        &output.canonical_entity_key("session", context.native_session_id.as_bytes())?,
        &canonical_todo_actor_run(output, context)?,
        &runtime_task_items(&items),
        output,
    )?;
    Ok(DecodeDisposition::Applied)
}

pub(super) fn decode_task_item(
    adapter_id: &AdapterId,
    context: &ClaudeTaskItemContext,
    record: &SourceRecord,
    output: &mut FactBatch,
) -> Result<DecodeDisposition, AdapterError> {
    let document: ClaudeTaskItemDocument = match serde_json::from_slice(&record.payload) {
        Ok(document) => document,
        Err(error) => {
            preserve_unknown(
                record,
                output,
                Some("task_item".to_string()),
                format!("Claude task item is not a supported JSON object: {error}"),
            )?;
            return Ok(DecodeDisposition::PreservedUnknown);
        }
    };
    if document.id != context.native_task_id {
        return preserve_task_contract_loss(
            record,
            output,
            "task_item",
            "payload id does not match the source file name",
        );
    }
    let Some(subject) = nonempty(&document.subject) else {
        return preserve_task_contract_loss(record, output, "task_item", "subject is empty");
    };
    let Some(native_status) = nonempty(&document.status) else {
        return preserve_task_contract_loss(record, output, "task_item", "status is empty");
    };
    if document
        .blocks
        .iter()
        .chain(document.blocked_by.iter())
        .any(|value| value.trim().is_empty())
    {
        return preserve_task_contract_loss(
            record,
            output,
            "task_item",
            "contains an empty dependency id",
        );
    }

    let mut native_collection_key = Vec::new();
    push_key_component(&mut native_collection_key, b"task-directory");
    push_key_component(
        &mut native_collection_key,
        context.native_collection_id.as_bytes(),
    );
    let mut native_task_key = native_collection_key.clone();
    push_key_component(&mut native_task_key, context.native_task_id.as_bytes());
    output.push(
        record,
        Fact::TaskSnapshot(TaskSnapshotFact {
            collection: EntityKey::native(
                adapter_id,
                record.source_instance_id,
                "task_collection",
                &native_collection_key,
            )?,
            // A Claude task-directory name can be a session id, team name, or
            // other native scope. Keep it unjoined until another native fact
            // disambiguates it rather than guessing from its spelling.
            session: None,
            run: None,
            team: None,
            native_collection_id: context.native_collection_id.clone(),
            native_owner_id: None,
            kind: TaskCollectionKind::NativeTaskList,
            coverage: TaskSnapshotCoverage::ItemDocument,
            items: vec![TaskItemSnapshot {
                task: EntityKey::native(
                    adapter_id,
                    record.source_instance_id,
                    "task",
                    &native_task_key,
                )?,
                native_task_id: Some(context.native_task_id.clone()),
                subject,
                description: Some(document.description),
                active_form: document.active_form.as_deref().and_then(nonempty),
                native_owner: document.owner.as_deref().and_then(nonempty),
                status: task_status(&native_status),
                blocks: document.blocks,
                blocked_by: document.blocked_by,
            }],
        }),
    )?;
    Ok(DecodeDisposition::Applied)
}

pub(super) fn decode_plan_document(
    adapter_id: &AdapterId,
    context: &ClaudePlanContext,
    record: &SourceRecord,
    output: &mut FactBatch,
) -> Result<DecodeDisposition, AdapterError> {
    let content = match std::str::from_utf8(&record.payload) {
        Ok(content) => content.to_string(),
        Err(error) => {
            preserve_unknown(
                record,
                output,
                Some("plan_document".to_string()),
                format!("Claude plan document is not valid UTF-8: {error}"),
            )?;
            return Ok(DecodeDisposition::PreservedUnknown);
        }
    };
    let title = first_markdown_heading(&content).unwrap_or_else(|| context.native_plan_id.clone());
    output.push(
        record,
        Fact::PlanSnapshot(PlanSnapshotFact {
            plan: EntityKey::native(
                adapter_id,
                record.source_instance_id,
                "plan",
                context.native_plan_id.as_bytes(),
            )?,
            native_plan_id: context.native_plan_id.clone(),
            title,
            size_bytes: record.payload.len() as u64,
            content,
            source_time: None,
        }),
    )?;
    Ok(DecodeDisposition::Applied)
}

pub(super) fn decode_artifact_content(
    adapter_id: &AdapterId,
    context: &ClaudeArtifactContentContext,
    record: &SourceRecord,
    output: &mut FactBatch,
) -> Result<DecodeDisposition, AdapterError> {
    if record.state == SourceRecordState::Absent {
        return Ok(DecodeDisposition::IgnoredKnown);
    }
    let session = EntityKey::native(
        adapter_id,
        record.source_instance_id,
        "session",
        context.native_session_id.as_bytes(),
    )?;
    let canonical_session =
        output.canonical_entity_key("session", context.native_session_id.as_bytes())?;
    let artifact_native_key = artifact_native_key(
        &context.native_session_id,
        Some(&context.native_artifact_id),
        None,
        context.version,
        None,
    );
    let canonical_artifact = output.canonical_entity_key("artifact", &artifact_native_key)?;
    output.push_native(
        record,
        &artifact_native_key,
        Fact::ArtifactContent(ArtifactContentFact {
            artifact: artifact_key(
                adapter_id,
                record.source_instance_id,
                &context.native_session_id,
                Some(&context.native_artifact_id),
                None,
                context.version,
                None,
            )?,
            session,
            canonical_artifact: Some(canonical_artifact),
            canonical_session: Some(canonical_session),
            native_artifact_id: context.native_artifact_id.clone(),
            native_file_hash: context.native_file_hash.clone(),
            version: context.version,
            size_bytes: record.payload.len() as u64,
            content: record.payload.clone(),
        }),
    )?;
    Ok(DecodeDisposition::Applied)
}

pub(super) fn decode_workflow_run(
    adapter_id: &AdapterId,
    context: &ClaudeWorkflowContext,
    record: &SourceRecord,
    output: &mut FactBatch,
) -> Result<DecodeDisposition, AdapterError> {
    if record.state == SourceRecordState::Absent {
        return Ok(DecodeDisposition::IgnoredKnown);
    }
    let native_snapshot: Value = match serde_json::from_slice(&record.payload) {
        Ok(value) => value,
        Err(error) => {
            return preserve_workflow_contract_loss(
                record,
                output,
                "workflow_run",
                &format!("is not valid JSON: {error}"),
            );
        }
    };
    let document: ClaudeWorkflowRunDocument = match serde_json::from_value(native_snapshot.clone())
    {
        Ok(document) => document,
        Err(error) => {
            return preserve_workflow_contract_loss(
                record,
                output,
                "workflow_run",
                &format!("is not a supported run document: {error}"),
            );
        }
    };
    if document.run_id != context.native_workflow_id {
        return preserve_workflow_contract_loss(
            record,
            output,
            "workflow_run",
            "payload runId does not match the source file name",
        );
    }
    for (field, value) in [
        ("taskId", document.task_id.as_str()),
        ("workflowName", document.workflow_name.as_str()),
        ("status", document.status.as_str()),
        ("defaultModel", document.default_model.as_str()),
        ("script", document.script.as_str()),
        ("scriptPath", document.script_path.as_str()),
        ("summary", document.summary.as_str()),
        ("timestamp", document.timestamp.as_str()),
    ] {
        if value.trim().is_empty() {
            return preserve_workflow_contract_loss(
                record,
                output,
                "workflow_run",
                &format!("has an empty {field}"),
            );
        }
    }
    let Ok(start_time) = i64::try_from(document.start_time) else {
        return preserve_workflow_contract_loss(
            record,
            output,
            "workflow_run",
            "startTime exceeds the supported epoch-millisecond range",
        );
    };
    let workflow = workflow_key(adapter_id, record.source_instance_id, context)?;
    output.push(
        record,
        Fact::WorkflowSnapshot(WorkflowSnapshotFact {
            workflow,
            session: EntityKey::native(
                adapter_id,
                record.source_instance_id,
                "session",
                context.native_session_id.as_bytes(),
            )?,
            project: EntityKey::native(
                adapter_id,
                record.source_instance_id,
                "project",
                context.project_slug.as_bytes(),
            )?,
            native_workflow_id: document.run_id,
            native_task_id: document.task_id,
            name: document.workflow_name,
            native_status: document.status.clone(),
            status: workflow_status(&document.status),
            default_model: document.default_model,
            script: document.script,
            script_path: document.script_path,
            args: document.args,
            summary: document.summary,
            error: document.error,
            started_at: epoch_millis_timestamp(start_time),
            finished_at: native_timestamp(&document.timestamp),
            duration_ms: document.duration_ms,
            agent_count: document.agent_count,
            total_tokens: document.total_tokens,
            total_tool_calls: document.total_tool_calls,
            native_snapshot,
        }),
    )?;
    Ok(DecodeDisposition::Applied)
}

pub(super) fn decode_workflow_journal(
    adapter_id: &AdapterId,
    context: &ClaudeWorkflowContext,
    record: &SourceRecord,
    output: &mut FactBatch,
) -> Result<DecodeDisposition, AdapterError> {
    if record.state == SourceRecordState::Absent {
        return Ok(DecodeDisposition::IgnoredKnown);
    }
    let value: Value = match serde_json::from_slice(&record.payload) {
        Ok(value) => value,
        Err(error) => {
            return preserve_workflow_contract_loss(
                record,
                output,
                "workflow_journal",
                &format!("record is not valid JSON: {error}"),
            );
        }
    };
    let document: ClaudeWorkflowJournalDocument = match serde_json::from_value(value.clone()) {
        Ok(document) => document,
        Err(error) => {
            return preserve_workflow_contract_loss(
                record,
                output,
                "workflow_journal",
                &format!("record is not supported: {error}"),
            );
        }
    };
    let Some(native_agent_id) = nonempty(&document.agent_id) else {
        return preserve_workflow_contract_loss(
            record,
            output,
            "workflow_journal",
            "record has an empty agentId",
        );
    };
    let Some(native_event_key) = nonempty(&document.key) else {
        return preserve_workflow_contract_loss(
            record,
            output,
            "workflow_journal",
            "record has an empty key",
        );
    };
    let (kind, result) = match document.kind.as_str() {
        "started" if value.get("result").is_none() => (WorkflowMemberEventKind::Started, None),
        "result" => {
            let Some(result) = value.get("result").cloned() else {
                return preserve_workflow_contract_loss(
                    record,
                    output,
                    "workflow_journal",
                    "result record is missing its result value",
                );
            };
            (WorkflowMemberEventKind::Result, Some(result))
        }
        "started" => {
            return preserve_workflow_contract_loss(
                record,
                output,
                "workflow_journal",
                "started record unexpectedly contains a result value",
            );
        }
        _ => {
            return preserve_workflow_contract_loss(
                record,
                output,
                "workflow_journal",
                "record has an unsupported event type",
            );
        }
    };

    let workflow = workflow_key(adapter_id, record.source_instance_id, context)?;
    let mut member_native_key = workflow_native_key(context);
    push_key_component(&mut member_native_key, native_agent_id.as_bytes());
    let child_run_native_key = format!(
        "{}\0{}\0{}",
        context.native_session_id, context.native_workflow_id, native_agent_id
    );
    let canonical_session =
        output.canonical_entity_key("session", context.native_session_id.as_bytes())?;
    let canonical_actor_run =
        output.canonical_entity_key("run", child_run_native_key.as_bytes())?;
    let canonical_workflow =
        output.canonical_entity_key("workflow", &workflow_native_key(context))?;
    let canonical_member = output.canonical_entity_key("workflow_member", &member_native_key)?;
    let mut affiliation_native_key = Vec::new();
    push_key_component(&mut affiliation_native_key, b"workflow");
    push_key_component(&mut affiliation_native_key, child_run_native_key.as_bytes());
    push_key_component(&mut affiliation_native_key, &workflow_native_key(context));
    let canonical_affiliation =
        output.canonical_entity_key("actor_affiliation", &affiliation_native_key)?;
    output.push(
        record,
        Fact::WorkflowMemberEvent(WorkflowMemberEventFact {
            workflow,
            member: EntityKey::native(
                adapter_id,
                record.source_instance_id,
                "workflow_member",
                &member_native_key,
            )?,
            child_run: EntityKey::native(
                adapter_id,
                record.source_instance_id,
                "run",
                child_run_native_key.as_bytes(),
            )?,
            session: EntityKey::native(
                adapter_id,
                record.source_instance_id,
                "session",
                context.native_session_id.as_bytes(),
            )?,
            project: EntityKey::native(
                adapter_id,
                record.source_instance_id,
                "project",
                context.project_slug.as_bytes(),
            )?,
            native_workflow_id: context.native_workflow_id.clone(),
            native_agent_id: native_agent_id.clone(),
            native_event_key,
            kind,
            result,
        }),
    )?;
    output.push_native(
        record,
        &affiliation_native_key,
        Fact::ActorAffiliationRevision(ActorAffiliationRevisionFact {
            affiliation: canonical_affiliation,
            actor_run: canonical_actor_run,
            session: canonical_session,
            dimension: ActorAffiliationDimension::Workflow,
            target: canonical_workflow,
            member: Some(canonical_member),
            native_target_id: Some(context.native_workflow_id.clone()),
            native_member_id: Some(native_agent_id),
            state: ActorAffiliationState::Present,
            effective_at: None,
        }),
    )?;
    Ok(DecodeDisposition::Applied)
}

pub(super) fn decode_session_index(
    adapter_id: &AdapterId,
    context: &ClaudeSessionIndexContext,
    record: &SourceRecord,
    output: &mut FactBatch,
) -> Result<DecodeDisposition, AdapterError> {
    if record.state == SourceRecordState::Absent {
        return Ok(DecodeDisposition::IgnoredKnown);
    }
    let native_snapshot: Value = match serde_json::from_slice(&record.payload) {
        Ok(value) => value,
        Err(error) => {
            return preserve_session_index_contract_loss(
                record,
                output,
                &format!("is not valid JSON: {error}"),
            );
        }
    };
    let document: ClaudeSessionIndexDocument = match serde_json::from_value(native_snapshot.clone())
    {
        Ok(document) => document,
        Err(error) => {
            return preserve_session_index_contract_loss(
                record,
                output,
                &format!("is not a supported document: {error}"),
            );
        }
    };
    if document.version != 1 {
        return preserve_session_index_contract_loss(
            record,
            output,
            "has an unsupported native version",
        );
    }
    if document.entries.len() > SESSION_INDEX_ENTRY_LIMIT {
        return preserve_session_index_contract_loss(
            record,
            output,
            &format!("exceeds the {SESSION_INDEX_ENTRY_LIMIT} entry bound"),
        );
    }
    if document
        .original_path
        .as_deref()
        .is_some_and(|path| path.trim().is_empty())
    {
        return preserve_session_index_contract_loss(record, output, "has an empty originalPath");
    }

    let mut session_ids = BTreeSet::new();
    let mut entries = Vec::with_capacity(document.entries.len());
    for entry in document.entries {
        if !is_uuid(&entry.session_id) {
            return preserve_session_index_contract_loss(
                record,
                output,
                "contains a non-UUID sessionId",
            );
        }
        if !session_ids.insert(entry.session_id.clone()) {
            return preserve_session_index_contract_loss(
                record,
                output,
                "contains duplicate sessionId entries",
            );
        }
        for (field, value) in [
            ("fullPath", entry.full_path.as_str()),
            ("firstPrompt", entry.first_prompt.as_str()),
            ("created", entry.created.as_str()),
            ("modified", entry.modified.as_str()),
            ("projectPath", entry.project_path.as_str()),
        ] {
            if value.trim().is_empty() {
                return preserve_session_index_contract_loss(
                    record,
                    output,
                    &format!("contains an entry with empty {field}"),
                );
            }
        }
        entries.push(SessionIndexEntrySnapshot {
            session: EntityKey::native(
                adapter_id,
                record.source_instance_id,
                "session",
                entry.session_id.as_bytes(),
            )?,
            native_session_id: entry.session_id,
            full_path: entry.full_path,
            file_mtime_ms: entry.file_mtime,
            first_prompt: entry.first_prompt,
            summary: entry.summary,
            message_count: entry.message_count,
            created_at: native_timestamp(&entry.created),
            modified_at: native_timestamp(&entry.modified),
            git_branch: entry.git_branch,
            project_path: entry.project_path,
            is_sidechain: entry.is_sidechain,
        });
    }

    output.push(
        record,
        Fact::SessionIndexSnapshot(SessionIndexSnapshotFact {
            project: EntityKey::native(
                adapter_id,
                record.source_instance_id,
                "project",
                context.project_slug.as_bytes(),
            )?,
            native_project_key: context.project_slug.clone(),
            native_version: document.version,
            original_path: document.original_path,
            entries,
            native_snapshot,
        }),
    )?;
    Ok(DecodeDisposition::Applied)
}

pub(super) fn decode_project_memory_document(
    adapter_id: &AdapterId,
    context: &ClaudeProjectMemoryContext,
    record: &SourceRecord,
    output: &mut FactBatch,
) -> Result<DecodeDisposition, AdapterError> {
    if record.state == SourceRecordState::Absent {
        return Ok(DecodeDisposition::IgnoredKnown);
    }
    let content = match std::str::from_utf8(&record.payload) {
        Ok(content) => content.to_string(),
        Err(error) => {
            preserve_unknown(
                record,
                output,
                Some("project_memory_document".to_string()),
                format!("Claude project memory document is not valid UTF-8: {error}"),
            )?;
            return Ok(DecodeDisposition::PreservedUnknown);
        }
    };
    let file_name = context
        .native_document_path
        .strip_prefix("memory/")
        .expect("validated project memory context");
    let fallback_title = file_name
        .strip_suffix(".md")
        .expect("validated project memory Markdown path");
    let title = first_markdown_heading(&content).unwrap_or_else(|| fallback_title.to_string());
    let mut document_native_key = Vec::new();
    push_key_component(&mut document_native_key, context.project_slug.as_bytes());
    push_key_component(
        &mut document_native_key,
        context.native_document_path.as_bytes(),
    );
    output.push(
        record,
        Fact::ProjectMemoryDocument(ProjectMemoryDocumentFact {
            document: EntityKey::native(
                adapter_id,
                record.source_instance_id,
                "project_memory_document",
                &document_native_key,
            )?,
            project: EntityKey::native(
                adapter_id,
                record.source_instance_id,
                "project",
                context.project_slug.as_bytes(),
            )?,
            native_project_key: context.project_slug.clone(),
            native_document_path: context.native_document_path.clone(),
            title,
            content,
            size_bytes: record.payload.len() as u64,
            is_index: context.is_index,
        }),
    )?;
    Ok(DecodeDisposition::Applied)
}

pub(super) fn decode_persisted_tool_result(
    adapter_id: &AdapterId,
    context: &ClaudePersistedToolResultContext,
    record: &SourceRecord,
    output: &mut FactBatch,
) -> Result<DecodeDisposition, AdapterError> {
    if record.state == SourceRecordState::Absent {
        return Ok(DecodeDisposition::IgnoredKnown);
    }
    let content = match std::str::from_utf8(&record.payload) {
        Ok(content) => content.to_string(),
        Err(error) => {
            preserve_unknown(
                record,
                output,
                Some("persisted_tool_result".to_string()),
                format!("Claude persisted tool result is not valid UTF-8: {error}"),
            )?;
            return Ok(DecodeDisposition::PreservedUnknown);
        }
    };
    let mut result_native_key = Vec::new();
    push_key_component(&mut result_native_key, context.native_session_id.as_bytes());
    push_key_component(
        &mut result_native_key,
        context.native_tool_use_id.as_bytes(),
    );
    output.push(
        record,
        Fact::PersistedToolResult(PersistedToolResultFact {
            result: EntityKey::native(
                adapter_id,
                record.source_instance_id,
                "persisted_tool_result",
                &result_native_key,
            )?,
            session: EntityKey::native(
                adapter_id,
                record.source_instance_id,
                "session",
                context.native_session_id.as_bytes(),
            )?,
            project: EntityKey::native(
                adapter_id,
                record.source_instance_id,
                "project",
                context.project_slug.as_bytes(),
            )?,
            native_project_key: context.project_slug.clone(),
            native_session_id: context.native_session_id.clone(),
            native_tool_use_id: context.native_tool_use_id.clone(),
            native_document_path: context.native_document_path.clone(),
            content,
            size_bytes: record.payload.len() as u64,
        }),
    )?;
    Ok(DecodeDisposition::Applied)
}

pub(super) fn decode_interpretation_settings(
    adapter_id: &AdapterId,
    context: &ClaudeInterpretationSettingsContext,
    record: &SourceRecord,
    output: &mut FactBatch,
) -> Result<DecodeDisposition, AdapterError> {
    if record.state == SourceRecordState::Absent {
        return Ok(DecodeDisposition::IgnoredKnown);
    }

    let decoded = decode_interpretation_settings_snapshot(&record.payload);
    let (document_status, settings, error_code, disposition) = match decoded {
        Ok(settings) => (
            InterpretationSettingsDocumentStatus::Valid,
            Some(settings),
            None,
            DecodeDisposition::Applied,
        ),
        Err(failure) => {
            output.push_diagnostic(AdapterDiagnostic {
                class: AdapterErrorClass::RecordPermanent,
                code: failure.code.to_string(),
                message: format!(
                    "Claude {} could not be interpreted: {}",
                    context.native_document_path, failure.message
                ),
            })?;
            (
                InterpretationSettingsDocumentStatus::Invalid,
                None,
                Some(failure.code.to_string()),
                DecodeDisposition::PreservedUnknown,
            )
        }
    };

    output.push(
        record,
        Fact::InterpretationSettings(InterpretationSettingsFact {
            document: EntityKey::native(
                adapter_id,
                record.source_instance_id,
                "interpretation_settings_document",
                context.native_document_path.as_bytes(),
            )?,
            scope: EntityKey::native(
                adapter_id,
                record.source_instance_id,
                "interpretation_settings_scope",
                b"root",
            )?,
            layer: context.layer,
            native_document_path: context.native_document_path.clone(),
            document_status,
            settings,
            error_code,
            size_bytes: record.payload.len() as u64,
        }),
    )?;
    Ok(disposition)
}

#[derive(Debug)]
pub(super) struct InterpretationSettingsDecodeFailure {
    pub(super) code: &'static str,
    pub(super) message: String,
}

impl InterpretationSettingsDecodeFailure {
    fn shape(message: impl Into<String>) -> Self {
        Self {
            code: "claude_settings_invalid_shape",
            message: message.into(),
        }
    }

    fn bounds(message: impl Into<String>) -> Self {
        Self {
            code: "claude_settings_bounds",
            message: message.into(),
        }
    }
}

pub(super) fn decode_interpretation_settings_snapshot(
    payload: &[u8],
) -> Result<InterpretationSettingsSnapshot, InterpretationSettingsDecodeFailure> {
    let value: Value =
        serde_json::from_slice(payload).map_err(|error| InterpretationSettingsDecodeFailure {
            code: "claude_settings_invalid_json",
            message: format!(
                "invalid JSON at line {}, column {}",
                error.line(),
                error.column()
            ),
        })?;
    let object = value.as_object().ok_or_else(|| {
        InterpretationSettingsDecodeFailure::shape("document root must be an object")
    })?;

    let permissions = match object.get("permissions") {
        None => None,
        Some(Value::Object(permissions)) => Some(permissions),
        Some(_) => {
            return Err(InterpretationSettingsDecodeFailure::shape(
                "permissions must be an object",
            ));
        }
    };

    Ok(InterpretationSettingsSnapshot {
        agent: optional_settings_string(object, "agent", "agent")?,
        model: optional_settings_string(object, "model", "model")?,
        effort_level: optional_settings_string(object, "effortLevel", "effortLevel")?,
        plans_directory: optional_settings_string(object, "plansDirectory", "plansDirectory")?,
        always_thinking_enabled: optional_settings_bool(
            object,
            "alwaysThinkingEnabled",
            "alwaysThinkingEnabled",
        )?,
        auto_compact_enabled: optional_settings_bool(
            object,
            "autoCompactEnabled",
            "autoCompactEnabled",
        )?,
        skip_auto_permission_prompt: optional_settings_bool(
            object,
            "skipAutoPermissionPrompt",
            "skipAutoPermissionPrompt",
        )?,
        permission_default_mode: optional_nested_settings_string(
            permissions,
            "defaultMode",
            "permissions.defaultMode",
        )?,
        disable_bypass_permissions_mode: optional_nested_settings_string(
            permissions,
            "disableBypassPermissionsMode",
            "permissions.disableBypassPermissionsMode",
        )?,
        disable_auto_mode: optional_nested_settings_string(
            permissions,
            "disableAutoMode",
            "permissions.disableAutoMode",
        )?,
        permission_allow: optional_nested_settings_string_array(
            permissions,
            "allow",
            "permissions.allow",
        )?,
        permission_ask: optional_nested_settings_string_array(
            permissions,
            "ask",
            "permissions.ask",
        )?,
        permission_deny: optional_nested_settings_string_array(
            permissions,
            "deny",
            "permissions.deny",
        )?,
        enabled_plugins: optional_settings_bool_map(object, "enabledPlugins")?,
        hook_events: optional_hook_event_summaries(object)?,
    })
}

pub(super) fn optional_settings_string(
    object: &serde_json::Map<String, Value>,
    key: &str,
    field: &str,
) -> Result<Option<String>, InterpretationSettingsDecodeFailure> {
    match object.get(key) {
        None => Ok(None),
        Some(Value::String(value)) => {
            validate_settings_string(value, field)?;
            Ok(Some(value.clone()))
        }
        Some(_) => Err(InterpretationSettingsDecodeFailure::shape(format!(
            "{field} must be a string"
        ))),
    }
}

pub(super) fn optional_nested_settings_string(
    object: Option<&serde_json::Map<String, Value>>,
    key: &str,
    field: &str,
) -> Result<Option<String>, InterpretationSettingsDecodeFailure> {
    match object {
        Some(object) => optional_settings_string(object, key, field),
        None => Ok(None),
    }
}

pub(super) fn optional_settings_bool(
    object: &serde_json::Map<String, Value>,
    key: &str,
    field: &str,
) -> Result<Option<bool>, InterpretationSettingsDecodeFailure> {
    match object.get(key) {
        None => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(InterpretationSettingsDecodeFailure::shape(format!(
            "{field} must be a boolean"
        ))),
    }
}

pub(super) fn optional_nested_settings_string_array(
    object: Option<&serde_json::Map<String, Value>>,
    key: &str,
    field: &str,
) -> Result<Option<Vec<String>>, InterpretationSettingsDecodeFailure> {
    let Some(object) = object else {
        return Ok(None);
    };
    let Some(value) = object.get(key) else {
        return Ok(None);
    };
    let Value::Array(values) = value else {
        return Err(InterpretationSettingsDecodeFailure::shape(format!(
            "{field} must be an array"
        )));
    };
    if values.len() > SETTINGS_COLLECTION_LIMIT {
        return Err(InterpretationSettingsDecodeFailure::bounds(format!(
            "{field} exceeds {SETTINGS_COLLECTION_LIMIT} entries"
        )));
    }
    values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let Value::String(value) = value else {
                return Err(InterpretationSettingsDecodeFailure::shape(format!(
                    "{field}[{index}] must be a string"
                )));
            };
            validate_settings_string(value, field)?;
            Ok(value.clone())
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

pub(super) fn optional_settings_bool_map(
    object: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<BTreeMap<String, bool>>, InterpretationSettingsDecodeFailure> {
    let Some(value) = object.get(key) else {
        return Ok(None);
    };
    let Value::Object(values) = value else {
        return Err(InterpretationSettingsDecodeFailure::shape(
            "enabledPlugins must be an object",
        ));
    };
    if values.len() > SETTINGS_COLLECTION_LIMIT {
        return Err(InterpretationSettingsDecodeFailure::bounds(format!(
            "enabledPlugins exceeds {SETTINGS_COLLECTION_LIMIT} entries"
        )));
    }
    values
        .iter()
        .map(|(plugin, enabled)| {
            validate_settings_string(plugin, "enabledPlugins key")?;
            let Value::Bool(enabled) = enabled else {
                return Err(InterpretationSettingsDecodeFailure::shape(format!(
                    "enabledPlugins.{plugin} must be a boolean"
                )));
            };
            Ok((plugin.clone(), *enabled))
        })
        .collect::<Result<BTreeMap<_, _>, _>>()
        .map(Some)
}

pub(super) fn optional_hook_event_summaries(
    object: &serde_json::Map<String, Value>,
) -> Result<Option<BTreeMap<String, HookEventSummary>>, InterpretationSettingsDecodeFailure> {
    let Some(value) = object.get("hooks") else {
        return Ok(None);
    };
    let Value::Object(events) = value else {
        return Err(InterpretationSettingsDecodeFailure::shape(
            "hooks must be an object",
        ));
    };
    if events.len() > SETTINGS_COLLECTION_LIMIT {
        return Err(InterpretationSettingsDecodeFailure::bounds(format!(
            "hooks exceeds {SETTINGS_COLLECTION_LIMIT} events"
        )));
    }
    let mut summaries = BTreeMap::new();
    for (event, value) in events {
        validate_settings_string(event, "hooks event")?;
        let Value::Array(matchers) = value else {
            return Err(InterpretationSettingsDecodeFailure::shape(format!(
                "hooks.{event} must be an array"
            )));
        };
        if matchers.len() > SETTINGS_COLLECTION_LIMIT {
            return Err(InterpretationSettingsDecodeFailure::bounds(format!(
                "hooks.{event} exceeds {SETTINGS_COLLECTION_LIMIT} matchers"
            )));
        }
        let mut hook_count = 0usize;
        for (index, matcher) in matchers.iter().enumerate() {
            let Value::Object(matcher) = matcher else {
                return Err(InterpretationSettingsDecodeFailure::shape(format!(
                    "hooks.{event}[{index}] must be an object"
                )));
            };
            let Some(Value::Array(hooks)) = matcher.get("hooks") else {
                return Err(InterpretationSettingsDecodeFailure::shape(format!(
                    "hooks.{event}[{index}].hooks must be an array"
                )));
            };
            hook_count = hook_count.checked_add(hooks.len()).ok_or_else(|| {
                InterpretationSettingsDecodeFailure::bounds(format!(
                    "hooks.{event} hook count exceeds platform limits"
                ))
            })?;
            if hook_count > SETTINGS_COLLECTION_LIMIT {
                return Err(InterpretationSettingsDecodeFailure::bounds(format!(
                    "hooks.{event} exceeds {SETTINGS_COLLECTION_LIMIT} hooks"
                )));
            }
        }
        summaries.insert(
            event.clone(),
            HookEventSummary {
                declared_matcher_count: matchers.len() as u64,
                declared_hook_count: hook_count as u64,
            },
        );
    }
    Ok(Some(summaries))
}

pub(super) fn validate_settings_string(
    value: &str,
    field: &str,
) -> Result<(), InterpretationSettingsDecodeFailure> {
    if value.len() > SETTINGS_STRING_MAX_BYTES {
        return Err(InterpretationSettingsDecodeFailure::bounds(format!(
            "{field} exceeds {SETTINGS_STRING_MAX_BYTES} bytes"
        )));
    }
    Ok(())
}

pub(super) fn preserve_session_index_contract_loss(
    record: &SourceRecord,
    output: &mut FactBatch,
    detail: &str,
) -> Result<DecodeDisposition, AdapterError> {
    preserve_unknown(
        record,
        output,
        Some("session_index".to_string()),
        format!("Claude session index {detail}"),
    )?;
    Ok(DecodeDisposition::PreservedUnknown)
}

pub(super) fn preserve_workflow_contract_loss(
    record: &SourceRecord,
    output: &mut FactBatch,
    native_kind: &str,
    detail: &str,
) -> Result<DecodeDisposition, AdapterError> {
    preserve_unknown(
        record,
        output,
        Some(native_kind.to_string()),
        format!("Claude {native_kind} {detail}"),
    )?;
    Ok(DecodeDisposition::PreservedUnknown)
}

pub(super) fn preserve_task_contract_loss(
    record: &SourceRecord,
    output: &mut FactBatch,
    native_kind: &str,
    detail: &str,
) -> Result<DecodeDisposition, AdapterError> {
    preserve_unknown(
        record,
        output,
        Some(native_kind.to_string()),
        format!("Claude {native_kind} {detail}"),
    )?;
    Ok(DecodeDisposition::PreservedUnknown)
}

pub(super) fn task_status(native_status: &str) -> TaskStatus {
    match native_status {
        "pending" => TaskStatus::Pending,
        "in_progress" => TaskStatus::InProgress,
        "completed" => TaskStatus::Completed,
        other => TaskStatus::Other(other.to_string()),
    }
}

pub(super) fn first_markdown_heading(content: &str) -> Option<String> {
    content.lines().find_map(|line| {
        let rest = line.strip_prefix('#')?;
        let first = rest.chars().next()?;
        if !first.is_whitespace() {
            return None;
        }
        nonempty(rest.trim_start_matches(char::is_whitespace))
    })
}

pub(super) fn team_member_key(
    adapter_id: &AdapterId,
    source_instance_id: u64,
    native_team_id: &str,
    native_member_name: &str,
) -> Result<EntityKey, AdapterError> {
    let native_key = team_member_native_key(native_team_id, native_member_name);
    EntityKey::native(adapter_id, source_instance_id, "team_member", &native_key)
}

pub(super) fn team_member_native_key(native_team_id: &str, native_member_name: &str) -> Vec<u8> {
    let mut native_key = Vec::new();
    push_key_component(&mut native_key, native_team_id.as_bytes());
    push_key_component(&mut native_key, native_member_name.as_bytes());
    native_key
}

pub(super) enum ClaudeTeamAffiliationActor<'a> {
    Root {
        native_session_id: &'a str,
    },
    Child {
        native_session_id: &'a str,
        run_native_key: &'a str,
    },
}

pub(super) fn emit_team_affiliation(
    record: &SourceRecord,
    output: &mut FactBatch,
    actor: ClaudeTeamAffiliationActor<'_>,
    native_team_id: &str,
    native_member_name: Option<&str>,
    effective_at: Option<QualifiedTimestamp>,
) -> Result<(), AdapterError> {
    let (native_session_id, run_native_key, actor_run) = match actor {
        ClaudeTeamAffiliationActor::Root { native_session_id } => (
            native_session_id,
            native_session_id,
            output.canonical_root_actor_run_key(native_session_id.as_bytes(), None)?,
        ),
        ClaudeTeamAffiliationActor::Child {
            native_session_id,
            run_native_key,
        } => (
            native_session_id,
            run_native_key,
            output.canonical_entity_key("run", run_native_key.as_bytes())?,
        ),
    };
    let mut affiliation_native_key = Vec::new();
    push_key_component(&mut affiliation_native_key, b"team");
    push_key_component(&mut affiliation_native_key, run_native_key.as_bytes());
    push_key_component(&mut affiliation_native_key, native_team_id.as_bytes());
    let member = native_member_name
        .map(|name| {
            output
                .canonical_entity_key("team_member", &team_member_native_key(native_team_id, name))
        })
        .transpose()?;
    output.push_native(
        record,
        &affiliation_native_key,
        Fact::ActorAffiliationRevision(ActorAffiliationRevisionFact {
            affiliation: output
                .canonical_entity_key("actor_affiliation", &affiliation_native_key)?,
            actor_run,
            session: output.canonical_entity_key("session", native_session_id.as_bytes())?,
            dimension: ActorAffiliationDimension::Team,
            target: output.canonical_entity_key("team", native_team_id.as_bytes())?,
            member,
            native_target_id: Some(native_team_id.to_string()),
            native_member_id: native_member_name.map(str::to_string),
            state: ActorAffiliationState::Present,
            effective_at,
        }),
    )?;
    Ok(())
}

pub(super) fn epoch_millis_timestamp(value: i64) -> QualifiedTimestamp {
    QualifiedTimestamp {
        value: crate::core::timefmt::epoch_ms_to_iso8601(value as f64),
        quality: TimestampQuality::NativeExact,
    }
}

pub(super) fn hash_component(hasher: &mut blake3::Hasher, value: &[u8]) {
    hasher.update(&(value.len() as u64).to_be_bytes());
    hasher.update(value);
}
