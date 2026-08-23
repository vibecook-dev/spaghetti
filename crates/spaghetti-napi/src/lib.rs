//! NAPI-RS bindings for the Spaghetti observation and query engine.
//!
//! This crate is the Rust side of `@vibecook/spaghetti-sdk-native`. It
//! hosts the persistent RFC 011 engine. The superseded RFC 003 bulk/live
//! writer is compiled only by the repository's `legacy-oracle` feature.
//!
//! # Layout (Phase A structural split)
//!
//! - [`core`] — source-agnostic pipeline: JSONL I/O, schema, event bus,
//!   SQLite writer / bulk FTS.
//! - [`claude`] — Claude Code–specific types, message FTS extraction,
//!   project tree walk, on-disk fingerprint discovery.
//! - [`codex`] / [`grok`] / [`factory`] — additional AgentSource native cold/warm readers.
//! - [`engine`] — persistent RFC 011 lifecycle, ownership, writer, and query
//!   workers (library-first; no Node types).
//! - [`source`] — adapter-neutral RFC 011 source drivers, provenance records,
//!   and bounded recovery scheduling.
//! - [`observer`] — RFC 012D store-free observation of one session tree:
//!   no database, declared scope only, shared decoder and reducers.
//! - `orchestrate` — feature-gated legacy differential tooling.

use napi_derive::napi;

pub mod adapter;
pub mod claude;
pub mod codex;
pub mod core;
mod coverage_runtime;
mod decode_runtime;
pub mod engine;
pub mod factory;
pub mod grok;
mod napi_catalog;
mod napi_engine;
pub mod observer;
#[cfg(feature = "legacy-oracle")]
pub mod orchestrate;
mod runtime_semantic_reducer;
// The committed RFC 012A/012C fixture graph, parsed for Rust tests only. Its
// last production consumer was the `parseRfc012*` N-API surface, which this
// lane deleted; every caller now lives under `#[cfg(test)]`.
#[cfg(test)]
mod semantic_contract;
pub mod source;
mod unknown_evidence_reducer;

pub use napi_engine::{
    open_spaghetti_engine, EngineCommitWaitOptions, EngineObservationOptions, EngineOpenOptions,
    EngineReconcileOptions, SpaghettiEngine,
};
pub use observer::{
    observe_session, ObserveSessionRequest, ObserverEvent, SpaghettiSessionObserver,
};
#[cfg(feature = "legacy-oracle")]
pub use orchestrate::ingest::{
    ingest, IngestError, IngestOptions, IngestProgress, IngestStats, IngestTask,
};
#[cfg(feature = "legacy-oracle")]
pub use orchestrate::live_ingest::{live_ingest_batch, LiveBatchResult, LiveRow, LiveRowId};

/// Returns the semver of the native addon.
#[napi]
pub fn native_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}
