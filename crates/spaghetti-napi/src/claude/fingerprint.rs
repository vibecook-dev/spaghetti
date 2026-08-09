//! Fingerprint diff — port of the warm-start fingerprint comparison in
//! `packages/sdk/src/data/ingest-service.ts` + `agent-data-service.ts`.
//!
//! Compares on-disk state of `<root_dir>` against `source_files`
//! fingerprints stored during the last ingest, producing the
//! added / modified / deleted change set that drives the warm-start
//! incremental ingest.
//!
//! ## TS provenance
//!
//! The TS side splits this into three pieces:
//! - `IngestServiceImpl.getAllFingerprints` / `upsertFingerprint` /
//!   `deleteFingerprint` — read/write of the `source_files` row.
//! - `AgentDataServiceImpl.performWarmStart` — iterates fingerprints,
//!   `fs.stat`s each, classifies changed vs removed vs grown.
//! - `AgentDataServiceImpl.saveAllFingerprints` — walks `projects/**` to
//!   fingerprint every session JSONL and every `sessions-index.json`.
//!
//! This module collapses those into a single `compute_diff` that returns a
//! normalised added/modified/deleted set. The incremental-parse and "grown
//! JSONL" optimisation live in the caller (the Rust ingest orchestrator in
//! a later commit) — here we just classify.
//!
//! ## Categories
//!
//! The `category` column on `source_files` lets the warm-start loop know
//! how to re-parse a file without re-reading it. The taxonomy mirrors the
//! paths the TS `ProjectParserImpl` walks:
//!
//! | category        | path shape                                                       |
//! |-----------------|------------------------------------------------------------------|
//! | `session`       | `projects/<slug>/<uuid>.jsonl`                                   |
//! | `subagent`      | `projects/<slug>/<session>/subagents/agent-*.jsonl`              |
//! | `tool_result`   | `projects/<slug>/<session>/tool-results/*.txt`                   |
//! | `memory`        | `projects/<slug>/memory/MEMORY.md`                               |
//! | `sessions_index`| `projects/<slug>/sessions-index.json`                            |
//! | `todo`          | `todos/<session>-agent-<agent>.json`                             |
//! | `task`          | `tasks/<session>/.lock` or `tasks/<session>/.highwatermark`      |
//! | `file_history`  | `file-history/<session>/<hash>@v<n>`                             |
//!
//! The existing TS ingest does not fingerprint `tasks/*/.lock`,
//! `.highwatermark`, `tool-results/*.txt`, `memory/MEMORY.md`, subagent
//! JSONL, todos, or file-history snapshots — only session JSONL and
//! `sessions-index.json` make it into `source_files` today. The Rust port
//! expands coverage (RFC 003 § "Warm start" calls for full coverage so
//! warm starts catch *any* artefact change, not just session appends). We
//! emit rows for every discovered file; the caller decides which ones to
//! write back.

use std::collections::HashMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use rusqlite::Connection;
use walkdir::WalkDir;

// ─── Category constants ─────────────────────────────────────────────────────

/// `projects/<slug>/<uuid>.jsonl`.
pub const CATEGORY_SESSION: &str = "session";
/// `projects/<slug>/<session>/subagents/agent-*.jsonl`.
pub const CATEGORY_SUBAGENT: &str = "subagent";
/// `projects/<slug>/<session>/tool-results/*.txt`.
pub const CATEGORY_TOOL_RESULT: &str = "tool_result";
/// `projects/<slug>/memory/MEMORY.md`.
pub const CATEGORY_MEMORY: &str = "memory";
/// `projects/<slug>/sessions-index.json`.
pub const CATEGORY_SESSIONS_INDEX: &str = "sessions_index";
/// `todos/<session>-agent-<agent>.json`.
pub const CATEGORY_TODO: &str = "todo";
/// `tasks/<session>/.lock` or `.highwatermark`.
pub const CATEGORY_TASK: &str = "task";
/// `file-history/<session>/<hash>@v<n>`.
pub const CATEGORY_FILE_HISTORY: &str = "file_history";
/// `plans/<slug>.md` — global, not project-scoped.
pub const CATEGORY_PLAN: &str = "plan";
/// `projects/<slug>/<session>/subagents/**/agent-*.meta.json`.
pub const CATEGORY_SUBAGENT_META: &str = "subagent_meta";
/// `projects/<slug>/<session>/subagents/workflows/<wf>/journal.jsonl`.
pub const CATEGORY_WORKFLOW_JOURNAL: &str = "workflow_journal";

// ─── Regex patterns — copied verbatim from project-parser.ts ────────────────

/// Matches canonical session file names `<uuid>.jsonl`.
static UUID_JSONL: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\.jsonl$")
        .expect("UUID_JSONL regex compiles")
});

/// Matches `agent-<id>.jsonl`. Port of `extractAgentId`'s pattern.
static SUBAGENT_FILE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^agent-(.+)\.jsonl$").expect("SUBAGENT_FILE regex compiles"));

/// Matches the `agent-<id>.meta.json` sidecar the parser reads for agent type
/// and description.
static SUBAGENT_META_FILE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^agent-(.+)\.meta\.json$").expect("SUBAGENT_META_FILE regex compiles")
});

/// Matches file-history snapshot names `<hash>@v<n>`.
static FILE_HISTORY_SNAPSHOT: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^([0-9a-f]+)@v(\d+)$").expect("FILE_HISTORY_SNAPSHOT regex compiles")
});

/// Matches todo file names `<session-id>-agent-<agent-id>.json`.
static TODO_FILE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^(.+?)-agent-(.+)\.json$").expect("TODO_FILE regex compiles"));

// ─── Errors ─────────────────────────────────────────────────────────────────

/// Errors produced by the fingerprint module.
#[derive(Debug, thiserror::Error)]
pub enum FingerprintError {
    /// An underlying SQLite error while loading fingerprints.
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// A filesystem error while walking or stating a file. Discovery
    /// tolerates missing subtrees (a missing `projects/` is fine), but a
    /// `metadata()` failure on a file we just enumerated is surfaced.
    #[error("i/o error at {path}: {source}")]
    Io {
        /// The path we were touching when the error occurred.
        path: PathBuf,
        /// The underlying `std::io::Error`.
        #[source]
        source: std::io::Error,
    },
}

// ─── Public types ──────────────────────────────────────────────────────────

/// A row in `source_files` — one fingerprint per tracked on-disk file.
///
/// Mirrors the TS `SourceFingerprint` shape plus the `category /
/// project_slug / session_id` columns that live in the table DDL but
/// aren't surfaced in the TS struct yet.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceFingerprint {
    /// Absolute path to the file.
    pub path: String,
    /// File mtime in milliseconds since the Unix epoch.
    pub mtime_ms: f64,
    /// File size in bytes.
    pub size: u64,
    /// For JSONL session files, the byte offset we last finished reading
    /// at. Used by the warm-start incremental path to resume reads instead
    /// of re-parsing from the top.
    pub byte_position: Option<u64>,
    /// One of the `CATEGORY_*` constants above.
    pub category: String,
    /// The containing project slug, if applicable (`projects/<slug>/...`).
    pub project_slug: Option<String>,
    /// The containing session id, if applicable.
    pub session_id: Option<String>,
}

/// A file discovered on disk during the warm-start scan that has no
/// corresponding `source_files` row yet.
#[derive(Debug, Clone, PartialEq)]
pub struct DiscoveredFile {
    /// Absolute path.
    pub path: String,
    /// `stat()`ed mtime.
    pub mtime_ms: f64,
    /// `stat()`ed size.
    pub size: u64,
    /// Category (see `CATEGORY_*`).
    pub category: String,
    /// Project slug if derivable from the path.
    pub project_slug: Option<String>,
    /// Session id if derivable from the path.
    pub session_id: Option<String>,
}

/// A file that is present both on disk and in `source_files`, but whose
/// on-disk mtime or size no longer matches the stored fingerprint.
#[derive(Debug, Clone, PartialEq)]
pub struct ModifiedFile {
    /// Absolute path.
    pub path: String,
    /// Fresh `stat()`ed mtime.
    pub new_mtime_ms: f64,
    /// Fresh `stat()`ed size.
    pub new_size: u64,
    /// The `byte_position` column from the stored row. For a growing
    /// JSONL this is where incremental parsing should resume; for other
    /// categories it's `None`.
    pub prior_byte_position: Option<u64>,
    /// Category.
    pub category: String,
    /// Project slug.
    pub project_slug: Option<String>,
    /// Session id.
    pub session_id: Option<String>,
}

/// The full added / modified / deleted classification returned by
/// [`compute_diff`].
#[derive(Debug, Default, Clone, PartialEq)]
pub struct FingerprintDiff {
    /// Files present on disk but not in `source_files`.
    pub added: Vec<DiscoveredFile>,
    /// Files present in both, with mismatching mtime or size.
    pub modified: Vec<ModifiedFile>,
    /// Fingerprint paths that no longer exist on disk.
    pub deleted: Vec<String>,
    /// Count of files where on-disk mtime+size matched the fingerprint
    /// exactly — useful for "nothing to do" short-circuits in the caller.
    pub unchanged_count: u32,
}

// ─── FingerprintStore — read side ──────────────────────────────────────────

/// Read-only accessor over the `source_files` table, scoped to one source.
///
/// Holds a borrow of an existing `rusqlite::Connection` — does not own
/// the connection so tests and the real ingest orchestrator can share one.
///
/// The source is a constructor argument rather than a per-call one because
/// every read has to be scoped: `source_files` is keyed `(source_id, path)`,
/// and two agents can hold the same absolute path. Unscoped, this handed
/// Codex's warm-unchanged check Claude's fingerprints (RFC 008 P8).
pub struct FingerprintStore<'a> {
    conn: &'a Connection,
    source_id: String,
}

impl<'a> FingerprintStore<'a> {
    /// Wrap a connection for one source. The caller is responsible for ensuring
    /// the schema is initialised (`source_files` must exist).
    pub fn new(conn: &'a Connection, source_id: impl Into<String>) -> Self {
        Self {
            conn,
            source_id: source_id.into(),
        }
    }

    /// Load this source's rows from `source_files` into a path-keyed map.
    ///
    /// Mirrors `IngestServiceImpl.getAllFingerprints` but also pulls the
    /// `category / project_slug / session_id` columns so the diff can
    /// route modifications back to the right parser without re-walking
    /// the path.
    ///
    /// Keying the map by path alone is safe *because* the query is
    /// source-scoped; the same path under two sources would otherwise collide
    /// silently and the later row would win.
    pub fn load_all(&self) -> Result<HashMap<String, SourceFingerprint>, FingerprintError> {
        let mut stmt = self.conn.prepare(
            "SELECT path, mtime_ms, size, byte_position, category, project_slug, session_id \
             FROM source_files WHERE source_id = ?1",
        )?;
        let rows = stmt.query_map([&self.source_id], |row| {
            Ok(SourceFingerprint {
                path: row.get::<_, String>(0)?,
                mtime_ms: row.get::<_, f64>(1).unwrap_or(0.0),
                size: row
                    .get::<_, Option<i64>>(2)?
                    .map(|v| v.max(0) as u64)
                    .unwrap_or(0),
                byte_position: row.get::<_, Option<i64>>(3)?.map(|v| v.max(0) as u64),
                category: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
                project_slug: row.get::<_, Option<String>>(5)?,
                session_id: row.get::<_, Option<String>>(6)?,
            })
        })?;

        let mut out = HashMap::new();
        for row in rows {
            let fp = row?;
            out.insert(fp.path.clone(), fp);
        }
        Ok(out)
    }
}

// ─── Discovery + diff ──────────────────────────────────────────────────────

/// Walk `<root_dir>` and diff discovered files against `stored`.
///
/// See the module docs for the taxonomy and per-category path shapes. The
/// result is a pure change set — no filesystem writes, no DB writes.
pub fn compute_diff(
    root_dir: &Path,
    stored: &HashMap<String, SourceFingerprint>,
) -> Result<FingerprintDiff, FingerprintError> {
    let discovered = discover_all(root_dir)?;

    let mut diff = FingerprintDiff::default();
    let mut seen_paths: std::collections::HashSet<&str> =
        std::collections::HashSet::with_capacity(discovered.len());

    for file in &discovered {
        seen_paths.insert(file.path.as_str());

        match stored.get(&file.path) {
            None => {
                diff.added.push(file.clone());
            }
            Some(prior) => {
                // Any mtime delta (not just a newer mtime) or a size change
                // flags the file as modified — matches the TS check
                // `stats.mtimeMs !== fp.mtimeMs || stats.size !== fp.size` in
                // `performWarmStart` (lifecycle-owner.ts). A file whose mtime
                // moved *backwards* (clock reset, restored-from-backup, git
                // checkout) still needs re-ingest, which `>` would have missed.
                let mtime_changed = file.mtime_ms != prior.mtime_ms;
                let size_diff = file.size != prior.size;
                if mtime_changed || size_diff {
                    diff.modified.push(ModifiedFile {
                        path: file.path.clone(),
                        new_mtime_ms: file.mtime_ms,
                        new_size: file.size,
                        prior_byte_position: prior.byte_position,
                        category: file.category.clone(),
                        project_slug: file.project_slug.clone(),
                        session_id: file.session_id.clone(),
                    });
                } else {
                    diff.unchanged_count += 1;
                }
            }
        }
    }

    for path in stored.keys() {
        if !seen_paths.contains(path.as_str()) {
            diff.deleted.push(path.clone());
        }
    }

    // Deterministic ordering eases testing and keeps log output stable.
    diff.added.sort_by(|a, b| a.path.cmp(&b.path));
    diff.modified.sort_by(|a, b| a.path.cmp(&b.path));
    diff.deleted.sort();

    Ok(diff)
}

// ─── Internal: discovery scan ──────────────────────────────────────────────

/// Walk the five top-level subtrees we care about and collect every file
/// that matches one of the recognised category shapes.
///
/// Uses explicit subtree walks instead of a single `WalkDir(root_dir)`
/// for two reasons:
/// 1. The TS parser scans exactly these subpaths with fixed glob patterns
///    — mirroring that keeps behaviour aligned across the Node / native
///    implementations.
/// 2. A `~/.claude` tree contains unrelated dirs (`plugins/`, `shell/`,
///    `ide/`, `statsig/`, etc.) we do *not* want to fingerprint. An
///    allow-listed walk is O(files we care about), not O(everything).
fn discover_all(root_dir: &Path) -> Result<Vec<DiscoveredFile>, FingerprintError> {
    let mut out = Vec::new();

    discover_projects(&root_dir.join("projects"), &mut out)?;
    discover_todos(&root_dir.join("todos"), &mut out)?;
    discover_tasks(&root_dir.join("tasks"), &mut out)?;
    discover_file_history(&root_dir.join("file-history"), &mut out)?;
    discover_plans(&root_dir.join("plans"), &mut out)?;

    Ok(out)
}

/// `plans/*.md` — global plans, not project-scoped.
///
/// The parser reads these via `parse_plans`, but discovery never fingerprinted
/// them, so an edited plan was invisible to warm start (RFC 008 Phase 1.4).
fn discover_plans(plans_dir: &Path, out: &mut Vec<DiscoveredFile>) -> Result<(), FingerprintError> {
    let entries = match fs::read_dir(plans_dir) {
        Ok(e) => e,
        Err(_) => return Ok(()), // no plans dir is absence, not an error
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        push_file(out, &path, CATEGORY_PLAN, None, None)?;
    }

    Ok(())
}

/// Scan `<root_dir>/projects/<slug>/*` for every artefact category we
/// recognise. A missing `projects/` dir is not an error — matches the
/// TS `try { ... } catch { /* projects dir doesn't exist */ }` pattern.
fn discover_projects(
    projects_dir: &Path,
    out: &mut Vec<DiscoveredFile>,
) -> Result<(), FingerprintError> {
    let entries = match fs::read_dir(projects_dir) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };

    for entry in entries.flatten() {
        let project_path = entry.path();
        if !project_path.is_dir() {
            continue;
        }
        let Some(slug) = project_path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let slug = slug.to_owned();

        scan_project_root(&project_path, &slug, out)?;
        scan_project_memory(&project_path, &slug, out)?;
        scan_project_sessions(&project_path, &slug, out)?;
    }

    Ok(())
}

/// The direct children of `projects/<slug>/`: session JSONLs and the
/// `sessions-index.json`.
fn scan_project_root(
    project_dir: &Path,
    slug: &str,
    out: &mut Vec<DiscoveredFile>,
) -> Result<(), FingerprintError> {
    let entries = match fs::read_dir(project_dir) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };

        if UUID_JSONL.is_match(name) {
            let session_id = name.trim_end_matches(".jsonl").to_owned();
            push_file(
                out,
                &path,
                CATEGORY_SESSION,
                Some(slug.to_owned()),
                Some(session_id),
            )?;
        } else if name == "sessions-index.json" {
            push_file(
                out,
                &path,
                CATEGORY_SESSIONS_INDEX,
                Some(slug.to_owned()),
                None,
            )?;
        }
    }

    Ok(())
}

/// `projects/<slug>/memory/MEMORY.md`.
fn scan_project_memory(
    project_dir: &Path,
    slug: &str,
    out: &mut Vec<DiscoveredFile>,
) -> Result<(), FingerprintError> {
    let memory = project_dir.join("memory").join("MEMORY.md");
    if memory.is_file() {
        push_file(out, &memory, CATEGORY_MEMORY, Some(slug.to_owned()), None)?;
    }
    Ok(())
}

/// The `projects/<slug>/<session>/subagents/...` and
/// `.../tool-results/...` subtrees. We only descend into session
/// directories whose name matches the UUID pattern — anything else is
/// noise (e.g. the `memory/` dir handled above).
fn scan_project_sessions(
    project_dir: &Path,
    slug: &str,
    out: &mut Vec<DiscoveredFile>,
) -> Result<(), FingerprintError> {
    let entries = match fs::read_dir(project_dir) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };

    // Session-subdir names are UUIDs; derive by stripping `.jsonl` off the
    // `UUID_JSONL` pattern, i.e. they match the same hex-dashed shape.
    static UUID_BARE: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$")
            .expect("UUID_BARE regex compiles")
    });

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if !UUID_BARE.is_match(name) {
            continue;
        }
        let session_id = name.to_owned();

        scan_subagents(&path.join("subagents"), slug, &session_id, out)?;
        scan_tool_results(&path.join("tool-results"), slug, &session_id, out)?;
    }

    Ok(())
}

/// `subagents/` dir — flat `agent-*.jsonl`, their `.meta.json` sidecars, and
/// the nested `workflows/<wf_id>/` tree.
///
/// The parser reads all four shapes (`read_one_subagent` opens the sibling
/// `.meta.json`; `read_workflow_journal` opens `journal.jsonl`; the nested walk
/// picks up workflow-orchestrated transcripts). Discovery previously saw only
/// the flat `agent-*.jsonl`, so a changed agent type, a changed journal, or a
/// whole workflow's transcripts were invisible to warm start — RFC 008
/// Phase 1.4.
fn scan_subagents(
    dir: &Path,
    slug: &str,
    session_id: &str,
    out: &mut Vec<DiscoveredFile>,
) -> Result<(), FingerprintError> {
    scan_agent_files(dir, slug, session_id, out)?;

    // Nested: subagents/workflows/<wf_id>/{agent-*.jsonl, agent-*.meta.json,
    // journal.jsonl}. Mirrors the parser's walk in `read_subagents`.
    let workflows = dir.join("workflows");
    if let Ok(wf_dirs) = fs::read_dir(&workflows) {
        for wf_entry in wf_dirs.flatten() {
            let wf_path = wf_entry.path();
            if !wf_path.is_dir() {
                continue;
            }
            scan_agent_files(&wf_path, slug, session_id, out)?;

            let journal = wf_path.join("journal.jsonl");
            if journal.is_file() {
                push_file(
                    out,
                    &journal,
                    CATEGORY_WORKFLOW_JOURNAL,
                    Some(slug.to_owned()),
                    Some(session_id.to_owned()),
                )?;
            }
        }
    }

    Ok(())
}

/// One directory's `agent-*.jsonl` transcripts plus their `.meta.json` sidecars.
///
/// The sidecar gets its own category rather than riding along with the
/// transcript: it changes independently (an agent's type or description can be
/// rewritten without the transcript growing), and a fingerprint that tracked
/// only the transcript would miss it.
fn scan_agent_files(
    dir: &Path,
    slug: &str,
    session_id: &str,
    out: &mut Vec<DiscoveredFile>,
) -> Result<(), FingerprintError> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if SUBAGENT_FILE.is_match(name) {
            push_file(
                out,
                &path,
                CATEGORY_SUBAGENT,
                Some(slug.to_owned()),
                Some(session_id.to_owned()),
            )?;
        } else if SUBAGENT_META_FILE.is_match(name) {
            push_file(
                out,
                &path,
                CATEGORY_SUBAGENT_META,
                Some(slug.to_owned()),
                Some(session_id.to_owned()),
            )?;
        }
    }

    Ok(())
}

/// `tool-results/` dir — any `*.txt` file.
fn scan_tool_results(
    dir: &Path,
    slug: &str,
    session_id: &str,
    out: &mut Vec<DiscoveredFile>,
) -> Result<(), FingerprintError> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if name.ends_with(".txt") {
            push_file(
                out,
                &path,
                CATEGORY_TOOL_RESULT,
                Some(slug.to_owned()),
                Some(session_id.to_owned()),
            )?;
        }
    }

    Ok(())
}

/// `todos/` — flat directory of `<session>-agent-<agent>.json` files.
fn discover_todos(todos_dir: &Path, out: &mut Vec<DiscoveredFile>) -> Result<(), FingerprintError> {
    let entries = match fs::read_dir(todos_dir) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if let Some(caps) = TODO_FILE.captures(name) {
            let session_id = caps.get(1).map(|m| m.as_str().to_owned());
            push_file(out, &path, CATEGORY_TODO, None, session_id)?;
        }
    }

    Ok(())
}

/// `tasks/<session>/{.lock,.highwatermark}`. We recurse one level and emit
/// an entry per dotfile — the TS parser uses both `.lock` existence and
/// `.highwatermark` content, so both need fingerprints.
fn discover_tasks(tasks_dir: &Path, out: &mut Vec<DiscoveredFile>) -> Result<(), FingerprintError> {
    let entries = match fs::read_dir(tasks_dir) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };

    for entry in entries.flatten() {
        let session_dir = entry.path();
        if !session_dir.is_dir() {
            continue;
        }
        let Some(session_id) = session_dir.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let session_id = session_id.to_owned();

        for task_file in [".lock", ".highwatermark"] {
            let path = session_dir.join(task_file);
            if path.is_file() {
                push_file(out, &path, CATEGORY_TASK, None, Some(session_id.clone()))?;
            }
        }
    }

    Ok(())
}

/// `file-history/<session>/<hash>@v<n>` — flat per-session directories
/// full of snapshot blobs.
fn discover_file_history(
    root: &Path,
    out: &mut Vec<DiscoveredFile>,
) -> Result<(), FingerprintError> {
    let walker = match fs::read_dir(root) {
        Ok(_) => WalkDir::new(root).min_depth(2).max_depth(2),
        Err(_) => return Ok(()),
    };

    for entry in walker.into_iter().filter_map(Result::ok) {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.into_path();
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if !FILE_HISTORY_SNAPSHOT.is_match(name) {
            continue;
        }
        // Session id is the parent directory name.
        let session_id = path.components().rev().nth(1).and_then(|c| match c {
            Component::Normal(s) => s.to_str().map(str::to_owned),
            _ => None,
        });

        push_file(out, &path, CATEGORY_FILE_HISTORY, None, session_id)?;
    }

    Ok(())
}

/// Stat `path` and push a `DiscoveredFile` with the given classification.
fn push_file(
    out: &mut Vec<DiscoveredFile>,
    path: &Path,
    category: &str,
    project_slug: Option<String>,
    session_id: Option<String>,
) -> Result<(), FingerprintError> {
    // A file enumerated by read_dir can vanish before we stat it (Claude Code
    // rotates/deletes transcripts concurrently). Treat NotFound as "the file
    // is absent" — skip it so the diff naturally sees it as deleted — but
    // surface any other IO error (permissions, etc.) unchanged rather than
    // aborting the whole ingest.
    let meta = match fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => {
            return Err(FingerprintError::Io {
                path: path.to_path_buf(),
                source: e,
            })
        }
    };
    let mtime_ms = mtime_to_ms(&meta);
    let size = meta.len();

    // We store absolute paths — canonicalise only if the passed-in path
    // is already absolute; otherwise `to_string_lossy()` is fine for the
    // caller's purposes. Any canonicalisation races with concurrent
    // filesystem edits and adds no value here.
    let path_str = path.to_string_lossy().into_owned();

    out.push(DiscoveredFile {
        path: path_str,
        mtime_ms,
        size,
        category: category.to_owned(),
        project_slug,
        session_id,
    });
    Ok(())
}

/// Convert a `std::fs::Metadata.modified()` into an `f64` of milliseconds
/// since the Unix epoch, matching JS `fs.Stats.mtimeMs`.
fn mtime_to_ms(meta: &fs::Metadata) -> f64 {
    match meta.modified() {
        Ok(st) => match st.duration_since(std::time::UNIX_EPOCH) {
            Ok(d) => {
                // Millisecond precision; JS's `mtimeMs` is a float that
                // carries sub-ms as a fractional part.
                (d.as_secs() as f64) * 1000.0 + (d.subsec_nanos() as f64) / 1_000_000.0
            }
            Err(_) => 0.0,
        },
        Err(_) => 0.0,
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::fs as stdfs;
    use std::io::Write;
    use std::path::PathBuf;

    // ─── Test helpers ──────────────────────────────────────────────────────

    /// Set up the minimal `source_files` schema our tests need. We don't
    /// run the full schema module here — that would pull every table in
    /// for no reason. The key mirrors the real one: `(source_id, path)`, so
    /// these tests exercise the same ownership rules production does.
    fn init_source_files(conn: &Connection) {
        conn.execute_batch(
            "CREATE TABLE source_files (\
               path TEXT NOT NULL,\
               source_id TEXT NOT NULL DEFAULT 'claude-code',\
               mtime_ms REAL,\
               size INTEGER,\
               byte_position INTEGER,\
               category TEXT,\
               project_slug TEXT,\
               session_id TEXT,\
               PRIMARY KEY (source_id, path)\
             )",
        )
        .expect("create source_files");
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_fp(
        conn: &Connection,
        path: &str,
        mtime_ms: f64,
        size: u64,
        byte_position: Option<u64>,
        category: &str,
        project_slug: Option<&str>,
        session_id: Option<&str>,
    ) {
        conn.execute(
            "INSERT INTO source_files \
             (path, mtime_ms, size, byte_position, category, project_slug, session_id) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                path,
                mtime_ms,
                size as i64,
                byte_position.map(|v| v as i64),
                category,
                project_slug,
                session_id,
            ],
        )
        .expect("insert fingerprint");
    }

    /// Insert a row owned by an explicit source. Only the fields the ownership
    /// tests care about; everything else is a placeholder.
    fn insert_fp_for(conn: &Connection, source_id: &str, path: &str, size: u64) {
        conn.execute(
            "INSERT INTO source_files \
             (path, source_id, mtime_ms, size, byte_position, category, project_slug, session_id) \
             VALUES (?1, ?2, 1.0, ?3, NULL, 'session', NULL, NULL)",
            rusqlite::params![path, source_id, size as i64],
        )
        .expect("insert fingerprint for source");
    }

    #[test]
    fn store_load_all_is_scoped_to_its_source() {
        let conn = Connection::open_in_memory().expect("open mem db");
        init_source_files(&conn);

        // The same absolute path under two sources — legal, and the reason the
        // key is (source_id, path). An unscoped load would collapse these into
        // one map entry and hand Codex whichever row happened to win.
        insert_fp_for(&conn, "claude-code", "/repo/shared/a.jsonl", 100);
        insert_fp_for(&conn, "codex", "/repo/shared/a.jsonl", 999);
        insert_fp_for(&conn, "grok", "/only/grok.jsonl", 7);

        let claude = FingerprintStore::new(&conn, "claude-code")
            .load_all()
            .expect("load claude");
        let codex = FingerprintStore::new(&conn, "codex")
            .load_all()
            .expect("load codex");
        let grok = FingerprintStore::new(&conn, "grok")
            .load_all()
            .expect("load grok");

        assert_eq!(claude.len(), 1);
        assert_eq!(claude["/repo/shared/a.jsonl"].size, 100);

        assert_eq!(codex.len(), 1);
        assert_eq!(codex["/repo/shared/a.jsonl"].size, 999);

        assert_eq!(grok.len(), 1);
        assert!(!grok.contains_key("/repo/shared/a.jsonl"));
    }

    #[test]
    fn store_load_all_is_empty_for_an_unknown_source() {
        let conn = Connection::open_in_memory().expect("open mem db");
        init_source_files(&conn);
        insert_fp_for(&conn, "claude-code", "/a.jsonl", 1);

        // A source with no rows must read as empty, not as "everything".
        // Warm-unchanged treats an empty map as "nothing persisted yet".
        let unknown = FingerprintStore::new(&conn, "not-a-source")
            .load_all()
            .expect("load unknown");
        assert!(unknown.is_empty());
    }

    /// Write a file and its parent directories.
    fn write_file(path: &Path, content: &[u8]) {
        if let Some(parent) = path.parent() {
            stdfs::create_dir_all(parent).expect("mkdir");
        }
        let mut f = stdfs::File::create(path).expect("create file");
        f.write_all(content).expect("write");
    }

    /// Stat a file and return `(mtime_ms, size)` as `compute_diff` will
    /// see them. Used to seed the store with fingerprints that exactly
    /// match reality.
    fn stat_ms_size(path: &Path) -> (f64, u64) {
        let meta = stdfs::metadata(path).expect("stat");
        (mtime_to_ms(&meta), meta.len())
    }

    fn uuid_jsonl(idx: u8) -> String {
        format!("aaaaaaaa-bbbb-cccc-dddd-00000000000{idx:x}.jsonl")
    }

    fn uuid_bare(idx: u8) -> String {
        format!("aaaaaaaa-bbbb-cccc-dddd-00000000000{idx:x}")
    }

    // ─── compute_diff tests ────────────────────────────────────────────────

    #[test]
    fn empty_dir_empty_store_yields_empty_diff() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let stored = HashMap::new();

        let diff = compute_diff(tmp.path(), &stored).expect("diff");

        assert!(diff.added.is_empty());
        assert!(diff.modified.is_empty());
        assert!(diff.deleted.is_empty());
        assert_eq!(diff.unchanged_count, 0);
    }

    #[test]
    fn empty_store_three_session_files_all_added() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let projects = tmp.path().join("projects").join("my-project");
        for i in 0..3u8 {
            write_file(&projects.join(uuid_jsonl(i)), b"{}\n");
        }

        let stored = HashMap::new();
        let diff = compute_diff(tmp.path(), &stored).expect("diff");

        assert_eq!(diff.added.len(), 3, "three new sessions -> 3 added");
        assert!(diff.modified.is_empty());
        assert!(diff.deleted.is_empty());
        assert_eq!(diff.unchanged_count, 0);

        for a in &diff.added {
            assert_eq!(a.category, CATEGORY_SESSION);
            assert_eq!(a.project_slug.as_deref(), Some("my-project"));
            assert!(a.session_id.is_some());
        }
    }

    #[test]
    fn matching_store_and_dir_yields_all_unchanged() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let projects = tmp.path().join("projects").join("p");
        let mut paths: Vec<PathBuf> = Vec::new();
        for i in 0..3u8 {
            let p = projects.join(uuid_jsonl(i));
            write_file(&p, b"hello\n");
            paths.push(p);
        }

        let mut stored = HashMap::new();
        for (i, p) in paths.iter().enumerate() {
            let (mtime, size) = stat_ms_size(p);
            let s = uuid_bare(i as u8);
            stored.insert(
                p.to_string_lossy().into_owned(),
                SourceFingerprint {
                    path: p.to_string_lossy().into_owned(),
                    mtime_ms: mtime,
                    size,
                    byte_position: Some(size),
                    category: CATEGORY_SESSION.to_owned(),
                    project_slug: Some("p".to_owned()),
                    session_id: Some(s),
                },
            );
        }

        let diff = compute_diff(tmp.path(), &stored).expect("diff");

        assert!(diff.added.is_empty(), "added: {:?}", diff.added);
        assert!(diff.modified.is_empty(), "modified: {:?}", diff.modified);
        assert!(diff.deleted.is_empty(), "deleted: {:?}", diff.deleted);
        assert_eq!(diff.unchanged_count, 3);
    }

    #[test]
    fn stale_mtime_marks_file_modified_and_carries_byte_position() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let projects = tmp.path().join("projects").join("p");
        let mut paths: Vec<PathBuf> = Vec::new();
        for i in 0..3u8 {
            let p = projects.join(uuid_jsonl(i));
            write_file(&p, b"x\n");
            paths.push(p);
        }

        // Seed all three with CURRENT mtime; then manually backdate the
        // store entry for file[1] so the on-disk mtime looks "newer".
        let mut stored = HashMap::new();
        for (i, p) in paths.iter().enumerate() {
            let (mtime, size) = stat_ms_size(p);
            let stale_mtime = if i == 1 { mtime - 10_000.0 } else { mtime };
            stored.insert(
                p.to_string_lossy().into_owned(),
                SourceFingerprint {
                    path: p.to_string_lossy().into_owned(),
                    mtime_ms: stale_mtime,
                    size,
                    byte_position: Some(42),
                    category: CATEGORY_SESSION.to_owned(),
                    project_slug: Some("p".to_owned()),
                    session_id: Some(uuid_bare(i as u8)),
                },
            );
        }

        let diff = compute_diff(tmp.path(), &stored).expect("diff");

        assert!(diff.added.is_empty());
        assert!(diff.deleted.is_empty());
        assert_eq!(diff.modified.len(), 1, "one stale-mtime -> 1 modified");
        assert_eq!(diff.unchanged_count, 2);

        let m = &diff.modified[0];
        assert_eq!(m.prior_byte_position, Some(42));
        assert_eq!(m.category, CATEGORY_SESSION);
    }

    #[test]
    fn older_on_disk_mtime_flags_file_modified() {
        // TS uses `!==`, so an mtime that moved *backwards* (stored newer than
        // disk) must still be flagged modified. The old `>` check reported it
        // as unchanged, silently skipping re-ingest.
        let tmp = tempfile::tempdir().expect("tempdir");
        let projects = tmp.path().join("projects").join("p");
        let p = projects.join(uuid_jsonl(0));
        write_file(&p, b"x\n");

        let (mtime, size) = stat_ms_size(&p);
        let mut stored = HashMap::new();
        stored.insert(
            p.to_string_lossy().into_owned(),
            SourceFingerprint {
                path: p.to_string_lossy().into_owned(),
                // Stored mtime is 10s NEWER than the on-disk mtime.
                mtime_ms: mtime + 10_000.0,
                size,
                byte_position: Some(7),
                category: CATEGORY_SESSION.to_owned(),
                project_slug: Some("p".to_owned()),
                session_id: Some(uuid_bare(0)),
            },
        );

        let diff = compute_diff(tmp.path(), &stored).expect("diff");
        assert_eq!(diff.modified.len(), 1, "backwards mtime -> modified");
        assert_eq!(diff.unchanged_count, 0);
        assert_eq!(diff.modified[0].prior_byte_position, Some(7));
    }

    #[test]
    fn push_file_skips_vanished_file() {
        // A file that no longer exists when stat'd must be skipped (so the diff
        // treats it as deleted) rather than aborting the whole scan with an
        // IO error.
        let mut out = Vec::new();
        let missing = PathBuf::from("/no/such/dir/vanished-abcdef.jsonl");
        push_file(&mut out, &missing, CATEGORY_SESSION, None, None)
            .expect("NotFound must be skipped, not surfaced as an error");
        assert!(out.is_empty(), "vanished file must not be recorded");
    }

    #[test]
    fn size_change_alone_flags_modified() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let projects = tmp.path().join("projects").join("p");
        let mut paths: Vec<PathBuf> = Vec::new();
        for i in 0..3u8 {
            let p = projects.join(uuid_jsonl(i));
            write_file(&p, b"initial\n");
            paths.push(p);
        }

        // Seed fingerprints with current mtime; lie about file[2]'s size.
        let mut stored = HashMap::new();
        for (i, p) in paths.iter().enumerate() {
            let (mtime, size) = stat_ms_size(p);
            let fake_size = if i == 2 { size + 999 } else { size };
            stored.insert(
                p.to_string_lossy().into_owned(),
                SourceFingerprint {
                    path: p.to_string_lossy().into_owned(),
                    mtime_ms: mtime,
                    size: fake_size,
                    byte_position: Some(100),
                    category: CATEGORY_SESSION.to_owned(),
                    project_slug: Some("p".to_owned()),
                    session_id: Some(uuid_bare(i as u8)),
                },
            );
        }

        let diff = compute_diff(tmp.path(), &stored).expect("diff");

        assert!(diff.added.is_empty());
        assert!(diff.deleted.is_empty());
        assert_eq!(diff.modified.len(), 1, "size-only change -> 1 modified");
        assert_eq!(diff.unchanged_count, 2);
        assert_eq!(diff.modified[0].prior_byte_position, Some(100));
    }

    #[test]
    fn missing_file_reports_deleted() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let projects = tmp.path().join("projects").join("p");
        let mut paths: Vec<PathBuf> = Vec::new();
        for i in 0..2u8 {
            let p = projects.join(uuid_jsonl(i));
            write_file(&p, b"hi\n");
            paths.push(p);
        }

        // Build a store with THREE entries; only two of them exist on disk.
        let mut stored = HashMap::new();
        for (i, p) in paths.iter().enumerate() {
            let (mtime, size) = stat_ms_size(p);
            stored.insert(
                p.to_string_lossy().into_owned(),
                SourceFingerprint {
                    path: p.to_string_lossy().into_owned(),
                    mtime_ms: mtime,
                    size,
                    byte_position: None,
                    category: CATEGORY_SESSION.to_owned(),
                    project_slug: Some("p".to_owned()),
                    session_id: Some(uuid_bare(i as u8)),
                },
            );
        }
        let ghost = projects.join(uuid_jsonl(9)).to_string_lossy().into_owned();
        stored.insert(
            ghost.clone(),
            SourceFingerprint {
                path: ghost.clone(),
                mtime_ms: 1.0,
                size: 1,
                byte_position: None,
                category: CATEGORY_SESSION.to_owned(),
                project_slug: Some("p".to_owned()),
                session_id: Some(uuid_bare(9)),
            },
        );

        let diff = compute_diff(tmp.path(), &stored).expect("diff");

        assert!(diff.added.is_empty());
        assert!(diff.modified.is_empty());
        assert_eq!(diff.deleted, vec![ghost]);
        assert_eq!(diff.unchanged_count, 2);
    }

    #[test]
    fn subagent_file_is_discovered_with_session_id() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = uuid_bare(1);
        let subagent = tmp
            .path()
            .join("projects")
            .join("s")
            .join(&session)
            .join("subagents")
            .join("agent-abc.jsonl");
        write_file(&subagent, b"{}\n");

        let diff = compute_diff(tmp.path(), &HashMap::new()).expect("diff");

        assert_eq!(diff.added.len(), 1);
        let a = &diff.added[0];
        assert_eq!(a.category, CATEGORY_SUBAGENT);
        assert_eq!(a.project_slug.as_deref(), Some("s"));
        assert_eq!(a.session_id.as_deref(), Some(session.as_str()));
    }

    // ─── RFC 008 Phase 1.4 — inputs the parser reads must be fingerprinted ──
    //
    // Each of these was consumed by the parser but invisible to discovery, so
    // editing one left warm start convinced nothing had changed.

    #[test]
    fn global_plans_are_discovered() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_file(&tmp.path().join("plans").join("my-plan.md"), b"# Plan\n");
        // Non-markdown siblings are not plan inputs.
        write_file(&tmp.path().join("plans").join("notes.txt"), b"ignore me\n");

        let diff = compute_diff(tmp.path(), &HashMap::new()).expect("diff");

        assert_eq!(diff.added.len(), 1);
        assert_eq!(diff.added[0].category, CATEGORY_PLAN);
        assert!(
            diff.added[0].project_slug.is_none(),
            "plans are global, not project-scoped"
        );
    }

    #[test]
    fn subagent_meta_sidecar_is_discovered_separately_from_its_transcript() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = uuid_bare(7);
        let dir = tmp
            .path()
            .join("projects")
            .join("s")
            .join(&session)
            .join("subagents");
        write_file(&dir.join("agent-abc.jsonl"), b"{}\n");
        write_file(&dir.join("agent-abc.meta.json"), b"{\"type\":\"x\"}\n");

        let diff = compute_diff(tmp.path(), &HashMap::new()).expect("diff");

        let mut categories: Vec<&str> = diff.added.iter().map(|a| a.category.as_str()).collect();
        categories.sort_unstable();
        // Separate rows: the sidecar's agent type can be rewritten without the
        // transcript growing, so one fingerprint could not cover both.
        assert_eq!(categories, vec![CATEGORY_SUBAGENT, CATEGORY_SUBAGENT_META]);
    }

    #[test]
    fn nested_workflow_transcripts_and_journal_are_discovered() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = uuid_bare(8);
        let wf = tmp
            .path()
            .join("projects")
            .join("s")
            .join(&session)
            .join("subagents")
            .join("workflows")
            .join("wf_1");
        write_file(&wf.join("agent-nested.jsonl"), b"{}\n");
        write_file(&wf.join("agent-nested.meta.json"), b"{}\n");
        write_file(&wf.join("journal.jsonl"), b"{}\n");

        let diff = compute_diff(tmp.path(), &HashMap::new()).expect("diff");

        let mut categories: Vec<&str> = diff.added.iter().map(|a| a.category.as_str()).collect();
        categories.sort_unstable();
        assert_eq!(
            categories,
            vec![
                CATEGORY_SUBAGENT,
                CATEGORY_SUBAGENT_META,
                CATEGORY_WORKFLOW_JOURNAL
            ]
        );
        // Session attribution survives the nesting — the workflow dir sits two
        // levels below `subagents/`, and losing it here would strand the rows.
        for added in &diff.added {
            assert_eq!(added.session_id.as_deref(), Some(session.as_str()));
            assert_eq!(added.project_slug.as_deref(), Some("s"));
        }
    }

    #[test]
    fn editing_a_journal_shows_up_as_a_modification() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = uuid_bare(9);
        let journal = tmp
            .path()
            .join("projects")
            .join("s")
            .join(&session)
            .join("subagents")
            .join("workflows")
            .join("wf_1")
            .join("journal.jsonl");
        write_file(&journal, b"{}\n");

        // Fingerprint it as it stands, then grow it.
        let first = compute_diff(tmp.path(), &HashMap::new()).expect("first diff");
        let stored: HashMap<String, SourceFingerprint> = first
            .added
            .iter()
            .map(|a| {
                (
                    a.path.clone(),
                    SourceFingerprint {
                        path: a.path.clone(),
                        mtime_ms: a.mtime_ms,
                        size: a.size,
                        byte_position: None,
                        category: a.category.clone(),
                        project_slug: a.project_slug.clone(),
                        session_id: a.session_id.clone(),
                    },
                )
            })
            .collect();
        write_file(&journal, b"{}\n{\"more\":true}\n");

        let second = compute_diff(tmp.path(), &stored).expect("second diff");

        assert_eq!(second.modified.len(), 1, "a grown journal must be modified");
        assert!(second.added.is_empty());
    }

    #[test]
    fn todo_file_is_discovered_with_session_id() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = uuid_bare(2);
        let todo = tmp
            .path()
            .join("todos")
            .join(format!("{session}-agent-foo.json"));
        write_file(&todo, b"[]");

        let diff = compute_diff(tmp.path(), &HashMap::new()).expect("diff");

        assert_eq!(diff.added.len(), 1);
        let a = &diff.added[0];
        assert_eq!(a.category, CATEGORY_TODO);
        assert_eq!(a.session_id.as_deref(), Some(session.as_str()));
        assert!(a.project_slug.is_none(), "todos are not project-scoped");
    }

    // ─── FingerprintStore tests ────────────────────────────────────────────

    #[test]
    fn store_load_all_returns_every_row() {
        let conn = Connection::open_in_memory().expect("open mem db");
        init_source_files(&conn);
        insert_fp(
            &conn,
            "/abs/a.jsonl",
            100.5,
            10,
            Some(10),
            CATEGORY_SESSION,
            Some("slug"),
            Some("sess1"),
        );
        insert_fp(
            &conn,
            "/abs/b.jsonl",
            200.0,
            20,
            None,
            CATEGORY_SUBAGENT,
            Some("slug"),
            Some("sess1"),
        );

        let store = FingerprintStore::new(&conn, "claude-code");
        let loaded = store.load_all().expect("load_all");

        assert_eq!(loaded.len(), 2);
        let a = loaded.get("/abs/a.jsonl").expect("a present");
        assert_eq!(a.mtime_ms, 100.5);
        assert_eq!(a.size, 10);
        assert_eq!(a.byte_position, Some(10));
        assert_eq!(a.category, CATEGORY_SESSION);
        assert_eq!(a.project_slug.as_deref(), Some("slug"));
        assert_eq!(a.session_id.as_deref(), Some("sess1"));

        let b = loaded.get("/abs/b.jsonl").expect("b present");
        assert_eq!(b.byte_position, None);
        assert_eq!(b.category, CATEGORY_SUBAGENT);
    }

    // ─── End-to-end: FingerprintStore + compute_diff ───────────────────────

    #[test]
    fn store_plus_compute_diff_round_trip() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let projects = tmp.path().join("projects").join("p");
        let session_file = projects.join(uuid_jsonl(0));
        write_file(&session_file, b"data\n");

        let conn = Connection::open_in_memory().expect("open mem db");
        init_source_files(&conn);

        let (mtime, size) = stat_ms_size(&session_file);
        insert_fp(
            &conn,
            &session_file.to_string_lossy(),
            mtime,
            size,
            Some(size),
            CATEGORY_SESSION,
            Some("p"),
            Some(&uuid_bare(0)),
        );

        let store = FingerprintStore::new(&conn, "claude-code");
        let loaded = store.load_all().expect("load_all");
        let diff = compute_diff(tmp.path(), &loaded).expect("diff");

        assert!(diff.added.is_empty());
        assert!(diff.modified.is_empty());
        assert!(diff.deleted.is_empty());
        assert_eq!(diff.unchanged_count, 1);
    }
}
