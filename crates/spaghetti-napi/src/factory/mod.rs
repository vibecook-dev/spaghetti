//! Factory.ai (Droid-style) source — candidate adapter.
//!
//! Layout: a configured data root with `sessions/*.jsonl` append transcripts
//! and optional sibling `session.json` replace documents. Catalog and scoped
//! observation remain unsupported.

mod adapter;

pub use adapter::FactoryAdapter;
