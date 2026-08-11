//! Adapter-neutral source mechanics for the RFC 011 observation engine.
//!
//! Drivers in this module identify and frame native source records. They do
//! not parse agent formats, produce facts, write SQLite, or publish changes.

mod append_delimited;
mod directory_snapshot;
mod file;
mod model;
mod presence_object;
mod recovery;
mod replace_document;
mod scheduler;

#[cfg(test)]
mod conformance;

pub use append_delimited::{
    AppendCheckpoint, AppendDelimitedConfig, AppendDelimitedFile, AppendItem, AppendRead,
    AppendTransition,
};
pub use directory_snapshot::{
    DirectoryChange, DirectoryChangeKind, DirectoryCheckpoint, DirectoryEntryKind,
    DirectoryEntryState, DirectoryScan, DirectorySelection, DirectorySelector, DirectorySnapshot,
    DirectorySnapshotConfig,
};
pub use file::platform_path_key;
pub use model::{
    DriverQuarantine, FileIdentity, RecordHash, RecordOrigin, Revision, SourceCursor,
    SourceDriverError, SourceMediaType, SourceRecord,
};
pub use presence_object::{
    PresenceCheckpoint, PresenceKind, PresenceObject, PresenceObjectConfig, PresenceRead,
};
pub use recovery::{
    DirtyCoalescer, DirtyHint, DirtyReason, DirtyScope, HintEnqueue, PollingPolicy, StartupAction,
    StartupPhase, WatchBeforeScan,
};
pub use replace_document::{
    MalformedRevisionGuard, MalformedRevisionPolicy, ParseFailureDecision, ReplaceCheckpoint,
    ReplaceDocument, ReplaceDocumentConfig, ReplaceRead,
};
pub use scheduler::{BoundedScheduler, IngestPriority, ScheduleOutcome, ScheduledWork, WorkKey};
