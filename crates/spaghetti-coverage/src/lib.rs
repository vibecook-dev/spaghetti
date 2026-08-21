//! Physical crate for RFC 012A coverage membership encoding.
//!
//! Store-free, transport-free, and adapter-free. The observation crate
//! consumes the digest and must not reimplement the framed membership
//! encoding.

use thiserror::Error;

const CONTRACT_LABEL: &[u8] = b"spaghetti/rfc012a/contract\0";
const MEMBERSHIP_DOMAIN: &[u8] = b"coverage-membership";
pub const MAX_ENCODED_MEMBERSHIP_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MembershipObject<'a> {
    pub stream_key: &'a [u8],
    pub object_key: &'a [u8],
    pub generation: u64,
    pub absent: bool,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CoverageEncodingError {
    #[error("coverage membership prefix must not be empty")]
    EmptyPrefix,
    #[error("coverage membership streams must be strictly ordered")]
    StreamsNotCanonical,
    #[error("coverage membership objects must be strictly ordered")]
    ObjectsNotCanonical,
    #[error("coverage membership encoded length is exhausted")]
    LengthExhausted,
    #[error("coverage membership exceeds its encoded byte bound")]
    MembershipTooLarge,
}

pub fn encode_membership(
    prefix: &[u8],
    streams: &[&[u8]],
    objects: &[MembershipObject<'_>],
) -> Result<Vec<u8>, CoverageEncodingError> {
    let encoded_bytes = validate_membership(prefix, streams, objects)?;
    let mut encoded = Vec::with_capacity(encoded_bytes);
    encoded.extend_from_slice(prefix);
    for stream in streams {
        append_component(&mut encoded, stream)?;
    }
    for object in objects {
        append_component(&mut encoded, object.stream_key)?;
        append_component(&mut encoded, object.object_key)?;
        encoded.extend_from_slice(&object.generation.to_be_bytes());
        encoded.push(u8::from(object.absent));
    }
    Ok(encoded)
}

pub fn membership_digest(
    prefix: &[u8],
    streams: &[&[u8]],
    objects: &[MembershipObject<'_>],
) -> Result<[u8; 32], CoverageEncodingError> {
    let encoded_bytes = validate_membership(prefix, streams, objects)?;
    let mut hasher = begin_contract_digest(encoded_bytes);
    hasher.update(prefix);
    for stream in streams {
        update_component(&mut hasher, stream)?;
    }
    for object in objects {
        update_component(&mut hasher, object.stream_key)?;
        update_component(&mut hasher, object.object_key)?;
        hasher.update(&object.generation.to_be_bytes());
        hasher.update(&[u8::from(object.absent)]);
    }
    Ok(*hasher.finalize().as_bytes())
}

fn validate_membership(
    prefix: &[u8],
    streams: &[&[u8]],
    objects: &[MembershipObject<'_>],
) -> Result<usize, CoverageEncodingError> {
    if prefix.is_empty() {
        return Err(CoverageEncodingError::EmptyPrefix);
    }
    if !streams.windows(2).all(|pair| pair[0] < pair[1]) {
        return Err(CoverageEncodingError::StreamsNotCanonical);
    }
    if !objects.windows(2).all(|pair| {
        (pair[0].stream_key, pair[0].object_key, pair[0].generation)
            < (pair[1].stream_key, pair[1].object_key, pair[1].generation)
    }) {
        return Err(CoverageEncodingError::ObjectsNotCanonical);
    }

    let mut encoded_bytes = prefix.len();
    for stream in streams {
        encoded_bytes = encoded_bytes
            .checked_add(8)
            .and_then(|value| value.checked_add(stream.len()))
            .ok_or(CoverageEncodingError::LengthExhausted)?;
    }
    for object in objects {
        encoded_bytes = encoded_bytes
            .checked_add(8)
            .and_then(|value| value.checked_add(object.stream_key.len()))
            .and_then(|value| value.checked_add(8))
            .and_then(|value| value.checked_add(object.object_key.len()))
            .and_then(|value| value.checked_add(9))
            .ok_or(CoverageEncodingError::LengthExhausted)?;
    }
    if encoded_bytes > MAX_ENCODED_MEMBERSHIP_BYTES {
        return Err(CoverageEncodingError::MembershipTooLarge);
    }
    Ok(encoded_bytes)
}

fn append_component(output: &mut Vec<u8>, component: &[u8]) -> Result<(), CoverageEncodingError> {
    let length =
        u64::try_from(component.len()).map_err(|_| CoverageEncodingError::LengthExhausted)?;
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(component);
    Ok(())
}

fn update_component(
    hasher: &mut blake3::Hasher,
    component: &[u8],
) -> Result<(), CoverageEncodingError> {
    let length =
        u64::try_from(component.len()).map_err(|_| CoverageEncodingError::LengthExhausted)?;
    hasher.update(&length.to_be_bytes());
    hasher.update(component);
    Ok(())
}

fn begin_contract_digest(membership_bytes: usize) -> blake3::Hasher {
    let mut hasher = blake3::Hasher::new();
    hasher.update(CONTRACT_LABEL);
    hasher.update(&(MEMBERSHIP_DOMAIN.len() as u64).to_be_bytes());
    hasher.update(MEMBERSHIP_DOMAIN);
    hasher.update(&1_u64.to_be_bytes());
    hasher.update(&(membership_bytes as u64).to_be_bytes());
    hasher
}

#[cfg(test)]
fn contract_digest(membership: &[u8]) -> [u8; 32] {
    let mut hasher = begin_contract_digest(membership.len());
    hasher.update(membership);
    *hasher.finalize().as_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_matches_one_shot_contract_hash_of_the_encoded_set() {
        let objects = [MembershipObject {
            stream_key: b"transcript",
            object_key: b"session.jsonl",
            generation: 3,
            absent: false,
        }];
        let encoded =
            encode_membership(b"decode/source-membership/v1", &[b"transcript"], &objects).unwrap();
        assert_eq!(
            membership_digest(b"decode/source-membership/v1", &[b"transcript"], &objects).unwrap(),
            contract_digest(&encoded)
        );
    }

    #[test]
    fn streaming_digest_scales_past_the_legacy_component_bound() {
        let object_key = vec![b'x'; 70 * 1024];
        let objects = [MembershipObject {
            stream_key: b"transcript",
            object_key: &object_key,
            generation: 3,
            absent: false,
        }];
        let digest =
            membership_digest(b"decode/source-membership/v1", &[b"transcript"], &objects).unwrap();
        assert_ne!(digest, [0_u8; 32]);
    }

    #[test]
    fn rejects_unordered_streams() {
        assert_eq!(
            encode_membership(b"prefix", &[b"z", b"a"], &[]).unwrap_err(),
            CoverageEncodingError::StreamsNotCanonical
        );
    }

    #[test]
    fn digest_is_stable_for_a_known_membership_set() {
        let objects = [MembershipObject {
            stream_key: b"transcript",
            object_key: b"session.jsonl",
            generation: 3,
            absent: false,
        }];
        let digest =
            membership_digest(b"decode/source-membership/v1", &[b"transcript"], &objects).unwrap();
        assert_eq!(
            digest,
            membership_digest(b"decode/source-membership/v1", &[b"transcript"], &objects).unwrap()
        );
        assert_ne!(digest, [0_u8; 32]);
    }

    #[test]
    fn empty_prefix_is_rejected() {
        assert_eq!(
            encode_membership(b"", &[], &[]).unwrap_err(),
            CoverageEncodingError::EmptyPrefix
        );
    }
}
