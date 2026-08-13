//! Versioned compression for large, opaque SQLite payloads.
//!
//! JSON used by projections stays plain JSON. Only payloads that are read as
//! whole blobs cross this boundary, and every compressed column stores its
//! codec beside it so old identity rows remain readable.

use super::EngineError;

pub(crate) const IDENTITY_CODEC: &str = "identity";
pub(crate) const ZSTD_V1_CODEC: &str = "zstd-v1";
/// The owning stream deliberately retained provenance only. This is a stored
/// state marker, not a byte codec: callers must not attempt to reconstruct a
/// payload that policy excluded.
pub(crate) const OMITTED_CODEC: &str = "omitted";

const COMPRESSION_LEVEL: i32 = 3;
const MIN_COMPRESS_BYTES: usize = 256;
const MIN_SAVINGS_BYTES: usize = 32;

#[derive(Debug)]
pub(crate) struct EncodedBlob {
    pub bytes: Vec<u8>,
    pub codec: &'static str,
}

pub(crate) struct BlobEncoder {
    compressor: zstd::bulk::Compressor<'static>,
}

impl BlobEncoder {
    pub(crate) fn new() -> Result<Self, EngineError> {
        let compressor = zstd::bulk::Compressor::new(COMPRESSION_LEVEL).map_err(|error| {
            EngineError::StorageCodec {
                operation: "create zstd compressor",
                detail: error.to_string(),
            }
        })?;
        Ok(Self { compressor })
    }

    pub(crate) fn encode(
        &mut self,
        value: &[u8],
        operation: &'static str,
    ) -> Result<EncodedBlob, EngineError> {
        if value.len() < MIN_COMPRESS_BYTES {
            return Ok(identity(value));
        }
        let compressed =
            self.compressor
                .compress(value)
                .map_err(|error| EngineError::StorageCodec {
                    operation,
                    detail: error.to_string(),
                })?;
        if compressed.len().saturating_add(MIN_SAVINGS_BYTES) >= value.len() {
            return Ok(identity(value));
        }
        Ok(EncodedBlob {
            bytes: compressed,
            codec: ZSTD_V1_CODEC,
        })
    }
}

pub(crate) fn decode(
    codec: &str,
    value: &[u8],
    max_decoded_bytes: usize,
    operation: &'static str,
) -> Result<Vec<u8>, EngineError> {
    match codec {
        OMITTED_CODEC => Err(EngineError::StorageCodec {
            operation,
            detail: "payload was not retained by the source stream policy".to_string(),
        }),
        IDENTITY_CODEC => {
            if value.len() > max_decoded_bytes {
                return Err(EngineError::StorageCodec {
                    operation,
                    detail: format!(
                        "identity payload is {} bytes; maximum is {max_decoded_bytes}",
                        value.len()
                    ),
                });
            }
            Ok(value.to_vec())
        }
        ZSTD_V1_CODEC => zstd::bulk::decompress(value, max_decoded_bytes).map_err(|error| {
            EngineError::StorageCodec {
                operation,
                detail: error.to_string(),
            }
        }),
        other => Err(EngineError::StorageCodec {
            operation,
            detail: format!("unsupported payload codec '{other}'"),
        }),
    }
}

pub(crate) fn omitted() -> EncodedBlob {
    EncodedBlob {
        bytes: Vec::new(),
        codec: OMITTED_CODEC,
    }
}

fn identity(value: &[u8]) -> EncodedBlob {
    EncodedBlob {
        bytes: value.to_vec(),
        codec: IDENTITY_CODEC,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compresses_repeated_payloads_and_round_trips() {
        let input =
            br#"{"message":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}"#
                .repeat(128);
        let mut encoder = BlobEncoder::new().unwrap();
        let encoded = encoder.encode(&input, "test encode").unwrap();
        assert_eq!(encoded.codec, ZSTD_V1_CODEC);
        assert!(encoded.bytes.len() < input.len() / 4);
        assert_eq!(
            decode(encoded.codec, &encoded.bytes, input.len(), "test decode").unwrap(),
            input
        );
    }

    #[test]
    fn keeps_small_payloads_as_identity_and_enforces_decode_bound() {
        let mut encoder = BlobEncoder::new().unwrap();
        let encoded = encoder.encode(b"{}", "test encode").unwrap();
        assert_eq!(encoded.codec, IDENTITY_CODEC);
        assert_eq!(
            decode(encoded.codec, &encoded.bytes, 2, "test decode").unwrap(),
            b"{}"
        );
        assert!(decode(encoded.codec, &encoded.bytes, 1, "test decode").is_err());
    }

    #[test]
    fn omitted_payloads_are_explicit_and_cannot_be_decoded() {
        let encoded = omitted();
        assert_eq!(encoded.codec, OMITTED_CODEC);
        assert!(encoded.bytes.is_empty());
        let error = decode(encoded.codec, &encoded.bytes, 1, "test decode").unwrap_err();
        assert!(error.to_string().contains("was not retained"));
    }
}
