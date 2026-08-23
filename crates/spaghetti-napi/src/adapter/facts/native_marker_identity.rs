#![cfg(test)]
//! RFC 012C native-marker fact identity, kept out of the production impl.
//!
//! [`NativeRuntimeMarkerRevisionFact::stable_native_fact_key`] is the RFC 012C
//! contract derivation of a native runtime marker's fact identity. It has no
//! production caller: the Claude transcript adapter commits native markers
//! under its own object key (`claude/runtime_facts.rs` builds
//! `b"runtime.native-marker\0"` + the marker id), so the contract derivation is
//! reached only from the committed `rfc012c-native-marker-v1.json` fixture,
//! which `semantic_contract.rs` both generates from it and validates against
//! it. Living in a `cfg(test)` module records that divergence where a reader
//! meets it, instead of leaving a dead item in the production surface.

use super::*;

impl NativeRuntimeMarkerRevisionFact {
    pub(crate) fn stable_native_fact_key(&self) -> Result<Vec<u8>, AdapterError> {
        self.validate()?;
        let mut encoded = Vec::new();
        encoded.extend_from_slice(b"spaghetti/runtime.native-marker/stable-native-key\0");
        encoded.extend_from_slice(&1_u32.to_be_bytes());
        push_component(&mut encoded, self.session.as_bytes());
        push_component(&mut encoded, self.actor_run.as_bytes());
        encoded.push(native_runtime_marker_kind_tag(&self.value));
        push_component(&mut encoded, self.native_marker_id.as_bytes());
        Ok(encoded)
    }
}
