use std::panic::RefUnwindSafe;
use std::path::Path;

use super::model::CursorReader;
use super::{
    FileIdentity, RecordOrigin, Revision, SourceCursor, SourceDriverError, SourceRecord,
    SqliteQuerySpec, SqliteRead, SqliteRowRecord, SqliteSnapshot, SqliteSnapshotConfig,
    SqliteValue,
};

const CHECKPOINT_MAGIC: &[u8] = b"SPKQ";
const CHECKPOINT_VERSION: u8 = 1;
const RECORD_MAGIC: &[u8] = b"SPKR";
const RECORD_VERSION: u8 = 1;
const SNAPSHOT_DOMAIN: &[u8] = b"spaghetti:key-value-snapshot:v1\0";

/// A bounded key/value view over an agent-owned SQLite state database. The
/// adapter declares the source query and exact/prefix selection; values remain
/// opaque until its decoder receives each stable record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyValueSnapshotConfig {
    pub query_name: String,
    pub sql: String,
    pub key_column: String,
    pub value_column: String,
    pub exact_keys: Vec<Vec<u8>>,
    pub key_prefixes: Vec<Vec<u8>>,
    pub max_database_bytes: usize,
    pub max_sidecar_bytes: usize,
    pub max_scan_rows: usize,
    pub max_entries: usize,
    pub max_value_bytes: usize,
    pub max_snapshot_bytes: usize,
    pub busy_timeout_ms: u64,
}

impl KeyValueSnapshotConfig {
    pub fn bounded(
        query_name: impl Into<String>,
        sql: impl Into<String>,
        key_column: impl Into<String>,
        value_column: impl Into<String>,
    ) -> Self {
        Self {
            query_name: query_name.into(),
            sql: sql.into(),
            key_column: key_column.into(),
            value_column: value_column.into(),
            exact_keys: Vec::new(),
            key_prefixes: Vec::new(),
            max_database_bytes: 512 * 1024 * 1024,
            max_sidecar_bytes: 256 * 1024 * 1024,
            max_scan_rows: 16_384,
            max_entries: 4_096,
            max_value_bytes: 4 * 1024 * 1024,
            max_snapshot_bytes: 64 * 1024 * 1024,
            busy_timeout_ms: 250,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyValueRecord {
    pub key: Vec<u8>,
    pub value: SqliteValue,
}

impl KeyValueRecord {
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(RECORD_MAGIC);
        bytes.push(RECORD_VERSION);
        put_bytes(&mut bytes, &self.key);
        encode_value(&mut bytes, &self.value);
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, SourceDriverError> {
        let Some(bytes) = bytes.strip_prefix(RECORD_MAGIC) else {
            return Err(SourceDriverError::InvalidCursor(
                "key/value record magic does not match".to_string(),
            ));
        };
        let mut reader = CursorReader::from_payload(bytes);
        if reader.byte()? != RECORD_VERSION {
            return Err(SourceDriverError::InvalidCursor(
                "unsupported key/value record version".to_string(),
            ));
        }
        let key = take_bytes(&mut reader)?.to_vec();
        let value = decode_value(&mut reader)?;
        reader.finish()?;
        if key.is_empty() {
            return Err(SourceDriverError::InvalidCursor(
                "key/value record has an empty key".to_string(),
            ));
        }
        Ok(Self { key, value })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyValueCheckpoint {
    pub generation: u64,
    pub identity: FileIdentity,
    pub schema_version: u64,
    pub revision: Revision,
    pub entry_count: u64,
}

impl KeyValueCheckpoint {
    pub fn cursor(&self) -> SourceCursor {
        SourceCursor::key_value_snapshot(self.revision)
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(128);
        bytes.extend_from_slice(CHECKPOINT_MAGIC);
        bytes.push(CHECKPOINT_VERSION);
        bytes.extend_from_slice(&self.generation.to_be_bytes());
        self.identity.encode_into(&mut bytes);
        bytes.extend_from_slice(&self.schema_version.to_be_bytes());
        bytes.extend_from_slice(self.revision.as_bytes());
        bytes.extend_from_slice(&self.entry_count.to_be_bytes());
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, SourceDriverError> {
        let Some(bytes) = bytes.strip_prefix(CHECKPOINT_MAGIC) else {
            return Err(SourceDriverError::InvalidCursor(
                "checkpoint magic does not match key/value snapshot driver".to_string(),
            ));
        };
        let mut reader = CursorReader::from_payload(bytes);
        if reader.byte()? != CHECKPOINT_VERSION {
            return Err(SourceDriverError::InvalidCursor(
                "unsupported key/value snapshot checkpoint version".to_string(),
            ));
        }
        let checkpoint = Self {
            generation: reader.u64()?,
            identity: FileIdentity::decode_from(&mut reader)?,
            schema_version: reader.u64()?,
            revision: reader.revision()?,
            entry_count: reader.u64()?,
        };
        reader.finish()?;
        if checkpoint.generation == 0 {
            return Err(SourceDriverError::InvalidCursor(
                "key/value snapshot generation must be greater than zero".to_string(),
            ));
        }
        Ok(checkpoint)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyValueRead {
    Missing,
    RetryTransient,
    Unchanged {
        checkpoint: KeyValueCheckpoint,
    },
    Snapshot {
        records: Vec<SourceRecord>,
        checkpoint: KeyValueCheckpoint,
        generation_changed: bool,
    },
}

pub struct KeyValueSnapshot {
    config: KeyValueSnapshotConfig,
}

impl KeyValueSnapshot {
    pub fn new(mut config: KeyValueSnapshotConfig) -> Result<Self, SourceDriverError> {
        config.exact_keys.sort();
        config.exact_keys.dedup();
        config.key_prefixes.sort();
        config.key_prefixes.dedup();
        validate_config(&config)?;
        Ok(Self { config })
    }

    pub fn read(
        &self,
        path: &Path,
        previous: Option<&KeyValueCheckpoint>,
        origin: &RecordOrigin,
        force_replay: bool,
    ) -> Result<KeyValueRead, SourceDriverError> {
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
        previous: Option<&KeyValueCheckpoint>,
        origin: &RecordOrigin,
        force_replay: bool,
        cancelled: F,
    ) -> Result<KeyValueRead, SourceDriverError>
    where
        F: Fn() -> bool + Clone + Send + RefUnwindSafe + 'static,
    {
        let sqlite = SqliteSnapshot::new(SqliteSnapshotConfig {
            queries: vec![SqliteQuerySpec {
                name: self.config.query_name.clone(),
                sql: self.config.sql.clone(),
                key_columns: vec![self.config.key_column.clone()],
            }],
            max_database_bytes: self.config.max_database_bytes,
            max_sidecar_bytes: self.config.max_sidecar_bytes,
            max_rows: self.config.max_scan_rows,
            max_value_bytes: self.config.max_value_bytes,
            max_snapshot_bytes: self.config.max_snapshot_bytes,
            busy_timeout_ms: self.config.busy_timeout_ms,
        })?;
        let (records, source_checkpoint) =
            match sqlite.read_confined(root, relative_path, None, origin, false, cancelled)? {
                SqliteRead::Missing => return Ok(KeyValueRead::Missing),
                SqliteRead::RetryTransient => return Ok(KeyValueRead::RetryTransient),
                SqliteRead::Unchanged { .. } => unreachable!("checkpointless SQLite read"),
                SqliteRead::Snapshot {
                    records,
                    checkpoint,
                    ..
                } => (records, checkpoint),
            };
        self.interpret_rows(records, source_checkpoint, previous, origin, force_replay)
    }

    fn interpret_rows(
        &self,
        records: Vec<SourceRecord>,
        source_checkpoint: super::SqliteCheckpoint,
        previous: Option<&KeyValueCheckpoint>,
        origin: &RecordOrigin,
        force_replay: bool,
    ) -> Result<KeyValueRead, SourceDriverError> {
        let mut selected = Vec::new();
        let mut selected_bytes = 0_usize;
        for record in records {
            let row = SqliteRowRecord::decode(&record.payload)?;
            let key = value_key(row.column(&self.config.key_column).ok_or_else(|| {
                SourceDriverError::InvalidConfig(format!(
                    "key/value query does not return key column {}",
                    self.config.key_column
                ))
            })?)?;
            if !self.matches(&key) {
                continue;
            }
            if selected.len() == self.config.max_entries {
                return Err(SourceDriverError::LimitExceeded(format!(
                    "key/value snapshot exceeds {} selected entries",
                    self.config.max_entries
                )));
            }
            let value = row
                .column(&self.config.value_column)
                .ok_or_else(|| {
                    SourceDriverError::InvalidConfig(format!(
                        "key/value query does not return value column {}",
                        self.config.value_column
                    ))
                })?
                .clone();
            let payload = KeyValueRecord {
                key: key.clone(),
                value,
            }
            .encode();
            selected_bytes = selected_bytes.checked_add(payload.len()).ok_or_else(|| {
                SourceDriverError::LimitExceeded("key/value snapshot size overflowed".to_string())
            })?;
            if selected_bytes > self.config.max_snapshot_bytes {
                return Err(SourceDriverError::LimitExceeded(format!(
                    "key/value snapshot exceeds {} bytes",
                    self.config.max_snapshot_bytes
                )));
            }
            selected.push((key, payload));
        }
        selected.sort_by(|left, right| left.0.cmp(&right.0));
        let revision = selected_revision(&selected);
        let entry_count = selected.len() as u64;
        if !force_replay
            && previous.is_some_and(|old| {
                old.identity == source_checkpoint.identity
                    && old.schema_version == source_checkpoint.schema_version
                    && old.revision == revision
                    && old.entry_count == entry_count
            })
        {
            return Ok(KeyValueRead::Unchanged {
                checkpoint: previous.expect("checked above").clone(),
            });
        }
        let generation = match previous {
            Some(old) => old.generation.checked_add(1).ok_or_else(|| {
                SourceDriverError::InvalidCursor(
                    "key/value snapshot generation overflowed".to_string(),
                )
            })?,
            None => 1,
        };
        let checkpoint = KeyValueCheckpoint {
            generation,
            identity: source_checkpoint.identity,
            schema_version: source_checkpoint.schema_version,
            revision,
            entry_count,
        };
        let cursor_start = previous.map_or(Revision::ZERO, |old| old.revision);
        let records = selected
            .into_iter()
            .enumerate()
            .map(|(ordinal, (_, payload))| {
                Ok(SourceRecord::new(
                    origin,
                    generation,
                    SourceCursor::key_value_snapshot(cursor_start),
                    checkpoint.cursor(),
                    u32::try_from(ordinal).map_err(|_| {
                        SourceDriverError::LimitExceeded(
                            "key/value record ordinal exceeds u32".to_string(),
                        )
                    })?,
                    payload,
                ))
            })
            .collect::<Result<Vec<_>, SourceDriverError>>()?;
        Ok(KeyValueRead::Snapshot {
            records,
            checkpoint,
            generation_changed: previous.is_some(),
        })
    }

    fn matches(&self, key: &[u8]) -> bool {
        self.config
            .exact_keys
            .binary_search_by(|candidate| candidate.as_slice().cmp(key))
            .is_ok()
            || self
                .config
                .key_prefixes
                .iter()
                .any(|prefix| key.starts_with(prefix))
    }
}

fn validate_config(config: &KeyValueSnapshotConfig) -> Result<(), SourceDriverError> {
    if config.query_name.trim().is_empty()
        || config.sql.trim().is_empty()
        || config.key_column.trim().is_empty()
        || config.value_column.trim().is_empty()
        || config.key_column == config.value_column
        || (config.exact_keys.is_empty() && config.key_prefixes.is_empty())
        || config
            .exact_keys
            .iter()
            .chain(&config.key_prefixes)
            .any(Vec::is_empty)
        || config.max_scan_rows == 0
        || config.max_entries == 0
        || config.max_entries > config.max_scan_rows
    {
        return Err(SourceDriverError::InvalidConfig(
            "key/value snapshot requires a named query, distinct columns, selectors, and valid bounds"
                .to_string(),
        ));
    }
    Ok(())
}

fn value_key(value: &SqliteValue) -> Result<Vec<u8>, SourceDriverError> {
    match value {
        SqliteValue::Text(value) | SqliteValue::Blob(value) if !value.is_empty() => {
            Ok(value.clone())
        }
        _ => Err(SourceDriverError::InvalidConfig(
            "key/value source keys must be non-empty TEXT or BLOB values".to_string(),
        )),
    }
}

fn selected_revision(entries: &[(Vec<u8>, Vec<u8>)]) -> Revision {
    let mut hasher = blake3::Hasher::new();
    hasher.update(SNAPSHOT_DOMAIN);
    for (_, payload) in entries {
        hasher.update(&(payload.len() as u64).to_be_bytes());
        hasher.update(payload);
    }
    Revision::from_bytes(*hasher.finalize().as_bytes())
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
                SourceDriverError::InvalidCursor("truncated key/value integer".to_string())
            })?,
        ))),
        2 => Ok(SqliteValue::RealBits(u64::from_be_bytes(
            reader.take(8)?.try_into().map_err(|_| {
                SourceDriverError::InvalidCursor("truncated key/value real".to_string())
            })?,
        ))),
        3 => Ok(SqliteValue::Text(take_bytes(reader)?.to_vec())),
        4 => Ok(SqliteValue::Blob(take_bytes(reader)?.to_vec())),
        tag => Err(SourceDriverError::InvalidCursor(format!(
            "unknown key/value value tag {tag}"
        ))),
    }
}

fn put_bytes(output: &mut Vec<u8>, value: &[u8]) {
    output.extend_from_slice(&(value.len() as u64).to_be_bytes());
    output.extend_from_slice(value);
}

fn take_bytes<'a>(reader: &mut CursorReader<'a>) -> Result<&'a [u8], SourceDriverError> {
    let length = reader.usize()?;
    reader.take(length)
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;
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

    fn fixture() -> (TempDir, std::path::PathBuf) {
        let root = TempDir::new().unwrap();
        let path = root.path().join("state.db");
        Connection::open(&path)
            .unwrap()
            .execute_batch(
                "CREATE TABLE state(key TEXT PRIMARY KEY, value BLOB);\n\
                 INSERT INTO state VALUES\n\
                   ('agent.exact', x'31'),\n\
                   ('agent.prefix.one', x'32'),\n\
                   ('unrelated', x'33');",
            )
            .unwrap();
        (root, path)
    }

    fn config() -> KeyValueSnapshotConfig {
        let mut config = KeyValueSnapshotConfig::bounded(
            "state",
            "SELECT key, value FROM state",
            "key",
            "value",
        );
        config.exact_keys = vec![b"agent.exact".to_vec()];
        config.key_prefixes = vec![b"agent.prefix.".to_vec()];
        config
    }

    #[test]
    fn exact_and_prefix_entries_converge_and_ignore_unrelated_changes() {
        let (_root, path) = fixture();
        let driver = KeyValueSnapshot::new(config()).unwrap();
        let KeyValueRead::Snapshot {
            records,
            checkpoint,
            ..
        } = driver.read(&path, None, &origin(), false).unwrap()
        else {
            panic!("first read should snapshot selected keys")
        };
        assert_eq!(records.len(), 2);
        assert_eq!(
            KeyValueRecord::decode(&records[0].payload).unwrap().key,
            b"agent.exact"
        );
        assert_eq!(
            KeyValueCheckpoint::decode(&checkpoint.encode()).unwrap(),
            checkpoint
        );

        Connection::open(&path)
            .unwrap()
            .execute("UPDATE state SET value = x'39' WHERE key = 'unrelated'", [])
            .unwrap();
        assert!(matches!(
            driver
                .read(&path, Some(&checkpoint), &origin(), false)
                .unwrap(),
            KeyValueRead::Unchanged { .. }
        ));

        Connection::open(&path)
            .unwrap()
            .execute("DELETE FROM state WHERE key = 'agent.prefix.one'", [])
            .unwrap();
        let KeyValueRead::Snapshot {
            records,
            checkpoint: next,
            generation_changed,
        } = driver
            .read(&path, Some(&checkpoint), &origin(), false)
            .unwrap()
        else {
            panic!("selected deletion should replace the snapshot")
        };
        assert_eq!(records.len(), 1);
        assert!(generation_changed);
        assert_eq!(next.generation, checkpoint.generation + 1);
    }

    #[test]
    fn selectors_and_selected_entry_bounds_fail_closed() {
        let (_root, path) = fixture();
        let mut invalid = config();
        invalid.exact_keys.clear();
        invalid.key_prefixes.clear();
        assert!(KeyValueSnapshot::new(invalid).is_err());

        let mut bounded = config();
        bounded.max_entries = 1;
        let error = KeyValueSnapshot::new(bounded)
            .unwrap()
            .read(&path, None, &origin(), false)
            .unwrap_err();
        assert!(matches!(error, SourceDriverError::LimitExceeded(_)));
    }
}
