use std::fs::{File, Metadata};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
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

/// Capture the identity and bounded size of a confined regular file without
/// following source-owned symlinks. Database drivers use this before and after
/// a consistent source transaction to distinguish replacement from an
/// ordinary source commit.
pub(crate) fn confined_file_stamp(
    root: &Path,
    relative_path: &Path,
    max_bytes: usize,
) -> Result<Option<FileStamp>, SourceDriverError> {
    let path = root.join(relative_path);
    let Some(file) = open_confined_file(root, relative_path)? else {
        return Ok(None);
    };
    let handle_metadata = file
        .metadata()
        .map_err(|error| io_error("reading metadata for", &path, error))?;
    if !handle_metadata.is_file() {
        return Err(SourceDriverError::PathEscape(
            path.to_string_lossy().into_owned(),
        ));
    }
    let stamp = file_stamp(&path, &handle_metadata);
    if stamp.len > max_bytes as u64 {
        return Err(SourceDriverError::LimitExceeded(format!(
            "source file exceeds {max_bytes} byte limit"
        )));
    }
    let path_metadata = std::fs::metadata(&path)
        .map_err(|error| io_error("rechecking metadata for", &path, error))?;
    if stamp != file_stamp(&path, &path_metadata) {
        return Err(SourceDriverError::Unstable(
            "source file identity changed while it was opened".to_string(),
        ));
    }
    Ok(Some(stamp))
}

pub(crate) fn read_stable_file(
    path: &Path,
    max_bytes: usize,
) -> Result<StableRead, SourceDriverError> {
    read_stable_file_with_hook(path, max_bytes, || {})
}

pub(crate) fn read_stable_file_confined(
    root: &Path,
    relative_path: &Path,
    max_bytes: usize,
) -> Result<StableRead, SourceDriverError> {
    read_stable_file_confined_with_hook(root, relative_path, max_bytes, || {})
}

/// Read at most `max_bytes` from the head of a confined file.
///
/// Unlike [`read_stable_file_confined`], an object larger than the bound is
/// not rejected: catalog discovery deliberately wants the head of a large
/// transcript and nothing else. Confinement, symlink refusal, and the
/// directory-walk guarantees are identical.
pub(crate) fn read_prefix_confined(
    root: &Path,
    relative_path: &Path,
    max_bytes: usize,
) -> Result<Option<Vec<u8>>, SourceDriverError> {
    use std::io::Read as _;

    let path = root.join(relative_path);
    let Some(file) = open_confined_file(root, relative_path)? else {
        return Ok(None);
    };
    let mut buffer = Vec::new();
    file.take(max_bytes as u64)
        .read_to_end(&mut buffer)
        .map_err(|error| io_error("reading the head of", &path, error))?;
    Ok(Some(buffer))
}

pub(crate) fn read_stable_file_confined_with_hook<F>(
    root: &Path,
    relative_path: &Path,
    max_bytes: usize,
    after_read: F,
) -> Result<StableRead, SourceDriverError>
where
    F: FnOnce(),
{
    let path = root.join(relative_path);
    let Some(file) = open_confined_file(root, relative_path)? else {
        return Ok(StableRead::Missing);
    };
    read_stable_opened(file, &path, max_bytes, after_read)
}

pub(crate) fn read_stable_file_with_hook<F>(
    path: &Path,
    max_bytes: usize,
    after_read: F,
) -> Result<StableRead, SourceDriverError>
where
    F: FnOnce(),
{
    let (parent, file_name) = parent_and_file_name(path)?;
    let Some(file) = open_confined_file(parent, file_name)? else {
        return Ok(StableRead::Missing);
    };
    read_stable_opened(file, path, max_bytes, after_read)
}

pub(crate) fn parent_and_file_name(path: &Path) -> Result<(&Path, &Path), SourceDriverError> {
    let file_name = path
        .file_name()
        .map(Path::new)
        .ok_or_else(|| SourceDriverError::PathEscape(path.to_string_lossy().into_owned()))?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    Ok((parent, file_name))
}

fn read_stable_opened<F>(
    mut file: File,
    path: &Path,
    max_bytes: usize,
    after_read: F,
) -> Result<StableRead, SourceDriverError>
where
    F: FnOnce(),
{
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

/// Open a source file relative to an already-approved root without following
/// symlinks in any source-owned path component. On POSIX hosts each component
/// is resolved from the preceding directory descriptor, so replacing a path
/// after discovery cannot redirect the read outside the root.
pub(crate) fn open_confined_file(
    root: &Path,
    relative_path: &Path,
) -> Result<Option<File>, SourceDriverError> {
    confined_relative_path_key(relative_path)?;
    let components = relative_path
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(PathBuf::from(value)),
            Component::CurDir => None,
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => None,
        })
        .collect::<Vec<_>>();
    if components.is_empty() {
        return Err(SourceDriverError::PathEscape(
            relative_path.to_string_lossy().into_owned(),
        ));
    }

    #[cfg(unix)]
    {
        use rustix::fs::{openat, Mode, OFlags, CWD};

        let directory_flags =
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW;
        let file_flags = OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW;
        let mut directory = match openat(CWD, root, directory_flags, Mode::empty()) {
            Ok(directory) => directory,
            Err(error) => return classify_confined_open_error(root, relative_path, error),
        };
        for component in &components[..components.len() - 1] {
            directory = match openat(&directory, component, directory_flags, Mode::empty()) {
                Ok(directory) => directory,
                Err(error) => return classify_confined_open_error(root, relative_path, error),
            };
        }
        let file = match openat(
            &directory,
            &components[components.len() - 1],
            file_flags,
            Mode::empty(),
        ) {
            Ok(file) => file,
            Err(error) => return classify_confined_open_error(root, relative_path, error),
        };
        Ok(Some(File::from(file)))
    }

    #[cfg(not(unix))]
    {
        let canonical_root = root
            .canonicalize()
            .map_err(|error| io_error("resolving confined root", root, error))?;
        let candidate = root.join(relative_path);
        let metadata = match std::fs::symlink_metadata(&candidate) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(io_error(
                    "reading confined path metadata for",
                    &candidate,
                    error,
                ))
            }
        };
        if metadata.file_type().is_symlink() {
            return Err(SourceDriverError::PathEscape(
                candidate.to_string_lossy().into_owned(),
            ));
        }
        let canonical_candidate = candidate
            .canonicalize()
            .map_err(|error| io_error("resolving confined source path", &candidate, error))?;
        if !canonical_candidate.starts_with(&canonical_root) {
            return Err(SourceDriverError::PathEscape(
                candidate.to_string_lossy().into_owned(),
            ));
        }
        File::open(&canonical_candidate)
            .map(Some)
            .map_err(|error| io_error("opening confined source", &candidate, error))
    }
}

#[cfg(unix)]
fn classify_confined_open_error(
    root: &Path,
    relative_path: &Path,
    error: rustix::io::Errno,
) -> Result<Option<File>, SourceDriverError> {
    if error == rustix::io::Errno::NOENT {
        return Ok(None);
    }
    let path = root.join(relative_path);
    if error == rustix::io::Errno::LOOP || error == rustix::io::Errno::NOTDIR {
        return Err(SourceDriverError::PathEscape(
            path.to_string_lossy().into_owned(),
        ));
    }
    Err(io_error("opening confined source", &path, error.into()))
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
        _ => FileIdentity::ConfinedPath(platform_path_key(path)),
    }
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn file_identity(path: &Path, _metadata: &Metadata) -> FileIdentity {
    FileIdentity::ConfinedPath(platform_path_key(path))
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

/// Binary-safe component-framed key for an already host-approved platform
/// path. Unlike [`confined_relative_path_key`], absolute roots are allowed.
pub fn platform_path_key(path: &Path) -> Vec<u8> {
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

    use tempfile::{NamedTempFile, TempDir};

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

    #[cfg(unix)]
    #[test]
    fn confined_open_rejects_final_and_intermediate_symlinks() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().unwrap();
        let root = temp.path().join("root");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret"), b"outside-secret").unwrap();

        symlink(outside.join("secret"), root.join("final")).unwrap();
        assert!(matches!(
            open_confined_file(&root, Path::new("final")),
            Err(SourceDriverError::PathEscape(_))
        ));

        symlink(&outside, root.join("nested")).unwrap();
        assert!(matches!(
            open_confined_file(&root, Path::new("nested/secret")),
            Err(SourceDriverError::PathEscape(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn confined_stable_read_does_not_follow_a_racing_replacement() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().unwrap();
        let root = temp.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("source.json");
        let outside = temp.path().join("secret.json");
        std::fs::write(&source, b"inside").unwrap();
        std::fs::write(&outside, b"outside-secret").unwrap();

        let result =
            read_stable_file_confined_with_hook(&root, Path::new("source.json"), 64, || {
                std::fs::remove_file(&source).unwrap();
                symlink(&outside, &source).unwrap();
            })
            .unwrap();

        assert!(matches!(result, StableRead::Unstable));
    }
}
