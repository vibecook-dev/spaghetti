//! OpenAI Codex CLI source — native cold/warm ingest.
//!
//! Layout: `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`. Chat turns are
//! `response_item` + `payload.type === "message"`; everything else is
//! skipped at extraction except `event_msg/token_count`, which attributes
//! tokens onto the preceding assistant message (ccusage-style).

pub mod estimate_tokens;
pub mod message_extractor;
pub mod reader;

pub use message_extractor::{project_jsonl_line, MessageProjection};
pub use reader::CodexReader;
