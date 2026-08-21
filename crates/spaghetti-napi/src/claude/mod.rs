//! Claude Code agent source — layout discovery, parsers, typed messages.
//!
//! Everything here assumes `~/.claude` path shapes and Anthropic-style
//! JSONL envelopes. The multi-agent store/writer lives in [`crate::core`];
//! this module is the first (and currently only) producer for the native
//! cold/warm path.

pub mod adapter;
#[cfg(test)]
mod catalog_conformance;
#[cfg(test)]
pub(crate) mod catalog_runtime;
/// On-disk fingerprint discovery + `source_files` store helpers.
///
/// The walk is Claude-layout-specific; `FingerprintStore` itself is a
/// thin SQLite accessor and is a candidate to lift into `core` in a
/// later phase if a second source needs the same table API.
#[cfg(feature = "legacy-oracle")]
pub mod fingerprint;
pub mod fts_text;
/// Anthropic JSONL → thin message projection (RFC 006 seam).
pub mod message_extractor;
#[cfg(feature = "legacy-oracle")]
pub mod project_parser;
pub mod session_metadata;
pub(crate) mod support_probe;
pub mod types;

pub(crate) use adapter::verified_support_release;
pub use adapter::ClaudeCodeAdapter;
pub use message_extractor::{project_jsonl_line, MessageProjection};
#[cfg(feature = "legacy-oracle")]
pub use project_parser::ProjectParser;
