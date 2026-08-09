//! Source-agnostic ingest core: I/O, schema, event bus, SQLite writer.
//!
//! These modules do not know Claude Code's on-disk layout or Anthropic
//! message envelopes. Producers (today: [`crate::claude`]) push
//! [`event::IngestEvent`]s; the writer commits them into the shared store.
//!
//! Phase A keeps Claude-shaped *payload types* on some event variants
//! (subagent, todo, …) — those live under [`crate::claude::types`] and
//! will thin out as more sources land.
//!
//! Phase B binds every core row to a [`DEFAULT_SOURCE_ID`] (or an
//! override) so multi-source indexes stay correct when native ingest
//! shares a DB with other agents.

pub mod errors;
pub mod event;
pub mod ingest_contract;
pub mod jsonl;
pub mod schema;
pub mod text;
pub mod timefmt;
pub mod token_activity;
pub mod writer;

pub use event::IngestEvent;
pub use jsonl::{read_jsonl_streaming, JsonlError, StreamingResult};

/// Default `source_id` for native Claude Code rows — matches the SQL
/// schema DEFAULT and the TS `AgentSourceId` for Claude.
pub const DEFAULT_SOURCE_ID: &str = "claude-code";
