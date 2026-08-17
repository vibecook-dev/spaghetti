use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use super::file::{file_identity, open_confined_file, parent_and_file_name};
use super::model::{io_error, CursorReader};
use super::{
    DriverQuarantine, FileIdentity, RecordHash, RecordOrigin, Revision, SourceCursor,
    SourceDriverError, SourceRecord,
};

const CHECKPOINT_MAGIC: &[u8] = b"SPAD";
const READ_BUFFER_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppendDelimitedConfig {
    pub delimiter: u8,
    pub normalize_crlf: bool,
    pub max_record_bytes: usize,
    pub max_batch_bytes: usize,
    pub max_records_per_batch: usize,
    pub prefix_anchor_bytes: usize,
}

impl AppendDelimitedConfig {
    pub fn json_lines() -> Self {
        Self {
            delimiter: b'\n',
            normalize_crlf: true,
            max_record_bytes: 4 * 1024 * 1024,
            max_batch_bytes: 8 * 1024 * 1024,
            max_records_per_batch: 1_024,
            prefix_anchor_bytes: 4 * 1024,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppendTransition {
    Initial,
    Continued,
    Truncated,
    IdentityChanged,
    PrefixMismatch,
    ContractReplay,
}

impl AppendTransition {
    pub fn starts_new_generation(self) -> bool {
        !matches!(self, Self::Continued)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppendCheckpoint {
    pub generation: u64,
    pub identity: FileIdentity,
    pub committed_offset: u64,
    pub observed_len: u64,
    pub unread_len: u64,
    pub incomplete_suffix_len: u64,
    prefix_start: u64,
    prefix_len: u64,
    prefix_hash: Revision,
}

impl AppendCheckpoint {
    pub fn cursor(&self) -> SourceCursor {
        SourceCursor::append_offset(self.committed_offset)
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(128);
        bytes.extend_from_slice(CHECKPOINT_MAGIC);
        bytes.push(1);
        bytes.extend_from_slice(&self.generation.to_be_bytes());
        self.identity.encode_into(&mut bytes);
        bytes.extend_from_slice(&self.committed_offset.to_be_bytes());
        bytes.extend_from_slice(&self.observed_len.to_be_bytes());
        bytes.extend_from_slice(&self.unread_len.to_be_bytes());
        bytes.extend_from_slice(&self.incomplete_suffix_len.to_be_bytes());
        bytes.extend_from_slice(&self.prefix_start.to_be_bytes());
        bytes.extend_from_slice(&self.prefix_len.to_be_bytes());
        bytes.extend_from_slice(self.prefix_hash.as_bytes());
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, SourceDriverError> {
        let mut reader = CursorReader::new(bytes, CHECKPOINT_MAGIC)?;
        let checkpoint = Self {
            generation: reader.u64()?,
            identity: FileIdentity::decode_from(&mut reader)?,
            committed_offset: reader.u64()?,
            observed_len: reader.u64()?,
            unread_len: reader.u64()?,
            incomplete_suffix_len: reader.u64()?,
            prefix_start: reader.u64()?,
            prefix_len: reader.u64()?,
            prefix_hash: reader.revision()?,
        };
        reader.finish()?;
        checkpoint.validate()?;
        Ok(checkpoint)
    }

    fn validate(&self) -> Result<(), SourceDriverError> {
        if self.generation == 0 {
            return Err(SourceDriverError::InvalidCursor(
                "append generation must be greater than zero".to_string(),
            ));
        }
        if self.committed_offset > self.observed_len
            || self.unread_len != self.observed_len - self.committed_offset
            || self.incomplete_suffix_len > self.unread_len
            || self.prefix_start.saturating_add(self.prefix_len) != self.committed_offset
        {
            return Err(SourceDriverError::InvalidCursor(
                "append checkpoint ranges are inconsistent".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppendItem {
    Record(SourceRecord),
    Quarantined(DriverQuarantine),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppendRead {
    Missing,
    RetryTransient,
    Batch {
        items: Vec<AppendItem>,
        checkpoint: AppendCheckpoint,
        transition: AppendTransition,
        needs_retry: bool,
        more_available: bool,
        snapshot_len: u64,
        /// Exact bytes read from the source handle, including continuity and
        /// checkpoint prefix anchors. Metadata reads are not byte payloads.
        bytes_read: u64,
    },
}

#[derive(Clone)]
pub struct AppendDelimitedFile {
    config: AppendDelimitedConfig,
}

impl AppendDelimitedFile {
    pub fn new(config: AppendDelimitedConfig) -> Result<Self, SourceDriverError> {
        if config.max_record_bytes == 0
            || config.max_batch_bytes == 0
            || config.max_records_per_batch == 0
            || config.prefix_anchor_bytes == 0
        {
            return Err(SourceDriverError::InvalidConfig(
                "append driver bounds must all be greater than zero".to_string(),
            ));
        }
        if config.max_record_bytes > config.max_batch_bytes {
            return Err(SourceDriverError::InvalidConfig(
                "max_record_bytes must not exceed max_batch_bytes".to_string(),
            ));
        }
        if config.max_records_per_batch > u32::MAX as usize {
            return Err(SourceDriverError::InvalidConfig(
                "max_records_per_batch exceeds provenance ordinal range".to_string(),
            ));
        }
        Ok(Self { config })
    }

    pub fn read(
        &self,
        path: &Path,
        previous: Option<&AppendCheckpoint>,
        origin: &RecordOrigin,
        force_contract_replay: bool,
    ) -> Result<AppendRead, SourceDriverError> {
        let (parent, file_name) = parent_and_file_name(path)?;
        let Some(file) = open_confined_file(parent, file_name)? else {
            return Ok(AppendRead::Missing);
        };
        self.read_opened(
            file,
            path,
            previous,
            origin,
            force_contract_replay,
            u64::MAX,
        )
    }

    pub fn read_confined(
        &self,
        root: &Path,
        relative_path: &Path,
        previous: Option<&AppendCheckpoint>,
        origin: &RecordOrigin,
        force_contract_replay: bool,
    ) -> Result<AppendRead, SourceDriverError> {
        let path = root.join(relative_path);
        let Some(file) = open_confined_file(root, relative_path)? else {
            return Ok(AppendRead::Missing);
        };
        self.read_opened(
            file,
            &path,
            previous,
            origin,
            force_contract_replay,
            u64::MAX,
        )
    }

    /// Confined append read with an exact physical source-byte ceiling. The
    /// limit covers framing plus continuity/checkpoint anchors and is enforced
    /// before each operating-system read.
    pub fn read_confined_bounded(
        &self,
        root: &Path,
        relative_path: &Path,
        previous: Option<&AppendCheckpoint>,
        origin: &RecordOrigin,
        force_contract_replay: bool,
        max_read_bytes: u64,
    ) -> Result<AppendRead, SourceDriverError> {
        if max_read_bytes == 0 {
            return Err(SourceDriverError::InvalidConfig(
                "append physical read limit must be greater than zero".to_string(),
            ));
        }
        let path = root.join(relative_path);
        let Some(file) = open_confined_file(root, relative_path)? else {
            return Ok(AppendRead::Missing);
        };
        self.read_opened(
            file,
            &path,
            previous,
            origin,
            force_contract_replay,
            max_read_bytes,
        )
    }

    fn read_opened(
        &self,
        mut file: File,
        path: &Path,
        previous: Option<&AppendCheckpoint>,
        origin: &RecordOrigin,
        force_contract_replay: bool,
        max_read_bytes: u64,
    ) -> Result<AppendRead, SourceDriverError> {
        let mut accounting = ReadAccounting::new(max_read_bytes);
        let mut read_context = ReadContext {
            path,
            accounting: &mut accounting,
        };
        let initial_metadata = file
            .metadata()
            .map_err(|error| io_error("reading metadata for", path, error))?;
        let snapshot_len = initial_metadata.len();
        let identity = file_identity(path, &initial_metadata);

        let (generation, start_offset, transition) = self.resume_point(
            &mut file,
            snapshot_len,
            &identity,
            previous,
            force_contract_replay,
            &mut read_context,
        )?;

        let framed = self.frame(
            &mut file,
            start_offset,
            snapshot_len,
            generation,
            origin,
            &mut read_context,
        )?;
        let prefix = self.prefix_anchor(&mut file, framed.committed_offset, &mut read_context)?;

        let handle_metadata = file
            .metadata()
            .map_err(|error| io_error("rechecking metadata for", path, error))?;
        let path_metadata = match std::fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(AppendRead::RetryTransient);
            }
            Err(error) => return Err(io_error("rechecking path for", path, error)),
        };
        if file_identity(path, &handle_metadata) != identity
            || file_identity(path, &path_metadata) != identity
            || handle_metadata.len() < snapshot_len
            || path_metadata.len() < snapshot_len
        {
            return Ok(AppendRead::RetryTransient);
        }

        let unread_len = snapshot_len - framed.committed_offset;
        let checkpoint = AppendCheckpoint {
            generation,
            identity,
            committed_offset: framed.committed_offset,
            observed_len: snapshot_len,
            unread_len,
            incomplete_suffix_len: framed.incomplete_suffix_len,
            prefix_start: prefix.start,
            prefix_len: prefix.len,
            prefix_hash: prefix.hash,
        };
        checkpoint.validate()?;

        Ok(AppendRead::Batch {
            items: framed.items,
            checkpoint,
            transition,
            needs_retry: framed.incomplete_suffix_len > 0 || framed.more_available,
            more_available: framed.more_available,
            snapshot_len,
            bytes_read: accounting.consumed,
        })
    }

    fn resume_point(
        &self,
        file: &mut File,
        snapshot_len: u64,
        identity: &FileIdentity,
        previous: Option<&AppendCheckpoint>,
        force_contract_replay: bool,
        context: &mut ReadContext<'_>,
    ) -> Result<(u64, u64, AppendTransition), SourceDriverError> {
        let Some(previous) = previous else {
            return Ok((1, 0, AppendTransition::Initial));
        };
        previous.validate()?;
        if force_contract_replay {
            return Ok((
                next_generation(previous.generation)?,
                0,
                AppendTransition::ContractReplay,
            ));
        }
        if &previous.identity != identity {
            return Ok((
                next_generation(previous.generation)?,
                0,
                AppendTransition::IdentityChanged,
            ));
        }
        if snapshot_len < previous.committed_offset {
            return Ok((
                next_generation(previous.generation)?,
                0,
                AppendTransition::Truncated,
            ));
        }
        if !verify_prefix(file, previous, context)? {
            return Ok((
                next_generation(previous.generation)?,
                0,
                AppendTransition::PrefixMismatch,
            ));
        }
        Ok((
            previous.generation,
            previous.committed_offset,
            AppendTransition::Continued,
        ))
    }

    fn frame(
        &self,
        file: &mut File,
        start_offset: u64,
        snapshot_len: u64,
        generation: u64,
        origin: &RecordOrigin,
        context: &mut ReadContext<'_>,
    ) -> Result<FramedBatch, SourceDriverError> {
        file.seek(SeekFrom::Start(start_offset))
            .map_err(|error| io_error("seeking", context.path, error))?;

        let mut output = FramedBatch::new(start_offset);
        let mut pending = PendingRecord::new(self.config.max_record_bytes);
        let mut buffer = [0_u8; READ_BUFFER_BYTES];
        let mut read_offset = start_offset;
        let mut batch_payload_bytes = 0_usize;

        'read: while read_offset < snapshot_len {
            let wanted =
                usize::try_from((snapshot_len - read_offset).min(READ_BUFFER_BYTES as u64))
                    .expect("bounded read length always fits usize");
            let count = context
                .accounting
                .read(file, &mut buffer[..wanted], context.path)?;
            if count == 0 {
                return Ok(FramedBatch::transient(output));
            }

            let chunk = &buffer[..count];
            let mut segment_start = 0;
            for (index, byte) in chunk.iter().enumerate() {
                if *byte != self.config.delimiter {
                    continue;
                }
                pending.push(&chunk[segment_start..index]);
                let cursor_end = read_offset + index as u64 + 1;
                let payload_len = pending.delivered_len(self.config.normalize_crlf);
                if !pending.oversized()
                    && batch_payload_bytes.saturating_add(payload_len) > self.config.max_batch_bytes
                {
                    output.more_available = true;
                    break 'read;
                }

                let ordinal = u32::try_from(output.items.len())
                    .expect("max_records_per_batch was validated against u32");
                let item = pending.finish(
                    origin,
                    generation,
                    output.committed_offset,
                    cursor_end,
                    ordinal,
                    self.config.normalize_crlf,
                );
                if let AppendItem::Record(record) = &item {
                    batch_payload_bytes += record.payload.len();
                }
                output.items.push(item);
                output.committed_offset = cursor_end;
                pending = PendingRecord::new(self.config.max_record_bytes);
                segment_start = index + 1;

                if output.items.len() == self.config.max_records_per_batch {
                    output.more_available = cursor_end < snapshot_len;
                    break 'read;
                }
            }
            pending.push(&chunk[segment_start..]);
            read_offset += count as u64;
        }

        if !output.more_available && read_offset >= snapshot_len {
            output.incomplete_suffix_len = pending.len;
        }
        Ok(output)
    }

    fn prefix_anchor(
        &self,
        file: &mut File,
        committed_offset: u64,
        context: &mut ReadContext<'_>,
    ) -> Result<PrefixAnchor, SourceDriverError> {
        let len = committed_offset.min(self.config.prefix_anchor_bytes as u64);
        let start = committed_offset - len;
        if len == 0 {
            return Ok(PrefixAnchor {
                start,
                len,
                hash: Revision::ZERO,
            });
        }
        file.seek(SeekFrom::Start(start))
            .map_err(|error| io_error("seeking to prefix anchor in", context.path, error))?;
        let mut bytes = vec![0; len as usize];
        context.accounting.read_exact(
            file,
            &mut bytes,
            "reading prefix anchor from",
            context.path,
        )?;
        Ok(PrefixAnchor {
            start,
            len,
            hash: Revision::digest(&bytes),
        })
    }
}

struct FramedBatch {
    items: Vec<AppendItem>,
    committed_offset: u64,
    incomplete_suffix_len: u64,
    more_available: bool,
}

impl FramedBatch {
    fn new(committed_offset: u64) -> Self {
        Self {
            items: Vec::new(),
            committed_offset,
            incomplete_suffix_len: 0,
            more_available: false,
        }
    }

    fn transient(mut partial: Self) -> Self {
        partial.more_available = true;
        partial
    }
}

struct PendingRecord {
    retained: Vec<u8>,
    hasher: blake3::Hasher,
    len: u64,
    max_record_bytes: usize,
}

impl PendingRecord {
    fn new(max_record_bytes: usize) -> Self {
        Self {
            retained: Vec::new(),
            hasher: blake3::Hasher::new(),
            len: 0,
            max_record_bytes,
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        self.hasher.update(bytes);
        self.len = self.len.saturating_add(bytes.len() as u64);
        if self.retained.len() <= self.max_record_bytes {
            let remaining = self
                .max_record_bytes
                .saturating_add(1)
                .saturating_sub(self.retained.len());
            self.retained
                .extend_from_slice(&bytes[..bytes.len().min(remaining)]);
        }
    }

    fn oversized(&self) -> bool {
        self.len > self.max_record_bytes as u64
    }

    fn delivered_len(&self, normalize_crlf: bool) -> usize {
        if self.oversized() {
            0
        } else if normalize_crlf && self.retained.last() == Some(&b'\r') {
            self.retained.len() - 1
        } else {
            self.retained.len()
        }
    }

    fn finish(
        mut self,
        origin: &RecordOrigin,
        generation: u64,
        cursor_start: u64,
        cursor_end: u64,
        ordinal: u32,
        normalize_crlf: bool,
    ) -> AppendItem {
        if self.oversized() {
            return AppendItem::Quarantined(DriverQuarantine {
                generation,
                cursor_start: SourceCursor::append_offset(cursor_start),
                cursor_end: SourceCursor::append_offset(cursor_end),
                ordinal_in_batch: ordinal,
                payload_len: self.len,
                payload_hash: RecordHash::from_bytes(*self.hasher.finalize().as_bytes()),
                reason: format!(
                    "delimiter-terminated record exceeds {} byte limit",
                    self.max_record_bytes
                ),
            });
        }
        if normalize_crlf && self.retained.last() == Some(&b'\r') {
            self.retained.pop();
        }
        AppendItem::Record(SourceRecord::new(
            origin,
            generation,
            SourceCursor::append_offset(cursor_start),
            SourceCursor::append_offset(cursor_end),
            ordinal,
            self.retained,
        ))
    }
}

struct PrefixAnchor {
    start: u64,
    len: u64,
    hash: Revision,
}

struct ReadAccounting {
    limit: u64,
    consumed: u64,
}

struct ReadContext<'a> {
    path: &'a Path,
    accounting: &'a mut ReadAccounting,
}

impl ReadAccounting {
    fn new(limit: u64) -> Self {
        Self { limit, consumed: 0 }
    }

    fn read(
        &mut self,
        file: &mut File,
        buffer: &mut [u8],
        path: &Path,
    ) -> Result<usize, SourceDriverError> {
        let remaining = self.limit.saturating_sub(self.consumed);
        if remaining == 0 && !buffer.is_empty() {
            return Err(self.limit_error());
        }
        let allowed = buffer
            .len()
            .min(usize::try_from(remaining).unwrap_or(usize::MAX));
        let count = file
            .read(&mut buffer[..allowed])
            .map_err(|error| io_error("reading", path, error))?;
        self.consumed = self.consumed.saturating_add(count as u64);
        Ok(count)
    }

    fn read_exact(
        &mut self,
        file: &mut File,
        buffer: &mut [u8],
        operation: &'static str,
        path: &Path,
    ) -> Result<(), SourceDriverError> {
        if buffer.len() as u64 > self.limit.saturating_sub(self.consumed) {
            return Err(self.limit_error());
        }
        file.read_exact(buffer)
            .map_err(|error| io_error(operation, path, error))?;
        self.consumed = self.consumed.saturating_add(buffer.len() as u64);
        Ok(())
    }

    fn limit_error(&self) -> SourceDriverError {
        SourceDriverError::LimitExceeded(format!(
            "append source reads exceed {} byte access reservation",
            self.limit
        ))
    }
}

fn verify_prefix(
    file: &mut File,
    checkpoint: &AppendCheckpoint,
    context: &mut ReadContext<'_>,
) -> Result<bool, SourceDriverError> {
    if checkpoint.prefix_len == 0 {
        return Ok(true);
    }
    file.seek(SeekFrom::Start(checkpoint.prefix_start))
        .map_err(|error| io_error("seeking to verify prefix in", context.path, error))?;
    let mut bytes = vec![0; checkpoint.prefix_len as usize];
    context
        .accounting
        .read_exact(file, &mut bytes, "verifying prefix in", context.path)?;
    Ok(Revision::digest(&bytes) == checkpoint.prefix_hash)
}

fn next_generation(current: u64) -> Result<u64, SourceDriverError> {
    current
        .checked_add(1)
        .ok_or_else(|| SourceDriverError::InvalidCursor("source generation overflowed".to_string()))
}

#[cfg(test)]
mod tests {
    use std::fs::OpenOptions;
    use std::io::Write;

    use tempfile::{NamedTempFile, TempDir};

    use super::*;
    use crate::source::SourceMediaType;

    fn origin() -> RecordOrigin {
        RecordOrigin {
            source_instance_id: 10,
            stream_id: 20,
            object_id: 30,
            observed_at: 40,
            source_timestamp_hint: None,
            media_type: SourceMediaType::new("application/x-ndjson").unwrap(),
        }
    }

    fn driver() -> AppendDelimitedFile {
        let mut config = AppendDelimitedConfig::json_lines();
        config.max_record_bytes = 16;
        config.max_batch_bytes = 32;
        config.max_records_per_batch = 8;
        AppendDelimitedFile::new(config).unwrap()
    }

    fn batch(
        read: AppendRead,
    ) -> (
        Vec<AppendItem>,
        AppendCheckpoint,
        AppendTransition,
        bool,
        bool,
    ) {
        match read {
            AppendRead::Batch {
                items,
                checkpoint,
                transition,
                needs_retry,
                more_available,
                ..
            } => (items, checkpoint, transition, needs_retry, more_available),
            other => panic!("expected stable append batch, got {other:?}"),
        }
    }

    #[test]
    fn partial_final_record_is_not_emitted_or_advanced() {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(b"{\"one\":1}\n{\"two\":").unwrap();
        file.flush().unwrap();

        let (items, checkpoint, transition, retry, _) =
            batch(driver().read(file.path(), None, &origin(), false).unwrap());
        assert_eq!(transition, AppendTransition::Initial);
        assert_eq!(items.len(), 1);
        assert_eq!(checkpoint.committed_offset, 10);
        assert_eq!(checkpoint.incomplete_suffix_len, 7);
        assert!(retry);

        let mut append = OpenOptions::new().append(true).open(file.path()).unwrap();
        append.write_all(b"2}\n").unwrap();
        append.flush().unwrap();
        let (items, next, transition, retry, _) = batch(
            driver()
                .read(file.path(), Some(&checkpoint), &origin(), false)
                .unwrap(),
        );
        assert_eq!(transition, AppendTransition::Continued);
        assert_eq!(items.len(), 1);
        let AppendItem::Record(record) = &items[0] else {
            panic!("expected ordinary record");
        };
        assert_eq!(record.payload, b"{\"two\":2}");
        assert_eq!(record.cursor_start.append_offset_value(), Some(10));
        assert_eq!(record.cursor_end.append_offset_value(), Some(20));
        assert_eq!(next.committed_offset, 20);
        assert!(!retry);
    }

    #[test]
    fn crlf_is_normalized_after_byte_cursors_are_calculated() {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(b"one\r\ntwo\r\n").unwrap();
        file.flush().unwrap();
        let (items, checkpoint, _, _, _) =
            batch(driver().read(file.path(), None, &origin(), false).unwrap());
        let payloads: Vec<_> = items
            .iter()
            .map(|item| match item {
                AppendItem::Record(record) => record.payload.clone(),
                AppendItem::Quarantined(_) => panic!("unexpected quarantine"),
            })
            .collect();
        assert_eq!(payloads, [b"one".to_vec(), b"two".to_vec()]);
        assert_eq!(checkpoint.committed_offset, 10);
    }

    #[test]
    fn complete_oversized_record_is_quarantined_and_cursor_advances() {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(b"0123456789abcdefX\nok\n").unwrap();
        file.flush().unwrap();
        let (items, checkpoint, _, _, _) =
            batch(driver().read(file.path(), None, &origin(), false).unwrap());
        assert!(matches!(items[0], AppendItem::Quarantined(_)));
        assert!(matches!(items[1], AppendItem::Record(_)));
        assert_eq!(checkpoint.committed_offset, 21);
    }

    #[test]
    fn truncate_and_rewrite_starts_a_new_generation() {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(b"first-record\n").unwrap();
        file.flush().unwrap();
        let (_, checkpoint, _, _, _) =
            batch(driver().read(file.path(), None, &origin(), false).unwrap());

        std::fs::write(file.path(), b"new\n").unwrap();
        let (items, next, transition, _, _) = batch(
            driver()
                .read(file.path(), Some(&checkpoint), &origin(), false)
                .unwrap(),
        );
        assert_eq!(transition, AppendTransition::Truncated);
        assert_eq!(next.generation, checkpoint.generation + 1);
        assert_eq!(items.len(), 1);
    }

    #[test]
    fn same_size_prefix_rewrite_starts_a_new_generation() {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(b"old\n").unwrap();
        file.flush().unwrap();
        let (_, checkpoint, _, _, _) =
            batch(driver().read(file.path(), None, &origin(), false).unwrap());
        std::fs::write(file.path(), b"new\n").unwrap();

        let (_, next, transition, _, _) = batch(
            driver()
                .read(file.path(), Some(&checkpoint), &origin(), false)
                .unwrap(),
        );
        assert_eq!(transition, AppendTransition::PrefixMismatch);
        assert_eq!(next.generation, checkpoint.generation + 1);
    }

    #[test]
    fn atomic_identity_replacement_starts_a_new_generation() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("stream.jsonl");
        std::fs::write(&path, b"old\n").unwrap();
        let (_, checkpoint, _, _, _) = batch(driver().read(&path, None, &origin(), false).unwrap());
        let replacement = directory.path().join("replacement");
        std::fs::write(&replacement, b"new\n").unwrap();
        std::fs::rename(replacement, &path).unwrap();

        let (_, next, transition, _, _) = batch(
            driver()
                .read(&path, Some(&checkpoint), &origin(), false)
                .unwrap(),
        );
        assert_eq!(transition, AppendTransition::IdentityChanged);
        assert_eq!(next.generation, checkpoint.generation + 1);
    }

    #[test]
    fn contract_replay_reframes_from_zero_in_a_new_generation() {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(b"one\ntwo\n").unwrap();
        file.flush().unwrap();
        let (_, checkpoint, _, _, _) =
            batch(driver().read(file.path(), None, &origin(), false).unwrap());

        let (items, next, transition, _, _) = batch(
            driver()
                .read(file.path(), Some(&checkpoint), &origin(), true)
                .unwrap(),
        );
        assert_eq!(transition, AppendTransition::ContractReplay);
        assert_eq!(next.generation, checkpoint.generation + 1);
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn batch_record_limit_preserves_next_record_boundary() {
        let mut config = AppendDelimitedConfig::json_lines();
        config.max_record_bytes = 8;
        config.max_batch_bytes = 8;
        config.max_records_per_batch = 1;
        let driver = AppendDelimitedFile::new(config).unwrap();
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(b"one\ntwo\n").unwrap();
        file.flush().unwrap();

        let (first, checkpoint, _, retry, more) =
            batch(driver.read(file.path(), None, &origin(), false).unwrap());
        assert_eq!(first.len(), 1);
        assert_eq!(checkpoint.committed_offset, 4);
        assert!(retry && more);
        let (second, checkpoint, _, retry, more) = batch(
            driver
                .read(file.path(), Some(&checkpoint), &origin(), false)
                .unwrap(),
        );
        assert_eq!(second.len(), 1);
        assert_eq!(checkpoint.committed_offset, 8);
        assert!(!retry && !more);
    }

    #[test]
    fn bounded_read_reports_physical_bytes_and_fails_before_overrun() {
        let root = TempDir::new().unwrap();
        let relative = Path::new("stream.jsonl");
        let path = root.path().join(relative);
        std::fs::write(&path, b"one\ntwo\n").unwrap();
        let driver = driver();

        assert!(matches!(
            driver.read_confined_bounded(root.path(), relative, None, &origin(), false, 15),
            Err(SourceDriverError::LimitExceeded(_))
        ));
        let read = driver
            .read_confined_bounded(root.path(), relative, None, &origin(), false, 16)
            .unwrap();
        let AppendRead::Batch {
            checkpoint,
            bytes_read,
            ..
        } = read
        else {
            panic!("expected append batch");
        };
        assert_eq!(bytes_read, 16); // 8 framed bytes + 8-byte checkpoint anchor.

        let mut append = OpenOptions::new().append(true).open(&path).unwrap();
        append.write_all(b"three\n").unwrap();
        append.flush().unwrap();
        let read = driver
            .read_confined_bounded(
                root.path(),
                relative,
                Some(&checkpoint),
                &origin(),
                false,
                28,
            )
            .unwrap();
        let AppendRead::Batch { bytes_read, .. } = read else {
            panic!("expected continued append batch");
        };
        assert_eq!(bytes_read, 28); // 8 verify + 6 append + 14 new anchor.
    }

    #[test]
    fn checkpoint_encoding_round_trips_and_rejects_corruption() {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(b"one\npartial").unwrap();
        file.flush().unwrap();
        let (_, checkpoint, _, _, _) =
            batch(driver().read(file.path(), None, &origin(), false).unwrap());
        assert_eq!(
            AppendCheckpoint::decode(&checkpoint.encode()).unwrap(),
            checkpoint
        );
        let mut corrupt = checkpoint.encode();
        corrupt.push(1);
        assert!(AppendCheckpoint::decode(&corrupt).is_err());
    }

    #[test]
    fn missing_file_is_reported_without_advancing_state() {
        let directory = TempDir::new().unwrap();
        assert_eq!(
            driver()
                .read(&directory.path().join("missing"), None, &origin(), false)
                .unwrap(),
            AppendRead::Missing
        );
    }
}
