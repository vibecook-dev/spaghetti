//! RFC 012A section 5.3: what the common decode boundary concluded about one
//! complete source record.
//!
//! These live beside the fact types rather than inside them because they are
//! not facts: they are the record-level classification the boundary attaches
//! after an adapter returns, and losing or reordering one while append slices
//! merge would misreport coverage.

use super::SourceRecordId;

/// Value-free RFC 012A evidence retained for an unknown source record
/// regardless of the selected raw-retention policy. The excerpt is produced
/// by the common decode boundary and contains no native values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BoundedNativeEvidence {
    pub source_record_id: SourceRecordId,
    pub observed_bytes: u64,
    pub payload_digest: [u8; 32],
    pub sanitized_excerpt: Vec<u8>,
}

/// RFC 012A's topology-independent outcome for one complete source record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RecordMappingDisposition {
    Mapped {
        fact_count: u32,
    },
    IgnoredKnown {
        reason_code: String,
    },
    RetainedUnknown {
        family_hint: Option<String>,
        bounded_evidence: BoundedNativeEvidence,
    },
    BufferedIncomplete,
    Malformed {
        reason_code: String,
        bounded_diagnostic: Vec<u8>,
    },
    UnsupportedVersion {
        observed_version: String,
    },
}
