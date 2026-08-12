//! NAPI-RS bindings for the spaghetti ingest core.
//!
//! This crate is the Rust side of `@vibecook/spaghetti-sdk-native`. It
//! hosts the ingest pipeline (RFC 003): streaming JSONL readers, SQLite
//! schema + writer, project parser, FTS text extraction, and the NAPI
//! `ingest()` entry point.
//!
//! # Layout (Phase A structural split)
//!
//! - [`core`] — source-agnostic pipeline: JSONL I/O, schema, event bus,
//!   SQLite writer / bulk FTS.
//! - [`claude`] — Claude Code–specific types, message FTS extraction,
//!   project tree walk, on-disk fingerprint discovery.
//! - [`codex`] / [`grok`] — additional AgentSource native cold/warm readers.
//! - [`engine`] — persistent RFC 011 lifecycle, ownership, writer, and query
//!   workers (library-first; no Node types).
//! - [`source`] — adapter-neutral RFC 011 source drivers, provenance records,
//!   and bounded recovery scheduling.
//! - [`orchestrate`] — NAPI entrypoints that glue cold/warm ingest
//!   and live batch writes onto the core writer.

// Dead code is expected until Phase 1 finishes wiring the orchestrator.
#![allow(dead_code)]

use napi_derive::napi;

pub mod adapter;
pub mod claude;
pub mod codex;
pub mod core;
pub mod engine;
pub mod grok;
mod napi_engine;
pub mod orchestrate;
pub mod source;

// Re-export NAPI entrypoints at the crate root so existing bindings and
// docs that name `ingest` / `live_ingest_batch` keep resolving.
pub use napi_engine::{
    open_spaghetti_engine, EngineHealth, EngineObservationOptions, EngineObservationStatus,
    EngineOpenOptions, EngineOverviewResult, EngineOwnerMetadata, EngineReconcileOptions,
    EngineReconcileResult, EngineStatus, SpaghettiEngine,
};
pub use orchestrate::ingest::{
    ingest, IngestError, IngestOptions, IngestProgress, IngestStats, IngestTask,
};
pub use orchestrate::live_ingest::{live_ingest_batch, LiveBatchResult, LiveRow, LiveRowId};

/// Returns the semver of the native addon.
#[napi]
pub fn native_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}
