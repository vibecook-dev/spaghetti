use std::collections::{BTreeMap, BTreeSet};
use std::panic::RefUnwindSafe;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::config::DbConfig;
use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use rusqlite::types::ValueRef;
use rusqlite::{Connection, DatabaseName, ErrorCode, OpenFlags, TransactionBehavior};

use super::file::confined_file_stamp;
use super::model::{io_error, CursorReader};
use super::{FileIdentity, RecordOrigin, Revision, SourceCursor, SourceDriverError, SourceRecord};

const CHECKPOINT_MAGIC: &[u8] = b"SPSQ";
const CHECKPOINT_VERSION: u8 = 1;
const ROW_MAGIC: &[u8] = b"SPSR";
const ROW_VERSION: u8 = 1;
const SNAPSHOT_DOMAIN: &[u8] = b"spaghetti:sqlite-snapshot:v1\0";

/// One adapter-declared, parameter-free query against an agent-owned source
/// database. `key_columns` identify a row inside this named result set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqliteQuerySpec {
    pub name: String,
    pub sql: String,
    pub key_columns: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqliteSnapshotConfig {
    pub queries: Vec<SqliteQuerySpec>,
    pub max_database_bytes: usize,
    pub max_sidecar_bytes: usize,
    pub max_rows: usize,
    pub max_value_bytes: usize,
    pub max_snapshot_bytes: usize,
    pub busy_timeout_ms: u64,
}

impl SqliteSnapshotConfig {
    pub fn bounded(queries: Vec<SqliteQuerySpec>) -> Self {
        Self {
            queries,
            max_database_bytes: 512 * 1024 * 1024,
            max_sidecar_bytes: 256 * 1024 * 1024,
            max_rows: 4_096,
            max_value_bytes: 4 * 1024 * 1024,
            max_snapshot_bytes: 64 * 1024 * 1024,
            busy_timeout_ms: 250,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SqliteValue {
    Null,
    Integer(i64),
    RealBits(u64),
    Text(Vec<u8>),
    Blob(Vec<u8>),
}

impl SqliteValue {
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Text(value) | Self::Blob(value) => Some(value),
            Self::Null | Self::Integer(_) | Self::RealBits(_) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqliteColumn {
    pub name: String,
    pub value: SqliteValue,
}

/// Stable binary record passed to the adapter. The source layer deliberately
/// does not parse JSON or vendor schemas.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqliteRowRecord {
    pub query_name: String,
    pub row_key: Vec<u8>,
    pub columns: Vec<SqliteColumn>,
}

impl SqliteRowRecord {
    pub fn column(&self, name: &str) -> Option<&SqliteValue> {
        self.columns
            .iter()
            .find(|column| column.name == name)
            .map(|column| &column.value)
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(ROW_MAGIC);
        bytes.push(ROW_VERSION);
        put_bytes(&mut bytes, self.query_name.as_bytes());
        put_bytes(&mut bytes, &self.row_key);
        put_u64(&mut bytes, self.columns.len() as u64);
        for column in &self.columns {
            put_bytes(&mut bytes, column.name.as_bytes());
            encode_value(&mut bytes, &column.value);
        }
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, SourceDriverError> {
        let Some(bytes) = bytes.strip_prefix(ROW_MAGIC) else {
            return Err(SourceDriverError::InvalidCursor(
                "SQLite row record magic does not match".to_string(),
            ));
        };
        let mut reader = CursorReader::from_payload(bytes);
        if reader.byte()? != ROW_VERSION {
            return Err(SourceDriverError::InvalidCursor(
                "unsupported SQLite row record version".to_string(),
            ));
        }
        let query_name = decode_utf8(take_bytes(&mut reader)?, "SQLite query name")?;
        let row_key = take_bytes(&mut reader)?.to_vec();
        let column_count = reader.usize()?;
        let mut columns = Vec::with_capacity(column_count);
        let mut names = BTreeSet::new();
        for _ in 0..column_count {
            let name = decode_utf8(take_bytes(&mut reader)?, "SQLite column name")?;
            if !names.insert(name.clone()) {
                return Err(SourceDriverError::InvalidCursor(format!(
                    "SQLite row record repeats column {name}"
                )));
            }
            columns.push(SqliteColumn {
                name,
                value: decode_value(&mut reader)?,
            });
        }
        reader.finish()?;
        if query_name.is_empty() || row_key.is_empty() || columns.is_empty() {
            return Err(SourceDriverError::InvalidCursor(
                "SQLite row record is incomplete".to_string(),
            ));
        }
        Ok(Self {
            query_name,
            row_key,
            columns,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqliteCheckpoint {
    pub generation: u64,
    pub identity: FileIdentity,
    pub schema_version: u64,
    pub revision: Revision,
    pub row_count: u64,
}

impl SqliteCheckpoint {
    pub fn cursor(&self) -> SourceCursor {
        SourceCursor::sqlite_snapshot(self.revision)
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(128);
        bytes.extend_from_slice(CHECKPOINT_MAGIC);
        bytes.push(CHECKPOINT_VERSION);
        bytes.extend_from_slice(&self.generation.to_be_bytes());
        self.identity.encode_into(&mut bytes);
        bytes.extend_from_slice(&self.schema_version.to_be_bytes());
        bytes.extend_from_slice(self.revision.as_bytes());
        bytes.extend_from_slice(&self.row_count.to_be_bytes());
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, SourceDriverError> {
        let Some(bytes) = bytes.strip_prefix(CHECKPOINT_MAGIC) else {
            return Err(SourceDriverError::InvalidCursor(
                "checkpoint magic does not match SQLite snapshot driver".to_string(),
            ));
        };
        let mut reader = CursorReader::from_payload(bytes);
        if reader.byte()? != CHECKPOINT_VERSION {
            return Err(SourceDriverError::InvalidCursor(
                "unsupported SQLite snapshot checkpoint version".to_string(),
            ));
        }
        let checkpoint = Self {
            generation: reader.u64()?,
            identity: FileIdentity::decode_from(&mut reader)?,
            schema_version: reader.u64()?,
            revision: reader.revision()?,
            row_count: reader.u64()?,
        };
        reader.finish()?;
        if checkpoint.generation == 0 {
            return Err(SourceDriverError::InvalidCursor(
                "SQLite snapshot generation must be greater than zero".to_string(),
            ));
        }
        Ok(checkpoint)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SqliteRead {
    Missing,
    RetryTransient,
    Unchanged {
        checkpoint: SqliteCheckpoint,
    },
    Snapshot {
        records: Vec<SourceRecord>,
        checkpoint: SqliteCheckpoint,
        generation_changed: bool,
    },
}

pub struct SqliteSnapshot {
    config: SqliteSnapshotConfig,
}

impl SqliteSnapshot {
    pub fn new(config: SqliteSnapshotConfig) -> Result<Self, SourceDriverError> {
        validate_config(&config)?;
        Ok(Self { config })
    }

    pub fn read(
        &self,
        path: &Path,
        previous: Option<&SqliteCheckpoint>,
        origin: &RecordOrigin,
        force_replay: bool,
    ) -> Result<SqliteRead, SourceDriverError> {
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let file_name = path
            .file_name()
            .map(Path::new)
            .ok_or_else(|| SourceDriverError::PathEscape(path.to_string_lossy().into_owned()))?;
        self.read_confined(parent, file_name, previous, origin, force_replay, || false)
    }

    pub fn read_confined<F>(
        &self,
        root: &Path,
        relative_path: &Path,
        previous: Option<&SqliteCheckpoint>,
        origin: &RecordOrigin,
        force_replay: bool,
        cancelled: F,
    ) -> Result<SqliteRead, SourceDriverError>
    where
        F: Fn() -> bool + Clone + Send + RefUnwindSafe + 'static,
    {
        if cancelled() {
            return Ok(SqliteRead::RetryTransient);
        }
        let before = match confined_file_stamp(root, relative_path, self.config.max_database_bytes)
        {
            Ok(Some(stamp)) => stamp,
            Ok(None) => return Ok(SqliteRead::Missing),
            Err(SourceDriverError::Unstable(_)) => return Ok(SqliteRead::RetryTransient),
            Err(error) => return Err(error),
        };
        self.check_sidecar_bound(root, relative_path, "-wal")?;
        self.check_sidecar_bound(root, relative_path, "-journal")?;

        // SQLite's NOFOLLOW flag rejects a symlink in any absolute path
        // component on supported Unix VFSes. Resolve only the engine-approved
        // root itself (for example macOS `/var` -> `/private/var`); the
        // relative source path was already opened component-by-component with
        // no-follow semantics by `confined_file_stamp` above.
        let open_root = root
            .canonicalize()
            .map_err(|error| io_error("resolving confined SQLite root", root, error))?;
        let path = open_root.join(relative_path);
        let material = match read_material(&path, &self.config, cancelled.clone()) {
            Ok(material) => material,
            Err(ReadFailure::Cancelled) => return Ok(SqliteRead::RetryTransient),
            Err(ReadFailure::Sqlite(error)) if retryable_sqlite_error(&error) => {
                return Ok(SqliteRead::RetryTransient)
            }
            Err(ReadFailure::Sqlite(error)) => {
                return Err(SourceDriverError::Database(error.to_string()))
            }
            Err(ReadFailure::Driver(error)) => return Err(error),
        };
        if cancelled() || material.data_version_before != material.data_version_after {
            return Ok(SqliteRead::RetryTransient);
        }
        let after = match confined_file_stamp(root, relative_path, self.config.max_database_bytes) {
            Ok(Some(stamp)) => stamp,
            Ok(None) | Err(SourceDriverError::Unstable(_)) => {
                return Ok(SqliteRead::RetryTransient)
            }
            Err(error) => return Err(error),
        };
        if before.identity != after.identity {
            return Ok(SqliteRead::RetryTransient);
        }

        let revision = snapshot_revision(&material.rows);
        let row_count = material.rows.len() as u64;
        let unchanged = !force_replay
            && previous.is_some_and(|old| {
                old.identity == after.identity
                    && old.schema_version == material.schema_version
                    && old.revision == revision
                    && old.row_count == row_count
            });
        if unchanged {
            return Ok(SqliteRead::Unchanged {
                checkpoint: previous.expect("checked above").clone(),
            });
        }

        // Each complete database result set is an atomic replacement epoch.
        // Advancing the generation lets the common projector retract rows
        // removed from the source without asking adapters to manufacture
        // vendor-specific tombstones.
        let generation = match previous {
            Some(old) => old.generation.checked_add(1).ok_or_else(|| {
                SourceDriverError::InvalidCursor(
                    "SQLite snapshot generation overflowed".to_string(),
                )
            })?,
            None => 1,
        };
        let checkpoint = SqliteCheckpoint {
            generation,
            identity: after.identity,
            schema_version: material.schema_version,
            revision,
            row_count,
        };
        let cursor_start = previous.map_or(Revision::ZERO, |old| old.revision);
        let records = material
            .rows
            .into_iter()
            .enumerate()
            .map(|(ordinal, row)| {
                Ok(SourceRecord::new(
                    origin,
                    generation,
                    SourceCursor::sqlite_snapshot(cursor_start),
                    checkpoint.cursor(),
                    u32::try_from(ordinal).map_err(|_| {
                        SourceDriverError::LimitExceeded(
                            "SQLite row ordinal exceeds u32".to_string(),
                        )
                    })?,
                    row.payload,
                ))
            })
            .collect::<Result<Vec<_>, SourceDriverError>>()?;
        Ok(SqliteRead::Snapshot {
            records,
            checkpoint,
            generation_changed: previous.is_some(),
        })
    }

    fn check_sidecar_bound(
        &self,
        root: &Path,
        relative_path: &Path,
        suffix: &str,
    ) -> Result<(), SourceDriverError> {
        let sidecar = suffixed_path(relative_path, suffix)?;
        match confined_file_stamp(root, &sidecar, self.config.max_sidecar_bytes) {
            Ok(_) => Ok(()),
            Err(SourceDriverError::Unstable(_)) => Ok(()),
            Err(error) => Err(error),
        }
    }
}

#[derive(Debug)]
struct EncodedRow {
    query_name: String,
    row_key: Vec<u8>,
    payload: Vec<u8>,
}

struct ReadMaterial {
    schema_version: u64,
    data_version_before: u64,
    data_version_after: u64,
    rows: Vec<EncodedRow>,
}

enum ReadFailure {
    Sqlite(rusqlite::Error),
    Driver(SourceDriverError),
    Cancelled,
}

impl From<rusqlite::Error> for ReadFailure {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

impl From<SourceDriverError> for ReadFailure {
    fn from(error: SourceDriverError) -> Self {
        Self::Driver(error)
    }
}

fn read_material<F>(
    path: &Path,
    config: &SqliteSnapshotConfig,
    cancelled: F,
) -> Result<ReadMaterial, ReadFailure>
where
    F: Fn() -> bool + Clone + Send + RefUnwindSafe + 'static,
{
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_PRIVATE_CACHE
        | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    let mut connection = Connection::open_with_flags(path, flags)?;
    if !connection.is_readonly(DatabaseName::Main)? {
        return Err(SourceDriverError::InvalidConfig(
            "source database did not open read-only".to_string(),
        )
        .into());
    }
    connection.busy_timeout(Duration::from_millis(config.busy_timeout_ms))?;
    connection.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)?;
    connection.pragma_update(None, "query_only", true)?;
    let progress_cancel = cancelled.clone();
    connection.progress_handler(1_000, Some(progress_cancel));
    connection.authorizer(Some(source_query_authorizer));

    let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
    let schema_version = pragma_u64(&transaction, "schema_version")?;
    let data_version_before = pragma_u64(&transaction, "data_version")?;
    let mut rows = Vec::new();
    let mut snapshot_bytes = 0_usize;
    for query in &config.queries {
        if cancelled() {
            return Err(ReadFailure::Cancelled);
        }
        if has_multiple_statements(&query.sql) {
            return Err(SourceDriverError::InvalidConfig(format!(
                "source query {} contains multiple statements",
                query.name
            ))
            .into());
        }
        let mut statement = transaction.prepare(&query.sql)?;
        if !statement.readonly() {
            return Err(SourceDriverError::InvalidConfig(format!(
                "source query {} is not read-only",
                query.name
            ))
            .into());
        }
        let column_names = statement
            .column_names()
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        if column_names.is_empty() {
            return Err(SourceDriverError::InvalidConfig(format!(
                "source query {} returns no columns",
                query.name
            ))
            .into());
        }
        let mut column_indexes = BTreeMap::new();
        for (index, name) in column_names.iter().enumerate() {
            if column_indexes.insert(name.clone(), index).is_some() {
                return Err(SourceDriverError::InvalidConfig(format!(
                    "source query {} repeats column {name}",
                    query.name
                ))
                .into());
            }
        }
        let key_indexes = query
            .key_columns
            .iter()
            .map(|name| {
                column_indexes.get(name).copied().ok_or_else(|| {
                    SourceDriverError::InvalidConfig(format!(
                        "source query {} does not return key column {name}",
                        query.name
                    ))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut query_rows = statement.query([])?;
        let mut query_keys = BTreeSet::new();
        while let Some(row) = query_rows.next()? {
            if cancelled() {
                return Err(ReadFailure::Cancelled);
            }
            if rows.len() == config.max_rows {
                return Err(SourceDriverError::LimitExceeded(format!(
                    "SQLite snapshot exceeds {} rows",
                    config.max_rows
                ))
                .into());
            }
            let mut columns = Vec::with_capacity(column_names.len());
            for (index, name) in column_names.iter().enumerate() {
                columns.push(SqliteColumn {
                    name: name.clone(),
                    value: value_from_ref(row.get_ref(index)?, config.max_value_bytes)?,
                });
            }
            let mut row_key = Vec::new();
            for index in &key_indexes {
                encode_value(&mut row_key, &columns[*index].value);
            }
            if row_key.is_empty() || !query_keys.insert(row_key.clone()) {
                return Err(SourceDriverError::InvalidConfig(format!(
                    "source query {} returned an empty or duplicate row key",
                    query.name
                ))
                .into());
            }
            let record = SqliteRowRecord {
                query_name: query.name.clone(),
                row_key: row_key.clone(),
                columns,
            };
            let payload = record.encode();
            snapshot_bytes = snapshot_bytes.checked_add(payload.len()).ok_or_else(|| {
                SourceDriverError::LimitExceeded("SQLite snapshot size overflowed".to_string())
            })?;
            if snapshot_bytes > config.max_snapshot_bytes {
                return Err(SourceDriverError::LimitExceeded(format!(
                    "SQLite snapshot exceeds {} bytes",
                    config.max_snapshot_bytes
                ))
                .into());
            }
            rows.push(EncodedRow {
                query_name: query.name.clone(),
                row_key,
                payload,
            });
        }
    }
    rows.sort_by(|left, right| {
        (&left.query_name, &left.row_key).cmp(&(&right.query_name, &right.row_key))
    });
    transaction.commit()?;
    let data_version_after = pragma_u64(&connection, "data_version")?;
    Ok(ReadMaterial {
        schema_version,
        data_version_before,
        data_version_after,
        rows,
    })
}

fn source_query_authorizer(context: AuthContext<'_>) -> Authorization {
    match context.action {
        AuthAction::Select
        | AuthAction::Function { .. }
        | AuthAction::Recursive
        | AuthAction::Transaction { .. } => Authorization::Allow,
        AuthAction::Read { .. } if context.database_name == Some("main") => Authorization::Allow,
        AuthAction::Pragma {
            pragma_name: "schema_version" | "data_version",
            pragma_value: None,
        } => Authorization::Allow,
        _ => Authorization::Deny,
    }
}

fn pragma_u64(connection: &Connection, pragma: &str) -> Result<u64, ReadFailure> {
    let value =
        connection.query_row(&format!("PRAGMA {pragma}"), [], |row| row.get::<_, i64>(0))?;
    u64::try_from(value).map_err(|_| {
        SourceDriverError::Database(format!("PRAGMA {pragma} returned a negative value")).into()
    })
}

fn value_from_ref(value: ValueRef<'_>, max_bytes: usize) -> Result<SqliteValue, ReadFailure> {
    let value = match value {
        ValueRef::Null => SqliteValue::Null,
        ValueRef::Integer(value) => SqliteValue::Integer(value),
        ValueRef::Real(value) => SqliteValue::RealBits(value.to_bits()),
        ValueRef::Text(value) => {
            if value.len() > max_bytes {
                return Err(SourceDriverError::LimitExceeded(format!(
                    "SQLite text value exceeds {max_bytes} bytes"
                ))
                .into());
            }
            SqliteValue::Text(value.to_vec())
        }
        ValueRef::Blob(value) => {
            if value.len() > max_bytes {
                return Err(SourceDriverError::LimitExceeded(format!(
                    "SQLite blob value exceeds {max_bytes} bytes"
                ))
                .into());
            }
            SqliteValue::Blob(value.to_vec())
        }
    };
    Ok(value)
}

fn snapshot_revision(rows: &[EncodedRow]) -> Revision {
    let mut hasher = blake3::Hasher::new();
    hasher.update(SNAPSHOT_DOMAIN);
    for row in rows {
        hasher.update(&(row.payload.len() as u64).to_be_bytes());
        hasher.update(&row.payload);
    }
    Revision::from_bytes(*hasher.finalize().as_bytes())
}

fn retryable_sqlite_error(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(detail, _)
            if matches!(
                detail.code,
                ErrorCode::DatabaseBusy
                    | ErrorCode::DatabaseLocked
                    | ErrorCode::OperationInterrupted
            )
    )
}

fn validate_config(config: &SqliteSnapshotConfig) -> Result<(), SourceDriverError> {
    if config.queries.is_empty() {
        return Err(SourceDriverError::InvalidConfig(
            "SQLite snapshot requires at least one named query".to_string(),
        ));
    }
    if config.max_database_bytes == 0
        || config.max_sidecar_bytes == 0
        || config.max_rows == 0
        || config.max_rows > u32::MAX as usize
        || config.max_value_bytes == 0
        || config.max_snapshot_bytes == 0
        || config.busy_timeout_ms > 30_000
    {
        return Err(SourceDriverError::InvalidConfig(
            "SQLite snapshot bounds are zero or outside supported limits".to_string(),
        ));
    }
    let mut names = BTreeSet::new();
    for query in &config.queries {
        if query.name.trim().is_empty()
            || query.name.len() > 128
            || !names.insert(query.name.clone())
            || query.sql.trim().is_empty()
            || query.key_columns.is_empty()
        {
            return Err(SourceDriverError::InvalidConfig(
                "SQLite query name, SQL, and unique row keys are required".to_string(),
            ));
        }
        let mut keys = BTreeSet::new();
        if query
            .key_columns
            .iter()
            .any(|key| key.trim().is_empty() || !keys.insert(key))
        {
            return Err(SourceDriverError::InvalidConfig(format!(
                "SQLite query {} has invalid or duplicate key columns",
                query.name
            )));
        }
    }
    Ok(())
}

fn has_multiple_statements(sql: &str) -> bool {
    let trimmed = sql.trim();
    let without_final = trimmed.strip_suffix(';').unwrap_or(trimmed);
    without_final.contains(';')
}

fn suffixed_path(path: &Path, suffix: &str) -> Result<PathBuf, SourceDriverError> {
    let file_name = path
        .file_name()
        .ok_or_else(|| SourceDriverError::PathEscape(path.to_string_lossy().into_owned()))?;
    let mut suffixed = file_name.to_os_string();
    suffixed.push(suffix);
    Ok(path.with_file_name(suffixed))
}

fn encode_value(output: &mut Vec<u8>, value: &SqliteValue) {
    match value {
        SqliteValue::Null => output.push(0),
        SqliteValue::Integer(value) => {
            output.push(1);
            output.extend_from_slice(&value.to_be_bytes());
        }
        SqliteValue::RealBits(value) => {
            output.push(2);
            output.extend_from_slice(&value.to_be_bytes());
        }
        SqliteValue::Text(value) => {
            output.push(3);
            put_bytes(output, value);
        }
        SqliteValue::Blob(value) => {
            output.push(4);
            put_bytes(output, value);
        }
    }
}

fn decode_value(reader: &mut CursorReader<'_>) -> Result<SqliteValue, SourceDriverError> {
    match reader.byte()? {
        0 => Ok(SqliteValue::Null),
        1 => Ok(SqliteValue::Integer(i64::from_be_bytes(
            reader.take(8)?.try_into().map_err(|_| {
                SourceDriverError::InvalidCursor("truncated SQLite integer".to_string())
            })?,
        ))),
        2 => Ok(SqliteValue::RealBits(u64::from_be_bytes(
            reader.take(8)?.try_into().map_err(|_| {
                SourceDriverError::InvalidCursor("truncated SQLite real".to_string())
            })?,
        ))),
        3 => Ok(SqliteValue::Text(take_bytes(reader)?.to_vec())),
        4 => Ok(SqliteValue::Blob(take_bytes(reader)?.to_vec())),
        tag => Err(SourceDriverError::InvalidCursor(format!(
            "unknown SQLite value tag {tag}"
        ))),
    }
}

fn put_u64(output: &mut Vec<u8>, value: u64) {
    output.extend_from_slice(&value.to_be_bytes());
}

fn put_bytes(output: &mut Vec<u8>, value: &[u8]) {
    put_u64(output, value.len() as u64);
    output.extend_from_slice(value);
}

fn take_bytes<'a>(reader: &mut CursorReader<'a>) -> Result<&'a [u8], SourceDriverError> {
    let length = reader.usize()?;
    reader.take(length)
}

fn decode_utf8(bytes: &[u8], label: &str) -> Result<String, SourceDriverError> {
    String::from_utf8(bytes.to_vec())
        .map_err(|_| SourceDriverError::InvalidCursor(format!("{label} is not UTF-8")))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    use rusqlite::params;
    use tempfile::TempDir;

    use super::*;
    use crate::source::SourceMediaType;

    fn origin() -> RecordOrigin {
        RecordOrigin {
            source_instance_id: 1,
            stream_id: 2,
            object_id: 3,
            observed_at: 4,
            source_timestamp_hint: None,
            media_type: SourceMediaType::new("application/vnd.sqlite3").unwrap(),
        }
    }

    fn config(sql: &str) -> SqliteSnapshotConfig {
        SqliteSnapshotConfig::bounded(vec![SqliteQuerySpec {
            name: "items".to_string(),
            sql: sql.to_string(),
            key_columns: vec!["id".to_string()],
        }])
    }

    fn fixture() -> (TempDir, PathBuf) {
        let root = TempDir::new().unwrap();
        let path = root.path().join("source.db");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE items(id INTEGER PRIMARY KEY, value BLOB);\n\
                 INSERT INTO items VALUES (2, x'62'), (1, x'61');",
            )
            .unwrap();
        drop(connection);
        (root, path)
    }

    #[test]
    fn rows_are_stable_sorted_bounded_and_checkpointed() {
        let (root, path) = fixture();
        let driver = SqliteSnapshot::new(config("SELECT id, value FROM items")).unwrap();
        let SqliteRead::Snapshot {
            records,
            checkpoint,
            ..
        } = driver.read(&path, None, &origin(), false).unwrap()
        else {
            panic!("first read should produce a snapshot")
        };
        assert_eq!(records.len(), 2);
        let first = SqliteRowRecord::decode(&records[0].payload).unwrap();
        assert_eq!(first.column("id"), Some(&SqliteValue::Integer(1)));
        assert_eq!(
            SqliteCheckpoint::decode(&checkpoint.encode()).unwrap(),
            checkpoint
        );
        assert!(matches!(
            driver
                .read(&path, Some(&checkpoint), &origin(), false)
                .unwrap(),
            SqliteRead::Unchanged { .. }
        ));

        Connection::open(&path)
            .unwrap()
            .execute("UPDATE items SET value = ?1 WHERE id = 1", params![b"new"])
            .unwrap();
        let SqliteRead::Snapshot {
            checkpoint: next,
            generation_changed,
            ..
        } = driver
            .read(&path, Some(&checkpoint), &origin(), false)
            .unwrap()
        else {
            panic!("modified rows should replace the snapshot")
        };
        assert!(generation_changed);
        assert_eq!(next.generation, checkpoint.generation + 1);
        assert_ne!(next.revision, checkpoint.revision);
        drop(root);
    }

    #[test]
    fn write_attach_and_multiple_statement_queries_are_rejected() {
        let (_root, path) = fixture();
        for sql in [
            "DELETE FROM items RETURNING id, value",
            "ATTACH DATABASE '/tmp/outside.db' AS outside",
            "SELECT id, value FROM items; SELECT id, value FROM items",
        ] {
            let driver = SqliteSnapshot::new(config(sql)).unwrap();
            let error = driver.read(&path, None, &origin(), false).unwrap_err();
            assert!(matches!(
                error,
                SourceDriverError::InvalidConfig(_) | SourceDriverError::Database(_)
            ));
        }
        assert_eq!(
            Connection::open(&path)
                .unwrap()
                .query_row("SELECT COUNT(*) FROM items", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            2
        );
    }

    #[test]
    fn cancellation_and_busy_lock_retry_without_mutating_source() {
        let (_root, path) = fixture();
        let cancelled = Arc::new(AtomicBool::new(true));
        let signal = Arc::clone(&cancelled);
        let driver = SqliteSnapshot::new(config("SELECT id, value FROM items")).unwrap();
        assert!(matches!(
            driver
                .read_confined(
                    path.parent().unwrap(),
                    Path::new("source.db"),
                    None,
                    &origin(),
                    false,
                    move || signal.load(Ordering::Acquire),
                )
                .unwrap(),
            SqliteRead::RetryTransient
        ));

        let locking = Connection::open(&path).unwrap();
        locking
            .execute_batch("BEGIN EXCLUSIVE; UPDATE items SET value = value;")
            .unwrap();
        let mut bounded = config("SELECT id, value FROM items");
        bounded.busy_timeout_ms = 0;
        let driver = SqliteSnapshot::new(bounded).unwrap();
        assert!(matches!(
            driver.read(&path, None, &origin(), false).unwrap(),
            SqliteRead::RetryTransient
        ));
        locking.execute_batch("ROLLBACK").unwrap();
    }

    #[test]
    fn row_and_value_limits_fail_closed() {
        let (_root, path) = fixture();
        let mut bounded = config("SELECT id, value FROM items");
        bounded.max_rows = 1;
        let error = SqliteSnapshot::new(bounded)
            .unwrap()
            .read(&path, None, &origin(), false)
            .unwrap_err();
        assert!(matches!(error, SourceDriverError::LimitExceeded(_)));

        let mut bounded = config("SELECT id, value FROM items");
        bounded.max_value_bytes = 0;
        assert!(SqliteSnapshot::new(bounded).is_err());
    }
}
