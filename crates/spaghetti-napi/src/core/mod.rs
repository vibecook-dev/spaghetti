//! Shared storage/text helpers for the persistent engine.
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

#[cfg(feature = "legacy-oracle")]
pub mod errors;
#[cfg(feature = "legacy-oracle")]
pub mod event;
#[cfg(feature = "legacy-oracle")]
pub mod ingest_contract;
#[cfg(feature = "legacy-oracle")]
pub mod jsonl;
#[cfg(feature = "legacy-oracle")]
mod legacy;
pub mod schema;
pub mod text;
pub mod timefmt;
#[cfg(feature = "legacy-oracle")]
pub mod token_activity;
#[cfg(feature = "legacy-oracle")]
pub mod writer;

#[cfg(feature = "legacy-oracle")]
pub use event::IngestEvent;
#[cfg(feature = "legacy-oracle")]
pub use jsonl::{read_jsonl_streaming, JsonlError, StreamingResult};
#[cfg(feature = "legacy-oracle")]
pub use legacy::DEFAULT_SOURCE_ID;
