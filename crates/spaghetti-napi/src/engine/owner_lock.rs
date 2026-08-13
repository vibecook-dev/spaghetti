//! Cooperative, crash-releasing database ownership lock.
//!
//! The lock itself is a SQLite `BEGIN EXCLUSIVE` transaction against a tiny
//! sidecar database. SQLite delegates locking to the host OS, so a crashed
//! process cannot leave an unrecoverable lock file behind. Human-readable
//! owner metadata is stored separately for diagnostics when a contender loses.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crossbeam_channel::{bounded, Receiver, Sender};
use rusqlite::{Connection, ErrorCode};
use serde::{Deserialize, Serialize};

use super::EngineError;

const OWNER_PROTOCOL_VERSION: u32 = 1;
static NEXT_OWNER_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OwnerMetadata {
    pub protocol_version: u32,
    pub owner_id: String,
    pub owner_label: String,
    pub process_id: u32,
    pub started_at_unix_ms: f64,
    pub database_path: String,
    pub executable: Option<String>,
    pub hostname: Option<String>,
    pub engine_version: String,
}

impl OwnerMetadata {
    pub fn new(database_path: &Path, owner_label: String) -> Self {
        let started_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let process_id = std::process::id();
        let sequence = NEXT_OWNER_ID.fetch_add(1, Ordering::Relaxed);
        let owner_id = format!("{process_id}-{}-{sequence}", started_at.as_nanos());
        let executable = std::env::current_exe()
            .ok()
            .map(|path| path.to_string_lossy().into_owned());
        let hostname = std::env::var("HOSTNAME")
            .or_else(|_| std::env::var("COMPUTERNAME"))
            .ok()
            .filter(|value| !value.is_empty());

        Self {
            protocol_version: OWNER_PROTOCOL_VERSION,
            owner_id,
            owner_label,
            process_id,
            started_at_unix_ms: started_at.as_secs_f64() * 1_000.0,
            database_path: database_path.to_string_lossy().into_owned(),
            executable,
            hostname,
            engine_version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

#[derive(Debug)]
struct LockStartupError {
    busy: bool,
    message: String,
}

/// Lifetime owner for the sidecar transaction and its metadata.
pub struct DatabaseOwnerLock {
    stop: Sender<()>,
    join: Option<JoinHandle<()>>,
    metadata_path: PathBuf,
    metadata: OwnerMetadata,
}

impl DatabaseOwnerLock {
    pub fn acquire(database_path: &Path, owner_label: String) -> Result<Self, EngineError> {
        let lock_path = sibling_path(database_path, ".owner-lock.sqlite3");
        let metadata_path = sibling_path(database_path, ".owner.json");
        let metadata = OwnerMetadata::new(database_path, owner_label);
        super::local_permissions::reject_symlink(&lock_path).map_err(|error| {
            EngineError::OwnerLock {
                lock_path: lock_path.clone(),
                detail: error.to_string(),
            }
        })?;
        super::local_permissions::reject_symlink(&metadata_path).map_err(|error| {
            EngineError::OwnerLock {
                lock_path: metadata_path.clone(),
                detail: error.to_string(),
            }
        })?;
        let (ready_tx, ready_rx) = bounded(1);
        let (stop_tx, stop_rx) = bounded(1);
        let thread_lock_path = lock_path.clone();

        let join = thread::Builder::new()
            .name("spaghetti-owner-lock".to_string())
            .spawn(move || lock_thread(thread_lock_path, ready_tx, stop_rx))
            .map_err(|error| EngineError::WorkerStart {
                worker: "owner-lock",
                detail: error.to_string(),
            })?;

        match ready_rx.recv() {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                let _ = join.join();
                let current_owner = read_metadata(&metadata_path);
                if error.busy {
                    return Err(EngineError::OwnerBusy {
                        database_path: database_path.to_path_buf(),
                        lock_path,
                        owner: current_owner.map(Box::new),
                    });
                }
                return Err(EngineError::OwnerLock {
                    lock_path,
                    detail: error.message,
                });
            }
            Err(_) => {
                let _ = join.join();
                return Err(EngineError::WorkerStart {
                    worker: "owner-lock",
                    detail: "lock worker exited before reporting readiness".to_string(),
                });
            }
        }

        if let Err(error) = write_metadata(&metadata_path, &metadata) {
            let _ = stop_tx.send(());
            let _ = join.join();
            return Err(EngineError::OwnerLock {
                lock_path,
                detail: format!("could not write owner metadata: {error}"),
            });
        }

        Ok(Self {
            stop: stop_tx,
            join: Some(join),
            metadata_path,
            metadata,
        })
    }

    pub fn metadata(&self) -> &OwnerMetadata {
        &self.metadata
    }

    pub fn release(&mut self) -> Result<(), EngineError> {
        if self.join.is_none() {
            return Ok(());
        }

        remove_metadata_if_owned(&self.metadata_path, &self.metadata.owner_id);
        let _ = self.stop.send(());
        let join = self.join.take().expect("join checked above");
        join.join().map_err(|_| EngineError::WorkerPanic {
            worker: "owner-lock",
        })
    }
}

impl Drop for DatabaseOwnerLock {
    fn drop(&mut self) {
        let _ = self.release();
    }
}

fn lock_thread(
    lock_path: PathBuf,
    ready: Sender<Result<(), LockStartupError>>,
    stop: Receiver<()>,
) {
    let connection = match Connection::open(&lock_path) {
        Ok(connection) => connection,
        Err(error) => {
            let _ = ready.send(Err(lock_error(error)));
            return;
        }
    };
    if let Err(error) = super::local_permissions::restrict_owner_file(&lock_path) {
        let _ = ready.send(Err(LockStartupError {
            busy: false,
            message: error.to_string(),
        }));
        return;
    }

    let startup = (|| -> rusqlite::Result<()> {
        connection.busy_timeout(Duration::ZERO)?;
        connection.execute_batch(
            "PRAGMA journal_mode=DELETE;
             CREATE TABLE IF NOT EXISTS owner_lock_protocol (
               version INTEGER NOT NULL
             );
             INSERT INTO owner_lock_protocol(version)
               SELECT 1 WHERE NOT EXISTS (SELECT 1 FROM owner_lock_protocol);
             BEGIN EXCLUSIVE;",
        )?;
        Ok(())
    })();

    if let Err(error) = startup {
        let _ = ready.send(Err(lock_error(error)));
        return;
    }

    if ready.send(Ok(())).is_err() {
        let _ = connection.execute_batch("ROLLBACK");
        return;
    }

    let _ = stop.recv();
    let _ = connection.execute_batch("ROLLBACK");
}

fn lock_error(error: rusqlite::Error) -> LockStartupError {
    let busy = matches!(
        &error,
        rusqlite::Error::SqliteFailure(inner, _)
            if matches!(inner.code, ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
    );
    LockStartupError {
        busy,
        message: error.to_string(),
    }
}

fn sibling_path(database_path: &Path, suffix: &str) -> PathBuf {
    let mut value: OsString = database_path.as_os_str().to_owned();
    value.push(suffix);
    PathBuf::from(value)
}

fn write_metadata(path: &Path, metadata: &OwnerMetadata) -> std::io::Result<()> {
    let payload = serde_json::to_vec_pretty(metadata).map_err(std::io::Error::other)?;
    fs::write(path, payload)?;
    super::local_permissions::restrict_owner_file(path)
}

fn read_metadata(path: &Path) -> Option<OwnerMetadata> {
    let payload = fs::read(path).ok()?;
    serde_json::from_slice(&payload).ok()
}

fn remove_metadata_if_owned(path: &Path, owner_id: &str) {
    if read_metadata(path)
        .as_ref()
        .map(|owner| owner.owner_id.as_str())
        == Some(owner_id)
    {
        let _ = fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn a_second_owner_gets_structured_metadata_and_release_is_recoverable() {
        let dir = tempdir().unwrap();
        let database = dir.path().join("engine.db");
        let mut first = DatabaseOwnerLock::acquire(&database, "first".to_string()).unwrap();

        let error = match DatabaseOwnerLock::acquire(&database, "second".to_string()) {
            Ok(_) => panic!("second owner unexpectedly acquired the database"),
            Err(error) => error,
        };
        match error {
            EngineError::OwnerBusy { owner, .. } => {
                let owner = owner.expect("owner metadata");
                assert_eq!(owner.owner_label, "first");
                assert_eq!(owner.process_id, std::process::id());
            }
            other => panic!("expected OwnerBusy, got {other:?}"),
        }

        first.release().unwrap();
        assert!(!sibling_path(&database, ".owner.json").exists());

        let mut second = DatabaseOwnerLock::acquire(&database, "second".to_string()).unwrap();
        second.release().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn owner_files_are_restricted_to_the_current_user() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("permissions.db");
        let mut owner = DatabaseOwnerLock::acquire(&database, "permissions".to_string()).unwrap();
        let lock = sibling_path(&database, ".owner-lock.sqlite3");
        let metadata = sibling_path(&database, ".owner.json");
        assert_eq!(
            fs::metadata(lock).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(metadata).unwrap().permissions().mode() & 0o777,
            0o600
        );
        owner.release().unwrap();
    }
}
