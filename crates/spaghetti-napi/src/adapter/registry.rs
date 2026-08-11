use std::collections::BTreeMap;
use std::sync::Arc;

use super::{AdapterError, AdapterId, AgentAdapter};

pub struct AdapterRegistryBuilder {
    adapters: Vec<Arc<dyn AgentAdapter>>,
}

impl AdapterRegistryBuilder {
    pub fn new() -> Self {
        Self {
            adapters: Vec::new(),
        }
    }

    pub fn register<A>(mut self, adapter: A) -> Self
    where
        A: AgentAdapter,
    {
        self.adapters.push(Arc::new(adapter));
        self
    }

    pub fn build(self) -> Result<AdapterRegistry, AdapterError> {
        let mut adapters = BTreeMap::new();
        for adapter in self.adapters {
            adapter.manifest().validate()?;
            let id = adapter.manifest().id.clone();
            if adapters.insert(id.clone(), adapter).is_some() {
                return Err(AdapterError::invalid_contract(format!(
                    "duplicate adapter id {id}"
                )));
            }
        }
        Ok(AdapterRegistry { adapters })
    }
}

impl Default for AdapterRegistryBuilder {
    fn default() -> Self {
        Self::new()
    }
}

pub struct AdapterRegistry {
    adapters: BTreeMap<AdapterId, Arc<dyn AgentAdapter>>,
}

impl AdapterRegistry {
    pub fn get(&self, id: &AdapterId) -> Option<&Arc<dyn AgentAdapter>> {
        self.adapters.get(id)
    }

    pub fn len(&self) -> usize {
        self.adapters.len()
    }

    pub fn is_empty(&self) -> bool {
        self.adapters.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use crate::adapter::{
        AdapterManifest, AdapterObjectContext, DecodeContext, DecodeDisposition, DiscoveryContext,
        FactBatch, SourceInstance, SourceInstanceSpec, SourceObjectDescriptor, StreamSpec,
    };
    use crate::source::SourceRecord;

    use super::*;

    struct EmptyAdapter {
        manifest: AdapterManifest,
    }

    impl EmptyAdapter {
        fn new(id: &str) -> Self {
            Self {
                manifest: AdapterManifest {
                    id: AdapterId::new(id).unwrap(),
                    display_name: id.to_string(),
                    adapter_version: "1.0.0".to_string(),
                    contract_version: 1,
                    source_schema_versions: Vec::new(),
                    capabilities: Vec::new(),
                },
            }
        }
    }

    impl AgentAdapter for EmptyAdapter {
        fn manifest(&self) -> &AdapterManifest {
            &self.manifest
        }

        fn discover(
            &self,
            _context: &DiscoveryContext,
        ) -> Result<Vec<SourceInstanceSpec>, AdapterError> {
            Ok(Vec::new())
        }

        fn streams(&self, _instance: &SourceInstance) -> Result<Vec<StreamSpec>, AdapterError> {
            Ok(Vec::new())
        }

        fn bootstrap_object(
            &self,
            _instance: &SourceInstance,
            _object: &SourceObjectDescriptor,
        ) -> Result<AdapterObjectContext, AdapterError> {
            Ok(AdapterObjectContext::empty())
        }

        fn decode(
            &self,
            _context: DecodeContext<'_>,
            _record: &SourceRecord,
            _output: &mut FactBatch,
        ) -> Result<DecodeDisposition, AdapterError> {
            Ok(DecodeDisposition::IgnoredKnown)
        }
    }

    #[test]
    fn registry_rejects_duplicate_open_ids() {
        let result = AdapterRegistryBuilder::new()
            .register(EmptyAdapter::new("same"))
            .register(EmptyAdapter::new("same"))
            .build();
        assert!(result.is_err());
    }

    #[test]
    fn registry_resolves_an_adapter_without_source_specific_dispatch() {
        let registry = AdapterRegistryBuilder::new()
            .register(EmptyAdapter::new("one"))
            .register(EmptyAdapter::new("two"))
            .build()
            .unwrap();
        assert_eq!(registry.len(), 2);
        assert!(registry.get(&AdapterId::new("two").unwrap()).is_some());
    }
}
