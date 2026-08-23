use crate::source::file::FileStamp;
use std::fs::OpenOptions;
use std::io::Cursor;
use std::io::Write;

use tempfile::{NamedTempFile, TempDir};

use super::*;
use crate::source::SourceMediaType;

// Test-only framing over the driver's private read path. It lives in the
// test module so the production region carries no `#[cfg(test)]` items.
impl AppendDelimitedFile {
    /// Frame bytes that were already captured by a descriptor-confined stable
    /// read. This performs no native I/O: the caller remains responsible for
    /// access accounting and for retaining the exact `FileStamp` paired with
    /// `bytes`. The returned `bytes_read` is therefore zero even though the
    /// ordinary framing logic reads the retained buffer internally.
    pub(crate) fn frame_retained_stable(
        &self,
        stamp: &FileStamp,
        bytes: &[u8],
        content_revision: Revision,
        previous: Option<&AppendCheckpoint>,
        origin: &RecordOrigin,
        force_contract_replay: bool,
    ) -> Result<AppendRead, SourceDriverError> {
        let snapshot_len = u64::try_from(bytes.len()).map_err(|_| {
            SourceDriverError::LimitExceeded(
                "retained append member length exceeds the platform range".to_string(),
            )
        })?;
        if stamp.len != snapshot_len || Revision::digest(bytes) != content_revision {
            return Err(SourceDriverError::InvalidCursor(
                "retained append member does not match its stable read".to_string(),
            ));
        }

        let mut retained = Cursor::new(bytes);
        let mut accounting = ReadAccounting::new(u64::MAX);
        let retained_path = Path::new("retained-append-member");
        let mut read_context = ReadContext {
            path: retained_path,
            accounting: &mut accounting,
        };
        let (generation, start_offset, transition) = self.resume_point(
            &mut retained,
            snapshot_len,
            &stamp.identity,
            previous,
            force_contract_replay,
            &mut read_context,
        )?;
        let framed = self.frame(
            &mut retained,
            start_offset,
            snapshot_len,
            generation,
            origin,
            &mut read_context,
        )?;
        let prefix =
            self.prefix_anchor(&mut retained, framed.committed_offset, &mut read_context)?;
        let unread_len = snapshot_len - framed.committed_offset;
        let checkpoint = AppendCheckpoint {
            generation,
            identity: stamp.identity.clone(),
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
            bytes_read: 0,
        })
    }
}

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

fn retained_stamp(bytes: &[u8]) -> FileStamp {
    FileStamp {
        identity: FileIdentity::ConfinedPath(b"retained-member".to_vec()),
        len: bytes.len() as u64,
        modified_ns: 1,
    }
}

#[test]
fn retained_stable_bytes_use_the_append_contract_without_native_io() {
    let mut config = AppendDelimitedConfig::json_lines();
    config.max_record_bytes = 16;
    config.max_batch_bytes = 32;
    config.max_records_per_batch = 1;
    let driver = AppendDelimitedFile::new(config).unwrap();
    let bytes = b"one\ntwo\n";
    let stamp = retained_stamp(bytes);

    let first = driver
        .frame_retained_stable(
            &stamp,
            bytes,
            Revision::digest(bytes),
            None,
            &origin(),
            false,
        )
        .unwrap();
    let AppendRead::Batch {
        items,
        checkpoint,
        transition,
        needs_retry,
        more_available,
        bytes_read,
        ..
    } = first
    else {
        panic!("retained stable bytes must frame as an append batch");
    };
    assert_eq!(transition, AppendTransition::Initial);
    assert_eq!(items.len(), 1);
    assert!(needs_retry);
    assert!(more_available);
    assert_eq!(bytes_read, 0);
    let AppendItem::Record(first_record) = &items[0] else {
        panic!("expected the first retained record");
    };
    assert_eq!(first_record.payload, b"one");

    let second = driver
        .frame_retained_stable(
            &stamp,
            bytes,
            Revision::digest(bytes),
            Some(&checkpoint),
            &origin(),
            false,
        )
        .unwrap();
    let (items, checkpoint, transition, needs_retry, more_available) = batch(second);
    assert_eq!(transition, AppendTransition::Continued);
    assert_eq!(items.len(), 1);
    assert!(!needs_retry);
    assert!(!more_available);
    assert_eq!(checkpoint.committed_offset, bytes.len() as u64);
    let AppendItem::Record(second_record) = &items[0] else {
        panic!("expected the second retained record");
    };
    assert_eq!(second_record.payload, b"two");
}

#[test]
fn retained_stable_bytes_reject_stamp_digest_and_prefix_drift() {
    let driver = driver();
    let bytes = b"old\n";
    let stamp = retained_stamp(bytes);
    assert!(matches!(
        driver.frame_retained_stable(
            &FileStamp {
                len: stamp.len + 1,
                ..stamp.clone()
            },
            bytes,
            Revision::digest(bytes),
            None,
            &origin(),
            false,
        ),
        Err(SourceDriverError::InvalidCursor(_))
    ));
    assert!(matches!(
        driver.frame_retained_stable(
            &stamp,
            bytes,
            Revision::digest(b"other"),
            None,
            &origin(),
            false,
        ),
        Err(SourceDriverError::InvalidCursor(_))
    ));

    let (_, checkpoint, _, _, _) = batch(
        driver
            .frame_retained_stable(
                &stamp,
                bytes,
                Revision::digest(bytes),
                None,
                &origin(),
                false,
            )
            .unwrap(),
    );
    let rewritten = b"new\n";
    let rewritten_stamp = FileStamp {
        len: rewritten.len() as u64,
        ..stamp
    };
    let (_, next, transition, _, _) = batch(
        driver
            .frame_retained_stable(
                &rewritten_stamp,
                rewritten,
                Revision::digest(rewritten),
                Some(&checkpoint),
                &origin(),
                false,
            )
            .unwrap(),
    );
    assert_eq!(transition, AppendTransition::PrefixMismatch);
    assert_eq!(next.generation, checkpoint.generation + 1);
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
fn exact_full_batch_stops_before_scanning_a_delimiter_free_tail() {
    const RECORD_PAYLOAD_BYTES: usize = 65_536;
    const FRAMING_READ_AHEAD_BYTES: usize = 65_536;
    const CHECKPOINT_ANCHOR_BYTES: usize = 4_096;
    const PHYSICAL_READ_CEILING: u64 =
        (RECORD_PAYLOAD_BYTES + FRAMING_READ_AHEAD_BYTES + CHECKPOINT_ANCHOR_BYTES) as u64;

    let root = TempDir::new().unwrap();
    let relative = Path::new("stream.jsonl");
    let path = root.path().join(relative);
    let mut bytes = vec![b'a'; RECORD_PAYLOAD_BYTES];
    bytes.push(b'\n');
    // The next record has no delimiter and exceeds both the logical
    // record bound and the remaining physical reservation. Once the
    // accepted first record exactly fills the batch, reading it would be
    // unnecessary and would turn a bounded prefix into a false failure.
    bytes.extend(std::iter::repeat_n(b'b', FRAMING_READ_AHEAD_BYTES * 2));
    std::fs::write(&path, bytes).unwrap();

    let driver = AppendDelimitedFile::new(AppendDelimitedConfig {
        delimiter: b'\n',
        normalize_crlf: true,
        max_record_bytes: RECORD_PAYLOAD_BYTES,
        max_batch_bytes: RECORD_PAYLOAD_BYTES,
        max_records_per_batch: 128,
        prefix_anchor_bytes: CHECKPOINT_ANCHOR_BYTES,
    })
    .unwrap();
    let read = driver
        .read_confined_bounded(
            root.path(),
            relative,
            None,
            &origin(),
            false,
            PHYSICAL_READ_CEILING,
        )
        .unwrap();
    let AppendRead::Batch {
        items,
        checkpoint,
        needs_retry,
        more_available,
        bytes_read,
        ..
    } = read
    else {
        panic!("expected bounded append batch");
    };
    assert_eq!(items.len(), 1);
    let AppendItem::Record(record) = &items[0] else {
        panic!("expected accepted first record");
    };
    assert_eq!(record.payload.len(), RECORD_PAYLOAD_BYTES);
    assert_eq!(checkpoint.committed_offset, RECORD_PAYLOAD_BYTES as u64 + 1);
    assert!(needs_retry && more_available);
    // 65,536 payload bytes plus its LF require the second 64-KiB
    // framing read. The driver may read that complete OS chunk, then
    // re-reads the 4-KiB continuity anchor: a conservative 132-KiB
    // candidate-oracle ceiling, not a global performance bound.
    assert_eq!(bytes_read, PHYSICAL_READ_CEILING);
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
