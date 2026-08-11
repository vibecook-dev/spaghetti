use std::path::Path;

use super::file::{read_stable_file, stamp_revision, StableRead};
use super::model::CursorReader;
use super::{
    DriverQuarantine, FileIdentity, RecordHash, RecordOrigin, Revision, SourceCursor,
    SourceDriverError, SourceRecord,
};

const CHECKPOINT_MAGIC: &[u8] = b"SPRD";

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
    pub identity: FileIdentity,
    pub revision: Revision,
}

impl ReplaceCheckpoint {
    pub fn cursor(&self) -> SourceCursor {
        SourceCursor::snapshot(self.revision)
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(96);
        bytes.extend_from_slice(CHECKPOINT_MAGIC);
        bytes.push(1);
        bytes.extend_from_slice(&self.generation.to_be_bytes());
        self.identity.encode_into(&mut bytes);
        bytes.extend_from_slice(self.revision.as_bytes());
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, SourceDriverError> {
        let mut reader = CursorReader::new(bytes, CHECKPOINT_MAGIC)?;
        let checkpoint = Self {
            generation: reader.u64()?,
            identity: FileIdentity::decode_from(&mut reader)?,
            revision: reader.revision()?,
        };
        reader.finish()?;
        if checkpoint.generation == 0 {
            return Err(SourceDriverError::InvalidCursor(
                "replace-document generation must be greater than zero".to_string(),
            ));
        }
        Ok(checkpoint)
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
        match read_stable_file(path, self.config.max_document_bytes)? {
            StableRead::Missing => Ok(ReplaceRead::Missing),
            StableRead::Unstable => Ok(ReplaceRead::RetryTransient),
            StableRead::Oversized(stamp) => {
                let revision = stamp_revision(&stamp);
                let (generation, generation_changed) =
                    generation(previous, incompatible_replacement)?;
                let checkpoint = ReplaceCheckpoint {
                    generation,
                    identity: stamp.identity,
                    revision,
                };
                if !generation_changed && previous.is_some_and(|old| old.revision == revision) {
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
                    identity: stamp.identity,
                    revision,
                };
                if !generation_changed && previous.is_some_and(|old| old.revision == revision) {
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
}

fn generation(
    previous: Option<&ReplaceCheckpoint>,
    incompatible_replacement: bool,
) -> Result<(u64, bool), SourceDriverError> {
    let Some(previous) = previous else {
        return Ok((1, true));
    };
    if incompatible_replacement {
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
}
