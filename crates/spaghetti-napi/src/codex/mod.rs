//! OpenAI Codex CLI source — native cold/warm ingest.
//!
//! Layout: `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`. Chat turns are
//! `response_item` + `payload.type === "message"`; everything else is
//! skipped at extraction except `event_msg/token_count`, which attributes
//! tokens onto the preceding assistant message (ccusage-style).

mod adapter;
#[cfg(test)]
mod catalog_conformance;
#[cfg(test)]
pub(crate) mod catalog_runtime;
#[cfg(feature = "legacy-oracle")]
pub mod estimate_tokens;
#[cfg(feature = "legacy-oracle")]
pub mod message_extractor;
#[cfg(feature = "legacy-oracle")]
pub mod reader;

pub(crate) use adapter::verified_support_release;
pub use adapter::CodexAdapter;

#[cfg(feature = "legacy-oracle")]
pub use message_extractor::{project_jsonl_line, MessageProjection};
#[cfg(feature = "legacy-oracle")]
pub use reader::CodexReader;
