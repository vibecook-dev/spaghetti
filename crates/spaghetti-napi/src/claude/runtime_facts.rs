//! RFC 012C runtime fact emission for the Claude Code adapter.
//!
//! RFC 012A puts native interpretation — and only native interpretation — in
//! the adapter: this module turns one already-parsed Claude transcript record
//! into the typed runtime facts RFC 012C defines, and nothing else. It opens
//! no files, reads no database, and knows nothing about durable commits or
//! observer delivery; both topologies get these facts from the one
//! `decode_record` spine.
//!
//! Eight families are sourced here. The reducer class RFC 012C §12 assigns to
//! each one is what decides its identity and its revision key:
//!
//! | family | reducer class | native evidence |
//! | --- | --- | --- |
//! | `runtime.message` | `CurrentGenerationLog` | one `user`/`assistant`/`system` record |
//! | `runtime.content-block` | `CurrentGenerationLog` | that record's content blocks |
//! | `runtime.tool` | `CorrelatedLifecycle` | `tool_use` / `tool_result` blocks |
//! | `runtime.user-input-request` | `CorrelatedLifecycle` | `AskUserQuestion` and its result |
//! | `runtime.effective-state` | `RevisionedEntity` | `message.model`, `mode`, `permissionMode`, `effort` |
//! | `runtime.native-marker` | `CurrentGenerationLog` | compaction boundaries, `progress`, `queue-operation` |
//! | `runtime.task` | `OwnedSetSnapshot` | the declared `todos/` sidecar |
//! | `runtime.plan` | `RevisionedEntity` | the declared `plans/` sidecar |
//!
//! Two rules govern everything below. Identity is deterministic: every key is
//! derived from declared native identifiers (or a documented, fixture-tested
//! fallback), never from iteration, batching, or arrival order. And evidence is
//! never invented: a value that a record does not prove is absent or
//! `Partial`, never a default.

use serde_json::Value;

use crate::adapter::{
    AdapterError, CanonicalEntityKey, ContentBlock, ContentBlockRevisionFact,
    ContentBlockRevisionValue, ContractCompleteness, EffectiveStateDimension,
    EffectiveStateEvidenceKind, EffectiveStateQualifiedValue, EffectiveStateRevisionFact,
    EffectiveStateValueAuthority, EffectiveStateValueProvenance, Fact, FactBatch,
    MessageRevisionFact, MessageRevisionRole, MessageRole, NativeCompactionPhase,
    NativeProgressState, NativeQueueOperation, NativeRuntimeMarkerProvenance,
    NativeRuntimeMarkerRevisionFact, NativeRuntimeMarkerValue, PlanRevisionFact,
    QualifiedValueQuality, TaskLifecycleState, TaskRevisionFact, ToolRevisionFact,
    ToolRevisionKind, UserInputKind, UserInputLifecycleState, UserInputOperation, UserInputOption,
    UserInputQuestion, UserInputRequestRevisionFact,
};
use crate::source::SourceRecord;

/// Version of the normalization rules this module applies to native values.
/// It travels in every fact's provenance; changing a rule below changes this
/// number and forces replay, because old and new facts would otherwise claim
/// the same semantic identity for differently normalized values.
pub(crate) const RUNTIME_NORMALIZATION_CONTRACT_VERSION: u32 = 1;

/// RFC 012C bounds a message's block snapshot; a longer native message is
/// still emitted, but as `Partial` evidence that cannot prove block absence.
const MAX_SNAPSHOT_CONTENT_BLOCKS: usize = 32;
/// Matches the common `MAX_RUNTIME_SEMANTIC_TEXT_BYTES` bound in
/// `adapter/facts.rs`. Longer native text is truncated on a character boundary
/// and the block is marked `Partial`.
const MAX_CONTENT_TEXT_BYTES: usize = 8 * 1024;
/// Unresolved tool calls retained per object so a later `tool_result` can name
/// its tool and correlate. Claude answers a call within a few records, so this
/// is normally empty or nearly so; the bound is what keeps decoder state from
/// growing with the transcript.
const MAX_OPEN_TOOLS: usize = 32;
/// Unresolved `AskUserQuestion` interactions retained per object. Every
/// revision of an interaction must carry its typed questions, so the pending
/// question set is retained until its result arrives.
const MAX_OPEN_QUESTIONS: usize = 4;
const MAX_OPEN_TOOL_NAME_BYTES: usize = 64;
const MAX_OPEN_QUESTION_BYTES: usize = 4 * 1024;
/// Declared suffix that gives a `tool_result` its own entity identity. Claude
/// names only the call, so RFC 012C §10.4's separate result entity uses this
/// documented, fixture-tested derivation from the call identity.
const TOOL_RESULT_ID_SUFFIX: &str = "#result";

const ASK_USER_QUESTION_TOOL: &str = "AskUserQuestion";
/// Tools whose input carries a plan document.
///
/// RFC 012C §10.2 allows tool lifecycle evidence to supply a plan revision. It
/// is the *only* Claude evidence that binds a plan to a session and actor: a
/// `plans/<slug>.md` sidecar names no owner, so that document stays a snapshot
/// fact rather than becoming a runtime plan with an invented owner.
const PLAN_TOOLS: [&str; 2] = ["ExitPlanMode", "EnterPlanMode"];

/// One native transcript record, already parsed by the adapter, in the terms
/// this module needs. Passing the decoded projection rather than re-parsing
/// keeps the record's JSON walked once.
pub(crate) struct TranscriptRuntimeRecord<'a> {
    pub session: &'a CanonicalEntityKey,
    pub actor_run: &'a CanonicalEntityKey,
    pub record_type: &'a str,
    pub record_uuid: Option<&'a str>,
    pub role: &'a MessageRole,
    pub content: &'a [ContentBlock],
    pub model: Option<&'a str>,
    pub raw: &'a Value,
}

/// Per-object decoder state the runtime families need beyond one record.
///
/// Everything here is bounded and content-addressed: correlation needs the
/// open calls, and effective state needs the last accepted value per dimension
/// so an unchanged value does not become a redundant revision on all 344k
/// usage-bearing rows of a real corpus.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct TranscriptRuntimeState {
    effective_state: [Option<[u8; 32]>; 4],
    open_tools: Vec<OpenTool>,
    open_questions: Vec<OpenQuestion>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OpenTool {
    native_id: String,
    tool_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OpenQuestion {
    native_id: String,
    questions: Vec<UserInputQuestion>,
}

impl TranscriptRuntimeState {
    /// Decode the runtime tail of the Claude transcript decoder state. A tail
    /// this decoder did not write is a stream-fatal contract error rather than
    /// a silently empty state, so a stale generation replays instead of
    /// pretending it had no open calls.
    pub(crate) fn decode(mut bytes: &[u8]) -> Result<Self, AdapterError> {
        fn take<'a>(bytes: &mut &'a [u8], len: usize) -> Result<&'a [u8], AdapterError> {
            if bytes.len() < len {
                return Err(state_error("runtime decoder state ended mid-field"));
            }
            let (head, tail) = bytes.split_at(len);
            *bytes = tail;
            Ok(head)
        }
        fn take_u8(bytes: &mut &[u8]) -> Result<u8, AdapterError> {
            Ok(take(bytes, 1)?[0])
        }
        fn take_text(
            bytes: &mut &[u8],
            len: usize,
            field: &'static str,
        ) -> Result<String, AdapterError> {
            String::from_utf8(take(bytes, len)?.to_vec()).map_err(|_| state_error(field))
        }

        let mut state = Self::default();
        let present = take_u8(&mut bytes)?;
        for (index, slot) in state.effective_state.iter_mut().enumerate() {
            if present & (1 << index) == 0 {
                continue;
            }
            let mut digest = [0_u8; 32];
            digest.copy_from_slice(take(&mut bytes, 32)?);
            *slot = Some(digest);
        }

        let open_tools = take_u8(&mut bytes)? as usize;
        if open_tools > MAX_OPEN_TOOLS {
            return Err(state_error(
                "runtime decoder state exceeds the open-tool bound",
            ));
        }
        for _ in 0..open_tools {
            let id_len = take_u8(&mut bytes)? as usize;
            let native_id = take_text(&mut bytes, id_len, "open tool id is not UTF-8")?;
            let name_len = take_u8(&mut bytes)? as usize;
            let tool_name = take_text(&mut bytes, name_len, "open tool name is not UTF-8")?;
            state.open_tools.push(OpenTool {
                native_id,
                tool_name,
            });
        }

        let open_questions = take_u8(&mut bytes)? as usize;
        if open_questions > MAX_OPEN_QUESTIONS {
            return Err(state_error(
                "runtime decoder state exceeds the open-interaction bound",
            ));
        }
        for _ in 0..open_questions {
            let id_len = take_u8(&mut bytes)? as usize;
            let native_id = take_text(&mut bytes, id_len, "open interaction id is not UTF-8")?;
            let encoded_len = u16::from_be_bytes(
                take(&mut bytes, 2)?
                    .try_into()
                    .expect("two bytes were taken"),
            ) as usize;
            let encoded = take(&mut bytes, encoded_len)?;
            let questions = serde_json::from_slice(encoded)
                .map_err(|_| state_error("open interaction questions are not decodable"))?;
            state.open_questions.push(OpenQuestion {
                native_id,
                questions,
            });
        }
        if !bytes.is_empty() {
            return Err(state_error("runtime decoder state has trailing bytes"));
        }
        Ok(state)
    }

    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut encoded = Vec::new();
        let mut present = 0_u8;
        for (index, slot) in self.effective_state.iter().enumerate() {
            if slot.is_some() {
                present |= 1 << index;
            }
        }
        encoded.push(present);
        for slot in self.effective_state.iter().flatten() {
            encoded.extend_from_slice(slot);
        }
        encoded.push(self.open_tools.len() as u8);
        for open in &self.open_tools {
            encoded.push(open.native_id.len() as u8);
            encoded.extend_from_slice(open.native_id.as_bytes());
            encoded.push(open.tool_name.len() as u8);
            encoded.extend_from_slice(open.tool_name.as_bytes());
        }
        encoded.push(self.open_questions.len() as u8);
        for open in &self.open_questions {
            encoded.push(open.native_id.len() as u8);
            encoded.extend_from_slice(open.native_id.as_bytes());
            let questions = serde_json::to_vec(&open.questions).unwrap_or_default();
            encoded.extend_from_slice(&(questions.len() as u16).to_be_bytes());
            encoded.extend_from_slice(&questions);
        }
        encoded
    }

    /// Remember an unresolved call. Identifiers or names beyond the retained
    /// bound are not stored: a later result for them stays unmatched evidence
    /// instead of being given a guessed tool name.
    fn open_tool(&mut self, native_id: &str, tool_name: &str) {
        if native_id.len() > u8::MAX as usize || tool_name.len() > MAX_OPEN_TOOL_NAME_BYTES {
            return;
        }
        if self.open_tools.len() == MAX_OPEN_TOOLS {
            self.open_tools.remove(0);
        }
        self.open_tools.push(OpenTool {
            native_id: native_id.to_string(),
            tool_name: tool_name.to_string(),
        });
    }

    fn close_tool(&mut self, native_id: &str) -> Option<String> {
        let index = self
            .open_tools
            .iter()
            .position(|open| open.native_id == native_id)?;
        Some(self.open_tools.remove(index).tool_name)
    }

    fn open_question(&mut self, native_id: &str, questions: &[UserInputQuestion]) {
        if native_id.len() > u8::MAX as usize {
            return;
        }
        let encoded = serde_json::to_vec(questions)
            .map(|value| value.len())
            .unwrap_or(usize::MAX);
        if encoded > MAX_OPEN_QUESTION_BYTES {
            return;
        }
        if self.open_questions.len() == MAX_OPEN_QUESTIONS {
            self.open_questions.remove(0);
        }
        self.open_questions.push(OpenQuestion {
            native_id: native_id.to_string(),
            questions: questions.to_vec(),
        });
    }

    fn close_question(&mut self, native_id: &str) -> Option<Vec<UserInputQuestion>> {
        let index = self
            .open_questions
            .iter()
            .position(|open| open.native_id == native_id)?;
        Some(self.open_questions.remove(index).questions)
    }

    /// Report whether a dimension's value changed, and remember the new one.
    fn observe_effective_state(&mut self, dimension: EffectiveStateDimension, value: &str) -> bool {
        let slot = &mut self.effective_state[effective_state_slot(dimension)];
        let observed = *blake3::hash(value.as_bytes()).as_bytes();
        let changed = slot.as_ref() != Some(&observed);
        *slot = Some(observed);
        changed
    }
}

fn effective_state_slot(dimension: EffectiveStateDimension) -> usize {
    match dimension {
        EffectiveStateDimension::Model => 0,
        EffectiveStateDimension::Effort => 1,
        EffectiveStateDimension::SessionMode => 2,
        EffectiveStateDimension::PermissionMode => 3,
    }
}

fn state_error(message: &'static str) -> AdapterError {
    AdapterError::new(
        crate::adapter::AdapterErrorClass::StreamFatal,
        "claude_runtime_decoder_state",
        message.to_string(),
    )
}

/// Emit every RFC 012C family this transcript record proves.
///
/// The order is the record's own: a message before its blocks, a call before
/// the result that closes it. Nothing here depends on that order for identity,
/// but a consumer reading the stream sees a coherent sequence.
pub(crate) fn emit_transcript_runtime_facts(
    record: &SourceRecord,
    input: &TranscriptRuntimeRecord<'_>,
    state: &mut TranscriptRuntimeState,
    output: &mut FactBatch,
) -> Result<(), AdapterError> {
    emit_message_and_blocks(record, input, state, output)?;
    emit_effective_state(record, input, state, output)?;
    emit_native_markers(record, input, output)?;
    Ok(())
}

/// The complete key set for a message's blocks, plus each block's own native
/// identity. Both come from one walk so the message snapshot and the block
/// facts cannot disagree about what the message contained.
struct BlockKey {
    snapshot_key: String,
    native_id: Option<String>,
}

fn block_key(block: &ContentBlock, ordinal: u32) -> BlockKey {
    let native_id = match block {
        ContentBlock::ToolCall { native_id, .. } => Some(native_id.clone()),
        ContentBlock::ToolResult { native_call_id, .. } => {
            Some(format!("{native_call_id}{TOOL_RESULT_ID_SUFFIX}"))
        }
        _ => None,
    };
    let native_id = native_id.filter(|value| is_semantic_text(value));
    let snapshot_key = match &native_id {
        Some(native_id) => format!("n:{native_id}"),
        None => format!("o:{ordinal}"),
    };
    BlockKey {
        snapshot_key,
        native_id,
    }
}

fn emit_message_and_blocks(
    record: &SourceRecord,
    input: &TranscriptRuntimeRecord<'_>,
    state: &mut TranscriptRuntimeState,
    output: &mut FactBatch,
) -> Result<(), AdapterError> {
    let Some(role) = message_revision_role(input.role) else {
        return Ok(());
    };
    // A message with no blocks has no representable current-generation state,
    // and a record without a native uuid has no stable message identity. Both
    // stay outside the family rather than acquiring an invented key.
    let Some(native_message_id) = input.record_uuid.filter(|value| is_semantic_text(value)) else {
        return Ok(());
    };
    if input.content.is_empty() {
        return Ok(());
    }

    let mut keys = Vec::with_capacity(input.content.len().min(MAX_SNAPSHOT_CONTENT_BLOCKS));
    let mut blocks = Vec::with_capacity(input.content.len().min(MAX_SNAPSHOT_CONTENT_BLOCKS));
    for (ordinal, block) in input.content.iter().enumerate() {
        if ordinal == MAX_SNAPSHOT_CONTENT_BLOCKS {
            break;
        }
        let key = block_key(block, ordinal as u32);
        if keys
            .iter()
            .any(|existing: &String| existing == &key.snapshot_key)
        {
            // Two blocks claiming one native identity make the snapshot
            // unprovable; fall back to the ordinal, which is unique by
            // construction.
            keys.push(format!("o:{ordinal}"));
            blocks.push((
                ordinal as u32,
                BlockKey {
                    snapshot_key: format!("o:{ordinal}"),
                    native_id: None,
                },
                block,
            ));
            continue;
        }
        keys.push(key.snapshot_key.clone());
        blocks.push((ordinal as u32, key, block));
    }
    let completeness = if input.content.len() > MAX_SNAPSHOT_CONTENT_BLOCKS {
        ContractCompleteness::Partial
    } else {
        ContractCompleteness::Complete
    };

    let message_key = message_stable_key(native_message_id);
    let message_fact_id = output.canonical_object_scoped_fact_id(
        record.generation,
        "runtime.message",
        &message_key,
    )?;
    let message = MessageRevisionFact {
        session: *input.session,
        actor_run: *input.actor_run,
        native_message_id: native_message_id.to_string(),
        role,
        ordered_content_block_keys: keys,
        completeness,
        operation: UserInputOperation::Upsert,
    };
    let revision = message.semantic_revision_key()?;
    output.push_native_object_scoped_with_revision(
        record,
        &message_key,
        &revision,
        Fact::MessageRevision(message),
    )?;

    for (ordinal, key, block) in blocks {
        let (content, block_completeness) = content_block_value(block);
        let native_tool_id = match block {
            ContentBlock::ToolCall { native_id, .. } => {
                Some(native_id.clone()).filter(|value| is_semantic_text(value))
            }
            ContentBlock::ToolResult { native_call_id, .. } => {
                Some(native_call_id.clone()).filter(|value| is_semantic_text(value))
            }
            _ => None,
        };
        let fact = ContentBlockRevisionFact {
            session: *input.session,
            actor_run: *input.actor_run,
            message: message_fact_id,
            native_content_block_id: key.native_id.clone(),
            ordinal,
            content,
            native_tool_call_or_result_id: native_tool_id,
            completeness: block_completeness,
            operation: UserInputOperation::Upsert,
        };
        let block_key_bytes = fact.stable_native_fact_key()?;
        let revision = fact.semantic_revision_key()?;
        output.push_native_object_scoped_with_revision(
            record,
            &block_key_bytes,
            &revision,
            Fact::ContentBlockRevision(fact),
        )?;

        emit_tool_and_interaction(record, input, state, output, block)?;
    }
    Ok(())
}

/// A tool call or result, and the `AskUserQuestion` interaction a call may
/// open. RFC 012C keeps calls and results as separate correlated entities, so
/// a result that arrives without its call is still evidence.
fn emit_tool_and_interaction(
    record: &SourceRecord,
    input: &TranscriptRuntimeRecord<'_>,
    state: &mut TranscriptRuntimeState,
    output: &mut FactBatch,
    block: &ContentBlock,
) -> Result<(), AdapterError> {
    match block {
        ContentBlock::ToolCall {
            native_id,
            name,
            input: tool_input,
        } => {
            if !is_semantic_text(native_id) || !is_semantic_text(name) {
                return Ok(());
            }
            let fact = ToolRevisionFact {
                session: *input.session,
                actor_run: *input.actor_run,
                native_tool_id: native_id.clone(),
                kind: ToolRevisionKind::Call,
                tool_name: name.clone(),
                correlated_native_id: None,
                completeness: ContractCompleteness::Complete,
                operation: UserInputOperation::Upsert,
            };
            push_tool_revision(record, output, &fact)?;
            state.open_tool(native_id, name);

            if PLAN_TOOLS.contains(&name.as_str()) {
                if let Some(plan) = string_field(tool_input, "plan") {
                    emit_plan_runtime_facts(
                        record,
                        input.session,
                        input.actor_run,
                        native_id,
                        &plan_subject(&plan),
                        plan_step_keys(&plan),
                        output,
                    )?;
                }
            }
            if name == ASK_USER_QUESTION_TOOL {
                if let Some(questions) = user_input_questions(tool_input) {
                    let fact = UserInputRequestRevisionFact {
                        session: *input.session,
                        actor_run: *input.actor_run,
                        native_tool_use_id: native_id.clone(),
                        kind: user_input_kind(&questions),
                        questions: questions.clone(),
                        state: UserInputLifecycleState::Pending,
                        operation: UserInputOperation::Upsert,
                        completeness: ContractCompleteness::Complete,
                        result_reference: None,
                    };
                    push_user_input_revision(record, output, &fact)?;
                    state.open_question(native_id, &questions);
                }
            }
            Ok(())
        }
        ContentBlock::ToolResult {
            native_call_id,
            is_error,
            ..
        } => {
            if !is_semantic_text(native_call_id) {
                return Ok(());
            }
            let questions = state.close_question(native_call_id);
            let Some(tool_name) = state.close_tool(native_call_id) else {
                // An unmatched result keeps its content-block evidence, which
                // already carries the native call identity. RFC 012C forbids
                // inventing the tool name a `tool_result` never carries, so no
                // tool entity is claimed for it.
                return Ok(());
            };
            let result_id = format!("{native_call_id}{TOOL_RESULT_ID_SUFFIX}");
            if !is_semantic_text(&result_id) {
                return Ok(());
            }
            let fact = ToolRevisionFact {
                session: *input.session,
                actor_run: *input.actor_run,
                native_tool_id: result_id.clone(),
                kind: ToolRevisionKind::Result,
                tool_name: tool_name.clone(),
                correlated_native_id: Some(native_call_id.clone()),
                completeness: ContractCompleteness::Complete,
                operation: UserInputOperation::Upsert,
            };
            push_tool_revision(record, output, &fact)?;

            // Correlation is a revision of the call entity, not a new entity:
            // RFC 012C §10.4 requires the call key to survive being answered.
            let correlated_call = ToolRevisionFact {
                session: *input.session,
                actor_run: *input.actor_run,
                native_tool_id: native_call_id.clone(),
                kind: ToolRevisionKind::Call,
                tool_name,
                correlated_native_id: Some(result_id),
                completeness: ContractCompleteness::Complete,
                operation: UserInputOperation::Upsert,
            };
            push_tool_revision(record, output, &correlated_call)?;

            if let Some(questions) = questions {
                let fact = UserInputRequestRevisionFact {
                    session: *input.session,
                    actor_run: *input.actor_run,
                    native_tool_use_id: native_call_id.clone(),
                    kind: user_input_kind(&questions),
                    questions,
                    state: if *is_error {
                        UserInputLifecycleState::Failed
                    } else {
                        UserInputLifecycleState::Resolved
                    },
                    operation: UserInputOperation::Upsert,
                    completeness: ContractCompleteness::Complete,
                    result_reference: Some(format!("{native_call_id}{TOOL_RESULT_ID_SUFFIX}")),
                };
                push_user_input_revision(record, output, &fact)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn push_tool_revision(
    record: &SourceRecord,
    output: &mut FactBatch,
    fact: &ToolRevisionFact,
) -> Result<(), AdapterError> {
    let mut key = Vec::new();
    key.extend_from_slice(b"runtime.tool\0");
    key.push(match fact.kind {
        ToolRevisionKind::Call => 1,
        ToolRevisionKind::Result => 2,
    });
    key.extend_from_slice(fact.native_tool_id.as_bytes());
    let revision = fact.semantic_revision_key()?;
    output.push_native_object_scoped_with_revision(
        record,
        &key,
        &revision,
        Fact::ToolRevision(fact.clone()),
    )?;
    Ok(())
}

fn push_user_input_revision(
    record: &SourceRecord,
    output: &mut FactBatch,
    fact: &UserInputRequestRevisionFact,
) -> Result<(), AdapterError> {
    let mut key = Vec::new();
    key.extend_from_slice(b"runtime.user-input-request\0");
    key.extend_from_slice(fact.native_tool_use_id.as_bytes());
    let revision = fact.semantic_revision_key()?;
    output.push_native_object_scoped_with_revision(
        record,
        &key,
        &revision,
        Fact::UserInputRequestRevision(fact.clone()),
    )?;
    Ok(())
}

fn message_stable_key(native_message_id: &str) -> Vec<u8> {
    let mut key = Vec::with_capacity(native_message_id.len() + 16);
    key.extend_from_slice(b"runtime.message\0");
    key.extend_from_slice(native_message_id.as_bytes());
    key
}

fn message_revision_role(role: &MessageRole) -> Option<MessageRevisionRole> {
    match role {
        MessageRole::User => Some(MessageRevisionRole::User),
        MessageRole::Assistant => Some(MessageRevisionRole::Assistant),
        MessageRole::System => Some(MessageRevisionRole::System),
        // A summary is not a turn in the conversation, and an unrecognized
        // native kind has no proven role.
        MessageRole::Summary | MessageRole::Other(_) => None,
    }
}

fn content_block_value(block: &ContentBlock) -> (ContentBlockRevisionValue, ContractCompleteness) {
    match block {
        ContentBlock::Text { text } => {
            let (text, completeness) = bounded_text(text);
            (ContentBlockRevisionValue::Text { text }, completeness)
        }
        ContentBlock::Thinking { text, redacted } => {
            let (text, completeness) = bounded_text(text);
            (
                ContentBlockRevisionValue::Thinking {
                    text,
                    redacted: *redacted,
                },
                completeness,
            )
        }
        ContentBlock::ToolCall { name, input, .. } => (
            ContentBlockRevisionValue::ToolCall {
                tool_name: bounded_text(name).0,
                input_digest: value_digest(input),
            },
            ContractCompleteness::Complete,
        ),
        ContentBlock::ToolResult {
            content, is_error, ..
        } => (
            ContentBlockRevisionValue::ToolResult {
                content_digest: value_digest(content),
                is_error: *is_error,
            },
            ContractCompleteness::Complete,
        ),
        ContentBlock::Image {
            media_type,
            data_hash,
        } => (
            ContentBlockRevisionValue::Image {
                media_type: media_type.clone(),
                data_hash: *data_hash,
            },
            ContractCompleteness::Complete,
        ),
        ContentBlock::Document {
            media_type,
            data_hash,
        } => (
            ContentBlockRevisionValue::Document {
                media_type: media_type.clone(),
                data_hash: *data_hash,
            },
            ContractCompleteness::Complete,
        ),
        ContentBlock::Native { native_kind, value } => (
            ContentBlockRevisionValue::NativeExtension {
                native_kind: normalized_native_kind(native_kind),
                value_digest: value_digest(value),
            },
            ContractCompleteness::Complete,
        ),
    }
}

/// Truncate on a character boundary, and say so. A silently shortened value
/// would claim to be the complete native text.
fn bounded_text(text: &str) -> (String, ContractCompleteness) {
    if text.len() <= MAX_CONTENT_TEXT_BYTES {
        return (text.to_string(), ContractCompleteness::Complete);
    }
    let mut end = MAX_CONTENT_TEXT_BYTES;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_string(), ContractCompleteness::Partial)
}

fn value_digest(value: &Value) -> [u8; 32] {
    let encoded = serde_json::to_vec(value).unwrap_or_default();
    *blake3::hash(&encoded).as_bytes()
}

/// Native extension kinds cross the boundary as bounded machine identifiers.
fn normalized_native_kind(native_kind: &str) -> String {
    let normalized: String = native_kind
        .chars()
        .map(|character| match character {
            'a'..='z' | '0'..='9' | '.' | '_' | '-' => character,
            'A'..='Z' => character.to_ascii_lowercase(),
            _ => '_',
        })
        .take(64)
        .collect();
    match normalized.chars().next() {
        Some('a'..='z') => normalized,
        _ => format!("x_{normalized}"),
    }
}

fn is_semantic_text(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_CONTENT_TEXT_BYTES && value.trim() == value
}

/// Effective state, one revisioned entity per actor and dimension.
///
/// RFC 012C §9 is strict about what each piece of evidence proves: a model on
/// a response proves it was effective *for that response*, and a dedicated
/// `permission-mode` record is a native transition. Neither is a default, so
/// an absent field emits nothing at all.
fn emit_effective_state(
    record: &SourceRecord,
    input: &TranscriptRuntimeRecord<'_>,
    state: &mut TranscriptRuntimeState,
    output: &mut FactBatch,
) -> Result<(), AdapterError> {
    let mut observed: Vec<(
        EffectiveStateDimension,
        String,
        EffectiveStateEvidenceKind,
        &'static str,
    )> = Vec::new();

    if let Some(model) = input.model.filter(|value| is_semantic_text(value)) {
        observed.push((
            EffectiveStateDimension::Model,
            model.to_string(),
            EffectiveStateEvidenceKind::ResponseObserved,
            "message.model",
        ));
    }
    // A record whose whole purpose is the mode change is a native transition;
    // the same field riding along on an ordinary record only proves the mode
    // was effective when that record was written.
    let transition = matches!(input.record_type, "permission-mode" | "mode");
    for (field, dimension, native_field) in [
        (
            "permissionMode",
            EffectiveStateDimension::PermissionMode,
            "record.permission_mode",
        ),
        ("mode", EffectiveStateDimension::SessionMode, "record.mode"),
        ("effort", EffectiveStateDimension::Effort, "record.effort"),
    ] {
        let Some(value) = string_field(input.raw, field) else {
            continue;
        };
        observed.push((
            dimension,
            value,
            if transition {
                EffectiveStateEvidenceKind::NativeTransition
            } else {
                EffectiveStateEvidenceKind::ResponseObserved
            },
            native_field,
        ));
    }

    for (dimension, value, evidence_kind, native_field) in observed {
        if !is_semantic_text(&value) || !state.observe_effective_state(dimension, &value) {
            continue;
        }
        let authority = match evidence_kind {
            EffectiveStateEvidenceKind::ConfiguredIntent => {
                EffectiveStateValueAuthority::NativeConfiguration
            }
            EffectiveStateEvidenceKind::ResponseObserved => {
                EffectiveStateValueAuthority::NativeResponse
            }
            EffectiveStateEvidenceKind::NativeTransition => {
                EffectiveStateValueAuthority::NativeTransition
            }
        };
        let fact = EffectiveStateRevisionFact {
            session: *input.session,
            actor_run: *input.actor_run,
            dimension,
            value: EffectiveStateQualifiedValue {
                value: Some(value),
                quality: QualifiedValueQuality::NativeClaimed,
                authority,
                completeness: ContractCompleteness::Complete,
                unknown_reason: None,
                effective_at: record.source_timestamp_hint,
                provenance: EffectiveStateValueProvenance {
                    native_field: native_field.to_string(),
                    normalization_contract_version: RUNTIME_NORMALIZATION_CONTRACT_VERSION,
                },
            },
            evidence_kind,
            completeness: ContractCompleteness::Complete,
            operation: UserInputOperation::Upsert,
        };
        let mut key = Vec::new();
        key.extend_from_slice(b"runtime.effective-state\0");
        key.push(effective_state_slot(dimension) as u8);
        key.extend_from_slice(input.actor_run.as_bytes());
        let revision = fact.semantic_revision_key()?;
        output.push_native_object_scoped_with_revision(
            record,
            &key,
            &revision,
            Fact::EffectiveStateRevision(fact),
        )?;
    }
    Ok(())
}

/// Compaction boundaries, tool progress, and queue operations.
///
/// RFC 012C §10.5 admits only native markers here — a host-side guess about
/// what the agent is doing belongs to a different family and never reaches
/// this one.
fn emit_native_markers(
    record: &SourceRecord,
    input: &TranscriptRuntimeRecord<'_>,
    output: &mut FactBatch,
) -> Result<(), AdapterError> {
    let effective_at = record.source_timestamp_hint;
    let markers: Vec<(
        String,
        NativeRuntimeMarkerValue,
        Option<String>,
        &'static str,
    )> = match input.record_type {
        "system" => {
            let mut markers = Vec::new();
            for (field, native_field) in [
                ("compactMetadata", "system.compact_boundary"),
                ("microcompactMetadata", "system.microcompact_boundary"),
            ] {
                let Some(metadata) = input.raw.get(field).and_then(Value::as_object) else {
                    continue;
                };
                let Some(marker_id) = input
                    .record_uuid
                    .filter(|value| is_semantic_text(value))
                    .map(|uuid| format!("{uuid}:{field}"))
                else {
                    continue;
                };
                markers.push((
                    marker_id,
                    NativeRuntimeMarkerValue::Compaction {
                        phase: NativeCompactionPhase::Boundary,
                        trigger: metadata
                            .get("trigger")
                            .and_then(Value::as_str)
                            .filter(|value| is_semantic_text(value))
                            .map(str::to_owned),
                        pre_tokens: metadata.get("preTokens").and_then(Value::as_u64),
                    },
                    None,
                    native_field,
                ));
            }
            markers
        }
        "progress" => {
            let Some(marker_id) = input
                .record_uuid
                .filter(|value| is_semantic_text(value))
                .map(str::to_owned)
            else {
                return Ok(());
            };
            vec![(
                marker_id,
                NativeRuntimeMarkerValue::Progress {
                    // A progress record proves activity was observed; it
                    // carries no completed/total counters to normalize, so
                    // those stay absent rather than becoming zero.
                    state: NativeProgressState::Active,
                    completed: None,
                    total: None,
                    detail_digest: input.raw.get("data").map(value_digest),
                },
                string_field(input.raw, "toolUseID"),
                "record.progress",
            )]
        }
        "queue-operation" => {
            let Some(operation) = string_field(input.raw, "operation") else {
                return Ok(());
            };
            let Some(operation) = queue_operation(&operation) else {
                return Ok(());
            };
            // Queue records carry no uuid, so their identity is the record
            // itself; `push_derived` binds it to the source record id.
            vec![(
                String::from("queue-operation"),
                NativeRuntimeMarkerValue::Queue {
                    operation,
                    depth: None,
                    item_digest: input.raw.get("content").map(value_digest),
                },
                None,
                "record.queue_operation",
            )]
        }
        _ => Vec::new(),
    };

    for (marker_id, value, correlated, native_field) in markers {
        let derived = input.record_type == "queue-operation";
        let fact = NativeRuntimeMarkerRevisionFact {
            session: *input.session,
            actor_run: *input.actor_run,
            native_marker_id: marker_id.clone(),
            correlated_native_id: correlated.filter(|value| is_semantic_text(value)),
            value,
            quality: QualifiedValueQuality::NativeClaimed,
            effective_at,
            provenance: NativeRuntimeMarkerProvenance {
                native_field: native_field.to_string(),
                normalization_contract_version: RUNTIME_NORMALIZATION_CONTRACT_VERSION,
            },
            completeness: ContractCompleteness::Complete,
            operation: UserInputOperation::Upsert,
        };
        let revision = fact.semantic_revision_key()?;
        if derived {
            output.push_derived_with_revision(
                record,
                b"runtime.native-marker/queue-operation",
                &revision,
                Fact::NativeRuntimeMarkerRevision(fact),
            )?;
        } else {
            let mut key = Vec::new();
            key.extend_from_slice(b"runtime.native-marker\0");
            key.extend_from_slice(marker_id.as_bytes());
            output.push_native_object_scoped_with_revision(
                record,
                &key,
                &revision,
                Fact::NativeRuntimeMarkerRevision(fact),
            )?;
        }
    }
    Ok(())
}

fn queue_operation(value: &str) -> Option<NativeQueueOperation> {
    match value {
        "enqueue" => Some(NativeQueueOperation::Enqueue),
        "dequeue" => Some(NativeQueueOperation::Dequeue),
        "drain" => Some(NativeQueueOperation::Drain),
        "remove" => Some(NativeQueueOperation::Remove),
        _ => None,
    }
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// Typed `AskUserQuestion` questions. RFC 012C §11 requires a consumer to
/// render choices without parsing native payloads, so anything that does not
/// normalize into the typed shape yields no interaction at all.
fn user_input_questions(input: &Value) -> Option<Vec<UserInputQuestion>> {
    let native = input.get("questions")?.as_array()?;
    if native.is_empty() {
        return None;
    }
    let mut questions = Vec::with_capacity(native.len());
    for entry in native.iter().take(32) {
        let prompt = string_field(entry, "question").or_else(|| string_field(entry, "prompt"))?;
        if !is_semantic_text(&prompt) {
            return None;
        }
        let options = entry
            .get("options")
            .and_then(Value::as_array)
            .map(|options| {
                options
                    .iter()
                    .take(32)
                    .filter_map(|option| {
                        let label = string_field(option, "label")?;
                        is_semantic_text(&label).then(|| UserInputOption {
                            label,
                            description: string_field(option, "description")
                                .filter(|value| is_semantic_text(value)),
                            preview: string_field(option, "preview")
                                .filter(|value| is_semantic_text(value)),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        questions.push(UserInputQuestion {
            header: string_field(entry, "header").filter(|value| is_semantic_text(value)),
            prompt,
            options,
            multi_select: entry
                .get("multiSelect")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        });
    }
    (!questions.is_empty()).then_some(questions)
}

fn user_input_kind(questions: &[UserInputQuestion]) -> UserInputKind {
    let any_free_text = questions.iter().any(|question| question.options.is_empty());
    let any_multi = questions.iter().any(|question| question.multi_select);
    let any_choice = questions
        .iter()
        .any(|question| !question.options.is_empty());
    match (any_free_text, any_choice, any_multi) {
        (true, false, _) => UserInputKind::FreeText,
        (false, true, true) => UserInputKind::MultiChoice,
        (false, true, false) => UserInputKind::Choice,
        _ => UserInputKind::Mixed,
    }
}

/// One item of a declared `todos/` snapshot.
pub(crate) struct TaskSnapshotItem {
    pub native_task_id: String,
    pub subject: String,
    pub state: TaskLifecycleState,
}

/// Project a decoded todo document onto the RFC 012C task family.
///
/// Claude todo items carry no native id, so identity comes from the same
/// content-fingerprint key the snapshot family already derives — hex-encoded
/// so it is a canonical bounded string. Reusing that key is what keeps the
/// snapshot and revision views of one todo pointing at the same task.
pub(crate) fn runtime_task_items(
    items: &[crate::adapter::TaskItemSnapshot],
) -> Vec<TaskSnapshotItem> {
    items
        .iter()
        .map(|item| TaskSnapshotItem {
            native_task_id: item
                .native_task_id
                .clone()
                .filter(|value| is_semantic_text(value))
                .unwrap_or_else(|| hex_key(item.task.as_bytes())),
            subject: item.subject.clone(),
            state: task_lifecycle_state(&item.status),
        })
        .collect()
}

fn hex_key(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write;
        let _ = write!(&mut encoded, "{byte:02x}");
    }
    encoded
}

/// Normalize a Claude todo status into the RFC 012C lifecycle.
///
/// `Other` deliberately becomes `Updated`, not a guessed terminal state: an
/// unrecognized native status proves the task was touched and nothing more.
/// RFC 012C is explicit that silence is not completion.
pub(crate) fn task_lifecycle_state(status: &crate::adapter::TaskStatus) -> TaskLifecycleState {
    match status {
        crate::adapter::TaskStatus::Pending => TaskLifecycleState::Created,
        crate::adapter::TaskStatus::InProgress => TaskLifecycleState::Updated,
        crate::adapter::TaskStatus::Completed => TaskLifecycleState::Completed,
        crate::adapter::TaskStatus::Other(_) => TaskLifecycleState::Updated,
    }
}

/// Ordered plan steps from a native plan document.
///
/// Claude plans are markdown, so a step is a list item or a sub-heading. Keys
/// carry their position because two steps may share text; the position is part
/// of the declared derivation, not an accident of iteration.
pub(crate) fn plan_step_keys(content: &str) -> Vec<String> {
    let mut steps = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim_start();
        let step = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
            .or_else(|| trimmed.strip_prefix("## "))
            .or_else(|| {
                let (index, rest) = trimmed.split_once(". ")?;
                index.chars().all(|c| c.is_ascii_digit()).then_some(rest)
            });
        let Some(step) = step.map(str::trim).filter(|value| !value.is_empty()) else {
            continue;
        };
        let ordinal = steps.len();
        steps.push(format!("{ordinal}:{}", bounded_step_text(step)));
        if steps.len() == 32 {
            break;
        }
    }
    steps
}

/// A plan's subject is its first heading when it has one, and its first
/// non-empty line otherwise. Both come from the document itself; neither is a
/// generated label.
pub(crate) fn plan_subject(plan: &str) -> String {
    plan.lines()
        .find_map(|line| {
            let trimmed = line.trim();
            let heading = trimmed.trim_start_matches('#').trim();
            (!heading.is_empty()).then(|| bounded_step_text(heading))
        })
        .unwrap_or_default()
}

fn bounded_step_text(step: &str) -> String {
    const MAX_STEP_BYTES: usize = 200;
    if step.len() <= MAX_STEP_BYTES {
        return step.to_string();
    }
    let mut end = MAX_STEP_BYTES;
    while end > 0 && !step.is_char_boundary(end) {
        end -= 1;
    }
    step[..end].to_string()
}

/// Tasks from the declared todo sidecar, as an RFC 012C owned-set snapshot.
///
/// The document *is* the complete task list for its actor, which is what lets
/// a member absent from the new revision be retracted. Every revision carries
/// the owned set so the reducer can prove that absence without seeing the
/// other members.
pub(crate) fn emit_task_snapshot_runtime_facts(
    record: &SourceRecord,
    session: &CanonicalEntityKey,
    actor_run: &CanonicalEntityKey,
    items: &[TaskSnapshotItem],
    output: &mut FactBatch,
) -> Result<(), AdapterError> {
    let owned_set: Vec<String> = items
        .iter()
        .map(|item| item.native_task_id.clone())
        .collect();
    if owned_set.is_empty() {
        return Ok(());
    }
    for item in items {
        if !is_semantic_text(&item.native_task_id) || !is_semantic_text(&item.subject) {
            continue;
        }
        let fact = TaskRevisionFact {
            session: *session,
            actor_run: *actor_run,
            native_task_id: item.native_task_id.clone(),
            subject: bounded_text(&item.subject).0,
            state: item.state,
            completeness: ContractCompleteness::Complete,
            operation: UserInputOperation::Upsert,
            owned_set: Some(owned_set.clone()),
        };
        let mut key = Vec::new();
        key.extend_from_slice(b"runtime.task\0");
        key.extend_from_slice(item.native_task_id.as_bytes());
        let revision = fact.semantic_revision_key()?;
        output.push_native_object_scoped_with_revision(
            record,
            &key,
            &revision,
            Fact::TaskRevision(fact),
        )?;
    }
    Ok(())
}

/// A plan from the declared `plans/` sidecar, as a revisioned entity.
pub(crate) fn emit_plan_runtime_facts(
    record: &SourceRecord,
    session: &CanonicalEntityKey,
    actor_run: &CanonicalEntityKey,
    native_plan_id: &str,
    subject: &str,
    ordered_step_keys: Vec<String>,
    output: &mut FactBatch,
) -> Result<(), AdapterError> {
    if !is_semantic_text(native_plan_id) || ordered_step_keys.is_empty() {
        return Ok(());
    }
    let subject = bounded_text(subject).0;
    if !is_semantic_text(&subject) {
        return Ok(());
    }
    let complete = ordered_step_keys.len() <= 32;
    let fact = PlanRevisionFact {
        session: *session,
        actor_run: *actor_run,
        native_plan_id: native_plan_id.to_string(),
        subject,
        ordered_step_keys: ordered_step_keys.into_iter().take(32).collect(),
        completeness: if complete {
            ContractCompleteness::Complete
        } else {
            ContractCompleteness::Partial
        },
        operation: UserInputOperation::Upsert,
        owned_set: None,
    };
    let mut key = Vec::new();
    key.extend_from_slice(b"runtime.plan\0");
    key.extend_from_slice(native_plan_id.as_bytes());
    let revision = fact.semantic_revision_key()?;
    output.push_native_object_scoped_with_revision(
        record,
        &key,
        &revision,
        Fact::PlanRevision(fact),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;
