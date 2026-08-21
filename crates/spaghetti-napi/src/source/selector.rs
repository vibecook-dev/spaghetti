//! Common confined component-glob matching for declared source selectors.
//!
//! Patterns are compiled before native access. Matching is byte-oriented and
//! component-aware so `*` never crosses a path separator and `**` is the only
//! recursive form.

use std::ffi::OsStr;
use std::path::{Component, Path};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GlobPattern(Vec<GlobComponent>);

#[derive(Debug, Clone, PartialEq, Eq)]
enum GlobComponent {
    Recursive,
    Segment(Vec<u8>),
}

impl GlobPattern {
    pub(crate) fn new(pattern: &str) -> Result<Self, String> {
        if pattern.is_empty() || pattern.starts_with('/') || pattern.ends_with('/') {
            return Err("selector must be a non-empty relative path".to_string());
        }
        let mut components = Vec::new();
        for component in pattern.split('/') {
            if component.is_empty() || component == "." || component == ".." {
                return Err("selector contains an invalid path component".to_string());
            }
            if component == "**" {
                if !matches!(components.last(), Some(GlobComponent::Recursive)) {
                    components.push(GlobComponent::Recursive);
                }
            } else if component.contains("**") {
                return Err("recursive wildcard must occupy a whole component".to_string());
            } else {
                components.push(GlobComponent::Segment(component.as_bytes().to_vec()));
            }
        }
        Ok(Self(components))
    }

    pub(crate) fn matches_path(&self, path: &Path) -> bool {
        normal_components(path).is_some_and(|path| self.matches(&path))
    }

    pub(crate) fn matches(&self, path: &[Vec<u8>]) -> bool {
        matches_components(&self.0, path)
    }
}

fn matches_components(pattern: &[GlobComponent], path: &[Vec<u8>]) -> bool {
    match (pattern.first(), path.first()) {
        (None, None) => true,
        (None, Some(_)) => false,
        (Some(GlobComponent::Recursive), _) => {
            matches_components(&pattern[1..], path)
                || (!path.is_empty() && matches_components(pattern, &path[1..]))
        }
        (Some(GlobComponent::Segment(segment)), Some(component)) => {
            matches_segment(segment, component) && matches_components(&pattern[1..], &path[1..])
        }
        (Some(GlobComponent::Segment(_)), None) => false,
    }
}

fn matches_segment(pattern: &[u8], value: &[u8]) -> bool {
    let mut states = vec![false; value.len() + 1];
    states[0] = true;
    for token in pattern {
        if *token == b'*' {
            for index in 1..=value.len() {
                states[index] = states[index] || states[index - 1];
            }
        } else {
            for index in (1..=value.len()).rev() {
                states[index] = states[index - 1] && (*token == b'?' || *token == value[index - 1]);
            }
            states[0] = false;
        }
    }
    states[value.len()]
}

fn normal_components(path: &Path) -> Option<Vec<Vec<u8>>> {
    path.components()
        .map(|component| match component {
            Component::Normal(value) => Some(os_bytes(value)),
            Component::CurDir => Some(Vec::new()),
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => None,
        })
        .filter(|component| component.as_ref().is_none_or(|value| !value.is_empty()))
        .collect()
}

#[cfg(unix)]
fn os_bytes(value: &OsStr) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    value.as_bytes().to_vec()
}

#[cfg(windows)]
fn os_bytes(value: &OsStr) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;
    value.encode_wide().flat_map(u16::to_be_bytes).collect()
}

#[cfg(not(any(unix, windows)))]
fn os_bytes(value: &OsStr) -> Vec<u8> {
    value.to_string_lossy().as_bytes().to_vec()
}
