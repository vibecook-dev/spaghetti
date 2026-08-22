//! Grok CLI (xAI) source — native cold/warm ingest.
//!
//! Layout: `~/.grok/sessions/<url-encoded-cwd>/<session-uuid>/chat_history.jsonl`
//! with sibling `summary.json` for cwd / id / title / times. All canonical
//! chat-history records are retained; the shared timeline later hides injected
//! context, expands embedded calls and pairs tool results.

mod adapter;
#[cfg(test)]
mod catalog_conformance;
#[cfg(test)]
pub(crate) mod catalog_runtime;
#[cfg(feature = "legacy-oracle")]
pub mod message_extractor;
#[cfg(feature = "legacy-oracle")]
pub mod reader;
#[cfg(feature = "legacy-oracle")]
pub mod sidecars;
pub(crate) mod support_probe;

pub(crate) use adapter::verified_support_release;
pub use adapter::GrokAdapter;

#[cfg(feature = "legacy-oracle")]
pub use message_extractor::{project_jsonl_line, MessageProjection};
#[cfg(feature = "legacy-oracle")]
pub use reader::GrokReader;
