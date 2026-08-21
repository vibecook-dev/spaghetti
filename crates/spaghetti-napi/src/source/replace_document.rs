use std::path::Path;

use super::file::{
    read_stable_file, read_stable_file_confined, stamp_revision, FileStamp, StableRead,
};
use super::model::CursorReader;
use super::{
    DriverQuarantine, FileIdentity, RecordHash, RecordOrigin, Revision, SourceCursor,
    SourceDriverError, SourceRecord,
};

const CHECKPOINT_MAGIC: &[u8] = b"SPRD";
const MALFORMED_GUARD_MAGIC: &[u8] = b"SPMG";
const CHECKPOINT_VERSION_PRESENT_ONLY: u8 = 1;
const CHECKPOINT_VERSION_PRESENCE_AWARE: u8 = 2;
const ABSENT_REVISION_BYTES: &[u8] = b"spaghetti:replace-document:absent:v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplaceDocumentConfig {
    pub max_document_bytes: usize,
}

impl Default for ReplaceDocumentConfig {
    fn default() -> Self {
        Self {
            max_document_bytes: 4 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplaceCheckpoint {
    pub generation: u64,
    pub present: bool,
    pub identity: Option<FileIdentity>,
    pub revision: Revision,
}

impl ReplaceCheckpoint {
    pub fn cursor(&self) -> SourceCursor {
        SourceCursor::snapshot(self.revision)
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(96);
        bytes.extend_from_slice(CHECKPOINT_MAGIC);
        bytes.push(CHECKPOINT_VERSION_PRESENCE_AWARE);
        bytes.extend_from_slice(&self.generation.to_be_bytes());
        bytes.push(u8::from(self.present));
        if let Some(identity) = &self.identity {
            identity.encode_into(&mut bytes);
        }
        bytes.extend_from_slice(self.revision.as_bytes());
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, SourceDriverError> {
        let Some(bytes) = bytes.strip_prefix(CHECKPOINT_MAGIC) else {
            return Err(SourceDriverError::InvalidCursor(
                "checkpoint magic does not match driver".to_string(),
            ));
        };
        let Some((&version, payload)) = bytes.split_first() else {
            return Err(SourceDriverError::InvalidCursor(
                "checkpoint is truncated".to_string(),
            ));
        };
        let mut reader = CursorReader::from_payload(payload);
        if version == CHECKPOINT_VERSION_PRESENT_ONLY {
            let checkpoint = Self {
                generation: reader.u64()?,
                present: true,
                identity: Some(FileIdentity::decode_from(&mut reader)?),
                revision: reader.revision()?,
            };
            reader.finish()?;
            checkpoint.validate()?;
            return Ok(checkpoint);
        }
        if version != CHECKPOINT_VERSION_PRESENCE_AWARE {
            return Err(SourceDriverError::InvalidCursor(
                "unsupported checkpoint version".to_string(),
            ));
        }
        let generation = reader.u64()?;
        let present = reader.bool()?;
        let identity = if present {
            Some(FileIdentity::decode_from(&mut reader)?)
        } else {
            None
        };
        let checkpoint = Self {
            generation,
            present,
            identity,
            revision: reader.revision()?,
        };
        reader.finish()?;
        checkpoint.validate()?;
        Ok(checkpoint)
    }

    fn validate(&self) -> Result<(), SourceDriverError> {
        if self.generation == 0 {
            return Err(SourceDriverError::InvalidCursor(
                "replace-document generation must be greater than zero".to_string(),
            ));
        }
        if self.present != self.identity.is_some() {
            return Err(SourceDriverError::InvalidCursor(
                "replace-document checkpoint identity does not match state".to_string(),
            ));
        }
        if !self.present && self.revision != absent_revision() {
            return Err(SourceDriverError::InvalidCursor(
                "replace-document absence checkpoint has an invalid revision".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplaceRead {
    Missing,
    RetryTransient,
    Unchanged {
        checkpoint: ReplaceCheckpoint,
    },
    Record {
        record: SourceRecord,
        checkpoint: ReplaceCheckpoint,
        generation_changed: bool,
    },
    Removed {
        record: SourceRecord,
        checkpoint: ReplaceCheckpoint,
    },
    Quarantined {
        quarantine: DriverQuarantine,
        checkpoint: ReplaceCheckpoint,
        generation_changed: bool,
    },
}

pub struct ReplaceDocument {
    config: ReplaceDocumentConfig,
}

impl ReplaceDocument {
    pub fn new(config: ReplaceDocumentConfig) -> Result<Self, SourceDriverError> {
        if config.max_document_bytes == 0 {
            return Err(SourceDriverError::InvalidConfig(
                "replace-document byte limit must be greater than zero".to_string(),
            ));
        }
        Ok(Self { config })
    }

    /// Reads one whole stable revision. Native identity replacement alone is
    /// semantically neutral: identical content remains unchanged. The caller
    /// sets `incompatible_replacement` only when its declared contract needs a
    /// new generation and a replay from the snapshot boundary.
    pub fn read(
        &self,
        path: &Path,
        previous: Option<&ReplaceCheckpoint>,
        origin: &RecordOrigin,
        incompatible_replacement: bool,
    ) -> Result<ReplaceRead, SourceDriverError> {
        self.interpret_read(
            read_stable_file(path, self.config.max_document_bytes)?,
            previous,
            origin,
            incompatible_replacement,
        )
    }

    pub fn read_confined(
        &self,
        root: &Path,
        relative_path: &Path,
        previous: Option<&ReplaceCheckpoint>,
        origin: &RecordOrigin,
        incompatible_replacement: bool,
    ) -> Result<ReplaceRead, SourceDriverError> {
        self.interpret_read(
            read_stable_file_confined(root, relative_path, self.config.max_document_bytes)?,
            previous,
            origin,
            incompatible_replacement,
        )
    }

    /// Frame content already obtained through a confined stable read. The
    /// caller keeps ownership until every retained byte/stamp/revision bound
    /// has passed, so another topology can fail without losing retry input.
    pub(crate) fn frame_retained_stable(
        &self,
        stamp: &FileStamp,
        bytes: &[u8],
        revision: Revision,
        previous: Option<&ReplaceCheckpoint>,
        origin: &RecordOrigin,
        incompatible_replacement: bool,
    ) -> Result<ReplaceRead, SourceDriverError> {
        if bytes.len() > self.config.max_document_bytes
            || u64::try_from(bytes.len()).ok() != Some(stamp.len)
            || Revision::digest(bytes) != revision
        {
            return Err(SourceDriverError::InvalidCursor(
                "retained stable document does not match its bounded read evidence".to_string(),
            ));
        }
        self.interpret_read(
            StableRead::Stable {
                stamp: stamp.clone(),
                bytes: bytes.to_vec(),
                revision,
            },
            previous,
            origin,
            incompatible_replacement,
        )
    }

    fn interpret_read(
        &self,
        read: StableRead,
        previous: Option<&ReplaceCheckpoint>,
        origin: &RecordOrigin,
        incompatible_replacement: bool,
    ) -> Result<ReplaceRead, SourceDriverError> {
        match read {
            StableRead::Missing => self.observe_absence(previous, origin),
            StableRead::Unstable => Ok(ReplaceRead::RetryTransient),
            StableRead::Oversized(stamp) => {
                let revision = stamp_revision(&stamp);
                let (generation, generation_changed) =
                    generation(previous, incompatible_replacement)?;
                let checkpoint = ReplaceCheckpoint {
                    generation,
                    present: true,
                    identity: Some(stamp.identity),
                    revision,
                };
                if !generation_changed
                    && previous.is_some_and(|old| old.present && old.revision == revision)
                {
                    return Ok(ReplaceRead::Unchanged { checkpoint });
                }
                let start = previous.map_or(Revision::ZERO, |old| old.revision);
                Ok(ReplaceRead::Quarantined {
                    quarantine: DriverQuarantine {
                        generation,
                        cursor_start: SourceCursor::snapshot(start),
                        cursor_end: SourceCursor::snapshot(revision),
                        ordinal_in_batch: 0,
                        payload_len: stamp.len,
                        payload_hash: RecordHash::from_bytes(*revision.as_bytes()),
                        reason: format!(
                            "stable document exceeds {} byte limit; hash represents bounded metadata revision",
                            self.config.max_document_bytes
                        ),
                    },
                    checkpoint,
                    generation_changed,
                })
            }
            StableRead::Stable {
                stamp,
                bytes,
                revision,
            } => {
                let (generation, generation_changed) =
                    generation(previous, incompatible_replacement)?;
                let checkpoint = ReplaceCheckpoint {
                    generation,
                    present: true,
                    identity: Some(stamp.identity),
                    revision,
                };
                if !generation_changed
                    && previous.is_some_and(|old| old.present && old.revision == revision)
                {
                    return Ok(ReplaceRead::Unchanged { checkpoint });
                }
                let start = previous.map_or(Revision::ZERO, |old| old.revision);
                Ok(ReplaceRead::Record {
                    record: SourceRecord::new(
                        origin,
                        generation,
                        SourceCursor::snapshot(start),
                        SourceCursor::snapshot(revision),
                        0,
                        bytes,
                    ),
                    checkpoint,
                    generation_changed,
                })
            }
        }
    }

    fn observe_absence(
        &self,
        previous: Option<&ReplaceCheckpoint>,
        origin: &RecordOrigin,
    ) -> Result<ReplaceRead, SourceDriverError> {
        let Some(previous) = previous else {
            return Ok(ReplaceRead::Missing);
        };
        previous.validate()?;
        if !previous.present {
            return Ok(ReplaceRead::Unchanged {
                checkpoint: previous.clone(),
            });
        }
        let checkpoint = ReplaceCheckpoint {
            generation: next_generation(previous.generation)?,
            present: false,
            identity: None,
            revision: absent_revision(),
        };
        Ok(ReplaceRead::Removed {
            record: SourceRecord::absent(
                origin,
                checkpoint.generation,
                previous.cursor(),
                checkpoint.cursor(),
                0,
            ),
            checkpoint,
        })
    }
}

fn generation(
    previous: Option<&ReplaceCheckpoint>,
    incompatible_replacement: bool,
) -> Result<(u64, bool), SourceDriverError> {
    let Some(previous) = previous else {
        return Ok((1, true));
    };
    previous.validate()?;
    if !previous.present || incompatible_replacement {
        Ok((
            previous.generation.checked_add(1).ok_or_else(|| {
                SourceDriverError::InvalidCursor("source generation overflowed".to_string())
            })?,
            true,
        ))
    } else {
        Ok((previous.generation, false))
    }
}

fn absent_revision() -> Revision {
    Revision::digest(ABSENT_REVISION_BYTES)
}

fn next_generation(current: u64) -> Result<u64, SourceDriverError> {
    current
        .checked_add(1)
        .ok_or_else(|| SourceDriverError::InvalidCursor("source generation overflowed".to_string()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MalformedRevisionPolicy {
    pub minimum_attempts: u32,
    pub settle_delay_ms: u64,
}

impl Default for MalformedRevisionPolicy {
    fn default() -> Self {
        Self {
            minimum_attempts: 2,
            settle_delay_ms: 100,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseFailureDecision {
    RetryTransient { attempt: u32 },
    Quarantine { attempts: u32 },
}

/// Bounded per-object guard used after an adapter rejects a complete snapshot.
/// A new revision resets the guard; repeated failure of one stable revision is
/// eventually classified as quarantine instead of retried forever.
pub struct MalformedRevisionGuard {
    policy: MalformedRevisionPolicy,
    current: Option<FailureState>,
}

struct FailureState {
    revision: Revision,
    first_seen_at_ms: i64,
    attempts: u32,
    quarantined: bool,
}

impl MalformedRevisionGuard {
    pub fn new(policy: MalformedRevisionPolicy) -> Result<Self, SourceDriverError> {
        if policy.minimum_attempts < 2 {
            return Err(SourceDriverError::InvalidConfig(
                "malformed revision policy requires at least two attempts".to_string(),
            ));
        }
        Ok(Self {
            policy,
            current: None,
        })
    }

    pub fn from_checkpoint(
        policy: MalformedRevisionPolicy,
        checkpoint: Option<&[u8]>,
    ) -> Result<Self, SourceDriverError> {
        let mut guard = Self::new(policy)?;
        let Some(bytes) = checkpoint else {
            return Ok(guard);
        };
        const CHECKPOINT_BYTES: usize = 4 + 1 + 32 + 8 + 4 + 1;
        if bytes.len() != CHECKPOINT_BYTES || &bytes[..4] != MALFORMED_GUARD_MAGIC || bytes[4] != 1
        {
            return Err(SourceDriverError::InvalidCursor(
                "malformed-revision guard checkpoint is invalid".to_string(),
            ));
        }
        let revision = Revision::from_bytes(bytes[5..37].try_into().expect("fixed revision slice"));
        let first_seen_at_ms =
            i64::from_be_bytes(bytes[37..45].try_into().expect("fixed first-seen slice"));
        let attempts = u32::from_be_bytes(bytes[45..49].try_into().expect("fixed attempts slice"));
        if attempts == 0 || bytes[49] > 1 {
            return Err(SourceDriverError::InvalidCursor(
                "malformed-revision guard checkpoint state is invalid".to_string(),
            ));
        }
        guard.current = Some(FailureState {
            revision,
            first_seen_at_ms,
            attempts,
            quarantined: bytes[49] == 1,
        });
        Ok(guard)
    }

    pub fn checkpoint(&self) -> Option<Vec<u8>> {
        let state = self.current.as_ref()?;
        let mut bytes = Vec::with_capacity(50);
        bytes.extend_from_slice(MALFORMED_GUARD_MAGIC);
        bytes.push(1);
        bytes.extend_from_slice(state.revision.as_bytes());
        bytes.extend_from_slice(&state.first_seen_at_ms.to_be_bytes());
        bytes.extend_from_slice(&state.attempts.to_be_bytes());
        bytes.push(u8::from(state.quarantined));
        Some(bytes)
    }

    pub fn classify_failure(&mut self, revision: Revision, now_ms: i64) -> ParseFailureDecision {
        let state = self.current.get_or_insert(FailureState {
            revision,
            first_seen_at_ms: now_ms,
            attempts: 0,
            quarantined: false,
        });
        if state.revision != revision {
            *state = FailureState {
                revision,
                first_seen_at_ms: now_ms,
                attempts: 0,
                quarantined: false,
            };
        }
        state.attempts = state.attempts.saturating_add(1);
        let elapsed = now_ms.saturating_sub(state.first_seen_at_ms).max(0) as u64;
        if !state.quarantined
            && (state.attempts < self.policy.minimum_attempts
                || elapsed < self.policy.settle_delay_ms)
        {
            ParseFailureDecision::RetryTransient {
                attempt: state.attempts,
            }
        } else {
            state.quarantined = true;
            ParseFailureDecision::Quarantine {
                attempts: state.attempts,
            }
        }
    }

    pub fn record_success(&mut self, revision: Revision) {
        if self
            .current
            .as_ref()
            .is_some_and(|state| state.revision == revision)
        {
            self.current = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use tempfile::{NamedTempFile, TempDir};

    use super::*;
    use crate::source::SourceMediaType;

    fn origin() -> RecordOrigin {
        RecordOrigin {
            source_instance_id: 1,
            stream_id: 2,
            object_id: 3,
            observed_at: 4,
            source_timestamp_hint: None,
            media_type: SourceMediaType::new("application/json").unwrap(),
        }
    }

    fn driver(max_document_bytes: usize) -> ReplaceDocument {
        ReplaceDocument::new(ReplaceDocumentConfig { max_document_bytes }).unwrap()
    }

    #[test]
    fn retained_stable_content_is_revalidated_before_driver_framing() {
        let retained_driver = driver(8);
        let bytes = b"document";
        let revision = Revision::digest(bytes);
        let stamp = FileStamp {
            identity: FileIdentity::ConfinedPath(b"document.json".to_vec()),
            len: bytes.len() as u64,
            modified_ns: 7,
        };
        let ReplaceRead::Record {
            record,
            checkpoint,
            generation_changed,
        } = retained_driver
            .frame_retained_stable(&stamp, bytes, revision, None, &origin(), false)
            .unwrap()
        else {
            panic!("an initial stable document must frame as one record");
        };
        assert!(generation_changed);
        assert_eq!(checkpoint.generation, 1);
        assert_eq!(checkpoint.revision, revision);
        assert_eq!(record.payload, bytes);

        let wrong_revision = Revision::digest(b"tampered");
        assert!(matches!(
            retained_driver.frame_retained_stable(
                &stamp,
                bytes,
                wrong_revision,
                None,
                &origin(),
                false,
            ),
            Err(SourceDriverError::InvalidCursor(_))
        ));
        let wrong_length = FileStamp {
            len: stamp.len + 1,
            ..stamp.clone()
        };
        assert!(matches!(
            retained_driver.frame_retained_stable(
                &wrong_length,
                bytes,
                revision,
                None,
                &origin(),
                false,
            ),
            Err(SourceDriverError::InvalidCursor(_))
        ));
        assert!(matches!(
            driver(7).frame_retained_stable(&stamp, bytes, revision, None, &origin(), false,),
            Err(SourceDriverError::InvalidCursor(_))
        ));
    }

    #[test]
    fn stable_whole_document_is_one_record_and_warm_read_is_unchanged() {
        let file = NamedTempFile::new().unwrap();
        std::fs::write(file.path(), b"{\"ok\":true}").unwrap();
        let first = driver(64)
            .read(file.path(), None, &origin(), false)
            .unwrap();
        let ReplaceRead::Record {
            record,
            checkpoint,
            generation_changed,
        } = first
        else {
            panic!("expected record");
        };
        assert_eq!(record.payload, b"{\"ok\":true}");
        assert!(generation_changed);
        assert!(matches!(
            driver(64)
                .read(file.path(), Some(&checkpoint), &origin(), false)
                .unwrap(),
            ReplaceRead::Unchanged { .. }
        ));
    }

    #[test]
    fn atomic_rename_with_identical_content_is_semantically_unchanged() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("summary.json");
        std::fs::write(&path, b"same").unwrap();
        let ReplaceRead::Record { checkpoint, .. } =
            driver(64).read(&path, None, &origin(), false).unwrap()
        else {
            panic!("expected initial record");
        };
        let replacement = directory.path().join("replacement");
        std::fs::write(&replacement, b"same").unwrap();
        std::fs::rename(replacement, &path).unwrap();

        let ReplaceRead::Unchanged { checkpoint: next } = driver(64)
            .read(&path, Some(&checkpoint), &origin(), false)
            .unwrap()
        else {
            panic!("identical replacement must be unchanged");
        };
        assert_eq!(next.generation, checkpoint.generation);
        assert_ne!(next.identity, checkpoint.identity);
    }

    #[test]
    fn incompatible_snapshot_replacement_increments_generation() {
        let file = NamedTempFile::new().unwrap();
        std::fs::write(file.path(), b"one").unwrap();
        let ReplaceRead::Record { checkpoint, .. } = driver(64)
            .read(file.path(), None, &origin(), false)
            .unwrap()
        else {
            panic!("expected initial record");
        };
        std::fs::write(file.path(), b"two").unwrap();
        let ReplaceRead::Record {
            checkpoint: next,
            generation_changed,
            ..
        } = driver(64)
            .read(file.path(), Some(&checkpoint), &origin(), true)
            .unwrap()
        else {
            panic!("expected replacement record");
        };
        assert!(generation_changed);
        assert_eq!(next.generation, checkpoint.generation + 1);
    }

    #[test]
    fn oversized_stable_document_is_quarantined_without_retaining_payload() {
        let file = NamedTempFile::new().unwrap();
        std::fs::write(file.path(), b"too-large").unwrap();
        let ReplaceRead::Quarantined { quarantine, .. } =
            driver(4).read(file.path(), None, &origin(), false).unwrap()
        else {
            panic!("expected quarantine");
        };
        assert_eq!(quarantine.payload_len, 9);
    }

    #[test]
    fn malformed_revision_retries_then_quarantines_only_if_revision_stays_stable() {
        let mut guard = MalformedRevisionGuard::new(MalformedRevisionPolicy {
            minimum_attempts: 2,
            settle_delay_ms: 100,
        })
        .unwrap();
        let first = Revision::digest(b"malformed-one");
        assert_eq!(
            guard.classify_failure(first, 1_000),
            ParseFailureDecision::RetryTransient { attempt: 1 }
        );
        let checkpoint = guard.checkpoint().unwrap();
        let mut guard = MalformedRevisionGuard::from_checkpoint(
            MalformedRevisionPolicy {
                minimum_attempts: 2,
                settle_delay_ms: 100,
            },
            Some(&checkpoint),
        )
        .unwrap();
        assert_eq!(
            guard.classify_failure(first, 1_050),
            ParseFailureDecision::RetryTransient { attempt: 2 }
        );
        assert_eq!(
            guard.classify_failure(first, 1_100),
            ParseFailureDecision::Quarantine { attempts: 3 }
        );

        let changed = Revision::digest(b"malformed-two");
        assert_eq!(
            guard.classify_failure(changed, 1_101),
            ParseFailureDecision::RetryTransient { attempt: 1 }
        );
    }

    #[test]
    fn checkpoint_encoding_round_trips() {
        let file = NamedTempFile::new().unwrap();
        std::fs::write(file.path(), b"document").unwrap();
        let ReplaceRead::Record { checkpoint, .. } = driver(64)
            .read(file.path(), None, &origin(), false)
            .unwrap()
        else {
            panic!("expected record");
        };
        assert_eq!(
            ReplaceCheckpoint::decode(&checkpoint.encode()).unwrap(),
            checkpoint
        );
    }

    #[test]
    fn legacy_present_checkpoint_decodes_for_restart_compatibility() {
        let file = NamedTempFile::new().unwrap();
        std::fs::write(file.path(), b"document").unwrap();
        let ReplaceRead::Record { checkpoint, .. } = driver(64)
            .read(file.path(), None, &origin(), false)
            .unwrap()
        else {
            panic!("expected record");
        };
        let mut legacy = Vec::new();
        legacy.extend_from_slice(CHECKPOINT_MAGIC);
        legacy.push(CHECKPOINT_VERSION_PRESENT_ONLY);
        legacy.extend_from_slice(&checkpoint.generation.to_be_bytes());
        checkpoint
            .identity
            .as_ref()
            .unwrap()
            .encode_into(&mut legacy);
        legacy.extend_from_slice(checkpoint.revision.as_bytes());

        assert_eq!(ReplaceCheckpoint::decode(&legacy).unwrap(), checkpoint);
    }

    #[test]
    fn deletion_is_observed_once_and_recreation_starts_a_new_generation() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("summary.json");
        std::fs::write(&path, b"one").unwrap();
        let ReplaceRead::Record {
            checkpoint: present,
            ..
        } = driver(64).read(&path, None, &origin(), false).unwrap()
        else {
            panic!("expected initial record");
        };

        std::fs::remove_file(&path).unwrap();
        let ReplaceRead::Removed {
            record,
            checkpoint: absent,
        } = driver(64)
            .read(&path, Some(&present), &origin(), false)
            .unwrap()
        else {
            panic!("expected removal observation");
        };
        assert_eq!(record.state, crate::source::SourceRecordState::Absent);
        assert_eq!(absent.generation, present.generation + 1);
        assert!(!absent.present);
        assert_eq!(ReplaceCheckpoint::decode(&absent.encode()).unwrap(), absent);
        assert!(matches!(
            driver(64)
                .read(&path, Some(&absent), &origin(), false)
                .unwrap(),
            ReplaceRead::Unchanged { .. }
        ));

        std::fs::write(&path, b"two").unwrap();
        let ReplaceRead::Record {
            checkpoint: recreated,
            generation_changed,
            ..
        } = driver(64)
            .read(&path, Some(&absent), &origin(), false)
            .unwrap()
        else {
            panic!("expected recreated record");
        };
        assert!(generation_changed);
        assert_eq!(recreated.generation, absent.generation + 1);
        assert!(recreated.present);
    }
}
