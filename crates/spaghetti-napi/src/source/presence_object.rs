use std::path::Path;

use super::file::{
    read_stable_file, read_stable_file_confined, stamp_revision, FileStamp, StableRead,
};
use super::model::CursorReader;
use super::{FileIdentity, RecordOrigin, Revision, SourceCursor, SourceDriverError, SourceRecord};

const CHECKPOINT_MAGIC: &[u8] = b"SPPO";
const ABSENT_REVISION_BYTES: &[u8] = b"spaghetti:presence:absent:v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresenceObjectConfig {
    pub include_content: bool,
    pub max_content_bytes: usize,
}

impl Default for PresenceObjectConfig {
    fn default() -> Self {
        Self {
            include_content: false,
            max_content_bytes: 64 * 1024,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresenceCheckpoint {
    pub generation: u64,
    pub present: bool,
    pub identity: Option<FileIdentity>,
    pub revision: Revision,
}

impl PresenceCheckpoint {
    pub fn cursor(&self) -> SourceCursor {
        SourceCursor::presence(self.revision)
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(96);
        bytes.extend_from_slice(CHECKPOINT_MAGIC);
        bytes.push(1);
        bytes.extend_from_slice(&self.generation.to_be_bytes());
        bytes.push(u8::from(self.present));
        if let Some(identity) = &self.identity {
            identity.encode_into(&mut bytes);
        }
        bytes.extend_from_slice(self.revision.as_bytes());
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, SourceDriverError> {
        let mut reader = CursorReader::new(bytes, CHECKPOINT_MAGIC)?;
        let generation = reader.u64()?;
        let present = reader.bool()?;
        let identity = if present {
            Some(FileIdentity::decode_from(&mut reader)?)
        } else {
            None
        };
        let revision = reader.revision()?;
        reader.finish()?;
        let checkpoint = Self {
            generation,
            present,
            identity,
            revision,
        };
        checkpoint.validate()?;
        Ok(checkpoint)
    }

    fn validate(&self) -> Result<(), SourceDriverError> {
        if self.generation == 0 {
            return Err(SourceDriverError::InvalidCursor(
                "presence generation must be greater than zero".to_string(),
            ));
        }
        if self.present != self.identity.is_some() {
            return Err(SourceDriverError::InvalidCursor(
                "presence checkpoint identity does not match state".to_string(),
            ));
        }
        if !self.present && self.revision != absent_revision() {
            return Err(SourceDriverError::InvalidCursor(
                "absence checkpoint has an invalid revision".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresenceKind {
    InitialAbsent,
    Created,
    Updated,
    Removed,
    Recreated,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PresenceRead {
    RetryTransient,
    Unchanged {
        checkpoint: PresenceCheckpoint,
    },
    Observation {
        kind: PresenceKind,
        record: SourceRecord,
        checkpoint: PresenceCheckpoint,
        content_omitted: bool,
    },
}

pub struct PresenceObject {
    config: PresenceObjectConfig,
}

impl PresenceObject {
    pub fn new(config: PresenceObjectConfig) -> Result<Self, SourceDriverError> {
        if config.max_content_bytes == 0 {
            return Err(SourceDriverError::InvalidConfig(
                "presence content byte limit must be greater than zero".to_string(),
            ));
        }
        Ok(Self { config })
    }

    pub fn read(
        &self,
        path: &Path,
        previous: Option<&PresenceCheckpoint>,
        origin: &RecordOrigin,
    ) -> Result<PresenceRead, SourceDriverError> {
        self.interpret_read(
            read_stable_file(path, self.config.max_content_bytes)?,
            previous,
            origin,
        )
    }

    pub fn read_confined(
        &self,
        root: &Path,
        relative_path: &Path,
        previous: Option<&PresenceCheckpoint>,
        origin: &RecordOrigin,
    ) -> Result<PresenceRead, SourceDriverError> {
        self.interpret_read(
            read_stable_file_confined(root, relative_path, self.config.max_content_bytes)?,
            previous,
            origin,
        )
    }

    fn interpret_read(
        &self,
        read: StableRead,
        previous: Option<&PresenceCheckpoint>,
        origin: &RecordOrigin,
    ) -> Result<PresenceRead, SourceDriverError> {
        match read {
            StableRead::Unstable => Ok(PresenceRead::RetryTransient),
            StableRead::Missing => self.observe_absence(previous, origin),
            StableRead::Oversized(stamp) => {
                let revision = stamp_revision(&stamp);
                self.observe_presence(previous, origin, stamp, revision, Vec::new(), true)
            }
            StableRead::Stable {
                stamp,
                bytes,
                revision,
            } => {
                let payload = if self.config.include_content {
                    bytes
                } else {
                    Vec::new()
                };
                self.observe_presence(previous, origin, stamp, revision, payload, false)
            }
        }
    }

    fn observe_absence(
        &self,
        previous: Option<&PresenceCheckpoint>,
        origin: &RecordOrigin,
    ) -> Result<PresenceRead, SourceDriverError> {
        let revision = absent_revision();
        if let Some(previous) = previous {
            previous.validate()?;
            if !previous.present {
                return Ok(PresenceRead::Unchanged {
                    checkpoint: previous.clone(),
                });
            }
        }
        let generation = match previous {
            Some(checkpoint) => next_generation(checkpoint.generation)?,
            None => 1,
        };
        let kind = if previous.is_some() {
            PresenceKind::Removed
        } else {
            PresenceKind::InitialAbsent
        };
        let checkpoint = PresenceCheckpoint {
            generation,
            present: false,
            identity: None,
            revision,
        };
        let cursor_start =
            previous.map_or(SourceCursor::presence(Revision::ZERO), |old| old.cursor());
        Ok(PresenceRead::Observation {
            kind,
            record: SourceRecord::absent(origin, generation, cursor_start, checkpoint.cursor(), 0),
            checkpoint,
            content_omitted: true,
        })
    }

    fn observe_presence(
        &self,
        previous: Option<&PresenceCheckpoint>,
        origin: &RecordOrigin,
        stamp: FileStamp,
        content_revision: Revision,
        payload: Vec<u8>,
        oversized: bool,
    ) -> Result<PresenceRead, SourceDriverError> {
        if let Some(previous) = previous {
            previous.validate()?;
        }
        let revision = presence_revision(&stamp, content_revision);
        let (generation, kind) = match previous {
            None => (1, PresenceKind::Created),
            Some(previous) if !previous.present => {
                (next_generation(previous.generation)?, PresenceKind::Created)
            }
            Some(previous) if previous.identity.as_ref() != Some(&stamp.identity) => (
                next_generation(previous.generation)?,
                PresenceKind::Recreated,
            ),
            Some(previous) if previous.revision != revision => {
                (previous.generation, PresenceKind::Updated)
            }
            Some(previous) => {
                return Ok(PresenceRead::Unchanged {
                    checkpoint: previous.clone(),
                });
            }
        };
        let checkpoint = PresenceCheckpoint {
            generation,
            present: true,
            identity: Some(stamp.identity),
            revision,
        };
        let cursor_start =
            previous.map_or(SourceCursor::presence(Revision::ZERO), |old| old.cursor());
        Ok(PresenceRead::Observation {
            kind,
            record: SourceRecord::new(
                origin,
                generation,
                cursor_start,
                checkpoint.cursor(),
                0,
                payload,
            ),
            checkpoint,
            content_omitted: oversized || !self.config.include_content,
        })
    }
}

fn absent_revision() -> Revision {
    Revision::digest(ABSENT_REVISION_BYTES)
}

fn presence_revision(stamp: &FileStamp, content_revision: Revision) -> Revision {
    let mut hasher = blake3::Hasher::new();
    hasher.update(stamp_revision(stamp).as_bytes());
    hasher.update(content_revision.as_bytes());
    Revision::from_bytes(*hasher.finalize().as_bytes())
}

fn next_generation(current: u64) -> Result<u64, SourceDriverError> {
    current
        .checked_add(1)
        .ok_or_else(|| SourceDriverError::InvalidCursor("source generation overflowed".to_string()))
}

#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::Duration;

    use tempfile::TempDir;

    use super::*;
    use crate::source::{SourceMediaType, SourceRecordState};

    fn origin() -> RecordOrigin {
        RecordOrigin {
            source_instance_id: 1,
            stream_id: 2,
            object_id: 3,
            observed_at: 4,
            source_timestamp_hint: None,
            media_type: SourceMediaType::new("application/octet-stream").unwrap(),
        }
    }

    fn driver(include_content: bool) -> PresenceObject {
        PresenceObject::new(PresenceObjectConfig {
            include_content,
            max_content_bytes: 64,
        })
        .unwrap()
    }

    fn observation(read: PresenceRead) -> (PresenceKind, SourceRecord, PresenceCheckpoint) {
        match read {
            PresenceRead::Observation {
                kind,
                record,
                checkpoint,
                ..
            } => (kind, record, checkpoint),
            other => panic!("expected presence observation, got {other:?}"),
        }
    }

    #[test]
    fn initial_absence_is_an_observation_then_becomes_unchanged() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("active.lock");
        let (kind, record, checkpoint) =
            observation(driver(false).read(&path, None, &origin()).unwrap());
        assert_eq!(kind, PresenceKind::InitialAbsent);
        assert_eq!(record.state, SourceRecordState::Absent);
        assert!(!checkpoint.present);
        assert!(matches!(
            driver(false)
                .read(&path, Some(&checkpoint), &origin())
                .unwrap(),
            PresenceRead::Unchanged { .. }
        ));
    }

    #[test]
    fn create_remove_and_recreate_have_distinct_generation_semantics() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("active.lock");
        let (_, _, absent) = observation(driver(true).read(&path, None, &origin()).unwrap());
        std::fs::write(&path, b"first").unwrap();
        let (kind, record, present) =
            observation(driver(true).read(&path, Some(&absent), &origin()).unwrap());
        assert_eq!(kind, PresenceKind::Created);
        assert_eq!(record.state, SourceRecordState::Present);
        assert_eq!(record.payload, b"first");
        assert_eq!(present.generation, absent.generation + 1);

        std::fs::remove_file(&path).unwrap();
        let (kind, record, removed) =
            observation(driver(true).read(&path, Some(&present), &origin()).unwrap());
        assert_eq!(kind, PresenceKind::Removed);
        assert_eq!(record.state, SourceRecordState::Absent);
        assert_eq!(removed.generation, present.generation + 1);

        std::fs::write(&path, b"second").unwrap();
        let (kind, _, recreated) =
            observation(driver(true).read(&path, Some(&removed), &origin()).unwrap());
        assert_eq!(kind, PresenceKind::Created);
        assert_eq!(recreated.generation, removed.generation + 1);
    }

    #[test]
    fn in_place_update_stays_in_generation() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("active.lock");
        std::fs::write(&path, b"one").unwrap();
        let (_, _, first) = observation(driver(true).read(&path, None, &origin()).unwrap());
        // Some filesystems have coarse timestamp resolution; changing length
        // also makes the metadata revision deterministic.
        thread::sleep(Duration::from_millis(2));
        std::fs::write(&path, b"two-longer").unwrap();
        let (kind, _, next) =
            observation(driver(true).read(&path, Some(&first), &origin()).unwrap());
        assert_eq!(kind, PresenceKind::Updated);
        assert_eq!(next.generation, first.generation);
    }

    #[test]
    fn atomic_identity_replacement_is_recreation_even_with_same_content() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("active.lock");
        std::fs::write(&path, b"same").unwrap();
        let (_, _, first) = observation(driver(true).read(&path, None, &origin()).unwrap());
        let replacement = directory.path().join("replacement");
        std::fs::write(&replacement, b"same").unwrap();
        std::fs::rename(replacement, &path).unwrap();

        let (kind, _, next) =
            observation(driver(true).read(&path, Some(&first), &origin()).unwrap());
        assert_eq!(kind, PresenceKind::Recreated);
        assert_eq!(next.generation, first.generation + 1);
    }

    #[test]
    fn checkpoint_encoding_round_trips() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("active.lock");
        std::fs::write(&path, b"present").unwrap();
        let (_, _, checkpoint) = observation(driver(false).read(&path, None, &origin()).unwrap());
        assert_eq!(
            PresenceCheckpoint::decode(&checkpoint.encode()).unwrap(),
            checkpoint
        );
    }
}
