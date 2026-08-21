//! Physical crate for RFC 012A coverage membership encoding.
//!
//! Store-free, transport-free, and adapter-free. The observation crate
//! consumes the digest and must not reimplement the framed membership
//! encoding.

use thiserror::Error;

const CONTRACT_LABEL: &[u8] = b"spaghetti/rfc012a/contract\0";
const MEMBERSHIP_DOMAIN: &[u8] = b"coverage-membership";

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
}

pub fn encode_membership(
    prefix: &[u8],
    streams: &[&[u8]],
    objects: &[MembershipObject<'_>],
) -> Result<Vec<u8>, CoverageEncodingError> {
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

    let mut encoded = prefix.to_vec();
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
    let encoded = encode_membership(prefix, streams, objects)?;
    Ok(contract_digest(&encoded))
}

fn append_component(output: &mut Vec<u8>, component: &[u8]) -> Result<(), CoverageEncodingError> {
    let length = u64::try_from(component.len()).map_err(|_| CoverageEncodingError::LengthExhausted)?;
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(component);
    Ok(())
}

fn contract_digest(membership: &[u8]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(CONTRACT_LABEL);
    hasher.update(&(MEMBERSHIP_DOMAIN.len() as u64).to_be_bytes());
    hasher.update(MEMBERSHIP_DOMAIN);
    hasher.update(&1_u64.to_be_bytes());
    hasher.update(&(membership.len() as u64).to_be_bytes());
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
        let encoded = encode_membership(b"decode/source-membership/v1", &[b"transcript"], &objects)
            .unwrap();
        assert_eq!(
            membership_digest(b"decode/source-membership/v1", &[b"transcript"], &objects).unwrap(),
            contract_digest(&encoded)
        );
    }

    #[test]
    fn rejects_unordered_streams() {
        assert_eq!(
            encode_membership(b"prefix", &[b"z", b"a"], &[]).unwrap_err(),
            CoverageEncodingError::StreamsNotCanonical
        );
    }
}
