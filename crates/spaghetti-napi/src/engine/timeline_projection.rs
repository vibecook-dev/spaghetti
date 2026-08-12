//! Writer-owned common content-block metadata used by timeline queries.
//!
//! The canonical message's ordered `content_json` remains authoritative. This
//! projection stores only source-neutral block dimensions needed for indexed
//! filters and facets, never presentation rows or repaired tool results.

use rusqlite::{params, Transaction};

use crate::adapter::ContentBlock;

use super::EngineError;

pub(super) fn replace_message_content_blocks(
    transaction: &Transaction<'_>,
    message_key: &[u8],
    session_key: &[u8],
    run_key: &[u8],
    content: &[ContentBlock],
) -> Result<(), EngineError> {
    transaction
        .execute(
            "DELETE FROM canonical_message_content_blocks WHERE message_key = ?1",
            [message_key],
        )
        .map_err(|error| sqlite_error("replace canonical message content blocks", error))?;

    for (ordinal, block) in content.iter().enumerate() {
        let (content_kind, tool_name, native_tool_call_id) = block_metadata(block);
        transaction
            .execute(
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

fn sqlite_error(operation: &'static str, error: rusqlite::Error) -> EngineError {
    EngineError::Sqlite {
        operation,
        detail: error.to_string(),
    }
}
