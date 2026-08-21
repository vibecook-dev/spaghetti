//! Store-free RFC 012A coverage membership encoding shared by durable and
//! scoped observation topologies.

use crate::adapter::{CoverageDomain, CoverageMembershipRevision, SemanticContractError};

pub(crate) const DECODE_SOURCE_MEMBERSHIP_PREFIX: &[u8] = b"decode/source-membership/v1";
pub(crate) const USAGE_V2_SOURCE_MEMBERSHIP_PREFIX: &[u8] =
    b"runtime.usage-v2/source-membership/v1";
pub(crate) const CATALOG_V1_SOURCE_MEMBERSHIP_PREFIX: &[u8] =
    b"library.catalog/source-membership/v1";

#[derive(Debug, Clone, Copy)]
pub(crate) struct CoverageMembershipObject<'a> {
    pub stream_key: &'a [u8],
    pub object_key: &'a [u8],
    pub generation: u64,
    pub absent: bool,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum CoverageMembershipEncodingError {
    #[error("coverage membership prefix must not be empty")]
    EmptyPrefix,
    #[error("coverage membership streams must be strictly ordered")]
    StreamsNotCanonical,
    #[error("coverage membership objects must be strictly ordered")]
    ObjectsNotCanonical,
    #[error("coverage domain cannot own source membership in this topology")]
    UnsupportedDomain,
    #[error("coverage membership encoded length is exhausted")]
    LengthExhausted,
    #[error("coverage membership exceeds its encoded byte bound")]
    MembershipTooLarge,
    #[error(transparent)]
    Contract(#[from] SemanticContractError),
}

pub(crate) fn source_membership_prefix(
    domain: &CoverageDomain,
) -> Result<Vec<u8>, CoverageMembershipEncodingError> {
    match domain {
        CoverageDomain::Decode => Ok(DECODE_SOURCE_MEMBERSHIP_PREFIX.to_vec()),
        CoverageDomain::FactFamily { family, version }
            if family == "runtime.usage-v2" && *version == 1 =>
        {
            Ok(USAGE_V2_SOURCE_MEMBERSHIP_PREFIX.to_vec())
        }
        CoverageDomain::FactFamily { family, version }
            if !family.is_empty() && family.trim() == family && *version > 0 =>
        {
            let mut prefix = b"fact-family/source-membership/v1\0".to_vec();
            append_vec_component(&mut prefix, family.as_bytes())?;
            prefix.extend_from_slice(&version.to_be_bytes());
            Ok(prefix)
        }
        CoverageDomain::ProjectionPack { pack, version }
            if pack == "library.catalog" && *version == 1 =>
        {
            Ok(CATALOG_V1_SOURCE_MEMBERSHIP_PREFIX.to_vec())
        }
        CoverageDomain::FactFamily { .. } | CoverageDomain::ProjectionPack { .. } => {
            Err(CoverageMembershipEncodingError::UnsupportedDomain)
        }
    }
}

/// Derive one membership revision through the store-free streaming encoder in
/// `spaghetti-coverage`, without materializing the encoded membership set.
pub(crate) fn derive_coverage_membership_revision(
    prefix: &[u8],
    streams: &[&[u8]],
    objects: &[CoverageMembershipObject<'_>],
) -> Result<CoverageMembershipRevision, CoverageMembershipEncodingError> {
    let digest = spaghetti_coverage::membership_digest(
        prefix,
        streams,
        &objects
            .iter()
            .map(|object| spaghetti_coverage::MembershipObject {
                stream_key: object.stream_key,
                object_key: object.object_key,
                generation: object.generation,
                absent: object.absent,
            })
            .collect::<Vec<_>>(),
    )
    .map_err(|error| match error {
        spaghetti_coverage::CoverageEncodingError::EmptyPrefix => {
            CoverageMembershipEncodingError::EmptyPrefix
        }
        spaghetti_coverage::CoverageEncodingError::StreamsNotCanonical => {
            CoverageMembershipEncodingError::StreamsNotCanonical
        }
        spaghetti_coverage::CoverageEncodingError::ObjectsNotCanonical => {
            CoverageMembershipEncodingError::ObjectsNotCanonical
        }
        spaghetti_coverage::CoverageEncodingError::LengthExhausted => {
            CoverageMembershipEncodingError::LengthExhausted
        }
        spaghetti_coverage::CoverageEncodingError::MembershipTooLarge => {
            CoverageMembershipEncodingError::MembershipTooLarge
        }
    })?;
    Ok(CoverageMembershipRevision::from_digest(digest))
}

fn append_vec_component(
    output: &mut Vec<u8>,
    component: &[u8],
) -> Result<(), CoverageMembershipEncodingError> {
    let length = u64::try_from(component.len())
        .map_err(|_| CoverageMembershipEncodingError::LengthExhausted)?;
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(component);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streaming_membership_matches_the_canonical_component_encoding() {
        let streams = [b"metadata".as_slice(), b"transcript".as_slice()];
        let objects = [
            CoverageMembershipObject {
                stream_key: b"metadata",
                object_key: b"session.json",
                generation: 1,
                absent: true,
            },
            CoverageMembershipObject {
                stream_key: b"transcript",
                object_key: b"session.jsonl",
                generation: 3,
                absent: false,
            },
        ];
        let actual = derive_coverage_membership_revision(
            USAGE_V2_SOURCE_MEMBERSHIP_PREFIX,
            &streams,
            &objects,
        )
        .unwrap();

        let mut encoded = USAGE_V2_SOURCE_MEMBERSHIP_PREFIX.to_vec();
        for component in [
            b"metadata".as_slice(),
            b"transcript".as_slice(),
            b"metadata".as_slice(),
            b"session.json".as_slice(),
        ] {
            encoded.extend_from_slice(&(component.len() as u64).to_be_bytes());
            encoded.extend_from_slice(component);
        }
        encoded.extend_from_slice(&1_u64.to_be_bytes());
        encoded.push(1);
        for component in [b"transcript".as_slice(), b"session.jsonl".as_slice()] {
            encoded.extend_from_slice(&(component.len() as u64).to_be_bytes());
            encoded.extend_from_slice(component);
        }
        encoded.extend_from_slice(&3_u64.to_be_bytes());
        encoded.push(0);

        assert_eq!(
            actual,
            CoverageMembershipRevision::derive(&encoded).unwrap()
        );
    }

    #[test]
    fn membership_requires_canonical_stream_and_object_order() {
        assert!(matches!(
            derive_coverage_membership_revision(
                DECODE_SOURCE_MEMBERSHIP_PREFIX,
                &[b"z", b"a"],
                &[]
            ),
            Err(CoverageMembershipEncodingError::StreamsNotCanonical)
        ));
        let objects = [
            CoverageMembershipObject {
                stream_key: b"stream",
                object_key: b"z",
                generation: 1,
                absent: false,
            },
            CoverageMembershipObject {
                stream_key: b"stream",
                object_key: b"a",
                generation: 1,
                absent: false,
            },
        ];
        assert!(matches!(
            derive_coverage_membership_revision(
                DECODE_SOURCE_MEMBERSHIP_PREFIX,
                &[b"stream"],
                &objects
            ),
            Err(CoverageMembershipEncodingError::ObjectsNotCanonical)
        ));
    }

    #[test]
    fn domain_prefix_preserves_usage_v2_and_separates_other_families() {
        assert_eq!(
            source_membership_prefix(&CoverageDomain::FactFamily {
                family: "runtime.usage-v2".to_string(),
                version: 1,
            })
            .unwrap(),
            USAGE_V2_SOURCE_MEMBERSHIP_PREFIX
        );
        assert_ne!(
            source_membership_prefix(&CoverageDomain::FactFamily {
                family: "runtime.task".to_string(),
                version: 1,
            })
            .unwrap(),
            source_membership_prefix(&CoverageDomain::FactFamily {
                family: "runtime.task".to_string(),
                version: 2,
            })
            .unwrap()
        );
        assert_eq!(
            source_membership_prefix(&CoverageDomain::ProjectionPack {
                pack: "library.catalog".to_string(),
                version: 1,
            })
            .unwrap(),
            CATALOG_V1_SOURCE_MEMBERSHIP_PREFIX
        );
        for domain in [
            CoverageDomain::ProjectionPack {
                pack: "history".to_string(),
                version: 1,
            },
            CoverageDomain::ProjectionPack {
                pack: "library.catalog".to_string(),
                version: 2,
            },
        ] {
            assert!(matches!(
                source_membership_prefix(&domain),
                Err(CoverageMembershipEncodingError::UnsupportedDomain)
            ));
        }
    }
}
