//! Top-level ingest orchestrator — NAPI `ingest()` entry point.
//!
//! # Role
//!
//! Glues together the pieces built in commits 1.1–1.6 into a single sync
//! function that runs a full cold-start ingest end-to-end, and exposes it
//! to Node via an `AsyncTask` so callers await a `Promise<IngestStats>`.
//!
//! # Pipeline
//!
//! ```text
//!   scan <agent_dir>/projects/*   (main thread)
//!             │
//!             ▼
//!   for each slug: ProjectParser::parse_project(…, &sender)
//!             │
//!             │   crossbeam_channel<IngestEvent>
//!             ▼
//!   Writer::run drains channel (writer thread)
//!             │
//!             ▼
//!   Drop sender → writer sees disconnect → returns WriterStats
//!             │
//!             ▼
//!   IngestStats (main thread)
//! ```
//!
//! Commit 1.7 is single-threaded on the parser side — projects are parsed
//! sequentially. Phase 2 parallelises that with rayon.
//!
//! Warm-start (mode: 'warm') is a Phase 3 concern and is intentionally
//! not implemented here; requesting `mode: "warm"` returns an error.
//!
//! Populated in RFC 003 commit 1.7.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crossbeam_channel::{bounded, unbounded};
use napi::bindgen_prelude::Unknown;
use napi::bindgen_prelude::{AsyncTask, Env, Error, Result, Status, Task};
use napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi_derive::napi;
use rayon::prelude::*;
use rusqlite::{Connection, OpenFlags};

use crate::claude::fingerprint::{self, FingerprintStore, SourceFingerprint};
use crate::claude::project_parser::ProjectParser;
use crate::core::event::IngestEvent;
use crate::core::writer::{Writer, WriterStats};

// ═══════════════════════════════════════════════════════════════════════════
// NAPI-exposed types
// ═══════════════════════════════════════════════════════════════════════════

/// Options for [`ingest`].
///
/// Mirrors the RFC 003 `IngestOptions` TypeScript shape. Fields that RFC
/// marks optional are `Option<T>` here and get defaulted in
/// [`IngestOptions::resolved`].
#[napi(object)]
#[derive(Debug, Clone)]
pub struct IngestOptions {
    /// Agent data root on disk (e.g. `~/.claude` or `~/.codex`).
    /// Paired with [`source_id`] to select the reader and stamp rows.
    pub agent_dir: String,
    pub db_path: String,
    /// `"cold"` | `"warm"`. Warm no-ops when fingerprints are unchanged.
    pub mode: String,
    pub progress_interval_ms: Option<u32>,
    pub parallelism: Option<u32>,
    /// Agent product id stamped on every core row (default `claude-code`).
    /// Optional so existing TS callers that omit it keep working.
    pub source_id: Option<String>,
    /// When `true`, bulk ingest stays on WAL + `synchronous=NORMAL`
    /// (desktop-safe). Default / omitted = fast MEMORY + OFF path.
    pub safe_bulk: Option<bool>,
}

/// Stats returned on successful ingest.
///
/// Mirrors the RFC 003 `IngestStats` shape. Errors accumulated during
/// ingest (e.g. bad JSONL lines) are returned in `errors`; fatal errors
/// reject the promise instead.
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct IngestStats {
    pub duration_ms: u32,
    pub projects_processed: u32,
    pub sessions_processed: u32,
    pub messages_written: u32,
    pub subagents_written: u32,
    /// Non-fatal errors collected during ingest — parse failures, missing
    /// session files, etc.
    pub errors: Vec<IngestError>,
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct IngestError {
    pub slug: String,
    pub message: String,
}

/// Progress snapshot for the optional on-progress callback. Fires once
/// on start (`phase = "scanning"`, projects_total set), once per project
/// completion (`phase = "parsing"`), and once at finalization
/// (`phase = "finalizing"`). The JS side can subscribe to drive a
/// progress bar / TUI status line without having to poll.
#[napi(object)]
#[derive(Debug, Clone)]
pub struct IngestProgress {
    pub phase: String,
    pub projects_done: u32,
    pub projects_total: u32,
    pub elapsed_ms: u32,
}

// ═══════════════════════════════════════════════════════════════════════════
// NAPI entry point
// ═══════════════════════════════════════════════════════════════════════════

/// Run a full ingest of `agent_dir`, writing into the SQLite file at
/// `db_path`. Returns a Promise that resolves to [`IngestStats`] or
/// rejects with a fatal error.
///
/// Only `mode: "cold"` is implemented in Phase 1.
///
/// The optional `on_progress` callback is invoked from the libuv
/// worker thread (threadsafe) with snapshots during ingest — start,
/// per-project-complete, and finalize. Throttled implicitly by the
/// coarse "per project" granularity.
#[napi(ts_return_type = "Promise<IngestStats>")]
pub fn ingest(
    opts: IngestOptions,
    #[napi(ts_arg_type = "(progress: IngestProgress) => void")] on_progress: Option<
        ThreadsafeFunction<IngestProgress, Unknown<'static>, IngestProgress, Status, false>,
    >,
) -> AsyncTask<IngestTask> {
    AsyncTask::new(IngestTask { opts, on_progress })
}

/// Libuv worker-thread task that runs [`run_ingest`] off the JS thread.
pub struct IngestTask {
    opts: IngestOptions,
    on_progress:
        Option<ThreadsafeFunction<IngestProgress, Unknown<'static>, IngestProgress, Status, false>>,
}

impl Task for IngestTask {
    type Output = IngestStats;
    type JsValue = IngestStats;

    fn compute(&mut self) -> Result<Self::Output> {
        // Wrap the threadsafe function in a plain closure so run_ingest
        // doesn't need to depend on napi types (keeps `cargo test` from
        // linking against Node runtime symbols).
        //
        // The closure captures `tsfn` by reference — it lives on the
        // stack frame of this `compute` call, which is guaranteed to
        // outlive the synchronous `run_ingest` below.
        let tsfn = self.on_progress.as_ref();
        let callback = tsfn.map(|t| {
            move |p: IngestProgress| {
                t.call(p, ThreadsafeFunctionCallMode::NonBlocking);
            }
        });
        let callback_ref: Option<&(dyn Fn(IngestProgress) + Send + Sync)> = callback
            .as_ref()
            .map(|c| c as &(dyn Fn(IngestProgress) + Send + Sync));
        run_ingest(&self.opts, callback_ref)
            .map_err(|e| Error::new(Status::GenericFailure, e.to_string()))
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Orchestration (no NAPI types below)
// ═══════════════════════════════════════════════════════════════════════════

/// Channel capacity per parser worker. The total channel size is this
/// multiplied by `parallelism`, so a fleet of 8 parsers gets 32k slots.
/// Each slot is one `IngestEvent` (≈ 1KB for a Message variant), so the
/// memory ceiling scales with parallelism up to ~32MB — well inside the
/// desktop-app envelope.
const CHANNEL_CAPACITY_PER_WORKER: usize = 4_096;

/// Ceiling on parsing parallelism. Beyond this, contention on the single
/// SQLite writer makes additional parsers wait on `sender.send` rather
/// than doing useful CPU work.
const MAX_PARALLELISM: usize = 8;

/// Resolve the effective parser-thread count.
///
/// - `None` or `Some(0)` → `min(available_parallelism, MAX_PARALLELISM)`.
/// - `Some(n)` → clamp to `[1, MAX_PARALLELISM]`.
fn resolve_parallelism(requested: Option<u32>) -> usize {
    let default = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(MAX_PARALLELISM);
    match requested {
        None | Some(0) => default,
        Some(n) => (n as usize).clamp(1, MAX_PARALLELISM),
    }
}

/// Fatal ingest errors — these reject the NAPI promise. Non-fatal
/// per-project errors are reported via `IngestStats.errors`.
#[derive(Debug, thiserror::Error)]
pub enum IngestInternalError {
    #[error("unsupported ingest mode: {0}; expected 'cold' or 'warm'")]
    UnsupportedMode(String),

    #[error("agent root dir not found or not a directory: {0}")]
    RootDirMissing(PathBuf),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("writer error: {0}")]
    Writer(#[from] crate::core::writer::WriterError),

    #[error("fingerprint error: {0}")]
    Fingerprint(#[from] crate::claude::fingerprint::FingerprintError),

    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("writer thread panicked")]
    WriterPanic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Cold,
    Warm,
}

/// Resolve the owned / defaulted version of `IngestOptions` for internal use.
struct ResolvedOptions {
    /// Agent data root (`agent_dir` NAPI field).
    root_dir: PathBuf,
    db_path: PathBuf,
    mode: Mode,
    /// Bound into every core row via the writer.
    source_id: String,
    /// Desktop-safe bulk PRAGMAs when true.
    safe_bulk: bool,
}

impl ResolvedOptions {
    fn from(opts: &IngestOptions) -> std::result::Result<Self, IngestInternalError> {
        let mode = match opts.mode.as_str() {
            "cold" => Mode::Cold,
            "warm" => Mode::Warm,
            other => return Err(IngestInternalError::UnsupportedMode(other.to_string())),
        };
        let root_dir = PathBuf::from(&opts.agent_dir);
        if !root_dir.is_dir() {
            return Err(IngestInternalError::RootDirMissing(root_dir));
        }
        let source_id = opts
            .source_id
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(crate::core::DEFAULT_SOURCE_ID)
            .to_owned();
        Ok(Self {
            root_dir,
            db_path: PathBuf::from(&opts.db_path),
            mode,
            source_id,
            safe_bulk: opts.safe_bulk.unwrap_or(false),
        })
    }

    fn bulk_mode(&self) -> crate::core::writer::BulkMode {
        if self.safe_bulk {
            crate::core::writer::BulkMode::Safe
        } else {
            crate::core::writer::BulkMode::Fast
        }
    }
}

/// Run an ingest synchronously. Visible to integration tests.
///
/// On `Mode::Warm`: stat-checks the claude dir against the stored
/// fingerprints first. If nothing changed, returns empty stats
/// immediately (this is the common case — opening the app with a fresh
/// ~/.claude). If anything changed, falls through to a full re-ingest
/// (cold path). Future work (Phase 2 perf) may incrementalise the
/// N-changes case to touch only affected projects.
///
/// If `on_progress` is provided, fires a threadsafe callback on start
/// (`scanning`), after each project completes (`parsing`), and at
/// finalize (`finalizing`). Safe to call from any thread.
pub(crate) fn run_ingest(
    opts: &IngestOptions,
    on_progress: Option<&(dyn Fn(IngestProgress) + Send + Sync)>,
) -> std::result::Result<IngestStats, IngestInternalError> {
    let start = Instant::now();
    let resolved = ResolvedOptions::from(opts)?;

    // Codex / Grok have their own readers (RFC 006) — branch before the Claude
    // project walk so we never treat `~/.codex` / `~/.grok` as `projects/*`.
    if resolved.source_id == "codex" {
        return run_codex_ingest(&resolved, on_progress, start);
    }
    if resolved.source_id == "grok" {
        return run_grok_ingest(&resolved, on_progress, start);
    }

    // Warm-start fast path: nothing changed since last ingest → return.
    if resolved.mode == Mode::Warm && warm_has_no_changes(&resolved)? {
        return Ok(IngestStats {
            duration_ms: u32::try_from(start.elapsed().as_millis()).unwrap_or(u32::MAX),
            ..IngestStats::default()
        });
    }

    let slugs = scan_project_slugs(&resolved.root_dir)?;
    let parallelism = resolve_parallelism(opts.parallelism);

    let elapsed_ms = || u32::try_from(start.elapsed().as_millis()).unwrap_or(u32::MAX);
    let total = u32::try_from(slugs.len()).unwrap_or(u32::MAX);
    let emit = |phase: &str, done: u32| {
        if let Some(cb) = on_progress {
            cb(IngestProgress {
                phase: phase.to_string(),
                projects_done: done,
                projects_total: total,
                elapsed_ms: elapsed_ms(),
            });
        }
    };

    emit("scanning", 0);

    // Channel scales with parallelism so parsers don't constantly block
    // on a saturated buffer. The writer is still single-threaded (SQLite
    // single-writer constraint), so beyond ~8 parsers the buffer mostly
    // queues work rather than unlocking additional throughput.
    let capacity = CHANNEL_CAPACITY_PER_WORKER.saturating_mul(parallelism);
    let (sender, receiver) = bounded::<IngestEvent>(capacity);
    let db_path = resolved.db_path.clone();
    let source_id = resolved.source_id.clone();
    let bulk_mode = resolved.bulk_mode();

    let writer_handle = std::thread::Builder::new()
        .name("spaghetti-writer".into())
        .spawn(
            move || -> std::result::Result<WriterStats, crate::core::writer::WriterError> {
                let mut writer = Writer::with_source_id(&db_path, source_id)?;
                writer.open_for_bulk_ingest_with_mode(bulk_mode)?;
                let stats = writer.run(receiver)?;
                writer.finish()?;
                Ok(stats)
            },
        )
        .map_err(IngestInternalError::Io)?;

    // Emit the global plans index first — mirrors the TS engine, which
    // sends every `plans/*.md` through `sink.onPlan` before the project
    // loop (project-parser.ts `parseAllProjectsStreaming`). All plan
    // events ride one pseudo-slug transaction that we close explicitly:
    // the writer commits on `ProjectComplete` and rolls back any
    // transaction still open at channel close, so without the marker a
    // plans-only ingest (zero projects) would lose every plan.
    let plans = crate::claude::project_parser::parse_plans(&resolved.root_dir);
    if !plans.is_empty() {
        const PLANS_TX_SLUG: &str = "<plans>";
        for plan in plans {
            if sender
                .send(IngestEvent::Plan {
                    slug: PLANS_TX_SLUG.to_owned(),
                    plan,
                })
                .is_err()
            {
                break; // writer died; join below surfaces the error
            }
        }
        let _ = sender.send(IngestEvent::ProjectComplete {
            slug: PLANS_TX_SLUG.to_owned(),
            duration_ms: 0,
        });
    }

    // Parse projects in parallel via a dedicated rayon pool. Using a
    // local pool (not the global one) means we control the thread count
    // precisely and don't contend with whatever else might be using the
    // global rayon pool (e.g. later rayon-using code in the same crate).
    //
    // Parser errors below the project-boundary level are already emitted
    // as IngestEvent::WorkerError inside parse_project; here we only
    // collect ChannelClosed / other unrecoverable failures. `filter_map`
    // lets every parser finish its own project before we reduce to a
    // Vec<IngestError>.
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(parallelism)
        .thread_name(|i| format!("spaghetti-parser-{i}"))
        .build()
        .map_err(|e| {
            IngestInternalError::Io(std::io::Error::other(format!(
                "failed to build rayon pool: {e}"
            )))
        })?;

    // Capture the fingerprint set BEFORE the parse phase reads any file.
    //
    // These stats (mtime + size + byte_position=None) become the stored
    // `source_files` rows. Stat'ing *after* parsing (the previous behaviour)
    // is a TOCTOU bug: if Claude Code appends to a JSONL between our read and
    // the post-parse stat, we'd record the larger post-append size as
    // "ingested" even though we only parsed up to the pre-append point — so
    // the next warm start sees no change and the appended bytes are lost.
    // Pre-parse stats mean any concurrent append shows up as a size/mtime
    // change on the next warm start and gets re-ingested.
    let empty_store: HashMap<String, SourceFingerprint> = HashMap::new();
    let pre_parse_diff = fingerprint::compute_diff(&resolved.root_dir, &empty_store)?;

    // Serialize per-project event streams onto the shared channel so the
    // writer sees each project's events contiguously. Without this, events
    // from N parallel parsers interleave, forcing the writer to commit+
    // re-open the per-project transaction on every slug switch — which
    // both inflates the `projects_processed` counter and triggers one
    // fsync per slug flip instead of one per project.
    //
    // Each parser builds its full event stream in a local unbounded
    // channel (memory cost is bounded by project size, typically a few
    // MB), then drains it into the shared channel while holding a mutex.
    // The drain is fast — just a tight loop of enum moves — so the lock
    // is held only briefly.
    let drain_lock: Mutex<()> = Mutex::new(());
    let projects_done = Arc::new(AtomicU32::new(0));
    let errors: Vec<IngestError> = pool.install(|| {
        slugs
            .par_iter()
            .filter_map(|slug| {
                let parser = ProjectParser::new();
                let (local_tx, local_rx) = unbounded::<IngestEvent>();
                let parse_result = parser.parse_project(&resolved.root_dir, slug, &local_tx);
                drop(local_tx);

                // Drain local → shared. Holding the drain_lock keeps this
                // project's events contiguous on the shared channel. If the
                // shared sender is disconnected (writer died), we abandon
                // remaining events rather than error — the orchestrator
                // reports the parse error regardless.
                let _guard = drain_lock.lock().expect("drain_lock poisoned");
                for ev in local_rx.iter() {
                    if sender.send(ev).is_err() {
                        break;
                    }
                }
                drop(_guard);

                // Report progress per-project-complete. The granularity is
                // coarse but matches what the callback contract promises,
                // and it's sufficient for a progress bar / status line.
                let done = projects_done.fetch_add(1, Ordering::Relaxed) + 1;
                emit("parsing", done);

                parse_result.err().map(|e| IngestError {
                    slug: slug.clone(),
                    message: e.to_string(),
                })
            })
            .collect()
    });

    // Emit fingerprints for every tracked file we saw, using the stats
    // captured BEFORE the parse phase (see `pre_parse_diff` above). The writer
    // clears source_files first so stale fingerprints from prior runs (for
    // files that no longer exist) don't linger. `compute_diff` with an empty
    // store returns every discovered file in `added`, which is exactly the
    // set we need to fingerprint.
    let _ = sender.send(IngestEvent::ClearSourceFiles);
    for discovered in pre_parse_diff.added {
        let ev = IngestEvent::Fingerprint {
            path: discovered.path,
            mtime_ms: discovered.mtime_ms,
            size: discovered.size,
            byte_position: None,
            category: discovered.category,
            project_slug: discovered.project_slug,
            session_id: discovered.session_id,
        };
        if sender.send(ev).is_err() {
            break;
        }
    }

    drop(sender);
    emit("finalizing", projects_done.load(Ordering::Relaxed));

    let writer_stats: WriterStats = writer_handle
        .join()
        .map_err(|_| IngestInternalError::WriterPanic)??;

    let duration_ms = u32::try_from(start.elapsed().as_millis()).unwrap_or(u32::MAX);

    Ok(IngestStats {
        duration_ms,
        projects_processed: writer_stats.projects_processed,
        sessions_processed: writer_stats.sessions_processed,
        messages_written: writer_stats.messages_written,
        subagents_written: writer_stats.subagents_written,
        errors,
    })
}

/// Warm-start pre-check: read the stored `source_files` fingerprints
/// and diff them against the current filesystem state. Returns `true`
/// iff nothing changed (no added, no modified, no deleted files).
///
/// Opens a short-lived read-only connection so this runs on the calling
/// thread without conflicting with the writer thread (which hasn't
/// started yet). If the DB file doesn't exist or can't be opened, treat
/// as "has changes" so the caller falls through to a full cold ingest.
fn warm_has_no_changes(
    resolved: &ResolvedOptions,
) -> std::result::Result<bool, IngestInternalError> {
    if !resolved.db_path.exists() {
        return Ok(false);
    }

    let conn = Connection::open_with_flags(
        &resolved.db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;

    let store = FingerprintStore::new(&conn);
    let stored = match store.load_all() {
        Ok(s) if s.is_empty() => return Ok(false), // nothing persisted yet
        Ok(s) => s,
        Err(_) => return Ok(false), // treat any read failure as "has changes"
    };

    // Multi-source: only consider fingerprints under this source's root so
    // Codex paths don't force Claude into perpetual full re-ingest (and vice versa).
    let root_s = resolved.root_dir.to_string_lossy();
    let filtered: HashMap<String, fingerprint::SourceFingerprint> = stored
        .into_iter()
        .filter(|(p, _)| p.starts_with(root_s.as_ref()))
        .collect();
    if filtered.is_empty() {
        return Ok(false);
    }

    let diff = fingerprint::compute_diff(&resolved.root_dir, &filtered)?;
    Ok(diff.added.is_empty() && diff.modified.is_empty() && diff.deleted.is_empty())
}

/// Codex cold/warm ingest — `source_id = "codex"`.
fn run_codex_ingest(
    resolved: &ResolvedOptions,
    on_progress: Option<&(dyn Fn(IngestProgress) + Send + Sync)>,
    start: Instant,
) -> std::result::Result<IngestStats, IngestInternalError> {
    use crate::codex::CodexReader;

    let sessions_dir = resolved.root_dir.join("sessions");
    if !sessions_dir.is_dir() {
        // No Codex sessions — empty success (additive source).
        return Ok(IngestStats {
            duration_ms: u32::try_from(start.elapsed().as_millis()).unwrap_or(u32::MAX),
            ..IngestStats::default()
        });
    }

    // Warm fast-path: fingerprints under sessions/ unchanged.
    if resolved.mode == Mode::Warm && resolved.db_path.exists() {
        if let Ok(conn) = Connection::open_with_flags(
            &resolved.db_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        ) {
            let store = FingerprintStore::new(&conn);
            if let Ok(stored) = store.load_all() {
                if !stored.is_empty() && CodexReader::warm_unchanged(&sessions_dir, &stored) {
                    return Ok(IngestStats {
                        duration_ms: u32::try_from(start.elapsed().as_millis()).unwrap_or(u32::MAX),
                        ..IngestStats::default()
                    });
                }
            }
        }
    }

    let emit = |phase: &str, done: u32, total: u32| {
        if let Some(cb) = on_progress {
            cb(IngestProgress {
                phase: phase.to_string(),
                projects_done: done,
                projects_total: total,
                elapsed_ms: u32::try_from(start.elapsed().as_millis()).unwrap_or(u32::MAX),
            });
        }
    };
    emit("scanning", 0, 0);

    let (sender, receiver) = bounded::<IngestEvent>(CHANNEL_CAPACITY_PER_WORKER * 4);
    let db_path = resolved.db_path.clone();
    let source_id = resolved.source_id.clone();
    let bulk_mode = resolved.bulk_mode();

    let writer_handle = std::thread::Builder::new()
        .name("spaghetti-writer-codex".into())
        .spawn(
            move || -> std::result::Result<WriterStats, crate::core::writer::WriterError> {
                let mut writer = Writer::with_source_id(&db_path, source_id)?;
                writer.open_for_bulk_ingest_with_mode(bulk_mode)?;
                let stats = writer.run(receiver)?;
                writer.finish()?;
                Ok(stats)
            },
        )
        .map_err(IngestInternalError::Io)?;

    // Wipe this source's entity rows + fingerprints, then full read.
    // ClearSourceFiles alone left deleted rollouts as permanent orphans.
    let _ = sender.send(IngestEvent::ClearSourceData);
    let read_stats = CodexReader::read_all(&sessions_dir, &sender)
        .map_err(|e| IngestInternalError::Io(std::io::Error::other(e.to_string())))?;
    drop(sender);
    emit("finalizing", read_stats.projects, read_stats.projects);

    let writer_stats: WriterStats = writer_handle
        .join()
        .map_err(|_| IngestInternalError::WriterPanic)??;

    Ok(IngestStats {
        duration_ms: u32::try_from(start.elapsed().as_millis()).unwrap_or(u32::MAX),
        projects_processed: writer_stats.projects_processed.max(read_stats.projects),
        sessions_processed: writer_stats.sessions_processed.max(read_stats.sessions),
        messages_written: writer_stats.messages_written,
        subagents_written: 0,
        errors: vec![],
    })
}

/// Grok cold/warm ingest — `source_id = "grok"`.
fn run_grok_ingest(
    resolved: &ResolvedOptions,
    on_progress: Option<&(dyn Fn(IngestProgress) + Send + Sync)>,
    start: Instant,
) -> std::result::Result<IngestStats, IngestInternalError> {
    use crate::grok::GrokReader;

    let sessions_dir = resolved.root_dir.join("sessions");
    if !sessions_dir.is_dir() {
        // No Grok sessions — empty success (additive source).
        return Ok(IngestStats {
            duration_ms: u32::try_from(start.elapsed().as_millis()).unwrap_or(u32::MAX),
            ..IngestStats::default()
        });
    }

    // Warm fast-path: fingerprints under sessions/ unchanged.
    if resolved.mode == Mode::Warm && resolved.db_path.exists() {
        if let Ok(conn) = Connection::open_with_flags(
            &resolved.db_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        ) {
            let store = FingerprintStore::new(&conn);
            if let Ok(stored) = store.load_all() {
                if !stored.is_empty() && GrokReader::warm_unchanged(&sessions_dir, &stored) {
                    return Ok(IngestStats {
                        duration_ms: u32::try_from(start.elapsed().as_millis()).unwrap_or(u32::MAX),
                        ..IngestStats::default()
                    });
                }
            }
        }
    }

    let emit = |phase: &str, done: u32, total: u32| {
        if let Some(cb) = on_progress {
            cb(IngestProgress {
                phase: phase.to_string(),
                projects_done: done,
                projects_total: total,
                elapsed_ms: u32::try_from(start.elapsed().as_millis()).unwrap_or(u32::MAX),
            });
        }
    };
    emit("scanning", 0, 0);

    let (sender, receiver) = bounded::<IngestEvent>(CHANNEL_CAPACITY_PER_WORKER * 4);
    let db_path = resolved.db_path.clone();
    let source_id = resolved.source_id.clone();
    let bulk_mode = resolved.bulk_mode();

    let writer_handle = std::thread::Builder::new()
        .name("spaghetti-writer-grok".into())
        .spawn(
            move || -> std::result::Result<WriterStats, crate::core::writer::WriterError> {
                let mut writer = Writer::with_source_id(&db_path, source_id)?;
                writer.open_for_bulk_ingest_with_mode(bulk_mode)?;
                let stats = writer.run(receiver)?;
                writer.finish()?;
                Ok(stats)
            },
        )
        .map_err(IngestInternalError::Io)?;

    // Wipe this source's entity rows + fingerprints, then full read.
    // ClearSourceFiles alone left deleted session dirs as permanent orphans.
    let _ = sender.send(IngestEvent::ClearSourceData);
    let read_stats = GrokReader::read_all(&sessions_dir, &sender)
        .map_err(|e| IngestInternalError::Io(std::io::Error::other(e.to_string())))?;
    drop(sender);
    emit("finalizing", read_stats.projects, read_stats.projects);

    let writer_stats: WriterStats = writer_handle
        .join()
        .map_err(|_| IngestInternalError::WriterPanic)??;

    Ok(IngestStats {
        duration_ms: u32::try_from(start.elapsed().as_millis()).unwrap_or(u32::MAX),
        projects_processed: writer_stats.projects_processed.max(read_stats.projects),
        sessions_processed: writer_stats.sessions_processed.max(read_stats.sessions),
        messages_written: writer_stats.messages_written,
        subagents_written: 0,
        errors: vec![],
    })
}

/// List immediate subdirectories of `<agent_dir>/projects/`. Each dir
/// name is a project slug. Non-directory entries (e.g. `.DS_Store`) are
/// skipped silently.
fn scan_project_slugs(agent_dir: &Path) -> std::result::Result<Vec<String>, std::io::Error> {
    let projects_dir = agent_dir.join("projects");
    if !projects_dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut slugs: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(&projects_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        if let Some(name) = entry.file_name().to_str() {
            slugs.push(name.to_owned());
        }
    }
    slugs.sort();
    Ok(slugs)
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::fs;
    use tempfile::TempDir;

    /// Build a minimal fake `~/.claude` with one project, one session, and
    /// two messages. Returns the root tempdir (keep alive for the test).
    fn fake_claude_fixture() -> TempDir {
        let dir = TempDir::new().unwrap();
        let slug = "-Users-me-proj";
        let project_dir = dir.path().join("projects").join(slug);
        fs::create_dir_all(&project_dir).unwrap();

        // sessions.json with one entry
        let session_id = "11111111-2222-3333-4444-555555555555";
        let sessions_index = format!(
            r#"{{
              "originalPath": "/Users/me/proj",
              "entries": [{{
                "sessionId": "{session_id}",
                "fullPath": "{}",
                "fileMtime": 0.0,
                "firstPrompt": "hi",
                "summary": "",
                "messageCount": 2,
                "created": "2026-04-17T00:00:00Z",
                "modified": "2026-04-17T00:00:01Z",
                "gitBranch": "main",
                "projectPath": "/Users/me/proj",
                "isSidechain": false
              }}]
            }}"#,
            project_dir.join(format!("{session_id}.jsonl")).display()
        );
        fs::write(project_dir.join("sessions-index.json"), sessions_index).unwrap();

        // Session JSONL with two user messages.
        let jsonl = r#"{"type":"user","uuid":"u1","timestamp":"2026-04-17T00:00:00Z","sessionId":"11111111-2222-3333-4444-555555555555","isSidechain":false,"userType":"external","cwd":"/","version":"1","gitBranch":"main","message":{"role":"user","content":"hello"}}
{"type":"user","uuid":"u2","timestamp":"2026-04-17T00:00:01Z","sessionId":"11111111-2222-3333-4444-555555555555","isSidechain":false,"userType":"external","cwd":"/","version":"1","gitBranch":"main","message":{"role":"user","content":"world"}}
"#;
        fs::write(project_dir.join(format!("{session_id}.jsonl")), jsonl).unwrap();

        dir
    }

    #[test]
    fn rejects_unsupported_mode() {
        let opts = IngestOptions {
            agent_dir: "/tmp".into(),
            db_path: "/tmp/out.db".into(),
            mode: "incremental".into(),
            progress_interval_ms: None,
            parallelism: None,
            source_id: None,
            safe_bulk: None,
        };
        let err = run_ingest(&opts, None).expect_err("unknown mode must be rejected");
        assert!(matches!(err, IngestInternalError::UnsupportedMode(_)));
    }

    #[test]
    fn warm_mode_with_no_existing_db_falls_through_to_full_ingest() {
        let claude = fake_claude_fixture();
        let db_dir = TempDir::new().unwrap();
        let db = db_dir.path().join("spaghetti.db");

        let opts = IngestOptions {
            agent_dir: claude.path().to_string_lossy().into(),
            db_path: db.to_string_lossy().into(),
            mode: "warm".into(),
            progress_interval_ms: None,
            parallelism: None,
            source_id: None,
            safe_bulk: None,
        };

        // DB doesn't exist yet — warm mode should fall through to a cold
        // ingest rather than error.
        let stats = run_ingest(&opts, None).expect("warm ingest against fresh DB should succeed");
        assert_eq!(stats.projects_processed, 1);
        assert_eq!(stats.messages_written, 2);
    }

    #[test]
    fn warm_mode_repeat_with_no_changes_is_a_noop() {
        let claude = fake_claude_fixture();
        let db_dir = TempDir::new().unwrap();
        let db = db_dir.path().join("spaghetti.db");

        // First pass — populate the DB and source_files fingerprints.
        let first_opts = IngestOptions {
            agent_dir: claude.path().to_string_lossy().into(),
            db_path: db.to_string_lossy().into(),
            mode: "cold".into(),
            progress_interval_ms: None,
            parallelism: None,
            source_id: None,
            safe_bulk: None,
        };
        let first = run_ingest(&first_opts, None).expect("cold ingest should succeed");
        assert_eq!(first.messages_written, 2);

        // Second pass — warm, fixture unchanged. Fast path should fire:
        // zero work reported in stats.
        let warm_opts = IngestOptions {
            agent_dir: claude.path().to_string_lossy().into(),
            db_path: db.to_string_lossy().into(),
            mode: "warm".into(),
            progress_interval_ms: None,
            parallelism: None,
            source_id: None,
            safe_bulk: None,
        };
        let second = run_ingest(&warm_opts, None).expect("warm ingest should succeed");
        assert_eq!(second.projects_processed, 0);
        assert_eq!(second.sessions_processed, 0);
        assert_eq!(second.messages_written, 0);
        assert!(second.errors.is_empty());
    }

    #[test]
    fn rejects_missing_agent_dir() {
        let opts = IngestOptions {
            agent_dir: "/definitely/not/here".into(),
            db_path: "/tmp/out.db".into(),
            mode: "cold".into(),
            progress_interval_ms: None,
            parallelism: None,
            source_id: None,
            safe_bulk: None,
        };
        let err = run_ingest(&opts, None).expect_err("missing dir must error");
        assert!(matches!(err, IngestInternalError::RootDirMissing(_)));
    }

    #[test]
    fn empty_agent_dir_produces_empty_stats() {
        let tmp = TempDir::new().unwrap();
        let db = tmp.path().join("spaghetti.db");
        let opts = IngestOptions {
            agent_dir: tmp.path().to_string_lossy().into(),
            db_path: db.to_string_lossy().into(),
            mode: "cold".into(),
            progress_interval_ms: None,
            parallelism: None,
            source_id: None,
            safe_bulk: None,
        };
        let stats = run_ingest(&opts, None).unwrap();
        assert_eq!(stats.projects_processed, 0);
        assert_eq!(stats.sessions_processed, 0);
        assert_eq!(stats.messages_written, 0);
        assert!(stats.errors.is_empty());
    }

    #[test]
    fn codex_ingest_writes_messages_with_source_id() {
        let tmp = TempDir::new().unwrap();
        let sessions = tmp.path().join("sessions/2026/01/01");
        fs::create_dir_all(&sessions).unwrap();
        let rollout = sessions.join("rollout-2026-01-01T00-00-00-019bbbbbbbbbbbbbbbbbbbbbbb.jsonl");
        fs::write(
            &rollout,
            r#"{"timestamp":"2026-01-01T00:00:00.000Z","type":"session_meta","payload":{"id":"codex-sess-1","cwd":"/tmp/codex-demo"}}
{"timestamp":"2026-01-01T00:00:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hello codex"}]}}
{"timestamp":"2026-01-01T00:00:02.000Z","type":"response_item","payload":{"type":"message","role":"assistant","id":"a1","content":[{"type":"output_text","text":"hi there"}]}}
{"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":11,"output_tokens":4,"cached_input_tokens":1,"reasoning_output_tokens":0,"total_tokens":16}}}}
{"type":"response_item","payload":{"type":"function_call","name":"shell"}}
"#,
        )
        .unwrap();

        let db = tmp.path().join("codex.db");
        let opts = IngestOptions {
            agent_dir: tmp.path().to_string_lossy().into(),
            db_path: db.to_string_lossy().into(),
            mode: "cold".into(),
            progress_interval_ms: None,
            parallelism: None,
            source_id: Some("codex".into()),
            safe_bulk: None,
        };
        let stats = run_ingest(&opts, None).expect("codex ingest");
        // Writer may count ClearSourceFiles + ProjectComplete as project boundaries.
        assert!(stats.projects_processed >= 1);
        assert!(stats.sessions_processed >= 1);
        assert!(stats.messages_written >= 2);

        let conn = Connection::open(&db).unwrap();
        let sid: String = conn
            .query_row("SELECT source_id FROM projects LIMIT 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(sid, "codex");
        let msg_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE source_id = 'codex'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(msg_count, 3, "function_call must create a tool_use row");
        let tool_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE source_id = 'codex' AND msg_type = 'tool_use'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tool_count, 1);
        let tokens: (i64, i64, i64) = conn
            .query_row(
                "SELECT input_tokens, output_tokens, cache_read_tokens FROM messages WHERE msg_type = 'assistant'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(tokens, (11, 4, 1));
    }

    #[test]
    fn grok_ingest_writes_messages_with_source_id() {
        let tmp = TempDir::new().unwrap();
        let session_id = "019f5d61-da35-7b60-a1b5-02055fd8fcdd";
        let cwd = "/tmp/grok-demo";
        let session_dir = tmp
            .path()
            .join("sessions")
            .join("%2Ftmp%2Fgrok-demo")
            .join(session_id);
        fs::create_dir_all(&session_dir).unwrap();
        fs::write(
            session_dir.join("chat_history.jsonl"),
            r#"{"type":"system","content":"You are Grok."}
{"type":"user","content":[{"type":"text","text":"hello grok"}]}
{"type":"assistant","content":"hi there"}
{"type":"tool_result","tool_call_id":"c1","content":"a/\nb/"}
{"type":"reasoning","id":"rs_1","summary":[{"type":"summary_text","text":"thinking"}],"encrypted_content":"x"}
"#,
        )
        .unwrap();
        fs::write(
            session_dir.join("summary.json"),
            format!(
                r#"{{
                  "info": {{"id": "{session_id}", "cwd": "{cwd}"}},
                  "created_at": "2026-07-13T21:28:41.941460Z",
                  "updated_at": "2026-07-13T23:07:59.611347Z",
                  "generated_title": "Grok Demo",
                  "session_summary": "Grok Demo",
                  "head_branch": "main"
                }}"#
            ),
        )
        .unwrap();

        let db = tmp.path().join("grok.db");
        let opts = IngestOptions {
            agent_dir: tmp.path().to_string_lossy().into(),
            db_path: db.to_string_lossy().into(),
            mode: "cold".into(),
            progress_interval_ms: None,
            parallelism: None,
            source_id: Some("grok".into()),
            safe_bulk: None,
        };
        let stats = run_ingest(&opts, None).expect("grok ingest");
        assert!(stats.projects_processed >= 1);
        assert!(stats.sessions_processed >= 1);
        // Every canonical Grok chat-history record is retained.
        assert!(stats.messages_written >= 5);

        let conn = Connection::open(&db).unwrap();
        let sid: String = conn
            .query_row("SELECT source_id FROM projects LIMIT 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(sid, "grok");
        let slug: String = conn
            .query_row("SELECT slug FROM projects LIMIT 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(slug, "-tmp-grok-demo");
        let msg_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE source_id = 'grok'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(msg_count, 5, "tool_result must create a message row");
        let types: Vec<String> = {
            let mut stmt = conn
                .prepare("SELECT msg_type FROM messages ORDER BY msg_index")
                .unwrap();
            stmt.query_map([], |r| r.get(0))
                .unwrap()
                .map(|r| r.unwrap())
                .collect()
        };
        assert_eq!(
            types,
            vec!["system", "user", "assistant", "tool_result", "reasoning"]
        );
        let reasoning_idx: i64 = conn
            .query_row(
                "SELECT msg_index FROM messages WHERE msg_type = 'reasoning'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(reasoning_idx, 4);
        let title: String = conn
            .query_row("SELECT first_prompt FROM sessions LIMIT 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(title, "Grok Demo");
    }

    #[test]
    fn end_to_end_ingest_writes_rows_and_fts() {
        let claude = fake_claude_fixture();
        let db_dir = TempDir::new().unwrap();
        let db = db_dir.path().join("spaghetti.db");

        let opts = IngestOptions {
            agent_dir: claude.path().to_string_lossy().into(),
            db_path: db.to_string_lossy().into(),
            mode: "cold".into(),
            progress_interval_ms: None,
            parallelism: None,
            source_id: None,
            safe_bulk: None,
        };

        let stats = run_ingest(&opts, None).expect("ingest should succeed");
        assert_eq!(stats.projects_processed, 1);
        assert_eq!(stats.sessions_processed, 1);
        assert_eq!(stats.messages_written, 2);
        assert!(stats.errors.is_empty());

        // Independent read-only connection verifies persistence.
        let conn = Connection::open(&db).unwrap();
        let project_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM projects", [], |r| r.get(0))
            .unwrap();
        let session_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
            .unwrap();
        let message_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
            .unwrap();
        let fts_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM search_fts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(project_count, 1);
        assert_eq!(session_count, 1);
        assert_eq!(message_count, 2);
        assert_eq!(fts_count, 2, "FTS triggers should have synced the messages");

        // Phase B: core rows must be stamped with source_id (default claude-code).
        let sid: String = conn
            .query_row("SELECT source_id FROM projects LIMIT 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(sid, crate::core::DEFAULT_SOURCE_ID);
        let msg_sid: String = conn
            .query_row("SELECT source_id FROM messages LIMIT 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(msg_sid, crate::core::DEFAULT_SOURCE_ID);
        let fp_sid: String = conn
            .query_row("SELECT source_id FROM source_files LIMIT 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(fp_sid, crate::core::DEFAULT_SOURCE_ID);
    }
}
