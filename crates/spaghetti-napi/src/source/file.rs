use std::fs::{File, Metadata};
use std::io::Read;
use std::path::{Component, Path};
use std::time::UNIX_EPOCH;

use super::model::{io_error, FileIdentity, Revision, SourceDriverError};

#[derive(Debug)]
pub(crate) enum StableRead {
    Missing,
    Oversized(FileStamp),
    Unstable,
    Stable {
        stamp: FileStamp,
        bytes: Vec<u8>,
        revision: Revision,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileStamp {
    pub identity: FileIdentity,
    pub len: u64,
    pub modified_ns: i128,
}

pub(crate) fn read_stable_file(
    path: &Path,
    max_bytes: usize,
) -> Result<StableRead, SourceDriverError> {
    read_stable_file_with_hook(path, max_bytes, || {})
}

pub(crate) fn read_stable_file_with_hook<F>(
    path: &Path,
    max_bytes: usize,
    after_read: F,
) -> Result<StableRead, SourceDriverError>
where
    F: FnOnce(),
{
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(StableRead::Missing);
        }
        Err(error) => return Err(io_error("opening", path, error)),
    };
    let before_metadata = file
        .metadata()
        .map_err(|error| io_error("reading metadata for", path, error))?;
    let before = file_stamp(path, &before_metadata);
    if before.len > max_bytes as u64 {
        after_read();
        let after_handle = file
            .metadata()
            .map_err(|error| io_error("rechecking metadata for", path, error))?;
        let after_path = match std::fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(StableRead::Unstable);
            }
            Err(error) => return Err(io_error("rechecking path for", path, error)),
        };
        if before != file_stamp(path, &after_handle) || before != file_stamp(path, &after_path) {
            return Ok(StableRead::Unstable);
        }
        return Ok(StableRead::Oversized(before));
    }

    let mut bytes = Vec::with_capacity(before.len as usize);
    file.read_to_end(&mut bytes)
        .map_err(|error| io_error("reading", path, error))?;
    after_read();

    let after_handle = file
        .metadata()
        .map_err(|error| io_error("rechecking metadata for", path, error))?;
    let after_path = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(StableRead::Unstable);
        }
        Err(error) => return Err(io_error("rechecking path for", path, error)),
    };
    let handle_stamp = file_stamp(path, &after_handle);
    let path_stamp = file_stamp(path, &after_path);
    if before != handle_stamp || before != path_stamp || bytes.len() as u64 != before.len {
        return Ok(StableRead::Unstable);
    }

    let revision = Revision::digest(&bytes);
    Ok(StableRead::Stable {
        stamp: before,
        bytes,
        revision,
    })
}

pub(crate) fn file_stamp(path: &Path, metadata: &Metadata) -> FileStamp {
    FileStamp {
        identity: file_identity(path, metadata),
        len: metadata.len(),
        modified_ns: modified_ns(metadata),
    }
}

pub(crate) fn stamp_revision(stamp: &FileStamp) -> Revision {
    let mut bytes = Vec::with_capacity(64);
    stamp.identity.encode_into(&mut bytes);
    bytes.extend_from_slice(&stamp.len.to_be_bytes());
    bytes.extend_from_slice(&stamp.modified_ns.to_be_bytes());
    Revision::digest(&bytes)
}

#[cfg(unix)]
pub(crate) fn file_identity(_path: &Path, metadata: &Metadata) -> FileIdentity {
    use std::os::unix::fs::MetadataExt;
    FileIdentity::Unix {
        device: metadata.dev(),
        inode: metadata.ino(),
    }
}

#[cfg(windows)]
pub(crate) fn file_identity(path: &Path, metadata: &Metadata) -> FileIdentity {
    use std::os::windows::fs::MetadataExt;

    match (metadata.volume_serial_number(), metadata.file_index()) {
        (Some(volume), Some(file)) => FileIdentity::Windows {
            volume: u64::from(volume),
            file: u128::from(file),
        },
        _ => FileIdentity::ConfinedPath(path_key(path)),
    }
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn file_identity(path: &Path, _metadata: &Metadata) -> FileIdentity {
    FileIdentity::ConfinedPath(path_key(path))
}

pub(crate) fn modified_ns(metadata: &Metadata) -> i128 {
    metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |duration| {
            i128::from(duration.as_secs()) * 1_000_000_000 + i128::from(duration.subsec_nanos())
        })
}

/// Binary-safe, component-framed path identity. `.` is ignored and parent or
/// root components are rejected so keys cannot smuggle an escaped path.
pub(crate) fn confined_relative_path_key(path: &Path) -> Result<Vec<u8>, SourceDriverError> {
    let mut output = vec![1];
    for component in path.components() {
        match component {
            Component::Normal(value) => {
                let bytes = os_bytes(value);
                output.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
                output.extend_from_slice(&bytes);
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(SourceDriverError::PathEscape(
                    path.to_string_lossy().into_owned(),
                ));
            }
        }
    }
    Ok(output)
}

pub(crate) fn path_key(path: &Path) -> Vec<u8> {
    let mut output = vec![1];
    for component in path.components() {
        let bytes = os_bytes(component.as_os_str());
        output.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
        output.extend_from_slice(&bytes);
    }
    output
}

#[cfg(unix)]
fn os_bytes(value: &std::ffi::OsStr) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    value.as_bytes().to_vec()
}

#[cfg(windows)]
fn os_bytes(value: &std::ffi::OsStr) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;
    value.encode_wide().flat_map(u16::to_be_bytes).collect()
}

#[cfg(not(any(unix, windows)))]
fn os_bytes(value: &std::ffi::OsStr) -> Vec<u8> {
    value.to_string_lossy().as_bytes().to_vec()
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use tempfile::NamedTempFile;

    use super::*;

    #[test]
    fn stable_read_detects_an_in_place_write_race() {
        let mut source = NamedTempFile::new().unwrap();
        source.write_all(b"before").unwrap();
        source.flush().unwrap();

        let result = read_stable_file_with_hook(source.path(), 64, || {
            std::fs::write(source.path(), b"after-and-longer").unwrap();
        })
        .unwrap();
        assert!(matches!(result, StableRead::Unstable));
    }

    #[test]
    fn confined_keys_reject_parent_components() {
        assert!(matches!(
            confined_relative_path_key(Path::new("../escape")),
            Err(SourceDriverError::PathEscape(_))
        ));
    }
}
