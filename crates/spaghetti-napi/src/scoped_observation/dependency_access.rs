//! Declared decoder-dependency access for one scoped observation pass.
//!
//! The adapter-facing [`SourceAccess`] trait cannot name a scope relation, so
//! this mediator resolves an object request only when its access-root and
//! canonical relative-path key identify exactly one host-granted `KnownObject`
//! relation. Every native read still flows through the attachment's existing
//! authorized pass and budget. Database queries and listings remain denied
//! until their distinct RFC 012A relation primitives are composed here.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;

use crate::adapter::{
    AdapterError, AdapterErrorClass, DependencyRevision, SourceAccess, SourceObjectList,
    SourceObjectListRequest, SourceQuery, SourceRows, SourceSnapshot,
};
use crate::source::{confined_relative_path_key, AccessPhase, Revision, ScopeIdentityInput};

use super::{
    ScopedKnownObjectReadRequest, ScopedObjectRead, ScopedObservationAccessError,
    ScopedObservationAccessPass, ScopedObservationAppendPassRequest, ScopedSourceFailureClass,
};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ScopedDependencyLocator {
    access_root: String,
    object_key: Vec<u8>,
}

#[derive(Clone, Copy)]
struct ScopedDependencyBinding<'a> {
    relation_id: &'a str,
    identity_inputs: &'a [ScopeIdentityInput<'a>],
    parent_token: Option<crate::source::AccessObjectToken>,
    depth: u32,
    max_bytes: u64,
    source_instance_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ScopedDependencyObservation {
    revision: DependencyRevision,
    oversized: bool,
    max_bytes: usize,
}

/// Decoder-local, non-serializable access authority for one exact scoped pass.
/// Debug output deliberately exposes only counts and the access phase.
pub(super) struct ScopedDecoderDependencyAccess<'a> {
    pass: &'a ScopedObservationAccessPass,
    bindings: BTreeMap<ScopedDependencyLocator, ScopedDependencyBinding<'a>>,
    max_access_root_bytes: usize,
    max_relative_path_bytes: usize,
    phase: AccessPhase,
    observations: Mutex<BTreeMap<ScopedDependencyLocator, ScopedDependencyObservation>>,
}

impl std::fmt::Debug for ScopedDecoderDependencyAccess<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ScopedDecoderDependencyAccess")
            .field("pass_id", &self.pass.pass_id())
            .field("declared_relation_count", &self.bindings.len())
            .field("phase", &self.phase)
            .field(
                "observed_dependency_count",
                &self
                    .observations
                    .lock()
                    .map(|observations| observations.len())
                    .unwrap_or_default(),
            )
            .finish_non_exhaustive()
    }
}

impl<'a> ScopedDecoderDependencyAccess<'a> {
    pub(super) fn from_requests(
        pass: &'a ScopedObservationAccessPass,
        requests: &'a [ScopedObservationAppendPassRequest<'a>],
        phase: AccessPhase,
    ) -> Result<Self, ScopedObservationAccessError> {
        if pass.released
            || pass.state.closed.load(std::sync::atomic::Ordering::Acquire)
            || !pass
                .state
                .pass_active
                .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(ScopedObservationAccessError::Closed);
        }
        if requests.len() != pass.known_objects.len() {
            return Err(invalid_dependency_binding());
        }

        let mut bindings = BTreeMap::new();
        let mut relation_ids = BTreeMap::new();
        let mut max_access_root_bytes = 0;
        let mut max_relative_path_bytes = 0;
        for request in requests {
            if request.max_bytes == 0 || relation_ids.insert(request.relation_id, ()).is_some() {
                return Err(invalid_dependency_binding());
            }
            let grant = pass
                .known_objects
                .get(request.relation_id)
                .ok_or_else(invalid_dependency_binding)?;
            let object_key = confined_relative_path_key(&grant.relative_path)
                .map_err(|_| invalid_dependency_binding())?;
            max_access_root_bytes = max_access_root_bytes.max(grant.access_root.len());
            max_relative_path_bytes = max_relative_path_bytes
                .max(grant.relative_path.as_os_str().as_encoded_bytes().len());
            let locator = ScopedDependencyLocator {
                access_root: grant.access_root.clone(),
                object_key,
            };
            let binding = ScopedDependencyBinding {
                relation_id: request.relation_id,
                identity_inputs: request.identity_inputs,
                parent_token: request.parent_token,
                depth: request.depth,
                max_bytes: request.max_bytes,
                source_instance_id: request.origin.source_instance_id,
            };
            if bindings.insert(locator, binding).is_some() {
                return Err(ScopedObservationAccessError::InvalidGrant(
                    "scoped decoder dependency locator is ambiguous".to_string(),
                ));
            }
        }
        if relation_ids.len() != pass.known_objects.len()
            || pass
                .known_objects
                .keys()
                .any(|relation_id| !relation_ids.contains_key(relation_id.as_str()))
        {
            return Err(invalid_dependency_binding());
        }

        Ok(Self {
            pass,
            bindings,
            max_access_root_bytes,
            max_relative_path_bytes,
            phase,
            observations: Mutex::new(BTreeMap::new()),
        })
    }

    /// Re-read every dependency used by this decode before decoder state is
    /// staged. A changed or unstable dependency makes the whole decoded batch
    /// transient; no primary checkpoint or decoder state can then advance.
    pub(super) fn revalidate(&self) -> Result<(), AdapterError> {
        let observations = self
            .observations
            .lock()
            .map_err(|_| dependency_lock_error())?
            .clone();
        for (locator, expected) in observations {
            let binding = self
                .bindings
                .get(&locator)
                .ok_or_else(dependency_invalid_contract)?;
            let current = self.read_binding(
                binding,
                &locator,
                expected.max_bytes,
                AccessPhase::Revalidation,
            )?;
            if current.revision != expected.revision || current.oversized != expected.oversized {
                return Err(dependency_changed());
            }
        }
        Ok(())
    }

    fn locator(
        &self,
        root_name: &str,
        relative_path: &Path,
    ) -> Result<ScopedDependencyLocator, AdapterError> {
        if root_name.is_empty()
            || root_name.len() > self.max_access_root_bytes
            || relative_path.as_os_str().as_encoded_bytes().is_empty()
            || relative_path.as_os_str().as_encoded_bytes().len() > self.max_relative_path_bytes
        {
            return Err(dependency_invalid_contract());
        }
        let object_key =
            confined_relative_path_key(relative_path).map_err(|_| dependency_invalid_contract())?;
        Ok(ScopedDependencyLocator {
            access_root: root_name.to_string(),
            object_key,
        })
    }

    fn read_binding(
        &self,
        binding: &ScopedDependencyBinding<'_>,
        locator: &ScopedDependencyLocator,
        max_bytes: usize,
        phase: AccessPhase,
    ) -> Result<SourceSnapshot, AdapterError> {
        let max_bytes_u64 = u64::try_from(max_bytes).map_err(|_| dependency_invalid_contract())?;
        if max_bytes == 0 || max_bytes_u64 > binding.max_bytes {
            return Err(dependency_invalid_contract());
        }
        let read = self
            .pass
            .read_known_object(ScopedKnownObjectReadRequest {
                relation_id: binding.relation_id,
                identity_inputs: binding.identity_inputs,
                phase,
                parent_token: binding.parent_token,
                depth: binding.depth,
                max_bytes: max_bytes_u64,
            })
            .map_err(scoped_access_error)?;
        let (payload, revision, oversized) = match read {
            ScopedObjectRead::Available { bytes, revision } => (Some(bytes), revision, false),
            ScopedObjectRead::Unavailable => (None, Revision::missing_dependency(), false),
            ScopedObjectRead::Oversized { revision, .. } => (None, revision, true),
            ScopedObjectRead::Unstable => return Err(dependency_changed()),
        };
        Ok(SourceSnapshot {
            payload,
            revision: DependencyRevision {
                source_instance_id: binding.source_instance_id,
                root_name: locator.access_root.clone(),
                object_key: locator.object_key.clone(),
                revision: *revision.as_bytes(),
            },
            oversized,
        })
    }

    fn record_observation(
        &self,
        locator: ScopedDependencyLocator,
        snapshot: &SourceSnapshot,
        max_bytes: usize,
    ) -> Result<(), AdapterError> {
        let mut observations = self
            .observations
            .lock()
            .map_err(|_| dependency_lock_error())?;
        let observed = ScopedDependencyObservation {
            revision: snapshot.revision.clone(),
            oversized: snapshot.oversized,
            max_bytes,
        };
        match observations.get(&locator) {
            Some(existing) if existing.max_bytes != max_bytes => Err(dependency_invalid_contract()),
            Some(existing) if existing != &observed => Err(dependency_changed()),
            Some(_) => Ok(()),
            None => {
                observations.insert(locator, observed);
                Ok(())
            }
        }
    }
}

impl SourceAccess for ScopedDecoderDependencyAccess<'_> {
    fn read_object(
        &self,
        root_name: &str,
        relative_path: &Path,
        max_bytes: usize,
    ) -> Result<SourceSnapshot, AdapterError> {
        let locator = self.locator(root_name, relative_path)?;
        let binding = self
            .bindings
            .get(&locator)
            .ok_or_else(dependency_invalid_contract)?;
        if let Some(existing) = self
            .observations
            .lock()
            .map_err(|_| dependency_lock_error())?
            .get(&locator)
        {
            if existing.max_bytes != max_bytes {
                return Err(dependency_invalid_contract());
            }
        }
        let snapshot = self.read_binding(binding, &locator, max_bytes, self.phase)?;
        self.record_observation(locator, &snapshot, max_bytes)?;
        Ok(snapshot)
    }

    fn query_source_db(&self, _query: &SourceQuery) -> Result<SourceRows, AdapterError> {
        Err(dependency_unsupported_primitive())
    }

    fn list_objects(
        &self,
        _request: &SourceObjectListRequest,
    ) -> Result<SourceObjectList, AdapterError> {
        Err(dependency_unsupported_primitive())
    }
}

fn invalid_dependency_binding() -> ScopedObservationAccessError {
    ScopedObservationAccessError::InvalidGrant(
        "scoped decoder dependency bindings do not match the exact pass relation set".to_string(),
    )
}

fn scoped_access_error(error: ScopedObservationAccessError) -> AdapterError {
    let class = match error {
        ScopedObservationAccessError::Closed
        | ScopedObservationAccessError::Source(
            ScopedSourceFailureClass::Unstable
            | ScopedSourceFailureClass::Database
            | ScopedSourceFailureClass::Io,
        ) => AdapterErrorClass::Transient,
        _ => AdapterErrorClass::InvalidContract,
    };
    AdapterError::new(
        class,
        "scoped_dependency_access_failed",
        "scoped decoder dependency access failed",
    )
}

fn dependency_invalid_contract() -> AdapterError {
    AdapterError::new(
        AdapterErrorClass::InvalidContract,
        "scoped_dependency_access_undeclared",
        "decoder requested dependency access without one exact declared known-object relation",
    )
}

fn dependency_unsupported_primitive() -> AdapterError {
    AdapterError::new(
        AdapterErrorClass::InvalidContract,
        "scoped_dependency_primitive_unsupported",
        "decoder requested a scoped dependency primitive that is not composed",
    )
}

fn dependency_changed() -> AdapterError {
    AdapterError::new(
        AdapterErrorClass::Transient,
        "scoped_dependency_changed",
        "scoped decoder dependency changed before state staging",
    )
}

fn dependency_lock_error() -> AdapterError {
    AdapterError::new(
        AdapterErrorClass::AdapterFatal,
        "scoped_dependency_lock",
        "scoped decoder dependency tracking failed",
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tempfile::TempDir;

    use crate::adapter::{
        fixture_scoped_access_request, supported_fixture_registry_with_scope, SourceQueryBounds,
    };
    use crate::scoped_observation::{
        ScopedKnownObjectGrant, ScopedObservationAccessHost, ScopedObservationAppendPassRequest,
    };
    use crate::source::{RecordOrigin, ScopeIdentityInput, SourceMediaType, SqliteQuerySpec};

    use super::*;

    const DEPENDENCY_SCOPE_DOCUMENT: &[u8] = br#"{"schema_version":1,"declaration_id":"fixture-scope","adapter_id":"fixture","ads_id":"fixture-ads","status":"promoted","roots":["root"],"programs":[{"program_id":"observe-session","root_entity_kind":"session","root_relation_id":"root-object","relations":[{"relation_id":"root-object","primitive":"KnownObject","access_root":"root","locator":"known-object","identity_inputs":["native-session-id"],"bounds":{"max_fan_out":1,"max_depth":1,"max_objects":1,"max_bytes":1024,"max_rows":0},"unavailable_behavior":"record_unavailable","claim_refs":["scope-evidence"]},{"relation_id":"decoder-sidecar","primitive":"KnownObject","access_root":"root","locator":"decoder-sidecar","identity_inputs":["native-session-id"],"bounds":{"max_fan_out":1,"max_depth":1,"max_objects":1,"max_bytes":1024,"max_rows":0},"unavailable_behavior":"record_unavailable","claim_refs":["scope-evidence"]}],"claim_refs":["scope-evidence"]}],"blockers":[],"claim_refs":["scope-evidence"]}"#;

    #[test]
    fn declared_dependency_access_is_bounded_redacted_and_state_distinct() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("private-dependency-root");
        std::fs::create_dir_all(&root).unwrap();
        let sidecar = root.join("sidecar.json");
        std::fs::write(&sidecar, b"stable").unwrap();
        let registry = supported_fixture_registry_with_scope(DEPENDENCY_SCOPE_DOCUMENT);
        let mut access_request = fixture_scoped_access_request(root.clone());
        access_request.known_objects.push(ScopedKnownObjectGrant {
            relation_id: "decoder-sidecar".to_string(),
            scope_root: false,
            access_root: "root".to_string(),
            locator_id: "decoder-sidecar".to_string(),
            root: root.clone(),
            relative_path: "sidecar.json".into(),
        });
        let host = ScopedObservationAccessHost::authorize(&registry, access_request).unwrap();
        let identity = [ScopeIdentityInput {
            name: "native-session-id",
            value: b"dependency-test-session",
        }];
        let root_origin = RecordOrigin {
            source_instance_id: 7,
            stream_id: 8,
            object_id: 9,
            observed_at: 10,
            source_timestamp_hint: None,
            media_type: SourceMediaType::new("application/json").unwrap(),
        };
        let dependency_origin = RecordOrigin {
            object_id: 11,
            ..root_origin.clone()
        };
        let requests = [
            ScopedObservationAppendPassRequest {
                relation_id: "root-object",
                identity_inputs: &identity,
                parent_token: None,
                depth: 1,
                max_bytes: 64,
                origin: &root_origin,
                force_contract_replay: false,
            },
            ScopedObservationAppendPassRequest {
                relation_id: "decoder-sidecar",
                identity_inputs: &identity,
                parent_token: None,
                depth: 1,
                max_bytes: 64,
                origin: &dependency_origin,
                force_contract_replay: false,
            },
        ];

        let pass = host.begin_pass().unwrap();
        let access =
            ScopedDecoderDependencyAccess::from_requests(&pass, &requests, AccessPhase::Initial)
                .unwrap();
        let debug = format!("{access:?}");
        assert!(debug.contains("declared_relation_count: 2"));
        assert!(!debug.contains("sidecar.json"));
        assert!(!debug.contains("private-dependency-root"));

        for denied in [
            access
                .read_object("root", Path::new("sidecar.json"), 65)
                .unwrap_err(),
            access
                .read_object("/Users/alice/private", Path::new("sidecar.json"), 16)
                .unwrap_err(),
            access
                .read_object("root", Path::new("../private/sidecar.json"), 16)
                .unwrap_err(),
            access
                .read_object("root", Path::new("other.json"), 16)
                .unwrap_err(),
            access
                .read_object(
                    "root",
                    &PathBuf::from("x".repeat(access.max_relative_path_bytes + 1)),
                    16,
                )
                .unwrap_err(),
        ] {
            let rendered = denied.to_string();
            assert_eq!(denied.class, AdapterErrorClass::InvalidContract);
            assert!(!rendered.contains("/Users"));
            assert!(!rendered.contains("alice"));
            assert!(!rendered.contains("private"));
        }
        let query = SourceQuery {
            root_name: "root".to_string(),
            relative_path: "sidecar.db".into(),
            query: SqliteQuerySpec {
                name: "fixture".to_string(),
                sql: "SELECT 1 AS key".to_string(),
                key_columns: vec!["key".to_string()],
            },
            bounds: SourceQueryBounds::default(),
        };
        assert_eq!(
            access.query_source_db(&query).unwrap_err().class,
            AdapterErrorClass::InvalidContract
        );
        assert_eq!(
            access
                .list_objects(&SourceObjectListRequest {
                    root_name: "root".to_string(),
                    include: vec!["*".to_string()],
                    exclude: Vec::new(),
                    max_entries: 1,
                })
                .unwrap_err()
                .class,
            AdapterErrorClass::InvalidContract
        );

        let stable = access
            .read_object("root", Path::new("sidecar.json"), 16)
            .unwrap();
        assert_eq!(stable.payload.as_deref(), Some(b"stable".as_slice()));
        assert_eq!(stable.revision.source_instance_id, 7);
        assert_eq!(stable.revision.root_name, "root");
        assert_eq!(
            stable.revision.object_key,
            confined_relative_path_key(Path::new("sidecar.json")).unwrap()
        );
        assert_eq!(
            stable.revision.revision,
            *Revision::digest(b"stable").as_bytes()
        );
        access.revalidate().unwrap();
        drop(access);
        drop(pass);

        std::fs::remove_file(&sidecar).unwrap();
        let pass = host.begin_pass().unwrap();
        let access =
            ScopedDecoderDependencyAccess::from_requests(&pass, &requests, AccessPhase::Initial)
                .unwrap();
        let missing = access
            .read_object("root", Path::new("sidecar.json"), 16)
            .unwrap();
        assert!(missing.payload.is_none());
        assert!(!missing.oversized);
        assert_eq!(
            missing.revision.revision,
            *Revision::missing_dependency().as_bytes()
        );
        access.revalidate().unwrap();
        drop(access);
        drop(pass);

        std::fs::write(&sidecar, b"0123456789abcdefg").unwrap();
        let pass = host.begin_pass().unwrap();
        let access =
            ScopedDecoderDependencyAccess::from_requests(&pass, &requests, AccessPhase::Initial)
                .unwrap();
        let oversized = access
            .read_object("root", Path::new("sidecar.json"), 16)
            .unwrap();
        assert!(oversized.payload.is_none());
        assert!(oversized.oversized);
        assert_ne!(oversized.revision.revision, missing.revision.revision);
        access.revalidate().unwrap();

        let mut ambiguous_request = fixture_scoped_access_request(root.clone());
        ambiguous_request
            .known_objects
            .push(ScopedKnownObjectGrant {
                relation_id: "decoder-sidecar".to_string(),
                scope_root: false,
                access_root: "root".to_string(),
                locator_id: "decoder-sidecar".to_string(),
                root,
                relative_path: "session.jsonl".into(),
            });
        let ambiguous_host =
            ScopedObservationAccessHost::authorize(&registry, ambiguous_request).unwrap();
        let ambiguous_pass = ambiguous_host.begin_pass().unwrap();
        assert!(matches!(
            ScopedDecoderDependencyAccess::from_requests(
                &ambiguous_pass,
                &requests,
                AccessPhase::Initial,
            ),
            Err(ScopedObservationAccessError::InvalidGrant(message))
                if message == "scoped decoder dependency locator is ambiguous"
        ));
    }
}
