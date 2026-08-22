//! Adapter-neutral source mechanics for the RFC 011 observation engine.
//!
//! Drivers in this module identify and frame native source records. They do
//! not parse agent formats, produce facts, write SQLite, or publish changes.

mod access;
mod append_delimited;
pub(crate) mod catalog_composition;
pub(crate) mod catalog_projection;
pub(crate) mod catalog_runtime_registry;
mod directory_snapshot;
mod file;
mod key_value_snapshot;
mod model;
mod presence_object;
mod recovery;
mod replace_document;
mod scheduler;
mod selector;
mod sqlite_snapshot;

#[cfg(test)]
mod conformance;

pub(crate) use access::{
    validate_evidence_locator_template, validate_relation_id,
    AuthorizedObservationDirectoryEntryReservation, AuthorizedObservationDirectoryReadAuthority,
    AuthorizedObservationDirectoryRootAuthority, AuthorizedObservationRuntimeStreamReservation,
    MAX_IDENTITY_VALUE_BYTES,
};
pub use access::{
    AccessBudget, AccessBudgetError, AccessBudgetSnapshot, AccessLimit, AccessObjectToken,
    AccessOperation, AccessOutcome, AccessPhase, AccessReservation, AccessReservationRequest,
    AccessTraceEntry, AuthorizedScopeAccessPlan, ScopeAccessBounds, ScopeAccessDenial,
    ScopeAccessPlan, ScopeAccessReport, ScopeAccessReportDigest, ScopeAccessRequest,
    ScopeAccessReservation, ScopeIdentityInput, ACCESS_TRACE_CONTRACT_VERSION,
    DEFAULT_ACCESS_TRACE_CAPACITY, SCOPE_ACCESS_REPORT_CONTRACT_VERSION,
};
pub use append_delimited::{
    AppendCheckpoint, AppendDelimitedConfig, AppendDelimitedFile, AppendItem, AppendRead,
    AppendTransition,
};
pub(crate) use directory_snapshot::{
    AuditedDirectoryScanError, DirectoryEntryAuditReservation, DirectoryEntryAuditor,
};
pub use directory_snapshot::{
    DirectoryChange, DirectoryChangeKind, DirectoryCheckpoint, DirectoryEntryKind,
    DirectoryEntryState, DirectoryScan, DirectorySelection, DirectorySelector, DirectorySnapshot,
    DirectorySnapshotConfig,
};
pub use file::platform_path_key;
pub(crate) use file::{confined_relative_path_from_key, confined_relative_path_key};
pub(crate) use file::{read_stable_file_confined, FileStamp, StableRead};
pub use key_value_snapshot::{
    KeyValueCheckpoint, KeyValueRead, KeyValueRecord, KeyValueSnapshot, KeyValueSnapshotConfig,
};
pub use model::{
    DriverQuarantine, FileIdentity, RecordHash, RecordOrigin, Revision, SourceCursor,
    SourceDriverError, SourceMediaType, SourceRecord, SourceRecordState,
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
pub(crate) use scheduler::SharedSourcePassPool;
pub use scheduler::{BoundedScheduler, IngestPriority, ScheduleOutcome, ScheduledWork, WorkKey};
pub(crate) use selector::GlobPattern;
pub use sqlite_snapshot::{
    SqliteCheckpoint, SqliteColumn, SqliteQuerySpec, SqliteRead, SqliteRowRecord, SqliteSnapshot,
    SqliteSnapshotConfig, SqliteValue,
};
