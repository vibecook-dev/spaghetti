//! One observed source object: read, decode, and the facts that fall out.
//!
//! This is the observer's whole I/O surface. It uses the same append driver,
//! the same `decode_record` boundary, and the same semantic-identity binding as
//! durable ingestion, which is what makes the observer's `SemanticRevisionRef`
//! values equal to the durable ones for the same revision.

use std::path::{Path, PathBuf};

use crate::adapter::{
    AdapterObjectContext, AgentAdapter, DecoderId, DriverSpec, Fact, FactSemanticContext,
    FactSemanticRevision, RawRetentionPolicy, RecordMappingDisposition, ScopeJoinUpdate,
    SourceInstance, SourceObjectDescriptor, StreamId,
};
use crate::decode_runtime::{
    decode_record, DecodeRuntimeLimits, DecodeRuntimeRequest, DecoderDependenciesDenied,
};
use crate::source::{
    AppendCheckpoint, AppendDelimitedFile, AppendItem, AppendRead, AppendTransition, RecordOrigin,
    ReplaceCheckpoint, ReplaceDocument, ReplaceRead, SourceMediaType, SourceRecord,
};

use super::scope::ScopeMember;
use super::ObserverError;

const MAX_FACTS_PER_RECORD: usize = 512;
const MAX_DIAGNOSTICS_PER_RECORD: usize = 64;
const JSONL_MEDIA_TYPE: &str = "application/x-ndjson";
const JSON_MEDIA_TYPE: &str = "application/json";

/// Why an object's generation advanced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ObjectReset {
    pub old_generation: u64,
    pub new_generation: u64,
    pub reason: &'static str,
}

fn transition_reason(transition: AppendTransition) -> &'static str {
    match transition {
        AppendTransition::Initial => "initial",
        AppendTransition::Continued => "continued",
        AppendTransition::Truncated => "truncated",
        AppendTransition::IdentityChanged => "identity_changed",
        AppendTransition::PrefixMismatch => "prefix_mismatch",
        AppendTransition::ContractReplay => "contract_replay",
    }
}

/// One decoded fact with everything an event needs.
pub(crate) struct DecodedFact {
    pub semantic: FactSemanticRevision,
    pub value: Fact,
    pub native_time: Option<i64>,
    pub observed_at: i64,
    pub byte_start: Option<u64>,
    pub byte_end: Option<u64>,
    pub record_digest: [u8; 32],
    pub generation: u64,
}

/// One record the adapter could not interpret, reduced to bounded evidence.
pub(crate) struct UnknownRecord {
    pub family_hint: Option<String>,
    pub observed_bytes: u64,
    pub source_record_id: crate::adapter::SourceRecordId,
    pub byte_start: Option<u64>,
    pub byte_end: Option<u64>,
    pub record_digest: [u8; 32],
    pub generation: u64,
    pub observed_at: i64,
}

/// Result of one reconciliation pass over one object.
#[derive(Default)]
pub(crate) struct ObjectPass {
    pub facts: Vec<DecodedFact>,
    /// Records the adapter retained as unknown. Dropping these would turn a
    /// coverage hole into silence.
    pub unknown: Vec<UnknownRecord>,
    pub joins: Vec<ScopeJoinUpdate>,
    pub reset: Option<ObjectReset>,
    /// Set when the driver reported the object as gone. Facts owned by the
    /// object are retracted by the caller.
    pub removed: bool,
    /// The driver has more bytes ready than the batch bound allowed.
    pub more_available: bool,
    pub error: Option<String>,
}

enum Driver {
    Append {
        driver: AppendDelimitedFile,
        checkpoint: Option<AppendCheckpoint>,
    },
    Replace {
        driver: ReplaceDocument,
        checkpoint: Option<ReplaceCheckpoint>,
    },
}

/// A single in-scope object the observer follows.
pub(crate) struct ObservedObject {
    member: ScopeMember,
    /// Resolved access root. The adapter declares where each named root lives;
    /// `home` is the agent root itself, not a `home/` subdirectory.
    root: PathBuf,
    driver: Driver,
    decoder: DecoderId,
    retention: RawRetentionPolicy,
    object_context: AdapterObjectContext,
    semantic_context: FactSemanticContext,
    origin: RecordOrigin,
    /// `(len, mtime)` at the last completed read, for the cheap unchanged-check
    /// a sweep uses before deciding to open anything.
    last_stat: Option<(u64, std::time::SystemTime)>,
    decoder_state: Option<Vec<u8>>,
    generation: u64,
    present: bool,
}

impl ObservedObject {
    /// Bind an object's decoder and semantic identity. This performs no I/O on
    /// the object itself, so an object that does not exist yet still has a
    /// final identity and can be watched.
    pub(crate) fn bind(
        adapter: &dyn AgentAdapter,
        instance: &SourceInstance,
        member: ScopeMember,
        local_object_id: u64,
    ) -> Result<Option<Self>, ObserverError> {
        let driver = match &member.driver {
            DriverSpec::AppendDelimited(config) => Driver::Append {
                driver: AppendDelimitedFile::new(config.clone())
                    .map_err(|error| ObserverError::Source(error.to_string()))?,
                checkpoint: None,
            },
            DriverSpec::ReplaceDocument(config) => Driver::Replace {
                driver: ReplaceDocument::new(config.clone())
                    .map_err(|error| ObserverError::Source(error.to_string()))?,
                checkpoint: None,
            },
            // Presence, directory, and database primitives are not part of the
            // v1 scoped surface; the declared relation simply stays unopened.
            _ => return Ok(None),
        };

        let object_key = crate::source::confined_relative_path_key(&member.key.relative_path)
            .map_err(|error| ObserverError::Source(error.to_string()))?;
        let descriptor = SourceObjectDescriptor {
            stream_id: StreamId::new(member.key.stream_id.clone())
                .map_err(|error| ObserverError::Adapter(error.to_string()))?,
            object_key: object_key.clone(),
            relative_path: member.key.relative_path.clone(),
        };
        let object_context = crate::decode_runtime::bootstrap_object_without_source_access(
            adapter,
            instance,
            &descriptor,
        )
        .map_err(|error| ObserverError::Adapter(error.to_string()))?;
        let semantic_context = FactSemanticContext::new(
            &adapter.manifest().id,
            instance.spec.identity_contract_version,
            instance.spec.stable_key.as_bytes(),
            member.key.stream_id.as_bytes(),
            &object_key,
            member.driver.framing_contract_version(),
        )
        .map_err(|error| ObserverError::Adapter(error.to_string()))?;

        let root = instance
            .root(&member.key.root_name)
            .map_err(|error| ObserverError::Adapter(error.to_string()))?
            .to_path_buf();

        let media_type = if matches!(member.driver, DriverSpec::AppendDelimited(_)) {
            JSONL_MEDIA_TYPE
        } else {
            JSON_MEDIA_TYPE
        };
        let origin = RecordOrigin {
            source_instance_id: instance.id,
            stream_id: local_object_id,
            object_id: local_object_id,
            observed_at: 0,
            source_timestamp_hint: None,
            media_type: SourceMediaType::new(media_type)
                .map_err(|error| ObserverError::Source(error.to_string()))?,
        };

        Ok(Some(Self {
            decoder: member.decoder.clone(),
            retention: member.retention,
            member,
            root,
            driver,
            object_context,
            semantic_context,
            origin,
            last_stat: None,
            decoder_state: None,
            generation: 0,
            present: false,
        }))
    }

    /// The declared relation that admitted this object. RFC 012D section 5
    /// requires every opened object to be attributable to exactly one.
    pub(crate) fn relation_id(&self) -> &'static str {
        self.member.relation_id
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn present(&self) -> bool {
        self.present
    }

    /// Cheap pre-check for a sweep: has this object plausibly changed?
    ///
    /// A `stat` costs a fraction of an open-and-frame, which is what lets a
    /// sweep cover a large scope without becoming a latency spike. It is only
    /// ever used to skip work: anything it cannot rule out is read normally,
    /// and an object it wrongly skips is still caught by the next full read,
    /// so the worst case is bounded staleness rather than a lost change.
    pub(crate) fn appears_unchanged(&self) -> bool {
        let Some((len, mtime)) = self.last_stat else {
            return false;
        };
        let path = self.root.join(&self.member.key.relative_path);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) => {
                let Ok(modified) = metadata.modified() else {
                    return false;
                };
                metadata.len() == len && modified == mtime
            }
            // Missing or unreadable is a change worth handling properly.
            Err(_) => false,
        }
    }

    fn record_stat(&mut self) {
        let path = self.root.join(&self.member.key.relative_path);
        self.last_stat = std::fs::symlink_metadata(&path).ok().and_then(|metadata| {
            metadata
                .modified()
                .ok()
                .map(|mtime| (metadata.len(), mtime))
        });
    }

    /// Read every complete record newer than the checkpoint and decode it.
    pub(crate) fn reconcile(&mut self, adapter: &dyn AgentAdapter, observed_at: i64) -> ObjectPass {
        self.origin.observed_at = observed_at;
        let root = self.root.clone();
        let pass = match &mut self.driver {
            Driver::Append { .. } => self.reconcile_append(adapter, &root),
            Driver::Replace { .. } => self.reconcile_replace(adapter, &root),
        };
        if pass.error.is_none() {
            self.record_stat();
        }
        pass
    }

    fn reconcile_append(&mut self, adapter: &dyn AgentAdapter, root: &Path) -> ObjectPass {
        let Driver::Append { driver, checkpoint } = &self.driver else {
            unreachable!("driver kind was matched by the caller");
        };
        let read = driver.read_confined(
            root,
            &self.member.key.relative_path,
            checkpoint.as_ref(),
            &self.origin,
            false,
        );
        let mut pass = ObjectPass::default();
        let read = match read {
            Ok(read) => read,
            Err(error) => {
                pass.error = Some(error.to_string());
                return pass;
            }
        };
        match read {
            AppendRead::Missing => {
                if self.present {
                    self.present = false;
                    pass.removed = true;
                }
                pass
            }
            AppendRead::RetryTransient => pass,
            AppendRead::Batch {
                items,
                checkpoint: next,
                transition,
                more_available,
                ..
            } => {
                let previous_generation = self.generation;
                if transition.starts_new_generation() && previous_generation != 0 {
                    pass.reset = Some(ObjectReset {
                        old_generation: previous_generation,
                        new_generation: next.generation,
                        reason: transition_reason(transition),
                    });
                    // Reset before replay: the decoder's carried state belongs
                    // to the old generation and must not colour the new one.
                    self.decoder_state = None;
                }
                self.generation = next.generation;
                self.present = true;
                pass.more_available = more_available;

                for item in &items {
                    let AppendItem::Record(record) = item else {
                        continue;
                    };
                    if let Err(error) = self.decode_one(adapter, record, &mut pass) {
                        pass.error = Some(error);
                        break;
                    }
                }
                if pass.error.is_none() {
                    if let Driver::Append { checkpoint, .. } = &mut self.driver {
                        *checkpoint = Some(next);
                    }
                }
                pass
            }
        }
    }

    fn reconcile_replace(&mut self, adapter: &dyn AgentAdapter, root: &Path) -> ObjectPass {
        let Driver::Replace { driver, checkpoint } = &self.driver else {
            unreachable!("driver kind was matched by the caller");
        };
        let read = driver.read_confined(
            root,
            &self.member.key.relative_path,
            checkpoint.as_ref(),
            &self.origin,
            false,
        );
        let mut pass = ObjectPass::default();
        let read = match read {
            Ok(read) => read,
            Err(error) => {
                pass.error = Some(error.to_string());
                return pass;
            }
        };
        match read {
            ReplaceRead::Missing => {
                if self.present {
                    self.present = false;
                    pass.removed = true;
                }
                pass
            }
            ReplaceRead::RetryTransient => pass,
            ReplaceRead::Unchanged { checkpoint: next } => {
                self.present = true;
                if let Driver::Replace { checkpoint, .. } = &mut self.driver {
                    *checkpoint = Some(next);
                }
                pass
            }
            ReplaceRead::Quarantined {
                checkpoint: next, ..
            } => {
                self.present = true;
                if let Driver::Replace { checkpoint, .. } = &mut self.driver {
                    *checkpoint = Some(next);
                }
                pass.error = Some("source document was quarantined by the driver".to_string());
                pass
            }
            ReplaceRead::Removed {
                checkpoint: next, ..
            } => {
                self.present = false;
                pass.removed = true;
                if let Driver::Replace { checkpoint, .. } = &mut self.driver {
                    *checkpoint = Some(next);
                }
                pass
            }
            ReplaceRead::Record {
                record,
                checkpoint: next,
                generation_changed,
            } => {
                let previous_generation = self.generation;
                if generation_changed && previous_generation != 0 {
                    pass.reset = Some(ObjectReset {
                        old_generation: previous_generation,
                        new_generation: record.generation,
                        reason: "document_replaced",
                    });
                    self.decoder_state = None;
                }
                self.generation = record.generation;
                self.present = true;
                if let Err(error) = self.decode_one(adapter, &record, &mut pass) {
                    pass.error = Some(error);
                } else if let Driver::Replace { checkpoint, .. } = &mut self.driver {
                    *checkpoint = Some(next);
                }
                pass
            }
        }
    }

    /// The one decode spine. Both topologies enter adapters here.
    fn decode_one(
        &mut self,
        adapter: &dyn AgentAdapter,
        record: &SourceRecord,
        pass: &mut ObjectPass,
    ) -> Result<(), String> {
        let attempt = decode_record(DecodeRuntimeRequest {
            adapter,
            decoder: &self.decoder,
            object_context: &self.object_context,
            source_access: &DecoderDependenciesDenied,
            record,
            semantic_context: &self.semantic_context,
            decoder_state: self.decoder_state.as_deref(),
            retention: self.retention,
            limits: DecodeRuntimeLimits {
                max_facts: MAX_FACTS_PER_RECORD,
                max_diagnostics: MAX_DIAGNOSTICS_PER_RECORD,
            },
        });
        let decoded = attempt.result.map_err(|error| error.to_string())?;
        if let Some(state) = decoded.next_decoder_state.clone() {
            self.decoder_state = Some(state);
        }
        let byte_start = record.cursor_start.append_offset_value();
        let byte_end = record.cursor_end.append_offset_value();
        for envelope in decoded.batch.facts() {
            let Some(semantic) = envelope.semantic_revision else {
                // RFC 011 legacy facts carry no RFC 012A revision. They belong
                // to durable projections, not to the runtime semantic families.
                continue;
            };
            pass.facts.push(DecodedFact {
                semantic,
                value: envelope.value.clone(),
                native_time: record.source_timestamp_hint,
                observed_at: record.observed_at,
                byte_start,
                byte_end,
                record_digest: *record.payload_hash.as_bytes(),
                generation: record.generation,
            });
        }
        if let RecordMappingDisposition::RetainedUnknown {
            family_hint,
            bounded_evidence,
        } = &decoded.mapping_disposition
        {
            pass.unknown.push(UnknownRecord {
                family_hint: family_hint.clone(),
                observed_bytes: bounded_evidence.observed_bytes,
                source_record_id: bounded_evidence.source_record_id,
                byte_start,
                byte_end,
                record_digest: *record.payload_hash.as_bytes(),
                generation: record.generation,
                observed_at: record.observed_at,
            });
        }
        pass.joins.extend(decoded.scope_join_updates);
        Ok(())
    }
}
