//! Adapter-neutral registry for authorized RFC 012B catalog producers.
//!
//! The common engine resolves this trait by adapter ID and never dispatches to
//! concrete Claude/Codex/Grok modules. Implementations still receive only a
//! borrowed typed authorization and one already-discovered source instance.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::adapter::{AdapterId, SourceCoverageSet, SourceInstance, TypedAccessAuthorization};
use crate::catalog_contract::{CatalogAccessPolicyDigest, CatalogCoveragePlanSource};

use super::catalog_composition::CatalogCompositionError;
use super::catalog_projection::CatalogSourceProjection;

pub(crate) trait CatalogSourceRuntime: Send + Sync + 'static {
    fn adapter_id(&self) -> &'static str;

    fn library_plan_source(
        &self,
        authorization: &TypedAccessAuthorization,
        instance: &SourceInstance,
        access_policy_digest: CatalogAccessPolicyDigest,
    ) -> Result<CatalogCoveragePlanSource, CatalogCompositionError>;

    fn produce_library_projection(
        &self,
        authorization: &TypedAccessAuthorization,
        instance: &SourceInstance,
        access_policy_digest: CatalogAccessPolicyDigest,
        prior_coverage: Option<&SourceCoverageSet>,
    ) -> Result<CatalogSourceProjection, CatalogCompositionError>;
}

#[derive(Default)]
pub(crate) struct CatalogSourceRuntimeRegistryBuilder {
    runtimes: Vec<Arc<dyn CatalogSourceRuntime>>,
}

impl CatalogSourceRuntimeRegistryBuilder {
    pub(crate) fn register<R>(mut self, runtime: R) -> Self
    where
        R: CatalogSourceRuntime,
    {
        self.runtimes.push(Arc::new(runtime));
        self
    }

    pub(crate) fn build(self) -> Result<CatalogSourceRuntimeRegistry, CatalogCompositionError> {
        let mut runtimes = BTreeMap::new();
        for runtime in self.runtimes {
            let adapter_id = AdapterId::new(runtime.adapter_id()).map_err(|_| {
                CatalogCompositionError::invalid("catalog runtime declares an invalid adapter ID")
            })?;
            if runtimes.insert(adapter_id, runtime).is_some() {
                return Err(CatalogCompositionError::invalid(
                    "catalog runtime registry contains a duplicate adapter ID",
                ));
            }
        }
        Ok(CatalogSourceRuntimeRegistry { runtimes })
    }
}

#[derive(Default)]
pub(crate) struct CatalogSourceRuntimeRegistry {
    runtimes: BTreeMap<AdapterId, Arc<dyn CatalogSourceRuntime>>,
}

impl CatalogSourceRuntimeRegistry {
    pub(crate) fn builder() -> CatalogSourceRuntimeRegistryBuilder {
        CatalogSourceRuntimeRegistryBuilder::default()
    }

    pub(crate) fn resolve(
        &self,
        adapter_id: &str,
    ) -> Result<Arc<dyn CatalogSourceRuntime>, CatalogCompositionError> {
        let adapter_id = AdapterId::new(adapter_id).map_err(|_| {
            CatalogCompositionError::invalid("catalog runtime lookup used an invalid adapter ID")
        })?;
        self.runtimes.get(&adapter_id).cloned().ok_or_else(|| {
            CatalogCompositionError::invalid(
                "authorized adapter has no registered catalog source runtime",
            )
        })
    }
}
