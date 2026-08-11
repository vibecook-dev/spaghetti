use std::fmt;
use std::sync::Arc;

const CURSOR_VERSION: u8 = 1;
const APPEND_CURSOR_KIND: u8 = 1;
const SNAPSHOT_CURSOR_KIND: u8 = 2;
const DIRECTORY_CURSOR_KIND: u8 = 3;
const PRESENCE_CURSOR_KIND: u8 = 4;
const HASH_BYTES: usize = 32;

/// Opaque, versioned cursor carried from a driver into provenance storage.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SourceCursor(Vec<u8>);

impl SourceCursor {
    pub fn from_opaque(bytes: Vec<u8>) -> Result<Self, SourceDriverError> {
        if bytes.len() < 2 || bytes[0] != CURSOR_VERSION {
            return Err(SourceDriverError::InvalidCursor(
                "unsupported or truncated source cursor".to_string(),
            ));
        }
        Ok(Self(bytes))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }

    pub fn append_offset(offset: u64) -> Self {
        let mut bytes = Vec::with_capacity(10);
        bytes.extend_from_slice(&[CURSOR_VERSION, APPEND_CURSOR_KIND]);
        bytes.extend_from_slice(&offset.to_be_bytes());
        Self(bytes)
    }

    pub fn append_offset_value(&self) -> Option<u64> {
        if self.0.len() != 10 || self.0[0] != CURSOR_VERSION || self.0[1] != APPEND_CURSOR_KIND {
            return None;
        }
        Some(u64::from_be_bytes(self.0[2..].try_into().ok()?))
    }

    pub fn snapshot(revision: Revision) -> Self {
        Self::hash_cursor(SNAPSHOT_CURSOR_KIND, revision)
    }

    pub fn directory(revision: Revision) -> Self {
        Self::hash_cursor(DIRECTORY_CURSOR_KIND, revision)
    }

    pub fn presence(revision: Revision) -> Self {
        Self::hash_cursor(PRESENCE_CURSOR_KIND, revision)
    }

    fn hash_cursor(kind: u8, revision: Revision) -> Self {
        let mut bytes = Vec::with_capacity(2 + HASH_BYTES);
        bytes.extend_from_slice(&[CURSOR_VERSION, kind]);
        bytes.extend_from_slice(revision.as_bytes());
        Self(bytes)
    }
}

impl fmt::Debug for SourceCursor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("SourceCursor")
            .field(&HexBytes(&self.0))
            .finish()
    }
}

/// BLAKE3 revision for a stable source snapshot.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Revision([u8; HASH_BYTES]);

impl Revision {
    pub const ZERO: Self = Self([0; HASH_BYTES]);

    pub fn digest(bytes: &[u8]) -> Self {
        Self(*blake3::hash(bytes).as_bytes())
    }

    pub fn from_bytes(bytes: [u8; HASH_BYTES]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; HASH_BYTES] {
        &self.0
    }
}

impl fmt::Debug for Revision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("Revision")
            .field(&HexBytes(&self.0))
            .finish()
    }
}

/// Hash of the payload bytes actually delivered to the adapter.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RecordHash([u8; HASH_BYTES]);

impl RecordHash {
    pub fn digest(bytes: &[u8]) -> Self {
        Self(*blake3::hash(bytes).as_bytes())
    }

    pub fn from_bytes(bytes: [u8; HASH_BYTES]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; HASH_BYTES] {
        &self.0
    }
}

impl fmt::Debug for RecordHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("RecordHash")
            .field(&HexBytes(&self.0))
            .finish()
    }
}

struct HexBytes<'a>(&'a [u8]);

impl fmt::Debug for HexBytes<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// Native file identity when the platform exposes one, otherwise a confined
/// path identity. Display paths are deliberately kept separate.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FileIdentity {
    Unix { device: u64, inode: u64 },
    Windows { volume: u64, file: u128 },
    ConfinedPath(Vec<u8>),
}

impl FileIdentity {
    pub(crate) fn encode_into(&self, output: &mut Vec<u8>) {
        match self {
            Self::Unix { device, inode } => {
                output.push(1);
                output.extend_from_slice(&device.to_be_bytes());
                output.extend_from_slice(&inode.to_be_bytes());
            }
            Self::Windows { volume, file } => {
                output.push(2);
                output.extend_from_slice(&volume.to_be_bytes());
                output.extend_from_slice(&file.to_be_bytes());
            }
            Self::ConfinedPath(path) => {
                output.push(3);
                output.extend_from_slice(&(path.len() as u64).to_be_bytes());
                output.extend_from_slice(path);
            }
        }
    }

    pub(crate) fn decode_from(input: &mut CursorReader<'_>) -> Result<Self, SourceDriverError> {
        match input.byte()? {
            1 => Ok(Self::Unix {
                device: input.u64()?,
                inode: input.u64()?,
            }),
            2 => Ok(Self::Windows {
                volume: input.u64()?,
                file: input.u128()?,
            }),
            3 => {
                let length = input.usize()?;
                Ok(Self::ConfinedPath(input.take(length)?.to_vec()))
            }
            tag => Err(SourceDriverError::InvalidCursor(format!(
                "unknown file identity tag {tag}"
            ))),
        }
    }
}

/// Stable IDs and observation metadata supplied by the common engine after an
/// object has been catalogued.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordOrigin {
    pub source_instance_id: u64,
    pub stream_id: u64,
    pub object_id: u64,
    pub observed_at: i64,
    pub source_timestamp_hint: Option<i64>,
    pub media_type: SourceMediaType,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SourceMediaType(Arc<str>);

impl SourceMediaType {
    pub fn new(value: impl Into<Arc<str>>) -> Result<Self, SourceDriverError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(SourceDriverError::InvalidConfig(
                "source media type must not be empty".to_string(),
            ));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One raw, framed source record. The adapter owns interpretation of payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRecord {
    pub source_instance_id: u64,
    pub stream_id: u64,
    pub object_id: u64,
    pub generation: u64,
    pub cursor_start: SourceCursor,
    pub cursor_end: SourceCursor,
    pub ordinal_in_batch: u32,
    pub observed_at: i64,
    pub source_timestamp_hint: Option<i64>,
    pub media_type: SourceMediaType,
    pub payload: Vec<u8>,
    pub payload_hash: RecordHash,
}

impl SourceRecord {
    pub(crate) fn new(
        origin: &RecordOrigin,
        generation: u64,
        cursor_start: SourceCursor,
        cursor_end: SourceCursor,
        ordinal_in_batch: u32,
        payload: Vec<u8>,
    ) -> Self {
        let payload_hash = RecordHash::digest(&payload);
        Self {
            source_instance_id: origin.source_instance_id,
            stream_id: origin.stream_id,
            object_id: origin.object_id,
            generation,
            cursor_start,
            cursor_end,
            ordinal_in_batch,
            observed_at: origin.observed_at,
            source_timestamp_hint: origin.source_timestamp_hint,
            media_type: origin.media_type.clone(),
            payload,
            payload_hash,
        }
    }
}

/// A complete source record the common layer can advance past but will not
/// retain as an ordinary payload because it violates a declared bound.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriverQuarantine {
    pub generation: u64,
    pub cursor_start: SourceCursor,
    pub cursor_end: SourceCursor,
    pub ordinal_in_batch: u32,
    pub payload_len: u64,
    pub payload_hash: RecordHash,
    pub reason: String,
}

#[derive(Debug, thiserror::Error)]
pub enum SourceDriverError {
    #[error("invalid source driver configuration: {0}")]
    InvalidConfig(String),

    #[error("invalid source cursor: {0}")]
    InvalidCursor(String),

    #[error("source path escapes its declared root: {0}")]
    PathEscape(String),

    #[error("source limit exceeded: {0}")]
    LimitExceeded(String),

    #[error("source I/O failed while {operation} {path}: {source}")]
    Io {
        operation: &'static str,
        path: String,
        #[source]
        source: std::io::Error,
    },
}

pub(crate) fn io_error(
    operation: &'static str,
    path: &std::path::Path,
    source: std::io::Error,
) -> SourceDriverError {
    SourceDriverError::Io {
        operation,
        path: path.to_string_lossy().into_owned(),
        source,
    }
}

pub(crate) struct CursorReader<'a> {
    remaining: &'a [u8],
}

impl<'a> CursorReader<'a> {
    pub(crate) fn new(bytes: &'a [u8], magic: &[u8]) -> Result<Self, SourceDriverError> {
        let Some(rest) = bytes.strip_prefix(magic) else {
            return Err(SourceDriverError::InvalidCursor(
                "checkpoint magic does not match driver".to_string(),
            ));
        };
        let mut reader = Self { remaining: rest };
        if reader.byte()? != CURSOR_VERSION {
            return Err(SourceDriverError::InvalidCursor(
                "unsupported checkpoint version".to_string(),
            ));
        }
        Ok(reader)
    }

    pub(crate) fn finish(self) -> Result<(), SourceDriverError> {
        if self.remaining.is_empty() {
            Ok(())
        } else {
            Err(SourceDriverError::InvalidCursor(
                "checkpoint contains trailing bytes".to_string(),
            ))
        }
    }

    pub(crate) fn byte(&mut self) -> Result<u8, SourceDriverError> {
        Ok(self.take(1)?[0])
    }

    pub(crate) fn bool(&mut self) -> Result<bool, SourceDriverError> {
        match self.byte()? {
            0 => Ok(false),
            1 => Ok(true),
            value => Err(SourceDriverError::InvalidCursor(format!(
                "invalid checkpoint boolean {value}"
            ))),
        }
    }

    pub(crate) fn u64(&mut self) -> Result<u64, SourceDriverError> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into().map_err(
            |_| SourceDriverError::InvalidCursor("truncated u64".to_string()),
        )?))
    }

    pub(crate) fn u128(&mut self) -> Result<u128, SourceDriverError> {
        Ok(u128::from_be_bytes(self.take(16)?.try_into().map_err(
            |_| SourceDriverError::InvalidCursor("truncated u128".to_string()),
        )?))
    }

    pub(crate) fn i128(&mut self) -> Result<i128, SourceDriverError> {
        Ok(i128::from_be_bytes(self.take(16)?.try_into().map_err(
            |_| SourceDriverError::InvalidCursor("truncated i128".to_string()),
        )?))
    }

    pub(crate) fn usize(&mut self) -> Result<usize, SourceDriverError> {
        usize::try_from(self.u64()?).map_err(|_| {
            SourceDriverError::InvalidCursor("checkpoint length is too large".to_string())
        })
    }

    pub(crate) fn revision(&mut self) -> Result<Revision, SourceDriverError> {
        let bytes = self
            .take(HASH_BYTES)?
            .try_into()
            .map_err(|_| SourceDriverError::InvalidCursor("truncated revision".to_string()))?;
        Ok(Revision::from_bytes(bytes))
    }

    pub(crate) fn take(&mut self, count: usize) -> Result<&'a [u8], SourceDriverError> {
        if count > self.remaining.len() {
            return Err(SourceDriverError::InvalidCursor(
                "checkpoint is truncated".to_string(),
            ));
        }
        let (value, rest) = self.remaining.split_at(count);
        self.remaining = rest;
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_cursors_sort_by_unsigned_byte_offset() {
        let cursors = [
            SourceCursor::append_offset(0),
            SourceCursor::append_offset(1),
            SourceCursor::append_offset(u32::MAX.into()),
            SourceCursor::append_offset(u64::MAX),
        ];
        assert!(cursors.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(cursors[2].append_offset_value(), Some(u64::from(u32::MAX)));
    }

    #[test]
    fn opaque_cursor_rejects_unknown_versions() {
        let error = SourceCursor::from_opaque(vec![9, APPEND_CURSOR_KIND]).unwrap_err();
        assert!(matches!(error, SourceDriverError::InvalidCursor(_)));
    }

    #[test]
    fn record_hashes_delivered_payload() {
        let origin = RecordOrigin {
            source_instance_id: 1,
            stream_id: 2,
            object_id: 3,
            observed_at: 4,
            source_timestamp_hint: None,
            media_type: SourceMediaType::new("application/json").unwrap(),
        };
        let record = SourceRecord::new(
            &origin,
            1,
            SourceCursor::append_offset(0),
            SourceCursor::append_offset(3),
            0,
            b"{}".to_vec(),
        );
        assert_eq!(record.payload_hash, RecordHash::digest(b"{}"));
    }
}
