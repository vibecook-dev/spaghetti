//! N-API surface for the RFC 012B catalog and the readiness vector.
//!
//! Catalog-first startup: the library listing, the single readiness surface,
//! and what one startup committed. Row shapes come straight from
//! `engine::catalog` and cross as JSON — nothing here restates a field.

use std::sync::Arc;

use napi::bindgen_prelude::{Env, Result, Task};
use napi_derive::napi;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::engine::{
    CatalogEntityResolution, CatalogPageBounds, CatalogProjectPageRequest, CatalogProjectRow,
    CatalogSessionPageRequest, CatalogSessionRow, EngineStatusSnapshot, SpaghettiEngineCore,
    DEFAULT_CATALOG_PAGE_LIMIT,
};
use crate::napi_engine::{encode_json, napi_error};

/// Which entity a persisted external reference names now.
///
/// The engine answers with an enum; JavaScript reads a discriminated object,
/// so the variant is flattened into `kind` here. A reference whose evidence
/// was retracted resolves to `retracted`, never to a different live entity.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CatalogResolution {
    /// `project` | `session` | `retracted` | `unknown`.
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub project: Option<CatalogProjectRow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub session: Option<CatalogSessionRow>,
}

impl From<CatalogEntityResolution> for CatalogResolution {
    fn from(value: CatalogEntityResolution) -> Self {
        let (kind, project, session) = match value {
            CatalogEntityResolution::LiveProject(project) => ("project", Some(*project), None),
            CatalogEntityResolution::LiveSession(session) => ("session", None, Some(*session)),
            CatalogEntityResolution::Retracted => ("retracted", None, None),
            CatalogEntityResolution::Unknown => ("unknown", None, None),
        };
        Self {
            kind: kind.to_string(),
            project,
            session,
        }
    }
}

/// What catalog-first startup committed before history began.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CatalogStartup {
    pub catalog_projects: u64,
    pub catalog_sessions: u64,
    /// Adapters whose discovery pass could not read their complete surface.
    pub degraded_sources: Vec<String>,
    pub supervisors_started: u32,
    pub history_background: bool,
    pub status: EngineStatusSnapshot,
}

/// Page request. Not native output — the caller composes it, and its fields
/// are optional where the resolved engine request's are not.
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
    type Output = String;
    type JsValue = String;

    fn compute(&mut self) -> Result<Self::Output> {
        let options = self.options.clone().unwrap_or(EngineCatalogPageOptions {
            cursor: None,
            limit: None,
            adapter_ids: None,
        });
        self.engine
            .catalog_projects(CatalogProjectPageRequest {
                bounds: CatalogPageBounds {
                    cursor: options.cursor,
                    limit: options.limit.unwrap_or(DEFAULT_CATALOG_PAGE_LIMIT),
                },
                adapter_ids: options.adapter_ids.unwrap_or_default(),
            })
            .map_err(napi_error)
            .and_then(|page| encode_json(&page))
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
    type Output = String;
    type JsValue = String;

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
        self.engine
            .catalog_sessions(CatalogSessionPageRequest {
                bounds: CatalogPageBounds {
                    cursor: options.cursor,
                    limit: options.limit.unwrap_or(DEFAULT_CATALOG_PAGE_LIMIT),
                },
                project_id: options.project_id,
                adapter_ids: options.adapter_ids.unwrap_or_default(),
            })
            .map_err(napi_error)
            .and_then(|page| encode_json(&page))
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
    type Output = String;
    type JsValue = String;

    fn compute(&mut self) -> Result<Self::Output> {
        self.engine
            .resolve_catalog_entity(self.external_ref.clone())
            .map_err(napi_error)
            .and_then(|resolution| encode_json(&CatalogResolution::from(resolution)))
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
    type Output = String;
    type JsValue = String;

    fn compute(&mut self) -> Result<Self::Output> {
        self.engine
            .wait_for_observation_start()
            .map_err(napi_error)?;
        encode_json(&self.engine.status())
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
    type Output = String;
    type JsValue = String;

    fn compute(&mut self) -> Result<Self::Output> {
        self.engine
            .readiness()
            .map_err(napi_error)
            .and_then(|readiness| encode_json(&readiness))
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}
