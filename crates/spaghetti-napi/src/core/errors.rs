//! Ingest error collection.
//!
//! One accumulator, filled by the writer as it consumes the event stream, and
//! read back by the orchestrator after the writer joins. The writer is the
//! single consumer of the channel, so it is the only place that sees every
//! failure — parsers run in parallel and their events interleave.
//!
//! Three things are tracked separately because they answer different
//! questions:
//!
//! - `errors` is what a human reads, and is capped. Nobody scrolls 40,000
//!   parse failures, and materialising them costs memory proportional to the
//!   damage.
//! - `total` is uncapped, because "3 files failed" and "30,000 files failed"
//!   should not look the same to a caller deciding whether to warn loudly.
//! - `errored_paths` is complete regardless of the cap, because it is not for
//!   display: it decides which fingerprints are withheld so the failed input
//!   retries on the next warm start. Capping it would silently mark the
//!   40,001st failure as successfully ingested.
//!
//! Introduced by RFC 008 Phase 2.

use std::collections::HashSet;

/// How badly a failure affected the ingest. Mirrors the wire shape frozen in
/// RFC 008 Phase 0 (`NativeIngestErrorSeverity` in `packages/sdk/src/native.ts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// One record was skipped; its project still committed.
    RecordSkip,
    /// A whole project was rolled back.
    ProjectFatal,
    /// A failure that happened before any project identity existed.
    Source,
}

impl Severity {
    /// The string form crossing the NAPI boundary. Must stay in sync with
    /// `NativeIngestErrorSeverity`.
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::RecordSkip => "record-skip",
            Severity::ProjectFatal => "project-fatal",
            Severity::Source => "source",
        }
    }
}

/// One reported failure. `slug` is absent only for [`Severity::Source`],
/// which by definition has no project — see the frozen contract's note on why
/// inventing one is forbidden.
#[derive(Debug, Clone)]
pub struct CollectedError {
    pub slug: Option<String>,
    pub path: String,
    pub severity: Severity,
    pub message: String,
}

/// Maximum errors retained for display. `total` and `errored_paths` keep
/// counting past it.
pub const DISPLAY_CAP: usize = 100;

#[derive(Debug, Default, Clone)]
pub struct ErrorReport {
    errors: Vec<CollectedError>,
    total: u32,
    errored_paths: HashSet<String>,
    /// Slugs that saw a `ProjectFatal`. Their fingerprints are withheld
    /// wholesale, not just for the one path that failed.
    fatal_slugs: HashSet<String>,
    /// A source-level failure occurred. The success marker must not publish.
    source_failed: bool,
}

impl ErrorReport {
    pub fn record(&mut self, err: CollectedError) {
        self.total = self.total.saturating_add(1);
        self.errored_paths.insert(err.path.clone());
        match err.severity {
            Severity::ProjectFatal => {
                if let Some(slug) = &err.slug {
                    self.fatal_slugs.insert(slug.clone());
                }
            }
            Severity::Source => self.source_failed = true,
            Severity::RecordSkip => {}
        }
        if self.errors.len() < DISPLAY_CAP {
            self.errors.push(err);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.total == 0
    }

    /// First [`DISPLAY_CAP`] errors, for display.
    pub fn errors(&self) -> &[CollectedError] {
        &self.errors
    }

    /// Uncapped count of every failure seen.
    pub fn total(&self) -> u32 {
        self.total
    }

    pub fn truncated(&self) -> bool {
        self.total as usize > self.errors.len()
    }

    /// Did this exact path fail? Complete regardless of the display cap.
    pub fn path_failed(&self, path: &str) -> bool {
        self.errored_paths.contains(path)
    }

    pub fn slug_is_fatal(&self, slug: &str) -> bool {
        self.fatal_slugs.contains(slug)
    }

    pub fn source_failed(&self) -> bool {
        self.source_failed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(path: &str, severity: Severity) -> CollectedError {
        CollectedError {
            slug: Some("s".into()),
            path: path.into(),
            severity,
            message: "boom".into(),
        }
    }

    #[test]
    fn the_display_list_caps_but_the_total_does_not() {
        let mut r = ErrorReport::default();
        for i in 0..(DISPLAY_CAP + 50) {
            r.record(err(&format!("p{i}"), Severity::RecordSkip));
        }
        assert_eq!(r.errors().len(), DISPLAY_CAP);
        assert_eq!(r.total(), (DISPLAY_CAP + 50) as u32);
        assert!(r.truncated());
    }

    #[test]
    fn every_errored_path_is_retained_past_the_cap() {
        // The whole point of keeping this set uncapped: a path past the
        // display cap must still withhold its fingerprint, or the file is
        // recorded as ingested and never retried.
        let mut r = ErrorReport::default();
        for i in 0..(DISPLAY_CAP + 50) {
            r.record(err(&format!("p{i}"), Severity::RecordSkip));
        }
        assert!(r.path_failed(&format!("p{}", DISPLAY_CAP + 49)));
        assert!(!r.path_failed("never-failed"));
    }

    #[test]
    fn exactly_at_the_cap_is_not_truncated() {
        let mut r = ErrorReport::default();
        for i in 0..DISPLAY_CAP {
            r.record(err(&format!("p{i}"), Severity::RecordSkip));
        }
        assert!(!r.truncated());
    }

    #[test]
    fn severity_drives_the_wider_consequences() {
        let mut r = ErrorReport::default();
        r.record(err("a", Severity::RecordSkip));
        assert!(!r.slug_is_fatal("s") && !r.source_failed());

        r.record(err("b", Severity::ProjectFatal));
        assert!(r.slug_is_fatal("s"), "a fatal withholds the whole project");

        r.record(CollectedError {
            slug: None,
            path: "c".into(),
            severity: Severity::Source,
            message: "boom".into(),
        });
        assert!(r.source_failed(), "a source error blocks publication");
    }
}
