use std::ffi::OsString;
use std::fs;
use std::path::Path;

/// Tighten a local engine-owned file even when it pre-dates this process with
/// permissive mode bits. Non-Unix platforms rely on their native ACL/default
/// creation policy; the path-confinement and owner-lock contracts still apply.
pub(super) fn restrict_owner_file(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let metadata = fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "engine owner file must not be a symbolic link",
            ));
        }
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

pub(super) fn reject_symlink(path: &Path) -> std::io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "engine owner file must not be a symbolic link",
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

pub(super) fn restrict_sqlite_files(database_path: &Path) -> std::io::Result<()> {
    restrict_owner_file(database_path)?;
    for suffix in ["-wal", "-shm"] {
        let mut sidecar: OsString = database_path.as_os_str().to_owned();
        sidecar.push(suffix);
        let sidecar = std::path::PathBuf::from(sidecar);
        match restrict_owner_file(&sidecar) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}
