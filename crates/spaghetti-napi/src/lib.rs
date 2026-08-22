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
//! - `orchestrate` — feature-gated legacy differential tooling.

// Dead code is expected until Phase 1 finishes wiring the orchestrator.
#![allow(dead_code)]

use napi_derive::napi;

pub mod adapter;
mod catalog_contract;
pub mod claude;
pub mod codex;
pub mod core;
mod coverage_runtime;
mod decode_runtime;
pub mod engine;
pub mod factory;
pub mod grok;
mod napi_engine;
mod observation_contract;
#[cfg(feature = "legacy-oracle")]
pub mod orchestrate;
mod runtime_semantic_reducer;
mod scoped_observation;
mod scoped_observation_napi;
mod scoped_observation_transport;
mod semantic_contract;
mod semantic_contract_napi;
pub mod source;
mod unknown_evidence_reducer;

pub use napi_engine::{
    open_spaghetti_engine, EngineCommitWaitOptions, EngineCommitWaitResult, EngineHealth,
    EngineObservationOptions, EngineObservationStatus, EngineOpenOptions, EngineOverviewResult,
    EngineOwnerMetadata, EngineReconcileOptions, EngineReconcileResult, EngineStatus,
    SpaghettiEngine,
};
#[cfg(feature = "legacy-oracle")]
pub use orchestrate::ingest::{
    ingest, IngestError, IngestOptions, IngestProgress, IngestStats, IngestTask,
};
#[cfg(feature = "legacy-oracle")]
pub use orchestrate::live_ingest::{live_ingest_batch, LiveBatchResult, LiveRow, LiveRowId};
pub use scoped_observation_napi::{open_scoped_observation_json, SpaghettiSessionObserver};

/// Returns the semver of the native addon.
#[napi]
pub fn native_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}
