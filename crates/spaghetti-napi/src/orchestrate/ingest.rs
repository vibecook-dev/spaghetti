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

use std::collections::{BTreeSet, HashMap};
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
use crate::claude::project_parser::{ParseError, ProjectParser};
use crate::core::errors::{CollectedError, ErrorReport, Severity};
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
    /// Non-fatal errors collected during ingest, capped for display. Read
    /// `error_count` for the real total — a caller that treats
    /// `errors.length` as the count will silently under-report once more
    /// than [`DISPLAY_CAP`](crate::core::errors::DISPLAY_CAP) inputs fail.
    pub errors: Vec<IngestError>,
    /// Uncapped number of errors seen.
    pub error_count: u32,
    /// True when `errors` was truncated, i.e. `error_count > errors.len()`.
    pub errors_truncated: bool,
}

/// One reported ingest failure. Matches `FrozenNativeIngestError` in
/// `packages/sdk/src/native.ts`, frozen by RFC 008 Phase 0.
#[napi(object)]
#[derive(Debug, Clone)]
pub struct IngestError {
    /// Absent for `severity = "source"`, which by definition happened before
    /// any project identity existed. Phase 0 made this optional precisely so
    /// such failures need not invent a slug.
    pub slug: Option<String>,
    /// Always present — every surfaced error can name a file even when it
    /// cannot name a project.
    pub path: String,
    /// One of `record-skip`, `project-fatal`, `source`.
    pub severity: String,
    pub message: String,
}

impl From<&crate::core::errors::CollectedError> for IngestError {
    fn from(e: &crate::core::errors::CollectedError) -> Self {
        Self {
            slug: e.slug.clone(),
            path: e.path.clone(),
            severity: e.severity.as_str().to_owned(),
            message: e.message.clone(),
        }
    }
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

/// What a writer thread hands back once the stream closes: the counters, and
/// every failure it saw on the way.
///
/// Kept together because they must be read together — `stats` alone cannot
/// tell a caller whether the run is trustworthy, which is exactly how a
/// dropped project once passed for success.
struct WriterOutcome {
    stats: WriterStats,
    errors: ErrorReport,
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
            move || -> std::result::Result<WriterOutcome, crate::core::writer::WriterError> {
                let mut writer = Writer::with_source_id(&db_path, source_id)?;
                writer.open_for_bulk_ingest_with_mode(bulk_mode)?;
                let stats = writer.run(receiver)?;
                let errors = writer.take_errors();
                writer.finish()?;
                Ok(WriterOutcome { stats, errors })
            },
        )
        .map_err(IngestInternalError::Io)?;

    // Full-source clear before the re-read (RFC 008 P1, Phase 1.1).
    //
    // Reaching this point means either a cold run or a warm run that found
    // changes, and both rebuild everything. Without the clear the rebuild was
    // upsert-only, so anything that *shrank* or *disappeared* survived: a
    // truncated session kept its dropped messages, a deleted session kept its
    // whole row, deleted sidecars kept their todos, subagents, and plans, and
    // removing `projects/` left every project indexed forever. Fingerprints
    // converged — `pre_parse_diff` re-emits the full discovered set below — but
    // the entity rows never did.
    //
    // Codex and Grok already cleared this way; Claude was the outlier.
    //
    // Ordered before the plans parse on purpose: the clear wipes the artifact
    // tables, `plans` among them, and the parse immediately below repopulates
    // them. That is also what lets independent inputs keep ingesting when
    // `projects/` is absent (Phase 1.5).
    //
    // On a cold run the DELETEs hit empty tables, so the cost is noise.
    let _ = sender.send(IngestEvent::ClearSourceData);

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
    // Record- and project-level failures travel as events and are collected
    // by the writer, which is the only consumer that sees all of them. Here we
    // collect just the one failure the event channel cannot carry: the channel
    // itself being closed. `filter_map` lets every parser finish its own
    // project before we reduce.
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
    let channel_failures: Vec<CollectedError> = pool.install(|| {
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

                match parse_result {
                    Ok(()) => None,
                    // Already on the wire as ProjectFatal + ProjectAbort; the
                    // writer records it. Reporting it here too would
                    // double-count against `error_count`.
                    Err(ParseError::Fatal { .. }) => None,
                    // The writer is gone, so nothing sent from here would
                    // arrive. This is the one failure the orchestrator must
                    // carry itself.
                    Err(e @ ParseError::ChannelClosed(_)) => Some(CollectedError {
                        slug: Some(slug.clone()),
                        path: resolved
                            .root_dir
                            .join("projects")
                            .join(slug)
                            .to_string_lossy()
                            .into_owned(),
                        severity: Severity::ProjectFatal,
                        message: e.to_string(),
                    }),
                }
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

    let WriterOutcome {
        stats: writer_stats,
        mut errors,
    } = writer_handle
        .join()
        .map_err(|_| IngestInternalError::WriterPanic)??;

    // Fold in the failures the event channel could not carry.
    for failure in channel_failures {
        errors.record(failure);
    }

    // Success-last contract publication (RFC 008 Phase 1.3).
    //
    // Everything the contract covers has now committed: the writer joined
    // without error, which means entity writes, the FTS rebuild, the token
    // rollup, and the fingerprint flush all finished. Publishing earlier — or
    // unconditionally — would let a failed run look complete, and the next warm
    // start would skip the repair it still needs.
    //
    // One report now, covering every severity — a skipped record, a
    // rolled-back project, and a source-level failure all withhold the marker.
    // Any of them means some input was not materialised, and the marker is
    // what decides whether the next warm start bothers to retry.
    publish_contract_if_clean(&resolved, &errors);

    let duration_ms = u32::try_from(start.elapsed().as_millis()).unwrap_or(u32::MAX);

    Ok(IngestStats {
        duration_ms,
        projects_processed: writer_stats.projects_processed,
        sessions_processed: writer_stats.sessions_processed,
        messages_written: writer_stats.messages_written,
        subagents_written: writer_stats.subagents_written,
        ..stats_errors(&errors)
    })
}

/// Project an [`ErrorReport`] onto the three error fields of [`IngestStats`].
fn stats_errors(errors: &ErrorReport) -> IngestStats {
    IngestStats {
        errors: errors.errors().iter().map(IngestError::from).collect(),
        error_count: errors.total(),
        errors_truncated: errors.truncated(),
        ..IngestStats::default()
    }
}

/// Mark this source as having completed under the current ingest contract,
/// but only when the run was clean.
///
/// Failing to publish is always safe — it costs one extra full re-ingest.
/// Publishing when we should not have is not: the marker is what defeats the
/// warm fast path, so a wrong `true` makes the repair unreachable. Every
/// failure path here therefore leaves the marker alone.
fn publish_contract_if_clean(resolved: &ResolvedOptions, errors: &ErrorReport) {
    if !errors.is_empty() {
        return;
    }
    let Ok(conn) = Connection::open(&resolved.db_path) else {
        return;
    };
    let _ = crate::core::ingest_contract::mark_source_contract_current(&conn, &resolved.source_id);
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

    if !crate::core::token_activity::is_materialized(&conn, &resolved.source_id).unwrap_or(false) {
        return Ok(false);
    }

    // Forced upgrade repair (RFC 008 Phase 1.3). Historical builds can leave
    // rows no fingerprint diff reveals — a parent-less sidecar written before a
    // project rolled back, say — so an older contract version has to defeat the
    // fast path even when every file matches. Absent reads as older, which is
    // the safe direction.
    if !crate::core::ingest_contract::is_source_contract_current(&conn, &resolved.source_id)
        .unwrap_or(false)
    {
        return Ok(false);
    }

    let store = FingerprintStore::new(&conn, &resolved.source_id);
    let stored = match store.load_all() {
        Ok(s) if s.is_empty() => return Ok(false), // nothing persisted yet
        Ok(s) => s,
        Err(_) => return Ok(false), // treat any read failure as "has changes"
    };

    // No path-prefix filter here. There used to be one, because `load_all` read
    // every source's rows and ownership had to be recovered from the path — the
    // exact `starts_with(root)` inference RFC 008 P8 warns against, and a real
    // hazard for roots that are string prefixes of each other (`/agent` vs
    // `/agent-old`). `load_all` is source-scoped now, so the rows are already
    // and only this source's.
    let diff = fingerprint::compute_diff(&resolved.root_dir, &stored)?;
    if !diff.added.is_empty() || !diff.modified.is_empty() || !diff.deleted.is_empty() {
        return Ok(false);
    }

    // Fingerprints track files, so a project directory holding no files at all
    // is invisible to the diff above — creating or removing one is a change no
    // file changed. A cold run indexes it anyway, because the slug list comes
    // from a directory scan, so without this check warm and cold disagree on
    // exactly the empty projects.
    let scanned: BTreeSet<String> = scan_project_slugs(&resolved.root_dir)?
        .into_iter()
        .collect();
    let mut stmt = conn.prepare("SELECT slug FROM projects WHERE source_id = ?1")?;
    let indexed = stmt
        .query_map([&resolved.source_id], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<BTreeSet<String>, _>>()?;

    Ok(scanned == indexed)
}

/// Does this source have any stored state left in the index?
///
/// Probed across every table that outlives a single run: canonical rows
/// (`projects`, `sessions`), fingerprints (`source_files`), and the
/// materialization / repair markers. Checking only sessions and fingerprints
/// would call a source "empty" while its projects or its contract marker
/// survived, and the clear would be skipped (RFC 008 Phase 1.5).
///
/// A read failure answers `true`: doing an idempotent clear we did not need is
/// cheap, while skipping one we did need leaves permanent orphans.
fn source_has_stored_state(db_path: &Path, source_id: &str) -> bool {
    if !db_path.exists() {
        return false;
    }
    let Ok(conn) = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) else {
        return true;
    };

    const PROBES: [&str; 4] = [
        "SELECT 1 FROM projects WHERE source_id = ?1 LIMIT 1",
        "SELECT 1 FROM sessions WHERE source_id = ?1 LIMIT 1",
        "SELECT 1 FROM source_files WHERE source_id = ?1 LIMIT 1",
        "SELECT 1 FROM source_materializations WHERE source_id = ?1 LIMIT 1",
    ];
    for sql in PROBES {
        match conn.query_row(sql, [source_id], |row| row.get::<_, i64>(0)) {
            Ok(_) => return true,
            Err(rusqlite::Error::QueryReturnedNoRows) => {}
            // Missing table or any other read failure — assume state exists.
            Err(_) => return true,
        }
    }
    false
}

/// Run the idempotent source clear on its own, with nothing to re-read.
///
/// Used when a source's root has disappeared: the files are gone, so the rows
/// must go too. Without this an absent root returned success and left every row
/// in place forever, because the reader had nothing to diff against.
fn clear_source_only(
    resolved: &ResolvedOptions,
    start: Instant,
) -> std::result::Result<IngestStats, IngestInternalError> {
    let (sender, receiver) = bounded::<IngestEvent>(4);
    let db_path = resolved.db_path.clone();
    let source_id = resolved.source_id.clone();

    let writer_handle = std::thread::Builder::new()
        .name("spaghetti-writer-clear".into())
        .spawn(
            move || -> std::result::Result<WriterOutcome, crate::core::writer::WriterError> {
                let mut writer = Writer::with_source_id(&db_path, source_id)?;
                let stats = writer.run(receiver)?;
                let errors = writer.take_errors();
                writer.finish()?;
                Ok(WriterOutcome { stats, errors })
            },
        )
        .map_err(IngestInternalError::Io)?;

    let _ = sender.send(IngestEvent::ClearSourceData);
    drop(sender);

    writer_handle
        .join()
        .map_err(|_| IngestInternalError::WriterPanic)??;

    // Deliberately no contract publication. The source is empty, not
    // materialised — and the clear just invalidated its marker, so a later run
    // against a restored root does the full read it needs.
    Ok(IngestStats {
        duration_ms: u32::try_from(start.elapsed().as_millis()).unwrap_or(u32::MAX),
        ..IngestStats::default()
    })
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
        // An absent root is a deletion, not a no-op. This used to return
        // success immediately, which left every Codex row indexed forever
        // once the user deleted the directory (RFC 008 Phase 1.5).
        //
        // Still cheap for the common case: a machine that never ran Codex has
        // no stored state, so the probe short-circuits and nothing is written.
        if source_has_stored_state(&resolved.db_path, &resolved.source_id) {
            return clear_source_only(resolved, start);
        }
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
            if crate::core::token_activity::is_materialized(&conn, &resolved.source_id)
                .unwrap_or(false)
                && crate::core::ingest_contract::is_source_contract_current(
                    &conn,
                    &resolved.source_id,
                )
                .unwrap_or(false)
            {
                let store = FingerprintStore::new(&conn, &resolved.source_id);
                if let Ok(stored) = store.load_all() {
                    if !stored.is_empty() && CodexReader::warm_unchanged(&sessions_dir, &stored) {
                        return Ok(IngestStats {
                            duration_ms: u32::try_from(start.elapsed().as_millis())
                                .unwrap_or(u32::MAX),
                            ..IngestStats::default()
                        });
                    }
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
            move || -> std::result::Result<WriterOutcome, crate::core::writer::WriterError> {
                let mut writer = Writer::with_source_id(&db_path, source_id)?;
                writer.open_for_bulk_ingest_with_mode(bulk_mode)?;
                let stats = writer.run(receiver)?;
                let errors = writer.take_errors();
                writer.finish()?;
                Ok(WriterOutcome { stats, errors })
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

    let WriterOutcome {
        stats: writer_stats,
        errors,
    } = writer_handle
        .join()
        .map_err(|_| IngestInternalError::WriterPanic)??;

    // Success-last, same rule and now the same error protocol as the Claude
    // path: whatever the reader reported as RecordSkip / ProjectFatal /
    // SourceError reached the writer and is in this report.
    publish_contract_if_clean(resolved, &errors);

    Ok(IngestStats {
        duration_ms: u32::try_from(start.elapsed().as_millis()).unwrap_or(u32::MAX),
        projects_processed: writer_stats.projects_processed.max(read_stats.projects),
        sessions_processed: writer_stats.sessions_processed.max(read_stats.sessions),
        messages_written: writer_stats.messages_written,
        subagents_written: 0,
        ..stats_errors(&errors)
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
        // An absent root is a deletion, not a no-op. This used to return
        // success immediately, which left every Grok row indexed forever
        // once the user deleted the directory (RFC 008 Phase 1.5).
        //
        // Still cheap for the common case: a machine that never ran Grok has
        // no stored state, so the probe short-circuits and nothing is written.
        if source_has_stored_state(&resolved.db_path, &resolved.source_id) {
            return clear_source_only(resolved, start);
        }
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
            if crate::core::token_activity::is_materialized(&conn, &resolved.source_id)
                .unwrap_or(false)
                && crate::core::ingest_contract::is_source_contract_current(
                    &conn,
                    &resolved.source_id,
                )
                .unwrap_or(false)
            {
                let store = FingerprintStore::new(&conn, &resolved.source_id);
                if let Ok(stored) = store.load_all() {
                    if !stored.is_empty() && GrokReader::warm_unchanged(&sessions_dir, &stored) {
                        return Ok(IngestStats {
                            duration_ms: u32::try_from(start.elapsed().as_millis())
                                .unwrap_or(u32::MAX),
                            ..IngestStats::default()
                        });
                    }
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
            move || -> std::result::Result<WriterOutcome, crate::core::writer::WriterError> {
                let mut writer = Writer::with_source_id(&db_path, source_id)?;
                writer.open_for_bulk_ingest_with_mode(bulk_mode)?;
                let stats = writer.run(receiver)?;
                let errors = writer.take_errors();
                writer.finish()?;
                Ok(WriterOutcome { stats, errors })
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

    let WriterOutcome {
        stats: writer_stats,
        errors,
    } = writer_handle
        .join()
        .map_err(|_| IngestInternalError::WriterPanic)??;

    // Success-last, same rule and now the same error protocol as the Claude
    // path: whatever the reader reported as RecordSkip / ProjectFatal /
    // SourceError reached the writer and is in this report.
    publish_contract_if_clean(resolved, &errors);

    Ok(IngestStats {
        duration_ms: u32::try_from(start.elapsed().as_millis()).unwrap_or(u32::MAX),
        projects_processed: writer_stats.projects_processed.max(read_stats.projects),
        sessions_processed: writer_stats.sessions_processed.max(read_stats.sessions),
        messages_written: writer_stats.messages_written,
        subagents_written: 0,
        ..stats_errors(&errors)
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
    pub(super) fn fake_claude_fixture() -> TempDir {
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
    fn warm_mode_repairs_an_incomplete_materialization_even_when_files_match() {
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

        run_ingest(&opts, None).expect("initial ingest");
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute(
                "DELETE FROM source_materializations WHERE source_id = 'claude-code'",
                [],
            )
            .unwrap();
            conn.execute(
                "DELETE FROM session_summary_totals WHERE source_id = 'claude-code'",
                [],
            )
            .unwrap();
        }

        let repaired = run_ingest(&opts, None).expect("warm repair");
        assert!(repaired.projects_processed > 0);
        let conn = Connection::open(&db).unwrap();
        assert!(crate::core::token_activity::is_materialized(&conn, "claude-code").unwrap());
        let count: i64 = conn
            .query_row(
                "SELECT SUM(parent_message_count) FROM session_summary_totals WHERE source_id = 'claude-code'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 2);
    }

    /// Build the standard warm options for a fixture + db pair.
    pub(super) fn warm_opts(agent_dir: &Path, db: &Path) -> IngestOptions {
        IngestOptions {
            agent_dir: agent_dir.to_string_lossy().into(),
            db_path: db.to_string_lossy().into(),
            mode: "warm".into(),
            progress_interval_ms: None,
            parallelism: None,
            source_id: None,
            safe_bulk: None,
        }
    }

    #[test]
    fn a_clean_ingest_publishes_the_contract_marker() {
        let claude = fake_claude_fixture();
        let db_dir = TempDir::new().unwrap();
        let db = db_dir.path().join("spaghetti.db");

        run_ingest(&warm_opts(claude.path(), &db), None).expect("initial ingest");

        let conn = Connection::open(&db).unwrap();
        assert!(
            crate::core::ingest_contract::is_source_contract_current(&conn, "claude-code").unwrap(),
            "a clean run must mark the source current, or every warm start repairs forever"
        );
    }

    #[test]
    fn a_stale_contract_version_defeats_the_warm_fast_path() {
        let claude = fake_claude_fixture();
        let db_dir = TempDir::new().unwrap();
        let db = db_dir.path().join("spaghetti.db");
        let opts = warm_opts(claude.path(), &db);

        run_ingest(&opts, None).expect("initial ingest");

        // Baseline: with the marker current and no file changes, warm is a no-op.
        let noop = run_ingest(&opts, None).expect("warm no-op");
        assert_eq!(noop.projects_processed, 0, "warm should be a no-op here");

        // Now age the marker without touching a single file. This is the case
        // fingerprints cannot see — a build whose rows are wrong in a way no
        // mtime or size reveals.
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute(
                "UPDATE source_materializations SET version = ?1 \
                 WHERE source_id = 'claude-code' AND projection = ?2",
                rusqlite::params![
                    crate::core::ingest_contract::RUST_INGEST_CONTRACT_VERSION - 1,
                    crate::core::ingest_contract::RUST_INGEST_CONTRACT
                ],
            )
            .unwrap();
        }

        let repaired = run_ingest(&opts, None).expect("forced repair");
        assert!(
            repaired.projects_processed > 0,
            "a stale contract version must force a re-ingest even when every file matches"
        );

        // And the repair republishes, so the next run is a no-op again.
        let conn = Connection::open(&db).unwrap();
        assert!(
            crate::core::ingest_contract::is_source_contract_current(&conn, "claude-code").unwrap()
        );
    }

    #[test]
    fn the_repair_is_per_source() {
        let claude = fake_claude_fixture();
        let db_dir = TempDir::new().unwrap();
        let db = db_dir.path().join("spaghetti.db");

        run_ingest(&warm_opts(claude.path(), &db), None).expect("initial ingest");

        // Mark an unrelated source current, then age Claude's marker.
        {
            let conn = Connection::open(&db).unwrap();
            crate::core::ingest_contract::mark_source_contract_current(&conn, "codex").unwrap();
            conn.execute(
                "UPDATE source_materializations SET version = ?1 \
                 WHERE source_id = 'claude-code' AND projection = ?2",
                rusqlite::params![
                    crate::core::ingest_contract::RUST_INGEST_CONTRACT_VERSION - 1,
                    crate::core::ingest_contract::RUST_INGEST_CONTRACT
                ],
            )
            .unwrap();
        }

        run_ingest(&warm_opts(claude.path(), &db), None).expect("forced repair");

        // Repairing Claude must leave Codex alone — a global marker would have
        // dragged every source through a full re-ingest.
        let conn = Connection::open(&db).unwrap();
        assert!(
            crate::core::ingest_contract::is_source_contract_current(&conn, "codex").unwrap(),
            "repairing claude-code must not invalidate codex"
        );
    }

    // ─── RFC 008 Phase 1.5 — an absent root is a deletion ──────────────────

    fn claude_row_counts(db: &Path) -> (i64, i64, i64) {
        let conn = Connection::open(db).unwrap();
        let one = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap() };
        (
            one("SELECT COUNT(*) FROM projects WHERE source_id = 'claude-code'"),
            one("SELECT COUNT(*) FROM sessions WHERE source_id = 'claude-code'"),
            one("SELECT COUNT(*) FROM source_files WHERE source_id = 'claude-code'"),
        )
    }

    #[test]
    fn deleting_every_project_clears_its_rows() {
        let claude = fake_claude_fixture();
        let db_dir = TempDir::new().unwrap();
        let db = db_dir.path().join("spaghetti.db");

        run_ingest(&warm_opts(claude.path(), &db), None).expect("initial ingest");
        let (projects, sessions, _) = claude_row_counts(&db);
        assert!(projects > 0 && sessions > 0, "fixture should have indexed");

        // The user deletes their whole projects tree.
        std::fs::remove_dir_all(claude.path().join("projects")).unwrap();

        run_ingest(&warm_opts(claude.path(), &db), None).expect("ingest after deletion");

        let (projects, sessions, files) = claude_row_counts(&db);
        assert_eq!(projects, 0, "deleted projects must not survive as orphans");
        assert_eq!(sessions, 0, "deleted sessions must not survive as orphans");
        assert_eq!(files, 0, "their fingerprints must go too");
    }

    #[test]
    fn global_plans_still_ingest_when_projects_is_absent() {
        let claude = fake_claude_fixture();
        let db_dir = TempDir::new().unwrap();
        let db = db_dir.path().join("spaghetti.db");

        run_ingest(&warm_opts(claude.path(), &db), None).expect("initial ingest");

        // Projects gone, but a global plan remains — it is an independent
        // input and must survive the deletion of everything else.
        std::fs::remove_dir_all(claude.path().join("projects")).unwrap();
        let plans_dir = claude.path().join("plans");
        std::fs::create_dir_all(&plans_dir).unwrap();
        std::fs::write(plans_dir.join("keep-me.md"), "# Keep\n").unwrap();

        run_ingest(&warm_opts(claude.path(), &db), None).expect("ingest after deletion");

        let conn = Connection::open(&db).unwrap();
        let plans: i64 = conn
            .query_row("SELECT COUNT(*) FROM plans", [], |r| r.get(0))
            .unwrap();
        assert_eq!(plans, 1, "a global plan must ingest even with no projects");
    }

    #[test]
    fn an_absent_source_with_no_stored_state_is_a_cheap_no_op() {
        // A machine that never ran Codex: no root, no rows. The probe must
        // short-circuit rather than spin up a writer to clear nothing.
        let empty = TempDir::new().unwrap();
        let db_dir = TempDir::new().unwrap();
        let db = db_dir.path().join("spaghetti.db");

        let opts = IngestOptions {
            agent_dir: empty.path().to_string_lossy().into(),
            db_path: db.to_string_lossy().into(),
            mode: "cold".into(),
            progress_interval_ms: None,
            parallelism: None,
            source_id: Some("codex".into()),
            safe_bulk: None,
        };

        let stats = run_ingest(&opts, None).expect("no-op ingest");
        assert_eq!(stats.sessions_processed, 0);
        assert!(stats.errors.is_empty());
        // No database was created for a source that has nothing.
        assert!(!db.exists(), "a pure no-op must not create the index file");
    }

    #[test]
    fn an_absent_codex_root_clears_its_rows_but_not_claudes() {
        // Exercises the clear_source_only branch, which the Claude test above
        // does not reach — that one goes through the normal event stream.
        let empty = TempDir::new().unwrap();
        let db_dir = TempDir::new().unwrap();
        let db = db_dir.path().join("spaghetti.db");

        // Seed both sources directly; building a real Codex fixture here would
        // test the reader, not the deletion branch.
        {
            let conn = Connection::open(&db).unwrap();
            crate::core::schema::initialize_schema(&conn).unwrap();
            for source in ["codex", "claude-code"] {
                conn.execute(
                    "INSERT INTO projects (slug, source_id, original_path, sessions_index, updated_at) \
                     VALUES (?1, ?2, '/x', '{}', 1)",
                    rusqlite::params![format!("proj-{source}"), source],
                )
                .unwrap();
            }
        }

        let opts = IngestOptions {
            agent_dir: empty.path().to_string_lossy().into(),
            db_path: db.to_string_lossy().into(),
            mode: "cold".into(),
            progress_interval_ms: None,
            parallelism: None,
            source_id: Some("codex".into()),
            safe_bulk: None,
        };
        run_ingest(&opts, None).expect("clear-only ingest");

        let conn = Connection::open(&db).unwrap();
        let count = |source: &str| -> i64 {
            conn.query_row(
                "SELECT COUNT(*) FROM projects WHERE source_id = ?1",
                [source],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(count("codex"), 0, "an absent codex root deletes codex rows");
        assert_eq!(count("claude-code"), 1, "and leaves claude alone");
    }

    #[test]
    fn stored_state_probe_covers_more_than_sessions() {
        let db_dir = TempDir::new().unwrap();
        let db = db_dir.path().join("probe.db");
        {
            let conn = Connection::open(&db).unwrap();
            crate::core::schema::initialize_schema(&conn).unwrap();
        }
        assert!(
            !source_has_stored_state(&db, "codex"),
            "an empty index has no state"
        );

        // A source whose only trace is its contract marker still has state —
        // probing sessions alone would call this empty and skip the clear.
        {
            let conn = Connection::open(&db).unwrap();
            crate::core::ingest_contract::mark_source_contract_current(&conn, "codex").unwrap();
        }
        assert!(source_has_stored_state(&db, "codex"));
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

// ═══════════════════════════════════════════════════════════════════════════
// RFC 008 Phase 1 exit gate — warm convergence matrix
// ═══════════════════════════════════════════════════════════════════════════
//
// The gate asks one question in several shapes: after a change on disk, does a
// warm run leave the index equal to what the files say, and is the run after
// that a true no-op?
//
// Convergence is asserted by comparing a warm run against a cold rebuild of the
// same tree. That is stronger than checking counts — it catches a warm path
// that drops rows *and* one that keeps stale ones, without hand-listing what
// each case is supposed to produce.

#[cfg(test)]
mod phase_1_gate {
    use super::tests::{fake_claude_fixture, warm_opts};
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// A comparable snapshot of everything a warm run is supposed to converge.
    #[derive(Debug, PartialEq)]
    struct Shape {
        projects: Vec<String>,
        sessions: Vec<String>,
        messages: Vec<(String, i64, String)>,
        plans: Vec<String>,
        todos: Vec<String>,
        subagents: Vec<String>,
        fingerprints: Vec<String>,
    }

    fn shape_of(db: &Path) -> Shape {
        let conn = Connection::open(db).unwrap();
        let col = |sql: &str| -> Vec<String> {
            let mut stmt = conn.prepare(sql).unwrap();
            let out = stmt
                .query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .filter_map(|r| r.ok())
                .collect();
            out
        };
        let messages = {
            let mut stmt = conn
                .prepare(
                    "SELECT session_id, msg_index, COALESCE(uuid,'') FROM messages \
                     ORDER BY session_id, msg_index",
                )
                .unwrap();
            let out = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                })
                .unwrap()
                .filter_map(|r| r.ok())
                .collect();
            out
        };

        Shape {
            projects: col("SELECT slug FROM projects ORDER BY slug"),
            sessions: col("SELECT id FROM sessions ORDER BY id"),
            messages,
            plans: col("SELECT slug FROM plans ORDER BY slug"),
            todos: col("SELECT session_id || '/' || agent_id FROM todos ORDER BY 1"),
            subagents: col("SELECT file_name FROM subagents ORDER BY file_name"),
            fingerprints: col("SELECT path FROM source_files ORDER BY path"),
        }
    }

    /// Warm-ingest `agent_dir`, assert the result equals a cold rebuild of the
    /// same tree, then assert the next warm run is a true no-op.
    fn assert_converges(agent_dir: &Path, db: &Path, case: &str) {
        run_ingest(&warm_opts(agent_dir, db), None)
            .unwrap_or_else(|e| panic!("{case}: warm failed: {e}"));
        let warm = shape_of(db);

        let fresh_dir = TempDir::new().unwrap();
        let fresh = fresh_dir.path().join("cold.db");
        let mut cold_opts = warm_opts(agent_dir, &fresh);
        cold_opts.mode = "cold".into();
        run_ingest(&cold_opts, None).unwrap_or_else(|e| panic!("{case}: cold failed: {e}"));
        let cold = shape_of(&fresh);

        assert_eq!(
            warm, cold,
            "{case}: warm did not converge on the cold result"
        );

        let again = run_ingest(&warm_opts(agent_dir, db), None)
            .unwrap_or_else(|e| panic!("{case}: second warm failed: {e}"));
        assert_eq!(
            again.projects_processed, 0,
            "{case}: a converged tree must warm-start to a no-op"
        );
        assert_eq!(shape_of(db), warm, "{case}: the no-op run changed rows");
    }

    const SESSION: &str = "11111111-2222-3333-4444-555555555555";
    const SLUG: &str = "-Users-me-proj";

    fn session_path(root: &Path) -> PathBuf {
        root.join("projects")
            .join(SLUG)
            .join(format!("{SESSION}.jsonl"))
    }

    fn session_dir(root: &Path) -> PathBuf {
        root.join("projects").join(SLUG).join(SESSION)
    }

    fn seeded() -> (TempDir, TempDir, PathBuf) {
        let claude = fake_claude_fixture();
        let db_dir = TempDir::new().unwrap();
        let db = db_dir.path().join("gate.db");
        run_ingest(&warm_opts(claude.path(), &db), None).expect("seed ingest");
        (claude, db_dir, db)
    }

    fn user_line(uuid: &str, text: &str) -> String {
        format!(
            "{{\"type\":\"user\",\"uuid\":\"{uuid}\",\"timestamp\":\"2026-04-17T00:00:09Z\",\
             \"sessionId\":\"{SESSION}\",\"isSidechain\":false,\"userType\":\"external\",\
             \"cwd\":\"/\",\"version\":\"1\",\"gitBranch\":\"main\",\
             \"message\":{{\"role\":\"user\",\"content\":\"{text}\"}}}}\n"
        )
    }

    // ─── File-level changes ────────────────────────────────────────────────

    #[test]
    fn append_converges() {
        let (claude, _d, db) = seeded();
        let p = session_path(claude.path());
        let mut body = fs::read_to_string(&p).unwrap();
        body.push_str(&user_line("u3", "third"));
        fs::write(&p, body).unwrap();

        assert_converges(claude.path(), &db, "append");
    }

    #[test]
    fn truncate_converges() {
        let (claude, _d, db) = seeded();
        let p = session_path(claude.path());
        let first = fs::read_to_string(&p).unwrap();
        let first = first.lines().next().unwrap().to_owned();
        fs::write(&p, format!("{first}\n")).unwrap();

        assert_converges(claude.path(), &db, "truncate");
    }

    #[test]
    fn rewrite_converges() {
        let (claude, _d, db) = seeded();
        let p = session_path(claude.path());
        // Same line count, different content and length — a rewrite the
        // metadata fingerprint can actually see.
        fs::write(
            &p,
            user_line("rewritten-1", "rewritten content, longer than before"),
        )
        .unwrap();

        assert_converges(claude.path(), &db, "rewrite");
    }

    #[test]
    fn session_deletion_converges() {
        let (claude, _d, db) = seeded();
        fs::remove_file(session_path(claude.path())).unwrap();

        assert_converges(claude.path(), &db, "session deletion");
    }

    #[test]
    fn empty_project_with_no_session_file_converges() {
        let (claude, _d, db) = seeded();
        // A project directory that exists but holds nothing readable.
        fs::create_dir_all(claude.path().join("projects").join("-empty-proj")).unwrap();

        assert_converges(claude.path(), &db, "empty project");
    }

    // ─── Sidecars: add, change, delete ─────────────────────────────────────

    #[test]
    fn sidecar_add_change_delete_converges() {
        let (claude, _d, db) = seeded();
        let sess = session_dir(claude.path());
        let subagents = sess.join("subagents");
        let wf = subagents.join("workflows").join("wf_1");
        let todo = claude
            .path()
            .join("todos")
            .join(format!("{SESSION}-agent-a.json"));
        let plan = claude.path().join("plans").join("p.md");

        fs::create_dir_all(&wf).unwrap();
        fs::create_dir_all(sess.join("tool-results")).unwrap();
        fs::create_dir_all(claude.path().join("todos")).unwrap();
        fs::create_dir_all(claude.path().join("plans")).unwrap();
        fs::write(subagents.join("agent-a.jsonl"), "{}\n").unwrap();
        fs::write(
            subagents.join("agent-a.meta.json"),
            "{\"agentType\":\"one\"}\n",
        )
        .unwrap();
        fs::write(wf.join("agent-nested.jsonl"), "{}\n").unwrap();
        fs::write(wf.join("journal.jsonl"), "{}\n").unwrap();
        fs::write(sess.join("tool-results").join("t1.txt"), "out\n").unwrap();
        fs::write(&todo, "[]\n").unwrap();
        fs::write(&plan, "# P\n").unwrap();
        assert_converges(claude.path(), &db, "sidecar add");

        fs::write(
            subagents.join("agent-a.meta.json"),
            "{\"agentType\":\"two-and-noticeably-longer\"}\n",
        )
        .unwrap();
        fs::write(wf.join("journal.jsonl"), "{}\n{\"grew\":true}\n").unwrap();
        fs::write(&plan, "# P, now with more content\n").unwrap();
        assert_converges(claude.path(), &db, "sidecar change");

        fs::remove_file(&todo).unwrap();
        fs::remove_file(&plan).unwrap();
        fs::remove_dir_all(&subagents).unwrap();
        assert_converges(claude.path(), &db, "sidecar delete");
    }

    // ─── Repair cases ──────────────────────────────────────────────────────

    fn age_contract(db: &Path) {
        let conn = Connection::open(db).unwrap();
        conn.execute(
            "UPDATE source_materializations SET version = ?1 \
             WHERE source_id = 'claude-code' AND projection = ?2",
            rusqlite::params![
                crate::core::ingest_contract::RUST_INGEST_CONTRACT_VERSION - 1,
                crate::core::ingest_contract::RUST_INGEST_CONTRACT
            ],
        )
        .unwrap();
    }

    fn plan_count(db: &Path, slug: &str) -> i64 {
        let conn = Connection::open(db).unwrap();
        conn.query_row("SELECT COUNT(*) FROM plans WHERE slug = ?1", [slug], |r| {
            r.get(0)
        })
        .unwrap()
    }

    #[test]
    fn seeded_orphans_survive_until_the_contract_bumps() {
        let (claude, _d, db) = seeded();

        // A row no fingerprint can explain — the historical-build case the
        // contract marker exists for. Every file on disk still matches.
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute(
                "INSERT INTO plans (slug, title, content, size, updated_at) \
                 VALUES ('orphan', 'Orphan', 'left by an older build', 1, 1)",
                [],
            )
            .unwrap();
        }

        let noop = run_ingest(&warm_opts(claude.path(), &db), None).expect("unchanged warm");
        assert_eq!(
            noop.projects_processed, 0,
            "files match, so warm is a no-op"
        );
        assert_eq!(
            plan_count(&db, "orphan"),
            1,
            "fingerprints cannot see this row — that is the premise, not a bug"
        );

        age_contract(&db);
        run_ingest(&warm_opts(claude.path(), &db), None).expect("forced repair");

        assert_eq!(
            plan_count(&db, "orphan"),
            0,
            "the repair must evict rows fingerprints cannot see"
        );
    }

    #[test]
    fn a_crash_before_contract_publication_converges_on_retry() {
        let (claude, _d, db) = seeded();

        // A run that did the work but died before publishing: the marker is
        // absent while the rows remain. Absent reads as older, so the next warm
        // run must repeat the work rather than trust the index.
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute(
                "DELETE FROM source_materializations \
                 WHERE source_id = 'claude-code' AND projection = ?1",
                [crate::core::ingest_contract::RUST_INGEST_CONTRACT],
            )
            .unwrap();
        }

        let retried = run_ingest(&warm_opts(claude.path(), &db), None).expect("retry");
        assert!(
            retried.projects_processed > 0,
            "an unpublished marker must force the work to run again"
        );

        {
            let conn = Connection::open(&db).unwrap();
            assert!(
                crate::core::ingest_contract::is_source_contract_current(&conn, "claude-code")
                    .unwrap(),
                "and the retry must publish, or every start repairs forever"
            );
        }

        let after = run_ingest(&warm_opts(claude.path(), &db), None).expect("post-retry warm");
        assert_eq!(after.projects_processed, 0, "the retry must converge");
    }

    #[test]
    fn multi_source_rows_survive_a_claude_repair() {
        let (claude, _d, db) = seeded();
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute(
                "INSERT INTO projects (slug, source_id, original_path, sessions_index, updated_at) \
                 VALUES ('codex-proj', 'codex', '/x', '{}', 1)",
                [],
            )
            .unwrap();
        }
        age_contract(&db);

        run_ingest(&warm_opts(claude.path(), &db), None).expect("claude repair");

        let conn = Connection::open(&db).unwrap();
        let codex: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM projects WHERE source_id = 'codex'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(codex, 1, "a claude repair must not touch codex rows");
    }

    #[test]
    fn a_partial_success_does_not_publish_the_contract_marker() {
        let (claude, _d, db) = seeded();
        age_contract(&db);

        // A record the parser cannot read. The run still completes and every
        // good record still ingests, which is exactly the partial success the
        // marker must not report as a finished repair.
        let path = session_path(claude.path());
        let mut body = fs::read_to_string(&path).unwrap();
        body.push_str("not-valid-json\n");
        fs::write(&path, body).unwrap();

        let stats = run_ingest(&warm_opts(claude.path(), &db), None).expect("run completes");

        let conn = Connection::open(&db).unwrap();
        assert!(
            !crate::core::ingest_contract::is_source_contract_current(&conn, "claude-code")
                .unwrap(),
            "a partial success must leave the contract unpublished so it retries"
        );

        // Phase 2 narrowed the blast radius. The same input that used to
        // discard the entire project — every good record with it — now skips
        // one record and commits the rest, and says so.
        let shape = shape_of(&db);
        assert_eq!(
            shape.messages.len(),
            2,
            "the good records around a bad line must survive"
        );
        assert_eq!(shape.projects.len(), 1, "the project must not be dropped");
        assert_eq!(stats.error_count, 1, "and the skip must be reported");
        assert_eq!(stats.errors[0].severity, "record-skip");
        assert!(!stats.errors_truncated);
    }

    #[test]
    fn project_deletion_converges() {
        let (claude, _d, db) = seeded();
        fs::remove_dir_all(claude.path().join("projects").join(SLUG)).unwrap();
        assert_converges(claude.path(), &db, "project deletion");
    }

    #[test]
    fn root_deletion_fails_loudly_and_identically_in_both_modes() {
        let (claude, _d, db) = seeded();
        let before = shape_of(&db);
        fs::remove_dir_all(claude.path()).unwrap();

        // Final root deletion is the one matrix case that deliberately does
        // NOT clear. An absent root is rejected before any source dispatch,
        // because the configured path going missing is far more often a
        // misconfiguration — wrong CLAUDE_CONFIG_DIR, unmounted volume — than
        // an intentional wipe, and clearing on it would turn a typo into
        // silent mass deletion.
        //
        // Convergence here means the two modes agree and neither mutates: an
        // absent root is refused, not half-applied. A root that still exists
        // with its contents gone is the case that clears, covered above.
        for mode in ["warm", "cold"] {
            let mut opts = warm_opts(claude.path(), &db);
            opts.mode = mode.into();
            let err = run_ingest(&opts, None)
                .expect_err(&format!("{mode}: an absent root must not succeed"));
            assert!(
                err.to_string().contains("agent root dir not found"),
                "{mode}: unexpected error: {err}"
            );
        }

        assert_eq!(shape_of(&db), before, "a refused ingest must not mutate");
    }

    #[test]
    fn plans_without_projects_converges() {
        let (claude, _d, db) = seeded();
        // `projects/` is gone but the global plans index remains. The plans
        // must survive the clear that the missing projects trigger, because
        // they are an independent input, not a child of any project.
        fs::remove_dir_all(claude.path().join("projects")).unwrap();
        let plans_dir = claude.path().join("plans");
        fs::create_dir_all(&plans_dir).unwrap();
        fs::write(plans_dir.join("keep-me.md"), "# Keep\n").unwrap();

        assert_converges(claude.path(), &db, "plans without projects");

        let conn = Connection::open(&db).unwrap();
        let plans: i64 = conn
            .query_row("SELECT COUNT(*) FROM plans", [], |r| r.get(0))
            .unwrap();
        assert_eq!(plans, 1, "global plans must still ingest without projects");
    }

    #[test]
    fn prefix_colliding_source_roots_and_duplicate_paths_converge() {
        let (claude, _d, db) = seeded();

        // Two hazards at once, both named by RFC 008 P8. `age-old` is a string
        // prefix sibling of the real root, and one seeded path sits *inside*
        // the Claude root while belonging to codex — so any ownership inferred
        // from the path rather than from source_id gets it wrong.
        let root = claude.path().to_string_lossy().to_string();
        let colliding = format!("{root}-old/projects/p/s.jsonl");
        let duplicate = session_path(claude.path()).to_string_lossy().to_string();
        {
            let conn = Connection::open(&db).unwrap();
            for path in [&colliding, &duplicate] {
                conn.execute(
                    "INSERT INTO source_files (path, source_id, mtime_ms, size, category) \
                     VALUES (?1, 'codex', 1.0, 1, 'session')",
                    [path],
                )
                .unwrap();
            }
        }

        // A converged tree must still fast-path: codex rows inside the Claude
        // root must not read as Claude files that appeared or changed.
        let stats = run_ingest(&warm_opts(claude.path(), &db), None).expect("warm");
        assert_eq!(
            stats.projects_processed, 0,
            "another source's rows must not defeat the fast path"
        );

        // And a Claude rebuild must not delete them.
        age_contract(&db);
        run_ingest(&warm_opts(claude.path(), &db), None).expect("repair");

        let conn = Connection::open(&db).unwrap();
        let survived: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM source_files WHERE source_id = 'codex'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            survived, 2,
            "a claude clear must not reach codex rows, wherever their paths point"
        );
    }
}
