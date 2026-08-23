use std::collections::BTreeMap;
use std::path::Path;

use super::file::{confined_relative_path_key, file_stamp, stamp_revision, FileStamp};
use super::model::{io_error, CursorReader};
use super::{FileIdentity, Revision, SourceCursor, SourceDriverError};

const CHECKPOINT_MAGIC: &[u8] = b"SPDS";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectorySnapshotConfig {
    pub max_entries: usize,
    /// Maximum native entries yielded by any one enumerated directory. This
    /// counts entries before selector filtering or full metadata reads, so an
    /// ignored population cannot turn a narrow retained snapshot into an
    /// unbounded native listing.
    pub max_entries_per_directory: usize,
    pub max_depth: usize,
}

impl Default for DirectorySnapshotConfig {
    fn default() -> Self {
        Self {
            max_entries: 100_000,
            max_entries_per_directory: 100_000,
            max_depth: 32,
        }
    }
}

impl DirectorySnapshotConfig {
    fn validate(&self) -> Result<(), SourceDriverError> {
        if self.max_entries == 0 {
            return Err(SourceDriverError::InvalidConfig(
                "directory max_entries must be greater than zero".to_string(),
            ));
        }
        if self.max_entries_per_directory == 0 || self.max_entries_per_directory > self.max_entries
        {
            return Err(SourceDriverError::InvalidConfig(
                "directory max_entries_per_directory must be within max_entries".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectoryEntryKind {
    File,
    Directory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectorySelection {
    Ignore,
    Include,
    Recurse,
    IncludeAndRecurse,
}

impl DirectorySelection {
    fn includes(self) -> bool {
        matches!(self, Self::Include | Self::IncludeAndRecurse)
    }

    fn recurses(self) -> bool {
        matches!(self, Self::Recurse | Self::IncludeAndRecurse)
    }
}

/// Selector invoked after a cheap entry kind lookup and before full metadata.
pub trait DirectorySelector {
    fn select(&self, relative_path: &Path, kind: DirectoryEntryKind) -> DirectorySelection;
}

impl<F> DirectorySelector for F
where
    F: Fn(&Path, DirectoryEntryKind) -> DirectorySelection,
{
    fn select(&self, relative_path: &Path, kind: DirectoryEntryKind) -> DirectorySelection {
        self(relative_path, kind)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryEntryState {
    pub path_key: Vec<u8>,
    pub display_path: String,
    pub kind: DirectoryEntryKind,
    pub identity: FileIdentity,
    pub revision: Revision,
    pub size_bytes: u64,
    pub modified_ns: i128,
    pub generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryCheckpoint {
    pub root_identity: FileIdentity,
    pub generation: u64,
    pub revision: Revision,
    pub entries: BTreeMap<Vec<u8>, DirectoryEntryState>,
}

impl DirectoryCheckpoint {
    pub fn cursor(&self) -> SourceCursor {
        SourceCursor::directory(self.revision)
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(CHECKPOINT_MAGIC);
        bytes.push(1);
        bytes.extend_from_slice(&self.generation.to_be_bytes());
        self.root_identity.encode_into(&mut bytes);
        bytes.extend_from_slice(self.revision.as_bytes());
        bytes.extend_from_slice(&(self.entries.len() as u64).to_be_bytes());
        for entry in self.entries.values() {
            encode_bytes(&mut bytes, &entry.path_key);
            encode_bytes(&mut bytes, entry.display_path.as_bytes());
            bytes.push(match entry.kind {
                DirectoryEntryKind::File => 1,
                DirectoryEntryKind::Directory => 2,
            });
            entry.identity.encode_into(&mut bytes);
            bytes.extend_from_slice(entry.revision.as_bytes());
            bytes.extend_from_slice(&entry.size_bytes.to_be_bytes());
            bytes.extend_from_slice(&entry.modified_ns.to_be_bytes());
            bytes.extend_from_slice(&entry.generation.to_be_bytes());
        }
        bytes
    }

    pub fn decode_for_config(
        bytes: &[u8],
        config: &DirectorySnapshotConfig,
    ) -> Result<Self, SourceDriverError> {
        config.validate()?;
        let mut reader = CursorReader::new(bytes, CHECKPOINT_MAGIC)?;
        let generation = reader.u64()?;
        let root_identity = FileIdentity::decode_from(&mut reader)?;
        let revision = reader.revision()?;
        let entry_count = reader.usize()?;
        if entry_count > config.max_entries {
            return Err(SourceDriverError::InvalidCursor(format!(
                "directory checkpoint contains {entry_count} entries, exceeding configured limit {}",
                config.max_entries
            )));
        }
        let mut entries = BTreeMap::new();
        let mut entries_per_directory = BTreeMap::<Vec<u8>, usize>::new();
        for _ in 0..entry_count {
            let path_key = decode_bytes(&mut reader)?;
            let parent_end = validate_checkpoint_path_key(&path_key, config.max_depth)?;
            let parent_count = entries_per_directory
                .entry(path_key[..parent_end].to_vec())
                .or_default();
            if *parent_count == config.max_entries_per_directory {
                return Err(SourceDriverError::InvalidCursor(format!(
                    "directory checkpoint exceeds configured per-directory limit {}",
                    config.max_entries_per_directory
                )));
            }
            *parent_count += 1;
            let display_path = String::from_utf8(decode_bytes(&mut reader)?).map_err(|_| {
                SourceDriverError::InvalidCursor(
                    "directory checkpoint display path is not UTF-8".to_string(),
                )
            })?;
            let kind = match reader.byte()? {
                1 => DirectoryEntryKind::File,
                2 => DirectoryEntryKind::Directory,
                value => {
                    return Err(SourceDriverError::InvalidCursor(format!(
                        "unknown directory entry kind {value}"
                    )));
                }
            };
            let entry = DirectoryEntryState {
                path_key: path_key.clone(),
                display_path,
                kind,
                identity: FileIdentity::decode_from(&mut reader)?,
                revision: reader.revision()?,
                size_bytes: reader.u64()?,
                modified_ns: reader.i128()?,
                generation: reader.u64()?,
            };
            if entry.path_key.is_empty() || entry.generation == 0 {
                return Err(SourceDriverError::InvalidCursor(
                    "directory checkpoint entry is invalid".to_string(),
                ));
            }
            if entries.insert(path_key, entry).is_some() {
                return Err(SourceDriverError::InvalidCursor(
                    "directory checkpoint contains duplicate path keys".to_string(),
                ));
            }
        }
        reader.finish()?;
        if generation == 0 {
            return Err(SourceDriverError::InvalidCursor(
                "directory generation must be greater than zero".to_string(),
            ));
        }
        let checkpoint = Self {
            root_identity,
            generation,
            revision,
            entries,
        };
        if snapshot_revision(&checkpoint.root_identity, &checkpoint.entries) != checkpoint.revision
        {
            return Err(SourceDriverError::InvalidCursor(
                "directory checkpoint revision does not match entries".to_string(),
            ));
        }
        Ok(checkpoint)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectoryChangeKind {
    Added,
    Modified,
    Replaced,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryChange {
    pub kind: DirectoryChangeKind,
    pub before: Option<DirectoryEntryState>,
    pub after: Option<DirectoryEntryState>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectoryScan {
    Unavailable,
    RetryTransient,
    Snapshot {
        changes: Vec<DirectoryChange>,
        checkpoint: DirectoryCheckpoint,
        root_moved: bool,
    },
}

pub struct DirectorySnapshot {
    config: DirectorySnapshotConfig,
}

impl DirectorySnapshot {
    pub fn new(config: DirectorySnapshotConfig) -> Result<Self, SourceDriverError> {
        config.validate()?;
        Ok(Self { config })
    }

    pub fn scan<S>(
        &self,
        root: &Path,
        previous: Option<&DirectoryCheckpoint>,
        selector: &S,
    ) -> Result<DirectoryScan, SourceDriverError>
    where
        S: DirectorySelector + ?Sized,
    {
        let root_metadata = match std::fs::symlink_metadata(root) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(DirectoryScan::Unavailable);
            }
            Err(error) => return Err(io_error("reading directory metadata for", root, error)),
        };
        if root_metadata.file_type().is_symlink() {
            return Err(SourceDriverError::PathEscape(
                root.to_string_lossy().into_owned(),
            ));
        }
        if !root_metadata.is_dir() {
            return Err(SourceDriverError::InvalidConfig(format!(
                "directory snapshot root is not a directory: {}",
                root.to_string_lossy()
            )));
        }
        let root_before = file_stamp(root, &root_metadata);

        let entries = match self.enumerate(root, selector) {
            Ok(entries) => entries,
            Err(SourceDriverError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                return Ok(DirectoryScan::RetryTransient);
            }
            Err(error) => return Err(error),
        };

        let root_after_metadata = match std::fs::symlink_metadata(root) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(DirectoryScan::RetryTransient);
            }
            Err(error) => return Err(io_error("rechecking directory root", root, error)),
        };
        let root_after = file_stamp(root, &root_after_metadata);
        if root_before != root_after {
            return Ok(DirectoryScan::RetryTransient);
        }

        finish_directory_snapshot(root_before, entries, previous)
    }

    fn enumerate<S>(
        &self,
        root: &Path,
        selector: &S,
    ) -> Result<BTreeMap<Vec<u8>, DirectoryEntryState>, SourceDriverError>
    where
        S: DirectorySelector + ?Sized,
    {
        let mut entries = BTreeMap::new();
        let mut pending = vec![(root.to_path_buf(), 0_usize)];
        while let Some((directory, depth)) = pending.pop() {
            let read_dir = match std::fs::read_dir(&directory) {
                Ok(read_dir) => read_dir,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Err(SourceDriverError::Io {
                        operation: "enumerating disappearing directory",
                        path: directory.to_string_lossy().into_owned(),
                        source: error,
                    });
                }
                Err(error) => return Err(io_error("enumerating", &directory, error)),
            };
            for (enumerated_entries, entry_result) in read_dir.enumerate() {
                let entry =
                    entry_result.map_err(|error| io_error("enumerating", &directory, error))?;
                if enumerated_entries == self.config.max_entries_per_directory {
                    return Err(SourceDriverError::LimitExceeded(format!(
                        "directory snapshot exceeded {} entries in one directory",
                        self.config.max_entries_per_directory
                    )));
                }
                let entry_path = entry.path();
                let file_type = entry
                    .file_type()
                    .map_err(|error| io_error("reading entry type for", &entry_path, error))?;
                // Symlinks are never followed by the v1 common driver.
                let kind = if file_type.is_file() {
                    DirectoryEntryKind::File
                } else if file_type.is_dir() {
                    DirectoryEntryKind::Directory
                } else {
                    continue;
                };
                let relative = entry_path.strip_prefix(root).map_err(|_| {
                    SourceDriverError::PathEscape(entry_path.to_string_lossy().into_owned())
                })?;
                let selection = selector.select(relative, kind);
                if selection == DirectorySelection::Ignore {
                    continue;
                }
                if kind == DirectoryEntryKind::Directory
                    && selection.recurses()
                    && depth < self.config.max_depth
                {
                    pending.push((entry_path.clone(), depth + 1));
                }
                if !selection.includes() {
                    continue;
                }
                if entries.len() == self.config.max_entries {
                    return Err(SourceDriverError::LimitExceeded(format!(
                        "directory snapshot exceeded {} entries",
                        self.config.max_entries
                    )));
                }

                // Re-read without following symlinks after selection. If the
                // entry changed type in between, retry the whole reconcile.
                let metadata = std::fs::symlink_metadata(&entry_path).map_err(|error| {
                    io_error("reading selected entry metadata for", &entry_path, error)
                })?;
                let same_kind = match kind {
                    DirectoryEntryKind::File => metadata.is_file(),
                    DirectoryEntryKind::Directory => metadata.is_dir(),
                };
                if metadata.file_type().is_symlink() || !same_kind {
                    return Err(SourceDriverError::Io {
                        operation: "validating changed directory entry",
                        path: entry_path.to_string_lossy().into_owned(),
                        source: std::io::Error::other("entry type changed during scan"),
                    });
                }
                let stamp = file_stamp(&entry_path, &metadata);
                let path_key = confined_relative_path_key(relative)?;
                let revision = entry_revision(kind, &stamp);
                let state = DirectoryEntryState {
                    path_key: path_key.clone(),
                    display_path: relative.to_string_lossy().into_owned(),
                    kind,
                    identity: stamp.identity,
                    revision,
                    size_bytes: stamp.len,
                    modified_ns: stamp.modified_ns,
                    generation: 1,
                };
                entries.insert(path_key, state);
            }
        }
        Ok(entries)
    }
}

fn finish_directory_snapshot(
    root: FileStamp,
    mut entries: BTreeMap<Vec<u8>, DirectoryEntryState>,
    previous: Option<&DirectoryCheckpoint>,
) -> Result<DirectoryScan, SourceDriverError> {
    let root_moved = previous.is_some_and(|old| old.root_identity != root.identity);
    let generation = match previous {
        Some(old) if root_moved => next_generation(old.generation)?,
        Some(old) => old.generation,
        None => 1,
    };
    assign_entry_generations(&mut entries, previous, root_moved)?;
    let revision = snapshot_revision(&root.identity, &entries);
    let changes = diff_entries(previous, &entries, root_moved);
    Ok(DirectoryScan::Snapshot {
        changes,
        checkpoint: DirectoryCheckpoint {
            root_identity: root.identity,
            generation,
            revision,
            entries,
        },
        root_moved,
    })
}

fn assign_entry_generations(
    entries: &mut BTreeMap<Vec<u8>, DirectoryEntryState>,
    previous: Option<&DirectoryCheckpoint>,
    root_moved: bool,
) -> Result<(), SourceDriverError> {
    let Some(previous) = previous else {
        return Ok(());
    };
    for (key, entry) in entries {
        let Some(old) = previous.entries.get(key) else {
            continue;
        };
        entry.generation = if root_moved || entry.identity != old.identity {
            next_generation(old.generation)?
        } else {
            old.generation
        };
    }
    Ok(())
}

fn diff_entries(
    previous: Option<&DirectoryCheckpoint>,
    entries: &BTreeMap<Vec<u8>, DirectoryEntryState>,
    root_moved: bool,
) -> Vec<DirectoryChange> {
    let mut changes = Vec::new();
    let Some(previous) = previous else {
        changes.extend(entries.values().cloned().map(|after| DirectoryChange {
            kind: DirectoryChangeKind::Added,
            before: None,
            after: Some(after),
        }));
        return changes;
    };

    for (key, after) in entries {
        match previous.entries.get(key) {
            None => changes.push(DirectoryChange {
                kind: DirectoryChangeKind::Added,
                before: None,
                after: Some(after.clone()),
            }),
            Some(before) if root_moved || before.identity != after.identity => {
                changes.push(DirectoryChange {
                    kind: DirectoryChangeKind::Replaced,
                    before: Some(before.clone()),
                    after: Some(after.clone()),
                });
            }
            Some(before) if before.revision != after.revision => changes.push(DirectoryChange {
                kind: DirectoryChangeKind::Modified,
                before: Some(before.clone()),
                after: Some(after.clone()),
            }),
            Some(_) => {}
        }
    }
    for (key, before) in &previous.entries {
        if !entries.contains_key(key) {
            changes.push(DirectoryChange {
                kind: DirectoryChangeKind::Removed,
                before: Some(before.clone()),
                after: None,
            });
        }
    }
    changes.sort_by(|left, right| change_key(left).cmp(change_key(right)));
    changes
}

fn change_key(change: &DirectoryChange) -> &[u8] {
    change
        .after
        .as_ref()
        .or(change.before.as_ref())
        .map_or(&[], |entry| entry.path_key.as_slice())
}

fn entry_revision(kind: DirectoryEntryKind, stamp: &FileStamp) -> Revision {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&[match kind {
        DirectoryEntryKind::File => 1,
        DirectoryEntryKind::Directory => 2,
    }]);
    hasher.update(stamp_revision(stamp).as_bytes());
    Revision::from_bytes(*hasher.finalize().as_bytes())
}

fn snapshot_revision(
    root_identity: &FileIdentity,
    entries: &BTreeMap<Vec<u8>, DirectoryEntryState>,
) -> Revision {
    let mut hasher = blake3::Hasher::new();
    let mut identity = Vec::new();
    root_identity.encode_into(&mut identity);
    hasher.update(&identity);
    for entry in entries.values() {
        hasher.update(&(entry.path_key.len() as u64).to_be_bytes());
        hasher.update(&entry.path_key);
        hasher.update(entry.revision.as_bytes());
        hasher.update(&entry.generation.to_be_bytes());
    }
    Revision::from_bytes(*hasher.finalize().as_bytes())
}

fn encode_bytes(output: &mut Vec<u8>, value: &[u8]) {
    output.extend_from_slice(&(value.len() as u64).to_be_bytes());
    output.extend_from_slice(value);
}

fn decode_bytes(reader: &mut CursorReader<'_>) -> Result<Vec<u8>, SourceDriverError> {
    let length = reader.usize()?;
    Ok(reader.take(length)?.to_vec())
}

fn validate_checkpoint_path_key(
    path_key: &[u8],
    max_depth: usize,
) -> Result<usize, SourceDriverError> {
    if path_key.first() != Some(&1) {
        return Err(SourceDriverError::InvalidCursor(
            "directory checkpoint path key has an invalid version".to_string(),
        ));
    }

    let mut offset = 1_usize;
    let mut component_count = 0_usize;
    let mut final_component_start = 1_usize;
    while offset < path_key.len() {
        final_component_start = offset;
        let length_end = offset.checked_add(8).ok_or_else(|| {
            SourceDriverError::InvalidCursor(
                "directory checkpoint path key length overflowed".to_string(),
            )
        })?;
        let length_bytes: [u8; 8] = path_key
            .get(offset..length_end)
            .ok_or_else(|| {
                SourceDriverError::InvalidCursor(
                    "directory checkpoint path key is truncated".to_string(),
                )
            })?
            .try_into()
            .expect("path-key component length has a fixed width");
        let component_length = usize::try_from(u64::from_be_bytes(length_bytes)).map_err(|_| {
            SourceDriverError::InvalidCursor(
                "directory checkpoint path component length exceeds this platform".to_string(),
            )
        })?;
        if component_length == 0 {
            return Err(SourceDriverError::InvalidCursor(
                "directory checkpoint path key contains an empty component".to_string(),
            ));
        }
        offset = length_end.checked_add(component_length).ok_or_else(|| {
            SourceDriverError::InvalidCursor(
                "directory checkpoint path component length overflowed".to_string(),
            )
        })?;
        if offset > path_key.len() {
            return Err(SourceDriverError::InvalidCursor(
                "directory checkpoint path key is truncated".to_string(),
            ));
        }
        component_count = component_count.checked_add(1).ok_or_else(|| {
            SourceDriverError::InvalidCursor(
                "directory checkpoint path component count overflowed".to_string(),
            )
        })?;
        if component_count.saturating_sub(1) > max_depth {
            return Err(SourceDriverError::InvalidCursor(format!(
                "directory checkpoint path exceeds configured maximum depth {max_depth}"
            )));
        }
    }
    if component_count == 0 {
        return Err(SourceDriverError::InvalidCursor(
            "directory checkpoint path key contains no components".to_string(),
        ));
    }
    Ok(final_component_start)
}

fn next_generation(current: u64) -> Result<u64, SourceDriverError> {
    current
        .checked_add(1)
        .ok_or_else(|| SourceDriverError::InvalidCursor("source generation overflowed".to_string()))
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    fn files(relative: &Path, kind: DirectoryEntryKind) -> DirectorySelection {
        match kind {
            DirectoryEntryKind::Directory => DirectorySelection::Recurse,
            DirectoryEntryKind::File
                if relative.extension().is_some_and(|value| value == "json") =>
            {
                DirectorySelection::Include
            }
            DirectoryEntryKind::File => DirectorySelection::Ignore,
        }
    }

    fn snapshot(scan: DirectoryScan) -> (Vec<DirectoryChange>, DirectoryCheckpoint, bool) {
        match scan {
            DirectoryScan::Snapshot {
                changes,
                checkpoint,
                root_moved,
            } => (changes, checkpoint, root_moved),
            other => panic!("expected directory snapshot, got {other:?}"),
        }
    }

    #[test]
    fn selector_and_ignore_rules_shape_the_snapshot() {
        let root = TempDir::new().unwrap();
        std::fs::create_dir(root.path().join("nested")).unwrap();
        std::fs::write(root.path().join("keep.json"), b"{}").unwrap();
        std::fs::write(root.path().join("skip.txt"), b"ignored").unwrap();
        std::fs::write(root.path().join("nested/also.json"), b"{}").unwrap();
        let (_, checkpoint, _) = snapshot(
            DirectorySnapshot::new(DirectorySnapshotConfig::default())
                .unwrap()
                .scan(root.path(), None, &files)
                .unwrap(),
        );
        let mut paths: Vec<_> = checkpoint
            .entries
            .values()
            .map(|entry| entry.display_path.as_str())
            .collect();
        paths.sort_unstable();
        assert_eq!(paths, ["keep.json", "nested/also.json"]);
    }

    #[test]
    fn reconcile_recovers_add_modify_remove_and_atomic_replace_without_hints() {
        let root = TempDir::new().unwrap();
        let first_path = root.path().join("first.json");
        let removed_path = root.path().join("removed.json");
        std::fs::write(&first_path, b"one").unwrap();
        std::fs::write(&removed_path, b"remove").unwrap();
        let driver = DirectorySnapshot::new(DirectorySnapshotConfig::default()).unwrap();
        let (_, first, _) = snapshot(driver.scan(root.path(), None, &files).unwrap());

        let replacement = root.path().join("replacement.tmp");
        std::fs::write(&replacement, b"two").unwrap();
        std::fs::rename(replacement, &first_path).unwrap();
        std::fs::remove_file(removed_path).unwrap();
        std::fs::write(root.path().join("added.json"), b"add").unwrap();

        let (changes, second, _) =
            snapshot(driver.scan(root.path(), Some(&first), &files).unwrap());
        let kinds: Vec<_> = changes.iter().map(|change| change.kind).collect();
        assert_eq!(
            kinds,
            [
                DirectoryChangeKind::Added,
                DirectoryChangeKind::Replaced,
                DirectoryChangeKind::Removed,
            ]
        );
        assert_ne!(second.revision, first.revision);
        let replaced = changes
            .iter()
            .find(|change| change.kind == DirectoryChangeKind::Replaced)
            .unwrap();
        assert_eq!(
            replaced.after.as_ref().unwrap().generation,
            replaced.before.as_ref().unwrap().generation + 1
        );
    }

    #[test]
    fn missing_root_is_unavailable_not_an_empty_deletion_snapshot() {
        let root = TempDir::new().unwrap();
        let path = root.path().join("temporarily-offline");
        assert_eq!(
            DirectorySnapshot::new(DirectorySnapshotConfig::default())
                .unwrap()
                .scan(&path, None, &files)
                .unwrap(),
            DirectoryScan::Unavailable
        );
    }

    #[test]
    fn root_identity_change_replaces_matching_children_in_a_new_root_generation() {
        let parent = TempDir::new().unwrap();
        let root = parent.path().join("sessions");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("same.json"), b"one").unwrap();
        let driver = DirectorySnapshot::new(DirectorySnapshotConfig::default()).unwrap();
        let (_, first, _) = snapshot(driver.scan(&root, None, &files).unwrap());

        std::fs::rename(&root, parent.path().join("old-sessions")).unwrap();
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("same.json"), b"one").unwrap();
        let (changes, second, root_moved) =
            snapshot(driver.scan(&root, Some(&first), &files).unwrap());
        assert!(root_moved);
        assert_eq!(second.generation, first.generation + 1);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].kind, DirectoryChangeKind::Replaced);
    }

    #[test]
    fn checkpoint_encoding_is_deterministic_and_round_trips() {
        let root = TempDir::new().unwrap();
        std::fs::write(root.path().join("b.json"), b"b").unwrap();
        std::fs::write(root.path().join("a.json"), b"a").unwrap();
        let (_, checkpoint, _) = snapshot(
            DirectorySnapshot::new(DirectorySnapshotConfig::default())
                .unwrap()
                .scan(root.path(), None, &files)
                .unwrap(),
        );
        let encoded = checkpoint.encode();
        assert_eq!(
            DirectoryCheckpoint::decode_for_config(&encoded, &DirectorySnapshotConfig::default())
                .unwrap(),
            checkpoint
        );
        assert_eq!(checkpoint.encode(), encoded);
    }

    #[test]
    fn checkpoint_restore_uses_the_exact_configured_entry_bound() {
        const ABOVE_LEGACY_BOUND: usize = 100_001;
        let root_identity = FileIdentity::Unix {
            device: 1,
            inode: 1,
        };
        let entries = (0..ABOVE_LEGACY_BOUND)
            .map(|index| {
                let display_path = format!("{index:06}.json");
                let path_key = confined_relative_path_key(Path::new(&display_path)).unwrap();
                let revision = Revision::digest(&path_key);
                (
                    path_key.clone(),
                    DirectoryEntryState {
                        path_key,
                        display_path,
                        kind: DirectoryEntryKind::File,
                        identity: FileIdentity::Unix {
                            device: 1,
                            inode: index as u64 + 2,
                        },
                        revision,
                        size_bytes: 0,
                        modified_ns: index as i128,
                        generation: 1,
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        let checkpoint = DirectoryCheckpoint {
            revision: snapshot_revision(&root_identity, &entries),
            root_identity,
            generation: 1,
            entries,
        };
        let encoded = checkpoint.encode();
        drop(checkpoint);

        let legacy_bound = DirectorySnapshotConfig {
            max_entries: 100_000,
            max_entries_per_directory: 100_000,
            max_depth: 64,
        };
        let error = DirectoryCheckpoint::decode_for_config(&encoded, &legacy_bound).unwrap_err();
        assert!(matches!(error, SourceDriverError::InvalidCursor(_)));
        assert!(error.to_string().contains("configured limit 100000"));

        let candidate_bound = DirectorySnapshotConfig {
            max_entries: 250_000,
            max_entries_per_directory: 250_000,
            max_depth: 64,
        };
        let restored = DirectoryCheckpoint::decode_for_config(&encoded, &candidate_bound).unwrap();
        assert_eq!(restored.entries.len(), ABOVE_LEGACY_BOUND);
        assert_eq!(
            restored.revision,
            snapshot_revision(&restored.root_identity, &restored.entries)
        );
    }

    #[test]
    fn checkpoint_restore_rejects_paths_above_the_current_depth_bound() {
        let root = TempDir::new().unwrap();
        std::fs::create_dir(root.path().join("nested")).unwrap();
        std::fs::write(root.path().join("nested/session.json"), b"{}").unwrap();
        let scan_config = DirectorySnapshotConfig {
            max_entries: 10,
            max_entries_per_directory: 10,
            max_depth: 1,
        };
        let (_, checkpoint, _) = snapshot(
            DirectorySnapshot::new(scan_config.clone())
                .unwrap()
                .scan(root.path(), None, &files)
                .unwrap(),
        );
        let encoded = checkpoint.encode();
        assert_eq!(
            DirectoryCheckpoint::decode_for_config(&encoded, &scan_config).unwrap(),
            checkpoint
        );

        let lowered_config = DirectorySnapshotConfig {
            max_entries: 10,
            max_entries_per_directory: 10,
            max_depth: 0,
        };
        let error = DirectoryCheckpoint::decode_for_config(&encoded, &lowered_config).unwrap_err();
        assert!(matches!(error, SourceDriverError::InvalidCursor(_)));
        assert!(error.to_string().contains("configured maximum depth 0"));
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_names_keep_binary_identity_separate_from_display() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        use std::path::PathBuf;

        let name = OsString::from_vec(b"session-\xff.json".to_vec());
        let path = PathBuf::from(name);
        let key = confined_relative_path_key(&path).unwrap();
        assert!(key.contains(&0xff));
        assert!(path.to_string_lossy().contains('\u{fffd}'));
    }

    #[test]
    fn entry_bound_aborts_instead_of_returning_a_partial_snapshot() {
        let root = TempDir::new().unwrap();
        std::fs::write(root.path().join("one.json"), b"1").unwrap();
        std::fs::write(root.path().join("two.json"), b"2").unwrap();
        let error = DirectorySnapshot::new(DirectorySnapshotConfig {
            max_entries: 1,
            max_entries_per_directory: 1,
            max_depth: 1,
        })
        .unwrap()
        .scan(root.path(), None, &files)
        .unwrap_err();
        assert!(matches!(error, SourceDriverError::LimitExceeded(_)));
    }

    #[test]
    fn per_directory_bound_counts_ignored_entries_before_selection() {
        use std::cell::Cell;

        let root = TempDir::new().unwrap();
        std::fs::write(root.path().join("one.json"), b"1").unwrap();
        std::fs::write(root.path().join("two.json"), b"2").unwrap();
        let config = DirectorySnapshotConfig {
            max_entries: 10,
            max_entries_per_directory: 2,
            max_depth: 1,
        };
        snapshot(
            DirectorySnapshot::new(config.clone())
                .unwrap()
                .scan(root.path(), None, &files)
                .unwrap(),
        );

        std::fs::write(root.path().join("ignored.txt"), b"ignored").unwrap();
        let selections = Cell::new(0_usize);
        let selector = |relative: &Path, kind| {
            selections.set(selections.get() + 1);
            files(relative, kind)
        };
        let error = DirectorySnapshot::new(config)
            .unwrap()
            .scan(root.path(), None, &selector)
            .unwrap_err();
        assert!(matches!(error, SourceDriverError::LimitExceeded(_)));
        assert!(error.to_string().contains("entries in one directory"));
        assert_eq!(selections.get(), 2);
    }

    #[test]
    fn checkpoint_restore_rejects_a_parent_above_the_active_fan_out_bound() {
        let root = TempDir::new().unwrap();
        for name in ["one.json", "two.json", "three.json"] {
            std::fs::write(root.path().join(name), b"{}").unwrap();
        }
        let scan_config = DirectorySnapshotConfig {
            max_entries: 10,
            max_entries_per_directory: 3,
            max_depth: 1,
        };
        let (_, checkpoint, _) = snapshot(
            DirectorySnapshot::new(scan_config.clone())
                .unwrap()
                .scan(root.path(), None, &files)
                .unwrap(),
        );
        let encoded = checkpoint.encode();
        assert_eq!(
            DirectoryCheckpoint::decode_for_config(&encoded, &scan_config).unwrap(),
            checkpoint
        );

        let lowered_config = DirectorySnapshotConfig {
            max_entries: 10,
            max_entries_per_directory: 2,
            max_depth: 1,
        };
        let error = DirectoryCheckpoint::decode_for_config(&encoded, &lowered_config).unwrap_err();
        assert!(matches!(error, SourceDriverError::InvalidCursor(_)));
        assert!(error.to_string().contains("per-directory limit 2"));
    }

    #[test]
    fn per_directory_bound_must_fit_inside_the_aggregate_bound() {
        for config in [
            DirectorySnapshotConfig {
                max_entries: 10,
                max_entries_per_directory: 0,
                max_depth: 1,
            },
            DirectorySnapshotConfig {
                max_entries: 10,
                max_entries_per_directory: 11,
                max_depth: 1,
            },
        ] {
            let error = DirectorySnapshot::new(config).err().unwrap();
            assert!(matches!(error, SourceDriverError::InvalidConfig(_)));
            assert!(error.to_string().contains("max_entries_per_directory"));
        }
    }
}
