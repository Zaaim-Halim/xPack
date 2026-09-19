//! SHA-256 helpers.

use std::io::Read;

use sha2::{Digest, Sha256};
use xpack_core::digest::Sha256Digest;
use xpack_core::{Error, Result};

/// Buffer size used when streaming. 64 KiB keeps large payload files off the
/// heap while staying well above the syscall-per-byte regime.
const CHUNK: usize = 64 * 1024;

/// Hashes an in-memory slice.
pub fn sha256(data: &[u8]) -> Sha256Digest {
    let mut hasher = Sha256::new();
    hasher.update(data);
    Sha256Digest::from_bytes(hasher.finalize().into())
}

/// Hashes a stream without buffering it, returning the digest and byte count.
///
/// Packages may bundle a multi-hundred-megabyte runtime, so nothing in the
/// verification path is ever read fully into memory.
pub fn sha256_reader<R: Read>(reader: &mut R) -> Result<(Sha256Digest, u64)> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; CHUNK];
    let mut total: u64 = 0;

    loop {
        let read = reader.read(&mut buffer).map_err(Error::BareIo)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        total += read as u64;
    }

    Ok((Sha256Digest::from_bytes(hasher.finalize().into()), total))
}

/// An incremental SHA-256 hasher.
///
/// Extraction hashes bytes as it writes them, so the digest is computed over
/// exactly what lands on disk. Hashing the source separately from writing it
/// would leave a window in which the two could differ.
#[derive(Default)]
pub struct Hasher(Sha256);

impl Hasher {
    /// Starts a new hash.
    pub fn new() -> Self {
        Self(Sha256::new())
    }

    /// Feeds more bytes into the hash.
    pub fn update(&mut self, data: &[u8]) {
        self.0.update(data);
    }

    /// Consumes the hasher and returns the digest.
    pub fn finish(self) -> Sha256Digest {
        Sha256Digest::from_bytes(self.0.finalize().into())
    }
}

impl std::fmt::Debug for Hasher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Hasher(..)")
    }
}

/// Compares an actual digest against the expected one, in constant time.
///
/// `subject` names the artefact so the resulting error is actionable — the
/// first question after a verification failure is always *which file*.
pub fn verify_digest(subject: &str, actual: Sha256Digest, expected: Sha256Digest) -> Result<()> {
    if actual == expected {
        Ok(())
    } else {
        Err(Error::Integrity(format!(
            "{subject}: sha-256 mismatch (expected {expected}, computed {actual})"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // RFC 6234 / NIST test vector for the empty string.
    const EMPTY: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    // Well-known vector for "abc".
    const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    #[test]
    fn matches_published_test_vectors() {
        assert_eq!(sha256(b"").to_hex(), EMPTY);
        assert_eq!(sha256(b"abc").to_hex(), ABC);
    }

    #[test]
    fn streaming_agrees_with_in_memory_across_chunk_boundaries() {
        // Spans several 64 KiB reads plus a partial final chunk.
        let data: Vec<u8> = (0..(CHUNK * 2 + 7)).map(|i| u8::try_from(i % 251).unwrap()).collect();
        let (digest, len) = sha256_reader(&mut data.as_slice()).unwrap();
        assert_eq!(digest, sha256(&data));
        assert_eq!(len, data.len() as u64);
    }

    #[test]
    fn incremental_hashing_agrees_with_one_shot() {
        let mut h = Hasher::new();
        h.update(b"a");
        h.update(b"bc");
        assert_eq!(h.finish(), sha256(b"abc"));
    }

    #[test]
    fn reports_which_artefact_failed() {
        let err = verify_digest("payload/app.jar", sha256(b"a"), sha256(b"b")).unwrap_err();
        assert!(err.to_string().contains("payload/app.jar"));
        assert!(err.is_integrity_failure());
    }
}
