//! N-API surface for the RFC 012B catalog and the readiness vector.
//!
//! These are the generated bindings for catalog-first startup: the library
//! listing, the single readiness surface, and what one startup committed.
//! Row shapes come straight from `engine::catalog`; nothing here reinterprets
//! them.

use std::sync::Arc;

use napi::bindgen_prelude::{Env, Result, Task};
use napi_derive::napi;

use crate::engine::{
    CatalogEntityResolution, CatalogPageBounds, CatalogProjectPageRequest, CatalogProjectRow,
    CatalogSessionPageRequest, CatalogSessionRow, IdentityConflict, Readiness, ReadinessField,
    SpaghettiEngineCore, DEFAULT_CATALOG_PAGE_LIMIT,
};
use crate::napi_engine::{napi_error, EngineStatus};

/// One field of the readiness vector.
#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineReadinessField {
    /// `pending` | `indexing` | `ready` | `degraded` | `unavailable`.
    pub state: String,
    /// Commit sequence this field's evidence was read at.
    pub committed_at_seq: f64,
    /// Human-readable progress or reason, when there is one to give.
    pub detail: Option<String>,
}

/// The single readiness surface. Each field is independent: `catalog` is
/// routinely `ready` while `history` is still `indexing` and `search` is
/// `pending`, which is exactly what catalog-first startup means.
#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineReadiness {
    pub catalog: EngineReadinessField,
    pub history: EngineReadinessField,
    pub usage: EngineReadinessField,
    pub capabilities: EngineReadinessField,
    pub artifacts: EngineReadinessField,
    pub search: EngineReadinessField,
    pub at_commit_seq: f64,
}

impl From<ReadinessField> for EngineReadinessField {
    fn from(value: ReadinessField) -> Self {
        Self {
            state: value.state.as_str().to_string(),
            committed_at_seq: value.committed_at_seq as f64,
            detail: value.detail,
        }
    }
}

impl From<Readiness> for EngineReadiness {
    fn from(value: Readiness) -> Self {
        Self {
            catalog: value.catalog.into(),
            history: value.history.into(),
            usage: value.usage.into(),
            capabilities: value.capabilities.into(),
            artifacts: value.artifacts.into(),
            search: value.search.into(),
            at_commit_seq: value.at_commit_seq as f64,
        }
    }
}

/// A competing project association retained next to the selected one.
#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineCatalogIdentityConflict {
    pub competing_native_project_key: String,
    pub basis: String,
    pub provenance: String,
}

impl From<IdentityConflict> for EngineCatalogIdentityConflict {
    fn from(value: IdentityConflict) -> Self {
        Self {
            competing_native_project_key: value.competing_native_project_key,
            basis: value.basis,
            provenance: value.provenance,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineCatalogProject {
    pub project_id: String,
    /// Persistable RFC 012A external reference. Stable across restarts.
    pub external_ref: String,
    pub adapter_id: String,
    pub native_project_key: String,
    pub display_name: Option<String>,
    pub display_path: Option<String>,
    /// `discovered` | `transcript_backed` | `hydrated` | `searchable`.
    pub catalog_state: String,
    pub degraded: bool,
    pub degraded_reason: Option<String>,
    pub session_count: f64,
    pub transcript_session_count: f64,
    pub hydrated_session_count: f64,
    pub latest_activity_at: Option<String>,
    pub last_commit_seq: f64,
}

impl From<CatalogProjectRow> for EngineCatalogProject {
    fn from(value: CatalogProjectRow) -> Self {
        Self {
            project_id: value.project_id,
            external_ref: value.external_ref,
            adapter_id: value.adapter_id,
            native_project_key: value.native_project_key,
            display_name: value.display_name,
            display_path: value.display_path,
            catalog_state: value.catalog_state.as_str().to_string(),
            degraded: value.degraded,
            degraded_reason: value.degraded_reason,
            session_count: value.session_count as f64,
            transcript_session_count: value.transcript_session_count as f64,
            hydrated_session_count: value.hydrated_session_count as f64,
            latest_activity_at: value.latest_activity_at,
            last_commit_seq: value.last_commit_seq as f64,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineCatalogSession {
    pub session_id: String,
    pub project_id: String,
    pub external_ref: String,
    pub adapter_id: String,
    pub native_session_id: Option<String>,
    pub title: Option<String>,
    pub catalog_state: String,
    pub degraded: bool,
    pub degraded_reason: Option<String>,
    /// Which native evidence produced the project association.
    pub association_basis: String,
    pub association_quality: String,
    pub association_provenance: String,
    pub native_created_at: Option<String>,
    pub native_updated_at: Option<String>,
    /// The count the agent claims. Absent rather than zero when unknown.
    pub native_message_count: Option<f64>,
    /// Messages actually decoded so far.
    pub decoded_message_count: f64,
    pub transcript_present: bool,
    pub identity_conflicts: Vec<EngineCatalogIdentityConflict>,
    pub last_commit_seq: f64,
}

impl From<CatalogSessionRow> for EngineCatalogSession {
    fn from(value: CatalogSessionRow) -> Self {
        Self {
            session_id: value.session_id,
            project_id: value.project_id,
            external_ref: value.external_ref,
            adapter_id: value.adapter_id,
            native_session_id: value.native_session_id,
            title: value.title,
            catalog_state: value.catalog_state.as_str().to_string(),
            degraded: value.degraded,
            degraded_reason: value.degraded_reason,
            association_basis: value.association_basis,
            association_quality: value.association_quality,
            association_provenance: value.association_provenance,
            native_created_at: value.native_created_at,
            native_updated_at: value.native_updated_at,
            native_message_count: value.native_message_count.map(|count| count as f64),
            decoded_message_count: value.decoded_message_count as f64,
            transcript_present: value.transcript_present,
            identity_conflicts: value
                .identity_conflicts
                .into_iter()
                .map(Into::into)
                .collect(),
            last_commit_seq: value.last_commit_seq as f64,
        }
    }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineCatalogProjectPage {
    pub projects: Vec<EngineCatalogProject>,
    /// Opaque continuation token bound to `atCommitSeq`.
    pub cursor: Option<String>,
    pub at_commit_seq: f64,
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineCatalogSessionPage {
    pub sessions: Vec<EngineCatalogSession>,
    pub cursor: Option<String>,
    pub at_commit_seq: f64,
}

/// Resolution of one persisted external reference. A reference whose evidence
/// was retracted resolves to `retracted`, never to a different live entity.
#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineCatalogResolution {
    /// `project` | `session` | `retracted` | `unknown`.
    pub kind: String,
    pub project: Option<EngineCatalogProject>,
    pub session: Option<EngineCatalogSession>,
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineCatalogPageOptions {
    pub cursor: Option<String>,
    pub limit: Option<u32>,
    /// Restrict to these adapters. Omitted or empty means all of them.
    pub adapter_ids: Option<Vec<String>>,
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineCatalogSessionPageOptions {
    pub project_id: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<u32>,
    pub adapter_ids: Option<Vec<String>>,
}

/// What catalog-first startup committed before history began.
#[napi(object)]
#[derive(Debug, Clone)]
pub struct EngineCatalogStartup {
    pub catalog_projects: f64,
    pub catalog_sessions: f64,
    /// Adapters whose discovery pass could not read their complete surface.
    pub degraded_sources: Vec<String>,
    pub supervisors_started: u32,
    pub history_background: bool,
    pub status: EngineStatus,
}

pub struct CatalogProjectsTask {
    engine: Arc<SpaghettiEngineCore>,
    options: Option<EngineCatalogPageOptions>,
}

impl CatalogProjectsTask {
    pub(crate) fn new(
        engine: Arc<SpaghettiEngineCore>,
        options: Option<EngineCatalogPageOptions>,
    ) -> Self {
        Self { engine, options }
    }
}

impl Task for CatalogProjectsTask {
    type Output = EngineCatalogProjectPage;
    type JsValue = EngineCatalogProjectPage;

    fn compute(&mut self) -> Result<Self::Output> {
        let options = self.options.clone().unwrap_or(EngineCatalogPageOptions {
            cursor: None,
            limit: None,
            adapter_ids: None,
        });
        let page = self
            .engine
            .catalog_projects(CatalogProjectPageRequest {
                bounds: CatalogPageBounds {
                    cursor: options.cursor,
                    limit: options.limit.unwrap_or(DEFAULT_CATALOG_PAGE_LIMIT),
                },
                adapter_ids: options.adapter_ids.unwrap_or_default(),
            })
            .map_err(napi_error)?;
        Ok(EngineCatalogProjectPage {
            projects: page.projects.into_iter().map(Into::into).collect(),
            cursor: page.cursor,
            at_commit_seq: page.at_commit_seq as f64,
        })
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

pub struct CatalogSessionsTask {
    engine: Arc<SpaghettiEngineCore>,
    options: Option<EngineCatalogSessionPageOptions>,
}

impl CatalogSessionsTask {
    pub(crate) fn new(
        engine: Arc<SpaghettiEngineCore>,
        options: Option<EngineCatalogSessionPageOptions>,
    ) -> Self {
        Self { engine, options }
    }
}

impl Task for CatalogSessionsTask {
    type Output = EngineCatalogSessionPage;
    type JsValue = EngineCatalogSessionPage;

    fn compute(&mut self) -> Result<Self::Output> {
        let options = self
            .options
            .clone()
            .unwrap_or(EngineCatalogSessionPageOptions {
                project_id: None,
                cursor: None,
                limit: None,
                adapter_ids: None,
            });
        let page = self
            .engine
            .catalog_sessions(CatalogSessionPageRequest {
                bounds: CatalogPageBounds {
                    cursor: options.cursor,
                    limit: options.limit.unwrap_or(DEFAULT_CATALOG_PAGE_LIMIT),
                },
                project_id: options.project_id,
                adapter_ids: options.adapter_ids.unwrap_or_default(),
            })
            .map_err(napi_error)?;
        Ok(EngineCatalogSessionPage {
            sessions: page.sessions.into_iter().map(Into::into).collect(),
            cursor: page.cursor,
            at_commit_seq: page.at_commit_seq as f64,
        })
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

pub struct CatalogResolveTask {
    engine: Arc<SpaghettiEngineCore>,
    external_ref: String,
}

impl CatalogResolveTask {
    pub(crate) fn new(engine: Arc<SpaghettiEngineCore>, external_ref: String) -> Self {
        Self {
            engine,
            external_ref,
        }
    }
}

impl Task for CatalogResolveTask {
    type Output = EngineCatalogResolution;
    type JsValue = EngineCatalogResolution;

    fn compute(&mut self) -> Result<Self::Output> {
        let resolution = self
            .engine
            .resolve_catalog_entity(self.external_ref.clone())
            .map_err(napi_error)?;
        Ok(match resolution {
            CatalogEntityResolution::LiveProject(project) => EngineCatalogResolution {
                kind: "project".to_string(),
                project: Some((*project).into()),
                session: None,
            },
            CatalogEntityResolution::LiveSession(session) => EngineCatalogResolution {
                kind: "session".to_string(),
                project: None,
                session: Some((*session).into()),
            },
            CatalogEntityResolution::Retracted => EngineCatalogResolution {
                kind: "retracted".to_string(),
                project: None,
                session: None,
            },
            CatalogEntityResolution::Unknown => EngineCatalogResolution {
                kind: "unknown".to_string(),
                project: None,
                session: None,
            },
        })
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

pub struct AwaitObservationStartTask {
    engine: Arc<SpaghettiEngineCore>,
}

impl AwaitObservationStartTask {
    pub(crate) fn new(engine: Arc<SpaghettiEngineCore>) -> Self {
        Self { engine }
    }
}

impl Task for AwaitObservationStartTask {
    type Output = EngineStatus;
    type JsValue = EngineStatus;

    fn compute(&mut self) -> Result<Self::Output> {
        self.engine
            .wait_for_observation_start()
            .map_err(napi_error)?;
        Ok(self.engine.status().into())
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

pub struct ReadinessTask {
    engine: Arc<SpaghettiEngineCore>,
}

impl ReadinessTask {
    pub(crate) fn new(engine: Arc<SpaghettiEngineCore>) -> Self {
        Self { engine }
    }
}

impl Task for ReadinessTask {
    type Output = EngineReadiness;
    type JsValue = EngineReadiness;

    fn compute(&mut self) -> Result<Self::Output> {
        self.engine.readiness().map(Into::into).map_err(napi_error)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}
