//! Bounded, owner-lifetime performance telemetry for the native engine.
//!
//! The hot path records into fixed atomic histograms. Snapshots are cheap,
//! allocate only when exposed to a caller, and never open another SQLite
//! connection or retain one sample per request.

use serde::{Deserialize, Serialize};
use std::array;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use ts_rs::TS;

const MAX_SOURCE_PERFORMANCE_DIMENSIONS: usize = 128;

// Sub-microsecond work shares the first bucket. The final overflow bucket uses
// the observed maximum as its percentile upper bound, so even unexpectedly
// long bootstrap commits remain representable without unbounded storage.
const LATENCY_BUCKET_UPPER_NS: [u64; 24] = [
    1_000,
    2_500,
    5_000,
    10_000,
    25_000,
    50_000,
    100_000,
    250_000,
    500_000,
    1_000_000,
    2_500_000,
    5_000_000,
    10_000_000,
    25_000_000,
    50_000_000,
    100_000_000,
    250_000_000,
    500_000_000,
    1_000_000_000,
    2_500_000_000,
    5_000_000_000,
    10_000_000_000,
    30_000_000_000,
    u64::MAX,
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LatencySnapshot {
    pub samples: u64,
    pub total_ns: u64,
    pub max_ns: u64,
    /// Approximate percentile represented by the selected fixed bucket's
    /// inclusive upper bound.
    pub p50_upper_ns: u64,
    pub p95_upper_ns: u64,
    pub p99_upper_ns: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedLatencySnapshot {
    pub name: String,
    pub latency: LatencySnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointPerformanceSnapshot {
    pub attempts: u64,
    pub completed: u64,
    pub blocked: u64,
    pub failures: u64,
    pub last_log_frames: u64,
    pub last_checkpointed_frames: u64,
    pub last_remaining_frames: u64,
    pub blocked_by_reader_ns: u64,
    pub latency: LatencySnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriterPerformanceSnapshot {
    pub uptime_ns: u64,
    pub commit_attempts: u64,
    pub committed: u64,
    pub failed: u64,
    pub facts_committed: u64,
    pub changes_published: u64,
    pub sqlite_rows_changed: u64,
    pub queue_depth: u64,
    pub queue_high_watermark: u64,
    pub checkpoint: CheckpointPerformanceSnapshot,
    pub timings: Vec<NamedLatencySnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryPerformanceSnapshot {
    pub uptime_ns: u64,
    pub requests_enqueued: u64,
    pub requests_completed: u64,
    pub queue_rejections: u64,
    pub queue_depth: u64,
    pub queue_high_watermark: u64,
    pub oldest_active_ns: u64,
    pub timings: Vec<NamedLatencySnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcePipelineSnapshot {
    pub read_attempts: u64,
    pub read_failures: u64,
    pub read_retries: u64,
    pub read_continuations: u64,
    pub records_read: u64,
    pub payload_bytes_read: u64,
    pub decode_attempts: u64,
    pub decode_failures: u64,
    pub decode_retries: u64,
    pub records_decoded: u64,
    pub facts_emitted: u64,
    pub records_quarantined: u64,
    pub timings: Vec<NamedLatencySnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceDimensionPerformanceSnapshot {
    pub adapter_id: String,
    pub stream_id: String,
    pub driver_kind: String,
    pub pipeline: SourcePipelineSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcePerformanceSnapshot {
    pub uptime_ns: u64,
    pub dimension_capacity: u64,
    /// Number of recorder assignments routed to the fixed overflow lane after
    /// the distinct adapter/stream cardinality cap was reached.
    pub dimension_overflow_assignments: u64,
    pub totals: SourcePipelineSnapshot,
    pub dimensions: Vec<SourceDimensionPerformanceSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoragePerformanceSnapshot {
    pub database_file_bytes: u64,
    pub wal_file_bytes: u64,
    pub shared_memory_file_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnginePerformanceSnapshot {
    pub writer: WriterPerformanceSnapshot,
    pub queries: QueryPerformanceSnapshot,
    pub source: SourcePerformanceSnapshot,
    pub storage: StoragePerformanceSnapshot,
}

// ── Reported shape ─────────────────────────────────────────────────────────
//
// The snapshots above are the recorder's own accounting and stay in
// nanoseconds. What crosses N-API is the projection below: durations in
// milliseconds, plus a mean the recorder never stores. These are not mirrors
// of the snapshots — the units differ and `mean_ms` is derived — so they are
// declared once, here, next to the numbers they convert, and `ts-rs` gives
// TypeScript its only copy.

fn ns_to_ms(value: u64) -> f64 {
    value as f64 / 1_000_000.0
}

/// One latency histogram, in milliseconds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct LatencyStats {
    pub samples: u64,
    pub total_ms: f64,
    /// Derived: `total_ms / samples`, and 0 when nothing was sampled.
    pub mean_ms: f64,
    pub max_ms: f64,
    pub p50_upper_ms: f64,
    pub p95_upper_ms: f64,
    pub p99_upper_ms: f64,
}

impl From<LatencySnapshot> for LatencyStats {
    fn from(value: LatencySnapshot) -> Self {
        let total_ms = ns_to_ms(value.total_ns);
        Self {
            samples: value.samples,
            total_ms,
            mean_ms: if value.samples == 0 {
                0.0
            } else {
                total_ms / value.samples as f64
            },
            max_ms: ns_to_ms(value.max_ns),
            p50_upper_ms: ns_to_ms(value.p50_upper_ns),
            p95_upper_ms: ns_to_ms(value.p95_upper_ns),
            p99_upper_ms: ns_to_ms(value.p99_upper_ns),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct NamedLatencyStats {
    pub name: String,
    pub latency: LatencyStats,
}

impl From<NamedLatencySnapshot> for NamedLatencyStats {
    fn from(value: NamedLatencySnapshot) -> Self {
        Self {
            name: value.name,
            latency: value.latency.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CheckpointPerformanceStats {
    pub attempts: u64,
    pub completed: u64,
    pub blocked: u64,
    pub failures: u64,
    pub last_log_frames: u64,
    pub last_checkpointed_frames: u64,
    pub last_remaining_frames: u64,
    pub blocked_by_reader_ms: f64,
    pub latency: LatencyStats,
}

impl From<CheckpointPerformanceSnapshot> for CheckpointPerformanceStats {
    fn from(value: CheckpointPerformanceSnapshot) -> Self {
        Self {
            attempts: value.attempts,
            completed: value.completed,
            blocked: value.blocked,
            failures: value.failures,
            last_log_frames: value.last_log_frames,
            last_checkpointed_frames: value.last_checkpointed_frames,
            last_remaining_frames: value.last_remaining_frames,
            blocked_by_reader_ms: ns_to_ms(value.blocked_by_reader_ns),
            latency: value.latency.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct WriterPerformanceStats {
    pub uptime_ms: f64,
    pub commit_attempts: u64,
    pub committed: u64,
    pub failed: u64,
    pub facts_committed: u64,
    pub changes_published: u64,
    pub sqlite_rows_changed: u64,
    pub queue_depth: u64,
    pub queue_high_watermark: u64,
    pub checkpoint: CheckpointPerformanceStats,
    pub timings: Vec<NamedLatencyStats>,
}

impl From<WriterPerformanceSnapshot> for WriterPerformanceStats {
    fn from(value: WriterPerformanceSnapshot) -> Self {
        Self {
            uptime_ms: ns_to_ms(value.uptime_ns),
            commit_attempts: value.commit_attempts,
            committed: value.committed,
            failed: value.failed,
            facts_committed: value.facts_committed,
            changes_published: value.changes_published,
            sqlite_rows_changed: value.sqlite_rows_changed,
            queue_depth: value.queue_depth,
            queue_high_watermark: value.queue_high_watermark,
            checkpoint: value.checkpoint.into(),
            timings: value.timings.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct QueryPerformanceStats {
    pub uptime_ms: f64,
    pub requests_enqueued: u64,
    pub requests_completed: u64,
    pub queue_rejections: u64,
    pub queue_depth: u64,
    pub queue_high_watermark: u64,
    pub oldest_active_ms: f64,
    pub timings: Vec<NamedLatencyStats>,
}

impl From<QueryPerformanceSnapshot> for QueryPerformanceStats {
    fn from(value: QueryPerformanceSnapshot) -> Self {
        Self {
            uptime_ms: ns_to_ms(value.uptime_ns),
            requests_enqueued: value.requests_enqueued,
            requests_completed: value.requests_completed,
            queue_rejections: value.queue_rejections,
            queue_depth: value.queue_depth,
            queue_high_watermark: value.queue_high_watermark,
            oldest_active_ms: ns_to_ms(value.oldest_active_ns),
            timings: value.timings.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SourcePipelineStats {
    pub read_attempts: u64,
    pub read_failures: u64,
    pub read_retries: u64,
    pub read_continuations: u64,
    pub records_read: u64,
    pub payload_bytes_read: u64,
    pub decode_attempts: u64,
    pub decode_failures: u64,
    pub decode_retries: u64,
    pub records_decoded: u64,
    pub facts_emitted: u64,
    pub records_quarantined: u64,
    pub timings: Vec<NamedLatencyStats>,
}

impl From<SourcePipelineSnapshot> for SourcePipelineStats {
    fn from(value: SourcePipelineSnapshot) -> Self {
        Self {
            read_attempts: value.read_attempts,
            read_failures: value.read_failures,
            read_retries: value.read_retries,
            read_continuations: value.read_continuations,
            records_read: value.records_read,
            payload_bytes_read: value.payload_bytes_read,
            decode_attempts: value.decode_attempts,
            decode_failures: value.decode_failures,
            decode_retries: value.decode_retries,
            records_decoded: value.records_decoded,
            facts_emitted: value.facts_emitted,
            records_quarantined: value.records_quarantined,
            timings: value.timings.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SourceDimensionPerformanceStats {
    pub adapter_id: String,
    pub stream_id: String,
    pub driver_kind: String,
    pub pipeline: SourcePipelineStats,
}

impl From<SourceDimensionPerformanceSnapshot> for SourceDimensionPerformanceStats {
    fn from(value: SourceDimensionPerformanceSnapshot) -> Self {
        Self {
            adapter_id: value.adapter_id,
            stream_id: value.stream_id,
            driver_kind: value.driver_kind,
            pipeline: value.pipeline.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SourcePerformanceStats {
    pub uptime_ms: f64,
    pub dimension_capacity: u64,
    /// Recorder assignments routed to the fixed overflow lane after the
    /// distinct adapter/stream cardinality cap was reached.
    pub dimension_overflow_assignments: u64,
    pub totals: SourcePipelineStats,
    pub dimensions: Vec<SourceDimensionPerformanceStats>,
}

impl From<SourcePerformanceSnapshot> for SourcePerformanceStats {
    fn from(value: SourcePerformanceSnapshot) -> Self {
        Self {
            uptime_ms: ns_to_ms(value.uptime_ns),
            dimension_capacity: value.dimension_capacity,
            dimension_overflow_assignments: value.dimension_overflow_assignments,
            totals: value.totals.into(),
            dimensions: value.dimensions.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct StoragePerformanceStats {
    pub database_file_bytes: u64,
    pub wal_file_bytes: u64,
    pub shared_memory_file_bytes: u64,
}

impl From<StoragePerformanceSnapshot> for StoragePerformanceStats {
    fn from(value: StoragePerformanceSnapshot) -> Self {
        Self {
            database_file_bytes: value.database_file_bytes,
            wal_file_bytes: value.wal_file_bytes,
            shared_memory_file_bytes: value.shared_memory_file_bytes,
        }
    }
}

/// Owner-lifetime telemetry as `getStats()` reports it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PerformanceStats {
    pub writer: WriterPerformanceStats,
    pub queries: QueryPerformanceStats,
    pub source: SourcePerformanceStats,
    pub storage: StoragePerformanceStats,
}

impl From<EnginePerformanceSnapshot> for PerformanceStats {
    fn from(value: EnginePerformanceSnapshot) -> Self {
        Self {
            writer: value.writer.into(),
            queries: value.queries.into(),
            source: value.source.into(),
            storage: value.storage.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SourceDimensionKey {
    adapter_id: String,
    stream_id: String,
    driver_kind: String,
}

struct SourceDimensionTelemetry {
    key: SourceDimensionKey,
    pipeline: SourcePipelineTelemetry,
}

#[derive(Default)]
struct SourcePipelineTelemetry {
    read_attempts: AtomicU64,
    read_failures: AtomicU64,
    read_retries: AtomicU64,
    read_continuations: AtomicU64,
    records_read: AtomicU64,
    payload_bytes_read: AtomicU64,
    decode_attempts: AtomicU64,
    decode_failures: AtomicU64,
    decode_retries: AtomicU64,
    records_decoded: AtomicU64,
    facts_emitted: AtomicU64,
    records_quarantined: AtomicU64,
    source_read: LatencyHistogram,
    decode_total: LatencyHistogram,
    adapter_decode: LatencyHistogram,
    fact_build: LatencyHistogram,
}

pub(crate) struct SourceTelemetry {
    opened_at: Instant,
    totals: SourcePipelineTelemetry,
    dimensions: Mutex<BTreeMap<SourceDimensionKey, Arc<SourceDimensionTelemetry>>>,
    overflow: Arc<SourceDimensionTelemetry>,
    dimension_overflow_assignments: AtomicU64,
}

#[derive(Clone)]
pub(crate) struct SourcePerformanceRecorder {
    telemetry: Arc<SourceTelemetry>,
    dimension: Arc<SourceDimensionTelemetry>,
}

pub(crate) struct SourceDecodeObservation {
    pub elapsed: Duration,
    pub adapter_elapsed: Duration,
    pub fact_build: Duration,
    pub outcome: SourceDecodeOutcome,
}

#[derive(Clone, Copy)]
pub(crate) enum SourceDecodeOutcome {
    Decoded { facts: u64, quarantined: bool },
    Retry,
    Failed,
}

impl SourceTelemetry {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            opened_at: Instant::now(),
            totals: SourcePipelineTelemetry::default(),
            dimensions: Mutex::new(BTreeMap::new()),
            overflow: Arc::new(SourceDimensionTelemetry {
                key: SourceDimensionKey {
                    adapter_id: "__overflow__".to_string(),
                    stream_id: "__overflow__".to_string(),
                    driver_kind: "__overflow__".to_string(),
                },
                pipeline: SourcePipelineTelemetry::default(),
            }),
            dimension_overflow_assignments: AtomicU64::new(0),
        })
    }

    pub(crate) fn recorder(
        self: &Arc<Self>,
        adapter_id: &str,
        stream_id: &str,
        driver_kind: &str,
    ) -> SourcePerformanceRecorder {
        let key = SourceDimensionKey {
            adapter_id: adapter_id.to_string(),
            stream_id: stream_id.to_string(),
            driver_kind: driver_kind.to_string(),
        };
        let dimension = {
            let mut dimensions = self
                .dimensions
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(existing) = dimensions.get(&key) {
                Arc::clone(existing)
            } else if dimensions.len() < MAX_SOURCE_PERFORMANCE_DIMENSIONS {
                let telemetry = Arc::new(SourceDimensionTelemetry {
                    key: key.clone(),
                    pipeline: SourcePipelineTelemetry::default(),
                });
                dimensions.insert(key, Arc::clone(&telemetry));
                telemetry
            } else {
                atomic_saturating_add(&self.dimension_overflow_assignments, 1);
                Arc::clone(&self.overflow)
            }
        };
        SourcePerformanceRecorder {
            telemetry: Arc::clone(self),
            dimension,
        }
    }

    pub(crate) fn snapshot(&self) -> SourcePerformanceSnapshot {
        let mut dimensions = self
            .dimensions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
            .map(|dimension| dimension.snapshot())
            .collect::<Vec<_>>();
        if self.dimension_overflow_assignments.load(Ordering::Acquire) > 0 {
            dimensions.push(self.overflow.snapshot());
        }
        SourcePerformanceSnapshot {
            uptime_ns: duration_ns(self.opened_at.elapsed()),
            dimension_capacity: u64::try_from(MAX_SOURCE_PERFORMANCE_DIMENSIONS)
                .unwrap_or(u64::MAX),
            dimension_overflow_assignments: self
                .dimension_overflow_assignments
                .load(Ordering::Acquire),
            totals: self.totals.snapshot(),
            dimensions,
        }
    }
}

impl SourcePerformanceRecorder {
    pub(crate) fn record_read(
        &self,
        elapsed: Duration,
        failed: bool,
        retry: bool,
        continuation: bool,
        records: u64,
        payload_bytes: u64,
    ) {
        self.telemetry.totals.record_read(
            elapsed,
            failed,
            retry,
            continuation,
            records,
            payload_bytes,
        );
        self.dimension.pipeline.record_read(
            elapsed,
            failed,
            retry,
            continuation,
            records,
            payload_bytes,
        );
    }

    pub(crate) fn record_decode(&self, observation: &SourceDecodeObservation) {
        self.telemetry.totals.record_decode(observation);
        self.dimension.pipeline.record_decode(observation);
    }
}

impl SourcePipelineTelemetry {
    fn record_read(
        &self,
        elapsed: Duration,
        failed: bool,
        retry: bool,
        continuation: bool,
        records: u64,
        payload_bytes: u64,
    ) {
        atomic_saturating_add(&self.read_attempts, 1);
        atomic_saturating_add(&self.records_read, records);
        atomic_saturating_add(&self.payload_bytes_read, payload_bytes);
        if failed {
            atomic_saturating_add(&self.read_failures, 1);
        }
        if retry {
            atomic_saturating_add(&self.read_retries, 1);
        }
        if continuation {
            atomic_saturating_add(&self.read_continuations, 1);
        }
        self.source_read.record(elapsed);
    }

    fn record_decode(&self, observation: &SourceDecodeObservation) {
        atomic_saturating_add(&self.decode_attempts, 1);
        match observation.outcome {
            SourceDecodeOutcome::Failed => {
                atomic_saturating_add(&self.decode_failures, 1);
            }
            SourceDecodeOutcome::Retry => {
                atomic_saturating_add(&self.decode_retries, 1);
            }
            SourceDecodeOutcome::Decoded { facts, quarantined } => {
                atomic_saturating_add(&self.records_decoded, 1);
                atomic_saturating_add(&self.facts_emitted, facts);
                if quarantined {
                    atomic_saturating_add(&self.records_quarantined, 1);
                }
            }
        }
        self.decode_total.record(observation.elapsed);
        self.adapter_decode.record(observation.adapter_elapsed);
        self.fact_build.record(observation.fact_build);
    }

    fn snapshot(&self) -> SourcePipelineSnapshot {
        SourcePipelineSnapshot {
            read_attempts: self.read_attempts.load(Ordering::Acquire),
            read_failures: self.read_failures.load(Ordering::Acquire),
            read_retries: self.read_retries.load(Ordering::Acquire),
            read_continuations: self.read_continuations.load(Ordering::Acquire),
            records_read: self.records_read.load(Ordering::Acquire),
            payload_bytes_read: self.payload_bytes_read.load(Ordering::Acquire),
            decode_attempts: self.decode_attempts.load(Ordering::Acquire),
            decode_failures: self.decode_failures.load(Ordering::Acquire),
            decode_retries: self.decode_retries.load(Ordering::Acquire),
            records_decoded: self.records_decoded.load(Ordering::Acquire),
            facts_emitted: self.facts_emitted.load(Ordering::Acquire),
            records_quarantined: self.records_quarantined.load(Ordering::Acquire),
            timings: [
                ("source_read", &self.source_read),
                ("decode_total", &self.decode_total),
                ("adapter_decode", &self.adapter_decode),
                ("fact_build", &self.fact_build),
            ]
            .into_iter()
            .map(|(name, histogram)| NamedLatencySnapshot {
                name: name.to_string(),
                latency: histogram.snapshot(),
            })
            .collect(),
        }
    }
}

impl SourceDimensionTelemetry {
    fn snapshot(&self) -> SourceDimensionPerformanceSnapshot {
        SourceDimensionPerformanceSnapshot {
            adapter_id: self.key.adapter_id.clone(),
            stream_id: self.key.stream_id.clone(),
            driver_kind: self.key.driver_kind.clone(),
            pipeline: self.pipeline.snapshot(),
        }
    }
}

pub(crate) struct LatencyHistogram {
    samples: AtomicU64,
    total_ns: AtomicU64,
    max_ns: AtomicU64,
    buckets: [AtomicU64; LATENCY_BUCKET_UPPER_NS.len()],
}

impl Default for LatencyHistogram {
    fn default() -> Self {
        Self {
            samples: AtomicU64::new(0),
            total_ns: AtomicU64::new(0),
            max_ns: AtomicU64::new(0),
            buckets: array::from_fn(|_| AtomicU64::new(0)),
        }
    }
}

impl LatencyHistogram {
    pub(crate) fn record(&self, elapsed: Duration) {
        self.record_ns(duration_ns(elapsed));
    }

    pub(crate) fn record_ns(&self, elapsed_ns: u64) {
        let bucket = LATENCY_BUCKET_UPPER_NS
            .partition_point(|upper| *upper < elapsed_ns)
            .min(LATENCY_BUCKET_UPPER_NS.len() - 1);
        atomic_saturating_add(&self.buckets[bucket], 1);
        atomic_saturating_add(&self.total_ns, elapsed_ns);
        atomic_max(&self.max_ns, elapsed_ns);
        // Publish the sample count last. An acquiring snapshot that observes
        // this sample also observes its bucket, total, and maximum updates,
        // rather than briefly seeing a sample with no corresponding bucket.
        atomic_saturating_add(&self.samples, 1);
    }

    pub(crate) fn snapshot(&self) -> LatencySnapshot {
        let samples = self.samples.load(Ordering::Acquire);
        let max_ns = self.max_ns.load(Ordering::Acquire);
        let buckets = self
            .buckets
            .each_ref()
            .map(|bucket| bucket.load(Ordering::Acquire));
        LatencySnapshot {
            samples,
            total_ns: self.total_ns.load(Ordering::Acquire),
            max_ns,
            p50_upper_ns: percentile_upper_bound(&buckets, samples, 50, max_ns),
            p95_upper_ns: percentile_upper_bound(&buckets, samples, 95, max_ns),
            p99_upper_ns: percentile_upper_bound(&buckets, samples, 99, max_ns),
        }
    }
}

pub(crate) fn duration_ns(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

pub(crate) fn atomic_saturating_add(target: &AtomicU64, value: u64) {
    let _ = target.fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
        Some(current.saturating_add(value))
    });
}

pub(crate) fn atomic_max(target: &AtomicU64, value: u64) {
    let _ = target.fetch_max(value, Ordering::AcqRel);
}

fn percentile_upper_bound(
    buckets: &[u64; LATENCY_BUCKET_UPPER_NS.len()],
    samples: u64,
    percentile: u64,
    max_ns: u64,
) -> u64 {
    if samples == 0 {
        return 0;
    }
    let target = u64::try_from((u128::from(samples) * u128::from(percentile)).div_ceil(100))
        .unwrap_or(u64::MAX);
    let mut cumulative = 0_u64;
    for (index, count) in buckets.iter().enumerate() {
        cumulative = cumulative.saturating_add(*count);
        if cumulative >= target {
            let upper = LATENCY_BUCKET_UPPER_NS[index];
            return if upper == u64::MAX { max_ns } else { upper };
        }
    }
    max_ns
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_histogram_reports_bounded_percentile_upper_limits() {
        let histogram = LatencyHistogram::default();
        for micros in 1..=100 {
            histogram.record(Duration::from_micros(micros));
        }
        let snapshot = histogram.snapshot();
        assert_eq!(snapshot.samples, 100);
        assert_eq!(snapshot.total_ns, 5_050_000);
        assert_eq!(snapshot.max_ns, 100_000);
        assert_eq!(snapshot.p50_upper_ns, 50_000);
        assert_eq!(snapshot.p95_upper_ns, 100_000);
        assert_eq!(snapshot.p99_upper_ns, 100_000);
    }

    #[test]
    fn empty_and_overflow_histograms_remain_well_defined() {
        let histogram = LatencyHistogram::default();
        assert_eq!(histogram.snapshot().p99_upper_ns, 0);
        histogram.record_ns(u64::MAX);
        let snapshot = histogram.snapshot();
        assert_eq!(snapshot.total_ns, u64::MAX);
        assert_eq!(snapshot.p50_upper_ns, u64::MAX);
        assert_eq!(snapshot.p99_upper_ns, u64::MAX);
    }

    #[test]
    fn source_telemetry_is_bounded_and_preserves_global_totals() {
        let telemetry = SourceTelemetry::new();
        let recorder = telemetry.recorder("adapter", "stream", "append");
        recorder.record_read(Duration::from_millis(2), false, false, true, 3, 120);
        recorder.record_decode(&SourceDecodeObservation {
            elapsed: Duration::from_millis(1),
            adapter_elapsed: Duration::from_micros(750),
            fact_build: Duration::from_micros(100),
            outcome: SourceDecodeOutcome::Decoded {
                facts: 4,
                quarantined: true,
            },
        });

        for index in 0..=MAX_SOURCE_PERFORMANCE_DIMENSIONS {
            telemetry
                .recorder("overflow-adapter", &format!("stream-{index}"), "snapshot")
                .record_read(Duration::from_micros(1), false, false, false, 1, 1);
        }

        let snapshot = telemetry.snapshot();
        assert_eq!(
            snapshot.dimension_capacity,
            MAX_SOURCE_PERFORMANCE_DIMENSIONS as u64
        );
        assert_eq!(snapshot.dimension_overflow_assignments, 2);
        assert_eq!(
            snapshot.dimensions.len(),
            MAX_SOURCE_PERFORMANCE_DIMENSIONS + 1,
            "the fixed overflow lane is reported separately"
        );
        assert_eq!(snapshot.totals.read_attempts, 130);
        assert_eq!(snapshot.totals.read_retries, 0);
        assert_eq!(snapshot.totals.read_continuations, 1);
        assert_eq!(snapshot.totals.records_read, 132);
        assert_eq!(snapshot.totals.payload_bytes_read, 249);
        assert_eq!(snapshot.totals.decode_attempts, 1);
        assert_eq!(snapshot.totals.records_decoded, 1);
        assert_eq!(snapshot.totals.facts_emitted, 4);
        assert_eq!(snapshot.totals.records_quarantined, 1);
        assert_eq!(
            snapshot
                .totals
                .timings
                .iter()
                .find(|timing| timing.name == "source_read")
                .unwrap()
                .latency
                .samples,
            snapshot.totals.read_attempts
        );
    }
}
