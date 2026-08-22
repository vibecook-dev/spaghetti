//! Strict, store-free transport request for the first public RFC 012D owner.
//!
//! This module is intentionally outside the common scoped-observation kernel:
//! it may parse a bounded JSON transport value, but it cannot weaken the
//! promoted support, scope-program, or native-access checks performed by the
//! configured attachment composition.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::{Deserialize, Deserializer};

use crate::adapter::{ContractVersionOffer, ContractVersionRequest};
use crate::observation_contract::{ObservationContractOffer, ObservationContractRequest};
use crate::scoped_observation::configured_attachment::{
    ScopedConfiguredAttachmentRequest, ScopedConfiguredRootIdentity,
};

pub(crate) const PUBLIC_SCOPED_OBSERVATION_REQUEST_CONTRACT_VERSION: u32 = 1;
pub(crate) const MAX_PUBLIC_SCOPED_OBSERVATION_REQUEST_JSON_BYTES: usize = 256 * 1024;

const MAX_ENCODED_IDENTITY_BYTES: usize = 87_382;
const PORTABLE_FACT_FAMILIES: &[&str] = &[
    "runtime.actor-affiliation",
    "runtime.actor-run",
    "runtime.usage-v2",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid scoped observation request")]
pub(crate) struct PublicScopedObservationRequestError;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PublicScopedObservationRequestWire {
    scoped_observation_request_contract_version: u32,
    adapter_id: String,
    persistence: String,
    scope_mode: String,
    configured_roots: Vec<String>,
    program_id: String,
    known_object_relative_paths: BTreeMap<String, String>,
    root_identity: PublicScopedRootIdentityWire,
    contract_request: ObservationContractRequest,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PublicScopedRootIdentityWire {
    session_identity_key: String,
    #[serde(deserialize_with = "required_nullable_string")]
    root_run_identity_key: Option<String>,
    relation_identity_inputs: BTreeMap<String, String>,
}

fn required_nullable_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer)
}

pub(crate) fn parse_public_scoped_observation_request(
    json: &str,
) -> Result<ScopedConfiguredAttachmentRequest, PublicScopedObservationRequestError> {
    if json.is_empty() || json.len() > MAX_PUBLIC_SCOPED_OBSERVATION_REQUEST_JSON_BYTES {
        return Err(PublicScopedObservationRequestError);
    }
    let wire: PublicScopedObservationRequestWire =
        serde_json::from_str(json).map_err(|_| PublicScopedObservationRequestError)?;
    if wire.scoped_observation_request_contract_version
        != PUBLIC_SCOPED_OBSERVATION_REQUEST_CONTRACT_VERSION
        || wire.persistence != "none"
        || wire.scope_mode != "exact_known_objects"
        || wire.configured_roots.is_empty()
        || wire.known_object_relative_paths.is_empty()
        || wire
            .configured_roots
            .iter()
            .chain(wire.known_object_relative_paths.values())
            .any(|value| invalid_transport_path(value))
    {
        return Err(PublicScopedObservationRequestError);
    }
    validate_portable_contract_request(&wire.contract_request)?;

    let session_identity_key = decode_identity(&wire.root_identity.session_identity_key)?;
    let root_run_identity_key = wire
        .root_identity
        .root_run_identity_key
        .as_deref()
        .map(decode_identity)
        .transpose()?
        .map(Arc::<[u8]>::from);
    let relation_identity_inputs = wire
        .root_identity
        .relation_identity_inputs
        .into_iter()
        .map(|(name, value)| Ok((name, Arc::<[u8]>::from(decode_identity(&value)?))))
        .collect::<Result<BTreeMap<_, _>, PublicScopedObservationRequestError>>()?;
    let identity = ScopedConfiguredRootIdentity::new(
        Arc::<[u8]>::from(session_identity_key),
        relation_identity_inputs,
    )?
    .with_optional_root_run_identity_key(root_run_identity_key);
    let configured_roots = wire
        .configured_roots
        .into_iter()
        .map(PathBuf::from)
        .collect();
    let known_object_relative_paths = wire
        .known_object_relative_paths
        .into_iter()
        .map(|(relation, path)| (relation, PathBuf::from(path)))
        .collect();
    ScopedConfiguredAttachmentRequest::new(
        wire.adapter_id,
        configured_roots,
        wire.program_id,
        known_object_relative_paths,
        identity,
        wire.contract_request,
        public_scoped_observation_contract_offer()?,
    )
    .map_err(|_| PublicScopedObservationRequestError)
}

pub(crate) fn public_scoped_observation_contract_offer(
) -> Result<ObservationContractOffer, PublicScopedObservationRequestError> {
    ObservationContractOffer::new(
        ContractVersionOffer {
            selection_contract_version: 1,
            model_major: 1,
            external_entity_reference_versions: vec![1],
            semantic_revision_reference_versions: vec![1],
            coverage_contract_versions: vec![1],
            fact_family_versions: PORTABLE_FACT_FAMILIES
                .iter()
                .map(|family| ((*family).to_owned(), vec![1]))
                .collect(),
            query_pack_versions: Vec::new(),
            observation_contract_versions: vec![1],
        },
        vec![1],
        vec![1],
        vec![1],
    )
    .map_err(|_| PublicScopedObservationRequestError)
}

fn validate_portable_contract_request(
    request: &ObservationContractRequest,
) -> Result<(), PublicScopedObservationRequestError> {
    let ContractVersionRequest {
        fact_family_versions,
        ..
    } = &request.contract_versions;
    let portable = PORTABLE_FACT_FAMILIES
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    if fact_family_versions.is_empty()
        || fact_family_versions
            .iter()
            .any(|(family, versions)| !portable.contains(family.as_str()) || !versions.contains(&1))
    {
        return Err(PublicScopedObservationRequestError);
    }
    Ok(())
}

fn decode_identity(value: &str) -> Result<Vec<u8>, PublicScopedObservationRequestError> {
    if value.is_empty() || value.len() > MAX_ENCODED_IDENTITY_BYTES {
        return Err(PublicScopedObservationRequestError);
    }
    let decoded = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| PublicScopedObservationRequestError)?;
    if decoded.is_empty()
        || decoded.len() > crate::source::MAX_IDENTITY_VALUE_BYTES
        || URL_SAFE_NO_PAD.encode(&decoded) != value
    {
        return Err(PublicScopedObservationRequestError);
    }
    Ok(decoded)
}

fn invalid_transport_path(value: &str) -> bool {
    value.is_empty()
        || value
            .bytes()
            .any(|byte| byte == 0 || byte.is_ascii_control())
}

impl From<crate::scoped_observation::ScopedObservationAccessError>
    for PublicScopedObservationRequestError
{
    fn from(_: crate::scoped_observation::ScopedObservationAccessError) -> Self {
        Self
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value as JsonValue};

    use super::*;

    fn request_value() -> JsonValue {
        let request = ObservationContractRequest::new(
            ContractVersionRequest {
                selection_contract_version: 1,
                model_major: 1,
                external_entity_reference_version: 1,
                semantic_revision_reference_version: 1,
                coverage_contract_versions: vec![1],
                fact_family_versions: BTreeMap::from([
                    ("runtime.actor-run".to_owned(), vec![1]),
                    ("runtime.usage-v2".to_owned(), vec![1]),
                ]),
                query_pack_versions: None,
                observation_contract_versions: Some(vec![1]),
            },
            vec![1],
            vec![1],
            vec![1],
        )
        .unwrap();
        json!({
            "scoped_observation_request_contract_version": 1,
            "adapter_id": "fixture",
            "persistence": "none",
            "scope_mode": "exact_known_objects",
            "configured_roots": ["/private/fixture-root"],
            "program_id": "fixture-program",
            "known_object_relative_paths": {
                "root-transcript": "sessions/session.jsonl"
            },
            "root_identity": {
                "session_identity_key": URL_SAFE_NO_PAD.encode(b"session-1"),
                "root_run_identity_key": null,
                "relation_identity_inputs": {
                    "native-session-id": URL_SAFE_NO_PAD.encode(b"session-1")
                }
            },
            "contract_request": request
        })
    }

    #[test]
    fn public_scoped_request_is_exact_bounded_and_portable_only() {
        let value = request_value();
        assert!(parse_public_scoped_observation_request(&value.to_string()).is_ok());

        let mut unknown = value.clone();
        unknown["native_path"] = json!("/Users/private/session.jsonl");
        assert_eq!(
            parse_public_scoped_observation_request(&unknown.to_string()).unwrap_err(),
            PublicScopedObservationRequestError
        );

        let mut durable = value.clone();
        durable["persistence"] = json!("sqlite");
        assert!(parse_public_scoped_observation_request(&durable.to_string()).is_err());

        let mut missing_nullable = value.clone();
        missing_nullable["root_identity"]
            .as_object_mut()
            .unwrap()
            .remove("root_run_identity_key");
        assert!(parse_public_scoped_observation_request(&missing_nullable.to_string()).is_err());

        let mut dynamic = value.clone();
        dynamic["scope_mode"] = json!("include_descendants");
        assert!(parse_public_scoped_observation_request(&dynamic.to_string()).is_err());

        let mut unsupported = value.clone();
        unsupported["contract_request"]["contract_versions"]["fact_family_versions"] =
            json!({"runtime.message": [1]});
        assert!(parse_public_scoped_observation_request(&unsupported.to_string()).is_err());
    }

    #[test]
    fn public_scoped_request_rejects_noncanonical_or_oversized_identity_before_decode() {
        let mut padded = request_value();
        padded["root_identity"]["session_identity_key"] = json!("c2Vzc2lvbi0x=");
        assert!(parse_public_scoped_observation_request(&padded.to_string()).is_err());

        let mut oversized = request_value();
        oversized["root_identity"]["session_identity_key"] =
            json!("A".repeat(MAX_ENCODED_IDENTITY_BYTES + 1));
        assert!(parse_public_scoped_observation_request(&oversized.to_string()).is_err());
    }

    #[test]
    fn public_offer_names_only_frozen_transport_families() {
        let offer = public_scoped_observation_contract_offer().unwrap();
        assert_eq!(
            offer
                .contract_versions
                .fact_family_versions
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            PORTABLE_FACT_FAMILIES
        );
    }
}
