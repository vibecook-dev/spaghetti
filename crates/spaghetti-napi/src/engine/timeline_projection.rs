//! Writer-owned common content-block metadata used by timeline queries.
//!
//! The canonical message's ordered `content_json` remains authoritative. This
//! projection stores only source-neutral block dimensions needed for indexed
//! filters and facets, never presentation rows or repaired tool results.

use rusqlite::{params, params_from_iter, Transaction};

use crate::adapter::ContentBlock;

use super::projection::execute_cached;
use super::EngineError;

const CONTENT_BLOCK_INSERT_BATCH_ROWS: usize = 512;

pub(super) struct MessageContentBlocks<'a> {
    pub message_key: &'a [u8],
    pub session_key: &'a [u8],
    pub run_key: &'a [u8],
    pub content: &'a [ContentBlock],
}

pub(super) fn replace_message_content_blocks(
    transaction: &Transaction<'_>,
    message_key: &[u8],
    session_key: &[u8],
    run_key: &[u8],
    content: &[ContentBlock],
    replaces_existing: bool,
) -> Result<(), EngineError> {
    if replaces_existing {
        execute_cached(
            transaction,
            "DELETE FROM canonical_message_content_blocks WHERE message_key = ?1",
            [message_key],
        )
        .map_err(|error| sqlite_error("replace canonical message content blocks", error))?;
    }

    for (ordinal, block) in content.iter().enumerate() {
        let (content_kind, tool_name, native_tool_call_id) = block_metadata(block);
        execute_cached(
            transaction,
            r#"
                INSERT INTO canonical_message_content_blocks (
                    message_key, session_key, run_key, block_ordinal,
                    content_kind, tool_name, native_tool_call_id
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                "#,
            params![
                message_key,
                session_key,
                run_key,
                sqlite_usize(ordinal)?,
                content_kind,
                tool_name,
                native_tool_call_id,
            ],
        )
        .map_err(|error| sqlite_error("index canonical message content block", error))?;
    }
    Ok(())
}

pub(super) fn insert_message_content_blocks(
    transaction: &Transaction<'_>,
    messages: &[MessageContentBlocks<'_>],
) -> Result<(), EngineError> {
    let block_count = messages.iter().map(|message| message.content.len()).sum();
    let mut rows = Vec::with_capacity(block_count);
    for message in messages {
        for (ordinal, block) in message.content.iter().enumerate() {
            rows.push((message, ordinal, block));
        }
    }
    for chunk in rows.chunks(CONTENT_BLOCK_INSERT_BATCH_ROWS) {
        let row = "(?, ?, ?, ?, ?, ?, ?)";
        let sql = format!(
            r#"
            INSERT INTO canonical_message_content_blocks (
                message_key, session_key, run_key, block_ordinal,
                content_kind, tool_name, native_tool_call_id
            ) VALUES {}
            "#,
            std::iter::repeat_n(row, chunk.len())
                .collect::<Vec<_>>()
                .join(", ")
        );
        let mut values = Vec::with_capacity(chunk.len() * 7);
        for (message, ordinal, block) in chunk {
            use rusqlite::types::Value;

            let (content_kind, tool_name, native_tool_call_id) = block_metadata(block);
            values.push(Value::Blob(message.message_key.to_vec()));
            values.push(Value::Blob(message.session_key.to_vec()));
            values.push(Value::Blob(message.run_key.to_vec()));
            values.push(Value::Integer(sqlite_usize(*ordinal)?));
            values.push(Value::Text(content_kind.to_string()));
            values.push(optional_text_value(tool_name));
            values.push(optional_text_value(native_tool_call_id));
        }
        let result = if chunk.len() == CONTENT_BLOCK_INSERT_BATCH_ROWS {
            transaction
                .prepare_cached(&sql)
                .and_then(|mut statement| statement.execute(params_from_iter(values.iter())))
        } else {
            transaction
                .prepare(&sql)
                .and_then(|mut statement| statement.execute(params_from_iter(values.iter())))
        };
        result
            .map_err(|error| sqlite_error("index canonical message content block batch", error))?;
    }
    Ok(())
}

fn block_metadata(block: &ContentBlock) -> (&'static str, Option<&str>, Option<&str>) {
    match block {
        ContentBlock::Text { .. } => ("text", None, None),
        ContentBlock::Thinking { .. } => ("thinking", None, None),
        ContentBlock::ToolCall {
            native_id, name, ..
        } => ("tool_call", Some(name), Some(native_id)),
        ContentBlock::ToolResult { native_call_id, .. } => {
            ("tool_result", None, Some(native_call_id))
        }
        ContentBlock::Image { .. } => ("image", None, None),
        ContentBlock::Document { .. } => ("document", None, None),
        ContentBlock::Native { .. } => ("native", None, None),
    }
}

fn sqlite_usize(value: usize) -> Result<i64, EngineError> {
    i64::try_from(value).map_err(|_| {
        EngineError::InvalidCommit("content block ordinal exceeds SQLite integer range".to_string())
    })
}

fn optional_text_value(value: Option<&str>) -> rusqlite::types::Value {
    value.map_or(rusqlite::types::Value::Null, |value| {
        rusqlite::types::Value::Text(value.to_string())
    })
}

fn sqlite_error(operation: &'static str, error: rusqlite::Error) -> EngineError {
    EngineError::Sqlite {
        operation,
        detail: error.to_string(),
    }
}
