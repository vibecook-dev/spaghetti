//! Bounded filesystem reads shared by every adapter's native support probe.
//!
//! A probe answers one question — does an installed agent look like the
//! artifact a support release declares? — before any decoding is authorized.
//! It therefore runs against a path nobody has validated yet, and every read
//! here is bounded and refuses to follow a symlink: a probe that could be
//! pointed at an arbitrary file, or made to walk an unbounded directory, would
//! be a way around the authorization it exists to establish.
//!
//! The three adapters carried byte-identical copies of these four helpers.
//! What differs between them is which objects they look at and what they
//! conclude, which stays in each adapter's own probe.

use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use super::AdapterError;

/// Probe failures are deliberately opaque: the message never names the path,
/// because a probe runs before the caller has proven it may read it.
pub(crate) fn probe_error(message: &'static str) -> AdapterError {
    AdapterError::invalid_contract(message)
}

/// The platform name support releases declare, which is not always the one
/// Rust reports.
pub(crate) fn platform_id() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "windows",
        other => other,
    }
}

/// Directory entries in a stable order, or an error past `limit`.
///
/// A missing directory is empty, not an error — an agent that has never
/// created one is a fact about the installation, not a failure. Sorting is
/// what makes a probe's conclusion independent of filesystem iteration order.
pub(crate) fn sorted_directory_entries(
    path: &Path,
    limit: usize,
) -> Result<Vec<PathBuf>, AdapterError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => {
            return Err(probe_error(
                "native support probe could not inspect a directory",
            ))
        }
    };
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(probe_error(
            "native support probe selected an invalid directory",
        ));
    }
    let mut entries = Vec::new();
    for entry in fs::read_dir(path)
        .map_err(|_| probe_error("native support probe could not read a directory"))?
    {
        let entry = entry
            .map_err(|_| probe_error("native support probe could not read a directory entry"))?;
        if entries.len() == limit {
            return Err(probe_error(
                "native support probe directory exceeded its entry bound",
            ));
        }
        entries.push(entry.path());
    }
    entries.sort();
    Ok(entries)
}

/// A whole file, up to `max_bytes`.
///
/// The bound is checked twice — against the declared length before opening,
/// and against what was actually read — because a file can grow between the
/// two, and the read takes `max_bytes + 1` so that growth is caught rather
/// than silently truncated into a shorter answer.
pub(crate) fn bounded_file_bytes(path: &Path, max_bytes: usize) -> Result<Vec<u8>, AdapterError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| probe_error("native support probe could not inspect a selected object"))?;
    if !metadata.file_type().is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > max_bytes as u64
    {
        return Err(probe_error(
            "native support probe selected an invalid or oversized object",
        ));
    }
    let file = File::open(path)
        .map_err(|_| probe_error("native support probe could not open a selected object"))?;
    let mut bytes = Vec::with_capacity((metadata.len() as usize).min(max_bytes));
    file.take(max_bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| probe_error("native support probe could not read a selected object"))?;
    if bytes.len() > max_bytes {
        return Err(probe_error(
            "native support probe object exceeded its byte bound",
        ));
    }
    Ok(bytes)
}
